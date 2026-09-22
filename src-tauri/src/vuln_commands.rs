//! Tauri surface for the vulnerability page: run scans, read findings, decide status.
//!
//! Every long path is `async + spawn_blocking` (project rule): the OSV waits are
//! async, the filesystem walk blocks, and the DB lock is only ever held for the
//! short upsert burst at the end — a 45-second network timeout must not sit on
//! the mutex.
//!
//! The queue itself lives in the frontend (one scan at a time, so OSV rate
//! limits and the per-project `CancelGuard` key both stay sane); this layer just
//! answers "scan this project now".

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use crate::git_commands::git_stdout;
use crate::test_commands::load_test_projects;
use crate::vuln_scan;
use crate::vuln_store::{self, NewFinding};
use crate::DbState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingsPage {
    pub findings: Vec<vuln_store::Finding>,
    pub totals: vuln_store::FindingTotals,
    pub total_count: i64,
}

/// `(package, count, criticalCount)` rows for the change-report badge.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageExposure {
    pub package: String,
    pub count: i64,
    pub critical: i64,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// The file list the secret scan reads: tracked files, optionally plus
/// git-clean untracked ones. NUL-separated like every other git contract here.
fn collect_tracked_files(dir: &Path, include_untracked: bool) -> Result<Vec<String>, String> {
    let split_z = |raw: String| {
        raw.split('\0').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<String>>()
    };
    let mut files = split_z(git_stdout(&dir.to_string_lossy(), &["ls-files", "-z"]).map_err(|e| format!("不是 git 仓库或 ls-files 失败：{}", e))?);
    if include_untracked {
        files.extend(split_z(git_stdout(&dir.to_string_lossy(), &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default()));
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn emit_progress(app: &AppHandle, project_id: &str, phase: &str, done: usize, total: usize) {
    let _ = app.emit(
        "vuln-scan-progress",
        serde_json::json!({ "projectId": project_id, "phase": phase, "done": done, "total": total }),
    );
}

#[tauri::command]
pub async fn scan_project_vulns(
    app: AppHandle,
    state: State<'_, DbState>,
    project_id: String,
    include_untracked: Option<bool>,
) -> Result<vuln_scan::VulnScanOutcome, String> {
    let projects = load_test_projects(state.clone())?;
    let project = projects
        .into_iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| format!("Project not found: {}", project_id))?;
    let include_untracked = include_untracked.unwrap_or(false);

    // Cancel key = project id, same convention as a test run: one live task per
    // project, and the existing `cancel_request` reaches it.
    let _guard = crate::cancellation::CancelGuard::new(&project_id);
    let cancelled = {
        let id = project_id.clone();
        Arc::new(move || crate::cancellation::is_cancelled(&id))
    };
    let started_at = now();
    emit_progress(&app, &project_id, "deps", 0, 0);

    let mut outcome = vuln_scan::VulnScanOutcome::default();
    let mut eco: BTreeMap<(String, String), String> = BTreeMap::new();

    // 1. Dependencies: lock files in, coordinates out, OSV answers back.
    match vuln_scan::collect_dependencies(Path::new(&project.path)) {
        Ok((deps, unsupported)) => {
            outcome.unsupported = unsupported;
            for dep in &deps {
                eco.insert((dep.name.clone(), dep.version.clone()), dep.ecosystem.clone());
            }
            outcome.deps_checked = deps.len();
            if !deps.is_empty() {
                let done = vuln_scan::query_osv(
                    &deps,
                    {
                        let cancelled = cancelled.clone();
                        move || cancelled()
                    },
                    {
                        let app = app.clone();
                        let id = project_id.clone();
                        move |done, total| emit_progress(&app, &id, "deps", done, total)
                    },
                )
                .await;
                match done {
                    Ok((vulns, truncated)) => {
                        outcome.vulns = vulns;
                        if truncated {
                            // Disclose the cap rather than presenting a partial list as complete.
                            outcome.unsupported = Some(match outcome.unsupported.take() {
                                Some(prev) => format!("{}；公告数量超过上限，仅统计前 300 条", prev),
                                None => "公告数量超过上限，仅统计前 300 条（按编号去重后计入）".to_string(),
                            });
                        }
                    }
                    Err(e) => {
                        outcome.status = "error".to_string();
                        outcome.error_kind = if e.contains("[E_CANCELLED]") { "cancelled".to_string() } else { "unknown".to_string() };
                        outcome.error = Some(e);
                    }
                }
            }
        }
        Err(e) => {
            outcome.status = "error".to_string();
            outcome.error_kind = "command".to_string();
            outcome.error = Some(e);
        }
    }

    // 2. Secrets over the git file list, on the blocking pool.
    if !cancelled() {
        match collect_tracked_files(Path::new(&project.path), include_untracked) {
            Ok(files) => {
                emit_progress(&app, &project_id, "secrets", 0, files.len());
                let dir = project.path.clone();
                let id = project_id.clone();
                let app2 = app.clone();
                let scanned = tokio::task::spawn_blocking(move || {
                    let dir = Path::new(&dir);
                    vuln_scan::scan_secrets(dir, &files, {
                        let app = app2;
                        let id2 = id.clone();
                        move |done, total| emit_progress(&app, &id2, "secrets", done, total)
                    })
                })
                .await
                .map_err(|e| format!("扫描任务已中止: {}", e))?;
                let (hits, checked, skipped) = scanned;
                outcome.secrets = hits;
                outcome.files_checked = checked;
                outcome.files_skipped = skipped;
            }
            Err(e) => {
                // A non-git project still got its dependency answer; record the
                // reason without erasing what worked.
                if outcome.status == "success" {
                    outcome.status = "error".to_string();
                    outcome.error_kind = "command".to_string();
                }
                outcome.error = Some(match outcome.error.take() {
                    Some(prev) => format!("{}；{}", prev, e),
                    None => e,
                });
            }
        }
    }

    if cancelled() {
        outcome.status = "cancelled".to_string();
        outcome.error_kind = "cancelled".to_string();
        outcome.error = Some("[E_CANCELLED] 扫描已取消".to_string());
    }

    // 3. Persist, then hand the outcome back for the toast. Secrets are stored as
    // `high`, so they join the high band, not the critical one.
    outcome.critical = outcome.vulns.iter().filter(|v| v.severity == "critical").count();
    outcome.high = outcome.vulns.iter().filter(|v| v.severity == "high").count() + outcome.secrets.len();
    outcome.total = outcome.vulns.len() + outcome.secrets.len();
    let conn = Arc::clone(&state.conn);
    let id = project_id.clone();
    let finished = now();
    let report = outcome.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let guard = conn.lock();
        if let Ok(guard) = guard {
            let _ = persist(&guard, &id, &report, &eco, &started_at, &finished);
        }
    })
    .await;

    emit_progress(&app, &project_id, "done", 1, 1);
    Ok(outcome)
}

/// Write the scan row and its findings; the scan never fails for a storage
/// hiccup — the tester already has the answer on screen.
fn persist(
    conn: &rusqlite::Connection,
    project_id: &str,
    outcome: &vuln_scan::VulnScanOutcome,
    eco: &BTreeMap<(String, String), String>,
    started_at: &str,
    finished_at: &str,
) -> Result<(), String> {
    let scan_id = format!("vs-{}", chrono::Utc::now().timestamp_millis());
    vuln_store::create_scan(conn, &scan_id, project_id, started_at)?;
    let mut seen = Vec::new();
    for vuln in &outcome.vulns {
        let ecosystem = eco
            .get(&(vuln.package.clone(), vuln.version.clone()))
            .cloned()
            .unwrap_or_else(|| "npm".to_string());
        let key = vuln_store::dependency_key(&ecosystem, &vuln.package, &vuln.id);
        seen.push(key.clone());
        let finding = NewFinding {
            kind: "dependency",
            dedup_key: &key,
            ecosystem: &ecosystem,
            package: &vuln.package,
            version: &vuln.version,
            vuln_id: &vuln.id,
            severity: &vuln.severity,
            summary: &vuln.summary,
            fixed_versions: vuln.fixed_versions.clone(),
            aliases: vuln.aliases.clone(),
            file: "",
            line: 0,
            preview: "",
            rule: "",
        };
        vuln_store::upsert_finding(conn, project_id, &scan_id, &finding, finished_at)?;
    }
    for hit in &outcome.secrets {
        let key = vuln_store::secret_key(&hit.file, &hit.rule, &hit.digest);
        // Absence never auto-fixes a secret, so `seen` (which drives the
        // dependency auto-fix) intentionally excludes these.
        let finding = NewFinding {
            kind: "secret",
            dedup_key: &key,
            ecosystem: "",
            package: "",
            version: "",
            vuln_id: "",
            severity: "high",
            summary: "",
            fixed_versions: Vec::new(),
            aliases: Vec::new(),
            file: &hit.file,
            line: hit.line as i64,
            preview: &hit.preview,
            rule: &hit.rule,
        };
        vuln_store::upsert_finding(conn, project_id, &scan_id, &finding, finished_at)?;
    }
    if outcome.deps_checked > 0 && outcome.error.is_none() {
        vuln_store::mark_missing_dependencies_fixed(conn, project_id, &seen, finished_at)?;
    }
    vuln_store::finish_scan(
        conn,
        &scan_id,
        &outcome.status,
        &outcome.error_kind,
        outcome.deps_checked as i64,
        outcome.files_checked as i64,
        finished_at,
    )?;
    let _ = vuln_store::prune_scans(conn, project_id, vuln_store::SCAN_RETENTION);
    Ok(())
}

#[tauri::command]
pub fn list_vuln_findings(
    state: State<'_, DbState>,
    project_id: String,
    status: Option<String>,
    kind: Option<String>,
    severity: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<FindingsPage, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let filter = vuln_store::FindingFilter {
        status: status.filter(|s| !s.is_empty()),
        kind: kind.filter(|s| !s.is_empty()),
        severity: severity.filter(|s| !s.is_empty()),
        limit: limit.unwrap_or(100).clamp(1, 500),
        offset: offset.unwrap_or(0).max(0),
    };
    Ok(FindingsPage {
        total_count: vuln_store::count_findings(&conn, &project_id, filter.status.as_deref())?,
        findings: vuln_store::list_findings(&conn, &project_id, &filter)?,
        totals: vuln_store::totals(&conn, &project_id)?,
    })
}

#[tauri::command]
pub fn set_vuln_finding_status(
    state: State<'_, DbState>,
    finding_id: i64,
    status: String,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    vuln_store::set_finding_status(&conn, finding_id, &status)
}

#[tauri::command]
pub fn list_vuln_scans(
    state: State<'_, DbState>,
    project_id: String,
) -> Result<Vec<vuln_store::ScanSummary>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    vuln_store::list_scans(&conn, &project_id, 10)
}

/// Read-only link for the change report: lock-file changes light up with the
/// packages' current open counts.
#[tauri::command]
pub fn find_package_exposures(
    state: State<'_, DbState>,
    project_id: String,
    packages: Vec<String>,
) -> Result<Vec<PackageExposure>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let rows = vuln_store::open_for_packages(&conn, &project_id, &packages)?;
    Ok(rows
        .into_iter()
        .map(|(package, count, critical)| PackageExposure { package, count, critical })
        .collect())
}
