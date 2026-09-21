//! Git plumbing for the change-driven regression report. Everything that needs a
//! judgement lives in `change_report` (pure); this file only gathers git's own output.

use serde::Serialize;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tauri::State;

use crate::change_report::{self, ChangeReport};
use crate::change_store;
use crate::git_commands::{ensure_git_repo, git_stdout};
use crate::test_commands::{load_test_projects, TestProject};
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

fn commits_of(repo: &Path, range: Option<&str>) -> Vec<String> {
    let mut args = vec!["log", "--format=%s", "-z", "--no-merges"];
    if let Some(range) = range {
        args.push(range);
    } else {
        return Vec::new();
    }
    git_lines(repo, &args).map(|raw| split_z(&raw)).unwrap_or_default()
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
        Ok(patch) => {
            if patch.len() > MAX_PATCH_BYTES {
                let mut end = MAX_PATCH_BYTES;
                while end > 0 && !patch.is_char_boundary(end) {
                    end -= 1;
                }
                patch[..end].to_string()
            } else {
                patch
            }
        }
        // A huge or broken patch must not cost the tester the whole report.
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

    let report = change_report::build_report(
        &base_label,
        &source,
        commits_of(dir, commit_range.as_deref()),
        &name_status,
        &numstat,
        &untracked,
        &known_tests,
        &inline_tests,
        &patch,
        patch.len() >= MAX_PATCH_BYTES,
    );

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

/// Ask the default model for the test-facing half of the report: which functions are
/// affected and what must be re-tested. Works from the stored analysis plus a bounded
/// patch, so it always matches what the panel just showed.
#[tauri::command]
pub async fn generate_change_report_ai(
    state: State<'_, DbState>,
    report_id: String,
) -> Result<String, String> {
    let (prompt, project_name) = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        let (data, patch, project_id, _) = change_store::ai_input(&conn, &report_id)?;
        let name = change_store::project_name(&conn, &project_id);
        let report: ChangeReport = serde_json::from_str(&data).map_err(|e| format!("报告数据损坏: {}", e))?;
        (build_ai_prompt(&name, &report, &patch), name)
    };

    let mut config = {
        let conn = state.conn.lock().map_err(|e| e.to_string())?;
        crate::test_commands::load_default_model_config(&conn)?
    };
    config.max_tokens = 3000;
    config.temperature = 0.2;

    let markdown = crate::ai_commands::generate_text(crate::ai_commands::GenerateTextRequest {
        config,
        system: "你是资深测试工程师，只依据给定的改动分析作答，不编造未出现的模块或接口。".to_string(),
        user: prompt,
    })
    .await
    .map_err(|e| format!("{} 生成失败：{}", project_name, e))?;

    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    change_store::set_ai(&conn, &report_id, &markdown)?;
    Ok(markdown)
}

fn build_ai_prompt(project_name: &str, report: &ChangeReport, patch: &str) -> String {
    let mut files = String::new();
    for file in report.files.iter().take(80) {
        files.push_str(&format!(
            "- {} [{}] +{} -{} 层={} 模块={} 风险={} 已有测试={}\n",
            file.path,
            file.status,
            file.adds,
            file.dels,
            file.layer,
            file.module,
            if file.risks.is_empty() { "-".to_string() } else { file.risks.join(",") },
            if file.has_test { "是" } else { "否" },
        ));
    }
    let mut api = String::new();
    for change in report.api_changes.iter().take(40) {
        api.push_str(&format!("- [{}] {} @ {}\n", change.kind, change.name, change.path));
    }
    let base = if report.base.is_empty() { "未提交改动（相对 HEAD）".to_string() } else { report.base.clone() };

    format!(
        "请根据下面的代码改动分析，产出面向测试同学的回归测试报告。\n\n\
         项目：{}\n基准：{}\n统计：{} 个文件，+{} -{}，涉及 {} 个模块，其中 {} 个改动没有配对测试\n\
         建议回归面（程序判定）：{}\n\
         提交说明：{}\n\n\
         改动文件：\n{}\n\
         公开接口变化：\n{}\n\
         补丁片段（可能已截断）：\n```diff\n{}\n```\n\n\
         严格按以下五个二级标题输出，不要添加其它章节或解释文字：\n\
         ## 受影响功能点\n## 必测场景\n## 建议回归范围\n## 兼容性与数据风险\n## 验收清单\n\n\
         要求：每条一行、以 - 开头；「必测场景」写明前置数据与预期结果；「建议回归范围」按 P0/P1/P2 标注；\
         只依据上面给出的文件与接口，不确定的写「需与开发确认」。",
        project_name,
        base,
        report.stats.files,
        report.stats.adds,
        report.stats.dels,
        report.stats.modules,
        report.stats.untested,
        if report.scope.is_empty() { "-".to_string() } else { report.scope.join(",") },
        if report.commits.is_empty() { "-".to_string() } else { report.commits.join("；") },
        files,
        if api.is_empty() { "- 无\n".to_string() } else { api },
        if patch.is_empty() { "（无补丁文本）".to_string() } else { patch.to_string() },
    )
}
