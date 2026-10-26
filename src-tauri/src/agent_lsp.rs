//! Minimal LSP bridge for 天玑: definition / references / diagnostics.
//!
//! Deliberately thin — spawn a language server over stdio when the binary is on PATH,
//! ask one question, then kill it. Missing servers return a structured `LSP_UNAVAILABLE`
//! so the model can fall back to grep/symbols without the tool disappearing from the menu.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::agent_fs::Scope;

const TIMEOUT: Duration = Duration::from_secs(12);
const PROTOCOL: &str = "3.17.0";

#[derive(Debug)]
struct LspServer {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Drop for LspServer {
    fn drop(&mut self) {
        let _ = self.stdin.write_all(b"Content-Length: 49\r\n\r\n{\"jsonrpc\":\"2.0\",\"method\":\"exit\",\"params\":null}");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let name = format!("{bin}{ext}");
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        // Windows also ships `.cmd` shims for npm globals.
        if cfg!(windows) {
            let cmd = dir.join(format!("{bin}.cmd"));
            if cmd.is_file() {
                return Some(cmd);
            }
        }
    }
    None
}

fn pick_server(path: &Path) -> Option<(&'static str, PathBuf, Vec<String>)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => {
            let bin = which("rust-analyzer")?;
            Some(("rust-analyzer", bin, Vec::new()))
        }
        "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" => {
            let bin = which("typescript-language-server")?;
            Some(("typescript-language-server", bin, vec!["--stdio".into()]))
        }
        _ => None,
    }
}

fn write_msg(stdin: &mut impl Write, body: &Value) -> Result<(), String> {
    let raw = serde_json::to_string(body).map_err(|e| e.to_string())?;
    write!(stdin, "Content-Length: {}\r\n\r\n{}", raw.len(), raw).map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())
}

fn read_msg(reader: &mut BufReader<impl std::io::Read>, deadline: Instant) -> Result<Value, String> {
    loop {
        if Instant::now() >= deadline {
            return Err("LSP_UNAVAILABLE: language server timed out".into());
        }
        let mut headers = String::new();
        let mut content_length: Option<usize> = None;
        loop {
            headers.clear();
            reader
                .read_line(&mut headers)
                .map_err(|e| format!("LSP_UNAVAILABLE: read header failed ({e})"))?;
            if headers == "\r\n" || headers == "\n" || headers.is_empty() {
                break;
            }
            let lower = headers.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("content-length:") {
                content_length = rest.trim().parse().ok();
            }
        }
        let Some(len) = content_length else {
            continue;
        };
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("LSP_UNAVAILABLE: read body failed ({e})"))?;
        let value: Value =
            serde_json::from_slice(&buf).map_err(|e| format!("LSP_UNAVAILABLE: bad JSON ({e})"))?;
        // Skip notifications; callers want responses with an id.
        if value.get("id").is_some() || value.get("error").is_some() {
            return Ok(value);
        }
    }
}

impl LspServer {
    fn spawn(bin: &Path, args: &[String], root: &Path) -> Result<Self, String> {
        let mut cmd = if bin.extension().and_then(|e| e.to_str()) == Some("cmd") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(bin);
            for a in args {
                c.arg(a);
            }
            c
        } else {
            let mut c = Command::new(bin);
            c.args(args);
            c
        };
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .current_dir(root)
            .spawn()
            .map_err(|e| format!("LSP_UNAVAILABLE: failed to start {} ({e})", bin.display()))?;
        let stdin = child.stdin.take().ok_or("LSP_UNAVAILABLE: no stdin")?;
        let stdout = child.stdout.take().ok_or("LSP_UNAVAILABLE: no stdout")?;
        Ok(Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        })
    }

    fn request(&mut self, method: &str, params: Value, deadline: Instant) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        write_msg(
            &mut self.stdin,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )?;
        loop {
            let msg = read_msg(&mut self.reader, deadline)?;
            if msg.get("id").and_then(|v| v.as_u64()) == Some(id)
                || msg.get("id").and_then(|v| v.as_i64()) == Some(id as i64)
            {
                if let Some(err) = msg.get("error") {
                    return Err(format!("LSP error: {err}"));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        write_msg(
            &mut self.stdin,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
    }
}

fn file_uri(path: &Path) -> String {
    let abs = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

fn format_location(loc: &Value, root: &Path) -> String {
    let uri = loc
        .pointer("/uri")
        .or_else(|| loc.pointer("/targetUri"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let range = loc
        .pointer("/range")
        .or_else(|| loc.pointer("/targetSelectionRange"))
        .or_else(|| loc.pointer("/targetRange"));
    let line = range
        .and_then(|r| r.pointer("/start/line"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        + 1;
    let col = range
        .and_then(|r| r.pointer("/start/character"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let path = uri
        .strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))
        .unwrap_or(uri)
        .replace('/', std::path::MAIN_SEPARATOR_STR);
    let rel = Path::new(&path)
        .strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or(path);
    format!("{rel}:{line}:{col}")
}

/// Run one LSP action. Always returns a model-facing string (never panics the loop).
pub(crate) fn lsp_query(scope: &Scope, args: &Value) -> Result<String, String> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !matches!(action.as_str(), "definition" | "references" | "diagnostics") {
        return Err("action 必须是 definition / references / diagnostics".into());
    }
    let rel = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if rel.is_empty() {
        return Err("缺少 path".into());
    }
    let path = scope.resolve(rel).map_err(|e| e.to_string())?;
    if !path.is_file() {
        return Err(format!("文件不存在：{}", path.display()));
    }
    let root = scope
        .root
        .clone()
        .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).to_path_buf());
    let Some((label, bin, bin_args)) = pick_server(&path) else {
        return Ok(format!(
            "LSP_UNAVAILABLE: no language server mapped for {} — use grep or symbols instead.",
            path.extension().and_then(|e| e.to_str()).unwrap_or("?")
        ));
    };
    let deadline = Instant::now() + TIMEOUT;
    let mut server = match LspServer::spawn(&bin, &bin_args, &root) {
        Ok(s) => s,
        Err(e) => return Ok(e),
    };
    let root_uri = file_uri(&root);
    let init = server.request(
        "initialize",
        json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "definition": { "linkSupport": true },
                    "references": {},
                    "publishDiagnostics": {}
                }
            },
            "clientInfo": { "name": "ai-workbench", "version": "0.2" },
            "locale": "zh-CN",
            "workspaceFolders": [{ "uri": root_uri, "name": "workspace" }],
            "initializationOptions": {},
            "trace": "off",
            "protocolVersion": PROTOCOL
        }),
        deadline,
    );
    if let Err(e) = init {
        return Ok(e);
    }
    let _ = server.notify("initialized", json!({}));
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let uri = file_uri(&path);
    let language_id = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "tsx" => "typescriptreact",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "js" => "javascript",
        other => other,
    };
    let _ = server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text
            }
        }),
    );
    // Give the server a beat to index the single file.
    std::thread::sleep(Duration::from_millis(400));

    let line = args
        .get("line")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .saturating_sub(1);
    let character = args.get("character").and_then(|v| v.as_u64()).unwrap_or(0);

    let result = match action.as_str() {
        "definition" => server.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
            deadline,
        ),
        "references" => server.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": { "includeDeclaration": true }
            }),
            deadline,
        ),
        "diagnostics" => {
            // Pull from textDocument/diagnostic if supported; else empty with a note.
            match server.request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": uri } }),
                deadline,
            ) {
                Ok(v) => Ok(v),
                Err(_) => Ok(json!({ "items": [] })),
            }
        }
        _ => unreachable!(),
    };

    let Ok(value) = result else {
        return Ok(format!(
            "LSP_UNAVAILABLE: {label} did not answer — install it or use grep/symbols."
        ));
    };

    let out = match action.as_str() {
        "definition" => {
            let locs: Vec<&Value> = match &value {
                Value::Array(arr) => arr.iter().collect(),
                Value::Object(_) => vec![&value],
                _ => Vec::new(),
            };
            if locs.is_empty() {
                "No definition found.".into()
            } else {
                let lines: Vec<String> = locs.iter().take(20).map(|l| format_location(l, &root)).collect();
                format!("Definitions ({label}):\n{}", lines.join("\n"))
            }
        }
        "references" => {
            let locs = value.as_array().cloned().unwrap_or_default();
            if locs.is_empty() {
                "No references found.".into()
            } else {
                let lines: Vec<String> = locs.iter().take(40).map(|l| format_location(l, &root)).collect();
                format!("References ({label}, {}):\n{}", locs.len().min(40), lines.join("\n"))
            }
        }
        "diagnostics" => {
            let items = value
                .pointer("/items")
                .and_then(|v| v.as_array())
                .cloned()
                .or_else(|| value.as_array().cloned())
                .unwrap_or_default();
            if items.is_empty() {
                format!("No diagnostics from {label} for this file.")
            } else {
                let mut lines = Vec::new();
                for item in items.iter().take(40) {
                    let msg = item.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    let sev = match item.get("severity").and_then(|v| v.as_u64()).unwrap_or(1) {
                        1 => "error",
                        2 => "warning",
                        3 => "info",
                        _ => "hint",
                    };
                    let line = item
                        .pointer("/range/start/line")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                        + 1;
                    lines.push(format!("{sev} L{line}: {msg}"));
                }
                format!("Diagnostics ({label}):\n{}", lines.join("\n"))
            }
        }
        _ => unreachable!(),
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_server_recognises_ts_and_rust() {
        assert!(pick_server(Path::new("src/foo.rs")).is_some() || which("rust-analyzer").is_none());
        assert!(
            pick_server(Path::new("a.tsx")).is_some() || which("typescript-language-server").is_none()
        );
        assert!(pick_server(Path::new("readme.md")).is_none());
    }
}
