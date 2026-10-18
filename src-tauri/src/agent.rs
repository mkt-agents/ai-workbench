//! AI Hub tool loop. Read-only tools run immediately. Writes and shell
//! commands wait for `agent_decide`. Paths must stay inside the selected repo.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use crate::agent_policy::{
    deny_reason, filter_specs, offered_tools, parse_review_verdict, policy_text, tool_gate, Gate,
    Permission, REVIEW_SYSTEM,
};
use crate::agent_stream::{chunk_usage, delta_content, SseLines, ThinkFilter};
use crate::ai_commands::{
    apply_auth_headers, strip_think_tags, trim_trailing_slash, AIModelConfig, ChatTurn,
};
use crate::cancellation::{is_cancelled, CancelGuard};

const MAX_ACTION_STEPS: usize = 32;
const MAX_ROUNDS: usize = 48;
const READ_CAP: usize = 20_000;
const WRITE_CAP: usize = 200_000;
const PREVIEW_CAP: usize = 2_000;
const LIST_CAP: usize = 200;
const GREP_CAP: usize = 30;
const CMD_CAP: usize = 8_000;
const APPROVAL_WAIT: Duration = Duration::from_secs(300);
const REVIEW_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAppShortcut {
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub args: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunRequest {
    pub config: AIModelConfig,
    pub system: String,
    pub user: String,
    pub request_id: String,
    pub repo_path: String,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    #[serde(default)]
    pub apps: Vec<AgentAppShortcut>,
    /// `read` / `workspace` / `review` / `full`, see `agent_policy::Permission`.
    #[serde(default)]
    pub permission: String,
    /// `false` for text-only tasks (polish / translate / …): no tools are offered.
    #[serde(default = "default_true")]
    pub tools: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

struct ModelTurn {
    text: String,
    calls: Vec<ToolCall>,
    /// Assistant message to replay on the next request (OpenAI object or Anthropic blocks).
    assistant: Value,
}

static DECISIONS: Mutex<Option<HashMap<String, oneshot::Sender<bool>>>> = Mutex::new(None);

fn decisions() -> std::sync::MutexGuard<'static, Option<HashMap<String, oneshot::Sender<bool>>>> {
    let mut guard = DECISIONS.lock().expect("agent decisions");
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    guard
}

fn decision_key(request_id: &str, call_id: &str) -> String {
    format!("{request_id}\u{1f}{call_id}")
}

#[tauri::command]
pub fn agent_decide(request_id: String, call_id: String, allow: bool) -> Result<(), String> {
    let key = decision_key(&request_id, &call_id);
    let tx = decisions()
        .as_mut()
        .and_then(|map| map.remove(&key))
        .ok_or_else(|| "没有等待确认的操作".to_string())?;
    let _ = tx.send(allow);
    Ok(())
}

fn tool_specs() -> Value {
    json!([
        {
            "name": "read_file",
            "description": "Read a text file inside the repository. Path is relative to the repo root.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }
        },
        {
            "name": "list_dir",
            "description": "List one directory inside the repository. Path is relative; empty means the root.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } }
            }
        },
        {
            "name": "grep",
            "description": "Search file contents under the repository for a literal substring. Skips dependencies and build output.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "Optional subdirectory" }
                },
                "required": ["pattern"]
            }
        },
        {
            "name": "write_file",
            "description": "Create or overwrite a text file inside the repository. Depending on the permission level the user may be asked first. Do not use this to delete files.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "run_command",
            "description": "Run a command with the working directory locked to the repository. Read-only git commands run immediately; others may need confirmation depending on the permission level. Refused: shutdown, format, disk wipe, force push, hard reset.",
            "parameters": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }
        },
        {
            "name": "open_page",
            "description": "Open a page in AI Workbench. tab is one of: ai-chat, ai-models, snippets, drafts, cursor-accounts, account-quota, git, runtime, plugins, cloudflared, hosts, devtools, app-launcher, settings. gitSub is optional: repos, commit, report.",
            "parameters": {
                "type": "object",
                "properties": {
                    "tab": { "type": "string" },
                    "gitSub": { "type": "string" }
                },
                "required": ["tab"]
            }
        },
        {
            "name": "launch_app",
            "description": "Launch a saved quick app by its name.",
            "parameters": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }
        }
    ])
}

/// Tool list in the provider's shape, limited to `names` (empty = no tools).
fn provider_tools(names: &[&str], anthropic: bool) -> Vec<Value> {
    filter_specs(&tool_specs(), names)
        .into_iter()
        .map(|spec| {
            if anthropic {
                json!({
                    "name": spec["name"],
                    "description": spec["description"],
                    "input_schema": spec["parameters"],
                })
            } else {
                json!({
                    "type": "function",
                    "function": {
                        "name": spec["name"],
                        "description": spec["description"],
                        "parameters": spec["parameters"],
                    }
                })
            }
        })
        .collect()
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push_str("\n…(truncated)");
    }
    out
}

fn emit_step(app: &AppHandle, id: &str, tool: &str, status: &str, summary: &str, detail: &str) {
    let _ = app.emit(
        "agent-step",
        json!({
            "id": id,
            "tool": tool,
            "status": status,
            "summary": summary,
            "detail": detail,
        }),
    );
}

fn emit_approval(app: &AppHandle, id: &str, call_id: &str, tool: &str, summary: &str, preview: &str) {
    let _ = app.emit(
        "agent-approval",
        json!({
            "id": id,
            "callId": call_id,
            "tool": tool,
            "summary": summary,
            "preview": preview,
        }),
    );
}

fn emit_text(app: &AppHandle, id: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = app.emit("generate-text-chunk", json!({ "id": id, "text": text }));
}

fn emit_done(app: &AppHandle, id: &str, started: Instant, usage: Option<(u64, u64)>) {
    let mut payload = json!({
        "id": id,
        "elapsedMs": started.elapsed().as_millis() as u64,
    });
    if let Some((prompt, completion)) = usage {
        payload["promptTokens"] = json!(prompt);
        payload["completionTokens"] = json!(completion);
    }
    let _ = app.emit("generate-text-done", payload);
}

fn emit_err(app: &AppHandle, id: &str, msg: &str) {
    let _ = app.emit("generate-text-error", json!({ "id": id, "error": msg }));
}

fn repo_root(path: &str) -> Result<PathBuf, String> {
    let root = PathBuf::from(path);
    if !root.is_dir() {
        return Err("没有可用的仓库目录".into());
    }
    root.canonicalize().map_err(|e| format!("无法解析仓库路径: {e}"))
}

/// Resolve `user_path` and reject anything outside `root`.
/// Missing parents are allowed so a confirmed write can create nested files.
fn resolve_in_repo(root: &Path, user_path: &str) -> Result<PathBuf, String> {
    let raw = user_path.trim();
    if raw.is_empty() {
        return Ok(root.to_path_buf());
    }
    let p = Path::new(raw);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let mut pending = Vec::new();
    let mut cursor = joined.as_path();
    let existing = loop {
        if cursor.exists() {
            break cursor;
        }
        let Some(name) = cursor.file_name() else {
            return Err("路径超出当前仓库".into());
        };
        if name == ".." || name == "." {
            return Err("路径超出当前仓库".into());
        }
        pending.push(name.to_os_string());
        match cursor.parent() {
            Some(parent) if parent != cursor => cursor = parent,
            _ => return Err("目录不存在".into()),
        }
    };
    let mut canon = existing.canonicalize().map_err(|e| e.to_string())?;
    if !canon.starts_with(root) {
        return Err("路径超出当前仓库".into());
    }
    for name in pending.iter().rev() {
        if name.is_empty() || name == ".." || name == "." {
            return Err("路径超出当前仓库".into());
        }
        canon.push(name);
    }
    Ok(canon)
}

fn arg_str(args: &Value, key: &str) -> String {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}

fn read_file(root: &Path, args: &Value) -> Result<String, String> {
    let path = resolve_in_repo(root, &arg_str(args, "path"))?;
    if !path.is_file() {
        return Err("不是文件".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if bytes.contains(&0) {
        return Err("这是二进制文件，未读取内容".into());
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(clip(&text, READ_CAP))
}

fn list_dir(root: &Path, args: &Value) -> Result<String, String> {
    let path = resolve_in_repo(root, &arg_str(args, "path"))?;
    if !path.is_dir() {
        return Err("不是目录".into());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let mark = if entry.path().is_dir() { "/" } else { "" };
        names.push(format!("{name}{mark}"));
        if names.len() >= LIST_CAP {
            break;
        }
    }
    names.sort();
    Ok(names.join("\n"))
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | ".next" | "coverage"
    )
}

fn grep_file(path: &Path, root: &Path, pattern: &str, hits: &mut Vec<String>) {
    if hits.len() >= GREP_CAP || !path.is_file() {
        return;
    }
    let Ok(meta) = path.metadata() else { return };
    if meta.len() > 1_000_000 {
        return;
    }
    let Ok(bytes) = std::fs::read(path) else { return };
    if bytes.contains(&0) {
        return;
    }
    let text = String::from_utf8_lossy(&bytes);
    let needle = pattern.to_lowercase();
    for (idx, line) in text.lines().enumerate() {
        if line.to_lowercase().contains(&needle) {
            let rel = path.strip_prefix(root).unwrap_or(path);
            hits.push(format!("{}:{}: {}", rel.display(), idx + 1, line.trim()));
            if hits.len() >= GREP_CAP {
                return;
            }
        }
    }
}

fn grep_tree(dir: &Path, root: &Path, pattern: &str, request_id: &str, hits: &mut Vec<String>) -> Result<(), String> {
    if hits.len() >= GREP_CAP {
        return Ok(());
    }
    if is_cancelled(request_id) {
        return Err(cancelled_message());
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    for entry in rd.flatten() {
        if hits.len() >= GREP_CAP {
            return Ok(());
        }
        if is_cancelled(request_id) {
            return Err(cancelled_message());
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if skip_dir(&name) {
                continue;
            }
            grep_tree(&path, root, pattern, request_id, hits)?;
            continue;
        }
        grep_file(&path, root, pattern, hits);
    }
    Ok(())
}

fn grep(root: &Path, args: &Value, request_id: &str) -> Result<String, String> {
    let pattern = arg_str(args, "pattern");
    if pattern.is_empty() {
        return Err("缺少搜索词".into());
    }
    let start = resolve_in_repo(root, &arg_str(args, "path"))?;
    let mut hits = Vec::new();
    if start.is_file() {
        grep_file(&start, root, &pattern, &mut hits);
    } else {
        grep_tree(&start, root, &pattern, request_id, &mut hits)?;
    }
    if hits.is_empty() {
        Ok("没有匹配".into())
    } else {
        Ok(hits.join("\n"))
    }
}

fn write_file(root: &Path, args: &Value) -> Result<String, String> {
    let rel = arg_str(args, "path");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.chars().count() > WRITE_CAP {
        return Err("内容过长".into());
    }
    let path = resolve_in_repo(root, &rel)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(format!("已写入 {}", path.display()))
}

pub(crate) fn command_blocked(command: &str) -> bool {
    let lower = command.to_lowercase();
    const BLOCKED: &[&str] = &[
        "shutdown",
        "restart-computer",
        "stop-computer",
        "logoff",
        "diskpart",
        "cipher /w",
        "mkfs",
        "format ",
        "format.com",
        "del /s",
        "rd /s",
        "rmdir /s",
        "remove-item",
        "reset --hard",
        "clean -f",
        "clean -x",
        "filter-branch",
    ];
    if BLOCKED.iter().any(|word| lower.contains(word)) {
        return true;
    }
    // rebase / --force 必须按 token 匹配：`git log --grep=rebase` 是查询，不是 rebase。
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.len() >= 2 && tokens[0] == "git" && tokens[1] == "rebase" {
        return true;
    }
    tokens
        .iter()
        .any(|t| *t == "--force" || *t == "--force-with-lease")
}

/// `git branch` 里真正只读的用法：只允许查询 flag；
/// 任何写 flag（-d/-D/-m/-M/-c/-C/--delete/…）或未知的 `-` 参数都判非只读。
/// 位置参数只在 `--list`/`-l`（此时是匹配模式）或紧跟取值 flag 时允许——
/// 裸的 `git branch <name>` 会新建分支。
fn branch_is_readonly(rest: &[&str]) -> bool {
    const QUERY_FLAGS: &[&str] = &[
        "-a", "-v", "-vv", "-r", "-av", "-avv", "-q", "-l", "--all", "--list", "--verbose",
        "--show-current", "--no-color", "--quiet",
    ];
    const VALUE_FLAGS: &[&str] = &[
        "--contains", "--no-contains", "--merged", "--no-merged", "--points-at", "--sort",
        "--format",
    ];
    let listing = rest.iter().any(|t| *t == "--list" || *t == "-l");
    let mut expect_value = false;
    rest.iter().all(|t| {
        if t.starts_with('-') {
            expect_value = VALUE_FLAGS.contains(t);
            QUERY_FLAGS.contains(t)
                || expect_value
                || VALUE_FLAGS
                    .iter()
                    .any(|f| t.starts_with(&format!("{f}=")))
        } else if expect_value {
            expect_value = false;
            true
        } else {
            listing
        }
    })
}

pub(crate) fn command_is_readonly(command: &str) -> bool {
    let lower = command.trim().to_lowercase();
    if lower.contains("&&") || lower.contains(';') || lower.contains('|') || lower.contains('`') {
        return false;
    }
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    // git 子命令逐个判定：branch/remote 有写形态，必须看参数
    if tokens[0] == "git" {
        match tokens.get(1).copied() {
            Some("status") | Some("diff") | Some("log") | Some("show") | Some("rev-parse")
            | Some("ls-files") | Some("ls-remote") => {
                // --output=<file> 会把结果写进文件
                return !tokens[2..].iter().any(|t| t.starts_with("--output"));
            }
            Some("config") => {
                return tokens.len() >= 3 && (tokens[2] == "--get" || tokens[2] == "--get-all" || tokens[2] == "--list")
            }
            Some("branch") => return tokens.len() >= 2 && branch_is_readonly(&tokens[2..]),
            Some("remote") => {
                // 裸 git remote / git remote -v 是查询；add/remove/rename/set-url/prune 都是写
                return tokens.len() == 2
                    || (tokens.len() == 3 && (tokens[2] == "-v" || tokens[2] == "--verbose"));
            }
            _ => return false,
        }
    }
    false
}

fn command_timeout(command: &str) -> Duration {
    let lower = command.to_lowercase();
    if lower.contains("git push")
        || lower.contains("git pull")
        || lower.contains("git fetch")
        || lower.contains("git ls-remote")
        || lower.contains("git clone")
    {
        Duration::from_secs(600)
    } else {
        Duration::from_secs(60)
    }
}

fn git_network_command(command: &str) -> bool {
    let lower = command.to_lowercase();
    lower.contains("git push")
        || lower.contains("git pull")
        || lower.contains("git fetch")
        || lower.contains("git ls-remote")
        || lower.contains("git clone")
}

fn stop_child(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn run_command(root: &Path, command: &str, request_id: &str) -> Result<String, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("命令为空".into());
    }
    if command_blocked(command) {
        return Err("这条命令被拒绝".into());
    }
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut c = Command::new("cmd");
        // 必须 raw_arg：std 按 MSVC 规则把 `"` 转义成 `\"`，cmd.exe 不认，带引号的参数
        // （commit -m "a b"）会被拆碎。/S 让 cmd 只剥最外层这对引号，其余原样执行。
        c.raw_arg(format!("/S /C \"{command}\"")).current_dir(root);
        // A credential dialog needs a real window. Hiding it makes push sit
        // until the timeout, then the model retries.
        if !git_network_command(command) {
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", command]).current_dir(root);
        c
    };
    // Commit-page push injects the Windows system proxy. A bare `cmd /C git push`
    // does not, so the same repo fails here with a connection reset.
    if git_network_command(command) {
        if let Some((http_proxy, https_proxy)) =
            crate::git_commands::git_proxy_env(&root.to_string_lossy())
        {
            cmd.env("http_proxy", &http_proxy);
            cmd.env("https_proxy", &https_proxy);
        }
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout {
            let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr {
            let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
        }
        buf
    });
    let started = Instant::now();
    let limit = command_timeout(command);
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) => {
                if is_cancelled(request_id) {
                    stop_child(&mut child);
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    return Err(cancelled_message());
                }
                if started.elapsed() > limit {
                    stop_child(&mut child);
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    return Err(format!("命令超过 {} 秒，已停止", limit.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let stdout_bytes = out_handle.join().unwrap_or_default();
    let stderr_bytes = err_handle.join().unwrap_or_default();
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&stdout_bytes));
    let err = String::from_utf8_lossy(&stderr_bytes);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if text.trim().is_empty() {
        text = format!("exit {code}");
    } else if code != 0 {
        // 失败要说出来：只看报错文本，弱模型分不清是警告还是命令没成功。放开头，clip 截不掉
        text = format!("exit {code}\n{text}");
    }
    Ok(clip(&text, CMD_CAP))
}

fn open_page(app: &AppHandle, args: &Value) -> Result<String, String> {
    let tab = arg_str(args, "tab");
    crate::tray::open_main_tab(app.clone(), tab.clone())?;
    let sub = arg_str(args, "gitSub");
    if tab == "git" && matches!(sub.as_str(), "repos" | "commit" | "report") {
        let _ = app.emit("navigate-git-subtab", sub.clone());
    }
    if sub.is_empty() {
        Ok(format!("已打开 {tab}"))
    } else {
        Ok(format!("已打开 {tab} / {sub}"))
    }
}

fn launch_named(apps: &[AgentAppShortcut], name: &str) -> Result<String, String> {
    let want = name.trim().to_lowercase();
    let found = apps.iter().find(|a| a.name.to_lowercase() == want);
    let Some(app) = found else {
        return Err("快捷应用里没有这个名字".into());
    };
    if app.path.trim().is_empty() {
        return Err("这个应用没有可执行路径".into());
    }
    let args = if app.args.trim().is_empty() {
        None
    } else {
        Some(app.args.clone())
    };
    #[cfg(windows)]
    {
        crate::app_launcher_commands::launch_app_inner(app.path.clone(), args)?;
            return Ok(format!("已启动 {}", app.name))
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Err("当前系统不支持启动应用".into())
    }
}

#[derive(Debug, PartialEq)]
enum Decision {
    Allow,
    Deny,
    /// Nobody answered within `APPROVAL_WAIT` (window hidden, user away).
    TimedOut,
}

async fn wait_decision(request_id: &str, call_id: &str) -> Result<Decision, String> {
    let (tx, mut rx) = oneshot::channel();
    decisions()
        .as_mut()
        .expect("map")
        .insert(decision_key(request_id, call_id), tx);
    let deadline = Instant::now() + APPROVAL_WAIT;
    loop {
        if is_cancelled(request_id) {
            decisions().as_mut().expect("map").remove(&decision_key(request_id, call_id));
            return Err(format!("[{}] 已取消", crate::config::ErrorCode::AiRequestCancelled.as_str()));
        }
        match rx.try_recv() {
            Ok(true) => return Ok(Decision::Allow),
            Ok(false) => return Ok(Decision::Deny),
            Err(oneshot::error::TryRecvError::Empty) => {}
            Err(oneshot::error::TryRecvError::Closed) => return Ok(Decision::Deny),
        }
        if Instant::now() > deadline {
            decisions().as_mut().expect("map").remove(&decision_key(request_id, call_id));
            return Ok(Decision::TimedOut);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn message_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for part in parts {
        if let Some(text) = part.as_str() {
            out.push_str(text);
        } else if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
            out.push_str(text);
        }
    }
    out
}

fn tool_arguments(value: Option<&Value>) -> String {
    match value {
        Some(v) if v.is_string() => v.as_str().unwrap_or("{}").to_string(),
        Some(v) if v.is_object() || v.is_array() => v.to_string(),
        _ => "{}".into(),
    }
}
fn parse_openai(body: &Value) -> Result<ModelTurn, String> {
    let message = body
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| "模型响应缺少 message".to_string())?;
    let mut message = message;
    let text_from_content = message_text(message.get("content").unwrap_or(&Value::Null));
    let mut calls = Vec::new();
    if let Some(arr) = message.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
        for call in arr.iter_mut() {
            let name = call
                .pointer("/function/name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let mut id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if id.is_empty() {
                id = format!("call-{}", calls.len());
                call["id"] = json!(id);
            }
            let arguments = tool_arguments(call.pointer("/function/arguments"));
            if let Some(function) = call.get_mut("function") {
                function["arguments"] = json!(arguments.clone());
            }
            calls.push(ToolCall { id, name, arguments });
        }
    }
    if message.get("content").map(|c| c.is_null()).unwrap_or(false) {
        message["content"] = json!("");
    }
    let mut text = text_from_content;
    if text.trim().is_empty() && calls.is_empty() {
        if let Some(extra) = message
            .get("reasoning")
            .and_then(|v| v.as_str())
            .or_else(|| message.get("reasoning_content").and_then(|v| v.as_str()))
        {
            text = extra.to_string();
        }
    }
    Ok(ModelTurn { text, calls, assistant: message })
}

fn parse_anthropic(body: &Value) -> Result<ModelTurn, String> {
    let mut blocks = body
        .get("content")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in &mut blocks {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let input = block.get("input").cloned().unwrap_or(json!({}));
                if !name.is_empty() {
                    let mut id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if id.is_empty() {
                        id = format!("call-{}", calls.len());
                        block["id"] = json!(id);
                    }
                    calls.push(ToolCall {
                        id,
                        name,
                        arguments: input.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(ModelTurn {
        text,
        calls,
        assistant: json!(blocks),
    })
}

fn round_is_action(names: &[String]) -> bool {
    names.iter().any(|name| !matches!(name.as_str(), "read_file" | "list_dir" | "grep"))
}

/// 比较两条命令是否「实质相同」：只换引号种类、多几个空格不算新做法。
fn normalize_command(command: &str) -> String {
    command
        .replace('\'', "\"")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 命令输出首行是 `exit N`（N≠0）即失败，见 `run_command`。
fn command_output_failed(output: &str) -> bool {
    output
        .lines()
        .next()
        .and_then(|l| l.trim().strip_prefix("exit "))
        .map_or(false, |code| code.trim() != "0")
}

/// 模型把刚失败的命令又提交了一遍（哪怕只换了引号）：在工具结果前插入提示，让它先诊断
/// 失败原因再换做法，否则弱模型会围绕同一个引号/路径问题无限重试。
/// 上一条成功时不提示——改完文件再跑一次 `git status` 是正常复查。
fn repeated_command_result(last: &str, last_failed: bool, current: &str, result: &str) -> String {
    if !last_failed || last.is_empty() || normalize_command(last) != normalize_command(current) {
        return result.to_string();
    }
    format!(
        "[Agent 提示] 这条命令和上一条实质相同，而上一条失败了。不要再重复；先分析上一条的输出（引号、路径、暂存区状态），换一种做法。\n\n{result}"
    )
}

fn limit_message(action_limited: bool, notes: &[String]) -> String {
    let mut msg = if action_limited {
        format!("已达到 {MAX_ACTION_STEPS} 步操作上限，停在这里。发送「继续」可接着做。")
    } else {
        format!("已达到 {MAX_ROUNDS} 轮上限，停在这里。发送「继续」可接着做。")
    };
    if !notes.is_empty() {
        msg.push_str("\n已做操作：");
        for note in notes.iter().take(24) {
            msg.push_str("\n- ");
            msg.push_str(note);
        }
    }
    msg
}

fn cancelled_message() -> String {
    format!(
        "[{}] 已取消",
        crate::config::ErrorCode::AiRequestCancelled.as_str()
    )
}

/// Polls the cancel flag while `fut` is pending, and drops it as soon as the
/// user hits stop. Dropping a reqwest send aborts the socket.
async fn await_cancellable<T>(
    fut: impl std::future::Future<Output = T>,
    request_id: &str,
) -> Result<T, String> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            result = &mut fut => return Ok(result),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if is_cancelled(request_id) {
                    return Err(cancelled_message());
                }
            }
        }
    }
}

fn decode_model_body(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(
            "模型返回了空内容。免费线路经常会中途断开，请再试一次，或换一个模型；只想聊天可以改为普通对话。"
                .into(),
        );
    }
    if trimmed.starts_with("data:") {
        return decode_sse(trimmed);
    }
    serde_json::from_str(trimmed).map_err(|e| {
        let snippet: String = trimmed.chars().take(180).collect();
        format!("模型响应不是 JSON：{snippet}（{e}）")
    })
}

/// Some routers ignore `stream: false` and send SSE. Fold the chunks into one
/// chat completion so the tool loop can read it.
fn decode_sse(text: &str) -> Result<Value, String> {
    let mut content = String::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut saw = false;
    for line in text.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        saw = true;
        if chunk.pointer("/choices/0/message").is_some() {
            return Ok(chunk);
        }
        if let Some(delta) = chunk.pointer("/choices/0/delta") {
            if let Some(piece) = delta.get("content").and_then(|v| v.as_str()) {
                content.push_str(piece);
            }
            if let Some(arr) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for call in arr {
                    let index = call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    if calls.len() <= index {
                        calls.resize(index + 1, json!({}));
                    }
                    merge_tool_delta(&mut calls[index], call);
                }
            }
        }
    }
    if !saw || (content.trim().is_empty() && calls.iter().all(|c| c.as_object().map(|o| o.is_empty()).unwrap_or(true)))
    {
        return Err("模型以流式返回，但没有有效内容。请重试，或换一个模型。".into());
    }
    Ok(json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": content,
                "tool_calls": calls.into_iter().filter(|c| c.get("function").is_some()).collect::<Vec<_>>(),
            }
        }]
    }))
}

fn merge_tool_delta(slot: &mut Value, delta: &Value) {
    if slot.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        *slot = json!({
            "id": "",
            "type": "function",
            "function": { "name": "", "arguments": "" }
        });
    }
    if let Some(id) = delta.get("id").and_then(|v| v.as_str()) {
        if !id.is_empty() {
            slot["id"] = json!(id);
        }
    }
    if let Some(name) = delta.pointer("/function/name").and_then(|v| v.as_str()) {
        if !name.is_empty() {
            slot["function"]["name"] = json!(name);
        }
    }
    if let Some(args) = delta.pointer("/function/arguments").and_then(|v| v.as_str()) {
        let prev = slot
            .pointer("/function/arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        slot["function"]["arguments"] = json!(format!("{prev}{args}"));
    }
}

/// `model_turn` error when the provider rejects the `tools` field; the caller retries without tools.
const TOOLS_UNSUPPORTED: &str = "__agent_tools_unsupported__";

struct RoundOutput {
    turn: ModelTurn,
    /// Visible text already sent to the frontend while streaming (empty = not streamed).
    streamed: String,
    usage: Option<(u64, u64)>,
}

/// One model round. With `stream` set (OpenAI-compatible only) the answer text is
/// emitted as it arrives; the full body is still folded by `decode_model_body`, so a
/// router that ignores `stream: true` and answers with plain JSON works the same.
async fn model_turn(
    client: &reqwest::Client,
    config: &AIModelConfig,
    anthropic: bool,
    messages: &Value,
    system: &str,
    tools: &[Value],
    stream: Option<&AppHandle>,
    request_id: &str,
) -> Result<RoundOutput, String> {
    let base = trim_trailing_slash(&config.base_url);
    let max_tokens = {
        let m = if config.max_tokens > 0 { config.max_tokens } else { 1536 };
        m.clamp(256, 4096)
    };
    let stream = if anthropic { None } else { stream };
    let (url, body) = if anthropic {
        let url = if base.ends_with("/v1") {
            format!("{base}/messages")
        } else {
            format!("{base}/v1/messages")
        };
        let mut body = json!({
            "model": config.model,
            "max_tokens": max_tokens,
            "system": system,
            "messages": messages,
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        (url, body)
    } else {
        let url = format!("{base}/chat/completions");
        let mut body = json!({
            "model": config.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "temperature": config.temperature.clamp(0.0, 1.0),
            "stream": stream.is_some(),
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        (url, body)
    };
    for attempt in 0..2 {
        let resp = await_cancellable(
            apply_auth_headers(client.post(&url).json(&body), config, anthropic).send(),
            request_id,
        )
        .await?
        .map_err(|e| format!("请求模型失败: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = await_cancellable(resp.text(), request_id).await?.unwrap_or_default();
            let snippet: String = text.chars().take(240).collect();
            if !tools.is_empty() && snippet.to_lowercase().contains("tool") {
                return Err(TOOLS_UNSUPPORTED.into());
            }
            return Err(format!("模型返回 HTTP {status}: {snippet}"));
        }
        let mut streamed = String::new();
        let mut usage = None;
        let text = match stream {
            Some(app) => {
                let mut resp = resp;
                let mut raw = Vec::new();
                let mut lines = SseLines::default();
                let mut filter = ThinkFilter::default();
                while let Some(bytes) = await_cancellable(resp.chunk(), request_id)
                    .await?
                    .map_err(|e| format!("读取模型响应失败: {e}"))?
                {
                    raw.extend_from_slice(&bytes);
                    for data in lines.push(&bytes) {
                        let Ok(chunk) = serde_json::from_str::<Value>(&data) else {
                            continue;
                        };
                        if let Some(u) = chunk_usage(&chunk) {
                            usage = Some(u);
                        }
                        if let Some(piece) = delta_content(&chunk) {
                            let visible = filter.push(piece);
                            emit_text(app, request_id, &visible);
                            streamed.push_str(&visible);
                        }
                    }
                }
                let tail = filter.finish();
                emit_text(app, request_id, &tail);
                streamed.push_str(&tail);
                String::from_utf8_lossy(&raw).into_owned()
            }
            None => await_cancellable(resp.text(), request_id).await?.unwrap_or_default(),
        };
        match decode_model_body(&text) {
            Ok(json) => {
                if usage.is_none() {
                    usage = chunk_usage(&json);
                }
                let turn = if anthropic { parse_anthropic(&json)? } else { parse_openai(&json)? };
                return Ok(RoundOutput { turn, streamed, usage });
            }
            Err(_) if attempt == 0 && text.trim().is_empty() => {
                if is_cancelled(request_id) {
                    return Err(cancelled_message());
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(
        "模型返回了空内容。免费线路经常会中途断开，请再试一次，或换一个模型。"
            .into(),
    )
}

/// Ask the reviewer model whether one action may run without the user (审查托管).
/// Any failure, timeout or unclear reply escalates: `(false, reason)`.
async fn review_action(
    client: &reqwest::Client,
    config: &AIModelConfig,
    anthropic: bool,
    user_request: &str,
    summary: &str,
    preview: &str,
    request_id: &str,
) -> (bool, String) {
    let user = format!(
        "User request:\n{}\n\nAction: {summary}\n\nDetails:\n{}",
        clip(user_request, 2_000),
        clip(preview, PREVIEW_CAP)
    );
    let messages = if anthropic {
        json!([{ "role": "user", "content": user }])
    } else {
        json!([
            { "role": "system", "content": REVIEW_SYSTEM },
            { "role": "user", "content": user },
        ])
    };
    let call = model_turn(client, config, anthropic, &messages, REVIEW_SYSTEM, &[], None, request_id);
    match tokio::time::timeout(REVIEW_WAIT, call).await {
        Ok(Ok(out)) => parse_review_verdict(&out.turn.text),
        Ok(Err(e)) => (false, format!("审查失败：{}", clip(&e, 120))),
        Err(_) => (false, "审查超时".into()),
    }
}

fn parse_args(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or(json!({}))
}

#[tauri::command]
pub async fn agent_run(app: AppHandle, req: AgentRunRequest) -> Result<(), String> {
    let request_id = req.request_id.clone();
    let _guard = CancelGuard::new(&request_id);
    let started = Instant::now();
    if req.config.base_url.trim().is_empty() || req.config.model.trim().is_empty() {
        let msg = "模型 Base URL 或模型名未配置";
        emit_err(&app, &request_id, msg);
        return Err(msg.into());
    }
    if req.user.trim().is_empty() {
        let msg = "问题为空";
        emit_err(&app, &request_id, msg);
        return Err(msg.into());
    }
    let permission = Permission::parse(&req.permission);
    // 没选仓库也能问（只读权限 / 纯文本任务）：不挂任何要工作目录的工具
    let root = if req.repo_path.trim().is_empty() {
        None
    } else {
        match repo_root(&req.repo_path) {
            Ok(p) => Some(p),
            Err(e) => {
                emit_err(&app, &request_id, &e);
                return Err(e);
            }
        }
    };
    let anthropic = req.config.provider.to_lowercase() == "anthropic";
    let mut tools_enabled = req.tools;
    let system = format!("{}{}", req.system, policy_text(permission, root.as_deref(), tools_enabled));
    let tool_names = offered_tools(permission, root.is_some(), tools_enabled);
    let mut tools = provider_tools(&tool_names, anthropic);
    // 流式回合按「多久没收到数据」计时；Anthropic 仍是非流式，要等整轮写完，用总时长上限
    let client = if anthropic {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(crate::config::GENERATE_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(crate::config::GENERATE_TEXT_TIMEOUT_SECS))
            .build()
    } else {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(crate::config::GENERATE_CONNECT_TIMEOUT_SECS))
            .read_timeout(Duration::from_secs(crate::config::GENERATE_STREAM_IDLE_SECS))
            .build()
    }
    .map_err(|e| e.to_string())?;

    let mut messages = Vec::new();
    if anthropic {
        for turn in &req.history {
            let role = if turn.role == "assistant" { "assistant" } else { "user" };
            messages.push(json!({ "role": role, "content": turn.content }));
        }
        messages.push(json!({ "role": "user", "content": req.user }));
    } else {
        messages.push(json!({ "role": "system", "content": system }));
        for turn in &req.history {
            let role = if turn.role == "assistant" { "assistant" } else { "user" };
            messages.push(json!({ "role": role, "content": turn.content }));
        }
        messages.push(json!({ "role": "user", "content": req.user }));
    }

    let mut final_text = String::new();
    let mut rounds = 0usize;
    let mut action_steps = 0usize;
    let mut notes: Vec<String> = Vec::new();
    // Ask 档每轮只问第一次：命令 / 启动应用各自记一次
    let mut shell_granted = false;
    let mut app_granted = false;
    // 上一条实际执行过的 shell 命令，用来识别原样重试。
    let mut last_shell_command = String::new();
    let mut last_shell_failed = false;
    let mut usage_total: Option<(u64, u64)> = None;
    loop {
        if rounds >= MAX_ROUNDS || action_steps >= MAX_ACTION_STEPS {
            break;
        }
        rounds += 1;
        if is_cancelled(&request_id) {
            let msg = format!("[{}] 已取消", crate::config::ErrorCode::AiRequestCancelled.as_str());
            emit_err(&app, &request_id, &msg);
            return Err(msg);
        }
        let round = model_turn(
            &client,
            &req.config,
            anthropic,
            &Value::Array(messages.clone()),
            &system,
            &tools,
            Some(&app),
            &request_id,
        )
        .await;
        let round = match round {
            Err(e) if e == TOOLS_UNSUPPORTED && tools_enabled => {
                // 模型不支持工具调用：去掉 tools 重发，这一轮只回答
                tools_enabled = false;
                tools.clear();
                emit_step(&app, &request_id, "notice", "info", "当前模型不支持工具调用，本轮只回答", "");
                rounds -= 1;
                continue;
            }
            Ok(r) => r,
            Err(e) => {
                let e = if e == TOOLS_UNSUPPORTED { "模型拒绝了这次请求（工具调用）。".to_string() } else { e };
                emit_err(&app, &request_id, &e);
                return Err(e);
            }
        };
        if let Some((p, c)) = round.usage {
            let (tp, tc) = usage_total.unwrap_or((0, 0));
            usage_total = Some((tp + p, tc + c));
        }
        let turn = round.turn;
        if round.streamed.trim().is_empty() {
            // 没走流式（Anthropic / 路由回了整段 JSON / 只有 reasoning 字段）：整轮一次性发
            let visible = strip_think_tags(&turn.text);
            if !visible.trim().is_empty() {
                final_text.push_str(&visible);
                emit_text(&app, &request_id, &visible);
            }
        } else {
            final_text.push_str(&round.streamed);
        }
        if turn.calls.is_empty() {
            if final_text.trim().is_empty() {
                let msg = "这个模型没有返回文本，也没有调用工具。";
                emit_err(&app, &request_id, msg);
                return Err(msg.into());
            }
            emit_done(&app, &request_id, started, usage_total);
            return Ok(());
        }
        if anthropic {
            messages.push(json!({ "role": "assistant", "content": turn.assistant }));
        } else {
            messages.push(turn.assistant.clone());
        }
        let mut tool_results = Vec::new();
        for call in &turn.calls {
            if is_cancelled(&request_id) {
                let msg = format!("[{}] 已取消", crate::config::ErrorCode::AiRequestCancelled.as_str());
                emit_err(&app, &request_id, &msg);
                return Err(msg);
            }
            let args = parse_args(&call.arguments);
            let summary = match call.name.as_str() {
                "read_file" => format!("读取 {}", arg_str(&args, "path")),
                "list_dir" => format!("列出 {}", arg_str(&args, "path")),
                "grep" => format!("搜索 {}", arg_str(&args, "pattern")),
                "write_file" => format!("写入 {}", arg_str(&args, "path")),
                "run_command" => format!("运行 {}", arg_str(&args, "command")),
                "open_page" => format!("打开 {}", arg_str(&args, "tab")),
                "launch_app" => format!("启动 {}", arg_str(&args, "name")),
                _ => format!("未知工具 {}", call.name),
            };
            emit_step(&app, &request_id, &call.name, "running", &summary, "");
            let command = if call.name == "run_command" {
                arg_str(&args, "command")
            } else {
                String::new()
            };
            let preview = match call.name.as_str() {
                "write_file" => clip(args.get("content").and_then(|v| v.as_str()).unwrap_or(""), PREVIEW_CAP),
                "run_command" => command.clone(),
                _ => summary.clone(),
            };
            let gate = tool_gate(permission, &call.name, &command);
            if gate == Gate::Deny {
                let reason = deny_reason(permission, &call.name, &command);
                emit_step(&app, &request_id, &call.name, "denied", &summary, &reason);
                notes.push(format!("{summary}（未执行）"));
                tool_results.push((call.id.clone(), reason));
                continue;
            }
            // 审查托管：先让 AI 审一遍，只有明确放行才不打扰用户
            let mut review_note = String::new();
            let mut needs_confirm = match gate {
                Gate::Ask => match call.name.as_str() {
                    "launch_app" => !app_granted,
                    _ => !shell_granted,
                },
                _ => false,
            };
            if gate == Gate::Review {
                let (allow, reason) =
                    review_action(&client, &req.config, anthropic, &req.user, &summary, &preview, &request_id).await;
                if is_cancelled(&request_id) {
                    let msg = cancelled_message();
                    emit_err(&app, &request_id, &msg);
                    return Err(msg);
                }
                if allow {
                    let detail = if reason.is_empty() { "AI 审查通过".to_string() } else { format!("AI 审查通过：{reason}") };
                    emit_step(&app, &request_id, &call.name, "running", &summary, &detail);
                } else {
                    needs_confirm = true;
                    review_note = if reason.is_empty() { "AI 审查：需要你确认".into() } else { format!("AI 审查：{reason}") };
                }
            }
            if needs_confirm {
                let title = if review_note.is_empty() { summary.clone() } else { format!("{summary}（{review_note}）") };
                emit_approval(&app, &request_id, &call.id, &call.name, &title, &preview);
                let decision = match wait_decision(&request_id, &call.id).await {
                    Ok(v) => v,
                    Err(e) => {
                        emit_err(&app, &request_id, &e);
                        return Err(e);
                    }
                };
                match decision {
                    Decision::Allow => {}
                    Decision::Deny => {
                        emit_step(&app, &request_id, &call.name, "denied", &summary, "用户拒绝");
                        notes.push(format!("{summary}（已拒绝）"));
                        tool_results.push((call.id.clone(), "用户拒绝了这个操作".to_string()));
                        continue;
                    }
                    Decision::TimedOut => {
                        let mins = APPROVAL_WAIT.as_secs() / 60;
                        emit_step(
                            &app,
                            &request_id,
                            &call.name,
                            "denied",
                            &summary,
                            &format!("等待确认超过 {mins} 分钟，已自动拒绝"),
                        );
                        notes.push(format!("{summary}（确认超时）"));
                        tool_results.push((
                            call.id.clone(),
                            format!(
                                "等待用户确认超过 {mins} 分钟，这个操作没有执行。不要重试，直接告诉用户：需要确认的操作是什么，确认后可以再发一次。"
                            ),
                        ));
                        continue;
                    }
                }
                // 审查转人工的那一次只放行这一条，不给整轮开口子
                if gate == Gate::Ask {
                    if call.name == "launch_app" {
                        app_granted = true;
                    } else {
                        shell_granted = true;
                    }
                }
            }
            let no_repo = || Err::<String, String>("没有选择仓库，不能读写文件或执行命令。".into());
            let result = match (call.name.as_str(), root.as_deref()) {
                ("read_file", Some(root)) => read_file(root, &args),
                ("list_dir", Some(root)) => list_dir(root, &args),
                ("grep", Some(root)) => grep(root, &args, &request_id),
                ("write_file", Some(root)) => write_file(root, &args),
                ("run_command", Some(root)) => run_command(root, &command, &request_id),
                ("read_file" | "list_dir" | "grep" | "write_file" | "run_command", None) => no_repo(),
                ("open_page", _) => open_page(&app, &args),
                ("launch_app", _) => launch_named(&req.apps, &arg_str(&args, "name")),
                _ => Err(format!("未知工具 {}", call.name)),
            };
            let failed = match &result {
                Ok(text) => command_output_failed(text),
                Err(_) => true,
            };
            match result {
                Ok(text) => {
                    let text =
                        repeated_command_result(&last_shell_command, last_shell_failed, &command, &text);
                    emit_step(&app, &request_id, &call.name, "done", &summary, &clip(&text, 500));
                    notes.push(summary);
                    tool_results.push((call.id.clone(), text));
                }
                Err(err) if err.contains("AI_REQUEST_CANCELLED") => {
                    emit_err(&app, &request_id, &err);
                    return Err(err);
                }
                Err(err) => {
                    emit_step(&app, &request_id, &call.name, "error", &summary, &err);
                    notes.push(format!("{summary}（失败）"));
                    let text = repeated_command_result(
                        &last_shell_command,
                        last_shell_failed,
                        &command,
                        &format!("错误: {err}"),
                    );
                    tool_results.push((call.id.clone(), text));
                }
            }
            if call.name == "run_command" {
                last_shell_command = command;
                last_shell_failed = failed;
            }
        }
        if round_is_action(&turn.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()) {
            action_steps += 1;
        }
        if anthropic {
            let blocks: Vec<Value> = tool_results
                .into_iter()
                .map(|(id, content)| {
                    json!({ "type": "tool_result", "tool_use_id": id, "content": content })
                })
                .collect();
            messages.push(json!({ "role": "user", "content": blocks }));
        } else {
            for (id, content) in tool_results {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": content,
                }));
            }
        }
    }
    let msg = limit_message(action_steps >= MAX_ACTION_STEPS, &notes);
    emit_text(&app, &request_id, &msg);
    emit_done(&app, &request_id, started, usage_total);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        command_blocked, command_is_readonly, command_output_failed, decode_model_body, limit_message,
        repeated_command_result, resolve_in_repo, round_is_action, run_command,
    };
    use std::fs;
    use std::process::Command;

    #[test]
    fn repeated_failed_command_gets_retry_hint() {
        let out = "exit 1\nfatal:did not match any file(s) ";
        let hinted = repeated_command_result("git add a b", true, "git add a b", out);
        assert!(hinted.contains("Agent 提示"));
        assert!(hinted.contains(out));
        // 只换引号 / 多空格也算重复
        let requoted = repeated_command_result(
            "git commit -m \"chore: x\"",
            true,
            "git commit  -m 'chore: x'",
            out,
        );
        assert!(requoted.contains("Agent 提示"));
        // 命令不同 / 首条命令 / 非命令调用（空串）：原样透传
        assert_eq!(repeated_command_result("git add a", true, "git add a b", out), out);
        assert_eq!(repeated_command_result("", true, "git add a b", out), out);
        assert_eq!(repeated_command_result("git add a b", true, "", out), out);
        // 上一条成功：改完文件再查一次状态是正常复查，不提示
        assert_eq!(repeated_command_result("git status", false, "git status", "ok"), "ok");
    }

    #[test]
    fn failure_is_read_from_the_exit_line() {
        assert!(command_output_failed("exit 1\nfatal: x"));
        assert!(command_output_failed("exit 128"));
        assert!(!command_output_failed("exit 0"));
        assert!(!command_output_failed("On branch master\nnothing to commit"));
        // 正文里出现 exit 不算
        assert!(!command_output_failed("done\nexit 1"));
    }

    #[test]
    fn empty_model_body_is_not_a_json_eof_error() {
        let err = decode_model_body("").unwrap_err();
        assert!(err.contains("空内容"), "{err}");
        assert!(!err.contains("EOF"), "{err}");
    }

    #[test]
    fn sse_chunks_fold_into_one_completion() {
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"好\",\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"run_command\",\"arguments\":\"{\\\"command\\\":\\\"git status\\\"}\"}}]}}]}\n",
            "\n",
            "data: [DONE]\n",
        );
        let body = decode_model_body(raw).unwrap();
        assert_eq!(
            body.pointer("/choices/0/message/content").and_then(|v| v.as_str()),
            Some("好")
        );
        assert_eq!(
            body.pointer("/choices/0/message/tool_calls/0/function/name")
                .and_then(|v| v.as_str()),
            Some("run_command")
        );
    }

    #[test]
    fn read_only_rounds_do_not_count_as_actions() {
        assert!(!round_is_action(&["read_file".into(), "grep".into()]));
        assert!(round_is_action(&["read_file".into(), "write_file".into()]));
        let msg = limit_message(true, &["读取 README.md".into()]);
        assert!(msg.contains("32"));
        assert!(msg.contains("继续"));
        assert!(msg.contains("读取 README.md"));
    }

    #[test]
    fn blocks_shutdown_and_format() {
        assert!(command_blocked("shutdown /s /t 0"));
        assert!(command_blocked("format C:"));
        assert!(command_blocked("git push --force"));
        assert!(command_blocked("git reset --hard HEAD"));
        assert!(!command_blocked("git status"));
        assert!(!command_blocked("git log --format=%s"));
        assert!(!command_blocked("git push origin master"));
        assert!(command_is_readonly("git status --short"));
        assert!(command_is_readonly("git diff"));
        assert!(!command_is_readonly("git push"));
        assert!(!command_is_readonly("git add -A && git commit -m x"));
    }

    /// rebase / --force 按 token 拦截：`git log --grep=rebase` 是查询，必须放行
    #[test]
    fn blocked_matches_tokens_not_substrings() {
        assert!(!command_blocked("git log --grep=rebase"));
        assert!(!command_blocked("git log --oneline rebase-check.txt"));
        assert!(command_blocked("git rebase main"));
        assert!(command_blocked("git rebase -i HEAD~3"));
        assert!(command_blocked("git push --force-with-lease origin main"));
        assert!(!command_blocked("git push origin main"));
    }

    /// branch/remote 有写形态：删分支、改远端必须走用户审批，不能按前缀判只读
    #[test]
    fn branch_and_remote_writes_need_approval() {
        // 查询形态保持只读
        assert!(command_is_readonly("git branch"));
        assert!(command_is_readonly("git branch -a"));
        assert!(command_is_readonly("git branch --list"));
        assert!(command_is_readonly("git branch --contains HEAD"));
        assert!(command_is_readonly("git remote"));
        assert!(command_is_readonly("git remote -v"));
        // 写形态必须审批
        assert!(!command_is_readonly("git branch -D main"));
        assert!(!command_is_readonly("git branch -d tmp"));
        assert!(!command_is_readonly("git branch -m old new"));
        assert!(!command_is_readonly("git branch --set-upstream-to origin/main"));
        assert!(!command_is_readonly("git remote remove origin"));
        assert!(!command_is_readonly("git remote add upstream https://example.com/x.git"));
        assert!(!command_is_readonly("git remote set-url origin ssh://x"));
        assert!(!command_is_readonly("git remote rename origin up"));
        // 裸分支名 = 新建分支；只有 --list 下的位置参数才是匹配模式
        assert!(!command_is_readonly("git branch newfeature"));
        assert!(!command_is_readonly("git branch -a newfeature"));
        assert!(command_is_readonly("git branch --list feat*"));
        assert!(command_is_readonly("git branch --merged=main"));
        // --output 会写文件
        assert!(!command_is_readonly("git diff --output=x.patch"));
        assert!(command_is_readonly("git log --grep=rebase"));
        // config 只允许读形态
        assert!(command_is_readonly("git config --get user.name"));
        assert!(!command_is_readonly("git config user.name x"));
    }

    #[test]
    fn rejects_paths_outside_the_repo() {
        let root = std::env::temp_dir().join(format!("aw-agent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.txt"), "hi").unwrap();
        let canon = root.canonicalize().unwrap();
        assert!(resolve_in_repo(&canon, "src/a.txt").is_ok());
        assert!(resolve_in_repo(&canon, "src/new/note.txt").is_ok());
        assert!(resolve_in_repo(&canon, "../outside.txt").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    /// 双引号参数必须原样到达 git：std 的 MSVC 转义（`\"`）cmd.exe 不认，
    /// 曾把 `-m "chore: ignore .zcode dir"` 拆成 `"chore:` + 三个 pathspec，模型只能反复重试。
    #[cfg(windows)]
    #[test]
    fn quoted_arguments_survive_cmd() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("aw-agent-quote-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let run = |c: &str| run_command(&root, c, "test-quote").unwrap();
        run("git init -q");
        let out = run(
            "git -c user.name=\"A B\" -c user.email=a@b.c commit -q --allow-empty -m \"chore: ignore .zcode dir\"",
        );
        assert_eq!(out.trim(), "exit 0", "commit failed: {out}");
        assert_eq!(run("git log -1 --format=%s").trim(), "chore: ignore .zcode dir");
        assert_eq!(run("git log -1 --format=%an").trim(), "A B");
        // 以引号开头的命令（带空格的程序路径）也不能被 cmd 的去引号规则吃掉
        assert!(run("\"git\" --version").contains("git version"));
        let _ = fs::remove_dir_all(&root);
    }
}
