use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use base64::Engine;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use url::Url;
use crate::config::{
    CURSOR_SOFT_QUIT_TIMEOUT_MS, CURSOR_STOP_SETTLE_MS, CURSOR_STOP_WAIT_TIMEOUT_MS,
    DIR_REMOVE_RETRY_INTERVAL_MS, DIR_SIZE_SCAN_MAX_ENTRIES, DIR_SIZE_SCAN_TIMEOUT_MS,
    FILE_COPY_MAX_RETRIES, FILE_COPY_RETRY_INTERVAL_MS, MAX_LIVE_DB_COPY_BYTES,
};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// TTL for the "which data dir is Cursor running with" probe. Reading it needs
/// `Get-CimInstance Win32_Process` (WMI, commonly 200–500 ms) and the Cursor
/// page re-reads login status on every window focus, so a short cache removes
/// the repeat cost while staying responsive.
const RUNNING_DATA_DIR_TTL: Duration = Duration::from_millis(2500);

/// TTL for the "is Cursor.exe running" probe. Short enough to detect a stop
/// promptly, long enough to collapse the bursts of sequential checks that
/// `quit_cursor` / `wait_until_cursor_stopped` perform.
const CURSOR_RUNNING_TTL: Duration = Duration::from_millis(500);

struct TimedCache<T> {
    value: Mutex<Option<(Instant, T)>>,
}

impl<T: Clone> TimedCache<T> {
    const fn new() -> Self {
        Self {
            value: Mutex::new(None),
        }
    }

    fn get(&self, ttl: Duration) -> Option<T> {
        let guard = self.value.lock().ok()?;
        match guard.as_ref() {
            Some((at, value)) if at.elapsed() < ttl => Some(value.clone()),
            _ => None,
        }
    }

    fn set(&self, value: T) {
        if let Ok(mut guard) = self.value.lock() {
            *guard = Some((Instant::now(), value));
        }
    }

    fn invalidate(&self) {
        if let Ok(mut guard) = self.value.lock() {
            *guard = None;
        }
    }
}

static RUNNING_DATA_DIR_CACHE: TimedCache<Option<PathBuf>> = TimedCache::new();
static CURSOR_RUNNING_CACHE: TimedCache<bool> = TimedCache::new();

/// Invalidate both probe caches after a state change (quit / launch) so the
/// next read observes reality instead of a stale snapshot.
fn invalidate_cursor_probes() {
    RUNNING_DATA_DIR_CACHE.invalidate();
    CURSOR_RUNNING_CACHE.invalidate();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorLoginStatus {
    pub email: String,
    pub name: String,
    pub is_logged_in: bool,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct CursorAuthSnapshot {
    email: String,
    name: String,
    items: HashMap<String, String>,
}

fn hidden_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn cursor_data_dir() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    Path::new(&appdata).join("Cursor")
}

fn default_user_dir() -> PathBuf {
    cursor_data_dir().join("User")
}

const SHARED_WORKSPACE_DIR_NAMES: &[&str] = &["workspaceStorage", "History", "snippets"];

fn default_workspace_linked_marker() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join(".default-workspace-linked-v1"))
}

/// Error code prefix for structured errors returned to the frontend.
/// Format: "[CODE] message" — parsed by the frontend `parseInvokeError`.
const ERR_INVALID_ACCOUNT_ID: &str = "[INVALID_ACCOUNT_ID]";

/// Windows reserved device names that the filesystem maps to devices, not
/// files. Using one of these as an account id silently discards all data
/// written for that account (e.g. "NUL" → the null device).
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL",
    "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn is_windows_reserved_name(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or(name)
        .to_ascii_uppercase();
    WINDOWS_RESERVED_NAMES.iter().any(|r| *r == base)
}

fn validate_account_id(account_id: &str) -> Result<(), String> {
    if account_id.is_empty()
        || account_id.contains("..")
        || account_id.contains('/')
        || account_id.contains('\\')
        || account_id.contains(':')
    {
        return Err(format!("{ERR_INVALID_ACCOUNT_ID} 账号 ID 包含非法字符或为空"));
    }
    #[cfg(windows)]
    {
        if is_windows_reserved_name(account_id) {
            return Err(format!(
                "{ERR_INVALID_ACCOUNT_ID} 账号 ID \"{account_id}\" 是 Windows 保留设备名，请换一个名称"
            ));
        }
    }
    Ok(())
}

fn backup_dir(account_id: &str) -> Result<PathBuf, String> {
    validate_account_id(account_id)?;
    let appdata = std::env::var("APPDATA")
        .map_err(|_| "Failed to resolve APPDATA".to_string())?;
    let dir = Path::new(&appdata)
        .join("com.ai-workbench.app")
        .join("cursor-backups")
        .join(account_id);
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create backup dir: {}", e))?;
    Ok(dir)
}

fn cursor_profile_dir(account_id: &str) -> Result<PathBuf, String> {
    validate_account_id(account_id)?;
    let dir = app_data_dir()?.join("cursor-profiles").join(account_id);
    fs::create_dir_all(&dir).map_err(|e| format!("创建 profile 目录失败: {}", e))?;
    Ok(dir)
}

fn cursor_shared_dir() -> Result<PathBuf, String> {
    let dir = app_data_dir()?.join("cursor-shared");
    fs::create_dir_all(&dir).map_err(|e| format!("创建 shared 目录失败: {}", e))?;
    Ok(dir)
}

fn shared_seeded_marker() -> Result<PathBuf, String> {
    Ok(cursor_shared_dir()?.join(".shared-seeded"))
}

fn shared_user_dir() -> Result<PathBuf, String> {
    let dir = cursor_shared_dir()?.join("User");
    fs::create_dir_all(&dir).map_err(|e| format!("创建 shared/User 失败: {}", e))?;
    Ok(dir)
}

fn shared_global_storage_dir() -> Result<PathBuf, String> {
    let dir = shared_user_dir()?.join("globalStorage");
    fs::create_dir_all(&dir).map_err(|e| format!("创建 shared/globalStorage 失败: {}", e))?;
    Ok(dir)
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    fs::symlink_metadata(path)
        .map(|m| m.file_attributes() & 0x400 != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_reparse_point(path: &Path) -> bool {
    path.is_symlink()
}

fn merge_missing_subdirs(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.is_dir() || is_reparse_point(src) {
        return Ok(());
    }
    fs::create_dir_all(dst).map_err(|e| format!("创建目录失败: {}", e))?;
    for entry in fs::read_dir(src).map_err(|e| format!("读取目录失败: {}", e))? {
        let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
        let dst_child = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|e| format!("读取文件类型失败: {}", e))?;
        if file_type.is_dir() {
            if !dst_child.exists() {
                merge_tree_into(&entry.path(), &dst_child)?;
            }
        } else if !dst_child.exists() {
            copy_file_retry(&entry.path(), &dst_child)?;
        }
    }
    Ok(())
}

fn merge_tree_into(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.exists() {
        return Ok(());
    }
    if src.is_file() {
        if !dst.exists() {
            copy_file_retry(src, dst)?;
        }
        return Ok(());
    }
    fs::create_dir_all(dst).map_err(|e| format!("创建目录失败: {}", e))?;
    for entry in fs::read_dir(src).map_err(|e| format!("读取目录失败: {}", e))? {
        let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
        merge_tree_into(&entry.path(), &dst.join(entry.file_name()))?;
    }
    Ok(())
}

/// Unlink a junction / symlink itself. Recursing into it would reach the
/// default Cursor data it points at.
fn remove_link(path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|e| format!("移除链接 {} 失败: {}", path.display(), e))
}

fn remove_path_for_link(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    if is_reparse_point(path) {
        return remove_link(path);
    }
    if path.is_dir() {
        return remove_dir_retry(path, 5);
    }
    fs::remove_file(path).map_err(|e| format!("移除 {} 失败: {}", path.display(), e))
}

/// Delete a directory tree without ever following reparse points. Used when
/// removing a Cursor profile, whose subdirectories are junctions into the
/// default Cursor data directory.
fn remove_tree_removing_links(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    if is_reparse_point(path) {
        return remove_link(path);
    }
    if !path.is_dir() {
        return fs::remove_file(path).map_err(|e| format!("删除 {} 失败: {}", path.display(), e));
    }
    for entry in fs::read_dir(path).map_err(|e| format!("读取目录失败: {}", e))? {
        let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
        let child = entry.path();
        if is_reparse_point(&child) {
            remove_link(&child)?;
        } else if child.is_dir() {
            remove_tree_removing_links(&child)?;
        } else {
            let _ = fs::remove_file(&child);
        }
    }
    remove_dir_retry(path, 5)
}

#[cfg(windows)]
fn ensure_dir_junction(link: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|e| format!("创建目录失败: {}", e))?;
    if link.exists() {
        if is_reparse_point(link) {
            let same_target = match (fs::canonicalize(link), fs::canonicalize(target)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            if same_target {
                return Ok(());
            }
            // Stale junction pointing elsewhere — recreate
            remove_path_for_link(link)?;
        } else if link.is_dir() {
            merge_tree_into(link, target)?;
            remove_dir_retry(link, 5)?;
        } else {
            fs::remove_file(link).map_err(|e| format!("移除文件失败: {}", e))?;
        }
    }
    if link.exists() {
        return Ok(());
    }
    let link_s = link.to_string_lossy().to_string();
    let target_s = target.to_string_lossy().to_string();
    let output = hidden_command("cmd")
        .args(["/c", "mklink", "/J", &link_s, &target_s])
        .output()
        .map_err(|e| format!("创建目录联接失败: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "创建目录联接 {} -> {} 失败: {}{}",
            link.display(),
            target.display(),
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn ensure_dir_junction(link: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|e| format!("创建目录失败: {}", e))?;
    if link.exists() {
        if is_reparse_point(link) {
            let same_target = match (fs::canonicalize(link), fs::canonicalize(target)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            if same_target {
                return Ok(());
            }
            remove_path_for_link(link)?;
        } else {
            merge_tree_into(link, target)?;
            remove_path_for_link(link)?;
        }
    }
    if link.exists() {
        return Ok(());
    }
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| format!("创建符号链接失败: {}", e))
}

#[cfg(windows)]
fn ensure_file_link(link: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    if !target.exists() && link.exists() && link.is_file() && !is_reparse_point(link) {
        copy_file_retry(link, target)?;
    }
    if link.exists() {
        if is_reparse_point(link) || (link.is_file() && fs::canonicalize(link).ok() == fs::canonicalize(target).ok()) {
            return Ok(());
        }
        remove_path_for_link(link)?;
    }
    if !target.exists() {
        return Ok(());
    }
    let link_s = link.to_string_lossy().to_string();
    let target_s = target.to_string_lossy().to_string();
    let output = hidden_command("cmd")
        .args(["/c", "mklink", &link_s, &target_s])
        .output()
        .map_err(|e| format!("创建文件联接失败: {}", e))?;
    if !output.status.success() {
        copy_file_retry(target, link)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn ensure_file_link(link: &Path, target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if !target.exists() && link.exists() && link.is_file() {
        copy_file_retry(link, target)?;
    }
    if link.exists() && !is_reparse_point(link) {
        remove_path_for_link(link)?;
    }
    if target.exists() && !link.exists() {
        std::os::unix::fs::symlink(target, link)
            .map_err(|e| format!("创建符号链接失败: {}", e))?;
    }
    Ok(())
}

fn seed_shared_from_default() -> Result<(), String> {
    let marker = shared_seeded_marker()?;
    if marker.exists() {
        let _ = cleanup_shared_bloated_state_dbs();
        return Ok(());
    }

    let shared = cursor_shared_dir()?;
    let shared_user = shared_user_dir()?;
    let _ = shared_global_storage_dir()?;
    let default_root = cursor_data_dir();
    let default_user = default_root.join("User");

    // Workspace dirs stay in default Cursor\User and are junction-linked from profiles.
    if default_user.exists() {
        for name in ["settings.json", "keybindings.json"] {
            let src = default_user.join(name);
            let dst = shared_user.join(name);
            if src.is_file() && !dst.exists() {
                copy_file_retry(&src, &dst)?;
            }
        }
        let src_storage = default_user.join("globalStorage").join("storage.json");
        let dst_storage = shared_user.join("globalStorage").join("storage.json");
        if src_storage.is_file() && !dst_storage.exists() {
            copy_file_retry(&src_storage, &dst_storage)?;
        }
    }

    let default_ext = default_root.join("extensions");
    let shared_ext = shared.join("extensions");
    if default_ext.is_dir() {
        merge_tree_into(&default_ext, &shared_ext)?;
    } else {
        fs::create_dir_all(&shared_ext).map_err(|e| format!("创建 extensions 失败: {}", e))?;
    }

    let _ = cleanup_shared_bloated_state_dbs();

    fs::write(&marker, "1").map_err(|e| format!("写入 shared 标记失败: {}", e))?;
    Ok(())
}

/// Fast path: merge any unique shared-layer folders into default Cursor so profiles can
/// junction to a single copy. Does NOT delete multi-GB shared copies (that freezes the UI).
fn reconcile_legacy_shared_workspace_copies() -> Result<(), String> {
    let marker = default_workspace_linked_marker()?;
    if marker.exists() {
        schedule_delete_legacy_shared_workspace_copies();
        return Ok(());
    }

    let default_user = default_user_dir();
    fs::create_dir_all(&default_user).map_err(|e| format!("创建 default User 失败: {}", e))?;

    if let Ok(shared_user) = shared_user_dir() {
        for name in SHARED_WORKSPACE_DIR_NAMES {
            let shared_dir = shared_user.join(name);
            let default_dir = default_user.join(name);
            if shared_dir.is_dir() && !is_reparse_point(&shared_dir) {
                merge_missing_subdirs(&shared_dir, &default_dir)?;
            }
        }
        for name in ["settings.json", "keybindings.json"] {
            let shared_file = shared_user.join(name);
            let default_file = default_user.join(name);
            if shared_file.is_file() && !default_file.exists() {
                copy_file_retry(&shared_file, &default_file)?;
            }
        }
        let shared_storage = shared_user.join("globalStorage").join("storage.json");
        let default_storage = default_user.join("globalStorage").join("storage.json");
        if shared_storage.is_file() && !default_storage.exists() {
            if let Some(parent) = default_storage.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("创建 globalStorage 失败: {}", e))?;
            }
            copy_file_retry(&shared_storage, &default_storage)?;
        }
    }

    for name in SHARED_WORKSPACE_DIR_NAMES {
        fs::create_dir_all(default_user.join(name))
            .map_err(|e| format!("创建 default {} 失败: {}", name, e))?;
    }

    fs::write(&marker, "1").map_err(|e| format!("写入 default-workspace 标记失败: {}", e))?;
    schedule_delete_legacy_shared_workspace_copies();
    Ok(())
}

fn schedule_delete_legacy_shared_workspace_copies() {
    std::thread::spawn(|| {
        let Ok(shared_user) = shared_user_dir() else {
            return;
        };
        for name in SHARED_WORKSPACE_DIR_NAMES {
            let shared_dir = shared_user.join(name);
            if shared_dir.is_dir() && !is_reparse_point(&shared_dir) {
                let _ = remove_dir_retry(&shared_dir, 12);
            }
        }
    });
}

fn shared_bloat_cleanup_marker() -> Result<PathBuf, String> {
    Ok(cursor_shared_dir()?.join(".shared-bloat-cleaned-v1"))
}

/// Remove redundant Cursor login DBs under cursor-shared (never profiles / default Cursor).
fn cleanup_shared_bloated_state_dbs() -> Result<CursorCleanupResult, String> {
    let marker = shared_bloat_cleanup_marker()?;
    if marker.exists() {
        return Ok(CursorCleanupResult {
            removed_files: 0,
            freed_bytes: 0,
            message: "共享层冗余登录库已清理过".into(),
        });
    }

    let shared_gs = shared_global_storage_dir()?;
    const NAMES: &[&str] = &[
        "state.vscdb",
        "state.vscdb-wal",
        "state.vscdb-shm",
        "state.vscdb.backup",
    ];

    let mut freed = 0u64;
    let mut removed = 0u32;
    for name in NAMES {
        let p = shared_gs.join(name);
        if !p.is_file() {
            continue;
        }
        let size = file_size(&p);
        match fs::remove_file(&p) {
            Ok(()) => {
                freed += size;
                removed += 1;
            }
            Err(e) => {
                return Err(format!("删除共享层冗余 {} 失败: {}", p.display(), e));
            }
        }
    }

    let _ = fs::write(&marker, "1");
    Ok(CursorCleanupResult {
        removed_files: removed,
        freed_bytes: freed,
        message: if removed == 0 {
            "共享层无冗余登录库".into()
        } else {
            format!(
                "已清理共享层冗余登录库 {} 个文件，释放约 {}",
                removed,
                format_bytes(freed)
            )
        },
    })
}

/// `globalStorage` entries that are never bulk-shared: the login database, the
/// recent-folder config (linked separately), and Cursor's own global-storage
/// backups (machine-local snapshots — heavy and meaningless to share).
const GLOBAL_STORAGE_EXCLUDED: &[&str] = &[
    "state.vscdb",
    "state.vscdb-wal",
    "state.vscdb-shm",
    "state.vscdb.backup",
    "state.vscdb.options.json",
    "storage.json",
    "backups",
];

/// Excluded entries an earlier build may have linked; unlinked on sight.
const GLOBAL_STORAGE_UNLINK_IF_LINKED: &[&str] = &["backups"];

fn is_profile_local_global_storage(name: &str) -> bool {
    GLOBAL_STORAGE_EXCLUDED
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

/// Share Cursor's machine-global data under `User/globalStorage` — agent
/// workspaces (`anysphere.cursor-agent-worker`), the conversation search index
/// (`conversation-search.db`), extension global storage, etc. — across account
/// profiles. The login database (`state.vscdb`) is deliberately excluded so
/// each account keeps independent sign-in state.
///
/// Best-effort per entry: a single locked/missing entry must never abort the
/// account launch.
fn link_global_storage_to_shared(profile: &Path) -> Result<(), String> {
    let default_gs = default_user_dir().join("globalStorage");
    fs::create_dir_all(&default_gs).map_err(|e| format!("创建默认 globalStorage 失败: {}", e))?;

    let profile_gs = profile.join("User").join("globalStorage");
    fs::create_dir_all(&profile_gs).map_err(|e| format!("创建 profile globalStorage 失败: {}", e))?;

    // Drop links a previous build created for entries we no longer share.
    for name in GLOBAL_STORAGE_UNLINK_IF_LINKED {
        let link = profile_gs.join(name);
        if link.exists() && is_reparse_point(&link) {
            if let Err(e) = remove_link(&link) {
                eprintln!("[cursor] unlink globalStorage '{name}': {e}");
            }
        }
    }

    // Union of both sides, so entries that only exist in the profile are merged
    // into the canonical default directory instead of being dropped.
    let mut names: Vec<String> = Vec::new();
    for dir in [&default_gs, &profile_gs] {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if is_profile_local_global_storage(&name) || names.iter().any(|n| n == &name) {
                continue;
            }
            names.push(name);
        }
    }

    for name in names {
        let link = profile_gs.join(&name);
        let target = default_gs.join(&name);
        let result = if target.is_dir() || link.is_dir() {
            ensure_dir_junction(&link, &target)
        } else if target.exists() || link.exists() {
            ensure_file_link(&link, &target)
        } else {
            Ok(())
        };
        if let Err(e) = result {
            eprintln!("[cursor] share globalStorage '{name}': {e}");
        }
    }
    Ok(())
}

fn link_profile_to_shared(profile: &Path) -> Result<(), String> {
    seed_shared_from_default()?;
    reconcile_legacy_shared_workspace_copies()?;

    let default_user = default_user_dir();
    fs::create_dir_all(&default_user).map_err(|e| format!("创建 default User 失败: {}", e))?;
    fs::create_dir_all(default_user.join("globalStorage"))
        .map_err(|e| format!("创建 default globalStorage 失败: {}", e))?;

    let shared = cursor_shared_dir()?;
    let default_root = cursor_data_dir();
    let default_ext = default_root.join("extensions");
    let extensions_target = if default_ext.is_dir() {
        default_ext
    } else {
        let shared_ext = shared.join("extensions");
        fs::create_dir_all(&shared_ext).map_err(|e| format!("创建 extensions 失败: {}", e))?;
        shared_ext
    };

    let profile_user = profile.join("User");
    fs::create_dir_all(&profile_user.join("globalStorage"))
        .map_err(|e| format!("创建 profile globalStorage 失败: {}", e))?;

    let dir_links = [
        (
            profile_user.join("workspaceStorage"),
            default_user.join("workspaceStorage"),
        ),
        (profile_user.join("History"), default_user.join("History")),
        (profile_user.join("snippets"), default_user.join("snippets")),
        (profile.join("extensions"), extensions_target),
    ];
    for (link, target) in dir_links {
        ensure_dir_junction(&link, &target)?;
    }

    let default_storage = default_user.join("globalStorage").join("storage.json");
    clear_path_readonly(&default_storage);

    let file_links = [
        (
            profile_user.join("settings.json"),
            default_user.join("settings.json"),
        ),
        (
            profile_user.join("keybindings.json"),
            default_user.join("keybindings.json"),
        ),
        (
            profile_user.join("globalStorage").join("storage.json"),
            default_storage,
        ),
    ];
    for (link, target) in file_links {
        if link.exists() || target.exists() {
            ensure_file_link(&link, &target)?;
        }
    }

    // Share machine-global Cursor data (agent workspaces, conversation search
    // index, extension storage) while keeping the login DB per-profile.
    link_global_storage_to_shared(profile)?;

    Ok(())
}

fn prepare_profile_shared(profile: &Path) -> Result<(), String> {
    seed_shared_from_default()?;
    link_profile_to_shared(profile)?;
    // Pull manual-launch global state into the shared layer *before* it flows into
    // the profile: the recent-workspace list lives in the global state DB, and not
    // merging it makes a workbench launch look like a different workspace set.
    if let Err(e) = seed_shared_state_from_default() {
        eprintln!("[cursor] shared state seed: {e}");
    }
    if let Err(e) = merge_recent_workspaces_from_default() {
        eprintln!("[cursor] recent workspace merge: {e}");
    }
    sync_shared_state_to_profile(profile)?;

    // Composer sessions live in their own tables (composerHeaders + cursorDiskKV),
    // not in ItemTable, so they need an explicit sync chain. Order matters:
    // default→shared must precede shared→profile, otherwise sessions created by a
    // manually launched Cursor never reach this profile; profile→shared in the
    // middle is a safety net for sessions whose backflow was missed on quit.
    if let Err(e) = sync_default_composer_to_shared() {
        eprintln!("[cursor] default composer sync: {e}");
    }
    if let Err(e) = sync_profile_composer_to_shared(profile) {
        eprintln!("[cursor] profile composer backflow: {e}");
    }
    if let Err(e) = sync_shared_composer_to_profile(profile) {
        eprintln!("[cursor] shared composer sync: {e}");
    }
    Ok(())
}

fn profiles_root_dir() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("cursor-profiles"))
}

pub fn ensure_shared_workspace_migration() -> Result<(), String> {
    seed_shared_from_default()?;
    reconcile_legacy_shared_workspace_copies()?;
    if let Err(e) = cleanup_shared_bloated_state_dbs() {
        eprintln!("[cursor] shared bloat cleanup: {e}");
    }
    // Heal BLOB-typed values left by older shared-state sync (black-screen fix).
    if let Ok(shared_db) = shared_state_vscdb() {
        if let Err(e) = repair_blob_typed_state(&shared_db) {
            eprintln!("[cursor] shared state type repair: {e}");
        }
    }
    // Heal existing installs whose shared layer predates the default-state seed,
    // then keep the recent-workspace list merged with the default Cursor.
    if let Err(e) = seed_shared_state_from_default() {
        eprintln!("[cursor] shared state seed: {e}");
    }
    if let Err(e) = merge_recent_workspaces_from_default() {
        eprintln!("[cursor] recent workspace merge: {e}");
    }

    let profiles_root = profiles_root_dir()?;
    if !profiles_root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&profiles_root).map_err(|e| format!("读取 profiles 失败: {}", e))? {
        let entry = entry.map_err(|e| format!("读取 profile 项失败: {}", e))?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let profile = entry.path();
        if let Err(e) = repair_blob_typed_state(&live_state_vscdb_in(&profile)) {
            eprintln!("[cursor] profile state type repair: {e}");
        }
        if is_profile_initialized_at(&profile) {
            let _ = link_profile_to_shared(&profile);
            let _ = sync_shared_state_to_profile(&profile);
        }
    }
    let orphaned = cleanup_orphan_profiles(&known_cursor_account_ids());
    if orphaned > 0 {
        eprintln!("[cursor] cleaned {orphaned} orphan profile(s)");
    }
    Ok(())
}

fn account_id_from_profile_path(profile: &Path) -> Option<String> {
    let profiles_root = profiles_root_dir().ok()?;
    let canonical_profile = fs::canonicalize(profile).ok()?;
    let canonical_root = fs::canonicalize(&profiles_root).ok()?;
    if !canonical_profile.starts_with(&canonical_root) {
        return None;
    }
    canonical_profile
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
}

fn parse_user_data_dir_from_cmdline(cmdline: &str) -> Option<PathBuf> {
    let marker = "--user-data-dir=";
    let idx = cmdline.find(marker)?;
    let rest = &cmdline[idx + marker.len()..];
    let raw = if rest.starts_with('"') {
        let end = rest[1..].find('"')? + 1;
        rest[1..end].to_string()
    } else {
        let end = rest.find(' ').unwrap_or(rest.len());
        rest[..end].to_string()
    };
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

fn detect_running_cursor_data_dir() -> Option<PathBuf> {
    if let Some(cached) = RUNNING_DATA_DIR_CACHE.get(RUNNING_DATA_DIR_TTL) {
        return cached;
    }
    let found = detect_running_cursor_data_dir_uncached();
    RUNNING_DATA_DIR_CACHE.set(found.clone());
    found
}

fn detect_running_cursor_data_dir_uncached() -> Option<PathBuf> {
    let output = hidden_command("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"name='Cursor.exe'\" | Select-Object -ExpandProperty CommandLine",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(dir) = parse_user_data_dir_from_cmdline(line) {
            if dir.exists() {
                return Some(dir);
            }
        }
    }
    None
}

fn detect_profile_from_running_cursor() -> Option<String> {
    let dir = detect_running_cursor_data_dir()?;
    account_id_from_profile_path(&dir)
}

fn profile_initialized_marker(profile: &Path) -> PathBuf {
    profile.join(".profile-initialized")
}

fn is_profile_initialized_at(profile: &Path) -> bool {
    profile_initialized_marker(profile).exists()
}

fn mark_profile_initialized_at(profile: &Path) -> Result<(), String> {
    fs::write(profile_initialized_marker(profile), "1")
        .map_err(|e| format!("标记 profile 失败: {}", e))
}

fn active_account_file() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("cursor-active-account.txt"))
}

fn set_active_account(account_id: Option<&str>) {
    if let Ok(path) = active_account_file() {
        match account_id {
            Some(id) => {
                let _ = fs::write(path, id);
            }
            None => {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn get_active_account_id() -> Option<String> {
    let path = active_account_file().ok()?;
    let content = fs::read_to_string(path).ok()?;
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn resolve_data_dir() -> PathBuf {
    // Prefer the Cursor process that is actually running. A stale
    // cursor-active-account.txt must not point us at an empty profile
    // while the user is logged into the default Cursor data dir.
    if let Some(dir) = detect_running_cursor_data_dir() {
        return dir;
    }
    if let Some(active) = get_active_account_id() {
        if let Ok(profile) = cursor_profile_dir(&active) {
            if live_state_vscdb_in(&profile).exists() {
                return profile;
            }
        }
    }
    cursor_data_dir()
}

fn live_state_vscdb_in(data_root: &Path) -> PathBuf {
    data_root
        .join("User")
        .join("globalStorage")
        .join("state.vscdb")
}

fn live_state_vscdb() -> PathBuf {
    live_state_vscdb_in(&resolve_data_dir())
}

fn jwt_exp_unix(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let padded = match payload.len() % 4 {
        0 => payload.to_string(),
        n => format!("{}{}", payload, "=".repeat(4 - n)),
    };
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(padded.as_bytes())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.as_bytes()))
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("exp").and_then(|v| v.as_i64())
}

fn token_expiry_warning(token: &str) -> Option<String> {
    let exp = jwt_exp_unix(token)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let left = exp - now;
    if left <= 0 {
        Some("token 已过期，请重新登录并捕获".to_string())
    } else if left <= 7 * 24 * 3600 {
        Some("token 即将过期（7 天内），建议重新捕获".to_string())
    } else {
        None
    }
}

fn copy_file_retry(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.exists() {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create dir: {}", e))?;
    }
    let mut last_err = String::new();
    for _ in 0..FILE_COPY_MAX_RETRIES {
        match fs::copy(src, dst) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = e.to_string();
                std::thread::sleep(Duration::from_millis(FILE_COPY_RETRY_INTERVAL_MS));
            }
        }
    }
    Err(format!("复制失败 {} -> {}: {}", src.display(), dst.display(), last_err))
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.exists() {
        return Ok(());
    }
    fs::create_dir_all(dst).map_err(|e| format!("Failed to create dir: {}", e))?;
    for entry in fs::read_dir(src).map_err(|e| format!("Failed to read dir: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let file_type = entry.file_type().map_err(|e| format!("Failed to get file type: {}", e))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            copy_file_retry(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn remove_dir_retry(path: &Path, attempts: u32) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let mut last_err = String::new();
    for _ in 0..attempts {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e.to_string();
                std::thread::sleep(Duration::from_millis(DIR_REMOVE_RETRY_INTERVAL_MS));
            }
        }
    }
    Err(format!("无法清除 {}: {}", path.display(), last_err))
}

fn clear_path_readonly(path: &Path) {
    if !path.exists() {
        return;
    }
    #[cfg(windows)]
    {
        let path_s = path.to_string_lossy().to_string();
        let _ = hidden_command("attrib").args(["-R", &path_s]).output();
    }
}

fn replace_dir(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.exists() {
        return Ok(());
    }
    if dst.exists() {
        if let Err(err) = remove_dir_retry(dst, 24) {
            let file_name = dst
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "dir".to_string());
            let parent = dst
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| dst.to_path_buf());
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let stale = parent.join(format!("{file_name}.stale-{stamp}"));
            fs::rename(dst, &stale).map_err(|rename_err| {
                format!(
                    "无法清除 {}: {err}；改名也失败: {rename_err}。请先完全退出 Cursor 后重试。",
                    dst.display()
                )
            })?;
        }
    }
    copy_dir_all(src, dst)
}

fn is_cursor_process_running() -> bool {
    if let Some(cached) = CURSOR_RUNNING_CACHE.get(CURSOR_RUNNING_TTL) {
        return cached;
    }
    let running = is_cursor_process_running_uncached();
    CURSOR_RUNNING_CACHE.set(running);
    running
}

fn is_cursor_process_running_uncached() -> bool {
    if let Ok(result) = hidden_command("tasklist")
        .args(["/FI", "IMAGENAME eq Cursor.exe", "/FO", "CSV", "/NH"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&result.stdout);
        if stdout.to_lowercase().contains("cursor.exe") {
            return true;
        }
    }
    false
}

fn wait_until_cursor_stopped(timeout_ms: u64) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if !is_cursor_process_running() {
            std::thread::sleep(Duration::from_millis(CURSOR_STOP_SETTLE_MS));
            return Ok(());
        }
        if start.elapsed().as_millis() as u64 > timeout_ms {
            return Err("关闭 Cursor 超时，请手动关闭后重试".to_string());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn app_data_dir() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| "Failed to resolve APPDATA".to_string())?;
    let dir = Path::new(&appdata).join("com.ai-workbench.app");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create app data dir: {}", e))?;
    Ok(dir)
}

fn cursor_exe_cache_file() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("cursor-exe-path.txt"))
}

fn read_cached_cursor_exe() -> Option<PathBuf> {
    let cache = cursor_exe_cache_file().ok()?;
    let content = fs::read_to_string(&cache).ok()?;
    let path = PathBuf::from(content.trim());
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn write_cached_cursor_exe(path: &Path) {
    if let Ok(cache) = cursor_exe_cache_file() {
        let _ = fs::write(cache, path.to_string_lossy().as_bytes());
    }
}

fn cache_cursor_exe_from_running() {
    if let Some(path) = cursor_exe_from_running_process() {
        write_cached_cursor_exe(&path);
    }
}

fn cursor_exe_from_running_process() -> Option<PathBuf> {
    let output = hidden_command("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-Process -Name Cursor -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Path)",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let path = PathBuf::from(line.trim());
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn cursor_exe_from_registry() -> Option<PathBuf> {
    let script = r#"
$keys = @(
  'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
)
Get-ItemProperty $keys -ErrorAction SilentlyContinue |
  Where-Object { $_.DisplayName -like '*Cursor*' } |
  ForEach-Object {
    if ($_.InstallLocation) { Join-Path $_.InstallLocation 'Cursor.exe' }
    elseif ($_.DisplayIcon) { ($_.DisplayIcon -split ',')[0] }
  } |
  Select-Object -First 1
"#;
    let output = hidden_command("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let path = PathBuf::from(line.trim());
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

fn cursor_exe_default_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(&local).join("Programs").join("cursor").join("Cursor.exe"));
        candidates.push(PathBuf::from(&local).join("Programs").join("Cursor").join("Cursor.exe"));
    }
    if let Ok(pf) = std::env::var("ProgramFiles") {
        candidates.push(PathBuf::from(&pf).join("Cursor").join("Cursor.exe"));
    }
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        candidates.push(PathBuf::from(&pf86).join("Cursor").join("Cursor.exe"));
    }
    // Custom install paths seen in the wild
    for drive in ["C:", "D:", "E:"] {
        for sub in ["d_app\\cursor", "apps\\cursor", "Program Files\\Cursor", "cursor"] {
            candidates.push(PathBuf::from(format!("{}\\{}\\Cursor.exe", drive, sub)));
        }
    }
    if let Ok(custom) = std::env::var("CURSOR_PATH") {
        candidates.push(PathBuf::from(custom));
    }
    candidates
}

fn cursor_exe_path() -> Result<PathBuf, String> {
    if let Some(path) = read_cached_cursor_exe() {
        return Ok(path);
    }
    if let Some(path) = cursor_exe_from_running_process() {
        write_cached_cursor_exe(&path);
        return Ok(path);
    }
    for path in cursor_exe_default_candidates() {
        if path.exists() {
            write_cached_cursor_exe(&path);
            return Ok(path);
        }
    }
    if let Some(path) = cursor_exe_from_registry() {
        write_cached_cursor_exe(&path);
        return Ok(path);
    }
    if let Ok(output) = hidden_command("where").arg("Cursor.exe").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let path = PathBuf::from(line.trim());
                if path.exists() {
                    write_cached_cursor_exe(&path);
                    return Ok(path);
                }
            }
        }
    }

    Err("找不到 Cursor 安装路径。请先手动打开一次 Cursor，或在环境变量 CURSOR_PATH 中设置 Cursor.exe 路径。".to_string())
}

fn emit_switch_progress(window: Option<&tauri::Window>, stage: &str, message: &str) {
    if let Some(w) = window {
        let _ = w.emit(
            "cursor:switch_progress",
            serde_json::json!({ "stage": stage, "message": message }),
        );
    }
}

fn is_auth_key(key: &str) -> bool {
    key.starts_with("cursorAuth/")
        || key == "glass.lastSignedInAuthId"
        || key == "adminSettings.cachedAuthId"
}

/// Repair databases written by older builds: their shared-state sync coerced
/// every `ItemTable` value into a BLOB. Cursor/VS Code then reads those back as
/// a `Uint8Array`, and `JSON.parse()` stringifies the array (e.g. `[]` → "91,93"),
/// throwing `Unexpected non-whitespace character after JSON at position 2` and
/// aborting GlassWorkbench startup — the observed black screen.
///
/// Any BLOB whose bytes are valid, control-character-free UTF-8 was a TEXT value
/// before the coercion, so convert it back. Genuine binary is left untouched.
fn repair_blob_typed_state(db_path: &Path) -> Result<usize, String> {
    if !db_path.exists() {
        return Ok(0);
    }
    let conn = Connection::open(db_path).map_err(|e| format!("打开数据库失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA busy_timeout=8000; PRAGMA synchronous=NORMAL;");

    let mut to_fix: Vec<(String, String)> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT key, value FROM ItemTable WHERE typeof(value) = 'blob'")
            .map_err(|e| format!("查询 ItemTable 失败: {}", e))?;
        let rows = stmt
            .query_map([], |row| {
                let key: String = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                Ok((key, bytes))
            })
            .map_err(|e| format!("读取 ItemTable 失败: {}", e))?;
        for row in rows {
            let (key, bytes) = row.map_err(|e| format!("读取行失败: {}", e))?;
            if is_auth_key(&key) {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            if text
                .chars()
                .any(|c| c.is_control() && c != '\t' && c != '\n' && c != '\r')
            {
                continue;
            }
            to_fix.push((key, text));
        }
    }
    if to_fix.is_empty() {
        return Ok(0);
    }

    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启事务失败: {}", e))?;
    {
        let mut upd = tx
            .prepare("UPDATE ItemTable SET value = ?2 WHERE key = ?1")
            .map_err(|e| format!("准备更新失败: {}", e))?;
        for (key, text) in &to_fix {
            upd.execute(rusqlite::params![key, text])
                .map_err(|e| format!("修复 {} 失败: {}", key, e))?;
        }
    }
    tx.commit().map_err(|e| format!("提交修复失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(to_fix.len())
}

fn export_non_auth_keys(db_path: &Path) -> Result<HashMap<String, rusqlite::types::Value>, String> {
    if !db_path.exists() {
        return Ok(HashMap::new());
    }
    let conn = open_sqlite_ro(db_path).or_else(|_| {
        Connection::open(db_path).map_err(|e| format!("打开数据库失败: {}", e))
    })?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM ItemTable")
        .map_err(|e| format!("查询 ItemTable 失败: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            let key: String = row.get(0)?;
            // Preserve the native SQLite type — a TEXT value must stay TEXT.
            // Coercing it to a BLOB made Cursor read a Uint8Array and its
            // JSON.parse() fail, which aborts workbench startup (black screen).
            let value: rusqlite::types::Value = row.get(1)?;
            Ok((key, value))
        })
        .map_err(|e| format!("读取 ItemTable 失败: {}", e))?;
    let mut keys = HashMap::new();
    for row in rows {
        let (key, value) = row.map_err(|e| format!("读取行失败: {}", e))?;
        if !is_auth_key(&key) {
            keys.insert(key, value);
        }
    }
    Ok(keys)
}

fn import_non_auth_keys(
    db_path: &Path,
    keys: &HashMap<String, rusqlite::types::Value>,
) -> Result<(), String> {
    if keys.is_empty() {
        return Ok(());
    }
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    let conn = Connection::open(db_path).map_err(|e| format!("打开数据库失败: {}", e))?;
    let _ = conn.execute_batch(
        "PRAGMA busy_timeout=8000;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
    );
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启事务失败: {}", e))?;
    {
        // Same no-op guard as the cursorDiskKV importer: only dirty pages when
        // a value actually changed, so unchanged syncs leave the file mtime
        // alone and the composer sync markers stay effective.
        let mut stmt = tx
            .prepare(
                "INSERT INTO ItemTable (key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value
                 WHERE ItemTable.value IS NOT excluded.value",
            )
            .map_err(|e| format!("准备写入失败: {}", e))?;
        for (key, value) in keys {
            if is_auth_key(key) {
                continue;
            }
            stmt.execute(rusqlite::params![key, value])
                .map_err(|e| format!("写入 {} 失败: {}", key, e))?;
        }
    }
    tx.commit()
        .map_err(|e| format!("提交 state 同步失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
}

fn shared_state_vscdb() -> Result<PathBuf, String> {
    Ok(shared_global_storage_dir()?.join("state.vscdb"))
}

// ============================================================================
// Composer session history sharing (composerHeaders + cursorDiskKV tables).
//
// Prior sync logic only copied ItemTable. Cursor's actual Composer session
// list lives in two other tables inside the same state.vscdb:
//   - composerHeaders: one row per Composer session (sidebar list source)
//   - cursorDiskKV keys `composerData:<uuid>` and `composer.content.<hash>`:
//     full session content blobs
// Without syncing these, a profile launched via `--user-data-dir` starts with
// an empty sidebar and any new session created inside it never flows back to
// the default Cursor dir or other profiles. The three functions below wire
// the same default → shared → profile pipeline used for ItemTable.
// ============================================================================

/// One row of the `composerHeaders` table. Types mirror the live schema; the
/// value column is preserved as raw bytes so JSON content is not mutated.
struct ComposerHeaderRow {
    composer_id: String,
    workspace_id: String,
    created_at: rusqlite::types::Value,
    last_updated_at: rusqlite::types::Value,
    is_archived: rusqlite::types::Value,
    is_subagent: rusqlite::types::Value,
    recency: rusqlite::types::Value,
    checkpoint_at: rusqlite::types::Value,
    value: rusqlite::types::Value,
    subagent_type_name: rusqlite::types::Value,
}

fn export_composer_headers(src_db: &Path) -> Result<Vec<ComposerHeaderRow>, String> {
    if !src_db.exists() {
        return Ok(Vec::new());
    }
    let conn = open_sqlite_ro(src_db)
        .or_else(|_| Connection::open(src_db).map_err(|e| format!("打开数据库失败: {}", e)))?;
    // composerHeaders may not exist on a fresh profile — treat as empty.
    let table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='composerHeaders'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if table_exists == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, \
             isSubagent, recency, checkpointAt, value, subagentTypeName \
             FROM composerHeaders",
        )
        .map_err(|e| format!("准备查询 composerHeaders 失败: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ComposerHeaderRow {
                composer_id: row.get(0)?,
                workspace_id: row.get(1)?,
                created_at: row.get(2)?,
                last_updated_at: row.get(3)?,
                is_archived: row.get(4)?,
                is_subagent: row.get(5)?,
                recency: row.get(6)?,
                checkpoint_at: row.get(7)?,
                value: row.get(8)?,
                subagent_type_name: row.get(9)?,
            })
        })
        .map_err(|e| format!("查询 composerHeaders 失败: {}", e))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("读取 composerHeaders 行失败: {}", e))?);
    }
    Ok(out)
}

fn import_composer_headers(dst_db: &Path, rows: &[ComposerHeaderRow]) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    if let Some(parent) = dst_db.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    let conn = Connection::open(dst_db).map_err(|e| format!("打开数据库失败: {}", e))?;
    let _ = conn.execute_batch(
        "PRAGMA busy_timeout=8000;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS composerHeaders (\
             composerId TEXT PRIMARY KEY, \
             workspaceId TEXT, \
             createdAt INTEGER, \
             lastUpdatedAt INTEGER, \
             isArchived INTEGER, \
             isSubagent INTEGER, \
             recency INTEGER, \
             checkpointAt INTEGER, \
             value TEXT, \
             subagentTypeName TEXT\
         ) WITHOUT ROWID;",
    );
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启事务失败: {}", e))?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT OR REPLACE INTO composerHeaders \
                 (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent, \
                  recency, checkpointAt, value, subagentTypeName) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .map_err(|e| format!("准备写入 composerHeaders 失败: {}", e))?;
        for r in rows {
            stmt.execute(rusqlite::params![
                r.composer_id,
                r.workspace_id,
                &r.created_at,
                &r.last_updated_at,
                &r.is_archived,
                &r.is_subagent,
                &r.recency,
                &r.checkpoint_at,
                &r.value,
                &r.subagent_type_name,
            ])
            .map_err(|e| format!("写入 composerHeaders 失败 ({}): {}", r.composer_id, e))?;
        }
    }
    tx.commit()
        .map_err(|e| format!("提交 composerHeaders 同步失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
}

/// Prefixes inside cursorDiskKV that should be shared across profiles.
/// - `composerData:<uuid>`: full Composer session payload
/// - `composer.content.<hash>`: content fragments referenced by Composer
/// Other prefixes (agentKv, bubbleId, checkpointId, inlineDiff, ...) are
/// skipped: they either may carry account-bound state or are bulky temporaries
/// that don't affect the sidebar session list.
const SHARED_CURSOR_KV_PREFIXES: &[&str] = &["composerData:", "composer.content."];

fn export_cursor_disk_kv(src_db: &Path) -> Result<HashMap<String, rusqlite::types::Value>, String> {
    if !src_db.exists() {
        return Ok(HashMap::new());
    }
    let conn = open_sqlite_ro(src_db)
        .or_else(|_| Connection::open(src_db).map_err(|e| format!("打开数据库失败: {}", e)))?;
    let table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='cursorDiskKV'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if table_exists == 0 {
        return Ok(HashMap::new());
    }
    // Filter in SQL, not in Rust: cursorDiskKV also holds multi-GB derived
    // caches (agentKv, bubbleId, checkpointId) that must not be materialized
    // on every sync. The PK B-tree of this WITHOUT ROWID table serves each
    // GLOB as a range scan.
    let mut stmt = conn
        .prepare(
            "SELECT key, value FROM cursorDiskKV
             WHERE key GLOB 'composerData:*' OR key GLOB 'composer.content.*'",
        )
        .map_err(|e| format!("查询 cursorDiskKV 失败: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, rusqlite::types::Value>(1)?))
        })
        .map_err(|e| format!("读取 cursorDiskKV 失败: {}", e))?;
    let mut out = HashMap::new();
    for row in rows {
        let (key, value) = row.map_err(|e| format!("读取 cursorDiskKV 行失败: {}", e))?;
        if SHARED_CURSOR_KV_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
        {
            out.insert(key, value);
        }
    }
    Ok(out)
}

fn import_cursor_disk_kv(
    dst_db: &Path,
    map: &HashMap<String, rusqlite::types::Value>,
) -> Result<(), String> {
    if map.is_empty() {
        return Ok(());
    }
    if let Some(parent) = dst_db.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    let conn = Connection::open(dst_db).map_err(|e| format!("打开数据库失败: {}", e))?;
    let _ = conn.execute_batch(
        "PRAGMA busy_timeout=8000;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT PRIMARY KEY, value BLOB) WITHOUT ROWID;",
    );
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启事务失败: {}", e))?;
    {
        // Skip rows whose value is unchanged: a no-op write transaction
        // dirties no pages, keeping the source DB fingerprint (and thus the
        // sync markers below) stable across no-change launches.
        let mut stmt = tx
            .prepare(
                "INSERT INTO cursorDiskKV (key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value
                 WHERE cursorDiskKV.value IS NOT excluded.value",
            )
            .map_err(|e| format!("准备写入 cursorDiskKV 失败: {}", e))?;
        for (key, value) in map {
            stmt.execute(rusqlite::params![key, value])
                .map_err(|e| format!("写入 cursorDiskKV 失败 ({}): {}", key, e))?;
        }
    }
    tx.commit()
        .map_err(|e| format!("提交 cursorDiskKV 同步失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
}

/// Cheap change detection for the composer sync chain: (mtime|size) of the
/// main DB plus the WAL size. Sources that did not change since their last
/// successful sync are skipped entirely, which keeps repeat launches fast.
/// The WAL size (not mtime — too twitchy) covers crash-quit leftovers whose
/// changes never reached the main file.
fn db_fingerprint(db: &Path) -> Option<String> {
    let meta = fs::metadata(db).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut fp = format!("{}|{}", mtime, meta.len());
    let mut wal_name = db.file_name()?.to_os_string();
    wal_name.push("-wal");
    if let Ok(wal_meta) = fs::metadata(db.with_file_name(wal_name)) {
        fp.push_str(&format!("|{}", wal_meta.len()));
    }
    Some(fp)
}

fn composer_sync_mark_path(source_db: &Path) -> Option<PathBuf> {
    let shared_db = shared_state_vscdb().ok()?;
    let dir = shared_db.parent()?.join(".composer-sync");
    let key = source_db.to_string_lossy().to_string();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&key, &mut hasher);
    Some(dir.join(format!("{:016x}.mark", std::hash::Hasher::finish(&hasher))))
}

/// True when the source DB changed since its last successful composer sync
/// (or it was never synced). Falls back to "sync" on any doubt.
fn composer_sync_pending(mark: &Path, source_db: &Path) -> bool {
    match (fs::read_to_string(mark), db_fingerprint(source_db)) {
        (Ok(prev), Some(fp)) => prev != fp,
        _ => true,
    }
}

fn composer_sync_done(mark: &Path, source_db: &Path) {
    if let Some(fp) = db_fingerprint(source_db) {
        if let Some(parent) = mark.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(mark, fp);
    }
}

/// Pull Composer headers + cursorDiskKV entries from the default Cursor dir
/// into the shared layer. Idempotent — every profile launch re-runs this so
/// manually-launched sessions keep flowing into shared across launches.
fn sync_default_composer_to_shared() -> Result<(), String> {
    let default_db = default_user_dir().join("globalStorage").join("state.vscdb");
    let shared_db = shared_state_vscdb()?;
    if !default_db.exists() {
        return Ok(());
    }
    let mark = composer_sync_mark_path(&default_db);
    if let Some(mark) = &mark {
        if !composer_sync_pending(mark, &default_db) {
            return Ok(());
        }
    }
    if let Some(parent) = shared_db.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 shared 目录失败: {}", e))?;
    }
    let headers = export_composer_headers(&default_db)?;
    if !headers.is_empty() {
        import_composer_headers(&shared_db, &headers)?;
    }
    let kv = export_cursor_disk_kv(&default_db)?;
    if !kv.is_empty() {
        import_cursor_disk_kv(&shared_db, &kv)?;
    }
    if let Some(mark) = &mark {
        composer_sync_done(mark, &default_db);
    }
    Ok(())
}

/// Push profile's Composer state back into the shared layer. Captures sessions
/// created inside this profile so the next profile launch (or a manual launch)
/// can see them. Per-row INSERT OR REPLACE gives last-write-wins per
/// composerId/key — same-session updates from different accounts overlay, which
/// matches the user expectation that the most recent edit wins.
fn sync_profile_composer_to_shared(profile: &Path) -> Result<(), String> {
    let profile_db = live_state_vscdb_in(profile);
    if !profile_db.exists() {
        return Ok(());
    }
    let mark = composer_sync_mark_path(&profile_db);
    if let Some(mark) = &mark {
        if !composer_sync_pending(mark, &profile_db) {
            return Ok(());
        }
    }
    let shared_db = shared_state_vscdb()?;
    if let Some(parent) = shared_db.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 shared 目录失败: {}", e))?;
    }
    let headers = export_composer_headers(&profile_db)?;
    if !headers.is_empty() {
        import_composer_headers(&shared_db, &headers)?;
    }
    let kv = export_cursor_disk_kv(&profile_db)?;
    if !kv.is_empty() {
        import_cursor_disk_kv(&shared_db, &kv)?;
    }
    if let Some(mark) = &mark {
        composer_sync_done(mark, &profile_db);
    }
    Ok(())
}

/// Pull shared Composer state into a profile before launch. After this, the
/// profile's sidebar will show sessions from the default Cursor dir and from
/// every other profile that has synced into shared.
fn sync_shared_composer_to_profile(profile: &Path) -> Result<(), String> {
    let shared_db = shared_state_vscdb()?;
    if !shared_db.exists() {
        return Ok(());
    }
    let mark = composer_sync_mark_path(&shared_db);
    if let Some(mark) = &mark {
        if !composer_sync_pending(mark, &shared_db) {
            return Ok(());
        }
    }
    let profile_db = live_state_vscdb_in(profile);
    if let Some(parent) = profile_db.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 profile 目录失败: {}", e))?;
    }
    let headers = export_composer_headers(&shared_db)?;
    if !headers.is_empty() {
        import_composer_headers(&profile_db, &headers)?;
    }
    let kv = export_cursor_disk_kv(&shared_db)?;
    if !kv.is_empty() {
        import_cursor_disk_kv(&profile_db, &kv)?;
    }
    if let Some(mark) = &mark {
        composer_sync_done(mark, &shared_db);
    }
    Ok(())
}

fn sync_shared_state_to_profile(profile: &Path) -> Result<(), String> {
    let shared_db = shared_state_vscdb()?;
    if !shared_db.exists() {
        return Ok(());
    }
    let keys = export_non_auth_keys(&shared_db)?;
    if keys.is_empty() {
        return Ok(());
    }
    import_non_auth_keys(&live_state_vscdb_in(profile), &keys)
}

/// Cursor / VS Code keeps the "recently opened workspaces" list here. Unlike the
/// per-workspace `workspaceStorage` tree (junctioned, genuinely shared), this key
/// lives in the *global* state DB, which is per-profile — so it has to be merged
/// explicitly. Otherwise a workbench launch and a manual launch show different
/// workspace histories.
const RECENT_WORKSPACES_KEY: &str = "history.recentlyOpenedPathsList";

fn read_state_value(db_path: &Path, key: &str) -> Result<Option<rusqlite::types::Value>, String> {
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = open_sqlite_ro(db_path)
        .or_else(|_| Connection::open(db_path).map_err(|e| format!("打开数据库失败: {}", e)))?;
    let mut stmt = conn
        .prepare("SELECT value FROM ItemTable WHERE key = ?1")
        .map_err(|e| format!("查询 ItemTable 失败: {}", e))?;
    let mut rows = stmt
        .query([key])
        .map_err(|e| format!("读取 {} 失败: {}", key, e))?;
    match rows
        .next()
        .map_err(|e| format!("读取 {} 失败: {}", key, e))?
    {
        // Keep the native SQLite type: Cursor JSON.parses these values and a
        // coerced BLOB aborts its workbench startup (black screen).
        Some(row) => Ok(Some(
            row.get(0).map_err(|e| format!("读取 {} 失败: {}", key, e))?,
        )),
        None => Ok(None),
    }
}

fn write_state_value(
    db_path: &Path,
    key: &str,
    value: rusqlite::types::Value,
) -> Result<(), String> {
    let mut keys = HashMap::new();
    keys.insert(key.to_string(), value);
    import_non_auth_keys(db_path, &keys)
}

/// Identity of an entry inside `history.recentlyOpenedPathsList`: a folder or a
/// multi-root `.code-workspace` file.
fn recent_entry_uri(entry: &serde_json::Value) -> String {
    entry
        .get("folderUri")
        .and_then(|v| v.as_str())
        .or_else(|| {
            entry
                .get("workspace")
                .and_then(|w| w.get("configPath"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or_default()
        .to_string()
}

/// Union two recent-workspace lists, keeping `primary`'s order and appending
/// entries only present in `secondary`. Returns `None` when the value is not a
/// JSON object (leave the caller's data untouched rather than clobber it).
fn merge_recent_workspaces_json(
    primary: &rusqlite::types::Value,
    secondary: Option<&rusqlite::types::Value>,
) -> Option<rusqlite::types::Value> {
    let as_text = |value: &rusqlite::types::Value| match value {
        rusqlite::types::Value::Text(text) => Some(text.clone()),
        rusqlite::types::Value::Blob(bytes) => String::from_utf8(bytes.clone()).ok(),
        _ => None,
    };
    let mut head: serde_json::Value = serde_json::from_str(&as_text(primary)?).ok()?;
    if !head.is_object() {
        return None;
    }

    let mut entries = head
        .get("entries")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut seen: HashSet<String> = entries.iter().map(recent_entry_uri).collect();

    if let Some(secondary) = secondary.and_then(as_text) {
        if let Ok(tail) = serde_json::from_str::<serde_json::Value>(&secondary) {
            for entry in tail
                .get("entries")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                let uri = recent_entry_uri(entry);
                if uri.is_empty() || seen.insert(uri) {
                    entries.push(entry.clone());
                }
            }
        }
    }

    head["entries"] = serde_json::Value::Array(entries);
    Some(rusqlite::types::Value::Text(head.to_string()))
}

/// Merge the default (manually launched) Cursor's recent-workspace list into the
/// shared layer, so projects opened outside the workbench still show up in every
/// profile. The default DB is only ever read — never written — so a manually
/// launched Cursor cannot be disturbed.
fn merge_recent_workspaces_from_default() -> Result<(), String> {
    let default_db = default_user_dir().join("globalStorage").join("state.vscdb");
    let Some(primary) = read_state_value(&default_db, RECENT_WORKSPACES_KEY)? else {
        return Ok(());
    };
    let shared_db = shared_state_vscdb()?;
    let secondary = read_state_value(&shared_db, RECENT_WORKSPACES_KEY)?;
    if let Some(merged) = merge_recent_workspaces_json(&primary, secondary.as_ref()) {
        write_state_value(&shared_db, RECENT_WORKSPACES_KEY, merged)?;
    }
    Ok(())
}

fn shared_state_seed_marker() -> Result<PathBuf, String> {
    Ok(cursor_shared_dir()?.join(".shared-state-seeded-v1"))
}

/// One-shot: bring the default Cursor's non-auth global state (recent workspaces,
/// workbench UI state, extension global state) into the shared layer, so a fresh
/// profile does not start from an almost empty state DB. Login keys are filtered
/// out by `export_non_auth_keys`, so accounts stay isolated.
fn seed_shared_state_from_default() -> Result<(), String> {
    let marker = shared_state_seed_marker()?;
    if marker.exists() {
        return Ok(());
    }
    let default_db = default_user_dir().join("globalStorage").join("state.vscdb");
    let keys = export_non_auth_keys(&default_db)?;
    if !keys.is_empty() {
        import_non_auth_keys(&shared_state_vscdb()?, &keys)?;
    }
    fs::write(&marker, "1").map_err(|e| format!("写入 shared state 标记失败: {}", e))?;
    Ok(())
}

fn sync_profile_state_to_shared(profile: &Path) -> Result<(), String> {
    let profile_db = live_state_vscdb_in(profile);
    if !profile_db.exists() {
        return Ok(());
    }
    let shared_db = shared_state_vscdb()?;

    // ItemTable non-auth keys: recent workspaces, workbench UI state, ...
    let keys = export_non_auth_keys(&profile_db)?;
    if !keys.is_empty() {
        import_non_auth_keys(&shared_db, &keys)?;
    }

    // Composer sessions created/updated inside this profile flow back into the
    // shared layer, so the next account (or a manual launch) can see them.
    let headers = export_composer_headers(&profile_db)?;
    if !headers.is_empty() {
        import_composer_headers(&shared_db, &headers)?;
    }
    let kv = export_cursor_disk_kv(&profile_db)?;
    if !kv.is_empty() {
        import_cursor_disk_kv(&shared_db, &kv)?;
    }
    Ok(())
}

fn open_sqlite_ro(path: &Path) -> Result<Connection, String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let uri = if normalized.contains(':') {
        format!("file:///{normalized}?mode=ro")
    } else {
        format!("file:{normalized}?mode=ro")
    };
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| format!("打开数据库失败 {}: {}", path.display(), e))
}

fn session_dir_pairs_for(backup: &Path, data_root: &Path) -> Vec<(PathBuf, PathBuf)> {
    vec![
        (
            backup.join("Local Storage"),
            data_root.join("Local Storage"),
        ),
        (
            backup.join("Session Storage"),
            data_root.join("Session Storage"),
        ),
    ]
}

fn backup_cursor_data_from_root(account_id: &str, data_root: &Path) -> Result<String, String> {
    let backup = backup_dir(account_id)?;
    let live_db = live_state_vscdb_in(data_root);
    if !live_db.exists() {
        return Err("未找到 Cursor 登录数据库，请先在 Cursor 中登录该账号".to_string());
    }

    let snapshot = extract_auth_from_db(&live_db).or_else(|_| {
        let temp_db = copy_live_db_for_read_at(&live_db)?;
        let result = extract_auth_from_db(&temp_db);
        if let Some(parent) = temp_db.parent() {
            let _ = fs::remove_dir_all(parent);
        }
        result
    })?;

    if snapshot.email.is_empty() {
        return Err("当前 Cursor 没有可读邮箱，请确认已登录后再捕获".to_string());
    }
    if snapshot
        .items
        .get("cursorAuth/accessToken")
        .map(|t| t.is_empty())
        .unwrap_or(true)
    {
        return Err("当前 Cursor 没有 accessToken，请确认已登录后再捕获".to_string());
    }

    let auth_json = serde_json::to_string_pretty(&snapshot)
        .map_err(|e| format!("序列化登录数据失败: {}", e))?;
    fs::write(backup.join("auth.json"), auth_json)
        .map_err(|e| format!("写入 auth.json 失败: {}", e))?;

    copy_file_retry(
        &data_root.join("Local State"),
        &backup.join("Local State"),
    )
    .map_err(|e| format!("复制 Local State 失败: {}", e))?;
    let _ = copy_file_retry(
        &data_root.join("sentry").join("session.json"),
        &backup.join("sentry").join("session.json"),
    );

    copy_network_session(&data_root.join("Network"), &backup.join("Network"))
        .map_err(|e| format!("复制 Network/Cookies 失败: {}", e))?;

    let copied_cookies = backup.join("Network").join("Cookies").exists();
    for (backup_dst, _) in session_dir_pairs_for(&backup, data_root) {
        let live = data_root.join(
            backup_dst
                .file_name()
                .unwrap_or_default(),
        );
        if live.exists() {
            replace_dir(&live, &backup_dst).map_err(|e| {
                format!("复制会话目录 {} 失败: {}", live.display(), e)
            })?;
        }
    }
    if !copied_cookies {
        return Err("未能复制 Cookies，捕获不完整。".to_string());
    }

    Ok(backup.to_string_lossy().to_string())
}

/// Copy live state.vscdb (+ wal/shm) to a temp file so we can read while Cursor holds a lock.
/// Refuses huge DBs — a multi-GB copy freezes the whole app.

fn copy_live_db_for_read_at(live_db: &Path) -> Result<PathBuf, String> {
    if !live_db.exists() {
        return Err("未找到 Cursor 登录数据库".to_string());
    }
    let size = file_size(live_db);
    if size > MAX_LIVE_DB_COPY_BYTES {
        return Err(format!(
            "Cursor 数据库过大（{}），跳过复制读取以免卡死",
            format_bytes(size)
        ));
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_dir = std::env::temp_dir().join(format!("ai-workbench-cursor-db-{}", stamp));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).map_err(|e| format!("创建临时目录失败: {}", e))?;

    let temp_db = temp_dir.join("state.vscdb");
    copy_file_retry(live_db, &temp_db)?;
    let _ = copy_file_retry(
        &live_db.with_file_name("state.vscdb-wal"),
        &temp_dir.join("state.vscdb-wal"),
    );
    let _ = copy_file_retry(
        &live_db.with_file_name("state.vscdb-shm"),
        &temp_dir.join("state.vscdb-shm"),
    );
    Ok(temp_db)
}

fn copy_live_db_for_read() -> Result<PathBuf, String> {
    copy_live_db_for_read_at(&live_state_vscdb())
}

fn extract_auth_from_db(path: &Path) -> Result<CursorAuthSnapshot, String> {
    let conn = open_sqlite_ro(path)?;
    let mut stmt = conn
        .prepare(
            "SELECT key, value FROM ItemTable \
             WHERE key LIKE 'cursorAuth/%' \
                OR key IN ('glass.lastSignedInAuthId', 'adminSettings.cachedAuthId')",
        )
        .map_err(|e| format!("查询 ItemTable 失败: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let value = match row.get::<_, rusqlite::types::Value>(1)? {
                rusqlite::types::Value::Text(s) => s,
                rusqlite::types::Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
                rusqlite::types::Value::Integer(n) => n.to_string(),
                rusqlite::types::Value::Real(n) => n.to_string(),
                rusqlite::types::Value::Null => String::new(),
            };
            Ok((key, value))
        })
        .map_err(|e| format!("读取 ItemTable 失败: {}", e))?;

    let mut snapshot = CursorAuthSnapshot::default();
    for row in rows.flatten() {
        let (key, value) = row;
        if !is_auth_key(&key) {
            continue;
        }
        if key == "cursorAuth/cachedEmail" {
            snapshot.email = value.trim_matches('"').to_string();
        }
        if key == "cursorAuth/cachedScopedProfile" {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&value) {
                if let Some(name) = json["displayName"].as_str() {
                    snapshot.name = name.to_string();
                }
            }
        }
        snapshot.items.insert(key, value);
    }
    if snapshot.name.is_empty() {
        snapshot.name = snapshot.email.clone();
    }
    Ok(snapshot)
}

fn extract_auth_from_live() -> Result<CursorAuthSnapshot, String> {
    let live_db = live_state_vscdb();
    if !live_db.exists() {
        return Err("未找到 Cursor 登录数据库".to_string());
    }

    // Prefer direct RO open. Empty auth means logged out — that is a valid result.
    // Never fall through to a multi-GB copy just because email/token is empty.
    match extract_auth_from_db(&live_db) {
        Ok(snap) => return Ok(snap),
        Err(direct_err) => {
            // Fallback only when the live DB cannot be opened (lock / IO).
            match copy_live_db_for_read() {
                Ok(temp_db) => {
                    let result = extract_auth_from_db(&temp_db);
                    if let Some(parent) = temp_db.parent() {
                        let _ = fs::remove_dir_all(parent);
                    }
                    result.map_err(|e| format!("{direct_err}; 复制读取亦失败: {e}"))
                }
                Err(copy_err) => Err(format!("{direct_err}; {copy_err}")),
            }
        }
    }
}

fn emails_match(a: &str, b: &str) -> bool {
    let na = a.trim().to_lowercase();
    let nb = b.trim().to_lowercase();
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    na == nb
}

fn identity_match(account_email: &str, account_name: &str, live: &CursorAuthSnapshot) -> bool {
    if !live.email.is_empty() {
        if emails_match(account_email, &live.email) || emails_match(account_name, &live.email) {
            return true;
        }
    }
    if !live.name.is_empty() {
        if emails_match(account_email, &live.name) || emails_match(account_name, &live.name) {
            return true;
        }
    }
    if let Some(auth_id) = live.items.get("glass.lastSignedInAuthId") {
        if let Some(uid) = auth_id.split('|').last() {
            if !uid.is_empty()
                && (account_email == uid
                    || account_name == uid
                    || account_email.contains(uid)
                    || account_name.contains(uid))
            {
                return true;
            }
        }
    }
    false
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorBackupStatus {
    pub account_id: String,
    pub complete: bool,
    pub auth_email: String,
    pub has_auth_json: bool,
    pub has_cookies: bool,
    pub reason: String,
    /// Non-blocking advisory (e.g. token expiring soon); absent when healthy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

fn inspect_backup_dir(account_id: &str, backup: &Path) -> CursorBackupStatus {
    let auth_json = backup.join("auth.json");
    let cookies = backup.join("Network").join("Cookies");
    let has_auth_json = auth_json.exists();
    let has_cookies = cookies.exists() && cookies.metadata().map(|m| m.len() > 0).unwrap_or(false);

    let mut auth_email = String::new();
    let mut has_token = false;
    if has_auth_json {
        if let Ok(content) = fs::read_to_string(&auth_json) {
            if let Ok(snap) = serde_json::from_str::<CursorAuthSnapshot>(&content) {
                auth_email = snap.email.clone();
                has_token = snap
                    .items
                    .get("cursorAuth/accessToken")
                    .map(|t| !t.is_empty())
                    .unwrap_or(false);
            }
        }
    }

    let (complete, reason) = if !has_auth_json {
        (
            false,
            "缺少 auth.json（旧快照不可靠，请登录该账号后重新捕获）".to_string(),
        )
    } else if !has_token {
        (false, "缺少 accessToken，请重新捕获".to_string())
    } else if auth_email.is_empty() {
        (false, "快照没有邮箱，请重新捕获".to_string())
    } else if !has_cookies {
        (false, "缺少 Cookies，请重新完整捕获".to_string())
    } else {
        (true, "完整".to_string())
    };

    let mut final_reason = reason;
    let mut final_complete = complete;
    let mut warning: Option<String> = None;
    if complete {
        if let Ok(content) = fs::read_to_string(&auth_json) {
            if let Ok(snap) = serde_json::from_str::<CursorAuthSnapshot>(&content) {
                if let Some(token) = snap.items.get("cursorAuth/accessToken") {
                    if let Some(warn) = token_expiry_warning(token) {
                        // An already-expired token blocks switching (incomplete).
                        // "Expiring soon" stays complete and is reported as a
                        // non-blocking warning so the UI can surface it early.
                        if warn.contains("已过期") {
                            final_complete = false;
                            final_reason = warn;
                        } else {
                            warning = Some(warn);
                        }
                    }
                }
            }
        }
    }

    CursorBackupStatus {
        account_id: account_id.to_string(),
        complete: final_complete,
        auth_email,
        has_auth_json,
        has_cookies,
        reason: final_reason,
        warning,
    }
}

fn load_auth_snapshot(backup: &Path) -> Result<CursorAuthSnapshot, String> {
    let auth_json = backup.join("auth.json");
    if !auth_json.exists() {
        return Err("快照不完整：缺少 auth.json。请登录目标账号后重新「捕获」。".to_string());
    }
    let content = fs::read_to_string(&auth_json)
        .map_err(|e| format!("读取 auth.json 失败: {}", e))?;
    let snap: CursorAuthSnapshot =
        serde_json::from_str(&content).map_err(|e| format!("解析 auth.json 失败: {}", e))?;
    if snap.items.get("cursorAuth/accessToken").map(|t| t.is_empty()).unwrap_or(true) {
        return Err("快照不完整：缺少 accessToken，请重新捕获。".to_string());
    }
    Ok(snap)
}

fn apply_auth_to_data_root(data_root: &Path, snapshot: &CursorAuthSnapshot) -> Result<(), String> {
    if snapshot.items.is_empty() {
        return Err("备份里没有 cursorAuth 登录数据，请重新捕获该账号".to_string());
    }
    let db = live_state_vscdb_in(data_root);
    if let Some(parent) = db.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    let conn = Connection::open(&db).map_err(|e| format!("打开 state.vscdb 失败: {}", e))?;
    let _ = conn.execute_batch(
        "PRAGMA busy_timeout=8000;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=MEMORY;
         CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
    );
    // Delete and insert in one transaction so a failed write rolls back the
    // previous login instead of leaving Cursor signed out.
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启事务失败: {}", e))?;
    tx.execute(
        "DELETE FROM ItemTable WHERE key LIKE 'cursorAuth/%' \
         OR key IN ('glass.lastSignedInAuthId', 'adminSettings.cachedAuthId')",
        [],
    )
    .map_err(|e| format!("清理旧登录数据失败: {}", e))?;
    {
        let mut stmt = tx
            .prepare("INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?, ?)")
            .map_err(|e| format!("准备写入失败: {}", e))?;
        for (key, value) in &snapshot.items {
            stmt.execute(rusqlite::params![key, value])
                .map_err(|e| format!("写入 {} 失败: {}", key, e))?;
        }
    }
    tx.commit()
        .map_err(|e| format!("提交登录数据失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
}

/// Essential Chromium session files (Cookies need matching Local State).
const NETWORK_SESSION_FILES: &[&str] = &[
    "Cookies",
    "Cookies-journal",
    "Network Persistent State",
    "NetworkDataMigrated",
    "TransportSecurity",
    "Trust Tokens",
    "Trust Tokens-journal",
];

fn copy_network_session(src_network: &Path, dst_network: &Path) -> Result<(), String> {
    if !src_network.exists() {
        return Ok(());
    }
    fs::create_dir_all(dst_network).map_err(|e| format!("创建 Network 目录失败: {}", e))?;

    // Stage the incoming session first. Live cookies stay until every copy
    // succeeds, so a locked or missing source file cannot log the user out.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let staging = dst_network.with_file_name(format!("Network.incoming-{stamp}"));
    let backup = dst_network.with_file_name(format!("Network.prev-{stamp}"));
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_dir_all(&backup);
    fs::create_dir_all(&staging).map_err(|e| format!("创建会话暂存目录失败: {}", e))?;

    let mut staged = Vec::new();
    for name in NETWORK_SESSION_FILES {
        let src = src_network.join(name);
        if !src.exists() {
            continue;
        }
        if let Err(e) = copy_file_retry(&src, &staging.join(name)) {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
        staged.push(*name);
    }
    if staged.is_empty() {
        let _ = fs::remove_dir_all(&staging);
        return Ok(());
    }

    fs::create_dir_all(&backup).map_err(|e| format!("创建会话回滚目录失败: {}", e))?;
    let swap = (|| {
        let mut moved = Vec::new();
        let mut placed_new = Vec::new();
        let rollback = |moved: &[&str], placed_new: &[&str]| {
            for name in placed_new.iter().rev() {
                let _ = fs::remove_file(dst_network.join(name));
            }
            for prev in moved.iter().rev() {
                let _ = move_file_replace(&backup.join(prev), &dst_network.join(prev));
            }
        };
        for name in NETWORK_SESSION_FILES {
            let live = dst_network.join(name);
            if live.exists() {
                if let Err(e) = move_file_replace(&live, &backup.join(name)) {
                    rollback(&moved, &placed_new);
                    return Err(e);
                }
                moved.push(*name);
            }
            if staged.contains(name) {
                if let Err(e) = move_file_replace(&staging.join(name), &live) {
                    rollback(&moved, &placed_new);
                    return Err(e);
                }
                if !moved.contains(name) {
                    placed_new.push(*name);
                }
            }
        }
        Ok(())
    })();

    let _ = fs::remove_dir_all(&staging);
    if swap.is_err() {
        let _ = fs::remove_dir_all(&backup);
        return swap;
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(())
}

fn move_file_replace(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    if dst.exists() {
        let _ = fs::remove_file(dst);
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(src, dst).map_err(|e| format!("移动 {} 失败: {}", src.display(), e))?;
            let _ = fs::remove_file(src);
            Ok(())
        }
    }
}

/// Backup current Cursor login (auth keys + browser session), not the whole 2GB globalStorage.
///
/// Internal only. It snapshots whichever data root is currently active into an
/// arbitrary account id and performs **no email check**, so exposing it as an IPC
/// entry point would let a wrong login be filed under the wrong account. The
/// supported path for saving a login is `finish_account_profile`, which validates
/// the captured email instead.
fn backup_cursor_data(account_id: String) -> Result<String, String> {
    let data_root = if get_active_account_id().is_some() {
        resolve_data_dir()
    } else {
        cursor_data_dir()
    };
    backup_cursor_data_from_root(&account_id, &data_root)
}

/// Restore Cursor login from a backup into a specific data directory.
fn restore_into_data_dir(account_id: &str, data_root: &Path) -> Result<(), String> {
    let backup = backup_dir(account_id)?;
    if !backup.exists() {
        return Err(format!("Backup not found for account: {}", account_id));
    }

    let status = inspect_backup_dir(account_id, &backup);
    if !status.complete {
        return Err(format!("无法切换：{}", status.reason));
    }

    let snapshot = load_auth_snapshot(&backup)?;
    let expected_email = if snapshot.email.is_empty() {
        status.auth_email.clone()
    } else {
        snapshot.email.clone()
    };

    copy_file_retry(
        &backup.join("Local State"),
        &data_root.join("Local State"),
    )?;
    copy_network_session(&backup.join("Network"), &data_root.join("Network"))?;
    apply_auth_to_data_root(data_root, &snapshot)?;
    copy_file_retry(
        &backup.join("sentry").join("session.json"),
        &data_root.join("sentry").join("session.json"),
    )?;

    for (src, dst) in session_dir_pairs_for(&backup, data_root) {
        if src.exists() {
            replace_dir(&src, &dst)?;
        }
    }

    verify_restored_login_in(data_root, &expected_email)
}

fn verify_restored_login_in(data_root: &Path, expected_email: &str) -> Result<(), String> {
    let cookies = data_root.join("Network").join("Cookies");
    if !cookies.exists() {
        return Err("恢复后缺少 Cookies，token 与浏览器会话不匹配。请重新捕获目标账号。".to_string());
    }
    if cookies.metadata().map(|m| m.len() == 0).unwrap_or(true) {
        return Err("恢复后 Cookies 为空，请重新捕获目标账号。".to_string());
    }

    if expected_email.is_empty() {
        return Ok(());
    }

    let live_db = live_state_vscdb_in(data_root);
    let live = extract_auth_from_db(&live_db).or_else(|_| {
        let temp = copy_live_db_for_read_at(&live_db)?;
        let r = extract_auth_from_db(&temp);
        if let Some(parent) = temp.parent() {
            let _ = fs::remove_dir_all(parent);
        }
        r
    })?;

    if live.email.is_empty() {
        return Err(format!(
            "恢复后无法读取登录邮箱（期望 {}）。请重新捕获目标账号。",
            expected_email
        ));
    }
    if !emails_match(expected_email, &live.email)
        && !identity_match(expected_email, expected_email, &live)
    {
        return Err(format!(
            "恢复后登录态不匹配：期望 {}，实际 {}。请重新捕获目标账号。",
            expected_email, live.email
        ));
    }
    Ok(())
}

/// Restore Cursor login from a backup into shared Cursor directory (legacy).
fn restore_cursor_data_impl(account_id: &str) -> Result<(), String> {
    restore_into_data_dir(account_id, &cursor_data_dir())
}

fn migrate_backup_to_profile_impl(account_id: &str) -> Result<PathBuf, String> {
    let profile = cursor_profile_dir(account_id)?;
    restore_into_data_dir(account_id, &profile)?;
    mark_profile_initialized_at(&profile)?;
    prepare_profile_shared(&profile)?;
    Ok(profile)
}

#[tauri::command]
pub fn inspect_cursor_backup(account_id: String) -> Result<CursorBackupStatus, String> {
    let backup = backup_dir(&account_id)?;
    if !backup.exists() {
        return Ok(CursorBackupStatus {
            account_id,
            complete: false,
            auth_email: String::new(),
            has_auth_json: false,
            has_cookies: false,
            reason: "备份目录不存在".to_string(),
            warning: None,
        });
    }
    Ok(inspect_backup_dir(&account_id, &backup))
}

fn get_cursor_login_status_sync() -> Result<CursorLoginStatus, String> {
    // Keep active-account in sync with the running process when possible.
    if let Some(dir) = detect_running_cursor_data_dir() {
        match account_id_from_profile_path(&dir) {
            Some(id) => set_active_account(Some(&id)),
            // Default %APPDATA%\Cursor — clear stale managed-profile pointer.
            None => {
                if get_active_account_id().is_some() {
                    set_active_account(None);
                }
            }
        }
    }

    if let Ok(snapshot) = extract_auth_from_live() {
        if !snapshot.email.is_empty() {
            return Ok(CursorLoginStatus {
                email: snapshot.email,
                name: snapshot.name,
                is_logged_in: true,
            });
        }
        if snapshot.items.contains_key("cursorAuth/accessToken") {
            return Ok(CursorLoginStatus {
                email: snapshot.email,
                name: snapshot.name,
                is_logged_in: true,
            });
        }
    }

    Ok(CursorLoginStatus {
        email: String::new(),
        name: String::new(),
        is_logged_in: false,
    })
}

/// Get current Cursor login from live state.vscdb (not the stale .backup file).
#[tauri::command]
pub async fn get_cursor_login_status() -> Result<CursorLoginStatus, String> {
    tokio::task::spawn_blocking(get_cursor_login_status_sync)
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

#[tauri::command]
pub async fn is_cursor_running() -> bool {
    tokio::task::spawn_blocking(is_cursor_process_running)
        .await
        .unwrap_or(false)
}

#[tauri::command]
pub async fn quit_cursor() -> Result<String, String> {
    tokio::task::spawn_blocking(quit_cursor_sync)
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

/// Blocking core of `quit_cursor`: taskkill plus a soft-quit wait loop in 300ms
/// steps, which must not run on the main thread.
fn quit_cursor_sync() -> Result<String, String> {
    // Cache install path before killing — launch after switch relies on this.
    cache_cursor_exe_from_running();

    if !is_cursor_process_running() {
        return Ok("Cursor 未在运行".to_string());
    }

    let images = [
        "Cursor.exe",
        "Cursor Helper.exe",
        "Cursor Helper (GPU).exe",
        "Cursor Helper (Renderer).exe",
        "Cursor Helper (Plugin).exe",
    ];

    // Soft quit first (no /F) so Cookies / WAL can flush
    for image in images {
        let _ = hidden_command("taskkill").args(["/IM", image]).output();
    }

    let soft_deadline = Instant::now() + Duration::from_millis(CURSOR_SOFT_QUIT_TIMEOUT_MS);
    while is_cursor_process_running() && Instant::now() < soft_deadline {
        std::thread::sleep(Duration::from_millis(300));
    }

    if is_cursor_process_running() {
        for image in images {
            let _ = hidden_command("taskkill")
                .args(["/F", "/IM", image])
                .output();
        }
    }

    wait_until_cursor_stopped(CURSOR_STOP_WAIT_TIMEOUT_MS)?;
    invalidate_cursor_probes();
    Ok("已关闭 Cursor".to_string())
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok()?.to_file_path().ok()
}

fn push_unique_folder(folders: &mut Vec<PathBuf>, path: PathBuf) {
    if folders.iter().any(|existing| existing == &path) {
        return;
    }
    folders.push(path);
}

fn recent_workspace_folders_from_default() -> Vec<PathBuf> {
    let storage_path = default_user_dir()
        .join("globalStorage")
        .join("storage.json");
    let Ok(raw) = fs::read_to_string(&storage_path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };

    let mut folders = Vec::new();
    if let Some(uri) = json
        .pointer("/windowsState/lastActiveWindow/folder")
        .and_then(|v| v.as_str())
    {
        if let Some(path) = file_uri_to_path(uri) {
            push_unique_folder(&mut folders, path);
        }
    }
    if let Some(entries) = json
        .pointer("/backupWorkspaces/folders")
        .and_then(|v| v.as_array())
    {
        for item in entries.iter().rev() {
            if let Some(uri) = item.get("folderUri").and_then(|v| v.as_str()) {
                if let Some(path) = file_uri_to_path(uri) {
                    push_unique_folder(&mut folders, path);
                }
            }
        }
    }
    folders
}

fn pick_launch_workspace_folder() -> Option<PathBuf> {
    recent_workspace_folders_from_default()
        .into_iter()
        .find(|path| path.exists())
}

fn launch_cursor_impl(account_id: Option<String>) -> Result<String, String> {
    let exe = cursor_exe_path()?;
    let exe_str = exe.to_string_lossy().to_string();

    if let Some(ref id) = account_id {
        if is_cursor_process_running() {
            quit_cursor_sync()?;
        }
        let profile = cursor_profile_dir(id)?;
        fs::create_dir_all(&profile).map_err(|e| format!("创建 profile 目录失败: {}", e))?;
        prepare_profile_shared(&profile)?;
        // Sync may carry over BLOB-typed values; heal before Cursor reads them,
        // otherwise its workbench aborts with a JSON.parse error (black screen).
        if let Err(e) = repair_blob_typed_state(&live_state_vscdb_in(&profile)) {
            eprintln!("[cursor] state type repair: {e}");
        }
        set_active_account(Some(id));

        spawn_cursor(&exe, Some(&profile))?;
        invalidate_cursor_probes();
        Ok(format!("已启动独立配置 Cursor（账号 {}）", id))
    } else {
        set_active_account(None);
        spawn_cursor(&exe, None)?;
        invalidate_cursor_probes();
        Ok(format!("已启动 {}", exe_str))
    }
}

/// Start Cursor as an independent process.
///
/// Launched directly rather than through `cmd /c start`: shell nesting mangled
/// quoting for install paths containing spaces, and could hand the GUI process
/// a broken window station. Real argv entries need no manual quoting.
fn spawn_cursor(exe: &Path, profile: Option<&Path>) -> Result<(), String> {
    let mut cmd = Command::new(exe);
    if let Some(profile) = profile {
        cmd.arg(format!("--user-data-dir={}", profile.to_string_lossy()));
        if let Some(folder) = pick_launch_workspace_folder() {
            cmd.arg(folder);
        }
    }
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动 Cursor 失败: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn launch_cursor(account_id: Option<String>) -> Result<String, String> {
    launch_cursor_impl(account_id)
}

#[tauri::command]
pub fn get_cursor_profile_dir(account_id: String) -> Result<String, String> {
    Ok(cursor_profile_dir(&account_id)?.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn init_account_profile(
    window: tauri::Window,
    account_id: String,
) -> Result<String, String> {
    let window = window.clone();
    tokio::task::spawn_blocking(move || init_account_profile_impl(&window, account_id))
        .await
        .map_err(|e| format!("初始化任务异常: {}", e))?
}

fn init_account_profile_impl(
    window: &tauri::Window,
    account_id: String,
) -> Result<String, String> {
    let _profile = cursor_profile_dir(&account_id)?;
    let already_this_profile = get_active_account_id().as_deref() == Some(account_id.as_str());

    // Before opening a new account profile, refresh the currently active account backup
    // so quitting does not leave the first account with a stale/incomplete snapshot.
    if is_cursor_process_running() && !already_this_profile {
        if let Some(active_id) = get_active_account_id() {
            if active_id != account_id {
                if let Ok(active_profile) = cursor_profile_dir(&active_id) {
                    if is_profile_initialized_at(&active_profile) {
                        emit_switch_progress(
                            Some(window),
                            "save",
                            "正在保存当前账号登录态（关闭前备份）…",
                        );
                        let capture = if data_root_has_login(&active_profile) {
                            active_profile.clone()
                        } else if let Some(running) = detect_running_cursor_data_dir() {
                            running
                        } else {
                            active_profile.clone()
                        };
                        if let Err(e) = backup_cursor_data_from_root(&active_id, &capture) {
                            eprintln!("[cursor] pre-init backup of {active_id}: {e}");
                        }
                    }
                }
            }
        }
        emit_switch_progress(Some(window), "quit", "正在关闭 Cursor 以打开新账号配置…");
        quit_cursor_sync()?;
    }

    if is_cursor_process_running() && already_this_profile {
        return Ok(
            "该账号的独立 Cursor 已在运行。请在其中登录，然后回到 AI Workbench 点「完成初始化」。"
                .to_string(),
        );
    }

    emit_switch_progress(Some(window), "launch", "正在打开独立配置（可能需数十秒）…");
    launch_cursor_impl(Some(account_id))?;
    emit_switch_progress(Some(window), "done", "已打开独立配置");
    Ok("已打开该账号的独立 Cursor 配置。请在其中登录，然后回到 AI Workbench 点「完成初始化」。".to_string())
}

fn path_same(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => {
            let norm = |p: &Path| {
                p.to_string_lossy()
                    .replace('/', "\\")
                    .trim_end_matches('\\')
                    .to_ascii_lowercase()
            };
            norm(a) == norm(b)
        }
    }
}

fn read_auth_snapshot_from_root(data_root: &Path) -> Result<CursorAuthSnapshot, String> {
    let live_db = live_state_vscdb_in(data_root);
    if !live_db.exists() {
        return Err("未找到 Cursor 登录数据库".into());
    }
    extract_auth_from_db(&live_db).or_else(|direct_err| {
        match copy_live_db_for_read_at(&live_db) {
            Ok(temp_db) => {
                let result = extract_auth_from_db(&temp_db);
                if let Some(parent) = temp_db.parent() {
                    let _ = fs::remove_dir_all(parent);
                }
                result.map_err(|e| format!("{direct_err}; {e}"))
            }
            Err(copy_err) => Err(format!("{direct_err}; {copy_err}")),
        }
    })
}

fn data_root_has_login(data_root: &Path) -> bool {
    match read_auth_snapshot_from_root(data_root) {
        Ok(s) => {
            !s.email.is_empty()
                && s.items
                    .get("cursorAuth/accessToken")
                    .map(|t| !t.is_empty())
                    .unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// Capture login only from this account's independent profile (or the running
/// Cursor that is already using that same profile). Never fall back to the
/// default %APPDATA%\\Cursor — that would steal another account's session.
fn resolve_finish_capture_root(profile: &Path) -> Result<PathBuf, String> {
    if data_root_has_login(profile) {
        return Ok(profile.to_path_buf());
    }
    if let Some(running) = detect_running_cursor_data_dir() {
        if path_same(&running, profile) && data_root_has_login(&running) {
            return Ok(running);
        }
    }
    Err(
        "请先在该账号的独立 Cursor 窗口中登录，再点「完成初始化」。不要用默认手动打开的 Cursor 来初始化其他账号。"
            .to_string(),
    )
}

/// Same Cursor login cannot occupy two slots. Remove leftover backup/profile
/// dirs that already captured this email under a different account id
/// (common after failed/re-added inits), so the current slot can finish.
fn reclaim_duplicate_email_slots(
    keep_account_id: &str,
    email: &str,
) -> Result<usize, String> {
    let email = email.trim();
    if email.is_empty() {
        return Ok(0);
    }
    let Ok(root) = backups_root_dir() else {
        return Ok(0);
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(0);
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let other_id = entry.file_name().to_string_lossy().to_string();
        if other_id == keep_account_id {
            continue;
        }
        let status = inspect_backup_dir(&other_id, &entry.path());
        if status.auth_email.is_empty() || !emails_match(&status.auth_email, email) {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
        if let Ok(profile) = cursor_profile_dir(&other_id) {
            let _ = fs::remove_dir_all(profile);
        }
        removed += 1;
    }
    Ok(removed)
}

fn finish_account_profile_impl(
    window: &tauri::Window,
    account_id: String,
    relaunch: Option<bool>,
) -> Result<String, String> {
    let should_relaunch = relaunch.unwrap_or(true);
    let profile = cursor_profile_dir(&account_id)?;
    let mut capture_root = resolve_finish_capture_root(&profile)?;

    emit_switch_progress(Some(window), "save", "正在保存登录态…");
    let preflight = read_auth_snapshot_from_root(&capture_root).map_err(|e| {
        format!("{e}。请先在该账号的独立 Cursor 中登录，再点「完成初始化」。")
    })?;
    match reclaim_duplicate_email_slots(&account_id, &preflight.email) {
        Ok(n) if n > 0 => {
            emit_switch_progress(
                Some(window),
                "save",
                &format!("已清理 {n} 个同邮箱旧配置，继续初始化…"),
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("[cursor] reclaim duplicate email: {e}"),
    }

    let path = match backup_cursor_data_from_root(&account_id, &capture_root) {
        Ok(p) => p,
        Err(first_err) => {
            if !is_cursor_process_running() {
                return Err(format!(
                    "{first_err}。请先在该账号的独立 Cursor 中登录，再点「完成初始化」。"
                ));
            }
            emit_switch_progress(Some(window), "quit", "正在关闭 Cursor 以便完整保存 Cookies…");
            quit_cursor_sync()?;
            capture_root = resolve_finish_capture_root(&profile)?;
            let retry_snap = read_auth_snapshot_from_root(&capture_root)?;
            let _ = reclaim_duplicate_email_slots(&account_id, &retry_snap.email);
            backup_cursor_data_from_root(&account_id, &capture_root).map_err(|e| {
                format!(
                    "{first_err}（关闭 Cursor 后仍失败: {e}）。请确认已在独立配置中登录后再试。"
                )
            })?
        }
    };

    // Captured from another path that is still this same profile after canonicalize.
    if !path_same(&capture_root, &profile) {
        if is_cursor_process_running() {
            emit_switch_progress(Some(window), "quit", "正在关闭 Cursor 以便写入独立配置…");
            quit_cursor_sync()?;
        }
        emit_switch_progress(Some(window), "seed", "正在写入独立账号配置…");
        restore_into_data_dir(&account_id, &profile).map_err(|e| {
            format!("登录态已备份，但写入独立配置失败: {e}")
        })?;
    }

    mark_profile_initialized_at(&profile)?;
    emit_switch_progress(Some(window), "seed", "正在链接共享工作区…");
    prepare_profile_shared(&profile)?;
    set_active_account(Some(&account_id));

    if should_relaunch && !is_cursor_process_running() {
        emit_switch_progress(Some(window), "launch", "正在重新打开 Cursor…");
        launch_cursor_impl(Some(account_id.clone()))?;
    }

    emit_switch_progress(Some(window), "done", "初始化完成");
    Ok(path)
}

#[tauri::command]
pub async fn finish_account_profile(
    window: tauri::Window,
    account_id: String,
    relaunch: Option<bool>,
) -> Result<String, String> {
    let window = window.clone();
    tokio::task::spawn_blocking(move || finish_account_profile_impl(&window, account_id, relaunch))
        .await
        .map_err(|e| format!("初始化任务异常: {}", e))?
}

#[tauri::command]
pub fn migrate_account_to_profile(account_id: String) -> Result<String, String> {
    let profile = migrate_backup_to_profile_impl(&account_id)?;
    Ok(profile.to_string_lossy().to_string())
}

fn switch_cursor_account_impl(
    window: Option<&tauri::Window>,
    target_account_id: String,
    current_account_id: Option<String>,
    relaunch: Option<bool>,
) -> Result<String, String> {
    let should_relaunch = relaunch.unwrap_or(true);

    let target_backup = backup_dir(&target_account_id)?;
    let target_status = inspect_backup_dir(&target_account_id, &target_backup);
    if !target_status.complete {
        return Err(format!(
            "目标账号快照不可用：{}。请先登录该账号并重新捕获。",
            target_status.reason
        ));
    }

    let target_profile = cursor_profile_dir(&target_account_id)?;
    let profile_ready = is_profile_initialized_at(&target_profile);

    // Profile mode: each account lives in its own --user-data-dir; no live overwrite needed.
    if !profile_ready {
        if let Some(current_id) = current_account_id.as_ref() {
            if current_id != &target_account_id {
                let current_backup = backup_dir(current_id)?;
                let mut should_backup = true;
                if current_backup.join("auth.json").exists() {
                    if let Ok(existing) = load_auth_snapshot(&current_backup) {
                        if let Ok(live) = extract_auth_from_live() {
                            if !existing.email.is_empty()
                                && !live.email.is_empty()
                                && !emails_match(&existing.email, &live.email)
                            {
                                return Err(format!(
                                    "拒绝覆盖快照：槽位是 {}，当前登录是 {}。请先确认 Cursor 中的当前登录账号，或对该账号重新「完成初始化」。",
                                    existing.email, live.email
                                ));
                            }
                            if !existing.email.is_empty()
                                && !live.email.is_empty()
                                && emails_match(&existing.email, &live.email)
                            {
                                should_backup = false;
                            }
                        }
                    }
                }
                if should_backup {
                    emit_switch_progress(window, "save", "正在保存当前账号状态…");
                    backup_cursor_data(current_id.clone())?;
                }
            }
        }
    }

    if is_cursor_process_running() {
        let active_profile = get_active_account_id()
            .or_else(|| detect_profile_from_running_cursor())
            .and_then(|id| cursor_profile_dir(&id).ok());
        if let Some(profile) = active_profile {
            let _ = sync_profile_state_to_shared(&profile);
        }
        emit_switch_progress(window, "quit", "正在关闭 Cursor…");
        cache_cursor_exe_from_running();
        quit_cursor_sync()?;
    } else {
        let _ = cursor_exe_path();
    }

    if profile_ready {
        emit_switch_progress(window, "restore", "正在切换到独立配置…");
        if should_relaunch {
            launch_cursor_impl(Some(target_account_id.clone()))?;
            emit_switch_progress(window, "done", "切换完成");
            Ok(format!(
                "已切换到 {}（独立配置，无需覆盖共享登录态）",
                target_status.auth_email
            ))
        } else {
            emit_switch_progress(window, "done", "切换完成");
            Ok(format!("已切换到 {}", target_status.auth_email))
        }
    } else if migrate_backup_to_profile_impl(&target_account_id).is_ok() {
        emit_switch_progress(window, "restore", "已升级为独立配置…");
        if should_relaunch {
            launch_cursor_impl(Some(target_account_id.clone()))?;
            emit_switch_progress(window, "done", "切换完成");
            Ok(format!(
                "已升级为独立配置并切换到 {}。该账号 Sign Out 不再影响其他账号。",
                target_status.auth_email
            ))
        } else {
            emit_switch_progress(window, "done", "切换完成");
            Ok(format!("已切换到 {}", target_status.auth_email))
        }
    } else {
        emit_switch_progress(window, "restore", "正在恢复目标账号（兼容模式）…");
        if let Err(e) = restore_cursor_data_impl(&target_account_id) {
            if let Some(current_id) = current_account_id.clone() {
                emit_switch_progress(window, "rollback", "恢复失败，正在回滚…");
                let _ = restore_cursor_data_impl(&current_id);
            }
            if should_relaunch {
                let _ = launch_cursor_impl(get_active_account_id());
            }
            return Err(format!(
                "{}。该账号可能在捕获后被 Sign Out 导致 token 作废，请重新登录并重捕。",
                e
            ));
        }

        let expected = target_status.auth_email.clone();
        let live = extract_auth_from_live().unwrap_or_default();
        let matched = (!expected.is_empty() && emails_match(&expected, &live.email))
            || identity_match(&expected, &expected, &live);
        if !expected.is_empty() && !live.email.is_empty() && !matched {
            if let Some(current_id) = current_account_id.clone() {
                let _ = restore_cursor_data_impl(&current_id);
            }
            if should_relaunch {
                let _ = launch_cursor_impl(get_active_account_id());
            }
            return Err(format!(
                "切换未生效：期望 {}，实际仍是 {}。该账号可能在捕获后被 Sign Out，请重新登录并重捕。",
                expected, live.email
            ));
        }

        let switched_email = if live.email.is_empty() {
            expected
        } else {
            live.email
        };

        if should_relaunch {
            emit_switch_progress(window, "launch", "正在重新打开 Cursor…");
            match launch_cursor_impl(None) {
                Ok(_) => {
                    emit_switch_progress(window, "done", "切换完成");
                    Ok(format!("已切换到 {} 并重新打开 Cursor", switched_email))
                }
                Err(e) => Ok(format!(
                    "账号已切换到 {}，但自动启动失败：{}。请手动打开 Cursor。",
                    switched_email, e
                )),
            }
        } else {
            emit_switch_progress(window, "done", "切换完成");
            Ok(format!("已切换到 {}", switched_email))
        }
    }
}

#[tauri::command]
pub async fn switch_cursor_account(
    window: tauri::Window,
    target_account_id: String,
    current_account_id: Option<String>,
    relaunch: Option<bool>,
) -> Result<String, String> {
    let window = window.clone();
    tokio::task::spawn_blocking(move || {
        switch_cursor_account_impl(
            Some(&window),
            target_account_id,
            current_account_id,
            relaunch,
        )
    })
    .await
    .map_err(|e| format!("切换任务异常: {}", e))?
}

#[tauri::command]
pub fn delete_cursor_backup(account_id: String) -> Result<(), String> {
    let backup = backup_dir(&account_id)?;
    remove_tree_removing_links(&backup)?;
    // The profile holds junctions/symlinks into the default Cursor data
    // (workspaceStorage, History, extensions, globalStorage/*) — unlink them
    // without following, then drop the profile directory itself.
    let profile = cursor_profile_dir(&account_id)?;
    remove_tree_removing_links(&profile)?;
    Ok(())
}

/// Account ids registered in the app database (best effort).
fn known_cursor_account_ids() -> Vec<String> {
    let Ok(dir) = app_data_dir() else {
        return Vec::new();
    };
    let db_path = dir.join("ai-workbench.db");
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT id FROM cursor_accounts") {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
            for id in rows.flatten() {
                ids.push(id);
            }
        }
    }
    ids
}

/// Remove profile directories no account references. They are app-managed
/// artifacts, so unreferenced ones are pure garbage — and removal unlinks
/// junctions instead of following them, so shared default data is untouched.
///
/// Guarded twice: nothing is deleted when the account list is unavailable, and
/// directories touched within the last hour are skipped (an account created
/// moments ago may not be persisted yet).
fn cleanup_orphan_profiles(known_ids: &[String]) -> usize {
    let mut removed = 0usize;
    for (name, path, _) in orphan_profile_dirs(known_ids) {
        match remove_tree_removing_links(&path) {
            Ok(()) => {
                eprintln!("[cursor] removed orphan profile '{name}'");
                removed += 1;
            }
            Err(e) => eprintln!("[cursor] orphan profile '{name}': {e}"),
        }
    }
    removed
}

/// Profile directories no account references, past the 1-hour grace period:
/// `(name, path, bytes)`. Sizes skip links so shared Cursor data is not counted.
fn orphan_profile_dirs(known_ids: &[String]) -> Vec<(String, PathBuf, u64)> {
    let mut found = Vec::new();
    // Never guess: an unreadable account list must not mark anything orphaned.
    if known_ids.is_empty() {
        return found;
    }
    let Ok(root) = profiles_root_dir() else {
        return found;
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return found;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if known_ids.iter().any(|id| id == &name) {
            continue;
        }
        // An account created moments ago may not be persisted yet.
        let recently_touched = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age < Duration::from_secs(3600))
            .unwrap_or(false);
        if recently_touched {
            continue;
        }
        let path = entry.path();
        let bytes = dir_size(&path);
        found.push((name, path, bytes));
    }
    found
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorOrphanProfiles {
    pub count: u32,
    pub bytes: u64,
    pub ids: Vec<String>,
}

/// Unreferenced profile directories, surfaced so the user can reclaim disk.
#[tauri::command]
pub fn get_cursor_orphan_profiles() -> Result<CursorOrphanProfiles, String> {
    let dirs = orphan_profile_dirs(&known_cursor_account_ids());
    Ok(CursorOrphanProfiles {
        count: dirs.len() as u32,
        bytes: dirs.iter().map(|(_, _, bytes)| *bytes).sum(),
        ids: dirs.into_iter().map(|(name, _, _)| name).collect(),
    })
}

#[tauri::command]
pub fn cleanup_cursor_orphan_profiles() -> Result<CursorCleanupResult, String> {
    let known = known_cursor_account_ids();
    if known.is_empty() {
        return Err("未能读取账号列表，已跳过清理".into());
    }
    let freed: u64 = orphan_profile_dirs(&known)
        .iter()
        .map(|(_, _, bytes)| *bytes)
        .sum();
    let removed = cleanup_orphan_profiles(&known);
    Ok(CursorCleanupResult {
        removed_files: removed as u32,
        freed_bytes: freed,
        message: if removed == 0 {
            "没有可清理的无主配置".into()
        } else {
            format!("已清理 {removed} 个无主配置，释放约 {}", format_bytes(freed))
        },
    })
}

fn tail_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(count);
    lines[start..].join("\n")
}

/// Copy-pasteable diagnostic dump for the Cursor page: paths, live login state
/// and the tail of the newest Cursor log session for this account.
#[tauri::command]
pub fn read_cursor_diagnostics(account_id: Option<String>) -> Result<String, String> {
    let mut out = String::new();
    out.push_str(&format!(
        "cursor-exe:   {}\n",
        cursor_exe_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "(not found)".into())
    ));
    out.push_str(&format!("running:      {}\n", is_cursor_process_running()));
    out.push_str(&format!(
        "active:       {}\n",
        get_active_account_id().unwrap_or_else(|| "-".into())
    ));

    let data_root = match account_id.as_deref() {
        Some(id) => cursor_profile_dir(id)?,
        None => cursor_data_dir(),
    };
    out.push_str(&format!("profile:      {}\n", data_root.display()));

    match get_cursor_login_status_sync() {
        Ok(login) => out.push_str(&format!(
            "live-login:   {} <{}> loggedIn={}\n",
            login.name, login.email, login.is_logged_in
        )),
        Err(e) => out.push_str(&format!("live-login:   <error: {e}>\n")),
    }

    if let Some(id) = account_id.as_deref() {
        if let Ok(status) = inspect_cursor_backup(id.to_string()) {
            out.push_str(&format!(
                "snapshot:     complete={} authEmail={} reason={}\n",
                status.complete, status.auth_email, status.reason
            ));
        }
    }

    let logs_root = data_root.join("logs");
    let mut sessions: Vec<PathBuf> = fs::read_dir(&logs_root)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    sessions.sort();
    if let Some(latest) = sessions.pop() {
        out.push_str(&format!(
            "\n=== newest log session: {} ===\n",
            latest.display()
        ));
        for rel in [
            "main.log",
            "renderer.log",
            "exthost.log",
            "window1_wb0/renderer.log",
        ] {
            let path = latest.join(rel);
            if let Ok(text) = fs::read_to_string(&path) {
                out.push_str(&format!(
                    "\n--- {rel} (tail 60) ---\n{}\n",
                    tail_lines(&text, 60)
                ));
            }
        }
    } else {
        out.push_str("\n(no Cursor log session found)\n");
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorBackupInfo {
    pub account_id: String,
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorDiskUsage {
    pub backups_bytes: u64,
    pub backups_full_db_bytes: u64,
    pub stale_db_count: u32,
    pub shared_bytes: u64,
    pub live_db_bytes: u64,
    pub backups_path: String,
    pub shared_path: String,
    pub live_db_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorCleanupResult {
    pub removed_files: u32,
    pub freed_bytes: u64,
    pub message: String,
}

fn backups_root_dir() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("cursor-backups"))
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// Old full-copy leftovers under cursor-backups/*/globalStorage/ (auth.json backups do not need these).
fn collect_stale_full_backup_dbs(backups_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(backups_root) else {
        return out;
    };
    const NAMES: &[&str] = &[
        "state.vscdb",
        "state.vscdb-wal",
        "state.vscdb-shm",
        "state.vscdb.backup",
    ];
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let gs = entry.path().join("globalStorage");
        if !gs.is_dir() {
            continue;
        }
        for name in NAMES {
            let p = gs.join(name);
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

fn try_remove_empty_dir(path: &Path) {
    if let Ok(mut rd) = fs::read_dir(path) {
        if rd.next().is_none() {
            let _ = fs::remove_dir(path);
        }
    }
}

fn get_cursor_disk_usage_sync() -> Result<CursorDiskUsage, String> {
    let backups_path = backups_root_dir()?;
    let shared_path = cursor_shared_dir()?;
    let live_db = live_state_vscdb();

    let stale = if backups_path.exists() {
        collect_stale_full_backup_dbs(&backups_path)
    } else {
        Vec::new()
    };
    let backups_full_db_bytes: u64 = stale.iter().map(|p| file_size(p)).sum();
    let stale_db_count = stale
        .iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "state.vscdb" || n == "state.vscdb.backup")
                .unwrap_or(false)
        })
        .count() as u32;

    Ok(CursorDiskUsage {
        backups_bytes: if backups_path.exists() {
            dir_size(&backups_path)
        } else {
            0
        },
        backups_full_db_bytes,
        stale_db_count,
        shared_bytes: if shared_path.exists() {
            shared_layer_bytes(&shared_path)
        } else {
            0
        },
        live_db_bytes: file_size(&live_db),
        backups_path: backups_path.to_string_lossy().to_string(),
        shared_path: shared_path.to_string_lossy().to_string(),
        live_db_path: live_db.to_string_lossy().to_string(),
    })
}

fn cleanup_cursor_full_backups_sync() -> Result<CursorCleanupResult, String> {
    let mut removed = 0u32;
    let mut freed = 0u64;
    let mut parts: Vec<String> = Vec::new();

    match cleanup_shared_bloated_state_dbs() {
        Ok(r) if r.removed_files > 0 => {
            removed += r.removed_files;
            freed += r.freed_bytes;
            parts.push(r.message);
        }
        Ok(_) => {}
        Err(e) => parts.push(format!("共享层清理跳过: {e}")),
    }
    schedule_delete_legacy_shared_workspace_copies();
    parts.push("已在后台清理共享层旧工作区副本".into());

    let backups_path = backups_root_dir()?;
    if !backups_path.exists() {
        return Ok(CursorCleanupResult {
            removed_files: removed,
            freed_bytes: freed,
            message: if parts.is_empty() {
                "没有找到 cursor-backups 目录".into()
            } else {
                parts.join("；")
            },
        });
    }

    let stale = collect_stale_full_backup_dbs(&backups_path);
    if stale.is_empty() {
        return Ok(CursorCleanupResult {
            removed_files: removed,
            freed_bytes: freed,
            message: if parts.is_empty() {
                "没有可清理的旧全量 state.vscdb（已是轻量快照）".into()
            } else {
                parts.join("；")
            },
        });
    }

    let mut parents = std::collections::HashSet::new();
    let mut backup_freed = 0u64;
    let mut backup_removed = 0u32;
    for path in &stale {
        let size = file_size(path);
        match fs::remove_file(path) {
            Ok(()) => {
                backup_freed += size;
                backup_removed += 1;
                if let Some(parent) = path.parent() {
                    parents.insert(parent.to_path_buf());
                }
            }
            Err(e) => {
                return Err(format!(
                    "删除失败 {}: {}（已删除 {} 个文件，释放 {}）",
                    path.display(),
                    e,
                    removed + backup_removed,
                    format_bytes(freed + backup_freed)
                ));
            }
        }
    }
    for gs in parents {
        try_remove_empty_dir(&gs);
    }
    removed += backup_removed;
    freed += backup_freed;
    parts.push(format!(
        "已清理 {} 个旧全量库文件，释放约 {}",
        backup_removed,
        format_bytes(backup_freed)
    ));

    Ok(CursorCleanupResult {
        removed_files: removed,
        freed_bytes: freed,
        message: parts.join("；"),
    })
}

#[tauri::command]
pub fn list_cursor_backups() -> Result<Vec<CursorBackupInfo>, String> {
    let backups_root = backups_root_dir()?;

    if !backups_root.exists() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();
    for entry in fs::read_dir(&backups_root).map_err(|e| format!("Failed to read backups: {}", e))? {
        if let Ok(entry) = entry {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let path = entry.path();
                let account_id = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let size = dir_size(&path);
                results.push(CursorBackupInfo {
                    account_id,
                    path: path.to_string_lossy().to_string(),
                    size_bytes: size,
                });
            }
        }
    }

    Ok(results)
}

#[tauri::command]
pub async fn get_cursor_disk_usage() -> Result<CursorDiskUsage, String> {
    tokio::task::spawn_blocking(get_cursor_disk_usage_sync)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn cleanup_cursor_full_backups() -> Result<CursorCleanupResult, String> {
    tokio::task::spawn_blocking(cleanup_cursor_full_backups_sync)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorDbSlimTarget {
    pub label: String,
    pub path: String,
    /// "rebuilt" | "vacuumed" | "failed"
    pub action: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub note: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorDbSlimReport {
    pub targets: Vec<CursorDbSlimTarget>,
    pub freed_bytes: u64,
    pub message: String,
}

/// Size of state.vscdb plus its WAL/SHM sidecars — the footprint users see in
/// Explorer and cleanup tools. The WAL can hold hundreds of MB before a
/// checkpoint, so counting only the main file under-reports.
fn state_db_cluster_bytes(db: &Path) -> u64 {
    let mut total = file_size(db);
    for suffix in ["-wal", "-shm"] {
        let mut name = db.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        total += file_size(&db.with_file_name(name));
    }
    total
}

fn remove_state_db_cluster(db: &Path) -> Result<(), String> {
    fs::remove_file(db).map_err(|e| format!("删除 {} 失败: {}", db.display(), e))?;
    for suffix in ["-wal", "-shm"] {
        let mut name = db.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        let sidecar = db.with_file_name(name);
        if sidecar.exists() {
            let _ = fs::remove_file(&sidecar);
        }
    }
    Ok(())
}

/// VACUUM rewrites the database file, reclaiming free pages left behind by
/// deletions (SQLite never shrinks a file on its own). A WAL checkpoint runs
/// first so pending sidecar content is folded back in. Content and login keys
/// are untouched; the caller must ensure Cursor is closed.
fn vacuum_state_db(db: &Path) -> Result<(), String> {
    let conn = Connection::open(db).map_err(|e| format!("打开数据库失败: {}", e))?;
    conn.execute_batch("PRAGMA busy_timeout=8000;")
        .map_err(|e| format!("设置 busy_timeout 失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    conn.execute_batch("VACUUM;")
        .map_err(|e| format!("VACUUM 失败: {}", e))?;
    Ok(())
}

fn vacuum_target(label: &str, db: &Path, targets: &mut Vec<CursorDbSlimTarget>) {
    if !db.exists() {
        return;
    }
    let before = state_db_cluster_bytes(db);
    let (action, note) = match vacuum_state_db(db) {
        Ok(()) => ("vacuumed".to_string(), String::new()),
        Err(e) => ("failed".to_string(), e),
    };
    targets.push(CursorDbSlimTarget {
        label: label.to_string(),
        path: db.to_string_lossy().to_string(),
        action,
        before_bytes: before,
        after_bytes: state_db_cluster_bytes(db),
        note,
    });
}

/// Rebuild the shared state.vscdb from scratch. It historically inherited a
/// full copy of the multi-GB default DB, while its real payload is only
/// ItemTable keys plus Composer rows. Composer data is salvaged before the
/// delete and re-imported afterwards, so no sessions are lost.
fn rebuild_shared_state_db(targets: &mut Vec<CursorDbSlimTarget>) {
    let shared_db = match shared_state_vscdb() {
        Ok(p) => p,
        Err(e) => {
            targets.push(CursorDbSlimTarget {
                label: "shared".into(),
                path: String::new(),
                action: "failed".into(),
                before_bytes: 0,
                after_bytes: 0,
                note: format!("定位共享层失败: {e}"),
            });
            return;
        }
    };
    if !shared_db.exists() {
        return;
    }
    let before = state_db_cluster_bytes(&shared_db);
    let salvage_headers = export_composer_headers(&shared_db).unwrap_or_default();
    let salvage_kv = export_cursor_disk_kv(&shared_db).unwrap_or_default();

    let mut note = String::new();
    if let Err(e) = remove_state_db_cluster(&shared_db) {
        targets.push(CursorDbSlimTarget {
            label: "shared".into(),
            path: shared_db.to_string_lossy().to_string(),
            action: "failed".into(),
            before_bytes: before,
            after_bytes: before,
            note: e,
        });
        return;
    }
    // Drop the seed marker too, so ItemTable keys are re-seeded into the fresh DB.
    if let Ok(marker) = shared_state_seed_marker() {
        let _ = fs::remove_file(&marker);
    }
    if let Err(e) = seed_shared_state_from_default() {
        note = format!("ItemTable 重新播种失败: {e}");
    }
    let _ = merge_recent_workspaces_from_default();
    if !salvage_headers.is_empty() {
        let _ = import_composer_headers(&shared_db, &salvage_headers);
    }
    if !salvage_kv.is_empty() {
        let _ = import_cursor_disk_kv(&shared_db, &salvage_kv);
    }
    // Bring the default dir's Composer history back in (profiles re-flow on
    // their next launch via prepare_profile_shared).
    let _ = sync_default_composer_to_shared();

    targets.push(CursorDbSlimTarget {
        label: "shared".into(),
        path: shared_db.to_string_lossy().to_string(),
        action: "rebuilt".into(),
        before_bytes: before,
        after_bytes: state_db_cluster_bytes(&shared_db),
        note,
    });
}

fn slim_cursor_state_dbs_sync() -> Result<CursorDbSlimReport, String> {
    let mut targets: Vec<CursorDbSlimTarget> = Vec::new();

    // VACUUM / rebuild must not race a live Cursor holding the DBs open.
    if is_cursor_process_running() {
        quit_cursor_sync()?;
    }

    rebuild_shared_state_db(&mut targets);

    let default_db = default_user_dir().join("globalStorage").join("state.vscdb");
    vacuum_target("default", &default_db, &mut targets);

    // Every account profile keeps its own isolated state.vscdb (login state);
    // VACUUM only reclaims free pages inside each one.
    if let Ok(profiles_root) = profiles_root_dir() {
        if let Ok(entries) = fs::read_dir(&profiles_root) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                let db = live_state_vscdb_in(&entry.path());
                let label = format!("profile:{}", entry.file_name().to_string_lossy());
                vacuum_target(&label, &db, &mut targets);
            }
        }
    }

    let freed_bytes: u64 = targets
        .iter()
        .map(|t| t.before_bytes.saturating_sub(t.after_bytes))
        .sum();
    let has_failure = targets.iter().any(|t| t.action == "failed");
    let message = if has_failure {
        format!(
            "部分数据库瘦身失败（详见 targets），已回收 {}",
            format_bytes(freed_bytes)
        )
    } else {
        format!("数据库瘦身完成，共回收 {}", format_bytes(freed_bytes))
    };
    Ok(CursorDbSlimReport {
        targets,
        freed_bytes,
        message,
    })
}

#[tauri::command]
pub async fn slim_cursor_state_dbs() -> Result<CursorDbSlimReport, String> {
    tokio::task::spawn_blocking(slim_cursor_state_dbs_sync)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn dir_size(path: &Path) -> u64 {
    let mut budget = DirSizeBudget {
        deadline: Instant::now() + Duration::from_millis(DIR_SIZE_SCAN_TIMEOUT_MS),
        remaining: DIR_SIZE_SCAN_MAX_ENTRIES,
    };
    dir_size_budget(path, &mut budget, true)
}

/// Size only the intentional shared layer (settings/extensions fallback under cursor-shared).
fn shared_layer_bytes(shared_path: &Path) -> u64 {
    let mut budget = DirSizeBudget {
        deadline: Instant::now() + Duration::from_millis(2500),
        remaining: 80_000,
    };
    let user = shared_path.join("User");
    let mut size = 0u64;
    for name in ["settings.json", "keybindings.json"] {
        size += file_size(&user.join(name));
    }
    size += file_size(&user.join("globalStorage").join("storage.json"));
    if let Ok(entries) = fs::read_dir(user.join("globalStorage")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("state.vscdb") {
                continue;
            }
            let path = entry.path();
            if path.is_file() {
                size += file_size(&path);
            } else if path.is_dir() {
                size += dir_size_budget(&path, &mut budget, false);
            }
        }
    }
    size += dir_size_budget(&shared_path.join("extensions"), &mut budget, false);
    size
}

struct DirSizeBudget {
    deadline: Instant,
    remaining: u32,
}

fn dir_size_budget(path: &Path, budget: &mut DirSizeBudget, skip_heavy_caches: bool) -> u64 {
    if budget.remaining == 0 || Instant::now() >= budget.deadline {
        return 0;
    }
    let mut size = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        if budget.remaining == 0 || Instant::now() >= budget.deadline {
            break;
        }
        budget.remaining = budget.remaining.saturating_sub(1);
        // Never measure through junctions / symlinks: a profile links into the
        // default Cursor data, which must neither be counted nor followed.
        if is_reparse_point(&entry.path()) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("state.vscdb") && skip_heavy_caches {
                continue;
            }
            size += metadata.len();
        } else if metadata.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if skip_heavy_caches
                && matches!(
                    name.as_ref(),
                    "Cache"
                        | "CachedData"
                        | "CachedConfigurations"
                        | "CachedExtensionVSIXs"
                        | "Code Cache"
                        | "GPUCache"
                        | "ShaderCache"
                        | "DawnCache"
                        | "logs"
                        | "Crashpad"
                        | "History"
                        | "workspaceStorage"
                        | "WebStorage"
                )
            {
                continue;
            }
            size += dir_size_budget(&entry.path(), budget, skip_heavy_caches);
        }
    }
    size
}

fn wait_for_login(expected_email: &str, timeout_ms: u64) -> Result<CursorLoginStatus, String> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let expected = expected_email.trim().to_lowercase();
    let mut last = CursorLoginStatus {
        email: String::new(),
        name: String::new(),
        is_logged_in: false,
    };
    while Instant::now() < deadline {
        if let Ok(status) = get_cursor_login_status_sync() {
            last = status.clone();
            if status.is_logged_in {
                let live = status.email.trim().to_lowercase();
                if expected.is_empty() || live == expected || live.contains(&expected) || expected.contains(&live) {
                    return Ok(status);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
    Err(format!(
        "等待登录超时：期望 {}，最后读到 {}（logged_in={}）",
        expected_email, last.email, last.is_logged_in
    ))
}

/// CLI self-test: migrate snapshots → switch company ↔ personal via profile mode.
pub fn run_cursor_switch_self_test(personal_id: &str, company_id: &str) -> Result<String, String> {
    let mut lines = Vec::new();
    let mut log = |msg: String| {
        eprintln!("[switch-test] {}", msg);
        lines.push(msg);
    };

    let personal = inspect_cursor_backup(personal_id.to_string())?;
    let company = inspect_cursor_backup(company_id.to_string())?;
    if !personal.complete {
        return Err(format!("个人账号快照不可用: {}", personal.reason));
    }
    if !company.complete {
        return Err(format!("公司账号快照不可用: {}", company.reason));
    }
    log(format!(
        "快照 OK — 个人: {} | 公司: {}",
        personal.auth_email, company.auth_email
    ));

    ensure_shared_workspace_migration()?;
    log("shared 工作区迁移完成".to_string());

    for (label, id) in [("个人", personal_id), ("公司", company_id)] {
        let profile = cursor_profile_dir(id)?;
        if !is_profile_initialized_at(&profile) {
            migrate_backup_to_profile_impl(id)?;
            log(format!("{} ({}) 已迁移到独立 profile", label, id));
        } else {
            prepare_profile_shared(&profile)?;
            log(format!("{} ({}) profile 已链接 shared", label, id));
        }
    }

    let default_ws = default_user_dir().join("workspaceStorage");
    if !default_ws.exists() {
        fs::create_dir_all(&default_ws).map_err(|e| format!("创建 default workspaceStorage 失败: {}", e))?;
    }
    for id in [personal_id, company_id] {
        let link = cursor_profile_dir(id)?.join("User").join("workspaceStorage");
        if !link.exists() {
            return Err(format!("profile {} 未链接 workspaceStorage", id));
        }
        if !is_reparse_point(&link) {
            log(format!("  提示: profile {} workspaceStorage 非 junction（可能为首次合并）", id));
        } else if let (Ok(link_target), Ok(default_target)) =
            (fs::canonicalize(&link), fs::canonicalize(&default_ws))
        {
            if link_target != default_target {
                return Err(format!(
                    "profile {} workspaceStorage 未指向 default Cursor（{} != {}）",
                    id,
                    link_target.display(),
                    default_target.display()
                ));
            }
        }
    }
    log("default workspaceStorage 链接检查通过".to_string());

    if std::env::var("WT_SKIP_SWITCH").is_ok() {
        log("WT_SKIP_SWITCH=1，跳过实际切换".to_string());
        return Ok(lines.join("\n"));
    }

    log("切换 公司 → 个人…".to_string());
    let r1 = switch_cursor_account_impl(
        None,
        personal_id.to_string(),
        Some(company_id.to_string()),
        Some(true),
    )?;
    log(format!("  结果: {}", r1));
    let login1 = wait_for_login(&personal.auth_email, 45_000)?;
    log(format!(
        "  登录验证: {} ({})",
        login1.email, login1.name
    ));

    log("切换 个人 → 公司…".to_string());
    let r2 = switch_cursor_account_impl(
        None,
        company_id.to_string(),
        Some(personal_id.to_string()),
        Some(true),
    )?;
    log(format!("  结果: {}", r2));
    let login2 = wait_for_login(&company.auth_email, 45_000)?;
    log(format!(
        "  登录验证: {} ({})",
        login2.email, login2.name
    ));

    log("全部通过 ✓".to_string());
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod account_id_tests {
    use super::validate_account_id;

    #[test]
    fn rejects_traversal_and_separators() {
        assert!(validate_account_id("../x").is_err());
        assert!(validate_account_id("a/b").is_err());
        assert!(validate_account_id("a\\b").is_err());
        assert!(validate_account_id("").is_err());
    }

    #[test]
    fn accepts_simple_id() {
        assert!(validate_account_id("acc-123").is_ok());
    }
}
