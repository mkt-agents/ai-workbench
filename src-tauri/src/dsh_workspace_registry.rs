//! DSH 工作区注册表（`~/.dsh/storages/workspace.json`）的 best-effort 写入。
//!
//! DSH UI 的「按工作区」分组只认注册表里每个工作区的 `sessionIds` 白名单；
//! ACP `session/new` 只传 `cwd`，不会登记 sessionId，所以企微通道创建的会话
//! 一律落在「未分组」。通道新建会话后在这里补一次登记。
//!
//! 约束（全部按 best-effort 处理，失败只让调用方写日志）：
//! - 只改 `tables.workspaces[<id>].sessionIds` 与 `updatedAt`，其余字段原样保留；
//! - 读-改-写 + 同目录锁 + 临时文件原子替换，避免半截文件；
//! - DSH 私有存储：UI 运行中可能用内存态覆盖本文件，需刷新页面才能看到归组；
//!   DSH 升级也可能变更存储格式——任何异常都只降级为日志，不阻断会话。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 注册表里的 sessionIds 形如 `session-<uuid>`；ACP 返回的是裸 uuid。
const SESSION_ID_PREFIX: &str = "session-";
const LOCK_FILE: &str = ".workspace.json.lock";
const LOCK_WAIT: Duration = Duration::from_millis(500);
/// 超过这个年龄的锁视为崩溃残留，直接回收。
const STALE_LOCK_AGE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegisterOutcome {
    /// 本次追加成功。
    Registered,
    /// 白名单里已有该会话（重复调用是幂等的）。
    AlreadyRegistered,
    /// 注册表里没有与目标路径匹配的工作区。
    WorkspaceNotFound,
    /// 用户没装 / 没用过 DSH UI，注册表不存在。
    MissingStore,
    /// 注册表不是预期的 JSON（版本变化或损坏）：不覆写。
    BadJson(String),
    /// 另一个写入者持有锁（DSH 或本进程的其它实例）。
    Locked,
    /// 读/写/替换失败。
    WriteFailed(String),
}

impl RegisterOutcome {
    /// 写进 `wecom.log` 的稳定 token（不含路径与会话内容）。
    pub(crate) fn token(&self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::AlreadyRegistered => "already_registered",
            Self::WorkspaceNotFound => "workspace_not_found",
            Self::MissingStore => "missing_store",
            Self::BadJson(_) => "bad_json",
            Self::Locked => "locked",
            Self::WriteFailed(_) => "write_failed",
        }
    }

    pub(crate) fn detail(&self) -> Option<&str> {
        match self {
            Self::BadJson(detail) | Self::WriteFailed(detail) => Some(detail),
            _ => None,
        }
    }
}

/// DSH home：`DSH_HOME` 优先，否则 `%USERPROFILE%\.dsh`（与 DSH 自身一致）。
pub(crate) fn dsh_home() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("DSH_HOME") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".dsh"))
}

/// `~/.dsh/storages/workspace.json`
pub(crate) fn default_store_path() -> Option<PathBuf> {
    dsh_home().map(|home| home.join("storages").join("workspace.json"))
}

/// 生产入口：按默认位置登记；找不到 DSH home 时返回 `MissingStore`。
pub(crate) fn register_session(workspace: &str, session_id: &str) -> RegisterOutcome {
    match default_store_path() {
        Some(path) => register_session_in(&path, workspace, session_id),
        None => RegisterOutcome::MissingStore,
    }
}

/// 把 `session_id` 登记到 `store_path` 里路径等于 `workspace` 的工作区下。
pub(crate) fn register_session_in(
    store_path: &Path,
    workspace: &str,
    session_id: &str,
) -> RegisterOutcome {
    let workspace = normalize_path(workspace);
    let session_id = normalize_session_id(session_id);
    if workspace.is_empty() || session_id.is_empty() {
        return RegisterOutcome::WorkspaceNotFound;
    }
    let raw = match std::fs::read(store_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RegisterOutcome::MissingStore;
        }
        Err(error) => return RegisterOutcome::WriteFailed(format!("读注册表失败：{error}")),
    };
    let mut store: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(error) => return RegisterOutcome::BadJson(format!("注册表不是合法 JSON：{error}")),
    };
    let table = match store.pointer_mut("/tables/workspaces") {
        Some(serde_json::Value::Object(table)) => table,
        _ => return RegisterOutcome::BadJson("缺少 tables.workspaces".into()),
    };
    let Some((_, workspace_entry)) = table
        .iter_mut()
        .find(|(_, entry)| {
            entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(normalize_path)
                .is_some_and(|path| path == workspace)
        })
    else {
        return RegisterOutcome::WorkspaceNotFound;
    };
    let Some(session_ids) = workspace_entry
        .get_mut("sessionIds")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return RegisterOutcome::BadJson("工作区缺少 sessionIds 数组".into());
    };
    if session_ids
        .iter()
        .filter_map(serde_json::Value::as_str)
        .any(|id| id == session_id)
    {
        return RegisterOutcome::AlreadyRegistered;
    }
    session_ids.push(serde_json::Value::String(session_id.clone()));
    workspace_entry["updatedAt"] = serde_json::Value::String(now_rfc3339_millis());

    let body = match serde_json::to_vec_pretty(&store) {
        Ok(body) => body,
        Err(error) => return RegisterOutcome::WriteFailed(format!("序列化注册表失败：{error}")),
    };
    let Some(parent) = store_path.parent() else {
        return RegisterOutcome::WriteFailed("注册表路径没有父目录".into());
    };
    // 同目录锁：串行化本进程 / 其它进程的写入，最多等 500ms。
    let lock_path = parent.join(LOCK_FILE);
    let _lock = match StoreLock::acquire(&lock_path, LOCK_WAIT) {
        Ok(lock) => lock,
        Err(error) => return error,
    };
    // 拿到锁后必须重读：别的写入者可能刚追加过同一条，也可能动了别的字段。
    match std::fs::read(store_path) {
        Ok(latest) if latest != raw => {
            let mut latest_store: serde_json::Value = match serde_json::from_slice(&latest) {
                Ok(value) => value,
                Err(error) => {
                    return RegisterOutcome::BadJson(format!("注册表不是合法 JSON：{error}"))
                }
            };
            match merge_into(&mut latest_store, &workspace, &session_id) {
                MergeResult::Already => return RegisterOutcome::AlreadyRegistered,
                MergeResult::NotFound => return RegisterOutcome::WorkspaceNotFound,
                MergeResult::Added => match serde_json::to_vec_pretty(&latest_store) {
                    Ok(latest_body) => return replace_atomically(store_path, parent, &latest_body),
                    Err(error) => {
                        return RegisterOutcome::WriteFailed(format!("序列化注册表失败：{error}"))
                    }
                },
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RegisterOutcome::MissingStore;
        }
        _ => {}
    }
    replace_atomically(store_path, parent, &body)
}

fn replace_atomically(
    store_path: &Path,
    parent: &Path,
    body: &[u8],
) -> RegisterOutcome {
    let temp = parent.join(format!(
        ".workspace.json.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    if let Err(error) = std::fs::write(&temp, body) {
        let _ = std::fs::remove_file(&temp);
        return RegisterOutcome::WriteFailed(format!("写临时注册表失败：{error}"));
    }
    if let Err(error) = std::fs::rename(&temp, store_path) {
        let _ = std::fs::remove_file(&temp);
        return RegisterOutcome::WriteFailed(format!("替换注册表失败：{error}"));
    }
    RegisterOutcome::Registered
}

enum MergeResult {
    Added,
    Already,
    NotFound,
}

/// 在已解析的注册表上再做一次「查找 + 追加」（重读后复用）。
fn merge_into(store: &mut serde_json::Value, workspace: &str, session_id: &str) -> MergeResult {
    let Some(serde_json::Value::Object(table)) = store.pointer_mut("/tables/workspaces") else {
        return MergeResult::NotFound;
    };
    let Some((_, entry)) = table.iter_mut().find(|(_, entry)| {
        entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(normalize_path)
            .is_some_and(|path| path == workspace)
    }) else {
        return MergeResult::NotFound;
    };
    let Some(ids) = entry
        .get_mut("sessionIds")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return MergeResult::NotFound;
    };
    if ids
        .iter()
        .filter_map(serde_json::Value::as_str)
        .any(|id| id == session_id)
    {
        return MergeResult::Already;
    }
    ids.push(serde_json::Value::String(session_id.to_string()));
    entry["updatedAt"] = serde_json::Value::String(now_rfc3339_millis());
    MergeResult::Added
}

struct StoreLock {
    path: PathBuf,
}

impl StoreLock {
    fn acquire(path: &Path, wait: Duration) -> Result<Self, RegisterOutcome> {
        let deadline = Instant::now() + wait;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => return Ok(Self { path: path.to_path_buf() }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // 崩溃残留的陈旧锁：超过 10 秒视为失效，删掉后重试（deadline 兜底）。
                    let stale = std::fs::metadata(path)
                        .and_then(|meta| meta.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > STALE_LOCK_AGE);
                    if stale {
                        let _ = std::fs::remove_file(path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(RegisterOutcome::Locked);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    return Err(RegisterOutcome::WriteFailed(format!("创建锁失败：{error}")))
                }
            }
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 路径比较口径：统一分隔符、去尾分隔符、去 `\\?\` 长路径前缀、ASCII 大小写不敏感。
fn normalize_path(path: &str) -> String {
    let mut normalized = path.trim().replace('/', "\\");
    if let Some(stripped) = normalized.strip_prefix(r"\\?\") {
        normalized = stripped.to_string();
    }
    while normalized.ends_with('\\') && normalized.len() > 3 {
        normalized.pop();
    }
    normalized.to_ascii_lowercase()
}

fn normalize_session_id(session_id: &str) -> String {
    let trimmed = session_id.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with(SESSION_ID_PREFIX) {
        trimmed.to_string()
    } else {
        format!("{SESSION_ID_PREFIX}{trimmed}")
    }
}

fn now_rfc3339_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_store() -> serde_json::Value {
        json!({
            "unit": {"name": "workspace", "version": 2},
            "global": {
                "initialized": true,
                "workspaceIds": ["ws-test"],
                "archivedSessionIds": [],
                "pinnedSessionIds": []
            },
            "tables": {
                "workspaces": {
                    "ws-test": {
                        "path": "D:\\d_project\\tc\\test",
                        "title": "test",
                        "sessionIds": ["session-existing"],
                        "createdAt": "2026-11-12T01:09:47.255Z",
                        "updatedAt": "2026-11-12T01:09:47.339Z",
                        "customField": {"keep": true}
                    }
                }
            }
        })
    }

    fn temp_store(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dsh-workspace-registry-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("workspace.json"), dir)
    }

    #[test]
    fn registers_and_preserves_unknown_fields() {
        let (store, dir) = temp_store("registers");
        std::fs::write(&store, serde_json::to_vec_pretty(&fixture_store()).unwrap()).unwrap();
        let outcome = register_session_in(&store, "d:/D_PROJECT/tc/test/", "abc-123");
        assert_eq!(outcome, RegisterOutcome::Registered);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
        assert_eq!(saved["unit"]["version"], 2);
        assert_eq!(saved["global"]["workspaceIds"][0], "ws-test");
        assert_eq!(saved["tables"]["workspaces"]["ws-test"]["customField"]["keep"], true);
        assert_eq!(saved["tables"]["workspaces"]["ws-test"]["title"], "test");
        let ids: Vec<&str> = saved["tables"]["workspaces"]["ws-test"]["sessionIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert_eq!(ids, ["session-existing", "session-abc-123"]);
        assert_ne!(
            saved["tables"]["workspaces"]["ws-test"]["updatedAt"],
            "2026-11-12T01:09:47.339Z"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_registration_is_idempotent() {
        let (store, dir) = temp_store("duplicate");
        std::fs::write(&store, serde_json::to_vec_pretty(&fixture_store()).unwrap()).unwrap();
        assert_eq!(
            register_session_in(&store, "D:\\d_project\\tc\\test", "session-existing"),
            RegisterOutcome::AlreadyRegistered
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_workspace_and_missing_store_are_reported() {
        let (store, dir) = temp_store("unknown");
        std::fs::write(&store, serde_json::to_vec_pretty(&fixture_store()).unwrap()).unwrap();
        assert_eq!(
            register_session_in(&store, "D:\\other", "abc"),
            RegisterOutcome::WorkspaceNotFound
        );
        let _ = std::fs::remove_dir_all(dir);
        let (missing, dir) = temp_store("missing");
        assert_eq!(
            register_session_in(&missing, "D:\\d_project\\tc\\test", "abc"),
            RegisterOutcome::MissingStore
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_json_is_never_overwritten() {
        let (store, dir) = temp_store("badjson");
        std::fs::write(&store, b"{not json").unwrap();
        let outcome = register_session_in(&store, "D:\\d_project\\tc\\test", "abc");
        assert!(matches!(outcome, RegisterOutcome::BadJson(_)));
        assert_eq!(std::fs::read(&store).unwrap(), b"{not json");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lock_contention_degrades_to_locked() {
        let (store, dir) = temp_store("locked");
        std::fs::write(&store, serde_json::to_vec_pretty(&fixture_store()).unwrap()).unwrap();
        std::fs::write(dir.join(LOCK_FILE), b"held").unwrap();
        assert_eq!(
            register_session_in(&store, "D:\\d_project\\tc\\test", "abc"),
            RegisterOutcome::Locked
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn path_normalization_is_case_and_separator_insensitive() {
        assert_eq!(normalize_path("D:/A/"), normalize_path("d:\\a\\"));
        assert_eq!(normalize_path(r"\\?\D:\A"), normalize_path("D:\\A"));
        assert_eq!(normalize_session_id("abc"), "session-abc");
        assert_eq!(normalize_session_id("session-abc"), "session-abc");
        assert_eq!(normalize_session_id("  "), "");
    }
}
