use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewWindow};

const PAGE_MESSAGES: usize = 100;

pub(crate) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS ai_hub_transcript_state (
        session_id TEXT PRIMARY KEY, revision INTEGER NOT NULL DEFAULT 0,
        legacy TEXT NOT NULL DEFAULT '', summary TEXT NOT NULL DEFAULT '', covered INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS ai_hub_transcript (
        session_id TEXT NOT NULL, ordinal INTEGER NOT NULL, message TEXT NOT NULL,
        PRIMARY KEY(session_id, ordinal)
    );")?;
    if !conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('ai_hub_transcript_state') WHERE name = 'usage')", [], |row| row.get::<_, bool>(0))? {
        conn.execute("ALTER TABLE ai_hub_transcript_state ADD COLUMN usage TEXT", [])?;
    }
    Ok(())
}

fn sql_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

fn import(tx: &Transaction<'_>, id: &str) -> rusqlite::Result<()> {
    if tx
        .query_row(
            "SELECT 1 FROM ai_hub_transcript_state WHERE session_id = ?1",
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Ok(());
    }
    let raw: String = tx.query_row(
        "SELECT turns FROM ai_hub_sessions WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let values: Vec<Value> = serde_json::from_str(&raw).map_err(sql_error)?;
    let mut legacy = Vec::new();
    let mut ordinal = 0;
    for item in values {
        if item["digest"].as_bool() == Some(true) {
            if let Some(text) = item["content"].as_str() {
                legacy.push(text.to_owned());
            }
        } else {
            tx.execute(
                "INSERT INTO ai_hub_transcript VALUES (?1, ?2, ?3)",
                params![id, ordinal, item.to_string()],
            )?;
            ordinal += 1;
        }
    }
    tx.execute(
        "INSERT INTO ai_hub_transcript_state(session_id, legacy) VALUES (?1, ?2)",
        params![id, legacy.join("\n")],
    )?;
    tx.execute(
        "UPDATE ai_hub_sessions SET turns = '[]' WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

fn length(conn: &Connection, id: &str) -> rusqlite::Result<usize> {
    conn.query_row(
        "SELECT coalesce(max(ordinal)+1, 0) FROM ai_hub_transcript WHERE session_id = ?1",
        [id],
        |r| r.get(0),
    )
}

fn messages(
    conn: &Connection,
    id: &str,
    from: usize,
    until: usize,
) -> rusqlite::Result<Vec<Value>> {
    let mut query = conn.prepare("SELECT message FROM ai_hub_transcript WHERE session_id = ?1 AND ordinal >= ?2 AND ordinal < ?3 ORDER BY ordinal")?;
    let result = query
        .query_map(params![id, from, until], |r| r.get::<_, String>(0))?
        .map(|row| serde_json::from_str(&row?).map_err(sql_error))
        .collect();
    result
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextInfo {
    pub(crate) covered: usize,
    pub(crate) estimated_tokens: Option<usize>,
    pub(crate) input_budget: Option<usize>,
    pub(crate) compacted: bool,
    /// Old snapshots have only a content-budget estimate; do not reinterpret it as window usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) budget: Option<crate::model_budget::ModelBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) total_input_tokens: Option<usize>,
    /// A manual compaction is not a complete provider request (it has no current tool schemas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) usage_kind: Option<String>,
}

impl ContextInfo {
    pub(crate) fn with_budget(mut self, budget: crate::model_budget::ModelBudget) -> Self {
        self.total_input_tokens = self.estimated_tokens.map(|used| used.saturating_add(budget.schema_tokens));
        self.budget = Some(budget);
        self.usage_kind = Some("request".into());
        self
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    turns: Vec<Value>,
    start: usize,
    total: usize,
    revision: u64,
    legacy: String,
    context: ContextInfo,
}

fn page(conn: &Connection, id: &str, before: Option<usize>) -> rusqlite::Result<Page> {
    let tx = conn.unchecked_transaction()?;
    import(&tx, id)?;
    let total = length(&tx, id)?;
    let end = before.unwrap_or(total).min(total);
    let start = end.saturating_sub(PAGE_MESSAGES);
    let (revision, legacy, covered, usage): (u64, String, usize, Option<String>) = tx.query_row(
        "SELECT revision, legacy, covered, usage FROM ai_hub_transcript_state WHERE session_id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut context: ContextInfo = usage
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    context.covered = covered;
    context.compacted = false;
    let turns = messages(&tx, id, start, end)?;
    tx.commit()?;
    Ok(Page {
        turns,
        start,
        total,
        revision,
        legacy,
        context,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    id: String,
    title: String,
    task: String,
    created_at: String,
    updated_at: String,
    total: usize,
}

fn list(conn: &Connection) -> rusqlite::Result<Vec<Session>> {
    let mut query = conn.prepare("SELECT id, title, task, created_at, updated_at FROM ai_hub_sessions ORDER BY updated_at DESC")?;
    let mut sessions = query
        .query_map([], |r| {
            Ok(Session {
                id: r.get(0)?,
                title: r.get(1)?,
                task: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
                total: 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let tx = conn.unchecked_transaction()?;
    for session in &mut sessions {
        if let Err(error) = import(&tx, &session.id) {
            // A malformed legacy row must not prevent opening other sessions.
            if !matches!(error, rusqlite::Error::ToSqlConversionFailure(_)) {
                return Err(error);
            }
        }
        session.total = length(&tx, &session.id)?;
    }
    tx.commit()?;
    Ok(sessions)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Edit {
    id: String,
    revision: u64,
    from: usize,
    turns: Vec<Value>,
    updated_at: String,
}

fn edit(conn: &Connection, input: Edit) -> Result<u64, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let run = || -> rusqlite::Result<Option<u64>> {
        import(&tx, &input.id)?;
        let total = length(&tx, &input.id)?;
        let revision: u64 = tx.query_row(
            "SELECT revision FROM ai_hub_transcript_state WHERE session_id = ?1",
            [&input.id],
            |r| r.get(0),
        )?;
        if revision != input.revision || input.from > total {
            return Ok(None);
        }
        tx.execute(
            "DELETE FROM ai_hub_transcript WHERE session_id = ?1 AND ordinal >= ?2",
            params![input.id, input.from],
        )?;
        for (i, item) in input.turns.iter().enumerate() {
            tx.execute(
                "INSERT INTO ai_hub_transcript VALUES (?1, ?2, ?3)",
                params![input.id, input.from + i, item.to_string()],
            )?;
        }
        tx.execute(
            "UPDATE ai_hub_transcript_state SET revision = revision + 1,
            summary = CASE WHEN covered > ?2 THEN '' ELSE summary END,
            usage = CASE WHEN covered > ?2 THEN NULL ELSE usage END,
            covered = CASE WHEN covered > ?2 THEN 0 ELSE covered END WHERE session_id = ?1",
            params![input.id, input.from],
        )?;
        tx.execute(
            "UPDATE ai_hub_sessions SET updated_at = ?2 WHERE id = ?1",
            params![input.id, input.updated_at],
        )?;
        Ok(Some(revision + 1))
    };
    let revision = run()
        .map_err(|e| e.to_string())?
        .ok_or("TRANSCRIPT_CHANGED")?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(revision)
}

pub(crate) fn prune(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM ai_hub_transcript WHERE session_id NOT IN (SELECT id FROM ai_hub_sessions)",
        [],
    )?;
    conn.execute("DELETE FROM ai_hub_transcript_state WHERE session_id NOT IN (SELECT id FROM ai_hub_sessions)", [])?;
    Ok(())
}

pub(crate) fn desktop(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == "ai-hub" {
        Ok(())
    } else {
        Err("TRANSCRIPT_DESKTOP_ONLY".into())
    }
}

pub(crate) async fn db<T: Send + 'static>(
    app: AppHandle,
    action: impl FnOnce(&Connection) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<crate::DbState>();
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        action(&conn)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn agent_session_list(app: AppHandle) -> Result<Vec<Session>, String> {
    db(app, |conn| list(conn).map_err(|e| e.to_string())).await
}

#[tauri::command]
pub(crate) async fn agent_session_search(
    app: AppHandle,
    window: WebviewWindow,
    query: String,
) -> Result<Vec<String>, String> {
    desktop(&window)?;
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    db(app, move |conn| {
        let mut stmt = conn
            .prepare(
                "SELECT session_id, legacy FROM ai_hub_transcript_state
            UNION ALL SELECT session_id, json_extract(message, '$.content') FROM ai_hub_transcript",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut matches = std::collections::HashSet::new();
        for row in rows {
            let (id, text) = row.map_err(|e| e.to_string())?;
            if text.is_some_and(|text| text.to_lowercase().contains(&query)) {
                matches.insert(id);
            }
        }
        Ok(matches.into_iter().collect())
    })
    .await
}

#[tauri::command]
pub(crate) async fn agent_session_page(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    before: Option<usize>,
) -> Result<Page, String> {
    desktop(&window)?;
    db(app, move |conn| {
        page(conn, &id, before).map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn agent_session_edit(
    app: AppHandle,
    window: WebviewWindow,
    input: Edit,
) -> Result<u64, String> {
    desktop(&window)?;
    db(app, move |conn| edit(conn, input)).await
}

#[tauri::command]
pub(crate) async fn agent_session_save(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
    title: String,
    updated_at: String,
    create: bool,
) -> Result<Vec<String>, String> {
    desktop(&window)?;
    db(app, move |conn| {
        let run = || -> rusqlite::Result<Vec<String>> {
            let tx = conn.unchecked_transaction()?;
            if create {
                tx.execute("INSERT INTO ai_hub_sessions(id,title,task,turns,created_at,updated_at) VALUES (?1,?2,'none','[]',?3,?3)", params![id, title, updated_at])?;
            } else {
                tx.execute("UPDATE ai_hub_sessions SET title = ?2, updated_at = ?3 WHERE id = ?1", params![id,title,updated_at])?;
            }
            let mut evicted = Vec::new();
            if create {
                let stale = {
                    let mut query = tx.prepare("SELECT id,title FROM ai_hub_sessions WHERE id != ?1 AND id NOT IN
                        (SELECT session_id FROM agent_tasks WHERE status IN ('running','waiting_approval'))
                        ORDER BY updated_at DESC, id DESC LIMIT -1 OFFSET 49")?;
                    let rows = query.query_map([&id], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                for (id, title) in stale {
                    tx.execute("DELETE FROM ai_hub_sessions WHERE id = ?1", [&id])?;
                    evicted.push(title);
                }
                prune(&tx)?;
                crate::agent_task::prune_deleted_sessions(&tx)?;
            }
            tx.commit()?;
            Ok(evicted)
        };
        run().map_err(|e| e.to_string())
    }).await
}

#[tauri::command]
pub(crate) async fn agent_session_delete(
    app: AppHandle,
    window: WebviewWindow,
    ids: Vec<String>,
) -> Result<(), String> {
    desktop(&window)?;
    db(app, move |conn| {
        let run = || -> rusqlite::Result<()> {
            let tx = conn.unchecked_transaction()?;
            for id in ids {
                tx.execute("DELETE FROM ai_hub_sessions WHERE id = ?1 AND NOT EXISTS
                    (SELECT 1 FROM agent_tasks WHERE session_id = ?1 AND status IN ('running','waiting_approval'))", [&id])?;
            }
            prune(&tx)?;
            crate::agent_task::prune_deleted_sessions(&tx)?;
            tx.commit()
        };
        run().map_err(|e| e.to_string())
    }).await
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextRef {
    pub(crate) session_id: String,
    pub(crate) revision: u64,
    pub(crate) before: usize,
}

#[derive(Default)]
pub(crate) struct Snapshot {
    pub(crate) rewound: bool,
    pub(crate) turns: Vec<Value>,
    pub(crate) legacy: String,
    pub(crate) summary: String,
    pub(crate) covered: usize,
}

pub(crate) async fn snapshot(
    app: &AppHandle,
    context: &ContextRef,
    checkpoint_only: bool,
) -> Result<Snapshot, String> {
    let context = context.clone();
    db(app.clone(), move |conn| {
        let run = || -> rusqlite::Result<Option<Snapshot>> {
            let tx = conn.unchecked_transaction()?;
            import(&tx, &context.session_id)?;
            let (revision, legacy, mut summary, mut covered): (u64, String, String, usize) = tx.query_row(
                "SELECT revision,legacy,summary,covered FROM ai_hub_transcript_state WHERE session_id = ?1", [&context.session_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            if revision != context.revision || context.before > length(&tx, &context.session_id)? { return Ok(None); }
            let rewound = context.before < covered;
            if rewound { summary.clear(); covered = 0; }
            let (turns, legacy, summary) = if checkpoint_only {
                (Vec::new(), String::new(), String::new())
            } else {
                (messages(&tx, &context.session_id, covered, context.before)?, legacy, summary)
            };
            tx.commit()?;
            Ok(Some(Snapshot {turns, legacy, summary, covered, rewound}))
        };
        run().map_err(|e| e.to_string())?.ok_or_else(|| "TRANSCRIPT_CHANGED".into())
    }).await
}

pub(crate) async fn save_summary(
    app: &AppHandle,
    context: &ContextRef,
    old_covered: usize,
    covered: usize,
    summary: String,
) -> Result<bool, String> {
    let context = context.clone();
    db(app.clone(), move |conn| {
        store_summary(conn, &context, old_covered, covered, &summary).map_err(|e| e.to_string())
    })
    .await
}

pub(crate) async fn record_usage(app: &AppHandle, context: &ContextRef, info: &ContextInfo) {
    let Ok(usage) = serde_json::to_string(info) else {
        return;
    };
    let context = context.clone();
    // Usage is informational. A save failure cannot invalidate a durable summary or transcript.
    let _ = db(app.clone(), move |conn| {
        conn.execute(
            "UPDATE ai_hub_transcript_state SET usage = ?1 WHERE session_id = ?2 AND revision = ?3",
            params![usage, context.session_id, context.revision],
        )
        .map_err(|error| error.to_string())
    })
    .await;
}

fn store_summary(
    conn: &Connection,
    context: &ContextRef,
    old_covered: usize,
    covered: usize,
    summary: &str,
) -> rusqlite::Result<bool> {
    if covered <= old_covered || covered > context.before {
        return Ok(false);
    }
    conn.execute(
        "UPDATE ai_hub_transcript_state SET covered = ?1, summary = ?2
        WHERE session_id = ?3 AND revision = ?4 AND covered = ?5",
        params![
            covered,
            summary,
            context.session_id,
            context.revision,
            old_covered
        ],
    )
    .map(|count| count == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn database(legacy: &[Value]) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE ai_hub_sessions (id TEXT PRIMARY KEY, title TEXT, task TEXT, turns TEXT, created_at TEXT, updated_at TEXT)").unwrap();
        db.execute(
            "INSERT INTO ai_hub_sessions VALUES ('s', 'Title', 'none', ?1, '', '')",
            [json!(legacy).to_string()],
        )
        .unwrap();
        init(&db).unwrap();
        db
    }

    fn write(
        db: &Connection,
        revision: u64,
        from: usize,
        turns: Vec<Value>,
    ) -> Result<u64, String> {
        edit(
            db,
            Edit {
                id: "s".into(),
                revision,
                from,
                turns,
                updated_at: String::new(),
            },
        )
    }

    #[test]
    fn original_transcript_survives_migration_and_120_rounds() {
        let first = json!({"role":"user", "content":"original first question"});
        let db = database(&[
            json!({"role":"assistant", "digest":true,"content":"old digest"}),
            first.clone(),
            json!({"role":"assistant", "content":"original first answer"}),
        ]);
        for i in 1..120 {
            assert_eq!(
                write(
                    &db,
                    i - 1,
                    i as usize * 2,
                    vec![
                        json!({"role":"user","content":format!("question {i}")}),
                        json!({"role":"assistant","content":format!("answer {i}")})
                    ]
                )
                .unwrap(),
                i
            );
        }
        let tail = page(&db, "s", None).unwrap();
        assert_eq!(
            (tail.start, tail.total, tail.turns.len(), tail.revision),
            (140, 240, 100, 119)
        );
        assert_eq!(tail.legacy, "old digest");
        let middle = page(&db, "s", Some(tail.start)).unwrap();
        let head = page(&db, "s", Some(middle.start)).unwrap();
        let exported: Vec<_> = head
            .turns
            .into_iter()
            .chain(middle.turns)
            .chain(tail.turns)
            .collect();
        assert_eq!(exported.len(), 240);
        assert_eq!(exported[0], first);
        assert_eq!(exported[239]["content"], "answer 119");
        assert_eq!(list(&db).unwrap()[0].total, 240);
    }

    #[test]
    fn rewrite_invalidates_overlapping_summary_and_rejects_old_writes() {
        let db = database(&[]);
        let messages = (0..8).map(|i| json!({"role": if i % 2 == 0 { "user" } else { "assistant" }, "content": i.to_string()})).collect();
        assert_eq!(write(&db, 0, 0, messages).unwrap(), 1);
        let context = ContextRef {
            session_id: "s".into(),
            revision: 1,
            before: 8,
        };
        assert!(store_summary(&db, &context, 0, 6, "keep user constraints").unwrap());
        assert!(!store_summary(&db, &context, 0, 8, "racing summary").unwrap());
        assert_eq!(write(&db, 1, 6, vec![]).unwrap(), 2);
        assert_eq!(page(&db, "s", None).unwrap().context.covered, 6);
        assert_eq!(write(&db, 2, 4, vec![]).unwrap(), 3);
        assert_eq!(page(&db, "s", None).unwrap().context.covered, 0);
        assert!(!store_summary(&db, &context, 0, 6, "stale summary").unwrap());
        assert_eq!(write(&db, 2, 4, vec![]).unwrap_err(), "TRANSCRIPT_CHANGED");
    }

    #[test]
    fn usage_breakdown_survives_reopen_without_reinterpreting_old_snapshots() {
        let db = database(&[]);
        page(&db, "s", None).unwrap();
        let legacy = json!({"covered":0,"compacted":false,"estimatedTokens":20241,"inputBudget":33967});
        db.execute("UPDATE ai_hub_transcript_state SET usage = ?1 WHERE session_id = 's'", [legacy.to_string()]).unwrap();
        let old = page(&db, "s", None).unwrap().context;
        assert_eq!(old.estimated_tokens, Some(20241));
        assert!(old.budget.is_none() && old.total_input_tokens.is_none());
        let config: crate::ai_commands::AIModelConfig = serde_json::from_value(json!({
            "name":"Current model","provider":"custom","apiKey":"","baseUrl":"http://localhost/v1", "model":"fixture",
            "temperature":0.0,"maxTokens":4096,"isDefault":false,"contextWindowTokens":1000000,"outputBudgetMode":"auto"
        })).unwrap();
        let budget = crate::model_budget::resolve_budget(&config, 0, 5000, None).unwrap();
        let info = ContextInfo { estimated_tokens:Some(20241), input_budget:Some(budget.input_budget), ..Default::default() }.with_budget(budget);
        db.execute("UPDATE ai_hub_transcript_state SET usage = ?1 WHERE session_id = 's'", [serde_json::to_string(&info).unwrap()]).unwrap();
        let reopened = page(&db, "s", None).unwrap().context;
        assert_eq!(reopened.total_input_tokens, Some(25241));
        assert_eq!(reopened.budget.unwrap().effective_window, 1000000);
        assert_eq!(reopened.usage_kind.as_deref(), Some("request"));
    }
}
