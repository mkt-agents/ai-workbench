//! Git plumbing for the one-shot regression report launched from a repo card.
//! Everything that needs a judgement lives in `change_report` (pure); this file
//! only gathers git's own output, hands the analysis to a model, and keeps the
//! collected report in an in-process cache so the AI step reads the exact same
//! data the panel just showed — no DB, no history.

use serde::Serialize;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

use crate::ai_commands::{generate_text, AIModelConfig, GenerateTextRequest};
use crate::cancellation::{self, CancelGuard};
use crate::change_report::{self, ChangeReport};
use crate::git_commands::{ensure_git_repo, git_stdout};

/// Enough patch text to read signatures off; the UI never shows the raw diff.
const MAX_PATCH_BYTES: usize = 200 * 1024;
/// What gets kept for the model: a prompt this long is already past useful.
const MAX_AI_PATCH_BYTES: usize = 24 * 1024;
/// How far back the "recent commits" mode may look.
const MAX_RECENT_COMMITS: i64 = 50;

/// Chunk budget for the map pass: a prompt this size is one the model actually
/// reads end to end, which a 300-file single call is not.
const AI_CHUNK_FILES: usize = 20;
const AI_CHUNK_APIS: usize = 3;
const AI_MAP_TOKENS: i64 = 1500;
const AI_REDUCE_TOKENS: i64 = 3000;
const AI_SYSTEM: &str = "你是资深测试工程师，只依据给定的改动分析作答，不编造未出现的模块或接口。";

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
pub struct TestReportBundle {
    pub report: ChangeReport,
    /// Cache key + cancellation token for the AI step.
    pub report_id: String,
    pub branch: String,
    pub head: String,
    pub repo_root: String,
    /// Last path segment of the analysed directory; used for export naming + prompt.
    pub repo_name: String,
    /// Repo-relative vs repo root: a module inside a bigger repo.
    pub subdir: bool,
    /// Kept in the cache for the model prompt only; never crosses IPC.
    #[serde(skip_serializing)]
    pub ai_patch: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiResult {
    pub markdown: String,
    pub warnings: Vec<String>,
}

/// What the AI step needs from a prior collect: the report + its bounded patch.
struct CacheEntry {
    repo_name: String,
    report: ChangeReport,
    ai_patch: String,
}

/// In-process, bounded, FIFO. A report is regenerated cheaply (re-collect) so
/// losing an old entry to eviction is harmless; 16 is far past any live session.
static REPORT_CACHE: Mutex<Vec<(String, CacheEntry)>> = Mutex::new(Vec::new());
const CACHE_CAP: usize = 16;
static REPORT_SEQ: AtomicU32 = AtomicU32::new(0);

fn next_report_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = REPORT_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("tr-{}-{}", millis, seq)
}

fn cache_put(id: String, entry: CacheEntry) {
    let mut cache = REPORT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(key, _)| key != &id);
    cache.push((id, entry));
    if cache.len() > CACHE_CAP {
        let overflow = cache.len() - CACHE_CAP;
        cache.drain(0..overflow);
    }
}

fn cache_get(id: &str) -> Option<CacheEntry> {
    let cache = REPORT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.iter().find(|(key, _)| key == id).map(|(_, entry)| CacheEntry {
        repo_name: entry.repo_name.clone(),
        report: entry.report.clone(),
        ai_patch: entry.ai_patch.clone(),
    })
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

/// `-- .` keeps a module's report to its own subtree; at the repo root it is a no-op.
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

fn repo_display_name(repo_path: &str) -> String {
    let trimmed = repo_path.trim_end_matches(['/', '\\']);
    Path::new(trimmed)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| trimmed.to_string())
}

/// Turn the two optional baseline params into a scope: base wins over last-N,
/// neither means the uncommitted working tree.
fn scope_of(base: Option<String>, last_commits: Option<i64>) -> ChangeScope {
    match (base.filter(|b| !b.trim().is_empty()), last_commits) {
        (Some(base), _) => ChangeScope::Ref(base),
        (None, Some(count)) => ChangeScope::Commits(count),
        (None, None) => ChangeScope::Uncommitted,
    }
}

pub(crate) fn collect_sync(repo_path: &str, scope: ChangeScope) -> Result<TestReportBundle, String> {
    let dir = Path::new(repo_path);
    ensure_git_repo(repo_path)?;

    let repo_root = git_lines(dir, &["rev-parse", "--show-toplevel"])?.trim().to_string();
    let branch = git_lines(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default().trim().to_string();
    let head = git_lines(dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default().trim().to_string();
    let subdir = !repo_root.is_empty() && {
        let normalized_root = repo_root.replace('\\', "/").trim_end_matches('/').to_lowercase();
        let normalized_dir = repo_path.replace('\\', "/").trim_end_matches('/').to_lowercase();
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

    Ok(TestReportBundle {
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
        report_id: String::new(),
        branch,
        head,
        repo_root,
        repo_name: repo_display_name(repo_path),
        subdir,
    })
}

/// The change set behind a report, for the UI to render and (optionally) hand to a model.
#[tauri::command]
pub async fn collect_test_report(
    repo_path: String,
    base: Option<String>,
    last_commits: Option<i64>,
) -> Result<TestReportBundle, String> {
    let scope = scope_of(base, last_commits);
    let mut bundle = tokio::task::spawn_blocking(move || collect_sync(&repo_path, scope))
        .await
        .map_err(|e| format!("分析任务已中止: {}", e))??;
    // Cache under a fresh id so the AI step reads exactly this analysis; the id is
    // also the cancellation token. A failed cache write must not lose what we return.
    let id = next_report_id();
    bundle.report_id = id.clone();
    cache_put(
        id,
        CacheEntry {
            repo_name: bundle.repo_name.clone(),
            report: bundle.report.clone(),
            ai_patch: bundle.ai_patch.clone(),
        },
    );
    Ok(bundle)
}

/// Per-repo rollup row inside a folder report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRepoRow {
    pub name: String,
    pub files: u32,
    pub adds: u32,
    pub dels: u32,
    pub untested: u32,
    /// `None` = collected fine; `Some(msg)` = this repo was skipped (not a repo /
    /// missing base / git error) and contributed nothing to the merged report.
    pub error: Option<String>,
}

/// The merged result of collecting every repo under one folder.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderReportBundle {
    pub report: ChangeReport,
    /// Cache key + cancellation token for the shared AI step.
    pub report_id: String,
    pub folder_name: String,
    pub repos: Vec<FolderRepoRow>,
}

/// Folder = N related repos, so the model gets one combined patch. Repos with no
/// changes add nothing; the total stays bounded so a big folder can't blow the prompt.
fn build_folder_ai_patch(entries: &[(&str, &TestReportBundle)]) -> String {
    let mut out = String::new();
    for (name, bundle) in entries {
        if bundle.ai_patch.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("### 仓库 {}\n", name));
        out.push_str(&bundle.ai_patch);
        out.push('\n');
    }
    truncate_bytes(&out, MAX_AI_PATCH_BYTES)
}

/// Collect every repo under a folder in one pass and fold the results into a single
/// regression report. The AI step is the ordinary `generate_test_report_ai`, keyed on
/// the merged `report_id`, so map-reduce + cancellation come for free. Repos that fail
/// (or have no changes) are reported per-row instead of aborting the whole folder.
#[tauri::command]
pub async fn collect_folder_report(
    app: AppHandle,
    folder_name: String,
    repo_paths: Vec<String>,
    base: Option<String>,
    last_commits: Option<i64>,
) -> Result<FolderReportBundle, String> {
    if repo_paths.is_empty() {
        return Err("该文件夹下没有可选的 Git 仓库".to_string());
    }
    let scope = scope_of(base, last_commits);
    let total = repo_paths.len();
    let mut collected: Vec<(String, TestReportBundle)> = Vec::new();
    let mut rows: Vec<FolderRepoRow> = Vec::new();

    for (index, path) in repo_paths.iter().enumerate() {
        let _ = app.emit(
            "test-report-collect-progress",
            serde_json::json!({ "folder": folder_name, "done": index, "total": total }),
        );
        let name = repo_display_name(path);
        let scope = scope.clone();
        let path = path.clone();
        let outcome = tokio::task::spawn_blocking(move || collect_sync(&path, scope))
            .await
            .map_err(|e| format!("分析任务已中止: {}", e))?;
        match outcome {
            Ok(bundle) => {
                rows.push(FolderRepoRow {
                    name: name.clone(),
                    files: bundle.report.stats.files,
                    adds: bundle.report.stats.adds,
                    dels: bundle.report.stats.dels,
                    untested: bundle.report.stats.untested,
                    error: None,
                });
                collected.push((name, bundle));
            }
            Err(err) => rows.push(FolderRepoRow {
                name,
                files: 0,
                adds: 0,
                dels: 0,
                untested: 0,
                error: Some(err),
            }),
        }
    }
    let _ = app.emit(
        "test-report-collect-progress",
        serde_json::json!({ "folder": folder_name, "done": total, "total": total }),
    );

    let with_changes: Vec<(&str, &TestReportBundle)> = collected
        .iter()
        .filter(|(_, b)| b.report.stats.files > 0)
        .map(|(n, b)| (n.as_str(), b))
        .collect();
    let report = change_report::merge_folder_report(
        &with_changes
            .iter()
            .map(|(n, b)| (*n, &b.report))
            .collect::<Vec<(&str, &ChangeReport)>>(),
    );
    let ai_patch = build_folder_ai_patch(&with_changes);

    // Only repo names + reports are needed to cache; drop the bundles.
    let id = next_report_id();
    cache_put(
        id.clone(),
        CacheEntry {
            repo_name: folder_name.clone(),
            report: report.clone(),
            ai_patch,
        },
    );
    Ok(FolderReportBundle { report, report_id: id, folder_name, repos: rows })
}

fn emit_ai_progress(app: &AppHandle, report_id: &str, done: usize, total: usize) {
    let _ = app.emit(
        "test-report-ai-progress",
        serde_json::json!({ "reportId": report_id, "done": done, "total": total }),
    );
}/// Ask the chosen model for the test-facing half of the report: which functions are
/// affected and what must be re-tested. Works from the cached analysis plus a bounded
/// patch, so it always matches what the panel just showed. Large reports go through a
/// map-reduce pass: one brief per module chunk, then one final call assembles the
/// five sections — so no chunk's files silently fall off the end of the prompt.
#[tauri::command]
pub async fn generate_test_report_ai(
    app: AppHandle,
    report_id: String,
    config: AIModelConfig,
) -> Result<AiResult, String> {
    // Cancel key = report id (`tr-<millis>-<seq>`), which can never collide with the
    // project ids used as keys by other long-running commands.
    let _guard = CancelGuard::new(&report_id);

    let entry = cache_get(&report_id).ok_or("报告已过期，请重新采集")?;
    let project_name = entry.repo_name;
    let report = entry.report;
    let patch = entry.ai_patch;

    let mut config = config;
    config.temperature = 0.2;

    let chunks = change_report::plan_ai_chunks(&report, AI_CHUNK_FILES, AI_CHUNK_APIS);
    // Every map call plus the final reduce; a single-pass report is just "the call".
    let total = if chunks.len() <= 1 { 1 } else { chunks.len() + 1 };
    emit_ai_progress(&app, &report_id, 0, total);

    let send = |config: AIModelConfig, user: String| {
        generate_text(GenerateTextRequest {
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
            if cancellation::is_cancelled(&report_id) {
                return Err("[E_CANCELLED] 生成已取消".to_string());
            }
            config.max_tokens = AI_MAP_TOKENS;
            let brief = send(config.clone(), build_ai_chunk_prompt(&project_name, &report, chunk))
                .await
                .map_err(|e| format!("{} 生成失败（模块 {}）：{}", project_name, chunk.module, e))?;
            summaries.push(format!("【{}】\n{}", chunk.module, brief.trim()));
            emit_ai_progress(&app, &report_id, index + 1, total);
        }
        if cancellation::is_cancelled(&report_id) {
            return Err("[E_CANCELLED] 生成已取消".to_string());
        }
        config.max_tokens = AI_REDUCE_TOKENS;
        send(config, build_ai_reduce_prompt(&project_name, &report, &summaries))
            .await
            .map_err(|e| format!("{} 汇总失败：{}", project_name, e))?
    };

    // Cross-check the answer against the static report: identifiers the model
    // mentioned that appear nowhere in it are flagged as suspects (never blocked).
    let warnings = change_report::verify_ai_against_report(&markdown, &report);
    Ok(AiResult { markdown, warnings })
}

/// Cancel a live AI generation for one report. The running command notices
/// before its next model call.
#[tauri::command]
pub fn cancel_test_report_ai(report_id: String) -> Result<(), String> {
    if !cancellation::is_active(&report_id) {
        return Err("该报告当前没有进行中的 AI 生成".to_string());
    }
    cancellation::cancel_request(&report_id);
    Ok(())
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
