//! Storing vulnerability scans and findings.
//!
//! rusqlite only (same rule as `change_store`/`test_scenarios`) so the whole
//! layer — including the upsert semantics — can be *run* from the probe crate.
//!
//! The one design decision worth stating: findings are keyed by a stable
//! `dedup_key` per project, not per scan. A re-scan therefore updates what is
//! true today (`last_seen`, severity, fixed versions) while leaving the
//! tester's judgement (`ignored` / `false_positive`) alone. A dependency that
//! was marked fixed but shows up again flips back to `open` — the scan, not
//! the human, is the source of truth for presence. Secrets are never
//! auto-fixed: absence from one scan proves nothing about the history of a
//! file we may simply have stopped tracking.

use rusqlite::{params, Connection};
use serde::Serialize;

/// Created by `lib.rs` at startup alongside the other tables.
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS vuln_scans (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    status TEXT NOT NULL,
    error_kind TEXT NOT NULL DEFAULT '',
    deps_checked INTEGER NOT NULL DEFAULT 0,
    files_checked INTEGER NOT NULL DEFAULT 0,
    findings_critical INTEGER NOT NULL DEFAULT 0,
    findings_high INTEGER NOT NULL DEFAULT 0,
    findings_total INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (project_id) REFERENCES test_projects(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS vuln_findings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL,
    scan_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    dedup_key TEXT NOT NULL,
    ecosystem TEXT NOT NULL DEFAULT '',
    package TEXT NOT NULL DEFAULT '',
    version TEXT NOT NULL DEFAULT '',
    vuln_id TEXT NOT NULL DEFAULT '',
    severity TEXT NOT NULL DEFAULT 'unknown',
    summary TEXT NOT NULL DEFAULT '',
    fixed_versions TEXT NOT NULL DEFAULT '[]',
    aliases TEXT NOT NULL DEFAULT '[]',
    file TEXT NOT NULL DEFAULT '',
    line INTEGER NOT NULL DEFAULT 0,
    preview TEXT NOT NULL DEFAULT '',
    rule TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'open',
    first_seen TEXT NOT NULL,
    last_seen TEXT NOT NULL,
    UNIQUE (project_id, dedup_key),
    FOREIGN KEY (project_id) REFERENCES test_projects(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_vuln_findings_project ON vuln_findings (project_id, status);
"#;

/// Run states reuse the five-value vocabulary of the test module, plus the
/// transient `running`. Only referenced by the test that keeps it closed.
#[allow(dead_code)]
pub const SCAN_STATUSES: &[&str] = &["running", "success", "failed", "error", "cancelled", "timeout"];
/// Human-decidable states of a finding; `store` is the only gate.
pub const FINDING_STATUSES: &[&str] = &["open", "fixed", "ignored", "false_positive"];
/// Scans kept per project before the oldest are dropped.
pub const SCAN_RETENTION: i64 = 20;

pub fn is_valid_status(status: &str) -> bool {
    FINDING_STATUSES.contains(&status)
}

/// Dependency = `eco|package|vulnId`; secret = `sec|file|rule|digest`.
pub fn dependency_key(ecosystem: &str, package: &str, vuln_id: &str) -> String {
    format!("{}|{}|{}", ecosystem, package, vuln_id)
}

pub fn secret_key(file: &str, rule: &str, digest: &str) -> String {
    format!("sec|{}|{}|{}", file, rule, digest)
}

/// One row of `vuln_findings`, shaped for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: i64,
    pub project_id: String,
    pub scan_id: String,
    pub kind: String,
    pub dedup_key: String,
    pub ecosystem: String,
    pub package: String,
    pub version: String,
    pub vuln_id: String,
    pub severity: String,
    pub summary: String,
    pub fixed_versions: Vec<String>,
    pub aliases: Vec<String>,
    pub file: String,
    pub line: i64,
    pub preview: String,
    pub rule: String,
    pub status: String,
    pub first_seen: String,
    pub last_seen: String,
}

/// What the scanner hands over per hit; `first_seen` defaults to `last_seen`.
#[derive(Debug, Clone)]
pub struct NewFinding<'a> {
    pub kind: &'a str,
    pub dedup_key: &'a str,
    pub ecosystem: &'a str,
    pub package: &'a str,
    pub version: &'a str,
    pub vuln_id: &'a str,
    pub severity: &'a str,
    pub summary: &'a str,
    pub fixed_versions: Vec<String>,
    pub aliases: Vec<String>,
    pub file: &'a str,
    pub line: i64,
    pub preview: &'a str,
    pub rule: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub id: String,
    pub project_id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub status: String,
    pub error_kind: String,
    pub deps_checked: i64,
    pub files_checked: i64,
    pub findings_critical: i64,
    pub findings_high: i64,
    pub findings_total: i64,
}

pub fn create_scan(conn: &Connection, id: &str, project_id: &str, started_at: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO vuln_scans (id, project_id, started_at, status) VALUES (?1, ?2, ?3, 'running')",
        params![id, project_id, started_at],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn finish_scan(
    conn: &Connection,
    id: &str,
    status: &str,
    error_kind: &str,
    deps_checked: i64,
    files_checked: i64,
    completed_at: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE vuln_scans SET status = ?2, error_kind = ?3, deps_checked = ?4, files_checked = ?5, completed_at = ?6 WHERE id = ?1",
        params![id, status, error_kind, deps_checked, files_checked, completed_at],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Insert-or-refresh one finding. An existing row keeps its human status unless
/// a dependency that was `fixed` came back — then the rescan reopens it.
pub fn upsert_finding(
    conn: &Connection,
    project_id: &str,
    scan_id: &str,
    finding: &NewFinding,
    now: &str,
) -> Result<(), String> {
    let fixed = serde_json::to_string(&finding.fixed_versions).unwrap_or_else(|_| "[]".to_string());
    let aliases = serde_json::to_string(&finding.aliases).unwrap_or_else(|_| "[]".to_string());
    let reopen = if finding.kind == "dependency" { " CASE WHEN status = 'fixed' THEN 'open' ELSE status END" } else { " status" };
    conn.execute(
        &format!(
            "INSERT INTO vuln_findings
                (project_id, scan_id, kind, dedup_key, ecosystem, package, version, vuln_id, severity, summary,
                 fixed_versions, aliases, file, line, preview, rule, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17)
             ON CONFLICT (project_id, dedup_key) DO UPDATE SET
                 scan_id = excluded.scan_id,
                 ecosystem = excluded.ecosystem,
                 package = excluded.package,
                 version = excluded.version,
                 vuln_id = excluded.vuln_id,
                 severity = excluded.severity,
                 summary = excluded.summary,
                 fixed_versions = excluded.fixed_versions,
                 aliases = excluded.aliases,
                 file = excluded.file,
                 line = excluded.line,
                 preview = excluded.preview,
                 rule = excluded.rule,
                 last_seen = excluded.last_seen,
                 status ={}",
            reopen
        ),
        params![
            project_id,
            scan_id,
            finding.kind,
            finding.dedup_key,
            finding.ecosystem,
            finding.package,
            finding.version,
            finding.vuln_id,
            finding.severity,
            finding.summary,
            fixed,
            aliases,
            finding.file,
            finding.line,
            finding.preview,
            finding.rule,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Dependency findings that were `open` but this scan did not see them are
/// fixed now — the version disappeared from the lock file or OSV stopped
/// answering for it. Secrets are deliberately excluded (see module doc).
pub fn mark_missing_dependencies_fixed(conn: &Connection, project_id: &str, seen: &[String], now: &str) -> Result<usize, String> {
    if seen.is_empty() {
        // Nothing seen only means "no dependency findings at all" when the scan
        // actually queried dependencies; guard against wiping on a failed query.
        return Ok(0);
    }
    let placeholders: Vec<String> = (1..=seen.len()).map(|i| format!("?{}", i + 2)).collect();
    let sql = format!(
        "UPDATE vuln_findings SET status = 'fixed', last_seen = ?2
         WHERE project_id = ?1 AND kind = 'dependency' AND status = 'open' AND dedup_key NOT IN ({})",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut args: Vec<Box<dyn rusqlite::ToSql>> =
        vec![Box::new(project_id.to_string()), Box::new(now.to_string())];
    for key in seen {
        args.push(Box::new(key.clone()));
    }
    let changed = stmt
        .execute(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())))
        .map_err(|e| e.to_string())?;
    Ok(changed)
}

/// Human decision on one finding; the four-value word list is the only gate.
pub fn set_finding_status(conn: &Connection, finding_id: i64, status: &str) -> Result<(), String> {
    if !is_valid_status(status) {
        return Err(format!("无法识别的漏洞状态：{}", status));
    }
    let changed = conn
        .execute(
            "UPDATE vuln_findings SET status = ?2 WHERE id = ?1",
            params![finding_id, status],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("漏洞记录不存在: {}", finding_id));
    }
    Ok(())
}

fn row_to_finding(row: &rusqlite::Row) -> rusqlite::Result<Finding> {
    let fixed: String = row.get(11)?;
    let aliases: String = row.get(12)?;
    Ok(Finding {
        id: row.get(0)?,
        project_id: row.get(1)?,
        scan_id: row.get(2)?,
        kind: row.get(3)?,
        dedup_key: row.get(4)?,
        ecosystem: row.get(5)?,
        package: row.get(6)?,
        version: row.get(7)?,
        vuln_id: row.get(8)?,
        severity: row.get(9)?,
        summary: row.get(10)?,
        fixed_versions: serde_json::from_str(&fixed).unwrap_or_default(),
        aliases: serde_json::from_str(&aliases).unwrap_or_default(),
        file: row.get(13)?,
        line: row.get(14)?,
        preview: row.get(15)?,
        rule: row.get(16)?,
        status: row.get(17)?,
        first_seen: row.get(18)?,
        last_seen: row.get(19)?,
    })
}

const FINDING_COLUMNS: &str =
    "id, project_id, scan_id, kind, dedup_key, ecosystem, package, version, vuln_id, severity, summary, fixed_versions, aliases, file, line, preview, rule, status, first_seen, last_seen";

/// Severity order the list defaults to: worst first, then most recently seen.
const FINDING_ORDER: &str = " ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 ELSE 4 END, last_seen DESC";

#[derive(Debug, Clone, Default)]
pub struct FindingFilter {
    pub status: Option<String>,
    pub kind: Option<String>,
    pub severity: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

pub fn list_findings(conn: &Connection, project_id: &str, filter: &FindingFilter) -> Result<Vec<Finding>, String> {
    let mut sql = format!("SELECT {} FROM vuln_findings WHERE project_id = ?1", FINDING_COLUMNS);
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id.to_string())];
    let bind = |sql: &mut String, args: &mut Vec<Box<dyn rusqlite::ToSql>>, column: &str, value: &str| {
        args.push(Box::new(value.to_string()));
        sql.push_str(&format!(" AND {} = ?", column));
        sql.push_str(&args.len().to_string());
    };
    if let Some(status) = &filter.status {
        bind(&mut sql, &mut args, "status", status);
    }
    if let Some(kind) = &filter.kind {
        bind(&mut sql, &mut args, "kind", kind);
    }
    if let Some(severity) = &filter.severity {
        bind(&mut sql, &mut args, "severity", severity);
    }
    sql.push_str(FINDING_ORDER);
    let limit = if filter.limit > 0 { filter.limit } else { 100 };
    args.push(Box::new(limit));
    args.push(Box::new(filter.offset));
    sql.push_str(&format!(" LIMIT ?{} OFFSET ?", args.len() - 1));
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())), row_to_finding)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn count_findings(conn: &Connection, project_id: &str, status: Option<&str>) -> Result<i64, String> {
    let sql = match status {
        Some(_) => "SELECT COUNT(*) FROM vuln_findings WHERE project_id = ?1 AND status = ?2",
        None => "SELECT COUNT(*) FROM vuln_findings WHERE project_id = ?1",
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    match status {
        Some(status) => stmt.query_row(params![project_id, status], |r| r.get(0)),
        None => stmt.query_row(params![project_id], |r| r.get(0)),
    }
    .map_err(|e| e.to_string())
}

/// Roll-up for the header badges: open counts per severity band.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingTotals {
    pub total: i64,
    pub open: i64,
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
    pub unknown: i64,
    pub secrets: i64,
}

pub fn totals(conn: &Connection, project_id: &str) -> Result<FindingTotals, String> {
    let mut stmt = conn
        .prepare("SELECT status, severity, kind, COUNT(*) FROM vuln_findings WHERE project_id = ?1 GROUP BY status, severity, kind")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))
        .map_err(|e| e.to_string())?;
    let mut out = FindingTotals::default();
    for row in rows {
        let (status, severity, kind, count) = row.map_err(|e| e.to_string())?;
        out.total += count;
        if status == "open" {
            out.open += count;
            match severity.as_str() {
                "critical" => out.critical += count,
                "high" => out.high += count,
                "medium" => out.medium += count,
                "low" => out.low += count,
                _ => out.unknown += count,
            }
        }
        if kind == "secret" {
            out.secrets += count;
        }
    }
    Ok(out)
}

/// Read-only link for the change report: how many open findings sit on the
/// packages this change touched (matched by package name, version-agnostic —
/// an old version with an open advisory is worth surfacing either way).
pub fn open_for_packages(conn: &Connection, project_id: &str, packages: &[String]) -> Result<Vec<(String, i64, i64)>, String> {
    let mut out = Vec::new();
    let mut stmt = conn
        .prepare(
            "SELECT COUNT(*), SUM(CASE WHEN severity = 'critical' THEN 1 ELSE 0 END)
             FROM vuln_findings WHERE project_id = ?1 AND kind = 'dependency' AND status = 'open' AND package = ?2",
        )
        .map_err(|e| e.to_string())?;
    for package in packages {
        let (count, critical): (i64, Option<i64>) = stmt
            .query_row(params![project_id, package], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap_or((0, None));
        if count > 0 {
            out.push((package.clone(), count, critical.unwrap_or(0)));
        }
    }
    Ok(out)
}

pub fn list_scans(conn: &Connection, project_id: &str, limit: i64) -> Result<Vec<ScanSummary>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, started_at, completed_at, status, error_kind, deps_checked, files_checked, findings_critical, findings_high, findings_total
             FROM vuln_scans WHERE project_id = ?1 ORDER BY started_at DESC LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project_id, limit], |row| {
            Ok(ScanSummary {
                id: row.get(0)?,
                project_id: row.get(1)?,
                started_at: row.get(2)?,
                completed_at: row.get(3)?,
                status: row.get(4)?,
                error_kind: row.get(5)?,
                deps_checked: row.get(6)?,
                files_checked: row.get(7)?,
                findings_critical: row.get(8)?,
                findings_high: row.get(9)?,
                findings_total: row.get(10)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn prune_scans(conn: &Connection, project_id: &str, keep: i64) -> Result<usize, String> {
    conn.execute(
        "DELETE FROM vuln_scans WHERE project_id = ?1 AND id NOT IN (
             SELECT id FROM vuln_scans WHERE project_id = ?1 ORDER BY started_at DESC LIMIT ?2
         )",
        params![project_id, keep],
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        db.execute_batch(&format!(
            "{} CREATE TABLE IF NOT EXISTS test_projects (id TEXT PRIMARY KEY, name TEXT NOT NULL);",
            SCHEMA_SQL
        ))
        .unwrap();
        db.execute("INSERT INTO test_projects (id, name) VALUES ('p1', 'demo')", []).unwrap();
        db
    }

    fn dep<'a>(key: &'a str, severity: &'a str) -> NewFinding<'a> {
        NewFinding {
            kind: "dependency",
            dedup_key: key,
            ecosystem: "npm",
            package: "axios",
            version: "0.21.1",
            vuln_id: "GHSA-8hc4-vh64-cxmj",
            severity,
            summary: "SSRF",
            fixed_versions: vec!["1.6.0".to_string()],
            aliases: vec!["CVE-2023-45857".to_string()],
            file: "",
            line: 0,
            preview: "",
            rule: "",
        }
    }

    #[test]
    fn a_scan_round_trips_with_counts() {
        let db = conn();
        create_scan(&db, "vs-1", "p1", "2026-09-22T10:00:00Z").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|axios|G1", "critical"), "2026-09-22T10:00:01Z").unwrap();
        finish_scan(&db, "vs-1", "success", "", 412, 900, "2026-09-22T10:00:02Z").unwrap();

        let scans = list_scans(&db, "p1", 10).unwrap();
        assert_eq!(scans.len(), 1);
        assert_eq!(scans[0].status, "success");
        assert_eq!(scans[0].deps_checked, 412);

        let totals = totals(&db, "p1").unwrap();
        assert_eq!((totals.total, totals.open, totals.critical), (1, 1, 1));

        let findings = list_findings(&db, "p1", &FindingFilter::default()).unwrap();
        assert_eq!(findings[0].severity, "critical");
        assert_eq!(findings[0].fixed_versions, vec!["1.6.0".to_string()]);
        assert_eq!(findings[0].aliases, vec!["CVE-2023-45857".to_string()]);
    }

    #[test]
    fn a_rescan_keeps_human_decisions_and_refreshes_facts() {
        let db = conn();
        create_scan(&db, "vs-1", "p1", "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|axios|G1", "high"), "t1").unwrap();
        let id = db.query_row::<i64, _, _>("SELECT id FROM vuln_findings WHERE dedup_key = 'npm|axios|G1'", [], |r| r.get(0)).unwrap();
        set_finding_status(&db, id, "ignored").unwrap();

        create_scan(&db, "vs-2", "p1", "t2").unwrap();
        let mut refreshed = dep("npm|axios|G1", "critical");
        refreshed.summary = "worse than we thought";
        upsert_finding(&db, "p1", "vs-2", &refreshed, "t2").unwrap();

        let rows = list_findings(&db, "p1", &FindingFilter::default()).unwrap();
        assert_eq!(rows.len(), 1, "same dedup key, not a second row");
        assert_eq!(rows[0].status, "ignored", "the tester's judgement survives");
        assert_eq!(rows[0].severity, "critical", "but the facts refresh");
        assert_eq!(rows[0].scan_id, "vs-2");
        assert_eq!(rows[0].first_seen, "t1", "first_seen stays at the original sighting");
        assert_eq!(rows[0].last_seen, "t2");
    }

    #[test]
    fn reopened_dependencies_flip_fixed_back_to_open_but_secrets_stay() {
        let db = conn();
        create_scan(&db, "vs-1", "p1", "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|axios|G1", "high"), "t1").unwrap();
        let mut secret = dep("sec|a.env|jwt|h123", "high");
        secret.kind = "secret";
        secret.package = "";
        upsert_finding(&db, "p1", "vs-1", &secret, "t1").unwrap();

        let dep_id = db.query_row::<i64, _, _>("SELECT id FROM vuln_findings WHERE kind = 'dependency'", [], |r| r.get(0)).unwrap();
        let secret_id = db.query_row::<i64, _, _>("SELECT id FROM vuln_findings WHERE kind = 'secret'", [], |r| r.get(0)).unwrap();
        set_finding_status(&db, dep_id, "fixed").unwrap();
        set_finding_status(&db, secret_id, "fixed").unwrap();

        // Same advisory answers again: the dependency is *not* fixed.
        upsert_finding(&db, "p1", "vs-1", &dep("npm|axios|G1", "high"), "t2").unwrap();
        let rows = list_findings(&db, "p1", &FindingFilter::default()).unwrap();
        let dep = rows.iter().find(|f| f.kind == "dependency").unwrap();
        let secret = rows.iter().find(|f| f.kind == "secret").unwrap();
        assert_eq!(dep.status, "open", "a resurfacing advisory reopens itself");
        assert_eq!(secret.status, "fixed", "human secret decisions are never auto-flipped");
    }

    #[test]
    fn a_dependency_that_disappears_from_the_lock_file_is_marked_fixed() {
        let db = conn();
        create_scan(&db, "vs-1", "p1", "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|axios|G1", "high"), "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|lodash|G2", "medium"), "t1").unwrap();
        let secret_key = "sec|a.env|jwt|h1";
        let mut secret = dep(secret_key, "high");
        secret.kind = "secret";
        upsert_finding(&db, "p1", "vs-1", &secret, "t1").unwrap();

        // Second scan only still sees axios; an empty `seen` never wipes.
        assert_eq!(mark_missing_dependencies_fixed(&db, "p1", &[], "t2").unwrap(), 0);
        let fixed = mark_missing_dependencies_fixed(&db, "p1", &["npm|axios|G1".to_string()], "t2").unwrap();
        assert_eq!(fixed, 1);
        let rows = list_findings(&db, "p1", &FindingFilter::default()).unwrap();
        assert_eq!(rows.iter().find(|f| f.dedup_key == "npm|lodash|G2").unwrap().status, "fixed");
        assert_eq!(
            rows.iter().find(|f| f.kind == "secret").unwrap().status,
            "open",
            "absent secrets must not be auto-fixed"
        );
    }

    #[test]
    fn status_words_are_a_closed_set_and_pagination_orders_worst_first() {
        let db = conn();
        create_scan(&db, "vs-1", "p1", "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|a|1", "low"), "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|b|2", "critical"), "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|c|3", "high"), "t1").unwrap();
        assert!(set_finding_status(&db, 1, "wontfix").is_err(), "not in the vocabulary");
        assert!(set_finding_status(&db, 9999, "ignored").is_err(), "unknown id fails loudly");

        let page = list_findings(
            &db,
            "p1",
            &FindingFilter { severity: Some("open".to_string()), limit: 2, ..Default::default() },
        )
        .unwrap();
        assert!(page.is_empty(), "severity filter is exact: open is a status, not a severity");

        let worst_first = list_findings(&db, "p1", &FindingFilter::default()).unwrap();
        assert_eq!(
            worst_first.iter().map(|f| f.severity.as_str()).collect::<Vec<_>>(),
            vec!["critical", "high", "low"]
        );
        let only_open = list_findings(
            &db,
            "p1",
            &FindingFilter { status: Some("open".to_string()), ..Default::default() },
        )
        .unwrap();
        assert_eq!(only_open.len(), 3);
        assert_eq!(count_findings(&db, "p1", Some("open")).unwrap(), 3);
        assert_eq!(count_findings(&db, "p1", None).unwrap(), 3);
    }

    #[test]
    fn the_change_report_link_counts_open_findings_per_package() {
        let db = conn();
        create_scan(&db, "vs-1", "p1", "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|axios|G1", "critical"), "t1").unwrap();
        upsert_finding(&db, "p1", "vs-1", &dep("npm|axios|G2", "high"), "t1").unwrap();
        let mut stale = dep("npm|moment|G3", "high");
        stale.package = "moment";
        upsert_finding(&db, "p1", "vs-1", &stale, "t1").unwrap();
        let id = db.query_row::<i64, _, _>("SELECT id FROM vuln_findings WHERE package = 'moment'", [], |r| r.get(0)).unwrap();
        set_finding_status(&db, id, "ignored").unwrap();

        let linked = open_for_packages(&db, "p1", &["axios".to_string(), "moment".to_string(), "vue".to_string()]).unwrap();
        assert_eq!(linked, vec![("axios".to_string(), 2, 1)], "ignored and clean packages stay out");
    }

    #[test]
    fn the_status_vocabularies_stay_closed() {
        // The UI translates these tokens; adding one without an i18n entry leaks.
        assert!(SCAN_STATUSES.contains(&"cancelled") && SCAN_STATUSES.contains(&"running"));
        assert_eq!(FINDING_STATUSES, ["open", "fixed", "ignored", "false_positive"]);
        assert!(is_valid_status("false_positive") && !is_valid_status("wontfix"));
    }

    #[test]
    fn scan_retention_drops_the_oldest_rows_only() {
        let db = conn();
        for (index, id) in ["a", "b", "c", "d"].iter().enumerate() {
            create_scan(&db, id, "p1", &format!("2026-09-2{}T00:00:00Z", index + 1)).unwrap();
        }
        assert_eq!(prune_scans(&db, "p1", 2).unwrap(), 2);
        let kept = list_scans(&db, "p1", 10).unwrap();
        assert_eq!(kept.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["d", "c"]);
    }
}
