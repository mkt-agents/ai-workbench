//! AI Hub tool loop. What a call may do is decided by `agent_policy` (permission
//! level); the tools themselves live in `agent_fs` / `agent_proc` / `agent_sys` /
//! `agent_computer`. Changes that need consent wait for `agent_decide`.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, UserAttentionType};
use tokio::sync::{mpsc, oneshot};

use crate::agent_fs::{arg_str, Protected, Scope};
use crate::agent_policy::{
    action_kind, deny_reason, filter_specs, offered_tools, parse_review_verdict, policy_text, tool_gate,
    Gate, Permission, Reach, REVIEW_SYSTEM,
};
use crate::agent_stream::{
    chunk_cache_hit, chunk_usage, delta_content, retry_plan, AnthropicStream, SseLines, ThinkFilter,
};
use crate::ai_commands::{
    apply_auth_headers, strip_think_tags, trim_trailing_slash, AIModelConfig, ChatTurn,
};
use crate::cancellation::{is_cancelled, CancelGuard};

const MAX_ACTION_STEPS: usize = 32;
const MAX_ROUNDS: usize = 48;
/// 单轮最多执行多少个工具调用；超出的回灌提示让模型分批（弱模型常常一次发十几个）。
const MAX_CALLS_PER_ROUND: usize = 8;
/// 同一批里并发执行的只读调用上限。
const MAX_PARALLEL: usize = 4;
/// 回灌给模型的单条工具结果上限（字符）。UI 用的是 `DETAIL_CAP`，两者是两回事：
/// 界面只给人看两千字，模型却拿到全文，几轮长日志就能把上下文吃掉一半。
const TOOL_RESULT_CAP: usize = 12_000;
/// 模型单轮输出上限，具体值仍由模型配置决定（`config.max_tokens`），这里只是天花板。
const MAX_OUTPUT_TOKENS: i64 = 8_192;
/// 429 / 5xx / 连接抖动的最多重试次数（不含首次尝试）。
const MAX_RETRIES: u32 = 3;
const PREVIEW_CAP: usize = 2_000;
/// Step detail sent to the UI (command output, errors, diffs).
const DETAIL_CAP: usize = 2_000;
/// Screenshots kept in the conversation; older ones become a text placeholder.
const KEEP_SHOTS: usize = 2;
/// Steps `update_plan` keeps: the hub card is a checklist, not a document.
const MAX_PLAN_STEPS: usize = 8;
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
    /// 备用模型（可选）：主模型这一轮彻底失败、而且一个字都没吐给用户时，换它把这一轮重来，
    /// 之后余下的回合都走它。前端挑一个同协议的其它模型填进来；空 = 不降级。
    #[serde(default)]
    pub fallback_config: Option<AIModelConfig>,
    pub system: String,
    pub user: String,
    pub request_id: String,
    pub repo_path: String,
    /// `repo_path` is a workspace folder: the service repositories picked inside it.
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    /// 内容块历史（新协议，见 `agent_protocol`）。非空时优先于 `history` + `user`，
    /// 让工具调用在历史里保留成可复用的 `tool_use` / `tool_result` 块。
    #[serde(default)]
    pub messages: Vec<crate::agent_protocol::Message>,
    /// 任务模式：`chat` / `code` / `doc` / `data` / `research` / `ops`。
    /// 空 = 按 `code` 处理（老前端不传时行为不变）。
    #[serde(default)]
    pub mode: String,
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
    /// 编辑档的写入也先给用户看 diff（设置项，默认关）。
    #[serde(default)]
    pub diff_confirm: bool,
    /// 用户说过「以后都允许」的 action_kind（设置项，跨轮生效，上限由前端维护）。
    #[serde(default)]
    pub always_allow: Vec<String>,
    /// `web_fetch` 的域名白名单（设置项）；空 = 不给这个工具。
    #[serde(default)]
    pub web_allow: Vec<String>,
    /// 是否把只读子代理 `task` 下发给模型（设置项，默认关）。
    #[serde(default)]
    pub subagents: bool,
    /// 用户启用并配置好的 MCP 服务；工具名是 `mcp_<server>_<tool>`。
    #[serde(default)]
    pub mcp: Vec<crate::mcp::McpServerSpec>,
    /// 只读调用是否允许并发（设置项，默认开）。关掉就一个个跑，步骤顺序更好读。
    #[serde(default = "default_true")]
    pub parallel_reads: bool,
    /// 429 / 5xx 是否自动重试（设置项，默认开）。关掉直接报错，免得重复计费。
    #[serde(default = "default_true")]
    pub retries: bool,
    /// 搜索是否仍屏蔽 `.gitignore` 里的文件（设置项，默认开）。
    #[serde(default = "default_true")]
    pub grep_ignore: bool,
    /// 长任务先让模型 `update_plan`（设置项，默认关）。
    #[serde(default)]
    pub plan_first: bool,
    /// 每轮上下文预算（token，设置项，默认 48k；0 = 用默认值）。
    #[serde(default)]
    pub context_tokens: usize,
    /// 单条工具结果回灌上限（字符，设置项，默认 12k；0 = 用默认值）。
    #[serde(default)]
    pub result_cap: usize,
    /// Anthropic 是否走流式（设置项，默认开；某些网关对 SSE 支持不好）。
    #[serde(default = "default_true")]
    pub anthropic_stream: bool,
    /// MCP 总开关（设置项，默认开；关掉这一轮不列任何 MCP 工具）。
    #[serde(default = "default_true")]
    pub mcp_enabled: bool,
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

/// User's answer on an approval card: allow?, "same kind for the rest of this turn",
/// "same kind from now on" (persisted by the front end as `settings.agentAlwaysAllow`).
type DecisionMsg = (bool, bool, bool);

static DECISIONS: Mutex<Option<HashMap<String, oneshot::Sender<DecisionMsg>>>> = Mutex::new(None);

/// 无 UI 调用方（企微渠道等）订阅某一 `request_id` 的文本流与审批提示。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum AgentBridgeEvent {
    Chunk(String),
    Done { limited: bool },
    Err(String),
    /// 工具闸门在等桌面端确认。`call_ids` 用于回灌答案（企微渠道据此在聊天里发审批码并解析回复）。
    ApprovalNeeded {
        call_ids: Vec<String>,
        summary: String,
        can_remember: bool,
    },
}

static TEXT_BRIDGES: Mutex<Option<HashMap<String, mpsc::UnboundedSender<AgentBridgeEvent>>>> =
    Mutex::new(None);

fn text_bridges(
) -> std::sync::MutexGuard<'static, Option<HashMap<String, mpsc::UnboundedSender<AgentBridgeEvent>>>>
{
    let mut guard = TEXT_BRIDGES.lock().expect("agent text bridges");
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    guard
}

/// 注册桥接；调用方在 `agent_run` 结束后应 `unsubscribe_agent_bridge`。
pub fn subscribe_agent_bridge(request_id: &str) -> mpsc::UnboundedReceiver<AgentBridgeEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    if let Some(map) = text_bridges().as_mut() {
        map.insert(request_id.to_string(), tx);
    }
    rx
}

pub fn unsubscribe_agent_bridge(request_id: &str) {
    if let Some(map) = text_bridges().as_mut() {
        map.remove(request_id);
    }
}

fn bridge_send(id: &str, ev: AgentBridgeEvent) {
    if let Some(map) = text_bridges().as_ref() {
        if let Some(tx) = map.get(id) {
            let _ = tx.send(ev);
        }
    }
}

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
    call_ids: Vec<String>,
    allow: bool,
    scope: Option<String>,
) -> Result<(), String> {
    // 只认正在跑的回合：主循环结束后迟到的答案、别的窗口的「代按」都不该生效。
    if !crate::cancellation::is_active(&request_id) {
        return Err(format!(
            "[{}] 这一轮已经结束，确认无效",
            crate::config::ErrorCode::AiApprovalStale.as_str()
        ));
    }
    // 合并审批把多个操作并成一张卡：一次决定把它们一起放行 / 拒绝。
    // 只把决定发给真正在等的那个 waiter（合并卡等的是第一个 call_id，其余只是跟着走）。
    let always = allow && scope.as_deref() == Some("always");
    let for_turn = allow && matches!(scope.as_deref(), Some("turn") | Some("always"));
    let mut guard = decisions();
    let map = guard.as_mut().expect("map");
    let mut answered = 0usize;
    for call_id in &call_ids {
        if let Some(tx) = map.remove(&decision_key(&request_id, call_id)) {
            let _ = tx.send((allow, for_turn, always));
            answered += 1;
        }
    }
    if answered == 0 {
        return Err(format!(
            "[{}] 没有等待确认的操作",
            crate::config::ErrorCode::AiApprovalStale.as_str()
        ));
    }
    Ok(())
}

/// 收尾时清掉这一回合的全部待决项：等待方已经返回，留着的 sender 只会被迟到的
/// `agent_decide` 命中，然后静默丢掉答案。
fn drop_decisions(request_id: &str) {
    let prefix = format!("{request_id}\u{1f}");
    if let Some(map) = decisions().as_mut() {
        map.retain(|key, _| !key.starts_with(&prefix));
    }
}

/// `agent_run` 每条返回路径都要清待决项；挂在 RAII 上比在每个 `return` 前补一行可靠。
struct ApprovalCleanup {
    request_id: String,
}

impl Drop for ApprovalCleanup {
    fn drop(&mut self) {
        drop_decisions(&self.request_id);
    }
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

/// Every tool the agent knows; `agent_policy::offered_tools` picks the ones a turn gets.
pub(crate) fn tool_specs() -> Value {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let n = |d: &str| json!({ "type": "integer", "description": d });
    let b = |d: &str| json!({ "type": "boolean", "description": d });
    json!([
        { "name": "update_plan",
          "description": "Publish your step list for a multi-step task, and update it as you go (send the whole list each time). The user sees it as a checklist; it is not the answer.",
          "parameters": obj(
              json!({ "steps": {
                  "type": "array",
                  "description": "Steps in order, at most 8",
                  "items": {
                      "type": "object",
                      "properties": {
                          "title": { "type": "string", "description": "Short imperative step" },
                          "status": { "type": "string", "description": "pending, in_progress or done" }
                      },
                      "required": ["title", "status"]
                  }
              } }),
              &["steps"]
          ) },
        { "name": "read_document",
          "description": "Read a document as plain text: docx (paragraph text), xlsx/xls (first sheet as TSV), pdf (text layer only, no OCR), or any text file (md/txt/csv/json). Layout is not reproduced. Files over 10MB are refused.",
          "parameters": obj(json!({ "path": s("File path"), "limit": n("Max characters (default 12000, up to 60000)") }), &["path"]) },
        { "name": "read_table",
          "description": "Load a CSV/TSV/XLSX and show its columns plus the first rows. For filtering or aggregation use table_query instead of reading thousands of rows.",
          "parameters": obj(json!({ "path": s("Table file"), "rows": n("Rows to preview (default 30, up to 200)") }), &["path"]) },
        { "name": "table_query",
          "description": "Run one read-only SELECT over a CSV/XLSX. The table is exposed as `t`, with the header row as column names. One statement only, no writes, at most 500 rows returned — aggregate instead of dumping rows.",
          "parameters": obj(json!({ "path": s("Table file"), "sql": s("SELECT … FROM t") }), &["path", "sql"]) },
        { "name": "remember",
          "description": "Save one durable fact to project memory (.ai-workbench/memory.md): a convention, a preference, a gotcha that will still matter next week. Not for temporary state — the write costs the user a confirmation.",
          "parameters": obj(json!({ "text": s("One sentence worth remembering"), "scope": s("project (default) or global") }), &["text"]) },
        { "name": "read_file",
          "description": "Read a text file with line numbers, in pages. Big files: read again with offset to continue.",
          "parameters": obj(json!({ "path": s("Relative to the repo, or absolute"), "offset": n("First line, 1-based (default 1)"), "limit": n("Lines to read (default 400)") }), &["path"]) },
        { "name": "list_dir",
          "description": "List one directory. Empty path = the repo root (or home folder without a repo).",
          "parameters": obj(json!({ "path": s("Directory") }), &[]) },
        { "name": "find_files",
          "description": "Find files by name with gitignore-style globs: *.tsx, use*Stream.ts, src/**/*.ts. Skips .gitignore'd files, node_modules, .git, target, dist.",
          "parameters": obj(json!({ "pattern": s("Wildcard pattern"), "path": s("Folder to search (default: repo root)"), "no_ignore": b("Also search files .gitignore excludes") }), &["pattern"]) },
        { "name": "grep",
          "description": "Search file contents (case-insensitive), honouring .gitignore. Literal text by default; set regex=true for a regular expression. mode=files lists matching files, mode=count counts per file. Optional glob limits file names.",
          "parameters": obj(json!({ "pattern": s("Text or regex"), "path": s("File or folder (default: repo root)"), "regex": b("Treat pattern as a regex"), "glob": s("File name filter, e.g. *.rs"), "mode": s("content (default), files or count"), "context": n("Lines of context around each match (0-5, default 0)"), "max": n("Max matches (default 50, up to 200)"), "no_ignore": b("Also search files .gitignore excludes") }), &["pattern"]) },
        { "name": "symbols",
          "description": "Where is a symbol defined? Heuristic scan (keyword + name: fn/def/class/interface/struct/impl/type/const…) over source files, returns path:line:signature. Use it before grepping a common name; it misses aliases and generated code, so cross-check with grep when the answer matters.",
          "parameters": obj(json!({ "query": s("Symbol name (or part of it)"), "path": s("File or folder (default: repo root)"), "kind": s("Only this kind: fn, class, interface, struct, impl, type, const…"), "max": n("Max hits (default 20, up to 50)") }), &["query"]) },
        { "name": "task",
          "description": "Hand one self-contained search question to a read-only sub-agent; it returns at most 800 characters of findings (paths + the lines that matter) instead of dumping matches into this conversation. It cannot change files, cannot ask the user, and cannot spawn another sub-agent. Give it a precise goal; do not use it for things you can check with one grep.",
          "parameters": obj(json!({ "goal": s("What to find out, specific enough to answer alone"), "path": s("Folder to start from (default: workspace root)") }), &["goal"]) },
        { "name": "web_fetch",
          "description": "Download a web page as plain text. https only, and only hosts the user put on the allowlist. No redirects are followed: if it reports a redirect, call again with the final address. Text is capped, so ask for the page you actually need.",
          "parameters": obj(json!({ "url": s("https://…") }), &["url"]) },
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
          "parameters": obj(json!({ "ms": n("Milliseconds") }), &[]) },
        // —— 企微业务（复用机器人 Bot ID/Secret，走 Open API；企微若未对机器人开放这些接口会回传 errcode） ——
        { "name": "wecom_doc_list",
          "description": "List WeChat Work docs you can access (by name search). Returns docid and url of each match.",
          "parameters": obj(json!({ "query": s("Name fragment to search, empty = list all") }), &[]) },
        { "name": "wecom_doc_read",
          "description": "Read one WeChat Work doc by its docid.",
          "parameters": obj(json!({ "docid": s("Doc id from wecom_doc_list") }), &["docid"]) },
        { "name": "wecom_doc_create",
          "description": "Create a new WeChat Work doc. doc_type: 1=doc, 2=slide, 3=smartdoc (default), 10=smartsheet.",
          "parameters": obj(json!({ "title": s("Doc name"), "doc_type": n("1 doc / 2 slide / 3 smartdoc(default) / 10 smartsheet") }), &["title"]) },
        { "name": "wecom_calendar_list",
          "description": "List the WeChat Work calendars available to this bot.",
          "parameters": obj(json!({}), &[]) },
        { "name": "wecom_calendar_create",
          "description": "Create a schedule/event on a WeChat Work calendar. start/end are unix seconds.",
          "parameters": obj(json!({ "calendar_id": s("Calendar id from wecom_calendar_list"), "title": s("Event summary"), "start": n("Start unix time (seconds)"), "end": n("End unix time (seconds)"), "attendees": s("Comma-separated userids, optional") }), &["calendar_id", "title", "start", "end"]) },
        { "name": "wecom_sheet_list",
          "description": "List WeChat Work smartsheets you can access (by name search).",
          "parameters": obj(json!({ "query": s("Name fragment to search, empty = list all") }), &[]) },
        { "name": "wecom_sheet_create",
          "description": "Create a new WeChat Work smartsheet.",
          "parameters": obj(json!({ "title": s("Smartsheet name") }), &["title"]) },
        { "name": "wecom_sheet_add_row",
          "description": "Append one or more rows to a WeChat Work smartsheet. fields is a JSON array of row objects, e.g. [{\"标题\":\"买菜\",\"状态\":\"待办\"}].",
          "parameters": obj(json!({ "sheet_id": s("Smartsheet docid from wecom_sheet_list"), "fields": s("JSON array of row objects") }), &["sheet_id", "fields"]) },
        { "name": "wecom_send_image",
          "description": "Send a local image file back to the current WeChat Work chat as an inline image. Use it after you produced or downloaded an image the user asked for. Path is absolute or workspace-relative.",
          "parameters": obj(json!({ "path": s("Path to an image file (png/jpg/gif/webp)") }), &["path"]) },
        { "name": "wecom_send_file",
          "description": "Send a local file back to the current WeChat Work chat. Text files are pasted as text; binary files are uploaded when the media endpoint is available, otherwise a note is sent. Use to deliver generated artifacts.",
          "parameters": obj(json!({ "path": s("Path to the file") }), &["path"]) }
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

/// Transport failures say nothing a developer can act on ("error sending request"):
/// name the likely cause, then keep the original text for the details drawer.
fn friendly_send_error(detail: &str) -> String {
    let lower = detail.to_lowercase();
    let hint = if lower.contains("timed out") || lower.contains("timeout") {
        "连接或读取超时：服务商没在时限内回应，稍后重试，或在模型配置里换一个服务商。"
    } else if lower.contains("dns") || lower.contains("name could not be resolved") {
        "域名解析失败：检查地址是否写对，以及本机网络 / 代理。"
    } else if lower.contains("ssl") || lower.contains("certificate") {
        "TLS 握手失败：地址的证书或 https 配置有问题。"
    } else if lower.contains("connection refused") || lower.contains("connect error") {
        "连不上服务：本地模型（Ollama / LM Studio）是否已启动、端口是否对？"
    } else {
        "请求没发出去：检查模型配置里的地址、密钥与本机网络。"
    };
    format!("{hint}｜原文：{detail}")
}

/// HTTP status → what to do next. The provider's body stays appended, unedited.
fn friendly_model_error(status: u16, snippet: &str) -> String {
    let head = match status {
        401 | 403 => "密钥无效或没有这个模型的权限（HTTP {s}）。到 AI 工作台 → 模型配置 核对 API Key 与模型名。",
        404 => "接口路径或模型名不对（HTTP {s}）。基础 URL 通常要到 /v1 为止，模型名以服务商列表里的写法为准。",
        429 => "被限流或额度用尽（HTTP {s}）。等一会儿再试，或换一个模型服务商。",
        400 if mentions_context(snippet) =>
            "这次发送的内容超出模型上下文（HTTP {s}）。开一个新对话，或减少附件与仓库上下文。",
        400 => "请求被拒绝（HTTP {s}）。多半是参数或模型名不对，具体看下面的原文。",
        s if (500..600).contains(&s) => "服务商暂时不可用（HTTP {s}）。稍后重试；本地模型请确认进程还在跑。",
        _ => "模型返回错误（HTTP {s}）。",
    }
    .replace("{s}", &status.to_string());
    format!("{head}｜原文：{snippet}")
}

fn mentions_context(body: &str) -> bool {
    let lower = body.to_lowercase();
    [
        "context length",
        "maximum context",
        "context_length",
        "too many tokens",
        "token count",
        "reduce your prompt",
        "input is too long",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push_str("\n…(truncated)");
    }
    out
}

/// 回灌给模型的工具结果上限：保留头尾 + 明确的续读指引。
/// `read_file` 自己有分页（`offset`）就提示续读；检索类提示缩小范围；其余只说截了多少。
fn clip_tool_result(name: &str, text: &str, cap: usize) -> String {
    let total = text.chars().count();
    if total <= cap {
        return text.to_string();
    }
    let head: String = text.chars().take(cap * 2 / 3).collect();
    let tail: String = text.chars().skip(total - cap / 3).collect();
    let advice = match name {
        "read_file" => "用 offset 接着读，不要重复读同一段",
        "grep" | "find_files" => "缩小 pattern 或 path 再查",
        _ => "需要细节请分步查",
    };
    format!("{head}\n\n…(已截断：原文 {total} 字符，{advice})\n\n{tail}")
}

fn emit_step(app: &AppHandle, id: &str, tool: &str, status: &str, summary: &str, detail: &str) {
    emit_step_full(app, id, tool, status, summary, detail, None);
}

/// `arg`: the call's primary argument, so the hub can render the step in the user's
/// own language. `summary` stays Chinese: it is also what the model and the folded
/// session digest read.
fn emit_step_full(
    app: &AppHandle,
    id: &str,
    tool: &str,
    status: &str,
    summary: &str,
    detail: &str,
    arg: Option<&str>,
) {
    let mut payload = json!({
        "id": id,
        "tool": tool,
        "status": status,
        "summary": summary,
        "detail": detail,
    });
    if let Some(arg) = arg.filter(|a| !a.is_empty()) {
        payload["arg"] = json!(arg);
    }
    let _ = app.emit("agent-step", payload);
}

/// First meaningful argument of a call (path / command / pattern / …), numbers included.
fn step_arg(args: &Value) -> String {
    for key in [
        "path", "command", "pattern", "target", "name", "from", "tab", "action", "kind", "port", "id", "keys",
        "text",
    ] {
        match args.get(key) {
            Some(Value::String(s)) if !s.trim().is_empty() => return s.trim().to_string(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

/// `diff`: `preview` is a unified diff (file / hosts change) and is rendered with colours.
/// The round is now blocked on the user, so the taskbar flashes until it is answered —
/// silently auto-denying after `APPROVAL_WAIT` was indistinguishable from a failure.
#[allow(clippy::too_many_arguments)] // 审批卡的字段就是这么多，拆结构体只是把参数换个地方写
fn emit_approval(
    app: &AppHandle,
    id: &str,
    call_id: &str,
    tool: &str,
    summary: &str,
    preview: &str,
    diff: bool,
    action_kind: &str,
    can_remember: bool,
) {
    let wait_ms = APPROVAL_WAIT.as_millis() as u64;
    let deadline_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64 + wait_ms)
        .unwrap_or(0);
    let _ = app.emit(
        "agent-approval",
        json!({
            "id": id,
            "callId": call_id,
            "tool": tool,
            "summary": summary,
            "preview": preview,
            "previewKind": if diff { "diff" } else { "text" },
            "deadlineAt": deadline_at,
            // 前端拿它写进「以后都允许」表；后端只认同一套 action_kind。
            "actionKind": action_kind,
            "canRemember": can_remember,
        }),
    );
    set_attention(app, true);
    bridge_send(
        id,
        AgentBridgeEvent::ApprovalNeeded {
            call_ids: vec![call_id.to_string()],
            summary: summary.to_string(),
            can_remember,
        },
    );
}

/// One operation inside a merged approval card.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalPatch {
    call_id: String,
    tool: String,
    summary: String,
    preview: String,
    /// `"diff"` — render with colours; `"text"` — plain.
    preview_kind: &'static str,
    action_kind: String,
}

/// 一个工具调用的「先决策、后执行」中间态：扫描阶段把所有判断算好，审批阶段合并，
/// 执行阶段按 tool_use 顺序落地。`skip` 非空 = 不执行，直接拿它当回灌理由。
#[derive(Default)]
struct Prepared {
    index: usize,
    call_id: String,
    name: String,
    summary: String,
    arg_text: String,
    args: Value,
    arg: String,
    command: String,
    kind: String,
    preview: String,
    preview_is_diff: bool,
    /// 扫描阶段遇到「跳过」就没算闸门；正常条目一定有值。
    gate: Option<Gate>,
    diff_confirming: bool,
    needs_confirm: bool,
    review_note: String,
    can_remember: bool,
    skip: Option<String>,
}

/// Merged approval: several changes in one round become one card instead of a stack.
/// `can_remember` is only true when every patch is the same `action_kind` — one click
/// cannot remember several kinds at once, and the "kind" is what gets stored.
fn emit_approval_batch(app: &AppHandle, id: &str, patches: &[ApprovalPatch], can_remember: bool) {
    let wait_ms = APPROVAL_WAIT.as_millis() as u64;
    let deadline_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64 + wait_ms)
        .unwrap_or(0);
    let call_ids: Vec<&str> = patches.iter().map(|p| p.call_id.as_str()).collect();
    let _ = app.emit(
        "agent-approval",
        json!({
            "id": id,
            // 老字段仍填第一项，保证单卡路径和合并路径用同一套前端状态
            "callId": patches[0].call_id,
            "tool": patches[0].tool,
            "summary": format!("本轮有 {} 个操作需要确认", patches.len()),
            "preview": "",
            "previewKind": "text",
            "deadlineAt": deadline_at,
            "actionKind": if can_remember { &patches[0].action_kind } else { "" },
            "canRemember": can_remember,
            "patches": patches,
            "callIds": call_ids,
        }),
    );
    set_attention(app, true);
    bridge_send(
        id,
        AgentBridgeEvent::ApprovalNeeded {
            call_ids: patches.iter().map(|p| p.call_id.clone()).collect(),
            summary: format!("本轮有 {} 个操作需要确认", patches.len()),
            can_remember,
        },
    );
}

/// The hub window is `skip_taskbar`, so it cannot flash a taskbar button of its own:
/// ask the main window too, and clear both once the round is no longer waiting.
fn set_attention(app: &AppHandle, on: bool) {
    for label in ["ai-hub", "main"] {
        if let Some(win) = app.get_webview_window(label) {
            let _ = win.request_user_attention(if on {
                Some(UserAttentionType::Critical)
            } else {
                None
            });
        }
    }
}

fn emit_text(app: &AppHandle, id: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let _ = app.emit("generate-text-chunk", json!({ "id": id, "text": text }));
    bridge_send(id, AgentBridgeEvent::Chunk(text.to_string()));
}

/// What the last round carried, for the answer source line. All fields are estimates
/// (`agent_budget` works on characters), so the UI labels them as such.
#[derive(Default, Clone, Copy)]
struct TurnStats {
    /// Characters of system prompt + conversation sent with the last round.
    context_chars: usize,
    /// Estimated share of the context budget that round used (0..1).
    budget_ratio: f32,
    /// Rounds where something had to be trimmed.
    trims: usize,
    /// Input tokens served from the provider's prompt cache.
    cache_hit: u64,
}

/// `limited`: the turn stopped at the round / action cap, so the UI offers「继续」.
fn emit_done(
    app: &AppHandle,
    id: &str,
    started: Instant,
    usage: Option<(u64, u64)>,
    limited: bool,
    stats: TurnStats,
) {
    set_attention(app, false);
    let mut payload = json!({
        "id": id,
        "elapsedMs": started.elapsed().as_millis() as u64,
        "limited": limited,
    });
    if let Some((prompt, completion)) = usage {
        payload["promptTokens"] = json!(prompt);
        payload["completionTokens"] = json!(completion);
    }
    if stats.context_chars > 0 {
        payload["contextChars"] = json!(stats.context_chars);
        // 三位小数够用，也避免 0.6200000000000001 这种浮点尾巴进 payload
        payload["budgetRatio"] = json!((stats.budget_ratio * 1000.0).round() / 1000.0);
        payload["trimmedRounds"] = json!(stats.trims);
    }
    if stats.cache_hit > 0 {
        payload["cacheHitTokens"] = json!(stats.cache_hit);
    }
    // 这一轮改过文件才给撤销入口：requestId 就是快照目录名
    if crate::agent_undo::has_snapshot(id) {
        payload["undoToken"] = json!(id);
    }
    let _ = app.emit("generate-text-done", payload);
    bridge_send(id, AgentBridgeEvent::Done { limited });
}

fn emit_err(app: &AppHandle, id: &str, msg: &str) {
    set_attention(app, false);
    let _ = app.emit("generate-text-error", json!({ "id": id, "error": msg }));
    bridge_send(id, AgentBridgeEvent::Err(msg.to_string()));
}

/// `Retry-After`（秒）。HTTP-date 形态忽略：实际遇到的都是秒数。
fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers().get("retry-after")?.to_str().ok()?.trim().parse().ok()
}

/// 退避重试补一条 info 步骤：否则用户只看到界面卡住，不知道在等什么。
fn emit_retry(app: &Option<&AppHandle>, id: &str, attempt: u32, wait: Duration, why: &str) {
    if let Some(app) = app {
        emit_step(
            app,
            id,
            "retry",
            "info",
            &format!("{why}，{:.1} 秒后自动重试（第 {} 次）", wait.as_secs_f64(), attempt + 1),
            "",
        );
    }
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
    /// `always`: the user also wants this kind remembered across turns.
    Allow { for_turn: bool, always: bool },
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
            Ok((true, for_turn, always)) => return Ok(Decision::Allow { for_turn, always }),
            Ok((false, _, _)) => return Ok(Decision::Deny),
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

/// 回合结束时的 Stop hooks：异步跑、失败不影响答案（没有消费者读它的输出）。
/// 只读权限不跑——hooks 等于执行用户脚本。
fn run_stop_hooks(hooks: &[crate::harness_config::HookEntry], scope: &Scope, permission: Permission) {
    if permission == Permission::Read {
        return;
    }
    let cwd = scope.root.clone().unwrap_or_else(|| scope.base.clone());
    for hook in hooks.iter().filter(|h| h.event == "stop") {
        let hook = hook.clone();
        let cwd = cwd.clone();
        tokio::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || crate::agent_hooks::run(&hook, &cwd, "stop")).await;
        });
    }
}

fn round_is_action(names: &[String]) -> bool {
    // 「算不算动作步」由 `agent_tools::TOOLS` 声明，别在这里再抄一份名字表。
    names.iter().any(|name| crate::agent_tools::counts_as_action(name))
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
            let sub = lower.trim_start_matches("git").trim_start_matches(['_', '-', ' ']);
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
        "symbols" => format!("定位符号 {}", a("query")),
        "web_fetch" => format!("抓取 {}", a("url")),
        "task" => format!("子代理：{}", a("goal")),
        // MCP 工具名是运行时的：只留服务器与工具名，别把整串参数塞进步骤行
        _ if name.starts_with("mcp_") => format!("MCP {}", name.trim_start_matches("mcp_")),
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
        "update_plan" => format!("更新计划（{} 步）", plan_steps(args).len()),
        _ => format!("未知工具 {name}"),
    }
}

/// Steps the model sent to `update_plan`, as `{ title, status }` with a known status.
/// Empty titles are dropped; more than `MAX_PLAN_STEPS` are cut (weak models send essays).
fn plan_steps(args: &Value) -> Vec<Value> {
    let Some(arr) = args.get("steps").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for step in arr.iter().take(MAX_PLAN_STEPS) {
        let title = step
            .get("title")
            .and_then(|v| v.as_str())
            .or_else(|| step.as_str())
            .unwrap_or("")
            .trim();
        if title.is_empty() {
            continue;
        }
        let status = match step.get("status").and_then(|v| v.as_str()).unwrap_or("") {
            "done" | "completed" | "complete" => "done",
            "in_progress" | "doing" | "active" => "in_progress",
            _ => "pending",
        };
        // 稳定 id：模型每轮重发整张清单，前端靠它保持行身份，进度高亮不整卡重画
        let id = format!("s{}", out.len() + 1);
        out.push(json!({ "id": id, "title": title, "status": status }));
    }
    out
}

/// `update_plan`: broadcast the model's checklist, and tell it what the user now sees.
fn update_plan(app: &AppHandle, request_id: &str, args: &Value) -> Result<String, String> {
    let steps = plan_steps(args);
    if steps.is_empty() {
        return Err("计划里没有有效步骤：steps 要是 [{ \"title\": …, \"status\": … }]。".into());
    }
    let done = steps.iter().filter(|s| s["status"] == "done").count();
    let current = steps
        .iter()
        .find(|s| s["status"] == "in_progress")
        .and_then(|s| s["title"].as_str())
        .unwrap_or("");
    let _ = app.emit("agent-plan", json!({ "id": request_id, "steps": steps }));
    Ok(if current.is_empty() {
        format!("计划已更新：共 {} 步，已完成 {done} 步。", steps.len())
    } else {
        format!("计划已更新：共 {} 步，已完成 {done} 步，进行中：{current}。", steps.len())
    })
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
    /// `web_fetch` 只认这些域名（空 = 已经不该被下发）。
    web_allow: &'a [String],
    client: &'a reqwest::Client,
    /// 本轮可用的 MCP 服务与它们的工具（工具名 → 服务/工具是多对一查找，不靠拆字符串）。
    mcp: &'a [(crate::mcp::McpServerSpec, Vec<crate::mcp::McpTool>)],
    /// 项目 hooks（`.ai-workbench/hooks.json`）；只读权限下不执行。
    hooks: &'a [crate::harness_config::HookEntry],
    /// 只读调用并发上限（设置项「并发读取」，关掉 = 1，逐个跑）。
    max_parallel: usize,
    /// 429 / 5xx 的重试次数（设置项「自动重试」，关掉 = 0）。
    max_retries: u32,
    /// 搜索是否仍屏蔽 `.gitignore` 里的文件（设置项「搜索时跳过被忽略的文件」）。
    grep_ignore: bool,
    /// 单条工具结果回灌上限（设置项「工具结果上限」，默认 12000 字符）。
    result_cap: usize,
}

/// List every enabled MCP server's tools once per turn, appending the specs the model sees.
/// One broken server (bad command, hang, empty list) only costs a notice step.
async fn attach_mcp_tools(
    app: &AppHandle,
    request_id: &str,
    servers: &[crate::mcp::McpServerSpec],
    tools: &mut Vec<Value>,
    anthropic: bool,
    announce: bool,
) -> Vec<(crate::mcp::McpServerSpec, Vec<crate::mcp::McpTool>)> {
    let mut out = Vec::new();
    for spec in servers {
        let probe = spec.clone();
        let listed = tokio::task::spawn_blocking(move || crate::mcp::list_tools(&probe)).await;
        match listed {
            Ok(Ok(list)) if !list.is_empty() => {
                for tool in &list {
                    tools.push(crate::mcp::provider_spec(&spec.name, tool, anthropic));
                }
                out.push((spec.clone(), list));
            }
            Ok(Ok(_)) => {
                if announce {
                    emit_step(
                        app,
                        request_id,
                        "mcp",
                        "info",
                        &format!("MCP 服务 {} 没有提供工具", spec.name),
                        "",
                    );
                }
            }
            Ok(Err(e)) => {
                if announce {
                    emit_step(app, request_id, "mcp", "error", &format!("MCP 服务 {} 不可用", spec.name), &e);
                }
            }
            Err(e) => {
                if announce {
                    emit_step(
                        app,
                        request_id,
                        "mcp",
                        "error",
                        &format!("MCP 服务 {} 启动失败", spec.name),
                        &e.to_string(),
                    );
                }
            }
        }
    }
    out
}

/// `mcp_fs_read_file` → the server spec and the original tool name it came from.
fn resolve_mcp(
    servers: &[(crate::mcp::McpServerSpec, Vec<crate::mcp::McpTool>)],
    full: &str,
) -> Option<(crate::mcp::McpServerSpec, String)> {
    for (spec, tools) in servers {
        for tool in tools {
            if crate::mcp::full_name(&spec.name, &tool.name) == full {
                return Some((spec.clone(), tool.name.clone()));
            }
        }
    }
    None
}

/// System prompt for the read-only sub-agent: findings only, it cannot change anything
/// and it cannot ask the user, so it must not promise to.
const TASK_SYSTEM: &str = "You are a read-only research sub-agent inside a coding agent. \
Use the file tools to answer the question. You cannot change, create or delete anything, and you cannot ask the user \
anything. Answer with findings only: the file paths and the one line that matters, at most 800 characters. If you find \
nothing, say which patterns you tried.";

/// Rounds the sub-agent may take before giving up.
const TASK_ROUNDS: usize = 16;
/// Characters of the sub-agent's answer that reach the main conversation.
const TASK_REPORT_CAP: usize = 800;
/// Tools a sub-agent may use: strictly read-only, and never `task` itself (no nesting).
/// `web_fetch` is in the list but only *offered* when this permission level would not ask
/// (see [`subagent_tools`]) — a sub-agent has no approval card to show.
const TASK_TOOLS: &[&str] = &[
    "read_file", "list_dir", "find_files", "grep", "symbols", "process_output", "list_processes", "find_port",
    "web_fetch",
];

/// What a sub-agent gets in this permission level.
///
/// Every [`TASK_TOOLS`] entry is Allow-gated except `web_fetch`, which asks the user in
/// 只读 / 编辑 / 审查档. A sub-agent has no channel to ask, so it simply does not get that
/// tool there; an empty whitelist also means "no web" (same rule as `offered_tools`).
fn subagent_tools(permission: Permission, web_allow: &[String]) -> Vec<&'static str> {
    let may_fetch = !web_allow.is_empty() && tool_gate(permission, "web_fetch", "") == Gate::Allow;
    TASK_TOOLS.iter().copied().filter(|name| *name != "web_fetch" || may_fetch).collect()
}

/// `task`: run a small read-only loop and bring back only the conclusion, so a wide search
/// does not fill the main context with raw matches.
async fn run_subagent(
    ctx: &ToolCtx<'_>,
    config: &AIModelConfig,
    permission: Permission,
    goal: &str,
    path: &str,
) -> Result<String, String> {
    let anthropic = config.provider.to_lowercase() == "anthropic";
    let start = if path.trim().is_empty() { ctx.scope.base.display().to_string() } else { path.trim().to_string() };
    let system = format!("{TASK_SYSTEM}\nStart from: {start}");
    let allowed = subagent_tools(permission, ctx.web_allow);
    let tools = provider_tools(&allowed, anthropic);
    let mut messages: Vec<Value> = vec![json!({ "role": "user", "content": goal })];
    // 子代理读过的文件（去重、相对路径）：结论旁边附一份，主代理和用户都知道它看了哪里
    let mut touched: Vec<String> = Vec::new();
    for _ in 0..TASK_ROUNDS {
        if is_cancelled(ctx.request_id) {
            return Err(cancelled_message());
        }
        let round = model_turn(
            ctx.client,
            config,
            anthropic,
            &Value::Array(messages.clone()),
            &system,
            &tools,
            None,
            ctx.request_id,
            ctx.max_retries,
        )
        .await
        .map_err(String::from)?;
        let turn = round.turn;
        if turn.calls.is_empty() {
            let text = strip_think_tags(&turn.text);
            let text = text.trim();
            if text.is_empty() {
                return Err("子代理没有给出结论".into());
            }
            let capped: String = text.chars().take(TASK_REPORT_CAP).collect();
            let mut report = if text.chars().count() > TASK_REPORT_CAP {
                format!("{capped}…（子代理结论已截断）")
            } else {
                capped
            };
            // 结论被截断时文件清单照样存活：主代理至少知道该去哪里自己看
            if !touched.is_empty() {
                let list: Vec<String> = touched.iter().take(8).cloned().collect();
                report.push_str(&format!("\n[引用文件] {}", list.join("、")));
            }
            return Ok(report);
        }
        if anthropic {
            messages.push(json!({ "role": "assistant", "content": turn.assistant }));
        } else {
            messages.push(turn.assistant.clone());
        }
        let mut results: Vec<(String, ToolOutput)> = Vec::new();
        for (index, call) in turn.calls.iter().enumerate() {
            if index >= MAX_CALLS_PER_ROUND {
                results.push((call.id.clone(), "子代理本轮调用过多，这个没有执行。".to_string().into()));
                continue;
            }
            // 白名单之外（包括 task 自己）不执行：子代理不能再派子代理
            if !allowed.contains(&call.name.as_str()) {
                results.push((
                    call.id.clone(),
                    format!("子代理只能用只读工具，{} 不在其中。", call.name).into(),
                ));
                continue;
            }
            let args = parse_args(&call.arguments).unwrap_or_else(|_| json!({}));
            // read_file / list_dir 的 path 记下来：结论旁附一份「引用文件」
            if matches!(call.name.as_str(), "read_file" | "list_dir") {
                if let Ok(p) = ctx.scope.resolve(&crate::agent_fs::arg_str(&args, "path")) {
                    let shown = p
                        .strip_prefix(&ctx.scope.base)
                        .unwrap_or(&p)
                        .display()
                        .to_string();
                    if !shown.is_empty() && !touched.contains(&shown) && touched.len() < 12 {
                        touched.push(shown);
                    }
                }
            }
            // 只走只读执行体：不经过 execute_tool，递归就断在这里（也就没有 async 递归）。
            let out = if call.name == "web_fetch" {
                let url = crate::agent_fs::arg_str(&args, "url");
                match await_cancellable(crate::agent_web::web_fetch(&url, ctx.web_allow), ctx.request_id).await {
                    Ok(Ok(text)) => ToolOutput::from(text),
                    Ok(Err(e)) => ToolOutput::from(format!("错误：{e}")),
                    Err(cancelled) => return Err(cancelled),
                }
            } else if is_readonly_tool(&call.name) {
                execute_readonly(ctx.scope, ctx.request_id, &call.name, &args, ctx.grep_ignore)
                    .unwrap_or_else(|e| ToolOutput::from(format!("错误：{e}")))
            } else {
                match call.name.as_str() {
                    "process_output" => crate::agent_proc::process_output(&args).map(ToolOutput::from),
                    "list_processes" => crate::agent_sys::list_processes(&args).map(ToolOutput::from),
                    "find_port" => crate::agent_sys::find_port(&args).await.map(ToolOutput::from),
                    other => Err(format!("子代理不能调用 {other}")),
                }
                .unwrap_or_else(|e| ToolOutput::from(format!("错误：{e}")))
            };
            results.push((call.id.clone(), out));
        }
        messages.extend(tool_result_messages(anthropic, results));
    }
    Ok("（子代理到 16 轮上限，没有给出结论；缩小范围再试，或直接用 grep）".into())
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

/// 完整工具管道：pre(hooks) → execute → post_hooks → finalize（结果预算）。
/// 调度循环只认这一层，方便加观测；prefetched 只读路径事后也走 post + finalize。
async fn dispatch_tool(
    ctx: &ToolCtx<'_>,
    config: &AIModelConfig,
    permission: Permission,
    name: &str,
    args: &Value,
) -> Result<ToolOutput, String> {
    let result = execute_tool(ctx, config, permission, name, args).await;
    finalize_tool_result(ctx, permission, name, result).await
}

async fn finalize_tool_result(
    ctx: &ToolCtx<'_>,
    permission: Permission,
    name: &str,
    mut result: Result<ToolOutput, String>,
) -> Result<ToolOutput, String> {
    // PostToolUse：脚本输出追加到结果末尾（agent_hooks 已截到 2k）。
    if permission != Permission::Read && !ctx.hooks.is_empty() {
        let post: Vec<_> = ctx
            .hooks
            .iter()
            .filter(|h| h.event == "postToolUse" && crate::agent_hooks::matches(&h.matcher, name))
            .cloned()
            .collect();
        if !post.is_empty() {
            let payload = match &result {
                Ok(out) => format!("{} {}", name, out.text.chars().take(500).collect::<String>()),
                Err(err) => format!("{} error={}", name, err.chars().take(300).collect::<String>()),
            };
            let cwd = ctx.scope.root.clone().unwrap_or_else(|| ctx.scope.base.clone());
            let mut extra = String::new();
            for hook in post {
                let cwd = cwd.clone();
                let payload = payload.clone();
                if let Ok(outcome) =
                    tokio::task::spawn_blocking(move || crate::agent_hooks::run(&hook, &cwd, &payload)).await
                {
                    if !outcome.output.is_empty() {
                        extra.push_str(&format!("\n[hook] {}", outcome.output));
                    }
                }
            }
            if !extra.is_empty() {
                if let Ok(out) = &mut result {
                    out.text.push_str(&extra);
                }
            }
        }
    }
    if let Ok(out) = &mut result {
        out.text = clip_tool_result(name, &out.text, ctx.result_cap);
    }
    result
}

async fn execute_tool(
    ctx: &ToolCtx<'_>,
    config: &AIModelConfig,
    permission: Permission,
    name: &str,
    args: &Value,
) -> Result<ToolOutput, String> {
    use crate::{agent_computer as pc, agent_fs as fs, agent_proc as proc, agent_sys as sys};
    // 项目 hooks：只读权限不跑（hooks 等于执行任意用户脚本）。
    // PreToolUse 非零退出 = 拒绝这次调用，把 hook 的输出回灌给模型。
    if permission != Permission::Read && !ctx.hooks.is_empty() {
        let hook_cwd = ctx.scope.root.clone().unwrap_or_else(|| ctx.scope.base.clone());
        let payload = format!("{name} {}", args);
        for hook in ctx
            .hooks
            .iter()
            .filter(|h| h.event == "preToolUse" && crate::agent_hooks::matches(&h.matcher, name))
        {
            let hook = hook.clone();
            let cwd = hook_cwd.clone();
            let payload = payload.clone();
            let outcome = tokio::task::spawn_blocking(move || crate::agent_hooks::run(&hook, &cwd, &payload))
                .await
                .map_err(|e| format!("hook 线程失败：{e}"))?;
            if !outcome.ok {
                return Err(format!("hook 拒绝执行 {name}：{}", outcome.output));
            }
        }
    }
    let needs_files = matches!(
        name,
        "read_file" | "list_dir" | "find_files" | "grep" | "write_file" | "edit_file" | "delete_path" | "move_path"
            | "run_command" | "start_process" | "read_document" | "read_table" | "table_query"
    );
    if needs_files && !ctx.scope.has_files() {
        return Err("没有选择工作区，不能读写文件或执行命令。".into());
    }
    let out: String = match name {
        // 长期记忆：写项目或全局的 memory.md（闸门是 Change，编辑档先问一次）
        "remember" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("project");
            let path = crate::agent_memory::append(ctx.scope.root.as_deref(), text, scope)?;
            format!("已记住（{}）：{}", path, text.trim())
        }
        // 文档 / 表格：非代码任务的主力，只读、可并发
        "read_document" => {
            let path = ctx
                .scope
                .resolve(args.get("path").and_then(|v| v.as_str()).unwrap_or(""))?;
            let cap = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(12_000).min(60_000) as usize;
            crate::agent_docs::read_document(&path, cap)?
        }
        "read_table" => {
            let path = ctx
                .scope
                .resolve(args.get("path").and_then(|v| v.as_str()).unwrap_or(""))?;
            let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(30).clamp(1, 200) as usize;
            let (header, data) = crate::agent_docs::read_rows(&path, rows)?;
            let preview = data.iter().map(|r| r.join("\t")).collect::<Vec<_>>().join("\n");
            format!("列：{}\n\n{}", header.join(" | "), preview)
        }
        "table_query" => {
            let path = ctx
                .scope
                .resolve(args.get("path").and_then(|v| v.as_str()).unwrap_or(""))?;
            let sql = args.get("sql").and_then(|v| v.as_str()).unwrap_or("");
            crate::agent_table::query(&path, sql, crate::agent_table::ROW_LIMIT)?
        }
        // 只读文件工具走同一个执行体：并发批会把它搬到后台线程上跑
        "read_file" | "list_dir" | "find_files" | "grep" | "symbols" => {
            return execute_readonly(ctx.scope, ctx.request_id, name, args, ctx.grep_ignore);
        }
        "write_file" | "edit_file" => {
            // 落盘前留一份原文件：这一轮的「撤销这次改动」靠它逐字节还原
            if let Ok(path) = ctx.scope.resolve(&fs::arg_str(args, "path")) {
                crate::agent_undo::snapshot_before(ctx.request_id, &path);
            }
            fs::write_or_edit(ctx.scope, name, args, ctx.protected)?
        }
        "delete_path" => {
            // 删除也留一份副本：撤销不用去回收站翻（目录不复制，回收站就是它的撤销）
            if let Ok(path) = ctx.scope.resolve(&fs::arg_str(args, "path")) {
                if path.is_file() && ctx.protected.check(&path, true).is_none() {
                    crate::agent_undo::snapshot_before_delete(ctx.request_id, &path);
                }
            }
            fs::delete_path(ctx.scope, args, ctx.protected)?
        }
        "move_path" => {
            // 移动不留字节，只记「从哪到哪」：撤销就是反向改名
            let from = ctx.scope.resolve(&fs::arg_str(args, "from"));
            let to = ctx.scope.resolve(&fs::arg_str(args, "to"));
            if let (Ok(from), Ok(to)) = (from, to) {
                if from.exists() && !to.exists() {
                    crate::agent_undo::snapshot_before_move(ctx.request_id, &from, &to);
                }
            }
            fs::move_path(ctx.scope, args, ctx.protected)?
        }
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
            // `spawn_blocking` 停不下来：这一轮被取消时它照样会把进程起起来。所以让它把编号
            // 送回异步侧——接收端已经没了（= 这一轮已取消）就当场按编号停掉，别留孤儿进程。
            let (tx, rx) = tokio::sync::oneshot::channel::<Result<(u32, String), String>>();
            tokio::task::spawn_blocking(move || match proc::start_process_id(&cwd, &args) {
                Ok(started) => {
                    if let Err(Ok((id, _))) = tx.send(Ok(started)) {
                        let _ = proc::stop_process(&json!({ "id": id }));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            });
            match await_cancellable(rx, ctx.request_id).await {
                Ok(Ok(started)) => started?.1,
                Ok(Err(_)) => return Err("启动后台进程的线程异常退出".into()),
                Err(cancelled) => return Err(cancelled),
            }
        }
        "process_output" => proc::process_output(args)?,
        "stop_process" => proc::stop_process(args)?,
        "open_page" => open_page(ctx.app, args)?,
        "launch_app" => launch_named(ctx.apps, &arg_str(args, "name"))?,
        "open_target" => sys::open_target(ctx.scope, args)?,
        "clipboard_read" => sys::clipboard_read()?,
        "web_fetch" => {
            let url = arg_str(args, "url");
            await_cancellable(crate::agent_web::web_fetch(&url, ctx.web_allow), ctx.request_id).await??
        }
        "task" => {
            let goal = arg_str(args, "goal");
            if goal.is_empty() {
                return Err("缺少 goal：写清楚要查什么".into());
            }
            run_subagent(ctx, config, permission, &goal, &arg_str(args, "path")).await?
        }
        _ if name.starts_with("mcp_") => {
            let (server, tool) = resolve_mcp(ctx.mcp, name)
                .ok_or_else(|| format!("{name} 不在本轮启用的 MCP 工具里"))?;
            let args = args.clone();
            // MCP 调用是阻塞的整段会话：放到阻塞线程里，停止键照样能收住
            let fut = tokio::task::spawn_blocking(move || crate::mcp::call_tool(&server, &tool, &args));
            await_cancellable(fut, ctx.request_id).await?.map_err(|e| e.to_string())??
        }
        "clipboard_write" => sys::clipboard_write(args)?,
        "list_processes" => sys::list_processes(args)?,
        "kill_process" => sys::kill_process(args)?,
        "find_port" => sys::find_port(args).await?,
        "kill_port" => sys::kill_port(args).await?,
        // 企微业务：文档 / 日程 / 智能表格，复用机器人通道凭证，走 Open API
        "wecom_doc_list" | "wecom_doc_read" | "wecom_doc_create"
        | "wecom_calendar_list" | "wecom_calendar_create"
        |         "wecom_sheet_list" | "wecom_sheet_create" | "wecom_sheet_add_row" => {
            crate::wecom_biz::run(ctx.app.clone(), name, args).await?
        }
        // 企微出站富媒体：把本地文件发回当前对话
        "wecom_send_image" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("缺少 path 参数")?
                .trim()
                .to_string();
            crate::wecom_media::send_image(&ctx.app, &path).await?
        }
        "wecom_send_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("缺少 path 参数")?
                .trim()
                .to_string();
            crate::wecom_media::send_file(&ctx.app, &path).await?
        }
        // 会等用户/等屏幕的几步：停止键要在 200ms 内收住，而不是等它自己跑完
        "hosts" => await_cancellable(sys::hosts(args), ctx.request_id).await??,
        "tunnel" => sys::tunnel(ctx.app, args).await?,
        "runtime" => sys::runtime(args).await?,
        "screenshot" => {
            let (text, png) = await_cancellable(tokio::task::spawn_blocking(pc::screenshot), ctx.request_id)
                .await?
                .map_err(|e| e.to_string())??;
            return Ok(ToolOutput { text, image: Some(png) });
        }
        "mouse" => pc::mouse(args)?,
        "keyboard" => pc::keyboard(args)?,
        "wait" => {
            let args = args.clone();
            // 等到一半被停止：不再返回「等完了」，直接让这一轮收尾
            await_cancellable(
                tokio::task::spawn_blocking(move || pc::wait(&args)),
                ctx.request_id,
            )
            .await?
            .map_err(|e| e.to_string())??
        }
        "update_plan" => update_plan(ctx.app, ctx.request_id, args)?,
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
        .is_some_and(|code| code.trim() != "0")
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

/// Prompt-cache breakpoints for the Anthropic body: the system prompt (role, policy text)
/// and the tool schema are byte-identical from one round to the next, so caching them
/// turns the repeated prefix into a 10%-priced read instead of fresh input tokens.
/// Anthropic 的前缀缓存 1024 token 起才生效：system + 工具表加起来不够就不打标记。
/// 打标记本身不额外计费，但一次都没命中过的短前缀纯属噪音。
fn cache_worth_marking(system: &str, tools: &[Value]) -> bool {
    const MIN_CACHE_TOKENS: usize = 1024;
    crate::agent_budget::estimate_tokens(system)
        + crate::agent_budget::estimate_tokens(&serde_json::to_string(tools).unwrap_or_default())
        >= MIN_CACHE_TOKENS
}

fn with_prompt_cache(mut body: Value, system: &str, tools: &[Value]) -> Value {
    body["system"] = json!([{
        "type": "text",
        "text": system,
        "cache_control": { "type": "ephemeral" }
    }]);
    if !tools.is_empty() {
        // Anthropic caches everything up to the marker: one marker on the last tool
        // covers the whole tool list.
        let mut list = tools.to_vec();
        if let Some(last) = list.last_mut() {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
        body["tools"] = json!(list);
    }
    body
}



struct RoundOutput {
    turn: ModelTurn,
    /// Visible text already sent to the frontend while streaming (empty = not streamed).
    streamed: String,
    usage: Option<(u64, u64)>,
    /// Input tokens served from the provider's prompt cache this round (0 = none/unreported).
    cache_hit: u64,
}

/// 一轮模型请求的失败。`switchable` 是给调用点的：这一轮一个字都没吐给用户、
/// 也不是取消 / 模型不支持工具，换个模型把这一轮重来一遍不会让用户看到重复内容。
struct RoundError {
    message: String,
    switchable: bool,
}

impl RoundError {
    fn fatal(message: impl Into<String>) -> Self {
        Self { message: message.into(), switchable: false }
    }

    /// 网络 / 服务端 / 空回答：这个模型这条路走不通了，调用点可以换备用模型重来。
    fn retryable(message: impl Into<String>) -> Self {
        Self { message: message.into(), switchable: true }
    }
}

/// `?` 出来的 String 一律按「不可降级」处理：取消、参数错误、解析失败换模型也没用。
impl From<String> for RoundError {
    fn from(message: String) -> Self {
        Self::fatal(message)
    }
}

/// 只想拿文案的调用点（子代理、审查员）用 `?` 直接转回 String。
impl From<RoundError> for String {
    fn from(err: RoundError) -> Self {
        err.message
    }
}

/// 还有下一次尝试时才安排等待。`max_retries` 是设置项（关掉 = 0），必须连同它一起判：
/// `retry_plan` 只认自己那 3 次上限，光靠它会在最后一次失败上也返回等待，
/// 于是真正的失败原因被「模型返回了空内容」盖掉。
fn retry_wait(attempt: u32, max_retries: u32, status: Option<u16>, retry_after: Option<u64>) -> Option<Duration> {
    if attempt >= max_retries {
        return None;
    }
    retry_plan(status, attempt, retry_after)
}

/// One model round. With `stream` set the answer text is emitted as it arrives, for
/// both protocols: OpenAI chunks are folded by `decode_model_body` out of the raw body,
/// Anthropic SSE is reassembled by `AnthropicStream` into a normal `message` body.
/// A router that ignores `stream: true` and answers with plain JSON works the same.
#[allow(clippy::too_many_arguments)] // 一轮请求的输入本身就是这些，调用点只有一处
async fn model_turn(
    client: &reqwest::Client,
    config: &AIModelConfig,
    anthropic: bool,
    messages: &Value,
    system: &str,
    tools: &[Value],
    stream: Option<&AppHandle>,
    request_id: &str,
    max_retries: u32,
) -> Result<RoundOutput, RoundError> {
    let base = trim_trailing_slash(&config.base_url);
    let max_tokens = {
        let m = if config.max_tokens > 0 { config.max_tokens } else { 1536 };
        // 4096 会把长回答 / 长工具参数直接截断（Anthropic 还要求必填）
        m.clamp(256, MAX_OUTPUT_TOKENS)
    };
    // `base_body` is the request without cache breakpoints: the fallback when the
    // upstream rejects `cache_control` (older proxies, OpenAI-compatible shims).
    let (url, base_body) = if anthropic {
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
        if stream.is_some() {
            body["stream"] = json!(true);
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
    let mut body = if anthropic && cache_worth_marking(system, tools) {
        with_prompt_cache(base_body.clone(), system, tools)
    } else {
        base_body.clone()
    };
    // 上游说「不认识 cache_control」时退回无断点的请求体，只退一次
    let mut cache_dropped = !anthropic;
    let mut empty_retry_used = false;
    // 流式能不能用是运行时才知道的：半路断开且一个字都没吐出去时，退回非流式重来一次
    let mut body_stream = stream.is_some();
    let mut stream_fallback_used = false;
    body["stream"] = json!(body_stream);
    // 最后一次尝试的失败原因：循环是走完而不是 return 的，别把它换成一句「空内容」
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        let sent = await_cancellable(
            apply_auth_headers(client.post(&url).json(&body), config, anthropic).send(),
            request_id,
        )
        .await?;
        let resp = match sent {
            Ok(resp) => resp,
            Err(e) => {
                let msg = friendly_send_error(&e.to_string());
                if let Some(wait) = retry_wait(attempt, max_retries, None, None) {
                    emit_retry(&stream, request_id, attempt, wait, "连接模型服务失败");
                    await_cancellable(tokio::time::sleep(wait), request_id).await?;
                    continue;
                }
                last_error = msg;
                break;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            let retry_after = retry_after_secs(&resp);
            let text = await_cancellable(resp.text(), request_id).await?.unwrap_or_default();
            let snippet: String = text.chars().take(240).collect();
            // 有些兼容网关会直接 400 掉 cache_control：去掉断点重发，缓存是加分项不是前提
            if !cache_dropped && snippet.to_lowercase().contains("cache_control") {
                cache_dropped = true;
                body = base_body.clone();
                // 回到 base_body 会把 stream 也带回来，按当前决定重写一遍
                body["stream"] = json!(body_stream);
                continue;
            }
            if !tools.is_empty() && snippet.to_lowercase().contains("tool") {
                return Err(RoundError::fatal(TOOLS_UNSUPPORTED));
            }
            let msg = friendly_model_error(status.as_u16(), &snippet);
            if let Some(wait) = retry_wait(attempt, max_retries, Some(status.as_u16()), retry_after) {
                emit_retry(&stream, request_id, attempt, wait, &format!("模型服务返回 {}", status.as_u16()));
                await_cancellable(tokio::time::sleep(wait), request_id).await?;
                continue;
            }
            last_error = msg;
            break;
        }
        let mut streamed = String::new();
        let mut usage = None;
        let mut cache_hit = 0u64;
        // Anthropic streams structure, not a body: this holds the reassembled `message`.
        let mut folded: Option<Value> = None;
        let text = match stream {
            Some(app) => {
                let mut resp = resp;
                // 整段读流包成一个 Result：出错时这次尝试的内容全部作废，由下面决定要不要退化
                let outcome: Result<Vec<u8>, String> = async {
                    let mut raw = Vec::new();
                    let mut lines = SseLines::default();
                    let mut filter = ThinkFilter::default();
                    let mut acc = AnthropicStream::default();
                    while let Some(bytes) = await_cancellable(resp.chunk(), request_id)
                        .await?
                        .map_err(|e| format!("读取模型响应失败: {e}"))?
                    {
                        raw.extend_from_slice(&bytes);
                        for data in lines.push(&bytes) {
                            if anthropic {
                                if let Some(piece) = acc.push(&data)? {
                                    emit_text(app, request_id, &piece);
                                    streamed.push_str(&piece);
                                }
                                continue;
                            }
                            let Ok(chunk) = serde_json::from_str::<Value>(&data) else {
                                continue;
                            };
                            if let Some(u) = chunk_usage(&chunk) {
                                usage = Some(u);
                            }
                            let hit = chunk_cache_hit(&chunk);
                            if hit > 0 {
                                cache_hit = hit;
                            }
                            if let Some(piece) = delta_content(&chunk) {
                                let visible = filter.push(piece);
                                emit_text(app, request_id, &visible);
                                streamed.push_str(&visible);
                            }
                        }
                    }
                    if anthropic {
                        usage = acc.usage();
                        cache_hit = acc.cache_read();
                        let body = acc.into_body();
                        // Nothing at all came back: fall through to the empty-answer retry.
                        let empty = body["content"].as_array().map(|a| a.is_empty()).unwrap_or(true);
                        folded = (!empty).then_some(body);
                    } else {
                        let tail = filter.finish();
                        emit_text(app, request_id, &tail);
                        streamed.push_str(&tail);
                    }
                    Ok(raw)
                }
                .await;
                match outcome {
                    Ok(raw) => String::from_utf8_lossy(&raw).into_owned(),
                    // 已经吐过字就认账（重发会让用户看到重复内容）；一个字都没出去才退回非流式
                    Err(_) if !stream_fallback_used && streamed.is_empty() => {
                        stream_fallback_used = true;
                        body_stream = false;
                        body["stream"] = json!(false);
                        emit_step(app, request_id, "retry", "info", "流式读取中断，改用非流式重试一次", "");
                        continue;
                    }
                    // 已经吐过字了：换模型重来会让用户看到两遍，只能报错
                    Err(e) => return Err(RoundError::fatal(e)),
                }
            }
            None => await_cancellable(resp.text(), request_id).await?.unwrap_or_default(),
        };
        // A folded Anthropic stream already is the body; everything else is decoded as-is.
        let decoded = match folded {
            Some(body) => Ok(body),
            None => decode_model_body(&text),
        };
        match decoded {
            Ok(json) => {
                if usage.is_none() {
                    usage = chunk_usage(&json);
                }
                if cache_hit == 0 {
                    // 非流式 / 兼容网关：命中数在顶层 usage 里（Anthropic 用 cache_read_input_tokens）
                    cache_hit = if anthropic {
                        json.pointer("/usage/cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                    } else {
                        chunk_cache_hit(&json)
                    };
                }
                let turn = if anthropic { parse_anthropic(&json)? } else { parse_openai(&json)? };
                return Ok(RoundOutput { turn, streamed, usage, cache_hit });
            }
            Err(_) if !empty_retry_used && text.trim().is_empty() => {
                empty_retry_used = true;
                if is_cancelled(request_id) {
                    return Err(RoundError::fatal(cancelled_message()));
                }
            }
            Err(e) => return Err(RoundError::fatal(e)),
        }
    }
    if last_error.is_empty() {
        last_error = "模型返回了空内容。免费线路经常会中途断开，请再试一次，或换一个模型。".to_string();
    }
    // 取消是用户按的停止，不是「模型这条路不通」，别拿去换备用模型
    if is_cancelled(request_id) {
        return Err(RoundError::fatal(cancelled_message()));
    }
    Err(RoundError::retryable(last_error))
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
    // 审查这一路短平快：重试只会拖时间，固定给 1 次（不含首次）
    let call = model_turn(client, config, anthropic, &messages, REVIEW_SYSTEM, &[], None, request_id, 1);
    match tokio::time::timeout(REVIEW_WAIT, call).await {
        Ok(Ok(out)) => parse_review_verdict(&out.turn.text),
        Ok(Err(e)) => (false, format!("审查失败：{}", clip(&e.message, 120))),
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

/// Tool arguments as JSON; a tool with no arguments gets `{}`.
///
/// 半截 / 非法的对象是**错误**，不能静默当成 `{}`：工具会以空参跑起来，报错看起来
/// 像工具自己坏了，模型下一轮还会原样重发。
fn parse_args(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(raw).map_err(|e| e.to_string())
}

/// `read_file(path*, offset, limit)` —— 某个工具的参数表，用于「参数不合法」的回灌。
/// 从 `tool_specs` 派生，避免和真正的 schema 分叉。
fn arg_hint(tool: &str) -> String {
    let specs = tool_specs();
    let Some(spec) = specs
        .as_array()
        .and_then(|all| all.iter().find(|s| s["name"].as_str() == Some(tool)))
    else {
        return String::new();
    };
    let required: Vec<&str> = spec["parameters"]["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let Some(props) = spec["parameters"]["properties"].as_object() else {
        return String::new();
    };
    let list: Vec<String> = props
        .keys()
        .map(|k| if required.contains(&k.as_str()) { format!("{k}*") } else { k.to_string() })
        .collect();
    if list.is_empty() {
        return String::new();
    }
    format!("{tool}({})", list.join(", "))
}

/// 一个调用能不能进「并发只读批」的三个前置条件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BatchStep {
    /// 工具只读（`is_readonly_tool`）。
    readonly: bool,
    /// `tool_gate` 判定直接放行。
    allowed: bool,
    /// 参数解析成功且没有冲突阻断。
    ready: bool,
}

/// 并发批只取本轮开头连续的合格调用：一旦遇到要审批 / 要改东西的调用就停，
/// 因为后面的读必须看到它的结果。返回调用下标。
fn plan_batches(steps: &[BatchStep], max_parallel: usize) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        if !(step.readonly && step.allowed && step.ready) {
            break;
        }
        if out.len() >= max_parallel {
            break;
        }
        out.push(i);
    }
    out
}

/// 设置项「搜索时跳过被忽略的文件」关掉后，等价于每次调用都传 `no_ignore: true`。
fn with_grep_ignore(args: &Value, grep_ignore: bool) -> Value {
    if grep_ignore {
        return args.clone();
    }
    let mut args = args.clone();
    if let Value::Object(map) = &mut args {
        map.insert("no_ignore".into(), Value::Bool(true));
    }
    args
}

/// 可以搬到后台线程并发执行的只读文件工具（检索与读文件最吃时间）。
/// `process_output` / `list_processes` 这类虽然也只读，但很快，且不走文件作用域，
/// 留在主循环里按原顺序执行。
fn is_readonly_tool(name: &str) -> bool {
    crate::agent_tools::can_prefetch(name)
}

/// 只读文件工具的执行体：只依赖 `Scope` + 请求号，所以能整块搬进 `spawn_blocking`
/// 真并发（`execute_tool` 本身是同步阻塞的，直接 fork 只会串行）。
fn execute_readonly(
    scope: &Scope,
    request_id: &str,
    name: &str,
    args: &Value,
    grep_ignore: bool,
) -> Result<ToolOutput, String> {
    use crate::agent_fs as fs;
    // 只对两个检索工具生效：读文件 / 列目录本来就不看 .gitignore
    let search_args: Value;
    let args = if matches!(name, "find_files" | "grep") {
        search_args = with_grep_ignore(args, grep_ignore);
        &search_args
    } else {
        args
    };
    let out = match name {
        "read_file" => fs::read_file(scope, args)?,
        "list_dir" => fs::list_dir(scope, args)?,
        "find_files" => fs::find_files(scope, args, request_id)?,
        "grep" => fs::grep(scope, args, request_id)?,
        "symbols" => crate::agent_symbols::symbols(scope, args, request_id)?,
        other => return Err(format!("{other} 不是只读文件工具")),
    };
    Ok(out.into())
}

/// 预执行结果：`started` 里的下标已经发过 `running` 步，主循环不要重复发。
#[derive(Default)]
struct Prefetched {
    started: HashSet<usize>,
    done: HashMap<usize, Result<ToolOutput, String>>,
}

/// 只读调用的并发预执行。主循环仍按原顺序收结果、发步骤、回灌模型，
/// 这样 Anthropic 的 `tool_result` 顺序（必须与 `tool_use` 同序）不受影响。
async fn prefetch_readonly(
    ctx: &ToolCtx<'_>,
    calls: &[ToolCall],
    permission: Permission,
) -> Prefetched {
    let mut prefetched = Prefetched::default();
    let upto = calls.len().min(MAX_CALLS_PER_ROUND);
    let mut parsed: Vec<Option<Value>> = Vec::with_capacity(upto);
    let mut steps: Vec<BatchStep> = Vec::with_capacity(upto);
    for call in calls.iter().take(upto) {
        // 批只取前缀：一旦遇到不是只读的调用就停（后面的读必须看到它的结果）。
        // 只读工具永远不会被 `conflict_block` 拦住（它只看写类工具），所以这里
        // 只判断「参数能解析 + 闸门放行」，不必再跑一次冲突检查。
        if !is_readonly_tool(&call.name) {
            break;
        }
        let args = parse_args(&call.arguments).ok();
        let allowed = args
            .as_ref()
            .map(|a| tool_gate(permission, &call.name, &gate_arg(&call.name, a)) == Gate::Allow)
            .unwrap_or(false);
        steps.push(BatchStep { readonly: true, allowed, ready: args.is_some() });
        parsed.push(args);
    }
    let picks = plan_batches(&steps, ctx.max_parallel);
    if picks.len() < 2 {
        return prefetched;
    }
    for &i in &picks {
        let Some(args) = parsed[i].clone() else { continue };
        prefetched.started.insert(i);
        let summary = call_summary(&calls[i].name, &args);
        let arg_text = step_arg(&args);
        emit_step_full(ctx.app, ctx.request_id, &calls[i].name, "running", &summary, "", Some(&arg_text));
    }
    // 真并发：每个调用一个后台线程（`Scope` 很小，克隆一份即可）
    let mut handles = Vec::new();
    for &i in &picks {
        let Some(args) = parsed[i].clone() else { continue };
        let scope = ctx.scope.clone();
        let request_id = ctx.request_id.to_string();
        let name = calls[i].name.clone();
        let grep_ignore = ctx.grep_ignore;
        handles.push((
            i,
            tokio::task::spawn_blocking(move || execute_readonly(&scope, &request_id, &name, &args, grep_ignore)),
        ));
    }
    for (i, handle) in handles {
        let result = match handle.await {
            Ok(result) => result,
            Err(e) => Err(format!("并发执行失败: {e}")),
        };
        prefetched.done.insert(i, result);
    }
    prefetched
}

/// 只读子代理之间没有先后依赖（各自调查一块，只回结论），同一轮派出多个时并发跑，
/// 结论仍按 `tool_use` 顺序回灌。只有一个时不预跑：串行就够，还少一层并发。
/// 闸门不是 `Allow` 的不预跑——那种要等用户点头，不能先斩后奏。
async fn prefetch_tasks(
    ctx: &ToolCtx<'_>,
    config: &AIModelConfig,
    calls: &[ToolCall],
    permission: Permission,
) -> Prefetched {
    let mut prefetched = Prefetched::default();
    let mut picks: Vec<(usize, String, String)> = Vec::new();
    for (index, call) in calls.iter().take(MAX_CALLS_PER_ROUND).enumerate() {
        if call.name != "task" || picks.len() >= ctx.max_parallel {
            continue;
        }
        let Ok(args) = parse_args(&call.arguments) else { continue };
        let goal = crate::agent_fs::arg_str(&args, "goal");
        if goal.trim().is_empty() {
            continue;
        }
        if tool_gate(permission, "task", &gate_arg("task", &args)) != Gate::Allow {
            continue;
        }
        let summary = call_summary("task", &args);
        let arg_text = step_arg(&args);
        emit_step_full(ctx.app, ctx.request_id, "task", "running", &summary, "", Some(&arg_text));
        prefetched.started.insert(index);
        picks.push((index, goal, crate::agent_fs::arg_str(&args, "path")));
    }
    if picks.len() < 2 {
        return prefetched;
    }
    let futures = picks
        .iter()
        .map(|(_, goal, path)| run_subagent(ctx, config, permission, goal, path));
    for ((index, ..), result) in picks.iter().zip(futures_util::future::join_all(futures).await) {
        // 子代理结论是一条文本，和别的工具结果一样装进 ToolOutput 再回灌
        prefetched.done.insert(*index, result.map(ToolOutput::from));
    }
    prefetched
}

#[tauri::command]
pub async fn agent_run(app: AppHandle, req: AgentRunRequest) -> Result<(), String> {
    let request_id = req.request_id.clone();
    let _guard = CancelGuard::new(&request_id);
    let _approvals = ApprovalCleanup { request_id: request_id.clone() };
    let started = Instant::now();
    if req.config.base_url.trim().is_empty() || req.config.model.trim().is_empty() {
        let msg = "模型 Base URL 或模型名未配置";
        emit_err(&app, &request_id, msg);
        return Err(msg.into());
    }
    // 新协议只看最后一条 user 的文本；旧路径仍看 `user` 字段。
    let last_ask = if req.messages.is_empty() {
        req.user.trim().to_string()
    } else {
        req.messages
            .iter()
            .rev()
            .find(|m| !m.is_assistant())
            .map(|m| m.joined_text().trim().to_string())
            .unwrap_or_default()
    };
    if last_ask.is_empty() {
        let msg = "问题为空";
        emit_err(&app, &request_id, msg);
        return Err(msg.into());
    }
    crate::agent_session_log::log_user(&request_id, &last_ask);
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
    let reach = Reach {
        files: scope.has_files(),
        tools: req.tools,
        vision: req.vision,
        web: !req.web_allow.is_empty(),
    };
    let mut tools_enabled = req.tools;
    let services = root.as_deref().map(|r| workspace_services(r, &req.services)).unwrap_or_default();
    // 项目/全局 harness 配置：上下文注入、技能正文与 hooks 都从这里来。
    let harness_config = crate::harness_config::load(root.as_deref());
    let hide_abs = req.request_id.starts_with("wecom-");
    let mut system = format!("{}{}", req.system, policy_text(permission, root.as_deref(), &services, reach, hide_abs));
    // 上下文管道：项目文档 / 长期记忆 / 技能索引 / `@` 引用，全部只读且不越出工作区。
    let context_bundle = crate::agent_context::collect(&harness_config, root.as_deref(), &last_ask);
    if !context_bundle.text.is_empty() {
        system.push_str(&context_bundle.text);
    }
    if req.plan_first && req.tools {
        // 设置项「先出计划」：只给一句引导，不强制——纯问答里加计划卡反而碍事
        system.push_str("\n\n任务超过两步时，先调用 `update_plan` 列出步骤再动手；一句话能答完的问题不要用。");
    }
    let tool_names = offered_tools(permission, reach);
    // Tools that exist at some level: those not offered here get the permission denial instead.
    let known_tools =
        offered_tools(Permission::Full, Reach { files: true, tools: true, vision: true, web: true });
    if tool_names.iter().any(|n| crate::agent_tools::is_computer(n)) {
        // The model must never see (or click) the AI hub window itself.
        crate::agent_computer::exclude_from_capture(&app);
    }
    // 两个协议都走流式，按「多久没收到数据」计时；审查那一路不带流式，靠外面的超时兜底
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(crate::config::GENERATE_CONNECT_TIMEOUT_SECS))
        .read_timeout(Duration::from_secs(crate::config::GENERATE_STREAM_IDLE_SECS))
        .build()
        .map_err(|e| e.to_string())?;
    // 子代理是设置项（默认关）：没开就不下发 task，省得模型反复试一个不存在的工具
    let mut tool_names = tool_names;
    if !req.subagents {
        tool_names.retain(|n| *n != "task");
    }
    // 按任务模式收窄工具子集（对话是最小集；具体规则见 `agent_tools::mode_allows`）。
    if !req.mode.trim().is_empty() {
        tool_names.retain(|n| crate::agent_tools::mode_allows(&req.mode, n));
    }
    // 技能 frontmatter 的 allowed-tools ∩ 已下发集合（空白名单 = 不额外收窄）。
    crate::agent_context::intersect_skill_tools(&harness_config, &last_ask, &mut tool_names);
    let mut tools = provider_tools(&tool_names, anthropic);
    // MCP：工具名是运行时的，不在这张静态表里；列一次、追加进 tools，然后按名字查找
    // 总开关关掉时连进程都不起：这一轮就是没有 mcp_* 工具
    let mcp_servers = if req.mcp_enabled && !req.mcp.is_empty() {
        attach_mcp_tools(&app, &request_id, &req.mcp, &mut tools, anthropic, true).await
    } else {
        Vec::new()
    };
    // 两个数值项：0 / 缺省 = 保持改动前的常量，给了值就夹在安全区间里
    let context_tokens = if req.context_tokens == 0 {
        crate::agent_budget::CONTEXT_TOKENS
    } else {
        req.context_tokens.clamp(8_000, 400_000)
    };
    let result_cap = if req.result_cap == 0 {
        TOOL_RESULT_CAP
    } else {
        req.result_cap.clamp(2_000, 64_000)
    };
    let ctx = ToolCtx {
        app: &app,
        scope: &scope,
        protected: &protected,
        apps: &req.apps,
        request_id: &request_id,
        web_allow: &req.web_allow,
        client: &client,
        mcp: &mcp_servers,
        hooks: &harness_config.hooks,
        result_cap,
        max_parallel: if req.parallel_reads { MAX_PARALLEL } else { 1 },
        max_retries: if req.retries { MAX_RETRIES } else { 0 },
        grep_ignore: req.grep_ignore,
    };

    // 历史统一走内容块协议：新入参直接用，旧入参包成 Text 块，翻译成 provider 形状只做一次。
    let protocol_messages: Vec<crate::agent_protocol::Message> = if req.messages.is_empty() {
        let mut msgs: Vec<crate::agent_protocol::Message> = req
            .history
            .iter()
            .map(|turn| crate::agent_protocol::Message::text(&turn.role, &turn.content))
            .collect();
        msgs.push(crate::agent_protocol::Message::text("user", &req.user));
        msgs
    } else {
        req.messages.clone()
    };
    let mut messages = crate::agent_protocol::provider_messages(&protocol_messages, anthropic, &system);

    let mut final_text = String::new();
    let mut rounds = 0usize;
    let mut action_steps = 0usize;
    let mut notes: Vec<String> = Vec::new();
    // 「本轮同类都允许」记下的类别（见 agent_policy::action_kind），只对 Ask 档生效
    let mut granted: HashSet<String> = HashSet::new();
    // 跨轮记忆（设置里的「以后都允许」）：本轮新记下的也并进来，不必等下一次提问
    let mut always_allowed: HashSet<String> = req.always_allow.iter().cloned().collect();
    // One "去提交页处理冲突" card per turn is enough.
    let mut conflict_noticed = false;
    // 模型编出来的工具名（如 git_add）：每个名字只在步骤里显示一次
    let mut unknown_shown: HashSet<String> = HashSet::new();
    // 上一条实际执行过的 shell 命令，用来识别原样重试。
    let mut last_shell_command = String::new();
    let mut last_shell_failed = false;
    let mut usage_total: Option<(u64, u64)> = None;
    // 压缩提示每轮只说一次，不要每轮都插一条
    let mut trim_noticed = false;
    // 来源行要的三个数：上下文大小、预算占比、压缩次数、缓存命中
    let mut stats = TurnStats::default();
    // 当前在用的模型。降级只可能发生一次：主模型这条路彻底不通了就整轮跟着备用模型走，
    // 免得每个回合都先撞一次墙（连同它的重试等待）。
    let mut config = req.config.clone();
    let mut fallback = req
        .fallback_config
        .clone()
        // 协议不同就不能中途换：messages / tools 已经按主模型的协议拼好了
        .filter(|c| (c.provider.to_lowercase() == "anthropic") == anthropic);
    let mut fell_back = false;
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
        // 上下文预算：先压工具结果，再丢最老的轮次，最后压系统提示；工具 schema 不动
        let schema_tokens =
            crate::agent_budget::estimate_tokens(&serde_json::to_string(&tools).unwrap_or_default());
        let trim = crate::agent_budget::fit_context(
            &mut system,
            &mut messages,
            schema_tokens,
            context_tokens,
            crate::agent_budget::KEEP_ROUNDS,
        );
        // 来源行：这一轮实际带了多少内容、占预算多少、压过几次
        stats.context_chars = crate::agent_budget::context_chars(&system, &messages);
        stats.budget_ratio = (trim.after as f32 / context_tokens as f32).clamp(0.0, 1.0);
        if trim.any() {
            stats.trims += 1;
            if !anthropic {
                // OpenAI 协议把系统提示放在第一条消息里，裁剪后要同步回去
                if let Some(first) = messages.first_mut() {
                    if first.get("role").and_then(|v| v.as_str()) == Some("system") {
                        first["content"] = json!(system);
                    }
                }
            }
            if !trim_noticed {
                trim_noticed = true;
                emit_step(
                    &app,
                    &request_id,
                    "notice",
                    "info",
                    "上下文接近上限，已压缩早期内容",
                    &format!("约 {} → {} token", trim.before, trim.after),
                );
            }
        }
        let round = model_turn(
            &client,
            &config,
            anthropic,
            &Value::Array(messages.clone()),
            &system,
            &tools,
            // 设置项「Anthropic 流式」关掉时按普通请求发：有些网关的 SSE 会中途断
            if anthropic && !req.anthropic_stream { None } else { Some(&app) },
            &request_id,
            ctx.max_retries,
        )
        .await;
        let round = match round {
            Err(e) if e.message == TOOLS_UNSUPPORTED && tools_enabled => {
                // 模型不支持工具调用：去掉 tools 重发，这一轮只回答
                tools_enabled = false;
                tools.clear();
                emit_step(&app, &request_id, "notice", "info", "当前模型不支持工具调用，本轮只回答", "");
                rounds -= 1;
                continue;
            }
            // 主模型这一轮彻底失败、一个字都没吐出去：让备用模型把这一轮重来（只有一次）
            Err(e) if e.switchable && !fell_back => match fallback.take() {
                Some(next) => {
                    emit_step(
                        &app,
                        &request_id,
                        "retry",
                        "info",
                        &format!("{} 没能回答，改用备用模型 {} 重试", config.name, next.name),
                        &e.message,
                    );
                    config = next;
                    fell_back = true;
                    rounds -= 1;
                    continue;
                }
                None => {
                    emit_err(&app, &request_id, &e.message);
                    return Err(e.message);
                }
            },
            Ok(r) => r,
            Err(e) => {
                let msg = if e.message == TOOLS_UNSUPPORTED {
                    "模型拒绝了这次请求（工具调用）。".to_string()
                } else {
                    e.message
                };
                emit_err(&app, &request_id, &msg);
                return Err(msg);
            }
        };
        if let Some((p, c)) = round.usage {
            let (tp, tc) = usage_total.unwrap_or((0, 0));
            usage_total = Some((tp + p, tc + c));
            // 用真实 usage 校准字符→token 的估算系数：这轮发出去的（system + 对话 + 工具表）
            // 加上模型的回答都按字符估，provider 报的总量是分母。裁剪决策从此越用越准。
            let answer = if round.turn.text.is_empty() { round.streamed.as_str() } else { round.turn.text.as_str() };
            let estimated = crate::agent_budget::estimate_tokens(&system)
                + crate::agent_budget::messages_tokens(&messages)
                + schema_tokens
                + crate::agent_budget::estimate_tokens(answer);
            crate::agent_budget::calibrate_from_usage(estimated, (p + c) as usize);
        }
        stats.cache_hit += round.cache_hit;
        let turn = round.turn;
        if round.streamed.trim().is_empty() {
            // 没走流式（路由回了整段 JSON / 只有 reasoning 字段）：整轮一次性发
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
            crate::agent_session_log::log_assistant(&request_id, &final_text);
            run_stop_hooks(&harness_config.hooks, &scope, permission);
            emit_done(&app, &request_id, started, usage_total, false, stats);
            return Ok(());
        }
        if anthropic {
            messages.push(json!({ "role": "assistant", "content": turn.assistant }));
        } else {
            messages.push(turn.assistant.clone());
        }
        let mut tool_results: Vec<(String, ToolOutput)> = Vec::new();
        // 只读调用先并发跑掉（`plan_batches` 只取开头连续的合格调用）；写类与要审批的仍串行
        let mut prefetched = prefetch_readonly(&ctx, &turn.calls, permission).await;
        // 只读子代理 `task` 也并发：结论仍按 tool_use 顺序回灌
        let tasks = prefetch_tasks(&ctx, &config, &turn.calls, permission).await;
        prefetched.started.extend(tasks.started);
        prefetched.done.extend(tasks.done);
        // —— 阶段 1：扫描。把每个调用归类（跳过 / 要审批 / 直接执行），先不执行 ——
        // 结果必须按 tool_use 顺序回灌（Anthropic 要求同序），所以连「跳过」的理由都先在
        // `Prepared` 里排好，阶段 3 统一推给 tool_results。
        let mut prepared: Vec<Prepared> = Vec::new();
        for (index, call) in turn.calls.iter().enumerate() {
            let mut p = Prepared {
                index,
                call_id: call.id.clone(),
                name: call.name.clone(),
                ..Default::default()
            };
            // 单轮调用上限：弱模型一次发十几个调用，逐个弹审批没有意义。
            if index >= MAX_CALLS_PER_ROUND {
                p.skip = Some(format!(
                    "本轮工具调用过多（{} 个，上限 {MAX_CALLS_PER_ROUND}），这个调用没有执行。请分批：先做最关键的几个，拿到结果再继续。",
                    turn.calls.len()
                ));
                prepared.push(p);
                continue;
            }
            if is_cancelled(&request_id) {
                let msg = cancelled_message();
                emit_err(&app, &request_id, &msg);
                return Err(msg);
            }
            // MCP 的名字是运行时来的，不在静态表里：先查本轮列到的这张表
            let is_mcp = resolve_mcp(&mcp_servers, &call.name).is_some();
            if !is_mcp && !known_tools.contains(&call.name.as_str()) {
                let reason = unknown_tool_reason(&call.name, &tool_names);
                if unknown_shown.insert(call.name.clone()) {
                    emit_step_full(
                        &app, &request_id, "unknown", "error",
                        &format!("模型调用了不存在的工具 {}", call.name), &reason, Some(&call.name),
                    );
                }
                p.skip = Some(reason);
                prepared.push(p);
                continue;
            }
            let args = match parse_args(&call.arguments) {
                Ok(args) => args,
                Err(err) => {
                    let hint = arg_hint(&call.name);
                    let reason = format!(
                        "工具参数不是合法 JSON（{err}），这个调用没有执行。{}请重新发一次完整的参数。",
                        if hint.is_empty() { String::new() } else { format!("参数：{hint}。") }
                    );
                    emit_step_full(
                        &app, &request_id, &call.name, "error",
                        &format!("{} 参数解析失败", call.name), &reason, Some(&call.name),
                    );
                    notes.push(format!("{}（参数错误）", call.name));
                    p.skip = Some(reason);
                    prepared.push(p);
                    continue;
                }
            };
            p.summary = call_summary(&call.name, &args);
            p.arg_text = step_arg(&args);
            if !prefetched.started.contains(&index) {
                emit_step_full(&app, &request_id, &call.name, "running", &p.summary, "", Some(&p.arg_text));
            }
            p.args = args;
            p.arg = gate_arg(&call.name, &p.args);
            p.command = if call.name == "run_command" { p.arg.clone() } else { String::new() };
            let mut gate = tool_gate(permission, &call.name, &p.arg);
            // 改前确认：编辑档的写入从「直接落地」降为「先看 diff」
            p.diff_confirming =
                crate::agent_policy::needs_diff_confirm(permission, &call.name, gate, req.diff_confirm);
            if p.diff_confirming {
                gate = Gate::Ask;
            }
            if gate == Gate::Deny {
                let reason = deny_reason(permission, &call.name, &p.arg);
                emit_step_full(&app, &request_id, &call.name, "denied", &p.summary, &reason, Some(&p.arg_text));
                notes.push(format!("{}（未执行）", p.summary));
                p.skip = Some(reason);
                prepared.push(p);
                continue;
            }
            if let Some(reason) = conflict_block(&scope, &call.name, &p.arg, &p.args) {
                emit_step_full(&app, &request_id, &call.name, "denied", &p.summary, &reason, Some(&p.arg_text));
                if !conflict_noticed {
                    conflict_noticed = true;
                    emit_step(&app, &request_id, "conflict", "info", CONFLICT_NOTICE, &reason);
                }
                notes.push(format!("{}（有冲突，未执行）", p.summary));
                p.skip = Some(reason);
                prepared.push(p);
                continue;
            }
            p.gate = Some(gate);
            p.kind = action_kind(&call.name, &p.arg);
            let (preview, preview_is_diff) = approval_preview(&scope, &call.name, &p.args, &p.summary);
            p.preview = preview;
            p.preview_is_diff = preview_is_diff;
            p.needs_confirm = gate == Gate::Ask && !granted.contains(&p.kind) && !always_allowed.contains(&p.kind);
            // 审查托管：先让 AI 审一遍，只有明确放行才不打扰用户
            if gate == Gate::Review {
                let (allow, reason) =
                    review_action(&client, &config, anthropic, &req.user, &p.summary, &p.preview, &request_id).await;
                if is_cancelled(&request_id) {
                    let msg = cancelled_message();
                    emit_err(&app, &request_id, &msg);
                    return Err(msg);
                }
                if allow {
                    let detail = if reason.is_empty() { "AI 审查通过".to_string() } else { format!("AI 审查通过：{reason}") };
                    emit_step_full(&app, &request_id, &call.name, "running", &p.summary, &detail, Some(&p.arg_text));
                } else {
                    p.needs_confirm = true;
                    p.review_note = if reason.is_empty() { "AI 审查：需要你确认".into() } else { format!("AI 审查：{reason}") };
                }
            }
            // 「以后都允许」只对 Ask 档提供：审查托管每次都要过 AI 审、完全权限本来不问；
            // 改前确认降下来的这一次也不提供——否则一按就把用户自己开的开关废了
            p.can_remember = gate == Gate::Ask && permission != Permission::Full && !p.diff_confirming;
            prepared.push(p);
        }

        // —— 阶段 2：需要确认的操作合成一张卡，等一次决定 ——
        let confirming: Vec<usize> = prepared
            .iter()
            .enumerate()
            .filter(|(_, p)| p.needs_confirm && p.skip.is_none())
            .map(|(i, _)| i)
            .collect();
        if !confirming.is_empty() {
            let first = &prepared[confirming[0]];
            // 合并卡只在「所有操作都是同一类」时给「以后都允许」——一次点击记不住好几种
            let single_kind = confirming.iter().all(|&i| prepared[i].kind == first.kind);
            let can_remember = first.can_remember && single_kind;
            if confirming.len() == 1 {
                let title = if first.review_note.is_empty() {
                    first.summary.clone()
                } else {
                    format!("{}（{}）", first.summary, first.review_note)
                };
                emit_approval(
                    &app, &request_id, &first.call_id, &first.name, &title, &first.preview,
                    first.preview_is_diff, &first.kind, first.can_remember,
                );
            } else {
                let patches: Vec<ApprovalPatch> = confirming
                    .iter()
                    .map(|&i| {
                        let p = &prepared[i];
                        let title = if p.review_note.is_empty() {
                            p.summary.clone()
                        } else {
                            format!("{}（{}）", p.summary, p.review_note)
                        };
                        ApprovalPatch {
                            call_id: p.call_id.clone(),
                            tool: p.name.clone(),
                            summary: title,
                            preview: p.preview.clone(),
                            preview_kind: if p.preview_is_diff { "diff" } else { "text" },
                            action_kind: p.kind.clone(),
                        }
                    })
                    .collect();
                emit_approval_batch(&app, &request_id, &patches, can_remember);
            }
            let decision = match wait_decision(&request_id, &first.call_id).await {
                Ok(v) => v,
                Err(e) => {
                    emit_err(&app, &request_id, &e);
                    return Err(e);
                }
            };
            set_attention(&app, false);
            for &i in &confirming {
                let p = &mut prepared[i];
                match decision {
                    Decision::Allow { for_turn, always } => {
                        // 审查转人工时不记「同类」：每条都要过审查
                        if for_turn && p.gate == Some(Gate::Ask) {
                            granted.insert(p.kind.clone());
                        }
                        if always {
                            always_allowed.insert(p.kind.clone());
                        }
                    }
                    Decision::Deny => {
                        emit_step_full(&app, &request_id, &p.name, "denied", &p.summary, "用户拒绝", Some(&p.arg_text));
                        notes.push(format!("{}（已拒绝）", p.summary));
                        p.skip = Some("用户拒绝了这个操作".to_string());
                    }
                    Decision::TimedOut => {
                        let mins = APPROVAL_WAIT.as_secs() / 60;
                        emit_step_full(
                            &app, &request_id, &p.name, "denied", &p.summary,
                            &format!("等待确认超过 {mins} 分钟，已自动拒绝"), Some(&p.arg_text),
                        );
                        notes.push(format!("{}（确认超时）", p.summary));
                        p.skip = Some(format!(
                            "等待用户确认超过 {mins} 分钟，这个操作没有执行。不要重试，直接告诉用户：需要确认的操作是什么，确认后可以再发一次。"
                        ));
                    }
                }
            }
        }

        // —— 阶段 3：dispatch_tool 管道（pre → execute → post → finalize）按序回灌 ——
        for p in prepared {
            if let Some(reason) = p.skip {
                crate::agent_session_log::log_tool(&request_id, &p.call_id, &p.name, &reason, true);
                tool_results.push((p.call_id, reason.into()));
                continue;
            }
            // 并发预取只跑了 execute；仍要补 post_hooks + 结果预算。
            let result = match prefetched.done.remove(&p.index) {
                Some(done) => finalize_tool_result(&ctx, permission, &p.name, done).await,
                None => dispatch_tool(&ctx, &config, permission, &p.name, &p.args).await,
            };
            let failed = match &result {
                Ok(out) => command_output_failed(&out.text),
                Err(_) => true,
            };
            match result {
                Ok(mut out) => {
                    out.text = repeated_command_result(&last_shell_command, last_shell_failed, &p.command, &out.text);
                    // A pull / merge / stash pop the AI ran left conflicts: hand over, don't resolve.
                    if let Some(root) = changed_repo(&scope, &p.name, &p.arg, &p.args) {
                        if let Some(reason) = conflict_reason(&crate::git_merge::conflict_state(&root.to_string_lossy())) {
                            out.text = format!("{}\n\n[Agent 提示] 出现冲突，已停止，不要自行解决。{reason}", out.text);
                            if !conflict_noticed {
                                conflict_noticed = true;
                                emit_step(&app, &request_id, "conflict", "info", CONFLICT_NOTICE, &reason);
                            }
                        }
                    }
                    // 写入的步骤详情带上 diff，用户能在步骤里看到改了什么
                    let detail = if p.preview_is_diff && matches!(p.name.as_str(), "write_file" | "edit_file" | "hosts") {
                        format!("{}\n\n{}", out.text, clip(&p.preview, DETAIL_CAP))
                    } else {
                        clip(&out.text, DETAIL_CAP)
                    };
                    emit_step_full(&app, &request_id, &p.name, "done", &p.summary, &detail, Some(&p.arg_text));
                    notes.push(p.summary);
                    crate::agent_session_log::log_tool(&request_id, &p.call_id, &p.name, &out.text, false);
                    tool_results.push((p.call_id, out));
                }
                Err(err) if err.contains("AI_REQUEST_CANCELLED") => {
                    emit_err(&app, &request_id, &err);
                    return Err(err);
                }
                Err(err) if err == crate::agent_computer::FAILSAFE_MESSAGE => {
                    // 紧急停止：不再给模型机会继续动鼠标键盘
                    emit_step_full(&app, &request_id, &p.name, "denied", &p.summary, &err, Some(&p.arg_text));
                    emit_text(&app, &request_id, &format!("\n\n{err}"));
                    emit_done(&app, &request_id, started, usage_total, false, stats);
                    return Ok(());
                }
                Err(err) => {
                    emit_step_full(
                        &app, &request_id, &p.name, "error", &p.summary, &clip(&err, DETAIL_CAP), Some(&p.arg_text),
                    );
                    notes.push(format!("{}（失败）", p.summary));
                    let text = repeated_command_result(
                        &last_shell_command, last_shell_failed, &p.command, &format!("错误: {err}"),
                    );
                    crate::agent_session_log::log_tool(&request_id, &p.call_id, &p.name, &text, true);
                    tool_results.push((p.call_id, text.into()));
                }
            }
            if p.name == "run_command" {
                last_shell_command = p.command;
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
    run_stop_hooks(&harness_config.hooks, &scope, permission);
    emit_done(&app, &request_id, started, usage_total, true, stats);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        arg_hint, await_cancellable, call_summary, candidates_error, unknown_tool_reason, clip_tool_result,
        command_blocked, command_is_readonly, command_output_failed, conflict_block, count_images, decode_model_body,
        friendly_model_error, friendly_send_error, gate_arg, is_readonly_tool, limit_message, match_app_name, parse_args,
        retry_wait, with_grep_ignore, cache_worth_marking,
        plan_batches, plan_steps, prune_images, repeated_command_result, round_is_action, step_arg, subagent_tools,
        tool_result_messages, unique_hit, with_prompt_cache, workspace_services, BatchStep, ToolOutput, MAX_PARALLEL,
        MAX_PLAN_STEPS, SHOT_PLACEHOLDER, TASK_TOOLS, TOOL_RESULT_CAP,
    };
    use crate::agent_proc::run_command;
    use serde_json::json;
    use std::fs;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn nested_agents_cannot_spawn_agents() {
        assert!(!TASK_TOOLS.contains(&"task"));
        // 子代理只读：写入、命令、系统设置都不在它的工具表里
        for denied in [
            "write_file", "edit_file", "delete_path", "move_path", "run_command", "start_process", "hosts", "kill_process",
            "screenshot",
        ] {
            assert!(!TASK_TOOLS.contains(&denied), "{denied}");
        }
        // 主循环把 task 当只读轮：不占动作步数，否则几轮子代理就把预算烧完
        assert!(!round_is_action(&["task".to_string()]));
    }

    /// `web_fetch` 需要审批（只读/编辑/审查档），子代理没有审批通道：只有完全权限才给它。
    #[test]
    fn subagent_offers_web_fetch_only_where_it_is_allowed() {
        use crate::agent_policy::Permission;
        let allow = vec!["example.com".to_string()];
        assert!(!subagent_tools(Permission::Read, &allow).contains(&"web_fetch"));
        assert!(!subagent_tools(Permission::Workspace, &allow).contains(&"web_fetch"));
        assert!(!subagent_tools(Permission::Review, &allow).contains(&"web_fetch"));
        assert!(subagent_tools(Permission::Full, &allow).contains(&"web_fetch"));
        // 白名单为空 = 没有联网能力，完全权限也不给
        assert!(!subagent_tools(Permission::Full, &[]).contains(&"web_fetch"));
    }

    /// 「自动重试」关掉时一次都不该等：`retry_plan` 只认自己那 3 次上限，光靠它会在
    /// 唯一一次尝试上返回等待，于是真正的失败原因被一句「模型返回了空内容」盖掉。
    #[test]
    fn retry_waits_only_while_attempts_remain() {
        assert!(retry_wait(0, 0, Some(503), None).is_none(), "关掉重试就不能再等");
        assert!(retry_wait(0, 0, None, None).is_none());
        // 最后一次尝试：已经没得重试了
        assert!(retry_wait(3, 3, Some(503), None).is_none());
        assert!(retry_wait(3, 3, None, None).is_none());
        // 还有下一次：429 / 5xx / 连接失败都值得等
        assert!(retry_wait(0, 3, Some(429), None).is_some());
        assert!(retry_wait(2, 3, Some(502), None).is_some());
        assert!(retry_wait(0, 3, None, None).is_some());
        // 400 / 401 这类换谁都得错：不等，直接把原文交给用户
        assert!(retry_wait(0, 3, Some(401), None).is_none());
        assert!(retry_wait(0, 3, Some(400), None).is_none());
        // 服务商给了 Retry-After 就听它的
        assert_eq!(retry_wait(0, 3, Some(429), Some(7)), Some(Duration::from_secs(7)));
    }

    /// Anthropic 1k token 起才缓存：短前缀不打标记（打了也永远命不中）。
    #[test]
    fn prompt_cache_skips_short_prefixes() {
        assert!(!cache_worth_marking("hi", &[]));
        assert!(!cache_worth_marking("", &[]));
        let tools = vec![json!({ "name": "read_file", "description": "x".repeat(6_000) })];
        assert!(cache_worth_marking("你是助手", &tools), "光工具表就够 1k token");
        assert!(cache_worth_marking(&"规则".repeat(1_200), &[]), "长 system 也够");
    }

    #[test]
    fn prompt_cache_marks_the_system_prompt_and_last_tool() {
        let tools = vec![json!({ "name": "read_file" }), json!({ "name": "grep" })];
        let body = with_prompt_cache(json!({ "model": "claude" }), "你是助手", &tools);
        // system 变成带断点的 block 数组
        assert_eq!(body["system"][0]["text"], json!("你是助手"));
        assert_eq!(body["system"][0]["cache_control"]["type"], json!("ephemeral"));
        // 断点只打最后一个工具：一个标记覆盖整个 tools 前缀
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"]["type"], json!("ephemeral"));
        // 没有工具时不该塞一个空 tools 字段
        let bare = with_prompt_cache(json!({ "model": "m" }), "s", &[]);
        assert!(bare.get("tools").is_none());
    }

    /// 停止键不该等一个 10 秒的 wait 跑完：轮询到取消就返回，且报的是取消而不是工具失败。
    #[tokio::test]
    async fn cancel_is_honoured_while_waiting() {
        let id = format!("aw-cancel-{}", std::process::id());
        let _guard = crate::cancellation::CancelGuard::new(&id);
        tokio::spawn({
            let id = id.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                crate::cancellation::cancel_request(&id);
            }
        });
        let started = Instant::now();
        let out = await_cancellable(tokio::time::sleep(Duration::from_secs(10)), &id).await;
        let err = out.expect_err("取消应让等待提前返回");
        assert!(err.contains("AI_REQUEST_CANCELLED"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    }

    #[test]
    fn plan_steps_normalise_status_and_drop_junk() {
        let steps = plan_steps(&json!({ "steps": [
            { "title": "读代码", "status": "in_progress" },
            { "title": "改代码", "status": "todo" },
            { "title": "跑测试", "status": "completed" },
            { "title": "   ", "status": "pending" },
            { "status": "pending" }
        ] }));
        let titles: Vec<_> = steps.iter().map(|s| s["title"].as_str().unwrap()).collect();
        assert_eq!(titles, vec!["读代码", "改代码", "跑测试"]);
        assert_eq!(steps[0]["status"], json!("in_progress"));
        assert_eq!(steps[1]["status"], json!("pending"));
        assert_eq!(steps[2]["status"], json!("done"));
        assert_eq!(plan_steps(&json!({ "steps": "x" })).len(), 0);
        // 弱模型会写一屏步骤：只留前 8 条
        let many = json!({ "steps": (0..20).map(|i| json!({ "title": format!("第 {i} 步"), "status": "pending" })).collect::<Vec<_>>() });
        assert_eq!(plan_steps(&many).len(), MAX_PLAN_STEPS);
    }

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
    }

    #[test]
    fn step_arg_prefers_the_call_target() {
        assert_eq!(step_arg(&json!({"path": " src/a.rs ", "command": "ls"})), "src/a.rs");
        assert_eq!(step_arg(&json!({"command": "git status"})), "git status");
        assert_eq!(step_arg(&json!({"pattern": "TODO", "path": ""})), "TODO");
        // numbers count too; nothing meaningful at all stays empty
        assert_eq!(step_arg(&json!({"port": 8080})), "8080");
        assert_eq!(step_arg(&json!({"path": "   ", "note": "x"})), "");
        assert_eq!(step_arg(&json!({})), "");
    }

    #[test]
    fn transport_errors_name_a_cause_and_keep_the_original() {
        for (raw, expect) in [
            ("operation timed out", "超时"),
            ("error sending request for url (https://x)", "请求没发出去"),
            ("destination ip is not available", "请求没发出去"),
            ("dns error: failed to lookup address", "域名解析"),
            ("invalid peer certificate: Expired", "TLS"),
            ("connection refused", "连不上服务"),
        ] {
            let msg = friendly_send_error(raw);
            assert!(msg.contains(expect), "{raw} -> {msg}");
            assert!(msg.contains("原文："), "{msg}");
            assert!(msg.ends_with(raw), "{msg}");
        }
    }

    #[test]
    fn http_errors_map_to_the_next_action() {
        let key = friendly_model_error(401, "Invalid api key");
        assert!(key.contains("密钥") && key.contains("模型配置") && key.contains("HTTP 401"));
        assert!(friendly_model_error(404, "not found").contains("基础 URL"));
        assert!(friendly_model_error(429, "slow down").contains("限流"));
        assert!(friendly_model_error(503, "unavailable").contains("暂时不可用"));
        assert!(friendly_model_error(418, "teapot").contains("模型返回错误（HTTP 418）"));
        // 400 splits on whether the body complains about the context window
        let long = friendly_model_error(
            400,
            "This model's maximum context length is 8192 tokens, however you requested 20000",
        );
        assert!(long.contains("超出模型上下文"), "{long}");
        assert!(!long.contains("参数或模型名不对"), "{long}");
        let bad = friendly_model_error(400, "temperature: value out of range");
        assert!(bad.contains("参数或模型名不对"), "{bad}");
        assert!(bad.contains("原文：temperature"), "{bad}");
    }

    #[test]
    fn bad_json_args_never_become_empty_objects() {
        // 无参工具照旧拿到 {}
        assert_eq!(parse_args("").unwrap(), json!({}));
        assert_eq!(parse_args("   ").unwrap(), json!({}));
        assert_eq!(parse_args("{\"path\":\"a\"}").unwrap()["path"], json!("a"));
        // 半截 JSON 是错误，不是空参
        assert!(parse_args("{\"path\":").is_err());
        assert!(parse_args("not json").is_err());
    }

    #[test]
    fn arg_hint_names_the_real_parameters() {
        let hint = arg_hint("read_file");
        assert!(hint.starts_with("read_file("), "{hint}");
        assert!(hint.contains("path*"), "{hint}");
        assert!(hint.contains("offset"), "{hint}");
        // 无参工具 / 不存在的工具：空提示，不瞎编
        assert_eq!(arg_hint("clipboard_read"), "");
        assert_eq!(arg_hint("no_such_tool"), "");
    }

    #[test]
    fn long_tool_results_are_clipped_with_a_hint() {
        assert_eq!(clip_tool_result("read_file", "short", TOOL_RESULT_CAP), "short");
        let text = "a".repeat(TOOL_RESULT_CAP + 5_000);
        let clipped = clip_tool_result("read_file", &text, TOOL_RESULT_CAP);
        assert!(clipped.chars().count() < TOOL_RESULT_CAP + 200, "{}", clipped.chars().count());
        assert!(clipped.contains("已截断"));
        assert!(clipped.contains("offset"));
        assert!(clip_tool_result("grep", &text, TOOL_RESULT_CAP).contains("缩小 pattern"));
        // 设置项把上限调小时立刻生效（同一段文本、不同预算）
        let tight = clip_tool_result("read_file", &text, 2_000);
        assert!(tight.chars().count() < 2_200, "{}", tight.chars().count());
        assert!(tight.contains("已截断"));
    }

    #[test]
    fn parallel_batches_are_prefixes_of_allowed_reads() {
        let read_ok = BatchStep { readonly: true, allowed: true, ready: true };
        let write = BatchStep { readonly: false, allowed: true, ready: true };
        let ask = BatchStep { readonly: true, allowed: false, ready: true };
        let broken = BatchStep { readonly: true, allowed: true, ready: false };
        assert_eq!(plan_batches(&[read_ok; 6], MAX_PARALLEL), vec![0, 1, 2, 3]);
        assert_eq!(MAX_PARALLEL, 4);
        // 「并发读取」关掉时上限是 1：顺序不变，只是一次跑一个
        assert_eq!(plan_batches(&[read_ok; 6], 1), vec![0]);
        // 第一个调用要改东西：后面的读必须看到它的结果，整轮串行
        assert_eq!(plan_batches(&[write, read_ok], MAX_PARALLEL), Vec::<usize>::new());
        assert_eq!(plan_batches(&[read_ok, write, read_ok], MAX_PARALLEL), vec![0]);
        // 要审批 / 参数坏掉的调用不并发
        assert_eq!(plan_batches(&[ask, read_ok], MAX_PARALLEL), Vec::<usize>::new());
        assert_eq!(plan_batches(&[broken], MAX_PARALLEL), Vec::<usize>::new());
        // open_page 会切页面，不算只读
        assert!(is_readonly_tool("grep") && is_readonly_tool("read_file"));
        assert!(!is_readonly_tool("write_file") && !is_readonly_tool("open_page"));
    }

    #[test]
    fn grep_ignore_switch_forces_no_ignore() {
        let args = json!({ "pattern": "*.rs" });
        // 默认（开着）：原样传下去，由模型 / 工具自己的 no_ignore 决定
        assert_eq!(with_grep_ignore(&args, true), args);
        assert!(with_grep_ignore(&args, false)["no_ignore"].as_bool().unwrap_or(false));
        // 模型明确传了 false，也会被设置项覆盖成 true
        let explicit = json!({ "pattern": "x", "no_ignore": false });
        assert!(with_grep_ignore(&explicit, false)["no_ignore"].as_bool().unwrap_or(false));
    }
}
