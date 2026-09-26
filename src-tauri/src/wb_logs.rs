//! Gateway request log + usage rollups.
//!
//! Written by the gateway on every proxied call, so it is append-heavy and
//! deliberately outside the db_load/db_save whitelist: the frontend only ever
//! asks for a page of it. `MAX_ROWS` keeps a long-lived gateway from filling
//! the disk (FIFO trim on insert, same shape as report_history).

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::DbState;

const MAX_ROWS: i64 = 5000;

/// One row as the gateway records it.
#[derive(Debug, Clone)]
pub struct LogEntryInput {
    pub ts: i64,
    pub key_id: Option<i64>,
    pub account_id: Option<i64>,
    pub model: Option<String>,
    pub stream: bool,
    pub status_code: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WbLogDto {
    pub id: i64,
    pub ts: i64,
    pub key_id: Option<i64>,
    pub key_label: Option<String>,
    pub account_id: Option<i64>,
    pub account_label: Option<String>,
    pub model: Option<String>,
    pub stream: bool,
    pub status_code: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatRow {
    pub label: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub errors: i64,
    pub avg_latency_ms: Option<i64>,
}

pub fn insert_log(conn: &Connection, input: &LogEntryInput) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO wb_request_logs
            (ts, key_id, account_id, model, stream, status_code,
             prompt_tokens, completion_tokens, latency_ms, error)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            input.ts,
            input.key_id,
            input.account_id,
            input.model,
            i64::from(input.stream),
            input.status_code,
            input.prompt_tokens,
            input.completion_tokens,
            input.latency_ms,
            input.error
        ],
    )
    .map_err(|e| e.to_string())?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM wb_request_logs
          WHERE id NOT IN (SELECT id FROM wb_request_logs ORDER BY ts DESC, id DESC LIMIT ?1)",
        params![MAX_ROWS],
    )
    .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Newest first, with the key/account names joined in so the panel does not
/// need a second round trip per row.
pub fn list_logs(
    conn: &Connection,
    limit: i64,
    key_id: Option<i64>,
    model: Option<&str>,
    since: Option<i64>,
) -> Result<Vec<WbLogDto>, String> {
    let mut sql = String::from(
        "SELECT l.id, l.ts, l.key_id, k.label, l.account_id, a.label, l.model, l.stream,
                l.status_code, l.prompt_tokens, l.completion_tokens, l.latency_ms, l.error
           FROM wb_request_logs l
      LEFT JOIN wb_api_keys k ON k.id = l.key_id
      LEFT JOIN codebuddy_accounts a ON a.id = l.account_id
          WHERE 1 = 1",
    );
    let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(id) = key_id {
        args.push(Box::new(id));
        sql.push_str(&format!(" AND l.key_id = ?{}", args.len()));
    }
    if let Some(m) = model {
        args.push(Box::new(m.to_string()));
        sql.push_str(&format!(" AND l.model = ?{}", args.len()));
    }
    if let Some(ts) = since {
        args.push(Box::new(ts));
        sql.push_str(&format!(" AND l.ts >= ?{}", args.len()));
    }
    args.push(Box::new(limit.clamp(1, MAX_ROWS)));
    sql.push_str(&format!(" ORDER BY l.ts DESC, l.id DESC LIMIT ?{}", args.len()));

    let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|v| v.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(refs.as_slice(), |row| {
            Ok(WbLogDto {
                id: row.get(0)?,
                ts: row.get(1)?,
                key_id: row.get(2)?,
                key_label: row.get(3)?,
                account_id: row.get(4)?,
                account_label: row.get(5)?,
                model: row.get(6)?,
                stream: row.get::<_, i64>(7)? != 0,
                status_code: row.get(8)?,
                prompt_tokens: row.get(9)?,
                completion_tokens: row.get(10)?,
                latency_ms: row.get(11)?,
                error: row.get(12)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn clear_logs(conn: &Connection, before_ts: Option<i64>) -> Result<usize, String> {
    match before_ts {
        Some(ts) => conn.execute("DELETE FROM wb_request_logs WHERE ts < ?1", params![ts]),
        None => conn.execute("DELETE FROM wb_request_logs", []),
    }
    .map_err(|e| e.to_string())
}

/// The `GROUP BY` key is chosen from a closed set — never interpolated from
/// user input — so the format! below cannot become an injection point.
pub fn stats(conn: &Connection, days: i64, group_by: &str) -> Result<Vec<StatRow>, String> {
    const DAY: &str = "date(l.ts / 1000, 'unixepoch', 'localtime')";
    const MODEL: &str = "COALESCE(l.model, '(unknown)')";
    const KEY: &str = "COALESCE(k.label, '(no key)')";
    let group = match group_by {
        "day" => DAY,
        "model" => MODEL,
        "key" => KEY,
        other => return Err(format!("未知分组: {other}")),
    };
    let since = chrono::Utc::now().timestamp_millis() - days.max(1) * 86_400_000;
    let sql = format!(
        "SELECT {group} AS bucket,
                COUNT(*),
                COALESCE(SUM(l.prompt_tokens), 0),
                COALESCE(SUM(l.completion_tokens), 0),
                SUM(CASE WHEN l.status_code >= 400 OR l.error IS NOT NULL THEN 1 ELSE 0 END),
                CAST(AVG(l.latency_ms) AS INTEGER)
           FROM wb_request_logs l
      LEFT JOIN wb_api_keys k ON k.id = l.key_id
          WHERE l.ts >= ?1
       GROUP BY bucket
       ORDER BY bucket DESC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![since], |row| {
            Ok(StatRow {
                label: row.get(0)?,
                requests: row.get(1)?,
                prompt_tokens: row.get(2)?,
                completion_tokens: row.get(3)?,
                errors: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                avg_latency_ms: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn distinct_models(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT model FROM wb_request_logs WHERE model IS NOT NULL ORDER BY model")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

async fn with_db<T, F>(state: &tauri::State<'_, DbState>, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T, String> + Send + 'static,
{
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        f(&guard)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

#[tauri::command]
pub async fn wb_log_list(
    state: tauri::State<'_, DbState>,
    limit: Option<i64>,
    key_id: Option<i64>,
    model: Option<String>,
    since: Option<i64>,
) -> Result<Vec<WbLogDto>, String> {
    with_db(&state, move |conn| {
        list_logs(conn, limit.unwrap_or(200), key_id, model.as_deref(), since)
    })
    .await
}

#[tauri::command]
pub async fn wb_log_clear(
    state: tauri::State<'_, DbState>,
    before_ts: Option<i64>,
) -> Result<usize, String> {
    with_db(&state, move |conn| clear_logs(conn, before_ts)).await
}

#[tauri::command]
pub async fn wb_log_stats(
    state: tauri::State<'_, DbState>,
    days: Option<i64>,
    group_by: Option<String>,
) -> Result<Vec<StatRow>, String> {
    with_db(&state, move |conn| {
        stats(conn, days.unwrap_or(7), group_by.as_deref().unwrap_or("day"))
    })
    .await
}

#[tauri::command]
pub async fn wb_log_models(state: tauri::State<'_, DbState>) -> Result<Vec<String>, String> {
    with_db(&state, distinct_models).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::codebuddy::ensure_schema(&c).unwrap();
        c
    }

    fn entry(ts: i64, model: &str, code: i64) -> LogEntryInput {
        LogEntryInput {
            ts,
            key_id: None,
            account_id: None,
            model: Some(model.to_string()),
            stream: false,
            status_code: Some(code),
            prompt_tokens: Some(10),
            completion_tokens: Some(20),
            latency_ms: Some(100),
            error: None,
        }
    }

    #[test]
    fn lists_newest_first_with_filters() {
        let c = conn();
        insert_log(&c, &entry(1_000, "a", 200)).unwrap();
        insert_log(&c, &entry(2_000, "b", 200)).unwrap();
        insert_log(&c, &entry(3_000, "a", 500)).unwrap();

        let all = list_logs(&c, 100, None, None, None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].ts, 3_000);

        let only_a = list_logs(&c, 100, None, Some("a"), None).unwrap();
        assert_eq!(only_a.iter().map(|l| l.ts).collect::<Vec<_>>(), vec![3_000, 1_000]);

        let since = list_logs(&c, 100, None, None, Some(2_000)).unwrap();
        assert_eq!(since.len(), 2);
    }

    #[test]
    fn fifo_trims_oldest_beyond_the_cap() {
        let c = conn();
        for i in 0..(MAX_ROWS + 5) {
            insert_log(&c, &entry(1_000 + i, "m", 200)).unwrap();
        }
        let kept = list_logs(&c, MAX_ROWS + 100, None, None, None).unwrap();
        assert_eq!(kept.len() as i64, MAX_ROWS);
        assert_eq!(kept[0].ts, 1_000 + MAX_ROWS + 4);
        assert!(!kept.iter().any(|l| l.ts == 1_000));
    }

    #[test]
    fn stats_group_by_model_totals_tokens_and_errors() {
        let c = conn();
        let now = chrono::Utc::now().timestamp_millis();
        insert_log(&c, &entry(now - 1_000, "deepseek", 200)).unwrap();
        let mut bad = entry(now - 2_000, "deepseek", 500);
        bad.prompt_tokens = Some(5);
        bad.completion_tokens = Some(0);
        insert_log(&c, &bad).unwrap();
        insert_log(&c, &entry(now - 3_000, "glm", 200)).unwrap();

        let by_model = stats(&c, 7, "model").unwrap();
        let deepseek = by_model.iter().find(|r| r.label == "deepseek").unwrap();
        assert_eq!(deepseek.requests, 2);
        assert_eq!(deepseek.prompt_tokens, 15);
        assert_eq!(deepseek.errors, 1);
        assert_eq!(by_model.iter().find(|r| r.label == "glm").unwrap().errors, 0);
    }

    #[test]
    fn stats_rejects_unknown_group_and_ignores_old_rows() {
        let c = conn();
        assert!(stats(&c, 7, "model); DROP TABLE wb_request_logs;--").is_err());
        insert_log(&c, &entry(100, "ancient", 200)).unwrap();
        assert!(stats(&c, 7, "day").unwrap().is_empty());
    }

    #[test]
    fn clear_respects_the_cutoff() {
        let c = conn();
        insert_log(&c, &entry(1_000, "a", 200)).unwrap();
        insert_log(&c, &entry(9_000, "a", 200)).unwrap();
        assert_eq!(clear_logs(&c, Some(5_000)).unwrap(), 1);
        assert_eq!(list_logs(&c, 10, None, None, None).unwrap()[0].ts, 9_000);
        assert_eq!(clear_logs(&c, None).unwrap(), 1);
        assert!(list_logs(&c, 10, None, None, None).unwrap().is_empty());
    }

    #[test]
    fn joins_key_and_account_labels() {
        let c = conn();
        c.execute(
            "INSERT INTO wb_api_keys (id, key, label, enabled, created_at) VALUES (7, 'wk-x', 'IDE', 1, 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO codebuddy_accounts (id, label, credential_type, credential, status, enabled, created_at)
             VALUES (3, '小号A', 'token', 't', 'active', 1, 1)",
            [],
        )
        .unwrap();
        let mut e = entry(1_000, "m", 200);
        e.key_id = Some(7);
        e.account_id = Some(3);
        insert_log(&c, &e).unwrap();
        let row = &list_logs(&c, 10, Some(7), None, None).unwrap()[0];
        assert_eq!(row.key_label.as_deref(), Some("IDE"));
        assert_eq!(row.account_label.as_deref(), Some("小号A"));
    }

    #[test]
    fn distinct_models_sorted() {
        let c = conn();
        insert_log(&c, &entry(1, "glm", 200)).unwrap();
        insert_log(&c, &entry(2, "deepseek", 200)).unwrap();
        assert_eq!(distinct_models(&c).unwrap(), vec!["deepseek", "glm"]);
    }
}
