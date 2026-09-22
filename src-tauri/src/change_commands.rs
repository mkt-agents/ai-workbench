//! Git plumbing for the change-driven regression report. Everything that needs a
//! judgement lives in `change_report` (pure); this file only gathers git's own output.

use serde::Serialize;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

use crate::change_report::{self, ChangeReport};
use crate::change_store;
use crate::coverage_delta;
use crate::git_commands::{ensure_git_repo, git_stdout};
use crate::test_commands::{load_test_projects, TestProject};
use crate::test_scenarios;
use crate::test_selection;
use crate::DbState;

/// Enough patch text to read signatures off; the UI never shows the raw diff.
const MAX_PATCH_BYTES: usize = 200 * 1024;
/// What gets stored for the model: a prompt this long is already past useful.
const MAX_AI_PATCH_BYTES: usize = 24 * 1024;
/// How far back the "recent commits" mode may look.
const MAX_RECENT_COMMITS: i64 = 50;

/// One of the three things a tester can mean by "the change".
#[derive(Debug, Clone)]
pub enum ChangeScope {
    /// Staged + unstaged + never-committed files, against `HEAD`.
    Uncommitted,
    /// The working tree against a branch / tag / sha.
    Ref(String),
    /// The last N commits.
    Commits(i64),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeReportBundle {
    pub report: ChangeReport,
    pub branch: String,
    pub head: String,
    pub repo_root: String,
    /// Project-relative vs repo root: a maven module inside a bigger repo.
    pub subdir: bool,
    /// Set once the row is stored; the AI step needs it. `None` = history unavailable.
    pub report_id: Option<String>,
    /// Kept for the model prompt only; the panel renders `report`.
    #[serde(skip_serializing)]
    pub ai_patch: String,
}

/// Reject anything git would read as an option rather than a revision. The value is
/// always passed as one argv element, so this is about git's own flags, not a shell.
fn validate_ref(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("基准不能为空".to_string());
    }
    if trimmed.len() > 200 || trimmed.starts_with('-') || trimmed.contains("..|") || trimmed.contains(char::is_whitespace) {
        return Err(format!("无法识别的基准：{}", trimmed));
    }
    Ok(trimmed.to_string())
}

/// Pathspecs are fixed constants, never user input.
const TEST_PATHS: &[&str] = &[
    "*Test.java",
    "*Tests.java",
    "*IT.java",
    "*Spec.java",
    "*_test.go",
    "*_test.rs",
    "*.test.ts",
    "*.test.tsx",
    "*.test.js",
    "*.test.jsx",
    "*.spec.ts",
    "*.spec.tsx",
    "*/__tests__/*",
];

fn git_lines(repo: &Path, args: &[&str]) -> Result<String, String> {
    git_stdout(&repo.to_string_lossy(), args)
}

/// `-- .` keeps a module project's report to its own subtree; at the repo root it is a
/// no-op.
fn scoped<'a>(base: Vec<&'a str>, extra: &'a [&'static str]) -> Vec<&'a str> {
    let mut args = base;
    args.extend_from_slice(extra);
    args.push("--");
    args.push(".");
    args
}

fn split_z(raw: &str) -> Vec<String> {
    raw.split('\0').map(|s| s.trim_matches('\n').trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Repo-root-relative `.rs` paths whose own content declares tests. Diff paths come
/// back root-relative even from a subdirectory, hence `repo_root`.
fn inline_test_sources(repo_root: &str, name_status_z: &str, untracked_z: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut check = |path: &str| {
        let key = path.replace('\\', "/").to_lowercase();
        if !key.ends_with(".rs") || out.contains(&key) {
            return;
        }
        let full = Path::new(repo_root).join(path);
        if fs::metadata(&full).map(|m| m.len() > 2 * 1024 * 1024).unwrap_or(true) {
            return;
        }
        let content = match fs::read_to_string(&full) {
            Ok(content) => content,
            Err(_) => return,
        };
        if content.contains("#[cfg(test)]") || content.contains("#[cfg_attr(test") {
            out.insert(key);
        }
    };
    for (_, _, path) in change_report::parse_name_status_z(name_status_z) {
        check(&path);
    }
    for path in split_z(untracked_z) {
        check(&path);
    }
    out
}

/// Commit records for the analysed range: subjects, full briefs, and the
/// path -> short-sha attribution that lets the UI answer "which commit is this
/// file from". `-z` keeps CJK and spaced paths verbatim; \x02/\x01 are ours.
fn commit_records(repo: &Path, range: Option<&str>) -> (Vec<String>, Vec<change_report::CommitBrief>, std::collections::BTreeMap<String, Vec<String>>) {
    let Some(range) = range else {
        return (Vec::new(), Vec::new(), Default::default());
    };
    let raw = git_lines(
        repo,
        &[
            "log",
            "--no-merges",
            "--format=%x02%H%x00%h%x00%an%x00%aI%x00%s%x00%b%x01",
            "--name-only",
            "-z",
            range,
        ],
    )
    .unwrap_or_default();
    let (details, by_file) = change_report::parse_log_z(&raw);
    let subjects = details.iter().map(|c| c.subject.clone()).collect();
    // Log paths are repo-root relative, like the report's diff paths.
    let file_commits = by_file
        .into_iter()
        .map(|(path, shas)| {
            (
                path.replace('\\', "/")
                    .trim_start_matches("./")
                    .to_lowercase(),
                shas,
            )
        })
        .collect();
    (subjects, details, file_commits)
}

fn truncate_bytes(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The `-U0` patch for the same range a stored report described — the "changed
/// lines" side of the incremental-coverage join. Re-running git is deliberate:
/// coverage artifacts describe today's working tree, so the diff must too.
fn patch_sync(dir: &Path, scope: &ChangeScope) -> Result<String, String> {
    let (one, two): (String, Option<String>) = match scope {
        ChangeScope::Uncommitted => ("HEAD".to_string(), None),
        ChangeScope::Ref(value) => (validate_ref(value)?, None),
        ChangeScope::Commits(count) => (
            format!("HEAD~{}", (*count).clamp(1, MAX_RECENT_COMMITS)),
            Some("HEAD".to_string()),
        ),
    };
    let mut args: Vec<&str> = vec!["diff", "--no-color", "-U0", one.as_str()];
    if let Some(two) = &two {
        args.push(two.as_str());
    }
    let patch = git_lines(dir, &scoped(args, &[]))?;
    Ok(truncate_bytes(&patch, MAX_PATCH_BYTES))
}

/// Rebuild the git range a stored report was generated from.
fn scope_from_stored(source: &str, base: &str) -> Result<ChangeScope, String> {
    match source {
        "uncommitted" => Ok(ChangeScope::Uncommitted),
        "base" => Ok(ChangeScope::Ref(base.to_string())),
        "commits" => base
            .strip_prefix("HEAD~")
            .and_then(|n| n.parse::<i64>().ok())
            .map(ChangeScope::Commits)
            .ok_or_else(|| format!("无法从基准重建提交范围：{}", base)),
        other => Err(format!("无法识别的报告来源：{}", other)),
    }
}

pub(crate) fn collect_sync(project: &TestProject, scope: ChangeScope) -> Result<ChangeReportBundle, String> {
    let dir = Path::new(&project.path);
    ensure_git_repo(&project.path)?;

    let repo_root = git_lines(dir, &["rev-parse", "--show-toplevel"])?.trim().to_string();
    let branch = git_lines(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default().trim().to_string();
    let head = git_lines(dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default().trim().to_string();
    let subdir = !repo_root.is_empty() && {
        let normalized_root = repo_root.replace('\\', "/").trim_end_matches('/').to_lowercase();
        let normalized_dir = project.path.replace('\\', "/").trim_end_matches('/').to_lowercase();
        normalized_dir != normalized_root
    };

    // Which git range describes this report.
    let (range, base_label, source, commit_range): (Vec<String>, String, String, Option<String>) = match &scope {
        ChangeScope::Uncommitted => (vec![], String::new(), "uncommitted".to_string(), None),
        ChangeScope::Ref(value) => {
            let value = validate_ref(value)?;
            let resolved = git_lines(dir, &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", value)])
                .unwrap_or_default();
            if resolved.trim().is_empty() {
                return Err(format!("找不到基准：{}（分支/tag/commit 是否存在？）", value));
            }
            (
                vec![value.clone()],
                value.clone(),
                "base".to_string(),
                Some(format!("{}..HEAD", value)),
            )
        }
        ChangeScope::Commits(count) => {
            let count = (*count).clamp(1, MAX_RECENT_COMMITS);
            let from = format!("HEAD~{}", count);
            (vec![from.clone(), "HEAD".to_string()], from, "commits".to_string(), Some(format!("HEAD~{}..HEAD", count)))
        }
    };

    // `git diff <anchor>` compares the working tree with that point, so staged and
    // unstaged edits land in one authoritative list instead of two overlapping ones.
    let anchor: Vec<&str> = match &scope {
        ChangeScope::Uncommitted => vec!["HEAD"],
        ChangeScope::Ref(_) | ChangeScope::Commits(_) => range.iter().map(String::as_str).collect(),
    };

    let mut name_args = vec!["diff", "--name-status", "-z"];
    name_args.extend(anchor.iter().copied());
    let mut num_args = vec!["diff", "--numstat", "-z"];
    num_args.extend(anchor.iter().copied());
    let mut patch_args = vec!["diff", "--no-color", "-U0"];
    patch_args.extend(anchor.iter().copied());

    let name_status = git_lines(dir, &scoped(name_args, &[]))?;
    let numstat = git_lines(dir, &scoped(num_args, &[]))?;
    let untracked = if matches!(scope, ChangeScope::Uncommitted) {
        git_lines(dir, &scoped(vec!["ls-files", "--others", "--exclude-standard", "-z"], &[]))?
    } else {
        String::new()
    };

    let patch = match git_lines(dir, &scoped(patch_args, &[])) {
        // A huge or broken patch must not cost the tester the whole report.
        Ok(patch) => truncate_bytes(&patch, MAX_PATCH_BYTES),
        Err(_) => String::new(),
    };

    // Rust keeps its tests inline (`#[cfg(test)] mod tests`), which no path convention
    // can see, so the changed sources themselves are read for it.
    let inline_tests = inline_test_sources(&repo_root, &name_status, &untracked);

    let known_tests: std::collections::HashSet<String> = git_lines(dir, &scoped(vec!["ls-files", "-z"], TEST_PATHS))
        .map(|raw| split_z(&raw))
        .unwrap_or_default()
        .into_iter()
        .collect();

    let (subjects, commit_details, file_commits) = commit_records(dir, commit_range.as_deref());

    let report = change_report::build_report(&change_report::ReportInputs {
        base: &base_label,
        source: &source,
        subjects,
        commit_details,
        file_commits,
        name_status_z: &name_status,
        numstat_z: &numstat,
        untracked: &untracked,
        known_test_paths: known_tests,
        inline_test_files: inline_tests,
        patch: &patch,
        truncated: patch.len() >= MAX_PATCH_BYTES,
    });

    Ok(ChangeReportBundle {
        ai_patch: {
            if patch.len() > MAX_AI_PATCH_BYTES {
                let mut end = MAX_AI_PATCH_BYTES;
                while end > 0 && !patch.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}\n…（补丁已截断）", &patch[..end])
            } else {
                patch
            }
        },
        report,
        branch,
        head,
        repo_root,
        subdir,
        report_id: None,
    })
}

/// The change set behind a report, for the UI to render and (optionally) hand to a model.
#[tauri::command]
pub async fn collect_change_report(
    state: tauri::State<'_, DbState>,
    project_id: String,
    base: Option<String>,
    last_commits: Option<i64>,
) -> Result<ChangeReportBundle, String> {
    let projects = load_test_projects(state.clone())?;
    let project = projects
        .into_iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| format!("Project not found: {}", project_id))?;
    let scope = match (base.filter(|b| !b.trim().is_empty()), last_commits) {
        (Some(base), _) => ChangeScope::Ref(base),
        (None, Some(count)) => ChangeScope::Commits(count),
        (None, None) => ChangeScope::Uncommitted,
    };
    let conn = Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let mut bundle = collect_sync(&project, scope)?;
        // Every generation is a report in the project's history; a failed write (the
        // project was deleted mid-flight) must not cost the tester what they see.
        bundle.report_id = save_sync(&conn, &project_id, &bundle).ok();
        Ok(bundle)
    })
    .await
    .map_err(|e| format!("分析任务已中止: {}", e))?
}

fn save_sync(
    conn: &Arc<Mutex<rusqlite::Connection>>,
    project_id: &str,
    bundle: &ChangeReportBundle,
) -> Result<String, String> {
    let data = serde_json::to_string(&bundle.report).map_err(|e| e.to_string())?;
    let id = format!("cr-{}", chrono::Utc::now().timestamp_millis());
    let created_at = chrono::Utc::now().to_rfc3339();
    let guard = conn.lock().map_err(|e| e.to_string())?;
    change_store::save(
        &guard,
        &change_store::NewReport {
            id: &id,
            project_id,
            base: &bundle.report.base,
            source: &bundle.report.source,
            created_at: &created_at,
            branch: &bundle.branch,
            head: &bundle.head,
            files: bundle.report.stats.files as i64,
            adds: bundle.report.stats.adds as i64,
            dels: bundle.report.stats.dels as i64,
            untested: bundle.report.stats.untested as i64,
            data: &data,
            patch: &bundle.ai_patch,
        },
    )?;
    Ok(id)
}

pub use change_store::{ChangeReportSummary, StoredChangeReport};

#[tauri::command]
pub fn list_change_reports(
    state: State<'_, DbState>,
    project_id: String,
) -> Result<Vec<ChangeReportSummary>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    change_store::list(&conn, &project_id, 50)
}

#[tauri::command]
pub fn get_change_report(state: State<'_, DbState>, report_id: String) -> Result<StoredChangeReport, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    change_store::get(&conn, &report_id)
}

#[tauri::command]
pub fn delete_change_report(state: State<'_, DbState>, report_id: String) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    change_store::delete(&conn, &report_id)
}

/// The honest "did our tests actually touch the changed lines": diff line
/// ranges × the coverage artifact's per-line hits. The result is cached on the
/// report so reopening is instant; recompute after a fresh coverage run.
#[tauri::command]
pub async fn compute_incremental_coverage(
    state: State<'_, DbState>,
    report_id: String,
) -> Result<coverage_delta::DeltaCoverage, String> {
    let projects = load_test_projects(state.clone())?;
    let (project_id, source, base) = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        change_store::scope_of(&conn, &report_id)?
    };
    let project = projects
        .into_iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| format!("Project not found: {}", project_id))?;
    let scope = scope_from_stored(&source, &base)?;
    let conn = Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        ensure_git_repo(&project.path)?;
        let patch = patch_sync(Path::new(&project.path), &scope)?;
        let changed = coverage_delta::parse_hunk_ranges(&patch);
        if changed.is_empty() {
            return Err("该报告范围内没有可定位行号的新增/修改行（补丁为空、纯删除或已被截断）".to_string());
        }
        let hits = crate::test_commands::coverage_line_hits(&project)?;
        let delta = coverage_delta::intersect(&changed, &hits);
        // The cache is a convenience; losing it must not lose the answer itself.
        if let (Ok(guard), Ok(json)) = (conn.lock(), serde_json::to_string(&delta)) {
            let _ = change_store::set_delta_coverage(&guard, &report_id, &json);
        }
        Ok(delta)
    })
    .await
    .map_err(|e| format!("增量覆盖率任务已中止: {}", e))?
}

/// Stamp (or revoke) the report's acceptance; the workflow derives its final
/// step from the returned timestamp.
#[tauri::command]
pub fn set_change_report_accepted(
    state: State<'_, DbState>,
    report_id: String,
    accepted: bool,
) -> Result<Option<String>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let stamp = accepted.then(now);
    change_store::set_accepted(&conn, &report_id, stamp.as_deref())?;
    Ok(stamp)
}

/// Chunk budget for the map pass: a prompt this size is one the model actually
/// reads end to end, which a 300-file single call is not.
const AI_CHUNK_FILES: usize = 20;
const AI_CHUNK_APIS: usize = 3;
const AI_MAP_TOKENS: i64 = 1500;
const AI_REDUCE_TOKENS: i64 = 3000;
const AI_SYSTEM: &str = "你是资深测试工程师，只依据给定的改动分析作答，不编造未出现的模块或接口。";

fn emit_ai_progress(app: &AppHandle, report_id: &str, done: usize, total: usize) {
    let _ = app.emit(
        "change-ai-progress",
        serde_json::json!({ "reportId": report_id, "done": done, "total": total }),
    );
}

/// Ask the default model for the test-facing half of the report: which functions are
/// affected and what must be re-tested. Works from the stored analysis plus a bounded
/// patch, so it always matches what the panel just showed. Large reports go through a
/// map-reduce pass: one brief per module chunk, then one final call assembles the
/// five sections — so no chunk's files silently fall off the end of the prompt.
#[tauri::command]
pub async fn generate_change_report_ai(
    app: AppHandle,
    state: State<'_, DbState>,
    report_id: String,
) -> Result<String, String> {
    // Cancel key = report id (`cr-<millis>`), which can never collide with the
    // project ids used as keys by test runs and vuln scans.
    let _guard = crate::cancellation::CancelGuard::new(&report_id);

    let (project_name, report, patch) = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        let (data, patch, project_id, _) = change_store::ai_input(&conn, &report_id)?;
        let name = change_store::project_name(&conn, &project_id);
        let report: ChangeReport = serde_json::from_str(&data).map_err(|e| format!("报告数据损坏: {}", e))?;
        (name, report, patch)
    };
    let mut config = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        crate::test_commands::load_default_model_config(&conn)?
    };
    config.temperature = 0.2;

    let chunks = change_report::plan_ai_chunks(&report, AI_CHUNK_FILES, AI_CHUNK_APIS);
    // Every map call plus the final reduce; a single-pass report is just "the call".
    let total = if chunks.len() <= 1 { 1 } else { chunks.len() + 1 };
    emit_ai_progress(&app, &report_id, 0, total);

    let send = |config: crate::ai_commands::AIModelConfig, user: String| {
        crate::ai_commands::generate_text(crate::ai_commands::GenerateTextRequest {
            config,
            system: AI_SYSTEM.to_string(),
            user,
        })
    };
    let markdown = if chunks.len() <= 1 {
        config.max_tokens = AI_REDUCE_TOKENS;
        send(config, build_ai_prompt(&project_name, &report, &patch))
            .await
            .map_err(|e| format!("{} 生成失败：{}", project_name, e))?
    } else {
        let mut summaries = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.iter().enumerate() {
            if crate::cancellation::is_cancelled(&report_id) {
                return Err("[E_CANCELLED] 生成已取消".to_string());
            }
            config.max_tokens = AI_MAP_TOKENS;
            let brief = send(config.clone(), build_ai_chunk_prompt(&project_name, &report, chunk))
                .await
                .map_err(|e| format!("{} 生成失败（模块 {}）：{}", project_name, chunk.module, e))?;
            summaries.push(format!("【{}】\n{}", chunk.module, brief.trim()));
            emit_ai_progress(&app, &report_id, index + 1, total);
        }
        if crate::cancellation::is_cancelled(&report_id) {
            return Err("[E_CANCELLED] 生成已取消".to_string());
        }
        config.max_tokens = AI_REDUCE_TOKENS;
        send(config, build_ai_reduce_prompt(&project_name, &report, &summaries))
            .await
            .map_err(|e| format!("{} 汇总失败：{}", project_name, e))?
    };

    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    change_store::set_ai(&conn, &report_id, &markdown)?;
    // Cross-check the answer against the static report: identifiers the model
    // mentioned that appear nowhere in it are flagged as suspects (never blocked).
    let warnings = change_report::verify_ai_against_report(&markdown, &report);
    if let Ok(json) = serde_json::to_string(&warnings) {
        let _ = change_store::set_ai_warnings(&conn, &report_id, &json);
    }
    // The acceptance sections are only useful if they become tickable rows, so they
    // are parsed right away; a report with no recognisable bullets just stays empty.
    if let Ok(project_id) = conn.query_row(
        "SELECT project_id FROM change_reports WHERE id = ?1",
        rusqlite::params![report_id],
        |row| row.get::<_, String>(0),
    ) {
        let rows = test_scenarios::scenarios_from_markdown(&markdown);
        let _ = test_scenarios::save_scenarios(&conn, &report_id, &project_id, &rows, &now());
    }
    Ok(markdown)
}

/// Cancel a live AI generation for one report. The running command notices
/// before its next model call.
#[tauri::command]
pub fn cancel_change_ai(report_id: String) -> Result<(), String> {
    if !crate::cancellation::is_active(&report_id) {
        return Err("该报告当前没有进行中的 AI 生成".to_string());
    }
    crate::cancellation::cancel_request(&report_id);
    Ok(())
}

/// Which tests the change points at, in the runner's own filter syntax. The UI ticks
/// entries off this list and hands the result to `run_test` as `args`.
/// Passing `only` re-derives the args for that ticked subset, so the filter syntax stays
/// in one place.
#[tauri::command]
pub fn select_change_tests(
    state: State<'_, DbState>,
    report_id: String,
    only: Option<Vec<String>>,
) -> Result<test_selection::TestSelection, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let (data, framework) = report_context(&conn, &report_id)?;
    let report: ChangeReport = serde_json::from_str(&data).map_err(|e| format!("报告数据损坏: {}", e))?;
    let selection = test_selection::select_tests(&report, &framework, test_selection::DEFAULT_MAX_TARGETS);
    Ok(match only {
        Some(names) if !names.is_empty() && names.len() < selection.targets.len() => {
            test_selection::subset(&selection, &names)
        }
        _ => selection,
    })
}

/// Parse the AI acceptance sections into tickable rows. Safe to call twice: existing
/// titles are skipped rather than duplicated.
#[tauri::command]
pub fn generate_change_scenarios(
    state: State<'_, DbState>,
    report_id: String,
) -> Result<usize, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let (ai, project_id): (Option<String>, String) = conn
        .query_row(
            "SELECT ai, project_id FROM change_reports WHERE id = ?1",
            rusqlite::params![report_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| format!("报告不存在: {}", report_id))?;
    let markdown = ai.filter(|text| !text.trim().is_empty()).ok_or("请先生成 AI 章节，再产出验收清单")?;
    let rows = test_scenarios::scenarios_from_markdown(&markdown);
    if rows.is_empty() {
        return Err("AI 章节里没有可采集的场景条目".to_string());
    }
    test_scenarios::save_scenarios(&conn, &report_id, &project_id, &rows, &now())
}

#[tauri::command]
pub fn add_change_scenario(
    state: State<'_, DbState>,
    report_id: String,
    title: String,
) -> Result<i64, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let project_id: String = conn
        .query_row("SELECT project_id FROM change_reports WHERE id = ?1", rusqlite::params![report_id], |row| row.get(0))
        .map_err(|_| format!("报告不存在: {}", report_id))?;
    test_scenarios::add_manual_scenario(&conn, &report_id, &project_id, &title, &now())
}

/// Record one acceptance item; returns the new progress so the bar updates without a
/// second round trip.
#[tauri::command]
pub fn set_scenario_status(
    state: State<'_, DbState>,
    scenario_id: i64,
    status: String,
    note: Option<String>,
    run_id: Option<String>,
) -> Result<test_scenarios::ScenarioSummary, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let report_id: String = conn
        .query_row("SELECT report_id FROM test_scenarios WHERE id = ?1", rusqlite::params![scenario_id], |row| row.get(0))
        .map_err(|_| format!("场景不存在: {}", scenario_id))?;
    test_scenarios::set_status(&conn, scenario_id, &status, note.as_deref(), run_id.as_deref(), &now())?;
    test_scenarios::summary(&conn, &report_id)
}

#[tauri::command]
pub fn delete_change_scenario(state: State<'_, DbState>, scenario_id: i64) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    test_scenarios::delete_scenario(&conn, scenario_id)
}

/// Note that a run was made for this report, so "did we test the change" has an answer.
#[tauri::command]
pub fn link_change_run(
    state: State<'_, DbState>,
    report_id: String,
    run_id: String,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let project_id: String = conn
        .query_row("SELECT project_id FROM change_reports WHERE id = ?1", rusqlite::params![report_id], |row| row.get(0))
        .map_err(|_| format!("报告不存在: {}", report_id))?;
    test_scenarios::link_run(&conn, &report_id, &run_id, &project_id, &now())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// `(stored report json, project framework)` for one report id.
fn report_context(conn: &rusqlite::Connection, report_id: &str) -> Result<(String, String), String> {
    let (data, project_id): (String, String) = conn
        .query_row(
            "SELECT data, project_id FROM change_reports WHERE id = ?1",
            rusqlite::params![report_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| format!("报告不存在: {}", report_id))?;
    let framework: String = conn
        .query_row("SELECT framework FROM test_projects WHERE id = ?1", rusqlite::params![project_id], |row| row.get(0))
        .unwrap_or_else(|_| "custom".to_string());
    Ok((data, framework))
}

fn build_ai_prompt(project_name: &str, report: &ChangeReport, patch: &str) -> String {
    let mut files = String::new();
    for file in report.files.iter().take(80) {
        files.push_str(&format!(
            "- {} [{}] +{} -{} 层={} 模块={} 风险={} 已有测试={}{}\n",
            file.path,
            file.status,
            file.adds,
            file.dels,
            file.layer,
            file.module,
            if file.risks.is_empty() { "-".to_string() } else { file.risks.join(",") },
            if file.has_test { "是" } else { "否" },
            if file.commits.is_empty() {
                String::new()
            } else {
                format!(" 提交={}", file.commits.join(","))
            },
        ));
    }
    let mut api = String::new();
    for change in report.api_changes.iter().take(40) {
        api.push_str(&format!("- [{}] {} @ {}\n", change.kind, change.name, change.path));
    }
    let base = if report.base.is_empty() { "未提交改动（相对 HEAD）".to_string() } else { report.base.clone() };
    let commits = commits_block(report);

    format!(
        "请根据下面的代码改动分析，产出面向测试同学的回归测试报告。\n\n\
         项目：{}\n基准：{}\n统计：{} 个文件，+{} -{}，涉及 {} 个模块，其中 {} 个改动没有配对测试\n\
         建议回归面（程序判定）：{}\n\
         提交明细（短 sha、说明、改动文件）：\n{}\n\
         改动文件：\n{}\n\
         公开接口变化：\n{}\n\
         补丁片段（可能已截断）：\n```diff\n{}\n```\n\n\
         严格按以下五个二级标题输出，不要添加其它章节或解释文字：\n\
         ## 受影响功能点\n## 必测场景\n## 建议回归范围\n## 兼容性与数据风险\n## 验收清单\n\n\
         要求：每条一行、以 - 开头；「必测场景」每条以 P0/P1/P2 开头标明优先级，再写「前置：…　预期：…」；\
         「建议回归范围」同样按 P0/P1/P2 标注；只依据上面给出的文件与接口，不确定的写「需与开发确认」。",
        project_name,
        base,
        report.stats.files,
        report.stats.adds,
        report.stats.dels,
        report.stats.modules,
        report.stats.untested,
        if report.scope.is_empty() { "-".to_string() } else { report.scope.join(",") },
        if commits.is_empty() { "- 无（未提交改动）\n".to_string() } else { commits },
        files,
        if api.is_empty() { "- 无\n".to_string() } else { api },
        if patch.is_empty() { "（无补丁文本）".to_string() } else { patch.to_string() },
    )
}

/// Per-commit attribution: which commit touched which files, so the model can
/// tie a scenario to an intent instead of guessing from one flat file list.
fn commits_block(report: &ChangeReport) -> String {
    let mut commits = String::new();
    for detail in report.commit_details.iter().take(20) {
        let touched: Vec<&str> = report
            .files
            .iter()
            .filter(|f| f.commits.iter().any(|s| *s == detail.short_sha))
            .map(|f| f.path.as_str())
            .take(12)
            .collect();
        commits.push_str(&format!(
            "- {} {}（{}，{}）：{}\n",
            detail.short_sha,
            detail.subject,
            detail.author,
            detail.date,
            if touched.is_empty() { "-".to_string() } else { touched.join(", ") }
        ));
        if !detail.body.trim().is_empty() {
            commits.push_str(&format!("  正文：{}\n", detail.body.trim().replace('\n', " ")));
        }
    }
    commits
}

fn file_line(file: &change_report::FileChange) -> String {
    format!(
        "- {} [{}] +{} -{} 层={} 风险={} 已有测试={}{}\n",
        file.path,
        file.status,
        file.adds,
        file.dels,
        file.layer,
        if file.risks.is_empty() { "-".to_string() } else { file.risks.join(",") },
        if file.has_test { "是" } else { "否" },
        if file.commits.is_empty() { String::new() } else { format!(" 提交={}", file.commits.join(",")) },
    )
}

/// Map pass: one focused brief per module chunk. No patch on purpose — the
/// chunk is small enough to judge from its own file and interface lists, and
/// pasting the same global patch into every call would only bury them.
fn build_ai_chunk_prompt(project_name: &str, report: &ChangeReport, chunk: &change_report::AiChunk) -> String {
    let files: String = chunk.files.iter().map(file_line).collect();
    let mut api = String::new();
    for change in &chunk.api {
        api.push_str(&format!("- [{}] {} @ {}\n", change.kind, change.name, change.path));
    }
    format!(
        "项目「{}」本次共改动 {} 个文件（+{} -{}），下面是模块「{}」的改动明细；全量建议回归面（程序判定）：{}。\n\n\
         改动文件：\n{}\n\
         公开接口变化：\n{}\n\
         请用不超过 8 行要点总结该模块：改了什么、可能影响哪些功能、有哪些兼容性或数据风险值得测试关注。\
         只依据上面内容，不确定的写「需与开发确认」；不要输出章节标题或解释文字。",
        project_name,
        report.stats.files,
        report.stats.adds,
        report.stats.dels,
        chunk.module,
        if report.scope.is_empty() { "-".to_string() } else { report.scope.join(",") },
        files,
        if api.is_empty() { "- 无\n".to_string() } else { api },
    )
}

/// Reduce pass: the five tester-facing sections are assembled from the module
/// briefs, so no chunk's detail is dropped no matter how big the report is.
fn build_ai_reduce_prompt(project_name: &str, report: &ChangeReport, summaries: &[String]) -> String {
    let base = if report.base.is_empty() { "未提交改动（相对 HEAD）".to_string() } else { report.base.clone() };
    let commits = commits_block(report);
    format!(
        "请把下面按模块整理的改动小结，汇总成一份面向测试同学的回归测试报告。\n\n\
         项目：{}\n基准：{}\n统计：{} 个文件，+{} -{}，涉及 {} 个模块，其中 {} 个改动没有配对测试\n\
         建议回归面（程序判定）：{}\n\
         提交明细（短 sha、说明、改动文件）：\n{}\n\
         各模块改动小结：\n{}\n\n\
         严格按以下五个二级标题输出，不要添加其它章节或解释文字：\n\
         ## 受影响功能点\n## 必测场景\n## 建议回归范围\n## 兼容性与数据风险\n## 验收清单\n\n\
         要求：每条一行、以 - 开头；「必测场景」每条以 P0/P1/P2 开头标明优先级，再写「前置：…　预期：…」；\
         「建议回归范围」同样按 P0/P1/P2 标注；只依据上面的小结与提交明细，不确定的写「需与开发确认」。",
        project_name,
        base,
        report.stats.files,
        report.stats.adds,
        report.stats.dels,
        report.stats.modules,
        report.stats.untested,
        if report.scope.is_empty() { "-".to_string() } else { report.scope.join(",") },
        if commits.is_empty() { "- 无（未提交改动）\n".to_string() } else { commits },
        summaries.join("\n\n"),
    )
}
