//! 企微会话键与短历史（内存 + 可选旁证目录）。

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    /// Versioned, domain-separated tuple hashing: identifiers never use delimiter concatenation.
    fn scoped(kind: &str, parts: &[&str]) -> Self {
        let encoded = serde_json::to_vec(parts).expect("string tuple serialization");
        Self {
            kind: kind.into(),
            id: format!("v2-{:x}", Sha256::digest(encoded)),
        }
    }

    pub fn dm_for(bot: &str, user: &str) -> Self {
        Self::scoped("dm", &["dm", bot, user])
    }

    pub fn group_user(bot: &str, group: &str, user: &str) -> Self {
        Self::scoped("group", &["group-user", bot, group, user])
    }

    pub fn shared_task(bot: &str, group: &str, task: &str) -> Self {
        Self::scoped("group", &["shared-task", bot, group, task])
    }

    /// Legacy lookup only. New ingress must use dm_for; no inferred bot migration.
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

    fn history(&mut self, key: &str, dir: Option<&Path>) -> Vec<ChatTurn> {
        self.touch(key);
        // Cache even an empty read. Hydration must populate memory before the next append.
        if !self.map.contains_key(key) {
            self.map.insert(key.to_string(), load_sidecar_at(dir, key));
        }
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
        self.touch(key);
        self.map.insert(key.to_string(), Vec::new());
    }

    fn clear_at(&mut self, key: &str, dir: Option<&Path>) -> Result<(), String> {
        // A durable clear marker in the exact sidecar is honored on restart and LRU eviction.
        // Preserve audit lines, but stop hydration at the most recent clear marker.
        if let Some(dir) = dir {
            write_sidecar_line(
                dir,
                key,
                &serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(), "cleared": true
                }),
            )?;
        }
        self.clear(key);
        Ok(())
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
    let dir = sidecar_dir();
    store()
        .as_mut()
        .map(|s| s.history(&key.as_str(), dir.as_deref()))
        .unwrap_or_default()
}

pub fn append_turn(key: &SessionKey, user: &str, assistant: &str) {
    append_turn_with_request(key, user, assistant, None);
}

pub fn append_turn_with_request(
    key: &SessionKey,
    user: &str,
    assistant: &str,
    request_id: Option<&str>,
) {
    let k = key.as_str();
    let dir = sidecar_dir();
    // Serialize hydration, append and clear including sidecar IO to avoid resurrecting old turns.
    let mut guard = store();
    if let Some(s) = guard.as_mut() {
        s.history(&k, dir.as_deref());
        s.append(&k, user, assistant);
    }
    if let Some(dir) = dir {
        if let Err(e) = append_sidecar_with_request_at(&dir, &k, user, assistant, request_id) {
            eprintln!("[wecom] session sidecar: {e}");
        }
    }
}

/// Clears only this exact session, not the legacy DM/group or another member's session.
/// New callers can use the checked form to surface persistence failures.
pub fn clear_session_checked(key: &SessionKey) -> Result<(), String> {
    let dir = Some(sidecar_dir().ok_or_else(|| "session directory unavailable".to_string())?);
    if let Some(s) = store().as_mut() {
        s.clear_at(&key.as_str(), dir.as_deref())?;
    }
    Ok(())
}

pub fn clear_session(key: &SessionKey) {
    if let Err(e) = clear_session_checked(key) {
        eprintln!("[wecom] session clear: {e}");
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

const SIDECAR_READ_BYTES: u64 = 256 * 1024;
const RECENT_READ_BYTES: u64 = 8 * 1024 * 1024;
const RECENT_MAX_FILES: usize = 512;

// Read a suffix only; discard partial boundary records rather than parsing fragments.
fn read_suffix(path: &Path, budget: u64) -> Result<(String, u64, bool), String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let start = len.saturating_sub(budget);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(budget)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let scanned = bytes.len() as u64;
    let mut partial = start > 0;
    let first = if start > 0 {
        bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |p| p + 1)
    } else {
        0
    };
    let last = if bytes.last() == Some(&b'\n') {
        bytes.len()
    } else {
        partial = !bytes.is_empty() || partial;
        bytes
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(first, |p| p + 1)
    };
    let text = std::str::from_utf8(&bytes[first..last.max(first)])
        .map_err(|_| "invalid UTF-8 in session suffix".to_string())?
        .to_string();
    Ok((text, scanned, partial))
}

/// Each JSONL record contains a user/assistant pair. Paths are injectable for isolated tests.
fn load_sidecar_at(dir: Option<&Path>, key: &str) -> Vec<ChatTurn> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let path = dir.join(format!("{}.jsonl", sanitize_key(key)));
    let data = match read_suffix(&path, SIDECAR_READ_BYTES) {
        Ok((data, _, _)) => data,
        Err(_) => return Vec::new(),
    };
    let mut turns = Vec::new();
    for line in data.lines().rev().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("cleared").and_then(|v| v.as_bool()) == Some(true) {
            break;
        }
        let (u, a) = (
            v.get("user").and_then(|x| x.as_str()),
            v.get("assistant").and_then(|x| x.as_str()),
        );
        if let (Some(u), Some(a)) = (u, a) {
            turns.push(ChatTurn {
                role: "assistant".into(),
                content: a.to_string(),
            });
            turns.push(ChatTurn {
                role: "user".into(),
                content: u.to_string(),
            });
            if turns.len() >= MAX_TURNS {
                break;
            }
        }
    }
    turns.reverse();
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentEntriesPage {
    pub entries: Vec<SessionLogEntry>,
    pub partial: bool,
    pub read_errors: Vec<String>,
    pub scanned_bytes: u64,
}

/// 读最近 `limit` 条会话旁证，按时间倒序。
/// `key` 传 `None` 汇总所有会话文件；传会话键（`wecom:dm:u1`）只读该会话。
pub fn recent_entries(key: Option<&str>, limit: usize) -> Vec<SessionLogEntry> {
    recent_entries_page(key, limit)
        .map(|p| p.entries)
        .unwrap_or_default()
}

#[allow(dead_code)] // Compatibility adapter; structured consumers use recent_entries_page.
pub fn recent_entries_checked(
    key: Option<&str>,
    limit: usize,
) -> Result<Vec<SessionLogEntry>, String> {
    let page = recent_entries_page(key, limit)?;
    if !page.read_errors.is_empty() {
        return Err(page.read_errors.join("; "));
    }
    Ok(page.entries)
}

pub fn recent_entries_page(key: Option<&str>, limit: usize) -> Result<RecentEntriesPage, String> {
    let dir = sidecar_dir().ok_or_else(|| "session directory unavailable".to_string())?;
    recent_entries_page_at(&dir, key, limit)
}

fn recent_entries_page_at(
    dir: &Path,
    key: Option<&str>,
    limit: usize,
) -> Result<RecentEntriesPage, String> {
    let limit = limit.clamp(1, 200);
    let mut page = RecentEntriesPage::default();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    match key {
        Some(k) => {
            let safe = sanitize_key(k);
            files.push((k.to_string(), dir.join(format!("{safe}.jsonl"))));
        }
        None => {
            let rd = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(page),
                Err(e) => return Err(crate::wecom_sanitize::redact_diagnostic(&e.to_string())),
            };
            for (index, entry) in rd.enumerate() {
                if index >= RECENT_MAX_FILES {
                    page.partial = true;
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        page.partial = true;
                        if page.read_errors.len() < 32 {
                            page.read_errors
                                .push(crate::wecom_sanitize::redact_diagnostic(&e.to_string()));
                        }
                        continue;
                    }
                };
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
    files.sort_by_cached_key(|(_, path)| {
        std::cmp::Reverse(std::fs::metadata(path).and_then(|m| m.modified()).ok())
    });
    let mut out: Vec<SessionLogEntry> = Vec::new();
    let mut remaining = RECENT_READ_BYTES;
    for (session, path) in files {
        if remaining == 0 {
            page.partial = true;
            break;
        }
        let budget = SIDECAR_READ_BYTES.min(remaining);
        remaining -= budget; // Failed reads also consume admission, preventing retry amplification.
        let data = match read_suffix(&path, budget) {
            Ok((data, bytes, partial)) => {
                remaining += budget - bytes;
                page.scanned_bytes += bytes;
                page.partial |= partial;
                data
            }
            Err(e) => {
                page.partial = true;
                page.read_errors
                    .push(crate::wecom_sanitize::redact_diagnostic(&e));
                continue;
            }
        };
        let mut file_entries = 0;
        for line in data.lines().rev().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                page.partial = true;
                if page.read_errors.len() < 32 {
                    page.read_errors.push("invalid session JSONL record".into());
                }
                continue;
            };
            if file_entries >= limit {
                page.partial = true;
                break;
            }
            if v.get("cleared").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
            file_entries += 1;
            out.push(SessionLogEntry {
                request_id: v
                    .get("requestId")
                    .and_then(|x| x.as_str())
                    .map(str::to_string),
                session: session.clone(),
                ts: v
                    .get("ts")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                user: v
                    .get("user")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                assistant: v
                    .get("assistant")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
        out.sort_by(|a, b| b.ts.cmp(&a.ts));
        if out.len() > limit {
            page.partial = true;
            out.truncate(limit);
        }
    }
    page.entries = out;
    page.read_errors.truncate(32);
    Ok(page)
}

/// 会话键 → 安全文件名（与 append_sidecar / load_sidecar 同规则）。
fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn append_sidecar_at(dir: &Path, key: &str, user: &str, assistant: &str) -> Result<(), String> {
    append_sidecar_with_request_at(dir, key, user, assistant, None)
}

fn append_sidecar_with_request_at(
    dir: &Path,
    key: &str,
    user: &str,
    assistant: &str,
    request_id: Option<&str>,
) -> Result<(), String> {
    write_sidecar_line(
        dir,
        key,
        &serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "requestId": request_id,
            "user": user,
            "assistant": assistant,
        }),
    )
}

fn write_sidecar_line(dir: &Path, key: &str, line: &serde_json::Value) -> Result<(), String> {
    let path = dir.join(format!("{}.jsonl", sanitize_key(key)));
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

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "wecom-session-test-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bounded_recent_page_preserves_request_and_reports_omissions() {
        let temp = TempDir::new();
        let key = "fixture";
        let path = temp.0.join("fixture.jsonl");
        std::fs::write(
            &path,
            format!("{}\n", "x".repeat(SIDECAR_READ_BYTES as usize + 100)),
        )
        .unwrap();
        append_sidecar_with_request_at(
            &temp.0,
            key,
            "synthetic question",
            "synthetic answer",
            Some("request-1"),
        )
        .unwrap();
        let page = recent_entries_page_at(&temp.0, Some(key), 30).unwrap();
        assert!(page.partial);
        assert!(page.scanned_bytes <= SIDECAR_READ_BYTES);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].request_id.as_deref(), Some("request-1"));
        let loaded = load_sidecar_at(Some(&temp.0), key);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content, "synthetic question");
        let missing = recent_entries_page_at(&temp.0, Some("missing"), 1).unwrap();
        assert!(missing.partial && !missing.read_errors.is_empty());
    }

    #[test]
    fn absent_history_directory_is_an_empty_complete_page() {
        let temp = TempDir::new();
        let page = recent_entries_page_at(&temp.0.join("not-created"), None, 30).unwrap();
        assert!(page.entries.is_empty());
        assert!(page.read_errors.is_empty());
        assert!(!page.partial);
        assert_eq!(page.scanned_bytes, 0);
    }

    #[test]
    fn incomplete_tail_and_invalid_records_are_visible() {
        let temp = TempDir::new();
        append_sidecar_at(&temp.0, "fixture", "question", "answer").unwrap();
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(temp.0.join("fixture.jsonl"))
            .unwrap();
        file.write_all(b"not-json\n{\"partial\":").unwrap();
        let page = recent_entries_page_at(&temp.0, Some("fixture"), 30).unwrap();
        assert!(page.partial);
        assert_eq!(page.entries.len(), 1);
        assert!(!page.read_errors.is_empty());
    }

    #[test]
    fn session_key_format() {
        assert_eq!(SessionKey::dm("u1").as_str(), "wecom:dm:u1");
        assert_eq!(SessionKey::group("g1").as_str(), "wecom:group:g1");
        assert_eq!(SessionKey::dm_for("b", "u").kind, "dm");
        assert_eq!(SessionKey::group_user("b", "g", "u").kind, "group");
        assert_eq!(SessionKey::shared_task("b", "g", "t").kind, "group");
    }

    #[test]
    fn scoped_keys_and_disk_paths_do_not_collide() {
        let keys = vec![
            SessionKey::dm_for("a:b", "c"),
            SessionKey::dm_for("a", "b:c"),
            SessionKey::dm_for("a_b", "c"),
            SessionKey::dm_for("a/b", "c"),
            SessionKey::dm_for("a", "b"),
            SessionKey::dm_for("b", "b"),
            SessionKey::group_user("a", "b", "c"),
            SessionKey::group_user("a", "b", "d"),
            SessionKey::group_user("a", "b:c", "d"),
            SessionKey::group_user("a", "b", "c:d"),
            SessionKey::shared_task("a", "b", "c"),
            SessionKey::shared_task("a", "other", "c"),
            SessionKey::shared_task("other", "b", "c"),
            SessionKey::shared_task("a", "b", "d"),
        ];
        let logical: std::collections::HashSet<_> = keys.iter().map(SessionKey::as_str).collect();
        let paths: std::collections::HashSet<_> = logical.iter().map(|k| sanitize_key(k)).collect();
        assert_eq!(logical.len(), keys.len());
        assert_eq!(paths.len(), keys.len());
    }

    #[test]
    fn disk_hydration_populates_memory_before_append() {
        let temp = TempDir::new();
        let key = SessionKey::dm_for("b", "u").as_str();
        append_sidecar_at(&temp.0, &key, "old-question", "old-answer").unwrap();
        let mut s = SessionStore::default();
        assert_eq!(s.history(&key, Some(&temp.0)).len(), 2);
        // Remove the disk source to prove the second read is hydrated memory, not reread.
        std::fs::remove_file(temp.0.join(format!("{}.jsonl", sanitize_key(&key)))).unwrap();
        s.append(&key, "new-question", "new-answer");
        let history = s.history(&key, Some(&temp.0));
        assert_eq!(history.len(), 4);
        assert_eq!(history[0].content, "old-question");
        assert_eq!(history[3].content, "new-answer");
    }

    #[test]
    fn clear_survives_restart_and_only_affects_exact_scope() {
        let temp = TempDir::new();
        let key = SessionKey::group_user("b", "g", "u").as_str();
        let other = SessionKey::group_user("b", "g", "v").as_str();
        let legacy = SessionKey::group("g").as_str();
        for k in [&key, &other, &legacy] {
            append_sidecar_at(&temp.0, k, "old", "answer").unwrap();
        }
        let mut s = SessionStore::default();
        assert_eq!(s.history(&key, Some(&temp.0)).len(), 2);
        s.clear_at(&key, Some(&temp.0)).unwrap();
        assert!(s.history(&key, Some(&temp.0)).is_empty());
        let mut restarted = SessionStore::default();
        assert!(restarted.history(&key, Some(&temp.0)).is_empty());
        assert_eq!(restarted.history(&other, Some(&temp.0)).len(), 2);
        assert_eq!(restarted.history(&legacy, Some(&temp.0)).len(), 2);
        append_sidecar_at(&temp.0, &key, "after-clear", "answer").unwrap();
        let loaded = load_sidecar_at(Some(&temp.0), &key);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content, "after-clear");
    }

    #[test]
    fn new_dm_never_infers_legacy_bot_ownership() {
        let temp = TempDir::new();
        append_sidecar_at(&temp.0, &SessionKey::dm("u").as_str(), "legacy", "secret").unwrap();
        for bot in ["b1", "b2"] {
            assert!(
                load_sidecar_at(Some(&temp.0), &SessionKey::dm_for(bot, "u").as_str()).is_empty()
            );
        }
    }
}
