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

/// `~/.dsh/sessions`：每个工作区一个 `--<encoded cwd>--` 目录，下面是会话目录。
pub(crate) fn default_sessions_root() -> Option<PathBuf> {
    dsh_home().map(|home| home.join("sessions"))
}

/// 生产入口：按默认位置登记；找不到 DSH home 时返回 `MissingStore`。
pub(crate) fn register_session(workspace: &str, session_id: &str) -> RegisterOutcome {
    let Some(path) = default_store_path() else {
        return RegisterOutcome::MissingStore;
    };
    let sessions_root = default_sessions_root();
    register_session_in(&path, sessions_root.as_deref(), workspace, session_id)
}

/// 把 `session_id` 登记到 `store_path` 里路径等于 `workspace` 的工作区下。
///
/// `sessions_root` 用来把 ACP 返回的裸 id 校正成 DSH 实际存储/展示用的形态
/// （见 [`resolve_session_id`]）；传 `None` 时按原值登记。
pub(crate) fn register_session_in(
    store_path: &Path,
    sessions_root: Option<&Path>,
    workspace: &str,
    session_id: &str,
) -> RegisterOutcome {
    let workspace = normalize_path(workspace);
    let raw_id = session_id.trim().to_string();
    let session_id = resolve_session_id(sessions_root, &workspace, &raw_id);
    if workspace.is_empty() || session_id.is_empty() {
        return RegisterOutcome::WorkspaceNotFound;
    }
    let legacy = legacy_session_id(&raw_id, &session_id);
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
    match upsert_session_id(workspace_entry, &session_id, legacy.as_deref()) {
        Ok(true) => {}
        Ok(false) => return RegisterOutcome::AlreadyRegistered,
        Err(outcome) => return outcome,
    }

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
            match merge_into(&mut latest_store, &workspace, &session_id, legacy.as_deref()) {
                MergeResult::Already => return RegisterOutcome::AlreadyRegistered,
                MergeResult::NotFound => return RegisterOutcome::WorkspaceNotFound,
                MergeResult::Bad(detail) => return RegisterOutcome::BadJson(detail),
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
    Bad(String),
}

/// 在已解析的注册表上再做一次「查找 + 追加」（重读后复用）。
fn merge_into(
    store: &mut serde_json::Value,
    workspace: &str,
    session_id: &str,
    legacy: Option<&str>,
) -> MergeResult {
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
    match upsert_session_id(entry, session_id, legacy) {
        Ok(true) => MergeResult::Added,
        Ok(false) => MergeResult::Already,
        Err(RegisterOutcome::BadJson(detail)) => MergeResult::Bad(detail),
        Err(_) => MergeResult::NotFound,
    }
}

/// 写入一条会话 id：先删掉同一条会话的旧形态条目（历史上给裸 UUID 误加过
/// `session-` 前缀，那个 id 在 DSH 里对不上任何会话），再按需追加真实 id。
/// 返回数组是否真的变了（`false` 表示本来就已经登记好）。
fn upsert_session_id(
    entry: &mut serde_json::Value,
    session_id: &str,
    legacy: Option<&str>,
) -> Result<bool, RegisterOutcome> {
    let Some(ids) = entry
        .get_mut("sessionIds")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Err(RegisterOutcome::BadJson("工作区缺少 sessionIds 数组".into()));
    };
    let mut changed = false;
    if let Some(legacy) = legacy.filter(|legacy| *legacy != session_id) {
        let before = ids.len();
        ids.retain(|value| value.as_str() != Some(legacy));
        changed |= ids.len() != before;
    }
    if !ids.iter().any(|value| value.as_str() == Some(session_id)) {
        ids.push(serde_json::Value::String(session_id.to_string()));
        changed = true;
    }
    if changed {
        entry["updatedAt"] = serde_json::Value::String(now_rfc3339_millis());
    }
    Ok(changed)
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

/// 会话 id 的真实形态由**创建方**决定：ACP `session/new` 返回裸 UUID，DSH 自己
/// （dsh-im）建的会话才是 `session-<uuid>`。注册表里的条目必须与 DSH 磁盘目录名
/// 完全一致，否则 UI 的 `candidateMatches` 会把它过滤掉（旧实现盲目补前缀，写进去
/// 的 `session-<uuid>` 对不上任何会话，表现为「会话不展示」）。
///
/// 这里按磁盘实存判定：先看裸 id 的目录，再看带前缀的目录，都没有就按原值。
fn resolve_session_id(sessions_root: Option<&Path>, workspace: &str, raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || workspace.is_empty() {
        return trimmed.to_string();
    }
    if trimmed.starts_with(SESSION_ID_PREFIX) {
        return trimmed.to_string();
    }
    let Some(root) = sessions_root else {
        return trimmed.to_string();
    };
    let dir = root.join(session_store_dir_name(workspace));
    if dir.join(trimmed).is_dir() {
        return trimmed.to_string();
    }
    let prefixed = format!("{SESSION_ID_PREFIX}{trimmed}");
    if dir.join(&prefixed).is_dir() {
        return prefixed;
    }
    trimmed.to_string()
}

/// 历史条目里「同一条会话的另一种形态」：真实 id 是裸值时是 `session-<raw>`，
/// 真实 id 带前缀时是裸 `<raw>`。调用方给的就是带前缀的话没有歧义，返回 `None`。
fn legacy_session_id(raw: &str, resolved: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with(SESSION_ID_PREFIX) {
        return None;
    }
    if raw == resolved {
        Some(format!("{SESSION_ID_PREFIX}{raw}"))
    } else {
        Some(raw.to_string())
    }
}

/// DSH 的会话目录编码：`D:\d_project\tc\test` → `--D-d_project-tc-test--`
/// （盘符后的 `:` 去掉，`\`/`/` 变 `-`，整体前后各加 `--`）。
fn session_store_dir_name(workspace: &str) -> String {
    let mut trimmed = workspace.trim().replace('/', "\\");
    while trimmed.len() > 3 && trimmed.ends_with('\\') {
        trimmed.pop();
    }
    let mut name = String::from("--");
    for ch in trimmed.chars() {
        match ch {
            '\\' => name.push('-'),
            ':' => {}
            _ => name.push(ch),
        }
    }
    name.push_str("--");
    name
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
        let outcome = register_session_in(&store, None, "d:/D_PROJECT/tc/test/", "abc-123");
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
        assert_eq!(ids, ["session-existing", "abc-123"]);
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
            register_session_in(&store, None, "D:\\d_project\\tc\\test", "session-existing"),
            RegisterOutcome::AlreadyRegistered
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_workspace_and_missing_store_are_reported() {
        let (store, dir) = temp_store("unknown");
        std::fs::write(&store, serde_json::to_vec_pretty(&fixture_store()).unwrap()).unwrap();
        assert_eq!(
            register_session_in(&store, None, "D:\\other", "abc"),
            RegisterOutcome::WorkspaceNotFound
        );
        let _ = std::fs::remove_dir_all(dir);
        let (missing, dir) = temp_store("missing");
        assert_eq!(
            register_session_in(&missing, None, "D:\\d_project\\tc\\test", "abc"),
            RegisterOutcome::MissingStore
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_json_is_never_overwritten() {
        let (store, dir) = temp_store("badjson");
        std::fs::write(&store, b"{not json").unwrap();
        let outcome = register_session_in(&store, None, "D:\\d_project\\tc\\test", "abc");
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
            register_session_in(&store, None, "D:\\d_project\\tc\\test", "abc"),
            RegisterOutcome::Locked
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn path_normalization_is_case_and_separator_insensitive() {
        assert_eq!(normalize_path("D:/A/"), normalize_path("d:\\a\\"));
        assert_eq!(normalize_path(r"\\?\D:\A"), normalize_path("D:\\A"));
    }

    /// DSH 的目录编码必须与实盘一致：`D:\d_project\tc\test` → `--D-d_project-tc-test--`。
    #[test]
    fn session_store_dir_name_matches_dsh_layout() {
        assert_eq!(
            session_store_dir_name(r"D:\d_project\tc\test"),
            "--D-d_project-tc-test--"
        );
        assert_eq!(
            session_store_dir_name("D:/ai_project/work-skills/"),
            "--D-ai_project-work-skills--"
        );
    }

    /// ACP 给裸 UUID、DSH 自建会话带 `session-` 前缀：按磁盘实存选形态，不猜。
    #[test]
    fn resolves_session_id_from_disk_shape() {
        let (store, dir) = temp_store("resolve");
        let sessions = dir.join("sessions");
        let ws_dir = sessions.join("--D-d_project-tc-test--");
        std::fs::create_dir_all(ws_dir.join("bare-uuid")).unwrap();
        std::fs::create_dir_all(ws_dir.join("session-prefixed-uuid")).unwrap();
        std::fs::write(&store, serde_json::to_vec_pretty(&fixture_store()).unwrap()).unwrap();

        assert_eq!(
            resolve_session_id(Some(&sessions), r"D:\d_project\tc\test", "bare-uuid"),
            "bare-uuid"
        );
        assert_eq!(
            resolve_session_id(
                Some(&sessions),
                r"D:\d_project\tc\test",
                "prefixed-uuid"
            ),
            "session-prefixed-uuid"
        );
        // 都没落盘（或没有 sessions 根）：按 ACP 原值，绝不自作主张补前缀。
        assert_eq!(
            resolve_session_id(Some(&sessions), r"D:\d_project\tc\test", "unknown-uuid"),
            "unknown-uuid"
        );
        assert_eq!(
            resolve_session_id(None, r"D:\d_project\tc\test", "unknown-uuid"),
            "unknown-uuid"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 历史脏条目（裸 UUID 被误写成 `session-<uuid>`）要就地纠正，不能留幽灵会话。
    #[test]
    fn registration_heals_legacy_prefixed_entry() {
        let (store, dir) = temp_store("heal");
        let mut fixture = fixture_store();
        fixture["tables"]["workspaces"]["ws-test"]["sessionIds"] =
            serde_json::json!(["session-existing", "session-abc-123"]);
        std::fs::write(&store, serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();

        let outcome = register_session_in(&store, None, r"D:\d_project\tc\test", "abc-123");
        assert_eq!(outcome, RegisterOutcome::Registered);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
        let ids: Vec<&str> = saved["tables"]["workspaces"]["ws-test"]["sessionIds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert_eq!(ids, ["session-existing", "abc-123"]);

        // 再登记一次是幂等的，不会重复追加。
        assert_eq!(
            register_session_in(&store, None, r"D:\d_project\tc\test", "abc-123"),
            RegisterOutcome::AlreadyRegistered
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
