use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use chrono::Local;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const MAX_BACKUPS: usize = 20;

fn hosts_path() -> PathBuf {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
    Path::new(&windir)
        .join("System32")
        .join("drivers")
        .join("etc")
        .join("hosts")
}

fn backups_dir() -> Result<PathBuf, String> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| "Failed to resolve APPDATA environment variable".to_string())?;
    let dir = Path::new(&appdata).join("com.ai-workbench.app").join("hosts-backups");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create backups dir: {}", e))?;
    Ok(dir)
}

fn hidden_cmd(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn flush_dns() {
    let _ = hidden_cmd("ipconfig").args(["/flushdns"]).output();
}

fn prune_backups(dir: &Path) {
    let mut files: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "bak").unwrap_or(false))
        .collect();
    files.sort_by(|a, b| {
        let ma = fs::metadata(a).and_then(|m| m.modified()).ok();
        let mb = fs::metadata(b).and_then(|m| m.modified()).ok();
        mb.cmp(&ma)
    });
    for old in files.into_iter().skip(MAX_BACKUPS) {
        let _ = fs::remove_file(old);
    }
}

fn path_under_backups(path: &Path) -> Result<(), String> {
    let dir = backups_dir()?;
    let canon_dir = fs::canonicalize(&dir).unwrap_or(dir.clone());
    let canon_path = fs::canonicalize(path).map_err(|e| format!("无效备份路径: {}", e))?;
    if !canon_path.starts_with(&canon_dir) {
        return Err("备份路径不在允许的目录内".to_string());
    }
    if canon_path.extension().map(|e| e != "bak").unwrap_or(true) {
        return Err("仅允许还原 .bak 备份文件".to_string());
    }
    Ok(())
}

fn decode_hosts(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return encoding_rs::UTF_16LE.decode(&bytes[2..]).0.into_owned();
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return encoding_rs::UTF_16BE.decode(&bytes[2..]).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        // Chinese Windows Notepad "ANSI" saves hosts as GBK.
        Err(_) => encoding_rs::GBK.decode(bytes).0.into_owned(),
    }
}

fn encode_hosts(content: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if content.chars().any(|c| !c.is_ascii()) {
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    out.extend_from_slice(content.as_bytes());
    out
}

#[tauri::command]
pub fn read_system_hosts() -> Result<String, String> {
    let path = hosts_path();
    let bytes = fs::read(&path).map_err(|e| format!("Failed to read hosts file: {}", e))?;
    Ok(decode_hosts(&bytes))
}

/// Whether the process can write the system hosts file (elevated), not merely admin group membership.
#[tauri::command]
pub fn is_admin() -> bool {
    let path = hosts_path();
    match fs::OpenOptions::new().write(true).open(&path) {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Snapshot the current hosts file. Internal only: `write_system_hosts` backs up
/// before every write and `restore_host_backup` calls it too, so no IPC entry
/// point is needed.
fn backup_hosts() -> Result<String, String> {
    let path = hosts_path();
    let bytes = fs::read(&path).map_err(|e| format!("Failed to read hosts file: {}", e))?;
    let dir = backups_dir()?;
    let stamp = chrono_stamp();
    let backup_path = dir.join(format!("hosts-{}.bak", stamp));
    fs::write(&backup_path, &bytes).map_err(|e| format!("Failed to write backup: {}", e))?;
    prune_backups(&dir);
    Ok(backup_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn write_system_hosts(content: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || write_system_hosts_sync(content))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

fn write_system_hosts_sync(content: String) -> Result<String, String> {
    let path = hosts_path();
    // Do not truncate the live file if we cannot keep a restorable copy.
    backup_hosts()?;
    let bytes = encode_hosts(&content);
    replace_file_atomic(&path, &bytes)?;
    flush_dns();
    Ok("已写入 hosts 并尝试刷新 DNS".to_string())
}

fn replace_file_atomic(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = dest.with_extension("ai-workbench.tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("写入临时 hosts 失败: {}", e))?;
    let replaced = replace_existing_file(dest, &tmp);
    if replaced.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    replaced
}

fn replace_existing_file(dest: &Path, replacement: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            ReplaceFileW, REPLACEFILE_IGNORE_MERGE_ERRORS, REPLACEFILE_WRITE_THROUGH,
        };
        fn wide(path: &Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }
        let dest_w = wide(dest);
        let repl_w = wide(replacement);
        unsafe {
            ReplaceFileW(
                PCWSTR(dest_w.as_ptr()),
                PCWSTR(repl_w.as_ptr()),
                PCWSTR::null(),
                REPLACEFILE_WRITE_THROUGH | REPLACEFILE_IGNORE_MERGE_ERRORS,
                None,
                None,
            )
            .map_err(|e| format!("替换 hosts 失败: {e}"))?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(replacement, dest).map_err(|e| format!("替换 hosts 失败: {e}"))
    }
}

#[derive(Debug, Serialize)]
pub struct HostBackup {
    pub path: String,
    pub name: String,
}

#[tauri::command]
pub fn list_host_backups() -> Result<Vec<HostBackup>, String> {
    let dir = backups_dir()?;
    let mut result = Vec::new();
    let entries =
        fs::read_dir(&dir).map_err(|e| format!("Failed to read backups dir: {}", e))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().map(|e| e == "bak").unwrap_or(false) {
            result.push(HostBackup {
                path: p.to_string_lossy().to_string(),
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            });
        }
    }
    result.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(result)
}

#[tauri::command]
pub async fn restore_host_backup(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || restore_host_backup_sync(path))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

fn restore_host_backup_sync(path: String) -> Result<String, String> {
    let p = Path::new(&path);
    path_under_backups(p)?;
    let content =
        fs::read_to_string(p).map_err(|e| format!("Failed to read backup: {}", e))?;
    let _ = backup_hosts();
    fs::write(hosts_path(), content).map_err(|e| format!("Failed to restore hosts: {}", e))?;
    flush_dns();
    Ok("已从备份还原并尝试刷新 DNS".to_string())
}

/// Manual on-demand snapshot, exposed so the UI can back up before risky edits.
#[tauri::command]
pub async fn backup_hosts_now() -> Result<String, String> {
    tokio::task::spawn_blocking(backup_hosts)
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

fn chrono_stamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}
