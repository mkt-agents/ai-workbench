//! AI Hub tool loop. What a call may do is decided by `agent_policy` (permission
//! level); the tools themselves live in `agent_fs` / `agent_proc` / `agent_sys` /
//! `agent_computer`. Changes that need consent wait for `agent_decide`.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use crate::agent_fs::{arg_str, Protected, Scope};
use crate::agent_policy::{
    action_kind, deny_reason, filter_specs, offered_tools, parse_review_verdict, policy_text, tool_gate,
    Gate, Permission, Reach, COMPUTER_TOOLS, REVIEW_SYSTEM,
};
use crate::agent_stream::{chunk_usage, delta_content, SseLines, ThinkFilter};
use crate::ai_commands::{
    apply_auth_headers, strip_think_tags, trim_trailing_slash, AIModelConfig, ChatTurn,
};
use crate::cancellation::{is_cancelled, CancelGuard};

const MAX_ACTION_STEPS: usize = 32;
const MAX_ROUNDS: usize = 48;
const PREVIEW_CAP: usize = 2_000;
/// Step detail sent to the UI (command output, errors, diffs).
const DETAIL_CAP: usize = 2_000;
/// Screenshots kept in the conversation; older ones become a text placeholder.
const KEEP_SHOTS: usize = 2;
const APPROVAL_WAIT: Duration = Duration::from_secs(300);
/// Summary of the `conflict` step; the hub shows an "open the commit page" button on it.
const CONFLICT_NOTICE: &str = "仓库有冲突，需要你在提交页处理";
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
    /// `repo_path` is a workspace folder: the service repositories picked inside it.
    #[serde(default)]
    pub services: Vec<String>,
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
    /// The model accepts images; with 完全权限 this unlocks operating the computer.
    #[serde(default)]
    pub vision: bool,
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

/// User's answer on an approval card: allow?, and "same kind for the rest of this turn".
type DecisionMsg = (bool, bool);

static DECISIONS: Mutex<Option<HashMap<String, oneshot::Sender<DecisionMsg>>>> = Mutex::new(None);

fn decisions() -> std::sync::MutexGuard<'static, Option<HashMap<String, oneshot::Sender<DecisionMsg>>>> {
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
pub fn agent_decide(
    request_id: String,
    call_id: String,
    allow: bool,
    scope: Option<String>,
) -> Result<(), String> {
    let key = decision_key(&request_id, &call_id);
    let tx = decisions()
        .as_mut()
        .and_then(|map| map.remove(&key))
        .ok_or_else(|| "没有等待确认的操作".to_string())?;
    let _ = tx.send((allow, allow && scope.as_deref() == Some("turn")));
    Ok(())
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

/// Every tool the agent knows; `agent_policy::offered_tools` picks the ones a turn gets.
fn tool_specs() -> Value {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let n = |d: &str| json!({ "type": "integer", "description": d });
    let b = |d: &str| json!({ "type": "boolean", "description": d });
    json!([
        { "name": "read_file",
          "description": "Read a text file with line numbers, in pages. Big files: read again with offset to continue.",
          "parameters": obj(json!({ "path": s("Relative to the repo, or absolute"), "offset": n("First line, 1-based (default 1)"), "limit": n("Lines to read (default 400)") }), &["path"]) },
        { "name": "list_dir",
          "description": "List one directory. Empty path = the repo root (or home folder without a repo).",
          "parameters": obj(json!({ "path": s("Directory") }), &[]) },
        { "name": "find_files",
          "description": "Find files by name with * and ? wildcards, e.g. *.tsx or use*Stream.ts. A pattern with / matches the relative path. Skips node_modules, .git, target, dist.",
          "parameters": obj(json!({ "pattern": s("Wildcard pattern"), "path": s("Folder to search (default: repo root)") }), &["pattern"]) },
        { "name": "grep",
          "description": "Search file contents (case-insensitive). Literal text by default; set regex=true for a regular expression. Optional glob limits file names.",
          "parameters": obj(json!({ "pattern": s("Text or regex"), "path": s("File or folder (default: repo root)"), "regex": b("Treat pattern as a regex"), "glob": s("File name filter, e.g. *.rs") }), &["pattern"]) },
        { "name": "write_file",
          "description": "Create a new file or fully rewrite one. For changes to an existing file prefer edit_file.",
          "parameters": obj(json!({ "path": s("File path"), "content": s("Whole new content") }), &["path", "content"]) },
        { "name": "edit_file",
          "description": "Replace an exact piece of text in a file. `old` must match exactly once (include enough surrounding lines), unless replace_all=true.",
          "parameters": obj(json!({ "path": s("File path"), "old": s("Exact current text"), "new": s("Replacement text"), "replace_all": b("Replace every occurrence") }), &["path", "old", "new"]) },
        { "name": "delete_path",
          "description": "Delete a file or folder by moving it to the Recycle Bin (recoverable).",
          "parameters": obj(json!({ "path": s("File or folder") }), &["path"]) },
        { "name": "move_path",
          "description": "Move or rename a file or folder. The target must not exist.",
          "parameters": obj(json!({ "from": s("Current path"), "to": s("New path") }), &["from", "to"]) },
        { "name": "run_command",
          "description": "Run a command to completion and return its output. cmd.exe by default, or PowerShell with shell=powershell. Read-only git commands run immediately. Not for servers or watchers: use start_process. Refused: shutdown, format, disk wipe, force push, hard reset.",
          "parameters": obj(json!({ "command": s("Command line"), "shell": s("cmd (default) or powershell"), "cwd": s("Working directory (default: repo root)"), "timeout_secs": n("Up to 600, default 60 (git network commands 600)") }), &["command"]) },
        { "name": "start_process",
          "description": "Start a long-running command in the background (dev server, watcher, build --watch). Returns an id; read with process_output, stop with stop_process.",
          "parameters": obj(json!({ "command": s("Command line"), "shell": s("cmd (default) or powershell"), "cwd": s("Working directory") }), &["command"]) },
        { "name": "process_output",
          "description": "Status and latest output of a background process started with start_process.",
          "parameters": obj(json!({ "id": n("Process id from start_process"), "tail_chars": n("How much of the end to return (default 8000)") }), &["id"]) },
        { "name": "stop_process",
          "description": "Stop a background process (and its child processes).",
          "parameters": obj(json!({ "id": n("Process id from start_process") }), &["id"]) },
        { "name": "open_page",
          "description": "Open a page in AI Workbench. tab is one of: ai-chat, ai-models, snippets, drafts, cursor-accounts, account-quota, git, runtime, plugins, cloudflared, hosts, devtools, app-launcher, settings. gitSub is optional: repos, commit, report.",
          "parameters": obj(json!({ "tab": s("Page id"), "gitSub": s("Git sub page") }), &["tab"]) },
        { "name": "launch_app",
          "description": "Launch a quick app or a program installed on this computer (Start Menu / Desktop shortcut) by its name. If several programs match, the error lists them: call again with one exact name.",
          "parameters": obj(json!({ "name": s("Program name, e.g. 360浏览器 or 微信") }), &["name"]) },
        { "name": "open_target",
          "description": "Open a file, folder or http(s) / mailto URL with its default program.",
          "parameters": obj(json!({ "target": s("Path or URL") }), &["target"]) },
        { "name": "clipboard_read",
          "description": "Read the text currently on the clipboard.",
          "parameters": obj(json!({}), &[]) },
        { "name": "clipboard_write",
          "description": "Put text on the clipboard.",
          "parameters": obj(json!({ "text": s("Text") }), &["text"]) },
        { "name": "list_processes",
          "description": "List running processes (pid, memory, name), largest first. Optional name filter.",
          "parameters": obj(json!({ "name": s("Part of the process name") }), &[]) },
        { "name": "kill_process",
          "description": "End a process by pid, or every process with an exact image name. System-critical processes are refused.",
          "parameters": obj(json!({ "pid": n("Process id"), "name": s("Image name, e.g. node.exe") }), &[]) },
        { "name": "find_port",
          "description": "Which processes listen on a TCP/UDP port.",
          "parameters": obj(json!({ "port": n("Port number") }), &["port"]) },
        { "name": "kill_port",
          "description": "End the processes that listen on a port (frees it).",
          "parameters": obj(json!({ "port": n("Port number") }), &["port"]) },
        { "name": "hosts",
          "description": "The Windows hosts file. action=read returns it; action=write replaces it with `content` (read first, then send the whole new file). Writing needs the app to run as administrator; a backup is made first.",
          "parameters": obj(json!({ "action": s("read or write"), "content": s("Whole new hosts file (write only)") }), &["action"]) },
        { "name": "tunnel",
          "description": "Cloudflare tunnels (cloudflared). action=status lists installation and running tunnels; start_quick exposes local_url on a temporary public URL; stop ends the tunnel with `id`.",
          "parameters": obj(json!({ "action": s("status, start_quick or stop"), "local_url": s("e.g. http://localhost:3000"), "id": s("Tunnel id from status") }), &["action"]) },
        { "name": "runtime",
          "description": "Installed runtime versions. kind is node, jdk, maven or python. action=list shows versions, active shows the current one, switch changes it (by version or path).",
          "parameters": obj(json!({ "action": s("list, active or switch"), "kind": s("node, jdk, maven or python"), "version": s("Version to switch to"), "path": s("Install path to switch to") }), &["action", "kind"]) },
        { "name": "screenshot",
          "description": "Capture the primary screen. Returns the image; later coordinates are pixels of this image.",
          "parameters": obj(json!({}), &[]) },
        { "name": "mouse",
          "description": "Mouse at screenshot coordinates. action: move, click, double_click, right_click (x, y optional for clicks = current position), drag (x, y to to_x, to_y), scroll (amount notches, positive = up).",
          "parameters": obj(json!({ "action": s("move, click, double_click, right_click, drag or scroll"), "x": n("X"), "y": n("Y"), "to_x": n("Drag target X"), "to_y": n("Drag target Y"), "amount": n("Scroll notches") }), &["action"]) },
        { "name": "keyboard",
          "description": "Keyboard. action=type enters `text` (any language); action=key presses a combo in `keys`, e.g. ctrl+s, alt+tab, enter, win+r.",
          "parameters": obj(json!({ "action": s("type or key"), "text": s("Text to type"), "keys": s("Key combo") }), &["action"]) },
        { "name": "wait",
          "description": "Wait for the screen to update (max 10000 ms).",
          "parameters": obj(json!({ "ms": n("Milliseconds") }), &[]) }
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

/// `diff`: `preview` is a unified diff (file / hosts change) and is rendered with colours.
fn emit_approval(app: &AppHandle, id: &str, call_id: &str, tool: &str, summary: &str, preview: &str, diff: bool) {
    let _ = app.emit(
        "agent-approval",
        json!({
            "id": id,
            "callId": call_id,
            "tool": tool,
            "summary": summary,
            "preview": preview,
            "previewKind": if diff { "diff" } else { "text" },
        }),
    );
}

fn emit_text(app: &AppHandle, id: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = app.emit("generate-text-chunk", json!({ "id": id, "text": text }));
}

/// `limited`: the turn stopped at the round / action cap, so the UI offers「继续」.
fn emit_done(app: &AppHandle, id: &str, started: Instant, usage: Option<(u64, u64)>, limited: bool) {
    let mut payload = json!({
        "id": id,
        "elapsedMs": started.elapsed().as_millis() as u64,
        "limited": limited,
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
        "filter-branch",
    ];
    if BLOCKED.iter().any(|word| lower.contains(word)) {
        return true;
    }
    // git 规则按子命令和 token 判定（agent_policy::git_call_blocked）：
    // `git log --grep=rebase` 是查询，`git -C x rebase`、`reset  --hard`（多空格）也要拦住。
    if lower
        .split_whitespace()
        .any(|t| t == "--force" || t == "--force-with-lease" || t.starts_with("--force-with-lease="))
    {
        return true;
    }
    crate::agent_policy::git_calls(command)
        .iter()
        .any(crate::agent_policy::git_call_blocked)
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
    // cmd.exe chains with a single `&`, `>` / `<` redirect into files, `$(…)` runs a
    // PowerShell subexpression: any of them turns a query into something else.
    if lower.contains('&')
        || lower.contains(';')
        || lower.contains('|')
        || lower.contains('`')
        || lower.contains('>')
        || lower.contains('<')
        || lower.contains("$(")
    {
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

fn norm_app_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// How well a spoken name fits a program's display name (case and spaces ignored):
/// exact, name contains it, it contains the name, then its characters in order
/// ("360浏览器" → "360安全浏览器"). Mirrors `installedScore` in aiHub/localActions.ts.
fn app_name_score(want: &str, name: &str) -> u32 {
    let n = norm_app_name(want);
    let a = norm_app_name(name);
    if n.is_empty() || a.is_empty() {
        return 0;
    }
    if n == a {
        return 100;
    }
    if a.contains(&n) {
        return 80;
    }
    if a.chars().count() >= 2 && n.contains(&a) {
        return 70;
    }
    if n.chars().count() >= 3 {
        let mut rest = a.chars();
        if n.chars().all(|c| rest.any(|h| h == c)) {
            return 60;
        }
    }
    0
}

/// Indexes of the names that best fit `want`: only the top score tier, shortest name first.
fn match_app_name(want: &str, names: &[&str]) -> Vec<usize> {
    let scored: Vec<(usize, u32)> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (i, app_name_score(want, n)))
        .filter(|(_, s)| *s > 0)
        .collect();
    let Some(best) = scored.iter().map(|(_, s)| *s).max() else {
        return Vec::new();
    };
    let mut hits: Vec<usize> = scored.into_iter().filter(|(_, s)| *s == best).map(|(i, _)| i).collect();
    hits.sort_by_key(|&i| names[i].chars().count());
    hits
}

/// A single program to start: one best hit, or duplicate entries that share one name.
fn unique_hit(hits: &[usize], names: &[&str]) -> Option<usize> {
    let first = *hits.first()?;
    let key = norm_app_name(names[first]);
    hits.iter().all(|&i| norm_app_name(names[i]) == key).then_some(first)
}

fn candidates_error(hits: &[usize], names: &[&str]) -> String {
    let list: Vec<&str> = hits.iter().take(8).map(|&i| names[i]).collect();
    format!(
        "没有启动：有 {} 个程序都匹配，请用其中一个完整名字再调用 launch_app：{}",
        hits.len(),
        list.join("、")
    )
}

/// Start Menu / Desktop programs, rescanned at most every 10 minutes.
#[cfg(windows)]
fn installed_apps() -> Vec<crate::app_launcher_commands::InstalledAppInfo> {
    use crate::app_launcher_commands::InstalledAppInfo;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, Vec<InstalledAppInfo>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(600) {
            return list.clone();
        }
    }
    let list = crate::app_launcher_commands::scan_desktop_apps_sync();
    *cache = Some((Instant::now(), list.clone()));
    list
}

fn start_program(path: &str, args: Option<String>, label: &str) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("这个应用没有可执行路径".into());
    }
    #[cfg(windows)]
    {
        crate::app_launcher_commands::launch_app_inner(path.to_string(), args)?;
        Ok(format!("已启动 {label}"))
    }
    #[cfg(not(windows))]
    {
        let _ = (args, label);
        Err("当前系统不支持启动应用".into())
    }
}

/// Quick apps first; nothing there → programs found in the Start Menu / on the Desktop.
fn launch_named(apps: &[AgentAppShortcut], name: &str) -> Result<String, String> {
    let quick: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
    let hits = match_app_name(name, &quick);
    if let Some(i) = unique_hit(&hits, &quick) {
        let app = &apps[i];
        let args = (!app.args.trim().is_empty()).then(|| app.args.clone());
        return start_program(&app.path, args, &app.name);
    }
    if !hits.is_empty() {
        return Err(candidates_error(&hits, &quick));
    }
    #[cfg(windows)]
    {
        let installed = installed_apps();
        let names: Vec<&str> = installed.iter().map(|a| a.display_name.as_str()).collect();
        let hits = match_app_name(name, &names);
        if let Some(i) = unique_hit(&hits, &names) {
            return start_program(&installed[i].exe_path, None, &installed[i].display_name);
        }
        if !hits.is_empty() {
            return Err(candidates_error(&hits, &names));
        }
    }
    Err("快捷应用和本机已安装的程序（开始菜单 / 桌面快捷方式）里都没有这个名字".into())
}

#[derive(Debug, PartialEq)]
enum Decision {
    /// `for_turn`: also allow the same kind of action for the rest of this turn.
    Allow { for_turn: bool },
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
            Ok((true, for_turn)) => return Ok(Decision::Allow { for_turn }),
            Ok((false, _)) => return Ok(Decision::Deny),
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
    names.iter().any(|name| {
        !matches!(
            name.as_str(),
            "read_file" | "list_dir" | "grep" | "find_files" | "process_output" | "list_processes" | "find_port"
                | "screenshot" | "wait"
        )
    })
}

/// One-line label for the step log and approval card.
/// Reply to a tool name the model made up: point it at the real tool instead of
/// a bare "unknown tool" it will just retry.
fn unknown_tool_reason(name: &str, offered: &[&str]) -> String {
    let lower = name.to_lowercase();
    let shell_like = ["git", "shell", "bash", "cmd", "powershell", "exec", "terminal", "command"]
        .iter()
        .any(|k| lower.contains(k));
    let hint = if shell_like && offered.contains(&"run_command") {
        let example = if lower.starts_with("git") {
            let sub = lower.trim_start_matches("git").trim_start_matches(|c: char| c == '_' || c == '-' || c == ' ');
            format!("git {}", if sub.is_empty() { "status".to_string() } else { sub.replace('_', " ") })
        } else {
            "git status".to_string()
        };
        format!(
            "命令（包括所有 git 操作）一律用 run_command 执行，例如 run_command {{\"command\": \"{example} …\"}}；多个文件放进同一条命令。"
        )
    } else {
        String::new()
    };
    format!(
        "没有 {name} 这个工具，不要再调用它。{hint}可用的工具只有：{}。",
        offered.join(", ")
    )
}

fn call_summary(name: &str, args: &Value) -> String {
    let a = |k: &str| arg_str(args, k);
    match name {
        "read_file" => format!("读取 {}", a("path")),
        "list_dir" => format!("列出 {}", a("path")),
        "find_files" => format!("查找文件 {}", a("pattern")),
        "grep" => format!("搜索 {}", a("pattern")),
        "write_file" => format!("写入 {}", a("path")),
        "edit_file" => format!("修改 {}", a("path")),
        "delete_path" => format!("删除 {}", a("path")),
        "move_path" => format!("移动 {} → {}", a("from"), a("to")),
        "run_command" => format!("运行 {}", a("command")),
        "start_process" => format!("后台运行 {}", a("command")),
        "process_output" => format!("查看后台进程 #{}", args.get("id").map(|v| v.to_string()).unwrap_or_default()),
        "stop_process" => format!("停止后台进程 #{}", args.get("id").map(|v| v.to_string()).unwrap_or_default()),
        "open_page" => format!("打开页面 {}", a("tab")),
        "launch_app" => format!("启动 {}", a("name")),
        "open_target" => format!("打开 {}", a("target")),
        "clipboard_read" => "读取剪贴板".into(),
        "clipboard_write" => "写入剪贴板".into(),
        "list_processes" => format!("查看进程 {}", a("name")),
        "kill_process" => format!("结束进程 {}{}", a("name"), args.get("pid").map(|v| v.to_string()).unwrap_or_default()),
        "find_port" => format!("查看端口 {}", args.get("port").map(|v| v.to_string()).unwrap_or_default()),
        "kill_port" => format!("释放端口 {}", args.get("port").map(|v| v.to_string()).unwrap_or_default()),
        "hosts" | "tunnel" | "runtime" => format!("{name} {} {}", a("action"), a("kind")).trim_end().to_string(),
        "screenshot" => "截屏".into(),
        "mouse" => format!("鼠标 {} {}", a("action"), args.get("x").map(|x| format!("({x}, {})", args.get("y").map(|y| y.to_string()).unwrap_or_default())).unwrap_or_default()).trim_end().to_string(),
        "keyboard" => format!("键盘 {} {}", a("action"), if a("action") == "type" { clip(&a("text"), 30) } else { a("keys") }),
        "wait" => "等待".into(),
        _ => format!("未知工具 {name}"),
    }
}

/// The value `tool_gate` / `action_kind` look at besides the tool name.
fn gate_arg(name: &str, args: &Value) -> String {
    match name {
        "run_command" | "start_process" => arg_str(args, "command"),
        "hosts" | "tunnel" | "runtime" => arg_str(args, "action"),
        _ => String::new(),
    }
}

/// Tool result: text for the model, plus a PNG (base64) for `screenshot`.
struct ToolOutput {
    text: String,
    image: Option<String>,
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self { text, image: None }
    }
}

struct ToolCtx<'a> {
    app: &'a AppHandle,
    scope: &'a Scope,
    protected: &'a Protected,
    apps: &'a [AgentAppShortcut],
    request_id: &'a str,
}

/// Working directory for command tools: `cwd` inside the scope, else the base.
fn command_cwd(scope: &Scope, args: &Value) -> Result<std::path::PathBuf, String> {
    let cwd = arg_str(args, "cwd");
    let dir = if cwd.is_empty() { scope.base.clone() } else { scope.resolve(&cwd)? };
    if !dir.is_dir() {
        return Err(format!("工作目录不存在：{}", dir.display()));
    }
    Ok(dir)
}

/// Repository containing `path`: the nearest folder upwards with a `.git`.
fn repo_root_of(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut cur = if path.is_dir() { Some(path) } else { path.parent() };
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// The repository a call changes: git writes (honouring `-C`) and file writes.
/// Read-only git and other programs return None.
fn changed_repo(scope: &Scope, tool: &str, command: &str, args: &Value) -> Option<std::path::PathBuf> {
    let target = match tool {
        "run_command" | "start_process" => {
            let calls = crate::agent_policy::git_calls(command);
            if calls.is_empty() || command_is_readonly(command) {
                return None;
            }
            let cwd = command_cwd(scope, args).ok()?;
            match calls.iter().find_map(|c| c.dir.as_deref()) {
                Some(dir) => crate::agent_fs::resolve_free(&cwd, dir),
                None => cwd,
            }
        }
        "write_file" | "edit_file" | "delete_path" => scope.resolve(&arg_str(args, "path")).ok()?,
        "move_path" => scope.resolve(&arg_str(args, "from")).ok()?,
        _ => return None,
    };
    repo_root_of(&target)
}

/// Conflicts are the user's: while the repository is mid-merge or has unmerged
/// paths, the agent may look but not change it (any permission level).
fn conflict_reason(state: &crate::git_merge::ConflictState) -> Option<String> {
    let why = crate::git_merge::blocked_reason(state, "继续")?;
    Some(format!(
        "没有执行：{why} 冲突必须由用户来处理，不要自己解决或绕开。停下来，告诉用户哪些文件冲突，请他在 Git 管理 → 提交页解决。"
    ))
}

fn conflict_block(scope: &Scope, tool: &str, command: &str, args: &Value) -> Option<String> {
    let root = changed_repo(scope, tool, command, args)?;
    conflict_reason(&crate::git_merge::conflict_state(&root.to_string_lossy()))
}

async fn execute_tool(ctx: &ToolCtx<'_>, name: &str, args: &Value) -> Result<ToolOutput, String> {
    use crate::{agent_computer as pc, agent_fs as fs, agent_proc as proc, agent_sys as sys};
    let needs_files = matches!(
        name,
        "read_file" | "list_dir" | "find_files" | "grep" | "write_file" | "edit_file" | "delete_path" | "move_path"
            | "run_command" | "start_process"
    );
    if needs_files && !ctx.scope.has_files() {
        return Err("没有选择工作区，不能读写文件或执行命令。".into());
    }
    let out: String = match name {
        "read_file" => fs::read_file(ctx.scope, args)?,
        "list_dir" => fs::list_dir(ctx.scope, args)?,
        "find_files" => fs::find_files(ctx.scope, args, ctx.request_id)?,
        "grep" => fs::grep(ctx.scope, args, ctx.request_id)?,
        "write_file" | "edit_file" => fs::write_or_edit(ctx.scope, name, args, ctx.protected)?,
        "delete_path" => fs::delete_path(ctx.scope, args, ctx.protected)?,
        "move_path" => fs::move_path(ctx.scope, args, ctx.protected)?,
        "run_command" => {
            let cwd = command_cwd(ctx.scope, args)?;
            let (args, rid) = (args.clone(), ctx.request_id.to_string());
            tokio::task::spawn_blocking(move || proc::run_command(&cwd, &args, &rid))
                .await
                .map_err(|e| e.to_string())??
        }
        "start_process" => {
            let cwd = command_cwd(ctx.scope, args)?;
            let args = args.clone();
            tokio::task::spawn_blocking(move || proc::start_process(&cwd, &args)).await.map_err(|e| e.to_string())??
        }
        "process_output" => proc::process_output(args)?,
        "stop_process" => proc::stop_process(args)?,
        "open_page" => open_page(ctx.app, args)?,
        "launch_app" => launch_named(ctx.apps, &arg_str(args, "name"))?,
        "open_target" => sys::open_target(ctx.scope, args)?,
        "clipboard_read" => sys::clipboard_read()?,
        "clipboard_write" => sys::clipboard_write(args)?,
        "list_processes" => sys::list_processes(args)?,
        "kill_process" => sys::kill_process(args)?,
        "find_port" => sys::find_port(args).await?,
        "kill_port" => sys::kill_port(args).await?,
        "hosts" => sys::hosts(args).await?,
        "tunnel" => sys::tunnel(ctx.app, args).await?,
        "runtime" => sys::runtime(args).await?,
        "screenshot" => {
            let (text, png) = tokio::task::spawn_blocking(pc::screenshot).await.map_err(|e| e.to_string())??;
            return Ok(ToolOutput { text, image: Some(png) });
        }
        "mouse" => pc::mouse(args)?,
        "keyboard" => pc::keyboard(args)?,
        "wait" => {
            let args = args.clone();
            tokio::task::spawn_blocking(move || pc::wait(&args)).await.map_err(|e| e.to_string())??
        }
        _ => return Err(format!("未知工具 {name}")),
    };
    Ok(out.into())
}

/// Approval-card preview: a diff for file / hosts changes, else the command or a summary.
fn approval_preview(scope: &Scope, name: &str, args: &Value, summary: &str) -> (String, bool) {
    match name {
        "write_file" | "edit_file" => match crate::agent_fs::change_preview(scope, name, args) {
            Some(diff) => (diff, true),
            None => (clip(args.get("content").and_then(|v| v.as_str()).unwrap_or(""), PREVIEW_CAP), false),
        },
        "hosts" => match crate::agent_sys::hosts_preview(args) {
            Some(diff) => (diff, true),
            None => (summary.to_string(), false),
        },
        "run_command" | "start_process" => {
            let cwd = arg_str(args, "cwd");
            let shell = arg_str(args, "shell");
            let mut text = arg_str(args, "command");
            if !shell.is_empty() {
                text = format!("[{shell}] {text}");
            }
            if !cwd.is_empty() {
                text.push_str(&format!("\n（在 {cwd}）"));
            }
            (text, false)
        }
        "clipboard_write" => (clip(&arg_str(args, "text"), PREVIEW_CAP), false),
        "keyboard" => (clip(&arg_str(args, "text"), PREVIEW_CAP), false),
        _ => (summary.to_string(), false),
    }
}

const SHOT_PLACEHOLDER: &str = "[早先的截图已省略]";

/// Image parts in one message: OpenAI `image_url` parts, or Anthropic `image` blocks
/// (possibly nested inside `tool_result`).
fn count_images(content: &Value) -> usize {
    match content {
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p.get("type").and_then(|t| t.as_str()) {
                Some("image_url") | Some("image") => 1,
                Some("tool_result") => p.get("content").map(count_images).unwrap_or(0),
                _ => 0,
            })
            .sum(),
        _ => 0,
    }
}

fn strip_images(content: &mut Value) {
    if let Value::Array(parts) = content {
        for p in parts.iter_mut() {
            match p.get("type").and_then(|t| t.as_str()) {
                Some("image_url") | Some("image") => *p = json!({ "type": "text", "text": SHOT_PLACEHOLDER }),
                Some("tool_result") => {
                    if let Some(inner) = p.get_mut("content") {
                        strip_images(inner);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Keep only the newest `keep` screenshots in the conversation (they are large).
fn prune_images(messages: &mut [Value], keep: usize) {
    let mut seen = 0usize;
    for msg in messages.iter_mut().rev() {
        let Some(content) = msg.get_mut("content") else { continue };
        let n = count_images(content);
        if n == 0 {
            continue;
        }
        if seen >= keep {
            strip_images(content);
        }
        seen += n;
    }
}

/// Messages carrying this round's tool results (and screenshots) back to the model.
fn tool_result_messages(anthropic: bool, results: Vec<(String, ToolOutput)>) -> Vec<Value> {
    let data_url = |png: &str| format!("data:image/png;base64,{png}");
    if anthropic {
        let blocks: Vec<Value> = results
            .into_iter()
            .map(|(id, out)| {
                let content = match out.image {
                    Some(png) => json!([
                        { "type": "text", "text": out.text },
                        { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": png } },
                    ]),
                    None => json!(out.text),
                };
                json!({ "type": "tool_result", "tool_use_id": id, "content": content })
            })
            .collect();
        return vec![json!({ "role": "user", "content": blocks })];
    }
    // OpenAI tool messages carry text only: screenshots follow as one user message.
    let mut msgs = Vec::new();
    let mut images = Vec::new();
    for (id, out) in results {
        let text = if out.image.is_some() { format!("{}\n（截图在下一条消息里）", out.text) } else { out.text };
        msgs.push(json!({ "role": "tool", "tool_call_id": id, "content": text }));
        if let Some(png) = out.image {
            images.push(json!({ "type": "image_url", "image_url": { "url": data_url(&png) } }));
        }
    }
    if !images.is_empty() {
        let mut parts = vec![json!({ "type": "text", "text": "这是刚才 screenshot 的截图。" })];
        parts.extend(images);
        msgs.push(json!({ "role": "user", "content": parts }));
    }
    msgs
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

pub(crate) fn cancelled_message() -> String {
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
    // SSE may open with comment lines (OpenRouter sends ": OPENROUTER PROCESSING" keep-alives).
    let first_field = trimmed
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with(':'));
    if first_field.is_some_and(|l| l.starts_with("data:")) {
        return decode_sse(trimmed);
    }
    if first_field.is_none() {
        return Err(
            "模型只返回了心跳，没有内容。免费线路经常会中途断开，请再试一次，或换一个模型。".into(),
        );
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

/// Service folders under a workspace root, relative to it. Empty when `root` is
/// itself a repository (then git already works from the root).
fn workspace_services(root: &std::path::Path, services: &[String]) -> Vec<String> {
    if root.join(".git").exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for s in services {
        let Ok(p) = std::path::Path::new(s.trim()).canonicalize() else { continue };
        let Ok(rel) = p.strip_prefix(root) else { continue };
        let rel = rel.display().to_string().replace('\\', "/");
        if !rel.is_empty() && !out.contains(&rel) {
            out.push(rel);
        }
    }
    out
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
        match crate::agent_fs::repo_root(&req.repo_path) {
            Ok(p) => Some(p),
            Err(e) => {
                emit_err(&app, &request_id, &e);
                return Err(e);
            }
        }
    };
    let anthropic = req.config.provider.to_lowercase() == "anthropic";
    // 完全权限：整台电脑（受保护目录除外）；其它档位只在仓库里
    let scope = Scope::new(root.clone(), permission == Permission::Full);
    let protected = Protected::from_env();
    let reach = Reach { files: scope.has_files(), tools: req.tools, vision: req.vision };
    let mut tools_enabled = req.tools;
    let services = root.as_deref().map(|r| workspace_services(r, &req.services)).unwrap_or_default();
    let system = format!("{}{}", req.system, policy_text(permission, root.as_deref(), &services, reach));
    let tool_names = offered_tools(permission, reach);
    // Tools that exist at some level: those not offered here get the permission denial instead.
    let known_tools = offered_tools(Permission::Full, Reach { files: true, tools: true, vision: true });
    if tool_names.iter().any(|n| COMPUTER_TOOLS.contains(n)) {
        // The model must never see (or click) the AI hub window itself.
        crate::agent_computer::exclude_from_capture(&app);
    }
    let mut tools = provider_tools(&tool_names, anthropic);
    let ctx = ToolCtx { app: &app, scope: &scope, protected: &protected, apps: &req.apps, request_id: &request_id };
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
    // 「本轮同类都允许」记下的类别（见 agent_policy::action_kind），只对 Ask 档生效
    let mut granted: HashSet<String> = HashSet::new();
    // One "去提交页处理冲突" card per turn is enough.
    let mut conflict_noticed = false;
    // 模型编出来的工具名（如 git_add）：每个名字只在步骤里显示一次
    let mut unknown_shown: HashSet<String> = HashSet::new();
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
            let msg = cancelled_message();
            emit_err(&app, &request_id, &msg);
            return Err(msg);
        }
        prune_images(&mut messages, KEEP_SHOTS);
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
            emit_done(&app, &request_id, started, usage_total, false);
            return Ok(());
        }
        if anthropic {
            messages.push(json!({ "role": "assistant", "content": turn.assistant }));
        } else {
            messages.push(turn.assistant.clone());
        }
        let mut tool_results: Vec<(String, ToolOutput)> = Vec::new();
        for call in &turn.calls {
            if is_cancelled(&request_id) {
                let msg = cancelled_message();
                emit_err(&app, &request_id, &msg);
                return Err(msg);
            }
            let args = parse_args(&call.arguments);
            if !known_tools.contains(&call.name.as_str()) {
                let reason = unknown_tool_reason(&call.name, &tool_names);
                if unknown_shown.insert(call.name.clone()) {
                    emit_step(&app, &request_id, "unknown", "error", &format!("模型调用了不存在的工具 {}", call.name), &reason);
                }
                tool_results.push((call.id.clone(), reason.into()));
                continue;
            }
            let summary = call_summary(&call.name, &args);
            emit_step(&app, &request_id, &call.name, "running", &summary, "");
            let arg = gate_arg(&call.name, &args);
            let command = if call.name == "run_command" { arg.clone() } else { String::new() };
            let gate = tool_gate(permission, &call.name, &arg);
            if gate == Gate::Deny {
                let reason = deny_reason(permission, &call.name, &arg);
                emit_step(&app, &request_id, &call.name, "denied", &summary, &reason);
                notes.push(format!("{summary}（未执行）"));
                tool_results.push((call.id.clone(), reason.into()));
                continue;
            }
            if let Some(reason) = conflict_block(&scope, &call.name, &arg, &args) {
                emit_step(&app, &request_id, &call.name, "denied", &summary, &reason);
                if !conflict_noticed {
                    conflict_noticed = true;
                    emit_step(&app, &request_id, "conflict", "info", CONFLICT_NOTICE, &reason);
                }
                notes.push(format!("{summary}（有冲突，未执行）"));
                tool_results.push((call.id.clone(), reason.into()));
                continue;
            }
            let kind = action_kind(&call.name, &arg);
            let (preview, preview_is_diff) = approval_preview(&scope, &call.name, &args, &summary);
            // 审查托管：先让 AI 审一遍，只有明确放行才不打扰用户
            let mut review_note = String::new();
            let mut needs_confirm = gate == Gate::Ask && !granted.contains(&kind);
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
                emit_approval(&app, &request_id, &call.id, &call.name, &title, &preview, preview_is_diff);
                let decision = match wait_decision(&request_id, &call.id).await {
                    Ok(v) => v,
                    Err(e) => {
                        emit_err(&app, &request_id, &e);
                        return Err(e);
                    }
                };
                match decision {
                    Decision::Allow { for_turn } => {
                        // 审查转人工时不记「同类」：每条都要过审查
                        if for_turn && gate == Gate::Ask {
                            granted.insert(kind.clone());
                        }
                    }
                    Decision::Deny => {
                        emit_step(&app, &request_id, &call.name, "denied", &summary, "用户拒绝");
                        notes.push(format!("{summary}（已拒绝）"));
                        tool_results.push((call.id.clone(), "用户拒绝了这个操作".to_string().into()));
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
                            )
                            .into(),
                        ));
                        continue;
                    }
                }
            }
            let result = execute_tool(&ctx, &call.name, &args).await;
            let failed = match &result {
                Ok(out) => command_output_failed(&out.text),
                Err(_) => true,
            };
            match result {
                Ok(mut out) => {
                    out.text = repeated_command_result(&last_shell_command, last_shell_failed, &command, &out.text);
                    // A pull / merge / stash pop the AI ran left conflicts: hand over, don't resolve.
                    if let Some(root) = changed_repo(&scope, &call.name, &arg, &args) {
                        if let Some(reason) = conflict_reason(&crate::git_merge::conflict_state(&root.to_string_lossy())) {
                            out.text = format!("{}\n\n[Agent 提示] 出现冲突，已停止，不要自行解决。{reason}", out.text);
                            if !conflict_noticed {
                                conflict_noticed = true;
                                emit_step(&app, &request_id, "conflict", "info", CONFLICT_NOTICE, &reason);
                            }
                        }
                    }
                    // 写入的步骤详情带上 diff，用户能在步骤里看到改了什么
                    let detail = if preview_is_diff && matches!(call.name.as_str(), "write_file" | "edit_file" | "hosts") {
                        format!("{}\n\n{}", out.text, clip(&preview, DETAIL_CAP))
                    } else {
                        clip(&out.text, DETAIL_CAP)
                    };
                    emit_step(&app, &request_id, &call.name, "done", &summary, &detail);
                    notes.push(summary);
                    tool_results.push((call.id.clone(), out));
                }
                Err(err) if err.contains("AI_REQUEST_CANCELLED") => {
                    emit_err(&app, &request_id, &err);
                    return Err(err);
                }
                Err(err) if err == crate::agent_computer::FAILSAFE_MESSAGE => {
                    // 紧急停止：不再给模型机会继续动鼠标键盘
                    emit_step(&app, &request_id, &call.name, "denied", &summary, &err);
                    emit_text(&app, &request_id, &format!("\n\n{err}"));
                    emit_done(&app, &request_id, started, usage_total, false);
                    return Ok(());
                }
                Err(err) => {
                    emit_step(&app, &request_id, &call.name, "error", &summary, &clip(&err, DETAIL_CAP));
                    notes.push(format!("{summary}（失败）"));
                    let text = repeated_command_result(
                        &last_shell_command,
                        last_shell_failed,
                        &command,
                        &format!("错误: {err}"),
                    );
                    tool_results.push((call.id.clone(), text.into()));
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
        messages.extend(tool_result_messages(anthropic, tool_results));
    }
    let msg = limit_message(action_steps >= MAX_ACTION_STEPS, &notes);
    emit_text(&app, &request_id, &msg);
    emit_done(&app, &request_id, started, usage_total, true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        call_summary, candidates_error, unknown_tool_reason, command_blocked, command_is_readonly, command_output_failed, conflict_block,
        count_images, decode_model_body, gate_arg, limit_message, match_app_name, prune_images, repeated_command_result,
        round_is_action, tool_result_messages, unique_hit, workspace_services, ToolOutput, SHOT_PLACEHOLDER,
    };
    use crate::agent_proc::run_command;
    use serde_json::json;
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
    fn sse_with_keepalive_comments_is_still_sse() {
        let raw = concat!(
            ": OPENROUTER PROCESSING\n\n",
            ": OPENROUTER PROCESSING\n\n",
            "data: {\"id\":\"gen-1\",\"choices\":[{\"delta\":{\"content\":\"已提交\"}}]}\n\n",
            "data: [DONE]\n",
        );
        let body = decode_model_body(raw).unwrap();
        assert_eq!(
            body.pointer("/choices/0/message/content").and_then(|v| v.as_str()),
            Some("已提交")
        );
        let only_ping = decode_model_body(": OPENROUTER PROCESSING\n\n").unwrap_err();
        assert!(only_ping.contains("心跳"), "{only_ping}");
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

    #[test]
    fn blacklist_catches_git_spelled_other_ways() {
        assert!(command_blocked("git reset  --hard"));
        assert!(command_blocked("cd sub && git rebase --skip"));
        assert!(command_blocked("\"C:\\Program Files\\Git\\cmd\\git.exe\" push --force-with-lease=main"));
        assert!(command_blocked("git checkout --theirs src/a.ts"));
        assert!(!command_blocked("git merge -S other"));
        // Chaining, redirects and subexpressions are never read-only.
        assert!(!command_is_readonly("git status & del a.txt"));
        assert!(!command_is_readonly("git log > out.txt"));
        assert!(!command_is_readonly("git status $(Remove-Item x)"));
        assert!(command_is_readonly("git log --format=%s"));
    }

    /// While a repo is mid-merge the agent may look, but neither git writes nor
    /// file edits go through — at any permission level.
    #[test]
    fn merge_in_progress_blocks_writes_but_not_reads() {
        let root = std::env::temp_dir().join(format!("aw-agent-merge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        let root = root.canonicalize().unwrap();
        let scope = crate::agent_fs::Scope::new(Some(root.clone()), false);
        assert!(conflict_block(&scope, "run_command", "git add -A", &json!({})).is_none(), "clear repo");
        fs::write(root.join(".git").join("MERGE_HEAD"), "x").unwrap();
        let reason = conflict_block(&scope, "run_command", "git add -A", &json!({})).unwrap();
        assert!(reason.contains("合并中") && reason.contains("提交页"), "{reason}");
        assert!(conflict_block(&scope, "write_file", "", &json!({ "path": "a.txt" })).is_some());
        assert!(conflict_block(&scope, "run_command", "git status", &json!({})).is_none());
        assert!(conflict_block(&scope, "run_command", "npm test", &json!({})).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn made_up_tools_point_at_run_command() {
        let offered = ["read_file", "run_command"];
        let git = unknown_tool_reason("git_add", &offered);
        assert!(git.contains("没有 git_add") && git.contains("\"git add …\""), "{git}");
        assert!(git.contains("可用的工具只有：read_file, run_command"), "{git}");
        assert!(unknown_tool_reason("git_commit_all", &offered).contains("git commit all"));
        let other = unknown_tool_reason("browse_web", &offered);
        assert!(!other.contains("run_command {") && other.contains("可用的工具只有"), "{other}");
        assert!(!unknown_tool_reason("git_add", &["read_file"]).contains("run_command {"));
    }

    #[test]
    fn workspace_services_are_relative_and_skip_repo_roots() {
        let root = std::env::temp_dir().join(format!("aw-agent-ws-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("order").join(".git")).unwrap();
        fs::create_dir_all(root.join("svc").join("user").join(".git")).unwrap();
        let root = root.canonicalize().unwrap();
        let picked = vec![
            root.join("order").display().to_string(),
            root.join("svc").join("user").display().to_string(),
            root.join("order").display().to_string(),
            std::env::temp_dir().display().to_string(),
        ];
        assert_eq!(workspace_services(&root, &picked), vec!["order", "svc/user"]);
        let repo = root.join("order");
        assert!(workspace_services(&repo, &picked).is_empty(), "a repo root needs no service list");
        let _ = fs::remove_dir_all(&root);
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
    fn old_screenshots_become_placeholders() {
        let shot = |n: u8| {
            json!({ "role": "user", "content": [
                { "type": "text", "text": format!("shot {n}") },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } },
            ]})
        };
        let anthropic_shot = json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t",
            "content": [{ "type": "text", "text": "x" }, { "type": "image", "source": {} }] }] });
        let mut msgs = vec![shot(1), json!({ "role": "tool", "content": "ok" }), anthropic_shot, shot(3)];
        prune_images(&mut msgs, 2);
        assert_eq!(count_images(&msgs[0]["content"]), 0);
        assert_eq!(msgs[0]["content"][1]["text"], SHOT_PLACEHOLDER);
        assert_eq!(count_images(&msgs[2]["content"]), 1);
        assert_eq!(count_images(&msgs[3]["content"]), 1);
    }

    #[test]
    fn screenshots_ride_after_openai_tool_messages() {
        let results = vec![
            ("c1".to_string(), ToolOutput { text: "截图 10×10".into(), image: Some("PNG".into()) }),
            ("c2".to_string(), ToolOutput { text: "ok".into(), image: None }),
        ];
        let msgs = tool_result_messages(false, results);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[1]["tool_call_id"], "c2");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][1]["image_url"]["url"], "data:image/png;base64,PNG");
        let results = vec![("c1".to_string(), ToolOutput { text: "s".into(), image: Some("PNG".into()) })];
        let anth = tool_result_messages(true, results);
        assert_eq!(anth.len(), 1);
        assert_eq!(anth[0]["content"][0]["content"][1]["source"]["data"], "PNG");
    }

    #[test]
    fn summaries_and_gate_args_cover_new_tools() {
        assert_eq!(call_summary("edit_file", &json!({ "path": "a.ts" })), "修改 a.ts");
        assert_eq!(call_summary("start_process", &json!({ "command": "npm run dev" })), "后台运行 npm run dev");
        assert_eq!(gate_arg("hosts", &json!({ "action": "write" })), "write");
        assert_eq!(gate_arg("run_command", &json!({ "command": "git status" })), "git status");
        assert!(round_is_action(&["start_process".into()]));
        assert!(!round_is_action(&["screenshot".into(), "find_files".into()]));
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
        let run = |c: &str| run_command(&root, &json!({ "command": c }), "test-quote").unwrap();
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

    #[test]
    fn app_names_match_loosely_but_rank_exact_first() {
        let names = ["360安全浏览器", "360压缩", "Google Chrome", "微信", "微信开发者工具"];
        assert_eq!(match_app_name("360浏览器", &names), vec![0]);
        assert_eq!(match_app_name("chrome", &names), vec![2]);
        assert_eq!(match_app_name("google  chrome", &names), vec![2]);
        assert_eq!(match_app_name("微信", &names), vec![3]);
        assert!(match_app_name("记事本", &names).is_empty());
        // Two characters are too short for the in-order rule ("36" would hit everything).
        assert!(match_app_name("3览", &names).is_empty());
    }

    #[test]
    fn several_app_hits_become_candidates_not_a_launch() {
        let names = ["360安全浏览器", "360压缩", "360安全卫士"];
        let hits = match_app_name("360", &names);
        assert_eq!(hits.len(), 3);
        assert_eq!(unique_hit(&hits, &names), None);
        let msg = candidates_error(&hits, &names);
        assert!(msg.contains("360压缩") && msg.contains("360安全卫士") && msg.contains("没有启动"));
        // Duplicate shortcuts with one name still launch.
        let dup = ["Visual Studio Code", "visual studio code"];
        assert_eq!(unique_hit(&match_app_name("code", &dup), &dup), Some(0));
    }}
