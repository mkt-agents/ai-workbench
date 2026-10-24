//! Minimal MCP (Model Context Protocol) client over stdio.
//!
//! Scope, deliberately small: one server = one command; we speak newline-delimited
//! JSON-RPC 2.0 to `cmd /c <command> <args>`, do `initialize` → `notifications/initialized`
//! → one request (`tools/list` or `tools/call`), then kill the process. No long-lived
//! session, no resources / prompts / sampling, no HTTP transport yet.
//!
//! The cost of that choice is a process start per call (npx cold start is a second or two);
//! the benefit is that a crashed or hung server can never poison the app — every call has
//! its own deadline and its own process.
//!
//! Tools are exposed to the model as `mcp_<server>_<tool>`; the mapping back to
//! (server, tool) is a lookup in the list built for this turn, never a string split
//! (server names may contain `_`).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A server the user configured (settings → 天玑 → MCP 服务).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    pub command: String,
    /// Argument line as typed by the user; split by [`split_args`].
    #[serde(default)]
    pub args: String,
}

#[derive(Debug, Clone)]
pub(crate) struct McpTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

const INIT_TIMEOUT: Duration = Duration::from_secs(20);
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const PROTOCOL_VERSION: &str = "2024-11-05";

/// `npx -y @modelcontextprotocol/server-filesystem "D:\my work"` → argv without quotes.
/// Backslash is literal on purpose (Windows paths are the common argument).
pub(crate) fn split_args(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for ch in raw.chars() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
                has = true;
            }
            Some(_) => cur.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                has = true;
            }
            None if ch.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            None => cur.push(ch),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Server name as it appears inside a tool name: lower-case, `[a-z0-9_]` only.
pub(crate) fn sanitize_name(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// The name the model sees.
pub(crate) fn full_name(server: &str, tool: &str) -> String {
    format!("mcp_{}_{}", sanitize_name(server), sanitize_name(tool))
}

/// One MCP tool, in the shape the chosen provider expects.
pub(crate) fn provider_spec(server: &str, tool: &McpTool, anthropic: bool) -> Value {
    let name = full_name(server, &tool.name);
    let description = if tool.description.trim().is_empty() {
        format!("MCP 工具 {}（来自 {}）", tool.name, sanitize_name(server))
    } else {
        format!("{}（MCP: {}）", tool.description.trim(), sanitize_name(server))
    };
    let schema = if tool.schema.is_object() { tool.schema.clone() } else { json!({ "type": "object" }) };
    if anthropic {
        json!({ "name": name, "description": description, "input_schema": schema })
    } else {
        json!({ "type": "function", "function": { "name": name, "description": description, "parameters": schema } })
    }
}

/// A child process talking JSON-RPC over stdin/stdout.
struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Session {
    fn start(spec: &McpServerSpec) -> Result<Self, String> {
        if spec.command.trim().is_empty() {
            return Err("没有填启动命令".into());
        }
        let mut cmd = Command::new("cmd");
        cmd.arg("/c")
            .arg(spec.command.trim())
            .args(split_args(&spec.args))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
        let stdin = child.stdin.take().ok_or("拿不到标准输入")?;
        let stdout = child.stdout.take().ok_or("拿不到标准输出")?;
        let (tx, lines) = mpsc::channel();
        // 服务端会往 stdout 打印日志，非 JSON 行直接丢；通道关闭即进程结束
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self { child, stdin, lines, next_id: 1 })
    }

    fn write(&mut self, msg: &Value) -> Result<(), String> {
        let mut text = msg.to_string();
        text.push('\n');
        self.stdin.write_all(text.as_bytes()).map_err(|e| format!("写入失败: {e}"))?;
        self.stdin.flush().map_err(|e| format!("写入失败: {e}"))
    }

    /// Send a request and wait for the response with the same id.
    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("{method} 超时（{}s）", timeout.as_secs()));
            }
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("{method} 超时（{}s）", timeout.as_secs()))?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if msg.get("id").and_then(|v| v.as_u64()) == Some(id) && msg.get("method").is_none() {
                if let Some(err) = msg.get("error") {
                    let text = err.get("message").and_then(|v| v.as_str()).unwrap_or("未知错误");
                    return Err(format!("{method} 失败: {text}"));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            // 服务端反向请求（sampling 等）：我们什么都不支持，明确拒绝，别让它干等
            if let (Some(method), Some(their_id)) = (msg.get("method").and_then(|v| v.as_str()), msg.get("id")) {
                let _ = self.write(&json!({
                    "jsonrpc": "2.0",
                    "id": their_id,
                    "error": { "code": -32601, "message": format!("{method} 不支持") }
                }));
            }
        }
    }

    fn handshake(&mut self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "ai-workbench", "version": env!("CARGO_PKG_VERSION") }
            }),
            INIT_TIMEOUT,
        )?;
        // 通知没有 id，也没有回复
        self.write(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The server's tool list. Blocking: call it from `spawn_blocking`.
pub(crate) fn list_tools(spec: &McpServerSpec) -> Result<Vec<McpTool>, String> {
    let mut session = Session::start(spec)?;
    session.handshake()?;
    let result = session.request("tools/list", json!({}), LIST_TIMEOUT)?;
    let tools = result
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let name = t.get("name").and_then(|v| v.as_str())?.to_string();
                    Some(McpTool {
                        description: t.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        schema: t.get("inputSchema").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                        name,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(tools)
}

/// Call one tool; the text parts of the result are joined. Blocking.
pub(crate) fn call_tool(spec: &McpServerSpec, tool: &str, args: &Value) -> Result<String, String> {
    let mut session = Session::start(spec)?;
    session.handshake()?;
    let result = session.request("tools/call", json!({ "name": tool, "arguments": args }), CALL_TIMEOUT)?;
    let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
    let text = result
        .get("content")
        .and_then(|v| v.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| match p.get("type").and_then(|v| v.as_str()) {
                    Some("text") => p.get("text").and_then(|v| v.as_str()).map(str::to_string),
                    // 图片 / 资源：只留一句占位，工具结果是文本进上下文的
                    Some(other) => Some(format!("[{other} 内容已省略]")),
                    None => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(if is_error { format!("工具返回错误：{result}") } else { "（工具没有返回文本）".into() });
    }
    Ok(if is_error { format!("工具返回错误：{text}") } else { text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_are_split_with_quotes() {
        assert_eq!(split_args(""), Vec::<String>::new());
        assert_eq!(split_args("  -y   pkg "), vec!["-y", "pkg"]);
        assert_eq!(split_args(r#"-y "D:\my work" --x"#), vec!["-y", r"D:\my work", "--x"]);
        assert_eq!(split_args("'a b' c"), vec!["a b", "c"]);
        // 空引号也是参数（某些 server 需要空串）
        assert_eq!(split_args(r#""""#), vec![""]);
    }

    #[test]
    fn names_are_sanitized_and_namespaced() {
        assert_eq!(sanitize_name("My Server"), "my_server");
        assert_eq!(sanitize_name("文件系统"), "");
        assert_eq!(sanitize_name("fs--svc"), "fs_svc");
        assert_eq!(full_name("My Server", "read_file"), "mcp_my_server_read_file");
    }

    /// 真的跑一个最小 MCP server（node 写个假服务），验证握手 / 列工具 / 调用 / 忽略日志行。
    /// 没有 node 就跳过：这个测试的价值在协议帧，不在环境。
    #[test]
    fn talks_to_a_fake_stdio_server() {
        let node = std::process::Command::new("node").arg("-v").output();
        if node.map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("skip: node not found");
            return;
        }
        const SERVER: &str = r#"
const readline = require('readline');
console.log('log line that is not json');           // 必须被忽略
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', (line) => {
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.method === 'initialize') {
    console.log(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: {
      protocolVersion: '2024-11-05', capabilities: { tools: {} },
      serverInfo: { name: 'fake', version: '1.0.0' } } }));
  } else if (msg.method === 'tools/list') {
    console.log(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { tools: [
      { name: 'echo', description: 'Echo the text', inputSchema: { type: 'object',
        properties: { text: { type: 'string' } }, required: ['text'] } } ] } }));
  } else if (msg.method === 'tools/call') {
    console.log(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: {
      content: [{ type: 'text', text: 'echo: ' + msg.params.arguments.text }] } }));
  } else if (msg.id !== undefined) {
    console.log(JSON.stringify({ jsonrpc: '2.0', id: msg.id,
      error: { code: -32601, message: 'no such method' } }));
  }
});
"#;
        let dir = std::env::temp_dir().join(format!("aw-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-server.js");
        std::fs::write(&script, SERVER).unwrap();
        let spec = McpServerSpec {
            name: "fake".into(),
            command: "node".into(),
            args: format!("\"{}\"", script.display()),
        };
        let tools = list_tools(&spec).expect("握手 + tools/list");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].schema["properties"]["text"]["type"], json!("string"));
        let out = call_tool(&spec, "echo", &json!({ "text": "hi" })).expect("tools/call");
        assert_eq!(out, "echo: hi");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provider_specs_match_each_protocol() {
        let tool = McpTool {
            name: "read_file".into(),
            description: "Read a file".into(),
            schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        };
        let openai = provider_spec("fs", &tool, false);
        assert_eq!(openai["type"], json!("function"));
        assert_eq!(openai["function"]["name"], json!("mcp_fs_read_file"));
        assert_eq!(openai["function"]["parameters"]["properties"]["path"]["type"], json!("string"));
        let anthropic = provider_spec("fs", &tool, true);
        assert_eq!(anthropic["name"], json!("mcp_fs_read_file"));
        assert_eq!(anthropic["input_schema"]["type"], json!("object"));
        // 没有 description 也要有话说；schema 不是对象时兜底成空对象
        let bare = McpTool { name: "ping".into(), description: "".into(), schema: json!(null) };
        let spec = provider_spec("fs", &bare, false);
        assert!(spec["function"]["description"].as_str().unwrap().contains("ping"));
        assert_eq!(spec["function"]["parameters"]["type"], json!("object"));
    }
}
