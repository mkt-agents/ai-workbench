//! Work todo schema, drafts→tasks migration, and logs→tasks merge.

use rusqlite::{params, Connection};

pub const MIGRATION_FLAG: &str = "work_tasks_migrated_from_drafts_v1";
pub const LOGS_MERGED_FLAG: &str = "work_logs_merged_into_tasks_v1";

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    for name in rows.flatten() {
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS work_tasks (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL DEFAULT '',
            notes TEXT NOT NULL DEFAULT '',
            tags TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'open',
            work_date TEXT,
            pinned INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            done_at TEXT
        );",
    )
    .map_err(|e| e.to_string())?;
    if !column_exists(conn, "work_tasks", "tags")? {
        conn.execute(
            "ALTER TABLE work_tasks ADD COLUMN tags TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn flag_done(conn: &Connection, key: &str) -> bool {
    conn.query_row(
        "SELECT value FROM app_settings WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .is_some_and(|v| v == "1" || v == "true")
}

fn set_flag(conn: &Connection, key: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO app_settings (key, value, updated_at) VALUES (?1, '1', datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn draft_display_title(title: &str, content: &str) -> String {
    let t = title.trim();
    if !t.is_empty() {
        return t.to_string();
    }
    content
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .unwrap_or_default()
}

fn log_title_from_content(content: &str, work_date: &str) -> String {
    let first = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim())
        .unwrap_or("");
    if first.is_empty() {
        return format!("\u{5de5}\u{4f5c}\u{65e5}\u{5fd7} {work_date}");
    }
    let mut out = String::new();
    for (i, ch) in first.chars().enumerate() {
        if i >= 80 {
            break;
        }
        out.push(ch);
    }
    if first.chars().count() > 80 {
        out.push('\u{2026}');
    }
    out
}

pub fn migrate_drafts_if_needed(conn: &Connection) -> Result<(), String> {
    ensure_schema(conn)?;
    if flag_done(conn, MIGRATION_FLAG) {
        return Ok(());
    }
    let table_ok = conn
        .prepare("SELECT 1 FROM drafts LIMIT 1")
        .map(|_| true)
        .unwrap_or(false);
    if !table_ok {
        set_flag(conn, MIGRATION_FLAG)?;
        return Ok(());
    }
    let mut stmt = conn
        .prepare("SELECT id, title, content, pinned, created_at, updated_at FROM drafts")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows.flatten() {
        let (id, title, content, pinned, created_at, updated_at) = row;
        let display = draft_display_title(&title, &content);
        let title_out = if display.is_empty() {
            "\u{65e0}\u{6807}\u{9898}".to_string()
        } else {
            display
        };
        conn.execute(
            "INSERT OR IGNORE INTO work_tasks (id, title, notes, tags, status, work_date, pinned, created_at, updated_at, done_at)
             VALUES (?1, ?2, ?3, '', 'open', NULL, ?4, ?5, ?6, NULL)",
            params![id, title_out, content, pinned, created_at, updated_at],
        )
        .map_err(|e| e.to_string())?;
    }
    let _ = conn.execute("DELETE FROM drafts", []);
    set_flag(conn, MIGRATION_FLAG)?;
    Ok(())
}

/// Merge daily journals into done tasks, then drop `work_daily_logs`.
pub fn migrate_logs_into_tasks_if_needed(conn: &Connection) -> Result<(), String> {
    ensure_schema(conn)?;
    if flag_done(conn, LOGS_MERGED_FLAG) {
        let _ = conn.execute_batch("DROP TABLE IF EXISTS work_daily_logs;");
        return Ok(());
    }
    let logs_exist = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_daily_logs'")
        .and_then(|mut s| s.exists([]))
        .unwrap_or(false);
    if logs_exist {
        let mut stmt = conn
            .prepare("SELECT work_date, content, updated_at FROM work_daily_logs")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            let (work_date, content, updated_at) = row;
            if content.trim().is_empty() {
                continue;
            }
            let id = format!("task-log-{work_date}");
            let title = log_title_from_content(&content, &work_date);
            conn.execute(
                "INSERT OR IGNORE INTO work_tasks
                 (id, title, notes, tags, status, work_date, pinned, created_at, updated_at, done_at)
                 VALUES (?1, ?2, ?3, '', 'done', ?4, 0, ?5, ?5, ?5)",
                params![id, title, content, work_date, updated_at],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    conn.execute_batch("DROP TABLE IF EXISTS work_daily_logs;")
        .map_err(|e| e.to_string())?;
    set_flag(conn, LOGS_MERGED_FLAG)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_title_falls_back_to_first_line() {
        assert_eq!(draft_display_title("", "hello\nworld"), "hello");
        assert_eq!(draft_display_title("T", "body"), "T");
    }

    #[test]
    fn log_title_truncates_and_fallback() {
        assert_eq!(
            log_title_from_content("", "2026-10-31"),
            "\u{5de5}\u{4f5c}\u{65e5}\u{5fd7} 2026-10-31"
        );
        assert_eq!(log_title_from_content("hi\nthere", "2026-10-31"), "hi");
        let long = "a".repeat(100);
        let t = log_title_from_content(&long, "d");
        assert_eq!(t.chars().count(), 81); // 80 + ellipsis
        assert!(t.ends_with('\u{2026}'));
    }

    #[test]
    fn merge_logs_creates_done_tasks_and_drops_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT, updated_at TEXT);
             CREATE TABLE work_daily_logs (
                work_date TEXT PRIMARY KEY,
                content TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_daily_logs VALUES ('2026-10-30', 'did stuff', '2026-10-30T12:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_daily_logs VALUES ('2026-10-31', '   ', '2026-10-31T12:00:00Z')",
            [],
        )
        .unwrap();
        migrate_logs_into_tasks_if_needed(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM work_tasks WHERE id = 'task-log-2026-10-30'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let status: String = conn
            .query_row(
                "SELECT status FROM work_tasks WHERE id = 'task-log-2026-10-30'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "done");
        let empty_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM work_tasks WHERE id = 'task-log-2026-10-31'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(empty_count, 0);
        let logs_gone: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='work_daily_logs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logs_gone, 0);
        // Idempotent
        migrate_logs_into_tasks_if_needed(&conn).unwrap();
    }
}
