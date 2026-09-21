//! Acceptance scenarios and the link between a change report and the runs made for it.
//!
//! rusqlite only (no tauri), so the queries and the markdown→rows parser can be compiled
//! and *run* from a scratch crate.
//!
//! The scenario rows come from parsing the AI section rather than asking the model for
//! JSON: a markdown bullet list is what the prompt already produces, and parsing it
//! cannot fail because a model ignored an output-format instruction.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS test_scenarios (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    report_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    run_id TEXT,
    title TEXT NOT NULL,
    detail TEXT,
    priority TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending',
    note TEXT,
    sort INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL DEFAULT 'ai',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (report_id) REFERENCES change_reports(id) ON DELETE CASCADE,
    FOREIGN KEY (project_id) REFERENCES test_projects(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_test_scenarios_report ON test_scenarios(report_id, sort);
CREATE TABLE IF NOT EXISTS change_report_runs (
    report_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (report_id, run_id),
    FOREIGN KEY (report_id) REFERENCES change_reports(id) ON DELETE CASCADE,
    FOREIGN KEY (run_id) REFERENCES test_runs(id) ON DELETE CASCADE,
    FOREIGN KEY (project_id) REFERENCES test_projects(id) ON DELETE CASCADE
);
"#;

/// The vocabulary the UI renders; anything else is rejected at the door.
pub const STATUSES: [&str; 4] = ["pending", "passed", "failed", "blocked"];

pub fn is_valid_status(status: &str) -> bool {
    STATUSES.contains(&status)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scenario {
    pub id: i64,
    pub report_id: String,
    pub project_id: String,
    pub run_id: Option<String>,
    pub title: String,
    pub detail: Option<String>,
    pub priority: String,
    pub status: String,
    pub note: Option<String>,
    pub sort: i64,
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewScenario {
    pub title: String,
    pub detail: Option<String>,
    pub priority: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioSummary {
    pub total: i64,
    pub passed: i64,
    pub failed: i64,
    pub blocked: i64,
    pub pending: i64,
    /// Rounded passed share, 0..=100. Computed on read so the UI shows one number
    /// without re-deriving it. An empty checklist is 0, not "fully accepted".
    pub percent: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLink {
    pub run_id: String,
    pub project_id: String,
    pub created_at: String,
    pub status: String,
    pub total_tests: i64,
    pub passed: i64,
    pub failed: i64,
    pub error_kind: String,
}

/// Section titles that hold acceptance items, in either language the model may answer
/// in. Matched case-insensitively after trimming.
const ACCEPTANCE_TITLES: &[&str] = &[
    "必测场景",
    "验收清单",
    "must-test scenarios",
    "acceptance checklist",
    "test scenarios",
];

/// Pull the acceptance bullets out of the AI markdown.
///
/// Continuation lines (indented under a bullet) become `detail`, and a leading
/// `P0`/`P1`/`P2` becomes `priority` so the UI can group by it.
pub fn scenarios_from_markdown(markdown: &str) -> Vec<NewScenario> {
    let mut out: Vec<NewScenario> = Vec::new();
    let mut in_section = false;
    let mut current: Option<NewScenario> = None;

    let flush = |current: &mut Option<NewScenario>, out: &mut Vec<NewScenario>| {
        if let Some(row) = current.take() {
            if !row.title.is_empty() && !out.iter().any(|existing| existing.title == row.title) {
                out.push(row);
            }
        }
    };

    for raw in markdown.lines() {
        let line = raw.trim_end();
        if let Some(heading) = line.strip_prefix("##").map(|rest| rest.trim()) {
            flush(&mut current, &mut out);
            let lower = heading.to_lowercase();
            in_section = ACCEPTANCE_TITLES.iter().any(|title| lower.contains(title));
            continue;
        }
        if !in_section {
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(bullet) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            flush(&mut current, &mut out);
            current = Some(scenario_from_bullet(bullet.trim()));
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut current, &mut out);
            continue;
        }
        // Indented prose under a bullet is the scenario's steps/expected result.
        if current.is_some() && (line.starts_with(' ') || line.starts_with('\t')) {
            if let Some(row) = current.as_mut() {
                let detail = row.detail.get_or_insert_with(String::new);
                if !detail.is_empty() {
                    detail.push('\n');
                }
                detail.push_str(trimmed);
            }
        }
    }
    flush(&mut current, &mut out);
    out
}

fn scenario_from_bullet(bullet: &str) -> NewScenario {
    let (priority, rest) = match bullet.split_once(char::is_whitespace) {
        Some((head, tail))
            if matches!(head.trim_end_matches([':', '）', ')', '】', ']', '-']).to_uppercase().as_str(), "P0" | "P1" | "P2" | "P3") =>
        {
            (
                head.trim_end_matches([':', '）', ')', '】', ']', '-']).to_uppercase(),
                tail.trim_start_matches([' ', ':', '：', '-', '—']).to_string(),
            )
        }
        _ => (String::new(), bullet.to_string()),
    };
    // Models write one line per scenario, so the precondition and the expectation arrive
    // glued together. Splitting them keeps the row title scannable and moves the expected
    // result into the detail line the UI already renders under it.
    let (title, expectation) = split_expectation(&rest);
    NewScenario {
        title: trim_title(strip_precondition(&title)),
        detail: expectation.map(trim_detail),
        priority,
    }
}

const PRECONDITION_LABELS: [&str; 6] = ["[前置]", "【前置】", "前置：", "前置:", "前置=", "precondition:"];
const EXPECTATION_LABELS: [&str; 7] =
    ["[预期]", "【预期】", "预期：", "预期:", "预期=", "expected:", "expect:"];

fn split_expectation(text: &str) -> (String, Option<String>) {
    for label in EXPECTATION_LABELS {
        if let Some((before, after)) = text.split_once(label) {
            let after = after.trim();
            if !after.is_empty() {
                return (before.to_string(), Some(after.to_string()));
            }
        }
    }
    (text.to_string(), None)
}

fn strip_precondition(text: &str) -> String {
    let trimmed = text.trim_start();
    for label in PRECONDITION_LABELS {
        if let Some(rest) = trimmed.strip_prefix(label) {
            return rest.trim_start_matches([' ', ':', '：', '=']).to_string();
        }
    }
    trimmed.to_string()
}

/// A trailing "前置=xxx，" separator is punctuation the split left behind.
fn trim_title(text: String) -> String {
    let cleaned = text.trim().trim_end_matches(['，', ',', '、', '；', ';', '：', ':']).trim().to_string();
    cleaned.chars().take(160).collect::<String>().trim().to_string()
}

fn trim_detail(text: String) -> String {
    text.trim().trim_end_matches(['，', ';']).trim().to_string()
}

/// Replace a report's scenarios with a freshly parsed set. Idempotent by title, so
/// pressing "生成验收清单" twice does not double the list.
pub fn save_scenarios(
    conn: &Connection,
    report_id: &str,
    project_id: &str,
    rows: &[NewScenario],
    now: &str,
) -> Result<usize, String> {
    let existing: Vec<String> = conn
        .prepare("SELECT title FROM test_scenarios WHERE report_id = ?1")
        .map_err(|e| e.to_string())?
        .query_map(params![report_id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut sort: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(sort), 0) FROM test_scenarios WHERE report_id = ?1",
            params![report_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let mut inserted = 0usize;
    for row in rows {
        if row.title.is_empty() || existing.contains(&row.title) {
            continue;
        }
        sort += 1;
        conn.execute(
            "INSERT INTO test_scenarios (report_id, project_id, title, detail, priority, status, sort, source, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, 'ai', ?7, ?7)",
            params![report_id, project_id, row.title, row.detail, row.priority, sort, now],
        )
        .map_err(|e| format!("写入验收场景失败: {}", e))?;
        inserted += 1;
    }
    Ok(inserted)
}

/// A manually added acceptance item, for the cases the model did not think of.
pub fn add_manual_scenario(
    conn: &Connection,
    report_id: &str,
    project_id: &str,
    title: &str,
    now: &str,
) -> Result<i64, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("场景标题不能为空".to_string());
    }
    conn.execute(
        "INSERT INTO test_scenarios (report_id, project_id, title, priority, status, sort, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, '', 'pending', (SELECT COALESCE(MAX(sort), 0) + 1 FROM test_scenarios WHERE report_id = ?1), 'manual', ?4, ?4)",
        params![report_id, project_id, title, now],
    )
    .map_err(|e| format!("新增场景失败: {}", e))?;
    Ok(conn.last_insert_rowid())
}

pub fn list_scenarios(conn: &Connection, report_id: &str) -> Result<Vec<Scenario>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, report_id, project_id, run_id, title, detail, priority, status, note, sort, source, created_at, updated_at
             FROM test_scenarios WHERE report_id = ?1 ORDER BY sort, id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![report_id], scenario_from_row)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn scenario_from_row(row: &rusqlite::Row) -> rusqlite::Result<Scenario> {
    Ok(Scenario {
        id: row.get(0)?,
        report_id: row.get(1)?,
        project_id: row.get(2)?,
        run_id: row.get(3)?,
        title: row.get(4)?,
        detail: row.get(5)?,
        priority: row.get(6)?,
        status: row.get(7)?,
        note: row.get(8)?,
        sort: row.get(9)?,
        source: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

/// Record the outcome of one acceptance item. An unknown id fails loudly: silently
/// losing a tester's tick would be worse than an error message.
pub fn set_status(
    conn: &Connection,
    scenario_id: i64,
    status: &str,
    note: Option<&str>,
    run_id: Option<&str>,
    now: &str,
) -> Result<(), String> {
    if !is_valid_status(status) {
        return Err(format!("未知的场景状态: {}", status));
    }
    let changed = conn
        .execute(
            "UPDATE test_scenarios
             SET status = ?2, note = COALESCE(?3, note), run_id = COALESCE(?4, run_id), updated_at = ?5
             WHERE id = ?1",
            params![scenario_id, status, note, run_id, now],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("场景不存在: {}", scenario_id));
    }
    Ok(())
}

pub fn delete_scenario(conn: &Connection, scenario_id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM test_scenarios WHERE id = ?1", params![scenario_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn summary(conn: &Connection, report_id: &str) -> Result<ScenarioSummary, String> {
    let mut out = ScenarioSummary::default();
    let mut stmt = conn
        .prepare("SELECT status, COUNT(*) FROM test_scenarios WHERE report_id = ?1 GROUP BY status")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![report_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in rows.flatten() {
        out.total += row.1;
        match row.0.as_str() {
            "passed" => out.passed = row.1,
            "failed" => out.failed = row.1,
            "blocked" => out.blocked = row.1,
            _ => out.pending += row.1,
        }
    }
    out.percent = if out.total == 0 {
        0
    } else {
        ((out.passed as f64 / out.total as f64) * 100.0).round() as u32
    };
    Ok(out)
}

/// Note that a run was made *for* this change report, so the report can show what has
/// already been executed against it.
pub fn link_run(conn: &Connection, report_id: &str, run_id: &str, project_id: &str, now: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR IGNORE INTO change_report_runs (report_id, run_id, project_id, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![report_id, run_id, project_id, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn runs_for_report(conn: &Connection, report_id: &str) -> Result<Vec<RunLink>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT l.run_id, l.project_id, l.created_at, r.status, r.total_tests, r.passed, r.failed, r.error_kind
             FROM change_report_runs l JOIN test_runs r ON r.id = l.run_id
             WHERE l.report_id = ?1 ORDER BY l.created_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![report_id], |row| {
            Ok(RunLink {
                run_id: row.get(0)?,
                project_id: row.get(1)?,
                created_at: row.get(2)?,
                status: row.get(3)?,
                total_tests: row.get(4)?,
                passed: row.get(5)?,
                failed: row.get(6)?,
                error_kind: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        db.execute_batch(
            "CREATE TABLE test_projects (id TEXT PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE test_runs (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, started_at TEXT, completed_at TEXT,
                duration_ms INTEGER, status TEXT, total_tests INTEGER, passed INTEGER, failed INTEGER,
                skipped INTEGER, output TEXT, suites TEXT, error_kind TEXT DEFAULT '');",
        )
        .unwrap();
        // The product DDL declares the parents; create them in the same order.
        db.execute_batch(&change_reports_ddl()).unwrap();
        db.execute_batch(SCHEMA_SQL).unwrap();
        db.execute("INSERT INTO test_projects (id, name) VALUES ('p1', 'demo')", []).unwrap();
        db.execute(
            "INSERT INTO change_reports (id, project_id, base, source, created_at, data) VALUES ('cr-1','p1','','uncommitted','t','{}')",
            [],
        )
        .unwrap();
        db
    }

    /// `change_reports` lives in `change_store`; the test only needs its shape.
    fn change_reports_ddl() -> String {
        use crate::change_store::SCHEMA_SQL as CHANGE_SCHEMA;
        CHANGE_SCHEMA.to_string()
    }

    const AI_MARKDOWN: &str = "## 受影响功能点\n- 账户注册\n\n## 必测场景\n- P0 注册时绑定推荐码：前置=有效推荐码，预期=referral 表新增记录\n  校验 hash 与旧种子不冲突\n- P1 管理员登录失败锁定\n- 普通场景没有优先级\n\n## 验收清单\n* 数据迁移脚本在预发环境跑通\n\n## 兼容性与数据风险\n- 不该被采集的一条\n";

    #[test]
    fn acceptance_bullets_become_scenarios_with_priorities() {
        let rows = scenarios_from_markdown(AI_MARKDOWN);
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "注册时绑定推荐码：前置=有效推荐码",
                "管理员登录失败锁定",
                "普通场景没有优先级",
                "数据迁移脚本在预发环境跑通",
            ],
            "only the acceptance sections are collected: {:?}",
            titles
        );
        assert_eq!(rows[0].priority, "P0");
        assert_eq!(rows[1].priority, "P1");
        assert_eq!(rows[2].priority, "");
        assert_eq!(
            rows[0].detail.as_deref(),
            Some("referral 表新增记录\n校验 hash 与旧种子不冲突"),
            "the split-off expectation and the indented prose share the detail line"
        );
        assert_eq!(rows[1].detail, None, "a bullet with no expected result has no detail");
    }

    #[test]
    fn a_one_line_precondition_splits_into_title_and_detail() {
        // Straight out of a real report: `[前置] … [预期] …`, once with a priority, once without.
        let rows = scenarios_from_markdown(
            "## 必测场景\n- [前置] 切换网价城市不重新查价 [预期] 缓存中仅更新网价字段，报价保持不变\n- P0 【前置】有有效推荐码【预期】绑定成功并写入 referral 表\n",
        );
        assert_eq!(rows[0].title, "切换网价城市不重新查价");
        assert_eq!(
            rows[0].detail.as_deref(),
            Some("缓存中仅更新网价字段，报价保持不变")
        );
        assert_eq!(rows[1].priority, "P0");
        assert_eq!(rows[1].title, "有有效推荐码");
        assert_eq!(rows[1].detail.as_deref(), Some("绑定成功并写入 referral 表"));
    }

    #[test]
    fn saving_is_idempotent_and_the_summary_tracks_the_ticks() {
        let db = conn();
        let rows = scenarios_from_markdown(AI_MARKDOWN);
        assert_eq!(save_scenarios(&db, "cr-1", "p1", &rows, "t1").unwrap(), 4);
        assert_eq!(save_scenarios(&db, "cr-1", "p1", &rows, "t2").unwrap(), 0, "no duplicates by title");

        let listed = list_scenarios(&db, "cr-1").unwrap();
        assert_eq!(listed.len(), 4);
        assert_eq!(listed[0].status, "pending");
        assert_eq!(summary(&db, "cr-1").unwrap(), ScenarioSummary { total: 4, pending: 4, ..Default::default() });
        assert_eq!(summary(&db, "cr-1").unwrap().percent, 0);

        set_status(&db, listed[0].id, "passed", Some("预发验证"), None, "t3").unwrap();
        set_status(&db, listed[1].id, "failed", None, Some("run-9"), "t3").unwrap();
        let sum = summary(&db, "cr-1").unwrap();
        assert_eq!((sum.total, sum.passed, sum.failed, sum.pending), (4, 1, 1, 2));
        assert_eq!(sum.percent, 25);
        assert_eq!(list_scenarios(&db, "cr-1").unwrap()[0].note.as_deref(), Some("预发验证"));
        assert_eq!(list_scenarios(&db, "cr-1").unwrap()[1].run_id.as_deref(), Some("run-9"));
    }

    #[test]
    fn bad_status_and_unknown_id_are_rejected() {
        let db = conn();
        save_scenarios(&db, "cr-1", "p1", &scenarios_from_markdown(AI_MARKDOWN), "t").unwrap();
        let id = list_scenarios(&db, "cr-1").unwrap()[0].id;
        assert!(set_status(&db, id, "done", None, None, "t").is_err(), "vocabulary is closed");
        assert!(set_status(&db, 9999, "passed", None, None, "t").is_err());
        assert!(add_manual_scenario(&db, "cr-1", "p1", "   ", "t").is_err());
    }

    #[test]
    fn manual_entries_and_deletes_keep_the_order_sane() {
        let db = conn();
        save_scenarios(&db, "cr-1", "p1", &scenarios_from_markdown(AI_MARKDOWN), "t").unwrap();
        add_manual_scenario(&db, "cr-1", "p1", "回滚脚本演练", "t").unwrap();
        let listed = list_scenarios(&db, "cr-1").unwrap();
        assert_eq!(listed.len(), 5);
        assert_eq!(listed[4].title, "回滚脚本演练");
        assert_eq!(listed[4].source, "manual");
        assert!(listed[4].sort > listed[3].sort);
        delete_scenario(&db, listed[4].id).unwrap();
        assert_eq!(list_scenarios(&db, "cr-1").unwrap().len(), 4);
    }

    #[test]
    fn a_run_links_to_the_report_and_carries_its_outcome() {
        let db = conn();
        db.execute(
            "INSERT INTO test_runs (id, project_id, status, total_tests, passed, failed, error_kind) VALUES ('run-1','p1','failed',10,8,2,'')",
            [],
        )
        .unwrap();
        link_run(&db, "cr-1", "run-1", "p1", "t").unwrap();
        link_run(&db, "cr-1", "run-1", "p1", "t").unwrap();
        let runs = runs_for_report(&db, "cr-1").unwrap();
        assert_eq!(runs.len(), 1, "the link is unique per (report, run)");
        assert_eq!((runs[0].status.as_str(), runs[0].passed, runs[0].failed), ("failed", 8, 2));
    }

    #[test]
    fn deleting_the_report_takes_its_scenarios_with_it() {
        let db = conn();
        save_scenarios(&db, "cr-1", "p1", &scenarios_from_markdown(AI_MARKDOWN), "t").unwrap();
        db.execute("DELETE FROM change_reports WHERE id = 'cr-1'", []).unwrap();
        assert!(list_scenarios(&db, "cr-1").unwrap().is_empty(), "FK cascade must clean up");
    }

    #[test]
    fn a_bullet_list_without_sections_still_works() {
        let rows = scenarios_from_markdown("- 只有一行\n- 另一行\n");
        assert!(rows.is_empty(), "no section header means nothing is claimed as acceptance");
        let rows = scenarios_from_markdown("## Must-test scenarios\n- P0 login works\n");
        assert_eq!(rows[0].title, "login works");
        assert_eq!(rows[0].priority, "P0");
    }
}
