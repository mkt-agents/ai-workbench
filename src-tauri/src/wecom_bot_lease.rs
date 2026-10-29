//! Per-Windows-user, per-Bot process lease. No credential or owner PID is stored.
//! The empty file may persist; exclusivity is the live OS handle, not existence.
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::path::Path;

pub struct BotLease {
    _file: File,
}

impl BotLease {
    pub fn acquire(bot_id: &str) -> Result<Self, String> {
        let root = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| "WECOM_LEASE_DIRECTORY: LOCALAPPDATA 未设置".to_string())?;
        Self::acquire_in(
            &Path::new(&root).join("com.ai-workbench.app/wecom-bot-leases"),
            bot_id,
        )
    }

    fn acquire_in(root: &Path, bot_id: &str) -> Result<Self, String> {
        std::fs::create_dir_all(root)
            .map_err(|_| "WECOM_LEASE_DIRECTORY: 创建机器人占用目录失败".to_string())?;
        let name = format!("{:x}.lock", Sha256::digest(bot_id.as_bytes()));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        #[cfg(not(windows))]
        return Err("WECOM_LEASE_PLATFORM: 仅支持 Windows 文件句柄锁".into());
        #[cfg(windows)]
        {
            let file = options.open(root.join(name)).map_err(|e| {
                if e.raw_os_error() == Some(32) || e.raw_os_error() == Some(33) {
                    "WECOM_BOT_OCCUPIED: 当前用户的此机器人已被另一通道占用（进程未知），请先停止该通道".to_string()
                } else {
                    "WECOM_LEASE_OPEN: 获取机器人占用句柄失败".to_string()
                }
            })?;
            Ok(Self { _file: file })
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn same_bot_exclusive_different_bot_independent_release_reacquires() {
        let root = std::env::temp_dir().join(format!(
            "wecom-lease-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let first = BotLease::acquire_in(&root, "fixture-bot-a").unwrap();
        assert!(BotLease::acquire_in(&root, "fixture-bot-a")
            .err()
            .unwrap()
            .starts_with("WECOM_BOT_OCCUPIED:"));
        let other = BotLease::acquire_in(&root, "fixture-bot-b").unwrap();
        drop(first);
        let next = BotLease::acquire_in(&root, "fixture-bot-a").unwrap();
        drop((next, other));
        let entries: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.metadata().unwrap().len() == 0
            && !e.file_name().to_string_lossy().contains("fixture")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
