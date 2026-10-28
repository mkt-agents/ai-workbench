//! 企微通道的排障日志（`%APPDATA%\com.ai-workbench.app\wecom.log`）。
//!
//! 之前所有出站失败都是 `let _ = send_raw(...)`，平台拒绝（errcode）也被静默吃掉，
//! 于是「Bot 不回话」只能靠猜。这里只记关键几行（收发 / 闸门 / 引擎 / 错误），
//! 超过 1MB 自动滚一份 `.1`。

use std::io::Write;
use std::path::PathBuf;

const MAX_BYTES: u64 = 1024 * 1024;

fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = base.join("com.ai-workbench.app");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("wecom.log"))
}

/// 追加一行：`12:03:41.512 [ack] msgid=… stream=…`。
pub fn write(scope: &str, message: &str) {
    let Some(path) = log_path() else { return };
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    let line = format!(
        "{} [{}] {}\n",
        chrono::Local::now().format("%m-%d %H:%M:%S%.3f"),
        scope,
        message
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// 读最后 `lines` 行（设置页「查看日志」用；暂未接 UI，先留着）。
#[allow(dead_code)]
pub fn tail(lines: usize) -> String {
    let Some(path) = log_path() else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_exists_after_write() {
        write("test", "hello");
        // 只验证不 panic；路径依赖运行环境，不在这里断言文件内容。
    }

    #[test]
    fn tail_does_not_panic_without_file() {
        let _ = tail(10);
    }
}
