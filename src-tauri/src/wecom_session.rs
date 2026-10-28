//! 企微会话键与短历史（内存 + 可选旁证目录）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::ai_commands::ChatTurn;

const MAX_TURNS: usize = 24;
const MAX_SESSIONS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionKey {
    pub kind: String,
    pub id: String,
}

impl SessionKey {
    pub fn dm(userid: &str) -> Self {
        Self {
            kind: "dm".into(),
            id: userid.to_string(),
        }
    }

    pub fn group(chatid: &str) -> Self {
        Self {
            kind: "group".into(),
            id: chatid.to_string(),
        }
    }

    pub fn as_str(&self) -> String {
        format!("wecom:{}:{}", self.kind, self.id)
    }
}

#[derive(Debug, Default)]
struct SessionStore {
    /// key → turns
    map: HashMap<String, Vec<ChatTurn>>,
    /// LRU order (oldest first)
    order: Vec<String>,
}

impl SessionStore {
    fn touch(&mut self, key: &str) {
        self.order.retain(|k| k != key);
        self.order.push(key.to_string());
        while self.order.len() > MAX_SESSIONS {
            if let Some(old) = self.order.first().cloned() {
                self.order.remove(0);
                self.map.remove(&old);
            } else {
                break;
            }
        }
    }

    fn history(&mut self, key: &str) -> Vec<ChatTurn> {
        self.touch(key);
        self.map.get(key).cloned().unwrap_or_default()
    }

    fn append(&mut self, key: &str, user: &str, assistant: &str) {
        self.touch(key);
        let turns = self.map.entry(key.to_string()).or_default();
        turns.push(ChatTurn {
            role: "user".into(),
            content: user.to_string(),
        });
        turns.push(ChatTurn {
            role: "assistant".into(),
            content: assistant.to_string(),
        });
        while turns.len() > MAX_TURNS {
            turns.remove(0);
        }
    }

    fn clear(&mut self, key: &str) {
        self.map.remove(key);
        self.order.retain(|k| k != key);
    }
}

static SESSIONS: Mutex<Option<SessionStore>> = Mutex::new(None);

fn store() -> std::sync::MutexGuard<'static, Option<SessionStore>> {
    let mut g = SESSIONS.lock().expect("wecom sessions");
    if g.is_none() {
        *g = Some(SessionStore::default());
    }
    g
}

pub fn session_history(key: &SessionKey) -> Vec<ChatTurn> {
    let k = key.as_str();
    // 内存优先；重启后内存为空时回读旁证 JSONL，避免多轮上下文丢失
    let mem = store().as_mut().map(|s| s.history(&k)).unwrap_or_default();
    if !mem.is_empty() {
        return mem;
    }
    load_sidecar(&k)
}

pub fn append_turn(key: &SessionKey, user: &str, assistant: &str) {
    let k = key.as_str();
    if let Some(s) = store().as_mut() {
        s.append(&k, user, assistant);
    }
    // 旁证：与 agent-sessions 同目录下 wecom-sessions/
    if let Err(e) = append_sidecar(&k, user, assistant) {
        eprintln!("[wecom] session sidecar: {e}");
    }
}

pub fn clear_session(key: &SessionKey) {
    let k = key.as_str();
    if let Some(s) = store().as_mut() {
        s.clear(&k);
    }
}

fn sidecar_dir() -> Option<PathBuf> {
    let base = dirs_data()?;
    let dir = base.join("wecom-sessions");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn dirs_data() -> Option<PathBuf> {
    // 与 agent_session_log 一致：%APPDATA%\com.ai-workbench.app\
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("com.ai-workbench.app"))
}

/// 从旁证 JSONL 回读历史（重启后内存为空时兜底）。每两行合成一对 user/assistant 轮次。
fn load_sidecar(key: &str) -> Vec<ChatTurn> {
    let dir = match sidecar_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}.jsonl"));
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut turns = Vec::new();
    for line in data.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let (u, a) = (
            v.get("user").and_then(|x| x.as_str()),
            v.get("assistant").and_then(|x| x.as_str()),
        );
        if let (Some(u), Some(a)) = (u, a) {
            turns.push(ChatTurn {
                role: "user".into(),
                content: u.to_string(),
            });
            turns.push(ChatTurn {
                role: "assistant".into(),
                content: a.to_string(),
            });
        }
    }
    while turns.len() > MAX_TURNS {
        turns.remove(0);
    }
    turns
}

/// 设置页「会话日志」折叠区的一条展示项。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLogEntry {
    pub session: String,
    pub ts: String,
    pub user: String,
    pub assistant: String,
}

/// 读最近 `limit` 条会话旁证，按时间倒序。
/// `key` 传 `None` 汇总所有会话文件；传会话键（`wecom:dm:u1`）只读该会话。
pub fn recent_entries(key: Option<&str>, limit: usize) -> Vec<SessionLogEntry> {
    let dir = match sidecar_dir() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    match key {
        Some(k) => {
            let safe = sanitize_key(k);
            files.push((k.to_string(), dir.join(format!("{safe}.jsonl"))));
        }
        None => {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        let name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        files.push((name, path));
                    }
                }
            }
        }
    }
    let mut out: Vec<SessionLogEntry> = Vec::new();
    for (session, path) in files {
        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in data.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            out.push(SessionLogEntry {
                session: session.clone(),
                ts: v.get("ts").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                user: v.get("user").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                assistant: v
                    .get("assistant")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    // 按时间戳倒序（RFC3339 字典序即时间序）；取最近 limit 条。
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    out.truncate(limit);
    out
}

/// 会话键 → 安全文件名（与 append_sidecar / load_sidecar 同规则）。
fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn append_sidecar(key: &str, user: &str, assistant: &str) -> Result<(), String> {
    let dir = sidecar_dir().ok_or_else(|| "no app data dir".to_string())?;
    let safe: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = dir.join(format!("{safe}.jsonl"));
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "user": user,
        "assistant": assistant,
    });
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_key_format() {
        assert_eq!(SessionKey::dm("u1").as_str(), "wecom:dm:u1");
        assert_eq!(SessionKey::group("g1").as_str(), "wecom:group:g1");
    }
}
