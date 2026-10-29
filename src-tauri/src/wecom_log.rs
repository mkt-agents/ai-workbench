//! Bounded, serialized WeCom diagnostic log; no message bodies should be supplied.
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_BYTES: u64 = 1024 * 1024;
const MAX_LINE_CHARS: usize = 16 * 1024;
static LOG_LOCK: Mutex<()> = Mutex::new(());

fn log_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "APPDATA unavailable".to_string())?;
    Ok(base.join("com.ai-workbench.app").join("wecom.log"))
}

fn write_at(path: &Path, scope: &str, message: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Sanitize before truncation: a cut credential must not evade detection.
    let safe = crate::wecom_sanitize::redact_diagnostic(message);
    let safe: String = safe.chars().take(MAX_LINE_CHARS).collect();
    let line = format!(
        "{} [{}] {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f%:z"),
        scope.replace(['\r', '\n'], " "),
        safe.replace(['\r', '\n'], " ")
    );
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len.saturating_add(line.len() as u64) > MAX_BYTES {
        let old = path.with_extension("log.1");
        // Windows rename does not replace an existing destination.
        match std::fs::remove_file(&old) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::fs::rename(path, old)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

pub fn write(scope: &str, message: &str) {
    let result = (|| -> Result<(), String> {
        let _guard = LOG_LOCK
            .lock()
            .map_err(|_| "log lock poisoned".to_string())?;
        write_at(&log_path()?, scope, message).map_err(|e| e.to_string())
    })();
    if let Err(e) = result {
        eprintln!(
            "[wecom-log-write-error] {}",
            crate::wecom_sanitize::redact_diagnostic(&e)
        );
    }
}

fn read_tail_at(path: &Path, lines: usize) -> Result<String, String> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.to_string()),
    };
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let start = len.saturating_sub(MAX_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        text = text
            .split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default();
    }
    let mut tail: Vec<&str> = text.lines().rev().take(lines).collect();
    tail.reverse();
    Ok(crate::wecom_sanitize::redact_diagnostic(&tail.join("\n")))
}

pub fn tail_checked(lines: usize) -> Result<String, String> {
    let _guard = LOG_LOCK
        .lock()
        .map_err(|_| "log lock poisoned".to_string())?;
    let path = log_path()?;
    let current = read_tail_at(&path, lines)?;
    let count = current.lines().count();
    if count >= lines {
        return Ok(current);
    }
    let old = read_tail_at(&path.with_extension("log.1"), lines - count)?;
    Ok([old, current]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

#[allow(dead_code)] // Compatibility for callers that display errors inline.
pub fn tail(lines: usize) -> String {
    tail_checked(lines).unwrap_or_else(|e| {
        format!(
            "[log-read-error] {}",
            crate::wecom_sanitize::redact_diagnostic(&e)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotates_repeatedly_and_redacts_in_isolated_directory() {
        let dir = std::env::temp_dir().join(format!("wecom-log-fixture-{}", rand::random::<u64>()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("wecom.log");
        for _ in 0..2 {
            std::fs::write(&path, vec![b'x'; MAX_BYTES as usize]).unwrap();
            write_at(&path, "test", "https://example.test/?token=fixture-secret").unwrap();
            assert!(path.with_extension("log.1").exists());
        }
        let text = read_tail_at(&path, 10).unwrap();
        assert!(!text.contains("fixture-secret"));
        assert!(text.contains("REDACTED"));
        assert!(read_tail_at(&dir, 10).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
