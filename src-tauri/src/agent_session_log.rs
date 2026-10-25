//! Minimal session event log: append model-visible facts as JSONL.
//! Primary restore path is AiHubTurn.blocks; this is a disk side log for debug / future projection.
//! Failures are swallowed — logging must never break a turn.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

fn sessions_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(appdata)
        .join("com.ai-workbench.app")
        .join("agent-sessions");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn append_line(request_id: &str, event: Value) {
    let Some(dir) = sessions_dir() else { return };
    let safe: String = request_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if safe.is_empty() {
        return;
    }
    let path = dir.join(format!("{safe}.jsonl"));
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let Ok(line) = serde_json::to_string(&event) else { return };
    let _ = writeln!(file, "{line}");
}

pub(crate) fn log_user(request_id: &str, text: &str) {
    append_line(
        request_id,
        json!({
            "ts": chrono_now_ms(),
            "kind": "user",
            "text": text.chars().take(4000).collect::<String>(),
        }),
    );
}

pub(crate) fn log_assistant(request_id: &str, text: &str) {
    append_line(
        request_id,
        json!({
            "ts": chrono_now_ms(),
            "kind": "assistant",
            "text": text.chars().take(8000).collect::<String>(),
        }),
    );
}

pub(crate) fn log_tool(request_id: &str, call_id: &str, name: &str, output: &str, is_error: bool) {
    append_line(
        request_id,
        json!({
            "ts": chrono_now_ms(),
            "kind": "tool",
            "callId": call_id,
            "name": name,
            "isError": is_error,
            "output": output.chars().take(4000).collect::<String>(),
        }),
    );
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_survives_empty_request_id() {
        log_user("", "hi");
        log_tool("_test_session_", "c1", "read_file", "ok", false);
    }
}