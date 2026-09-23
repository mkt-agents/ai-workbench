//! Persistence for the AI half of a regression report — and only that half.
//!
//! The report itself deliberately stays out of the database: it is a projection of
//! the current working tree, recomputed on every selection, so a stored copy would
//! only go stale and mislead. The AI answer is a different kind of thing — it costs
//! a model round trip (map-reduce, tens of seconds and real tokens) and it is the
//! artefact a tester actually works from. That is what this table keeps.
//!
//! Storage only: `&Connection` in, data out. The Tauri commands at the bottom are
//! thin wrappers so the logic stays testable against an in-memory database.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::DbState;

/// How many answers to keep. A row is a few KB of Markdown, so this stays far
/// below a megabyte while covering roughly a month of daily use on one repo.
const MAX_ENTRIES: i64 = 50;

const SCHEMA_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS report_ai_history (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        target_kind TEXT NOT NULL,
        target_label TEXT NOT NULL,
        baseline TEXT NOT NULL DEFAULT '',
        model TEXT NOT NULL DEFAULT '',
        requirement TEXT NOT NULL DEFAULT '',
        markdown TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_report_ai_history_created
        ON report_ai_history(created_at DESC);
"#;

/// Idempotent; called from the setup block next to the other tables.
pub fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA_SQL).map_err(|e| e.to_string())
}

/// What the panel hands over when a generation succeeds.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryInput {
    /// `repo` | `folder` — drives the icon and the label wording in the list.
    pub target_kind: String,
    pub target_label: String,
    pub baseline: String,
    pub model: String,
    /// Snapshot of the requirement box, so a past answer can be read in context.
    pub requirement: String,
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: i64,
    pub target_kind: String,
    pub target_label: String,
    pub baseline: String,
    pub model: String,
    pub requirement: String,
    pub markdown: String,
    pub created_at: i64,
}

/// Insert one answer and drop the oldest rows past `MAX_ENTRIES`, so the table
/// cannot grow without bound. Returns the new row id.
pub fn insert_entry(conn: &Connection, input: &HistoryInput, created_at: i64) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO report_ai_history
            (target_kind, target_label, baseline, model, requirement, markdown, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            input.target_kind,
            input.target_label,
            input.baseline,
            input.model,
            input.requirement,
            input.markdown,
            created_at
        ],
    )
    .map_err(|e| e.to_string())?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM report_ai_history
          WHERE id NOT IN (
              SELECT id FROM report_ai_history ORDER BY created_at DESC, id DESC LIMIT ?1
          )",
        params![MAX_ENTRIES],
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Newest first. The Markdown travels with the row: a full list is well under a
/// megabyte, and the panel can then expand any entry without a second round trip.
pub fn list_entries(conn: &Connection) -> Result<Vec<HistoryEntry>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, target_kind, target_label, baseline, model, requirement, markdown, created_at
               FROM report_ai_history
              ORDER BY created_at DESC, id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                target_kind: row.get(1)?,
                target_label: row.get(2)?,
                baseline: row.get(3)?,
                model: row.get(4)?,
                requirement: row.get(5)?,
                markdown: row.get(6)?,
                created_at: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn delete_entry(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM report_ai_history WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Commands. Thin wrappers: the connection lock is taken on a blocking thread so
// a slow disk never stalls the async runtime, matching `db_save` elsewhere.
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn save_report_ai_history(
    state: tauri::State<'_, DbState>,
    entry: HistoryInput,
) -> Result<i64, String> {
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        insert_entry(&guard, &entry, now_millis())
    })
    .await
    .map_err(|e| format!("保存失败: {}", e))?
}

#[tauri::command]
pub async fn list_report_ai_history(state: tauri::State<'_, DbState>) -> Result<Vec<HistoryEntry>, String> {
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        list_entries(&guard)
    })
    .await
    .map_err(|e| format!("读取失败: {}", e))?
}

#[tauri::command]
pub async fn delete_report_ai_history(state: tauri::State<'_, DbState>, id: i64) -> Result<(), String> {
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        delete_entry(&guard, id)
    })
    .await
    .map_err(|e| format!("删除失败: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    fn input(label: &str, markdown: &str) -> HistoryInput {
        HistoryInput {
            target_kind: "repo".to_string(),
            target_label: label.to_string(),
            baseline: "HEAD~5".to_string(),
            model: "longcat-2.0".to_string(),
            requirement: "新增退款审批".to_string(),
            markdown: markdown.to_string(),
        }
    }

    #[test]
    fn schema_is_idempotent() {
        let conn = mem_conn();
        // A second run must not fail on the existing table/index.
        ensure_schema(&conn).unwrap();
    }

    #[test]
    fn round_trips_an_entry_with_its_context() {
        let conn = mem_conn();
        let id = insert_entry(&conn, &input("p1", "## 必测场景\n- [P0] 退款"), 1_700_000_000_000).unwrap();
        let entries = list_entries(&conn).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.id, id);
        assert_eq!(entry.target_label, "p1");
        assert_eq!(entry.baseline, "HEAD~5");
        assert_eq!(entry.model, "longcat-2.0");
        assert_eq!(entry.requirement, "新增退款审批");
        assert!(entry.markdown.contains("必测场景"));
        assert_eq!(entry.created_at, 1_700_000_000_000);
    }

    #[test]
    fn lists_newest_first() {
        let conn = mem_conn();
        insert_entry(&conn, &input("old", "a"), 1_000).unwrap();
        insert_entry(&conn, &input("new", "b"), 2_000).unwrap();
        let labels: Vec<String> = list_entries(&conn).unwrap().into_iter().map(|e| e.target_label).collect();
        assert_eq!(labels, vec!["new".to_string(), "old".to_string()]);
    }

    #[test]
    fn keeps_only_the_newest_entries() {
        let conn = mem_conn();
        for index in 0..(MAX_ENTRIES + 5) {
            insert_entry(&conn, &input(&format!("repo-{}", index), "x"), 1_000 + index).unwrap();
        }
        let entries = list_entries(&conn).unwrap();
        assert_eq!(entries.len(), MAX_ENTRIES as usize);
        // The oldest five went out; the newest survived.
        assert_eq!(entries[0].target_label, format!("repo-{}", MAX_ENTRIES + 4));
        assert!(entries.iter().all(|e| e.target_label != "repo-0"));
    }

    #[test]
    fn deleting_one_leaves_the_rest() {
        let conn = mem_conn();
        let keep = insert_entry(&conn, &input("keep", "a"), 1_000).unwrap();
        let drop_id = insert_entry(&conn, &input("drop", "b"), 2_000).unwrap();
        delete_entry(&conn, drop_id).unwrap();
        let entries = list_entries(&conn).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, keep);
    }
}
