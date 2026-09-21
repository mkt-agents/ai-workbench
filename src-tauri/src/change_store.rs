//! Storing change-regression reports.
//!
//! rusqlite only — no tauri types — so these queries can be compiled and *run* from a
//! scratch crate even when the app's own test binary cannot start on this machine.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Created by `lib.rs` at startup alongside the other test tables.
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS change_reports (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    base TEXT NOT NULL,
    source TEXT NOT NULL,
    created_at TEXT NOT NULL,
    branch TEXT NOT NULL DEFAULT '',
    head TEXT NOT NULL DEFAULT '',
    files INTEGER NOT NULL DEFAULT 0,
    adds INTEGER NOT NULL DEFAULT 0,
    dels INTEGER NOT NULL DEFAULT 0,
    untested INTEGER NOT NULL DEFAULT 0,
    data TEXT NOT NULL,
    patch TEXT NOT NULL DEFAULT '',
    ai TEXT,
    FOREIGN KEY (project_id) REFERENCES test_projects(id) ON DELETE CASCADE
);
"#;

/// Newest reports kept per project before the oldest are dropped.
pub const RETENTION_PER_PROJECT: i64 = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeReportSummary {
    pub id: String,
    pub project_id: String,
    pub base: String,
    pub source: String,
    pub created_at: String,
    pub branch: String,
    pub files: i64,
    pub adds: i64,
    pub dels: i64,
    pub untested: i64,
    pub has_ai: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredChangeReport {
    pub summary: ChangeReportSummary,
    pub report: serde_json::Value,
    pub ai: Option<String>,
    /// Acceptance checklist parsed from `ai` plus whatever the tester added by hand.
    pub scenarios: Vec<crate::test_scenarios::Scenario>,
    pub scenario_summary: crate::test_scenarios::ScenarioSummary,
    /// Runs that were executed for this report.
    pub runs: Vec<crate::test_scenarios::RunLink>,
}

/// One row's worth of what the collector produced.
pub struct NewReport<'a> {
    /// Caller-generated primary key (`cr-<millis>`), so two runs never collide on a
    /// timestamp the store invented.
    pub id: &'a str,
    pub project_id: &'a str,
    pub base: &'a str,
    pub source: &'a str,
    pub created_at: &'a str,
    pub branch: &'a str,
    pub head: &'a str,
    pub files: i64,
    pub adds: i64,
    pub dels: i64,
    pub untested: i64,
    pub data: &'a str,
    pub patch: &'a str,
}

/// `id, project_id, …` — the order `row_to_summary` reads. `ai` is last so `has_ai`
/// is just "is that column non-null".
const SUMMARY_COLUMNS: &str = "id, project_id, base, source, created_at, branch, files, adds, dels, untested, ai";

fn row_to_summary(row: &rusqlite::Row) -> rusqlite::Result<ChangeReportSummary> {
    Ok(ChangeReportSummary {
        id: row.get(0)?,
        project_id: row.get(1)?,
        base: row.get(2)?,
        source: row.get(3)?,
        created_at: row.get(4)?,
        branch: row.get(5)?,
        files: row.get(6)?,
        adds: row.get(7)?,
        dels: row.get(8)?,
        untested: row.get(9)?,
        has_ai: row.get::<_, Option<String>>(10)?.is_some(),
    })
}

pub fn save(conn: &Connection, report: &NewReport) -> Result<(), String> {
    conn.execute(
        "INSERT INTO change_reports
            (id, project_id, base, source, created_at, branch, head, files, adds, dels, untested, data, patch)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            report.id,
            report.project_id,
            report.base,
            report.source,
            report.created_at,
            report.branch,
            report.head,
            report.files,
            report.adds,
            report.dels,
            report.untested,
            report.data,
            report.patch,
        ],
    )
    .map_err(|e| e.to_string())?;
    prune(conn, report.project_id, RETENTION_PER_PROJECT)?;
    Ok(())
}

/// Drop everything but the newest `keep` reports of one project. `rowid` breaks ties
/// for rows written within the same second.
pub fn prune(conn: &Connection, project_id: &str, keep: i64) -> Result<usize, String> {
    conn.execute(
        "DELETE FROM change_reports WHERE project_id = ?1 AND id NOT IN (
             SELECT id FROM change_reports WHERE project_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2
         )",
        params![project_id, keep],
    )
    .map_err(|e| e.to_string())
}

pub fn list(conn: &Connection, project_id: &str, limit: i64) -> Result<Vec<ChangeReportSummary>, String> {
    let sql = format!(
        "SELECT {} FROM change_reports WHERE project_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        SUMMARY_COLUMNS
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project_id, limit], row_to_summary)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn get(conn: &Connection, report_id: &str) -> Result<StoredChangeReport, String> {
    let sql = format!("SELECT {}, data, ai FROM change_reports WHERE id = ?1", SUMMARY_COLUMNS);
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let (summary, data, ai) = stmt
        .query_row(params![report_id], |row| {
            Ok((row_to_summary(row)?, row.get::<_, String>(11)?, row.get::<_, Option<String>>(12)?))
        })
        .map_err(|_| format!("报告不存在: {}", report_id))?;
    Ok(StoredChangeReport {
        scenarios: crate::test_scenarios::list_scenarios(conn, report_id).unwrap_or_default(),
        scenario_summary: crate::test_scenarios::summary(conn, report_id).unwrap_or_default(),
        runs: crate::test_scenarios::runs_for_report(conn, report_id).unwrap_or_default(),
        summary,
        report: serde_json::from_str(&data).unwrap_or(serde_json::Value::Null),
        ai,
    })
}

pub fn set_ai(conn: &Connection, report_id: &str, ai: &str) -> Result<(), String> {
    let changed = conn
        .execute("UPDATE change_reports SET ai = ?2 WHERE id = ?1", params![report_id, ai])
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("报告不存在: {}", report_id));
    }
    Ok(())
}

/// What the model needs: the stored analysis plus the bounded patch, and the owning
/// project so the prompt can name it.
pub fn ai_input(conn: &Connection, report_id: &str) -> Result<(String, String, String, String), String> {
    conn.query_row(
        "SELECT data, patch, project_id, created_at FROM change_reports WHERE id = ?1",
        params![report_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .map_err(|_| format!("报告不存在: {}", report_id))
}

pub fn project_name(conn: &Connection, project_id: &str) -> String {
    conn.query_row(
        "SELECT name FROM test_projects WHERE id = ?1",
        params![project_id],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|_| project_id.to_string())
}

pub fn delete(conn: &Connection, report_id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM change_reports WHERE id = ?1", params![report_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        // The parent table is only needed for the FK clause; the cascade itself is DDL.
        db.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        db.execute_batch(&format!(
            "{} CREATE TABLE IF NOT EXISTS test_projects (id TEXT PRIMARY KEY, name TEXT NOT NULL);",
            SCHEMA_SQL
        ))
        .unwrap();
        db.execute("INSERT INTO test_projects (id, name) VALUES ('p1', 'demo')", []).unwrap();
        db
    }

    fn report<'a>(id: &'a str, created_at: &'a str) -> NewReport<'a> {
        NewReport {
            id,
            project_id: "p1",
            base: "",
            source: "uncommitted",
            created_at,
            branch: "master",
            head: "abc1234",
            files: 3,
            adds: 10,
            dels: 2,
            untested: 1,
            data: r#"{"stats":{"files":3},"scope":["apiContract"]}"#,
            patch: "@@ -1 +1 @@",
        }
    }

    #[test]
    fn a_saved_report_reads_back_with_every_field_in_place() {
        let db = conn();
        save(&db, &report("cr-1", "2026-09-21T10:00:00Z")).unwrap();
        let stored = get(&db, "cr-1").unwrap();

        assert_eq!(stored.summary.id, "cr-1");
        assert_eq!(stored.summary.project_id, "p1");
        assert_eq!(stored.summary.branch, "master");
        assert_eq!(
            (stored.summary.files, stored.summary.adds, stored.summary.dels, stored.summary.untested),
            (3, 10, 2, 1)
        );
        assert!(!stored.summary.has_ai, "a fresh report has no AI section");
        assert_eq!(stored.report["stats"]["files"], 3, "the stored JSON survives the round trip");
        assert_eq!(stored.report["scope"][0], "apiContract");
        assert!(stored.ai.is_none());

        let rows = list(&db, "p1", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "cr-1");
    }

    #[test]
    fn the_ai_step_updates_the_same_row_it_was_generated_from() {
        let db = conn();
        save(&db, &report("cr-2", "2026-09-21T10:00:00Z")).unwrap();
        set_ai(&db, "cr-2", "## 受影响功能点\n- 登录").unwrap();

        let stored = get(&db, "cr-2").unwrap();
        assert!(stored.summary.has_ai);
        assert_eq!(stored.ai.unwrap(), "## 受影响功能点\n- 登录");

        let (data, patch, project_id, _) = ai_input(&db, "cr-2").unwrap();
        assert!(data.contains("\"files\":3"), "got: {}", data);
        assert_eq!(patch, "@@ -1 +1 @@");
        assert_eq!(project_id, "p1");
        assert_eq!(project_name(&db, "p1"), "demo");

        assert!(set_ai(&db, "cr-missing", "x").is_err(), "an unknown id must fail loudly");
        assert!(get(&db, "cr-missing").is_err());
        assert!(ai_input(&db, "cr-missing").is_err());
    }

    #[test]
    fn retention_keeps_the_newest_and_leaves_other_projects_alone() {
        let db = conn();
        for (index, id) in ["1", "2", "3", "4"].iter().enumerate() {
            let time = ["2026-09-21T10:00:00Z", "2026-09-21T10:01:00Z", "2026-09-21T10:02:00Z", "2026-09-21T10:03:00Z"][index];
            save(&db, &report(&format!("cr-{}", id), time)).unwrap();
        }
        assert_eq!(list(&db, "p1", 10).unwrap().len(), 4, "under the cap, nothing is dropped");

        prune(&db, "p1", 2).unwrap();
        let kept = list(&db, "p1", 10).unwrap();
        assert_eq!(
            kept.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["cr-4", "cr-3"],
            "newest first, oldest dropped"
        );

        db.execute(
            "INSERT INTO change_reports (id, project_id, base, source, created_at, data) VALUES ('x','p2','','','t','{}')",
            [],
        )
        .unwrap();
        prune(&db, "p1", 1).unwrap();
        assert_eq!(list(&db, "p1", 10).unwrap().len(), 1);
        assert_eq!(list(&db, "p2", 10).unwrap().len(), 1, "pruning one project cannot touch another");

        delete(&db, "x").unwrap();
        assert!(list(&db, "p2", 10).unwrap().is_empty());
    }
}
