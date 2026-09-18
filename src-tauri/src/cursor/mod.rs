use std::collections::HashMap;
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

/// Legacy `cursor-shared` layer path. Not created on read: after the
/// single-source migration it is deleted and must stay deleted.
fn cursor_shared_dir() -> PathBuf {
    app_data_dir().map(|d| d.join("cursor-shared")).unwrap_or_default()
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
    // Try in "unlink the mount point" order: `RemoveDirectoryW` drops a
    // junction — live or dangling — without touching its target, while
    // `DeleteFileW` on one fails with ERROR_ACCESS_DENIED. A dangling junction
    // reports `is_dir() == false` (that follows the link) and `is_dir() ==
    // false` from its own metadata too, so no attribute check can pick for us.
    if fs::remove_dir(path).is_ok() {
        return Ok(());
    }
    let link = path.to_string_lossy().to_string();
    if let Ok(output) = hidden_command("cmd").args(["/c", "rmdir", &link]).output() {
        if output.status.success() {
            return Ok(());
        }
    }
    if !is_reparse_point(path) && !path.exists() {
        return Ok(()); // someone already removed it
    }
    fs::remove_file(path)
        .map(|_| ())
        .map_err(|e| format!("移除链接 {} 失败: {}", path.display(), e))
}

fn remove_path_for_link(path: &Path) -> Result<(), String> {
    // Reparse check first: a dangling link does not `exist()` yet must still be
    // removed, or nothing can take its name again.
    if is_reparse_point(path) {
        return remove_link(path);
    }
    if !path.exists() {
        return Ok(());
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
    // Reparse first: a dangling junction does not `exist()` but must still be
    // unlinked, or the tree holding it can never be removed.
    if is_reparse_point(path) {
        return remove_link(path);
    }
    if !path.exists() {
        return Ok(());
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
    // A dangling junction (its target was deleted) reports exists() == false
    // yet still occupies the name, so probe the reparse point first.
    if is_reparse_point(link) {
        let same_target = match (fs::canonicalize(link), fs::canonicalize(target)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if same_target {
            return Ok(());
        }
        // Stale or dangling junction pointing elsewhere — recreate
        remove_path_for_link(link)?;
    } else if link.exists() {
        if link.is_dir() {
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

// ============================================================================
// Single-source profile shells.
//
// %APPDATA%\Cursor\User is the ONLY real data source (workspaces, sessions,
// settings, extensions). Account profiles are thin `--user-data-dir` shells
// whose User directory is a junction into the default one; per-account login
// is swapped into the shared state.vscdb as cursorAuth/* keys only.
// ============================================================================

/// Serializes old-layout → shell conversion (see `convert_profile_to_shell`).
static SHELL_CONVERT_LOCK: Mutex<()> = Mutex::new(());

/// Drop junction links inside a real globalStorage dir before the whole User
/// tree is renamed away — unlink first so links are never walked into.
fn unlink_children_in_global_storage(profile: &Path) {
    let gs = profile.join("User").join("globalStorage");
    let Ok(entries) = fs::read_dir(&gs) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_reparse_point(&path) {
            let _ = remove_link(&path);
        }
    }
}

/// Persist a profile's login into its auth.json slot before its state.vscdb
/// stops existing, so single-source switching keeps a usable snapshot.
fn save_profile_auth_snapshot(account_id: &str, profile: &Path) {
    let Ok(backup) = backup_dir(account_id) else {
        return;
    };
    let Ok(snapshot) = read_auth_snapshot_from_root(profile) else {
        return;
    };
    if snapshot.email.is_empty() {
        return;
    }
    if let Ok(existing) = load_auth_snapshot(&backup) {
        if existing.email == snapshot.email {
            return; // slot already holds this account's auth
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(&snapshot) {
        let _ = fs::write(backup.join("auth.json"), json);
    }
}

/// Merge a profile's real `globalStorage` entries into the single source,
/// skipping the whole `state.vscdb*` family: sessions have already flowed
/// across via `copy_composer_tables` and the login via `auth.json`. Copying a
/// foreign `-wal`/`-shm` sidecar next to the default DB would have SQLite
/// apply a WAL from another database — corruption, not just staleness.
fn merge_global_storage_excluding_login_db(profile_user: &Path, default_user: &Path) -> Result<(), String> {
    let src = profile_user.join("globalStorage");
    let dst = default_user.join("globalStorage");
    if !src.is_dir() || is_reparse_point(&src) {
        return Ok(());
    }
    fs::create_dir_all(&dst).map_err(|e| format!("创建 default globalStorage 失败: {}", e))?;
    for entry in fs::read_dir(&src).map_err(|e| format!("读取 globalStorage 失败: {}", e))? {
        let entry = entry.map_err(|e| format!("读取 globalStorage 项失败: {}", e))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("state.vscdb") {
            continue;
        }
        let dst_child = dst.join(name);
        if dst_child.exists() {
            continue;
        }
        let child_src = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            merge_tree_into(&child_src, &dst_child)?;
        } else if child_src.is_file() {
            copy_file_retry(&child_src, &dst_child)?;
        }
    }
    Ok(())
}

/// Convert an old-layout profile (real User data + per-copy sessions) into a
/// junction shell over the default Cursor dir. Everything unique is merged
/// into the default first; the User tree is renamed to `User.pre-shell-backup`
/// rather than deleted so a problem is recoverable by hand.
fn convert_profile_to_shell(account_id: Option<&str>, profile: &Path) -> Result<(), String> {
    // The startup migration and a user-initiated launch can both reach this,
    // and a half-renamed User tree is unrecoverable — serialize.
    let _guard = SHELL_CONVERT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let user = profile.join("User");
    if !user.exists() || is_reparse_point(&user) {
        return Ok(());
    }

    // globalStorage/* sublinks created by the old scheme point at the default
    // dir already; drop them so the rename below sees a plain tree.
    unlink_children_in_global_storage(profile);

    let profile_db = user.join("globalStorage").join("state.vscdb");
    if profile_db.is_file() {
        if let Some(id) = account_id {
            save_profile_auth_snapshot(id, profile);
        }
        // Sessions created inside this profile flow into the single source.
        let default_db = default_user_dir().join("globalStorage").join("state.vscdb");
        copy_composer_tables(&profile_db, &default_db)?;
    }

    let default_user = default_user_dir();
    fs::create_dir_all(&default_user).map_err(|e| format!("创建 default User 失败: {}", e))?;
    for name in SHARED_WORKSPACE_DIR_NAMES {
        merge_missing_subdirs(&user.join(name), &default_user.join(name))?;
    }
    merge_global_storage_excluding_login_db(&user, &default_user)?;
    for name in ["settings.json", "keybindings.json"] {
        let src = user.join(name);
        let dst = default_user.join(name);
        if src.is_file() && !dst.exists() {
            copy_file_retry(&src, &dst)?;
        }
    }

    let backup_user = profile.join("User.pre-shell-backup");
    if backup_user.exists() {
        let _ = remove_tree_removing_links(&backup_user);
    }
    fs::rename(&user, &backup_user).map_err(|e| format!("封存旧 profile User 失败: {}", e))?;
    ensure_dir_junction(&user, &default_user)?;
    Ok(())
}

/// Make a profile a thin shell: User junctions onto the default dir and the
/// extensions dir onto the shared install. Runs on every managed launch.
fn ensure_profile_shell(account_id: Option<&str>, profile: &Path) -> Result<(), String> {
    convert_profile_to_shell(account_id, profile)?;
    let default_root = cursor_data_dir();
    ensure_dir_junction(&profile.join("User"), &default_root.join("User"))?;

    // A profile can still hold a real extensions dir after its User was
    // converted (older builds linked it separately): fold it into the single
    // source before replacing it with a junction, so nothing is dropped.
    let ext = profile.join("extensions");
    if ext.is_dir() && !is_reparse_point(&ext) {
        merge_missing_subdirs(&ext, &default_root.join("extensions"))?;
        let _ = remove_tree_removing_links(&ext);
    }
    ensure_dir_junction(&ext, &default_root.join("extensions"))?;
    Ok(())
}

fn profiles_root_dir() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("cursor-profiles"))
}

/// Fold anything unique to the old `cursor-shared` layer back into the default
/// Cursor dir, then drop the layer — single-source has no second copy to keep
/// in step. Best-effort: a locked file must not abort startup.
fn drop_legacy_shared_layer() -> Result<CursorCleanupResult, String> {
    let shared = cursor_shared_dir();
    if !shared.exists() {
        return Ok(CursorCleanupResult {
            removed_files: 0,
            freed_bytes: 0,
            message: "无共享层需要清理".into(),
        });
    }
    let default_user = default_user_dir();
    let shared_user = shared.join("User");
    fs::create_dir_all(&default_user).map_err(|e| format!("创建 default User 失败: {}", e))?;

    let mut freed = 0u64;
    for name in SHARED_WORKSPACE_DIR_NAMES {
        let src = shared_user.join(name);
        freed += dir_size(&src);
        let _ = merge_missing_subdirs(&src, &default_user.join(name));
    }
    // The shared login DB is a synced copy: sessions flow into the single
    // source, its `state.vscdb*` files never merge (foreign WAL = corruption).
    let shared_db = shared_user.join("globalStorage").join("state.vscdb");
    let default_db = default_user.join("globalStorage").join("state.vscdb");
    if shared_db.is_file() {
        let _ = copy_composer_tables(&shared_db, &default_db);
    }
    freed += dir_size(&shared_user.join("globalStorage"));
    let _ = merge_global_storage_excluding_login_db(&shared_user, &default_user);
    let _ = remove_tree_removing_links(&shared);

    Ok(CursorCleanupResult {
        removed_files: 1,
        freed_bytes: freed,
        message: "已合并并移除旧共享层".into(),
    })
}

fn schedule_drop_legacy_shared_layer() {
    std::thread::spawn(|| {
        if let Err(e) = drop_legacy_shared_layer() {
            eprintln!("[cursor] drop legacy shared layer: {e}");
        }
    });
}

/// Startup hook: heal legacy damage, then convert every profile to a junction
/// shell over the default Cursor dir and retire the shared layer.
pub fn ensure_shared_workspace_migration() -> Result<(), String> {
    // Heal BLOB-typed values left by the old shared-state sync (black-screen fix).
    let default_db = default_user_dir().join("globalStorage").join("state.vscdb");
    if let Err(e) = repair_blob_typed_state(&default_db) {
        eprintln!("[cursor] default state type repair: {e}");
    }

    let profiles_root = profiles_root_dir()?;
    if profiles_root.exists() && !is_cursor_process_running() {
        for entry in fs::read_dir(&profiles_root).map_err(|e| format!("读取 profiles 失败: {}", e))? {
            let entry = entry.map_err(|e| format!("读取 profile 项失败: {}", e))?;
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let profile = entry.path();
            let id = account_id_from_profile_path(&profile);
            if let Err(e) = ensure_profile_shell(id.as_deref(), &profile) {
                eprintln!("[cursor] shell convert {}: {e}", profile.display());
            }
        }
    }
    schedule_drop_legacy_shared_layer();

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

// ============================================================================
// Composer session history sharing (composerHeaders + cursorDiskKV tables).
//
// Prior sync logic only copied ItemTable. Cursor's actual Composer session
// list lives in two other tables inside the same state.vscdb:
//   - composerHeaders: one row per Composer session (sidebar list source)
//   - cursorDiskKV: session payload and message bodies (see the whitelist
//     below)
// Without syncing these, a profile launched via `--user-data-dir` starts with
// an empty sidebar and any new session created inside it never flows back to
// the default Cursor dir or other profiles. `copy_composer_tables` wires the
// same default → shared → profile pipeline used for ItemTable.
// ============================================================================

/// Prefixes inside cursorDiskKV that should be shared across profiles.
/// - `composerData:<uuid>`: Composer session payload (metadata / fullRows)
/// - `composer.content.<hash>`: content fragments referenced by Composer
/// - `bubbleId:<composerId>:<bubbleId>`: individual message bodies. Since the
///   agent-native UI these render the session content — excluding them made
///   synced sessions show as empty shells (headers only, no messages).
/// - `checkpointId:<composerId>:<hash>`: session checkpoint snapshots, keyed
///   by the same composerIds the sidebar already lists.
/// Other prefixes (agentKv, inlineDiff, ...) stay skipped: agentKv is a
/// multi-GB derived cache and inline diffs are bulky temporaries.
const SHARED_CURSOR_KV_PREFIXES: &[&str] = &[
    "composerData:",
    "composer.content.",
    "bubbleId:",
    "checkpointId:",
];

fn shared_kv_glob_clause() -> String {
    SHARED_CURSOR_KV_PREFIXES
        .iter()
        .map(|p| format!("key GLOB '{}*'", p.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Attach `path` as a queryable schema under `alias`. Plain path form (no
/// `mode=ro`): a read-only attach fails on sources with a live WAL sidecar,
/// which is exactly the case when Cursor crashed without checkpointing.
fn attach_db(conn: &Connection, path: &Path, alias: &str) -> Result<(), String> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS {alias};",
        normalized.replace('\'', "''")
    ))
    .map_err(|e| format!("附加数据库失败 {}: {}", path.display(), e))
}

fn src_has_table(conn: &Connection, alias: &str, table: &str) -> bool {
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM {alias}.sqlite_master WHERE type='table' AND name='{table}'"
        ),
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
    > 0
}

/// Stream composerHeaders src→dst in one SQL statement. Upsert keyed on
/// composerId; the `IS NOT` guards skip no-op writes so unchanged sources
/// leave the destination fingerprint alone (keeps sync markers effective).
fn copy_composer_headers(conn: &Connection) -> Result<(), String> {
    if !src_has_table(conn, "src", "composerHeaders") {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS main.composerHeaders (\
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
         ) WITHOUT ROWID;
         INSERT INTO main.composerHeaders AS dst (composerId, workspaceId, createdAt, \
                 lastUpdatedAt, isArchived, isSubagent, recency, checkpointAt, value, subagentTypeName)
             SELECT composerId, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent, \
                    recency, checkpointAt, value, subagentTypeName
             FROM src.composerHeaders
             ON CONFLICT(composerId) DO UPDATE SET
                 workspaceId = excluded.workspaceId,
                 createdAt = excluded.createdAt,
                 lastUpdatedAt = excluded.lastUpdatedAt,
                 isArchived = excluded.isArchived,
                 isSubagent = excluded.isSubagent,
                 recency = excluded.recency,
                 checkpointAt = excluded.checkpointAt,
                 value = excluded.value,
                 subagentTypeName = excluded.subagentTypeName
             WHERE dst.lastUpdatedAt IS NOT excluded.lastUpdatedAt
                OR dst.value IS NOT excluded.value;",
    )
    .map_err(|e| format!("流式同步 composerHeaders 失败: {}", e))
}

/// Stream whitelisted cursorDiskKV rows src→dst (ATTACH + INSERT..SELECT).
/// Never materializes values in Rust memory: bubble rows span 100k+ entries
/// and hundreds of MB, which the old export-HashMap-then-insert path copied
/// row-by-row through the process heap on every launch.
fn copy_cursor_disk_kv(conn: &Connection) -> Result<(), String> {
    if !src_has_table(conn, "src", "cursorDiskKV") {
        return Ok(());
    }
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS main.cursorDiskKV (key TEXT PRIMARY KEY, value BLOB) WITHOUT ROWID;
         INSERT INTO main.cursorDiskKV (key, value)
             SELECT key, value FROM src.cursorDiskKV WHERE {}
             ON CONFLICT(key) DO UPDATE SET value = excluded.value
             WHERE main.cursorDiskKV.value IS NOT excluded.value;",
        shared_kv_glob_clause()
    );
    conn.execute_batch(&sql)
        .map_err(|e| format!("流式同步 cursorDiskKV 失败: {}", e))
}

/// One-way streaming sync of the Composer tables (headers + whitelisted
/// cursorDiskKV rows) between two state.vscdb files.
fn copy_composer_tables(src_db: &Path, dst_db: &Path) -> Result<(), String> {
    if !src_db.exists() {
        return Ok(());
    }
    if let Some(parent) = dst_db.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    let conn = Connection::open(dst_db).map_err(|e| format!("打开数据库失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA busy_timeout=8000; PRAGMA synchronous=NORMAL;");
    attach_db(&conn, src_db, "src")?;
    let result = copy_composer_headers(&conn).and_then(|()| copy_cursor_disk_kv(&conn));
    let _ = conn.execute_batch("DETACH DATABASE src;");
    result?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
}

/// Read a Cursor state.vscdb that may be live (Cursor running, WAL mode).
///
/// A `mode=ro` open is deliberately *not* used: SQLite refuses a read-only
/// connection to a WAL database while another process owns its shared-memory
/// file ("unable to open database file"), which made the login unreadable
/// exactly when Cursor was open — and the copy-to-temp fallback is capped far
/// below a real session DB. Opening normally with `query_only` takes only read
/// locks, so nothing can be written.
fn open_state_db_for_read(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("打开数据库失败 {}: {}", path.display(), e))?;
    conn.execute_batch("PRAGMA busy_timeout=8000; PRAGMA query_only=ON;")
        .map_err(|e| format!("初始化只读连接失败: {}", e))?;
    Ok(conn)
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
    let conn = open_state_db_for_read(path)?;
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

fn migrate_backup_to_profile_impl(account_id: &str) -> Result<PathBuf, String> {
    let profile = cursor_profile_dir(account_id)?;
    ensure_profile_shell(Some(account_id), &profile)?;
    restore_into_data_dir(account_id, &profile)?;
    mark_profile_initialized_at(&profile)?;
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

/// Remove every stored login from the single shared state.vscdb. Used when a
/// slot has no usable snapshot: without this the new window would inherit (and
/// then re-capture) whichever account happened to be signed in before.
fn clear_auth_in_data_root(data_root: &Path) -> Result<(), String> {
    let db = live_state_vscdb_in(data_root);
    if !db.exists() {
        return Ok(());
    }
    let conn = Connection::open(&db).map_err(|e| format!("打开 state.vscdb 失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA busy_timeout=8000; PRAGMA synchronous=NORMAL;");
    conn.execute(
        "DELETE FROM ItemTable WHERE key LIKE 'cursorAuth/%' \
         OR key IN ('glass.lastSignedInAuthId', 'adminSettings.cachedAuthId')",
        [],
    )
    .map_err(|e| format!("清除登录态失败: {}", e))?;
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
}

/// Point the single shared Cursor data dir at one account: write its
/// `cursorAuth/*` keys into the shared state.vscdb and refresh the profile's
/// own Chromium session files. `restore_into_data_dir` reaches the shared DB
/// through the profile's `User` junction, so profile == default storage here.
fn apply_account_login(account_id: &str, profile: &Path) -> Result<bool, String> {
    let backup = backup_dir(account_id)?;
    let status = inspect_backup_dir(account_id, &backup);
    if !status.complete {
        return Ok(false);
    }
    restore_into_data_dir(account_id, profile)?;
    Ok(true)
}

/// The account's email: the caller's copy wins while the row is still being
/// created, otherwise the persisted one.
fn known_account_email(account_id: &str, expected: Option<&str>) -> Option<String> {
    expected
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(str::to_string)
        .or_else(|| account_email_in_db(account_id))
}

/// The account's own email, as the user entered it on the Cursor page.
fn account_email_in_db(account_id: &str) -> Option<String> {
    let db_path = app_data_dir().ok()?.join("ai-workbench.db");
    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row(
        "SELECT email FROM cursor_accounts WHERE id = ?1",
        rusqlite::params![account_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// Which account slot already claims this email.
fn owner_account_for_email(email: &str) -> Option<String> {
    let root = backups_root_dir().ok()?;
    for entry in fs::read_dir(&root).ok()?.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if let Ok(snapshot) = load_auth_snapshot(&entry.path()) {
            if emails_match(&snapshot.email, email) {
                return Some(id);
            }
        }
    }
    None
}

/// The shared DB holds exactly one session. Before it is replaced or cleared,
/// copy it back into a slot so launching another profile can never destroy the
/// only live copy of a login. Errs with that address when it cannot be
/// attributed, so the caller leaves it alone instead of signing out silently.
fn preserve_current_login() -> Result<(), String> {
    // Cookies are per user-data-dir even though the DB is shared: snapshot from
    // wherever Cursor actually runs, falling back to the owner's profile.
    let root = resolve_data_dir();
    let live = match read_auth_snapshot_from_root(&root) {
        Ok(live) => live,
        Err(_) => return Ok(()),
    };
    if live.email.is_empty() {
        return Ok(());
    }
    let owner = owner_account_for_email(&live.email)
        .or_else(get_active_account_id)
        .ok_or_else(|| live.email.clone())?;
    if let Err(e) = backup_cursor_data_from_root(&owner, &root) {
        eprintln!("[cursor] 从 {} 保留登录态失败: {e}", root.display());
        let profile = cursor_profile_dir(&owner)?;
        backup_cursor_data_from_root(&owner, &profile)?;
    }
    Ok(())
}

fn launch_cursor_impl(
    window: Option<&tauri::Window>,
    account_id: Option<String>,
    expected_email: Option<String>,
) -> Result<String, String> {
    let exe = cursor_exe_path()?;
    let exe_str = exe.to_string_lossy().to_string();

    if let Some(ref id) = account_id {
        if is_cursor_process_running() {
            emit_switch_progress(window, "quit", "正在关闭当前 Cursor…");
            quit_cursor_sync()?;
        }
        let profile = cursor_profile_dir(id)?;
        fs::create_dir_all(&profile).map_err(|e| format!("创建 profile 目录失败: {}", e))?;
        emit_switch_progress(window, "sync", "正在链接共享工作区与会话…");
        ensure_profile_shell(Some(id), &profile)?;

        // The shared DB may already hold the login the user just typed in this
        // account's window; treating that as "someone else's session" would
        // clear it and send them into a login loop. The caller knows the email
        // during a first init, before the account row is persisted.
        let live_email = read_auth_snapshot_from_root(&profile)
            .map(|snap| snap.email)
            .unwrap_or_default();
        let already_here = !live_email.is_empty()
            && known_account_email(id, expected_email.as_deref())
                .map(|email| emails_match(&email, &live_email))
                .unwrap_or(false);

        emit_switch_progress(window, "restore", "正在应用账号登录态…");
        if already_here {
            set_active_account(Some(id));
        } else {
            // The session being displaced must have somewhere to go first, or a
            // switch would silently discard the only copy of someone's login.
            preserve_current_login().map_err(|who| format!(
                "当前登录的 {who} 还没有任何快照，已停止切换以免丢失它的登录态。请先对该账号「完成初始化」，或在 Cursor 中退出登录后重试。"
            ))?;
            if apply_account_login(id, &profile)? {
                set_active_account(Some(id));
            } else {
                // Nothing usable for this account: open signed out rather than
                // as somebody else.
                clear_auth_in_data_root(&profile)?;
                set_active_account(Some(id));
                emit_switch_progress(
                    window,
                    "restore",
                    "该账号尚无可用登录快照，将以未登录状态打开",
                );
            }
        }
        // A previous build's sync may have left BLOB-typed values; Cursor's
        // workbench aborts on them with a JSON.parse error (black screen).
        if let Err(e) = repair_blob_typed_state(&live_state_vscdb_in(&profile)) {
            eprintln!("[cursor] state type repair: {e}");
        }

        emit_switch_progress(window, "launch", "正在启动 Cursor…");
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
pub async fn launch_cursor(
    window: tauri::Window,
    account_id: Option<String>,
    expected_email: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        launch_cursor_impl(Some(&window), account_id, expected_email)
    })
    .await
    .map_err(|e| format!("启动任务异常: {}", e))?
}

#[tauri::command]
pub fn get_cursor_profile_dir(account_id: String) -> Result<String, String> {
    Ok(cursor_profile_dir(&account_id)?.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn init_account_profile(
    window: tauri::Window,
    account_id: String,
    expected_email: Option<String>,
) -> Result<String, String> {
    let window = window.clone();
    tokio::task::spawn_blocking(move || {
        init_account_profile_impl(&window, account_id, expected_email)
    })
    .await
    .map_err(|e| format!("初始化任务异常: {}", e))?
}

fn init_account_profile_impl(
    window: &tauri::Window,
    account_id: String,
    expected_email: Option<String>,
) -> Result<String, String> {
    let _profile = cursor_profile_dir(&account_id)?;
    let already_this_profile = get_active_account_id().as_deref() == Some(account_id.as_str());

    if is_cursor_process_running() && already_this_profile {
        return Ok(
            "该账号的独立 Cursor 已在运行。请在其中登录，然后回到 AI Workbench 点「完成初始化」。"
                .to_string(),
        );
    }

    // Quit before opening the other profile: Cursor locks its own Cookies, and
    // `launch_cursor_impl` snapshots whoever was signed in before displacing
    // that session.
    if is_cursor_process_running() {
        emit_switch_progress(Some(window), "quit", "正在关闭 Cursor 以打开新账号配置…");
        quit_cursor_sync()?;
    }

    emit_switch_progress(Some(window), "launch", "正在打开独立配置（可能需数十秒）…");
    launch_cursor_impl(Some(window), Some(account_id), expected_email)?;
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

/// Capture login from the single shared data source (the profile's `User` is a
/// junction to it). Say precisely what is wrong: an empty source means nobody
/// has signed in yet, a foreign email means the wrong account is signed in.
fn resolve_finish_capture_root(profile: &Path) -> Result<PathBuf, String> {
    if data_root_has_login(profile) {
        return Ok(profile.to_path_buf());
    }
    let live_email = read_auth_snapshot_from_root(profile)
        .map(|snap| snap.email)
        .unwrap_or_default();
    if live_email.is_empty() {
        return Err(
            "共享数据源里当前没有登录态。请先点「重新打开 Cursor」，在 Cursor 中登录该账号，再点「完成初始化」。"
                .to_string(),
        );
    }
    Err(format!(
        "当前登录的是 {live_email}，不是这个账号。请在 Cursor 中退出并登录目标账号后再点「完成初始化」。"
    ))
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
        // Profiles are junction shells over the single source: never recurse
        // into them with a plain recursive delete.
        let _ = remove_tree_removing_links(&entry.path());
        if let Ok(profile) = cursor_profile_dir(&other_id) {
            let _ = remove_tree_removing_links(&profile);
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
        format!("读取登录态失败: {e}")
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
                    "{first_err}。请确认已在 Cursor 中登录该账号后重试。"
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
    ensure_profile_shell(Some(&account_id), &profile)?;
    set_active_account(Some(&account_id));

    if should_relaunch && !is_cursor_process_running() {
        emit_switch_progress(Some(window), "launch", "正在重新打开 Cursor…");
        launch_cursor_impl(Some(window), Some(account_id.clone()), None)?;
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

/// Switching accounts under single source = swapping the one shared login.
/// Order matters: quit (Cookies unlock) → preserve the outgoing session →
/// apply the target, so no account's token is lost to the overwrite. The
/// outgoing session is attributed by email, so it also covers a Cursor the user
/// opened by hand rather than through this page.
fn switch_cursor_account_impl(
    window: Option<&tauri::Window>,
    target_account_id: String,
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
    let expected = target_status.auth_email.clone();

    // Close Cursor first: its Network/Cookies are locked while it runs, so the
    // outgoing account can only be snapshotted after it exits.
    if is_cursor_process_running() {
        emit_switch_progress(window, "quit", "正在关闭 Cursor…");
        cache_cursor_exe_from_running();
        quit_cursor_sync()?;
    }

    if !should_relaunch {
        emit_switch_progress(window, "save", "正在保存当前账号登录态…");
        preserve_current_login().map_err(|who| format!(
            "当前登录的 {who} 还没有任何快照，已停止切换以免丢失它的登录态。请先对该账号「完成初始化」，或在 Cursor 中退出登录后重试。"
        ))?;
        emit_switch_progress(window, "restore", "正在切换账号登录态…");
        let profile = cursor_profile_dir(&target_account_id)?;
        ensure_profile_shell(Some(&target_account_id), &profile)?;
        if !apply_account_login(&target_account_id, &profile)? {
            return Err("目标账号快照不可用，请重新登录并捕获。".to_string());
        }
        emit_switch_progress(window, "done", "切换完成");
        return Ok(format!("已切换到 {}", expected));
    }

    emit_switch_progress(window, "launch", "正在以新账号打开 Cursor…");
    launch_cursor_impl(window, Some(target_account_id.clone()), Some(expected.clone()))?;

    let live = extract_auth_from_live().unwrap_or_default();
    let matched = expected.is_empty()
        || emails_match(&expected, &live.email)
        || identity_match(&expected, &expected, &live);
    if !matched && !live.email.is_empty() {
        return Err(format!(
            "切换未生效：期望 {expected}，实际仍是 {}。该账号快照可能已失效，请重新登录并捕获。",
            live.email
        ));
    }
    emit_switch_progress(window, "done", "切换完成");
    let switched_email = if live.email.is_empty() { expected } else { live.email };
    Ok(format!("已切换到 {switched_email}"))
}

#[tauri::command]
pub async fn switch_cursor_account(
    window: tauri::Window,
    target_account_id: String,
    current_account_id: Option<String>,
    relaunch: Option<bool>,
) -> Result<String, String> {
    let window = window.clone();
    // Seed the active-account hint from what the page believes is current, so a
    // session whose address is in no slot yet can still be attributed on switch.
    if let Some(id) = current_account_id.as_deref() {
        set_active_account(Some(id));
    }
    tokio::task::spawn_blocking(move || {
        switch_cursor_account_impl(Some(&window), target_account_id, relaunch)
    })
    .await
    .map_err(|e| format!("切换任务异常: {}", e))?
}

#[tauri::command]
pub async fn delete_cursor_backup(account_id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || delete_cursor_backup_impl(account_id))
        .await
        .map_err(|e| format!("删除任务异常: {}", e))?
}

fn delete_cursor_backup_impl(account_id: String) -> Result<(), String> {
    // Profile first, snapshot second: if the profile is busy (Cursor still open
    // on it) the account keeps a usable snapshot instead of ending up
    // half-deleted with an empty slot.
    // The profile holds junctions/symlinks into the default Cursor data
    // (User, extensions) — unlink them without following, then drop the
    // directory itself.
    let slot_email = backup_dir(&account_id)
        .ok()
        .and_then(|backup| load_auth_snapshot(&backup).ok())
        .map(|snapshot| snapshot.email)
        .unwrap_or_default();

    let profile = cursor_profile_dir(&account_id)?;
    remove_tree_removing_links(&profile)?;
    let backup = backup_dir(&account_id)?;
    remove_tree_removing_links(&backup)?;

    if get_active_account_id().as_deref() == Some(account_id.as_str()) {
        set_active_account(None);
    }
    // Under single source the deleted account's session may still be the one
    // loaded in the shared DB. Leaving it would sign the next window back in as
    // a deleted account — and make it unattributable for future preserves.
    if !slot_email.is_empty() {
        let live = read_auth_snapshot_from_root(&cursor_data_dir()).unwrap_or_default();
        if emails_match(&slot_email, &live.email) {
            let _ = clear_auth_in_data_root(&cursor_data_dir());
        }
    }
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
    /// Total size of the sealed `User.pre-shell-backup` trees left behind when
    /// old-layout profiles were converted to junction shells.
    pub sealed_bytes: u64,
    pub sealed_count: u32,
    pub live_db_bytes: u64,
    pub backups_path: String,
    pub live_db_path: String,
}

/// Sealed pre-shell profile data. Each still contains junctions into the single
/// source, so only `remove_tree_removing_links` may touch them.
fn sealed_profile_backups() -> Vec<PathBuf> {
    let Ok(root) = profiles_root_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("User.pre-shell-backup"))
        .filter(|path| path.is_dir() && !is_reparse_point(path))
        .collect()
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

    let sealed = sealed_profile_backups();
    let sealed_bytes: u64 = sealed.iter().map(|p| dir_size(p)).sum();

    Ok(CursorDiskUsage {
        backups_bytes: if backups_path.exists() {
            dir_size(&backups_path)
        } else {
            0
        },
        backups_full_db_bytes,
        stale_db_count,
        sealed_bytes,
        sealed_count: sealed.len() as u32,
        live_db_bytes: file_size(&live_db),
        backups_path: backups_path.to_string_lossy().to_string(),
        live_db_path: live_db.to_string_lossy().to_string(),
    })
}

/// Drop the sealed pre-shell profile trees. Their sessions and login have
/// already been folded into the single source during conversion, so this only
/// removes the safety copies.
fn cleanup_cursor_sealed_backups_sync() -> Result<CursorCleanupResult, String> {
    let sealed = sealed_profile_backups();
    if sealed.is_empty() {
        return Ok(CursorCleanupResult {
            removed_files: 0,
            freed_bytes: 0,
            message: "没有待清理的旧配置封存".into(),
        });
    }
    let mut freed = 0u64;
    let mut removed = 0u32;
    let mut failures: Vec<String> = Vec::new();
    for path in sealed {
        let bytes = dir_size(&path);
        match remove_tree_removing_links(&path) {
            Ok(()) => {
                freed += bytes;
                removed += 1;
            }
            Err(e) => failures.push(e),
        }
    }
    let message = if failures.is_empty() {
        format!("已清理 {removed} 份旧配置封存，释放约 {}", format_bytes(freed))
    } else {
        format!(
            "已清理 {removed} 份，{} 份失败：{}",
            failures.len(),
            failures.join("；")
        )
    };
    Ok(CursorCleanupResult {
        removed_files: removed,
        freed_bytes: freed,
        message,
    })
}

#[tauri::command]
pub async fn cleanup_cursor_sealed_backups() -> Result<CursorCleanupResult, String> {
    tokio::task::spawn_blocking(cleanup_cursor_sealed_backups_sync)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn cleanup_cursor_full_backups_sync() -> Result<CursorCleanupResult, String> {
    let mut removed = 0u32;
    let mut freed = 0u64;
    let mut parts: Vec<String> = Vec::new();

    match drop_legacy_shared_layer() {
        Ok(r) if r.removed_files > 0 => {
            removed += r.removed_files;
            freed += r.freed_bytes;
            parts.push(r.message);
        }
        Ok(_) => {}
        Err(e) => parts.push(format!("共享层清理跳过: {e}")),
    }

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

fn slim_cursor_state_dbs_sync() -> Result<CursorDbSlimReport, String> {
    let mut targets: Vec<CursorDbSlimTarget> = Vec::new();

    // VACUUM / rebuild must not race a live Cursor holding the DBs open.
    if is_cursor_process_running() {
        quit_cursor_sync()?;
    }

    // The legacy shared layer is a redundant copy: fold anything unique into
    // the single source, then drop it.
    if let Ok(r) = drop_legacy_shared_layer() {
        if r.removed_files > 0 {
            targets.push(CursorDbSlimTarget {
                label: "legacy-shared".into(),
                path: cursor_shared_dir().to_string_lossy().to_string(),
                action: "removed".into(),
                before_bytes: r.freed_bytes,
                after_bytes: 0,
                note: r.message,
            });
        }
    }

    let default_db = default_user_dir().join("globalStorage").join("state.vscdb");
    vacuum_target("default", &default_db, &mut targets);

    // Profiles are junction shells over the single source, so their "own"
    // state.vscdb resolves to the same physical file — canonicalize to avoid
    // vacuuming it once per account. Only genuinely separate DBs get a pass.
    if let Ok(profiles_root) = profiles_root_dir() {
        let mut seen: Vec<Option<std::path::PathBuf>> = vec![fs::canonicalize(&default_db).ok()];
        if let Ok(entries) = fs::read_dir(&profiles_root) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    continue;
                }
                let db = live_state_vscdb_in(&entry.path());
                if !db.exists() {
                    continue;
                }
                let canon = fs::canonicalize(&db).ok();
                if seen.iter().any(|s| *s == canon) {
                    continue;
                }
                seen.push(canon);
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
            ensure_profile_shell(Some(id), &profile)?;
            log(format!("{} ({}) profile 壳已指向 default", label, id));
        }
    }

    let default_user = default_user_dir();
    for id in [personal_id, company_id] {
        let link = cursor_profile_dir(id)?.join("User");
        if !link.exists() {
            return Err(format!("profile {} 未链接 User 目录", id));
        }
        if !is_reparse_point(&link) {
            return Err(format!("profile {} 的 User 不是 junction（单一真源未生效）", id));
        }
        if let (Ok(link_target), Ok(default_target)) =
            (fs::canonicalize(&link), fs::canonicalize(&default_user))
        {
            if link_target != default_target {
                return Err(format!(
                    "profile {} User 未指向 default Cursor（{} != {}）",
                    id,
                    link_target.display(),
                    default_target.display()
                ));
            }
        }
    }
    log("单一真源 User 链接检查通过".to_string());

    if std::env::var("WT_SKIP_SWITCH").is_ok() {
        log("WT_SKIP_SWITCH=1，跳过实际切换".to_string());
        return Ok(lines.join("\n"));
    }

    log("切换 公司 → 个人…".to_string());
    let r1 = switch_cursor_account_impl(None, personal_id.to_string(), Some(true))?;
    log(format!("  结果: {}", r1));
    let login1 = wait_for_login(&personal.auth_email, 45_000)?;
    log(format!(
        "  登录验证: {} ({})",
        login1.email, login1.name
    ));

    log("切换 个人 → 公司…".to_string());
    let r2 = switch_cursor_account_impl(None, company_id.to_string(), Some(true))?;
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

#[cfg(all(test, windows))]
mod junction_tests {
    use super::{is_reparse_point, remove_tree_removing_links};
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn make_junction(link: &Path, target: &Path) {
        let out = Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/J",
                &link.to_string_lossy().to_string(),
                &target.to_string_lossy().to_string(),
            ])
            .output()
            .expect("mklink should run");
        assert!(out.status.success(), "mklink failed: {:?}", out);
    }

    /// A profile's `extensions` can outlive its shared-layer target (the layer
    /// is deleted), leaving a dangling junction that reports `exists() ==
    /// false`. Deleting the profile must still unlink it — and must never
    /// reach through a live junction into the single source.
    #[test]
    fn removes_dangling_and_live_junctions_without_touching_targets() {
        let root = std::env::temp_dir().join(format!("aiwb-junction-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let live_target = root.join("live-target");
        fs::create_dir_all(&live_target).unwrap();
        fs::write(live_target.join("keep.txt"), b"data").unwrap();

        let profile = root.join("profile");
        fs::create_dir_all(&profile).unwrap();
        make_junction(&profile.join("extensions"), &root.join("gone-target"));
        make_junction(&profile.join("User"), &live_target);
        assert!(is_reparse_point(&profile.join("extensions")));
        assert!(!profile.join("extensions").exists(), "precondition: dangling");

        // Why remove_link must not pick by attributes: a dangling junction is
        // neither `is_dir()` (that follows the link) nor a directory per its own
        // metadata, and DeleteFileW on it is denied.
        let dangling = profile.join("extensions");
        assert!(!fs::symlink_metadata(&dangling).unwrap().is_dir());
        let by_removal_order = fs::remove_dir(&dangling);
        assert!(
            by_removal_order.is_ok(),
            "RemoveDirectoryW should unlink a dangling junction: {by_removal_order:?}"
        );

        remove_tree_removing_links(&profile).expect("profile tree should be removable");

        assert!(!profile.exists(), "profile dir should be gone");
        assert!(
            live_target.join("keep.txt").exists(),
            "data behind a junction must survive"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
