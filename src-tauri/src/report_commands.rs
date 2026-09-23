//! Git plumbing for the one-shot regression report launched from a repo card.
//! Everything that needs a judgement lives in `change_report` (pure); this file
//! only gathers git's own output, hands the analysis to a model, and keeps the
//! collected report in an in-process cache so the AI step reads the exact same
//! data the panel just showed — no DB, no history.

use serde::Serialize;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

use crate::ai_commands::{generate_text, AIModelConfig, GenerateTextRequest};
use crate::cancellation::{self, CancelGuard};
use crate::change_report::{self, ChangeReport};
use crate::git_commands::git_stdout;

/// Enough patch text to read signatures off; the UI never shows the raw diff.
const MAX_PATCH_BYTES: usize = 200 * 1024;
/// What gets kept for the model: a prompt this long is already past useful.
const MAX_AI_PATCH_BYTES: usize = 24 * 1024;
/// How far back the "recent commits" mode may look.
const MAX_RECENT_COMMITS: i64 = 50;

/// Chunk budget for the map pass. A chunk this size is one the model reads end to
/// end, which a 300-file single call is not — but the budget is deliberately
/// generous, because **every chunk is one more model round trip** and the tester
/// waits out all of them. 60 file lines is ~6KB of prompt.
const AI_CHUNK_FILES: usize = 60;
/// Same reasoning for interface changes: each is one short line, so the old
/// budget of 3 split API-heavy modules into a stream of their own chunks.
const AI_CHUNK_APIS: usize = 10;
const AI_MAP_TOKENS: i64 = 1500;
const AI_REDUCE_TOKENS: i64 = 3000;
/// How many module briefs to ask for at once. Kept low on purpose: hosted
/// endpoints and local DeepSeek instances commonly cap concurrent requests at
/// 1–2, and being *rejected* costs far more wall-clock than serialising would
/// have — the tester waits out the whole round only to see a failure. With the
/// chunk sizing above there are only a couple of chunks in practice, so
/// concurrency buys little anyway.
const AI_MAP_CONCURRENCY: usize = 2;
/// Retries for a call the provider rejected for backpressure (429 / concurrency
/// limit / "too many requests"). Short, because someone is watching a progress
/// bar; a failed report is worse than a slightly slower one.
const AI_RETRIES: usize = 2;
const AI_RETRY_BASE_MS: u64 = 700;
/// Repos under one folder are independent, so collect them concurrently: a folder
/// of N drops from N sequential passes to ~ceil(N/this). Bounded like the AI map
/// pass so a big folder doesn't thrash the machine with git processes.
const FOLDER_COLLECT_CONCURRENCY: usize = 4;
const AI_SYSTEM: &str = "你是资深测试工程师，只依据给定的改动分析作答，不编造未出现的模块或接口。";

/// One of the ways a tester can mean "the change".
#[derive(Debug, Clone)]
pub enum ChangeScope {
    /// Staged + unstaged + never-committed files, against `HEAD`.
    Uncommitted,
    /// The working tree against a branch / tag / sha.
    Ref(String),
    /// The last N commits.
    Commits(i64),
    /// Everything that landed inside a date window, both ends inclusive. Either
    /// bound may be omitted; both are `YYYY-MM-DD` as typed into the date inputs.
    Since {
        since: Option<String>,
        until: Option<String>,
    },
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
/// file from". The raw `log -z` output is gathered by the caller (concurrently
/// with the other git reads); \x02/\x01 are ours.
fn commit_records(raw_log: &str) -> (Vec<String>, Vec<change_report::CommitBrief>, std::collections::BTreeMap<String, Vec<String>>) {
    let (details, by_file) = change_report::parse_log_z(raw_log);
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

/// Turn the optional baseline params into a scope. Precedence: an explicit ref
/// wins over a commit count, which wins over a date window; nothing at all means
/// the uncommitted working tree.
fn scope_of(
    base: Option<String>,
    last_commits: Option<i64>,
    since: Option<String>,
    until: Option<String>,
) -> Result<ChangeScope, String> {
    if let Some(base) = base.filter(|value| !value.trim().is_empty()) {
        return Ok(ChangeScope::Ref(base));
    }
    if let Some(count) = last_commits {
        return Ok(ChangeScope::Commits(count));
    }
    let since = date_bound(since)?;
    let until = date_bound(until)?;
    if since.is_none() && until.is_none() {
        return Ok(ChangeScope::Uncommitted);
    }
    // A backwards window resolves to no commits at all; say so instead of handing
    // back an empty report that looks like "nothing changed".
    if let (Some(from), Some(to)) = (&since, &until) {
        if from > to {
            return Err(format!("开始日期 {} 晚于结束日期 {}", from, to));
        }
    }
    Ok(ChangeScope::Since { since, until })
}

/// Validate one `YYYY-MM-DD` bound from the date inputs. The shape check is not
/// cosmetic: the value ends up in git's argv, and git would read anything
/// starting with `-` as one of its own options.
fn date_bound(value: Option<String>) -> Result<Option<String>, String> {
    match value.map(|raw| raw.trim().to_string()).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) if is_iso_date(&text) => Ok(Some(text)),
        Some(text) => Err(format!("日期格式应为 YYYY-MM-DD：{}", text)),
    }
}

fn is_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

/// What the report shows as its baseline for a date window.
fn date_window_label(since: &Option<String>, until: &Option<String>) -> String {
    match (since, until) {
        (Some(from), Some(to)) => format!("{} ~ {}", from, to),
        (Some(from), None) => format!(">= {}", from),
        (None, Some(to)) => format!("<= {}", to),
        (None, None) => String::new(),
    }
}

/// The diff endpoints a date window describes: the parent of the oldest commit
/// inside the window, up to the newest one.
///
/// `git diff` understands no `--since/--until`, so the window is resolved into a
/// commit range first. Resolving it here (rather than only filtering `log`) keeps
/// the diff, the commit list and the per-file attribution on one range — a report
/// whose file list disagrees with its commit list is worse than no report.
fn resolve_date_window(
    dir: &Path,
    since: &Option<String>,
    until: &Option<String>,
) -> Result<(String, String), String> {
    // Both ends are inclusive. `--until=<date>` alone resolves to midnight, which
    // would silently drop everything committed on the closing day.
    let since_arg = since.as_ref().map(|value| format!("--since={} 00:00:00", value));
    let until_arg = until.as_ref().map(|value| format!("--until={} 23:59:59", value));

    let mut args: Vec<&str> = vec!["log", "--no-merges", "--format=%H"];
    if let Some(value) = since_arg.as_deref() {
        args.push(value);
    }
    if let Some(value) = until_arg.as_deref() {
        args.push(value);
    }
    args.push("HEAD");

    let raw = git_lines(dir, &args)?;
    let commits: Vec<&str> = raw.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if commits.is_empty() {
        return Err("该时间区间内没有提交，请调整日期或改用其它基准".to_string());
    }
    // `git log` walks newest first, so the first entry is the window's tip.
    let tip = commits[0].to_string();
    let earliest = commits[commits.len() - 1].to_string();

    let parent_spec = format!("{}^", earliest);
    let from = match git_lines(dir, &["rev-parse", "--verify", "--quiet", &parent_spec]) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        // The window opens at the root commit, which has no parent: the root then
        // stands in as the base and its own changes fall outside the window.
        _ => earliest,
    };
    Ok((from, tip))
}

/// Join a scoped worker thread. The git reads are written not to panic, but a
/// stray panic must surface as an error instead of unwinding the process.
fn joined(result: Result<Result<String, String>, Box<dyn std::any::Any + Send>>) -> Result<String, String> {
    result.unwrap_or_else(|_| Err("分析任务已中止".to_string()))
}

pub(crate) fn collect_sync(repo_path: &str, scope: ChangeScope) -> Result<TestReportBundle, String> {
    let dir = Path::new(repo_path);
    if !dir.exists() {
        return Err(format!("路径不存在: {}", repo_path));
    }
    if !dir.is_dir() {
        return Err(format!("路径不是目录: {}", repo_path));
    }

    // Validate the baseline text before spending any process spawns on it.
    let base_ref: Option<String> = match &scope {
        ChangeScope::Ref(value) => Some(validate_ref(value)?),
        _ => None,
    };

    // Which git range describes this report.
    let (range, base_label, source, commit_range): (Vec<String>, String, String, Option<String>) = match &scope {
        ChangeScope::Uncommitted => (vec![], String::new(), "uncommitted".to_string(), None),
        ChangeScope::Ref(_) => {
            let value = base_ref.clone().unwrap_or_default();
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
        ChangeScope::Since { since, until } => {
            let (from, tip) = resolve_date_window(dir, since, until)?;
            (
                vec![from.clone(), tip.clone()],
                date_window_label(since, until),
                "since".to_string(),
                Some(format!("{}..{}", from, tip)),
            )
        }
    };

    // `git diff <anchor>` compares the working tree with that point, so staged and
    // unstaged edits land in one authoritative list instead of two overlapping ones.
    // `--no-optional-locks`: the diffs below run concurrently with each other and
    // must not race on git's opportunistic index refresh.
    let anchor: Vec<&str> = match &scope {
        ChangeScope::Uncommitted => vec!["HEAD"],
        ChangeScope::Ref(_) | ChangeScope::Commits(_) | ChangeScope::Since { .. } => {
            range.iter().map(String::as_str).collect()
        }
    };

    let mut name_args = vec!["--no-optional-locks", "diff", "--name-status", "-z"];
    name_args.extend(anchor.iter().copied());
    let mut num_args = vec!["--no-optional-locks", "diff", "--numstat", "-z"];
    num_args.extend(anchor.iter().copied());
    let mut patch_args = vec!["--no-optional-locks", "diff", "--no-color", "-U0"];
    patch_args.extend(anchor.iter().copied());
    let uncommitted = matches!(scope, ChangeScope::Uncommitted);

    // Every git read below targets the same repo and is read-only, so they all run
    // at once: on Windows each git spawn costs tens of milliseconds and a sequential
    // pass used to pay that ~10× per repo. The patch diff is the heaviest single
    // call, so it keeps a thread of its own.
    let (top, branch, head, base_ok, name_status, numstat, untracked, patch, known_raw, log_raw) = std::thread::scope(|s| {
        let h_top = s.spawn(|| git_lines(dir, &["rev-parse", "--show-toplevel"]));
        let h_branch = s.spawn(|| git_lines(dir, &["rev-parse", "--abbrev-ref", "HEAD"]));
        let h_head = s.spawn(|| git_lines(dir, &["rev-parse", "--short", "HEAD"]));
        let h_base = s.spawn(|| match base_ref.as_deref() {
            Some(value) => git_lines(dir, &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", value)]).unwrap_or_default(),
            None => String::new(),
        });
        let h_names = s.spawn(|| git_lines(dir, &scoped(name_args, &[])));
        let h_nums = s.spawn(|| git_lines(dir, &scoped(num_args, &[])));
        let h_patch = s.spawn(|| git_lines(dir, &scoped(patch_args, &[])));
        let h_others = s.spawn(|| {
            if uncommitted {
                git_lines(dir, &scoped(vec!["ls-files", "--others", "--exclude-standard", "-z"], &[]))
            } else {
                Ok(String::new())
            }
        });
        let h_tests = s.spawn(|| git_lines(dir, &scoped(vec!["ls-files", "-z"], TEST_PATHS)));
        let h_log = s.spawn(|| match commit_range.as_deref() {
            Some(range) => git_lines(
                dir,
                &["log", "--no-merges", "--format=%x02%H%x00%h%x00%an%x00%aI%x00%s%x00%b%x01", "--name-only", "-z", range],
            ),
            None => Ok(String::new()),
        });
        (
            joined(h_top.join()),
            joined(h_branch.join()).unwrap_or_default(),
            joined(h_head.join()).unwrap_or_default(),
            h_base.join().unwrap_or_default(),
            joined(h_names.join()),
            joined(h_nums.join()),
            joined(h_others.join()),
            joined(h_patch.join()),
            joined(h_tests.join()),
            joined(h_log.join()).unwrap_or_default(),
        )
    });

    // `--show-toplevel` doubles as the repo check; its failure text tells a
    // non-repo apart from anything else.
    let repo_root = top
        .map_err(|err| {
            if err.contains("not a git repository") || err.contains("work tree") {
                format!("不是 Git 仓库: {}", repo_path)
            } else {
                err
            }
        })?
        .trim()
        .to_string();
    let branch = branch.trim().to_string();
    let head = head.trim().to_string();
    let subdir = !repo_root.is_empty() && {
        let normalized_root = repo_root.replace('\\', "/").trim_end_matches('/').to_lowercase();
        let normalized_dir = repo_path.replace('\\', "/").trim_end_matches('/').to_lowercase();
        normalized_root != normalized_dir
    };
    if let Some(value) = &base_ref {
        if base_ok.trim().is_empty() {
            return Err(format!("找不到基准：{}（分支/tag/commit 是否存在？）", value));
        }
    }

    let name_status = name_status?;
    let numstat = numstat?;
    let untracked = untracked?;
    // A huge or broken patch must not cost the tester the whole report.
    let patch = patch.map(|p| truncate_bytes(&p, MAX_PATCH_BYTES)).unwrap_or_default();

    // Rust keeps its tests inline (`#[cfg(test)] mod tests`), which no path convention
    // can see, so the changed sources themselves are read for it.
    let inline_tests = inline_test_sources(&repo_root, &name_status, &untracked);

    let known_tests: std::collections::HashSet<String> = known_raw
        .map(|raw| split_z(&raw))
        .unwrap_or_default()
        .into_iter()
        .collect();

    let (subjects, commit_details, file_commits) = commit_records(&log_raw);

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
    since: Option<String>,
    until: Option<String>,
) -> Result<TestReportBundle, String> {
    let scope = scope_of(base, last_commits, since, until)?;
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
    since: Option<String>,
    until: Option<String>,
) -> Result<FolderReportBundle, String> {
    if repo_paths.is_empty() {
        return Err("该文件夹下没有可选的 Git 仓库".to_string());
    }
    let scope = scope_of(base, last_commits, since, until)?;
    let total = repo_paths.len();
    let _ = app.emit(
        "test-report-collect-progress",
        serde_json::json!({ "folder": folder_name, "done": 0, "total": total }),
    );

    // Each repo's collection is its own pile of git spawns, so run a bounded number
    // at once: a 10-repo folder used to pay ~10 sequential spawns × 10 repos; now
    // it pays ~10 spawns × ~3 waves. Results carry their original index so they can
    // be re-sorted into tree order after the unordered finish.
    use futures_util::stream::{self, StreamExt};
    let finished = AtomicUsize::new(0);
    let mut outcomes: Vec<(usize, String, Result<TestReportBundle, String>)> = stream::iter(
        repo_paths.into_iter().enumerate(),
    )
    .map(|(index, path)| {
        let scope = scope.clone();
        let app = app.clone();
        let folder_name = folder_name.clone();
        let finished = &finished;
        async move {
            let name = repo_display_name(&path);
            let outcome = tokio::task::spawn_blocking(move || collect_sync(&path, scope))
                .await
                .map_err(|e| format!("分析任务已中止: {}", e))
                .and_then(|r| r);
            let done = finished.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = app.emit(
                "test-report-collect-progress",
                serde_json::json!({ "folder": folder_name, "done": done, "total": total }),
            );
            (index, name, outcome)
        }
    })
    .buffer_unordered(FOLDER_COLLECT_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    outcomes.sort_by_key(|(index, _, _)| *index);

    let mut collected: Vec<(String, TestReportBundle)> = Vec::new();
    let mut rows: Vec<FolderRepoRow> = Vec::new();
    for (_, name, outcome) in outcomes {
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
/// patch, so it always matches what the panel just showed. `requirement` is the
/// requirement/acceptance text the tester pasted in (optional) — with it the answer
/// also has to say which asks the change covered and which it missed. Large reports
/// go through a map-reduce pass: one brief per module chunk, then one final call
/// assembles the sections — so no chunk's files silently fall off the end of the prompt.
/// Whether a failed call is worth retrying: the provider asking us to slow down
/// (429 / concurrency limit), or a transient upstream hiccup.
///
/// Matching on the message text is deliberate — the endpoints in play
/// (OpenAI-compatible, Anthropic, local DeepSeek) each word this differently and a
/// string is all we get back. A bad key or an unknown model must NOT match:
/// retrying those only makes the tester wait longer for the same error.
fn is_retryable(err: &str) -> bool {
    let text = err.to_lowercase();
    [
        "429",
        "rate limit",
        "ratelimit",
        "too many requests",
        "concurren",
        "并发",
        "限流",
        "请求过多",
        "请求频率",
        "timed out",
        "timeout",
        "502",
        "503",
        "504",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// One model call with retries. Shared by the single/reduce path and every map
/// brief, so both obey exactly one retry policy.
async fn send_with_retry(config: AIModelConfig, system: String, user: String) -> Result<String, String> {
    let mut attempt = 0usize;
    loop {
        let result = generate_text(GenerateTextRequest {
            config: config.clone(),
            system: system.clone(),
            user: user.clone(),
        })
        .await;
        match result {
            Ok(text) => return Ok(text),
            Err(err) => {
                // Back off a little longer each round.
                if attempt >= AI_RETRIES || !is_retryable(&err) {
                    return Err(err);
                }
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(AI_RETRY_BASE_MS * attempt as u64)).await;
            }
        }
    }
}

#[tauri::command]
pub async fn generate_test_report_ai(
    app: AppHandle,
    report_id: String,
    config: AIModelConfig,
    system: Option<String>,
    requirement: Option<String>,
) -> Result<AiResult, String> {
    // Cancel key = report id (	r-<millis>-<seq>), which can never collide with the
    // project ids used as keys by other long-running commands.

    let _guard = CancelGuard::new(&report_id);
    let entry = cache_get(&report_id).ok_or("报告已过期，请重新采集")?;
    let project_name = entry.repo_name;
    let report = entry.report;
    let patch = entry.ai_patch;
    let requirement: String = requirement.unwrap_or_default().chars().take(MAX_REQUIREMENT_CHARS).collect();

    let mut config = config;
    config.temperature = 0.2;
    let system_prompt = std::sync::Arc::new(system.unwrap_or_else(|| AI_SYSTEM.to_string()));

    let chunks = change_report::plan_ai_chunks(&report, AI_CHUNK_FILES, AI_CHUNK_APIS);
    // Every map call plus the final reduce; a single-pass report is just "the call".
    let total = if chunks.len() <= 1 { 1 } else { chunks.len() + 1 };
    emit_ai_progress(&app, &report_id, 0, total);

    let markdown = if chunks.len() <= 1 {
        config.max_tokens = AI_REDUCE_TOKENS;
        send_with_retry(
            config,
            (*system_prompt).clone(),
            build_ai_prompt(&project_name, &report, &patch, &requirement),
        )
        .await
        .map_err(|e| format!("{} 生成失败：{}", project_name, e))?
    } else {
        use futures_util::stream::{self, StreamExt};
        // Module briefs are independent, so ask for them concurrently (bounded):
        // an N-module report drops from N sequential round trips to ~ceil(N/concurrency).
        // Build every request up front (cheap, synchronous) so the stream owns its data
        // and never borrows `config`/`report`. Results are placed back by index to keep
        // module order; progress ticks as each lands; an error or cancel drops the stream,
        // cancelling in-flight calls.
        let requests: Vec<(usize, String, String, AIModelConfig)> = chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                let mut map_config = config.clone();
                map_config.max_tokens = AI_MAP_TOKENS;
                (
                    index,
                    chunk_label(chunk),
                    build_ai_chunk_prompt(&project_name, &report, chunk, &requirement),
                    map_config,
                )
            })
            .collect();
        let concurrency = requests.len().min(AI_MAP_CONCURRENCY);
        let mut summaries: Vec<Option<String>> = (0..requests.len()).map(|_| None).collect();
        let mut done = 0usize;
        let mut briefs = stream::iter(requests)
            .map(|(index, module, prompt, map_config)| {
                let system = system_prompt.clone();
                async move {
                    let result = send_with_retry(map_config, (*system).clone(), prompt).await;
                    (index, module, result)
                }
            })
            .buffer_unordered(concurrency);
        while let Some((index, module, result)) = briefs.next().await {
            let brief = result.map_err(|e| format!("{} 生成失败（模块 {}）：{}", project_name, module, e))?;
            done += 1;
            emit_ai_progress(&app, &report_id, done, total);
            summaries[index] = Some(format!("【{}】\n{}", module, brief.trim()));
            if cancellation::is_cancelled(&report_id) {
                return Err("[E_CANCELLED] 生成已取消".to_string());
            }
        }
        let summaries: Vec<String> = summaries.into_iter().flatten().collect();
        if cancellation::is_cancelled(&report_id) {
            return Err("[E_CANCELLED] 生成已取消".to_string());
        }
        config.max_tokens = AI_REDUCE_TOKENS;
        send_with_retry(
            config,
            (*system_prompt).clone(),
            build_ai_reduce_prompt(&project_name, &report, &summaries, &requirement),
        )
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

/// How much of a pasted requirement the map pass sees. The reduce pass gets the
/// full text; the map pass only needs enough to know which ask a module serves,
/// and it runs once per chunk (4 at a time), so it stays bounded on purpose.
const AI_MAP_REQUIREMENT_CHARS: usize = 1500;

/// A tester may paste a whole PRD into the box; cap it once at the command
/// boundary so no single model call can be blown out by it.
const MAX_REQUIREMENT_CHARS: usize = 8000;

/// The requirement / acceptance text a tester pasted in. Empty means "none
/// given" — every prompt then keeps its original code-only wording, so the
/// feature degrades to exactly what it did before.
fn requirement_block(requirement: &str, limit: Option<usize>) -> String {
    let text = requirement.trim();
    if text.is_empty() {
        return String::new();
    }
    let (body, cut) = match limit {
        Some(max) if text.chars().count() > max => {
            let head: String = text.chars().take(max).collect();
            (head, true)
        }
        _ => (text.to_string(), false),
    };
    format!(
        "需求与验收标准（由产品/测试提供，是判断「该测什么」的主要依据）：\n{}\n{}\n",
        body,
        if cut { "（以上为节选，完整需求见最终汇总）" } else { "" }
    )
}

/// The section list + per-line format rules. A requirement adds the coverage
/// cross-check section: "did the change actually answer the ask" is the one
/// question a tester cannot answer from a diff alone. The scenario line format
/// is fixed and pipe-delimited so the panel can render it as a case, not prose.
fn output_rules(has_requirement: bool) -> String {
    // No「建议回归范围」section on purpose: the panel already shows the
    // program-computed scope chips right above the AI output, so asking the model
    // to restate them was pure output-token cost — and output is what a local
    // model is slowest at.
    let mut sections = "## 受影响功能点\n## 必测场景\n## 兼容性与数据风险\n## 验收清单\n".to_string();
    if has_requirement {
        sections.push_str("## 需求覆盖对照\n");
    }
    let mut rules = "严格按以下二级标题输出，不要添加其它章节或解释文字：\n".to_string();
    rules.push_str(&sections);
    rules.push_str(
        "\n要求：每条一行、以 - 开头；\
         「必测场景」每条固定格式 `- [P0] 场景名 ｜ 前置：… ｜ 步骤：… ｜ 预期：…`\
         （优先级只取 P0/P1/P2，字段之间用全角竖线 ｜ 分隔，四个字段都要写）；\
         每个章节不超过 8 条，整份回答尽量精炼——每多输出一个字，本地模型就多生成一个字；\
         只依据上面给出的文件与接口，不确定的写「需与开发确认」。",
    );
    if has_requirement {
        rules.push_str(
            "「需求覆盖对照」逐条对应需求点，格式 `- [已覆盖] 需求点 ｜ 依据：文件或接口 ｜ 缺口：…`，\
             覆盖判断只能取 已覆盖 / 部分覆盖 / 未见对应改动；\
             需求中提到但改动里找不到对应文件的，必须判为「未见对应改动」，不要替它找理由。",
        );
    }
    rules
}

fn build_ai_prompt(project_name: &str, report: &ChangeReport, patch: &str, requirement: &str) -> String {
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
    let ask = requirement_block(requirement, None);
    let rules = output_rules(!ask.is_empty());

    format!(
        "请根据下面的代码改动分析，产出面向测试同学的回归测试报告。\n\n\
         项目：{}\n基准：{}\n统计：{} 个文件，+{} -{}，涉及 {} 个模块，其中 {} 个改动没有配对测试\n\
         建议回归面（程序判定）：{}\n\
         {}\
         提交明细（短 sha、说明、改动文件）：\n{}\n\
         改动文件：\n{}\n\
         公开接口变化：\n{}\n\
         补丁片段（可能已截断）：\n```diff\n{}\n```\n\n\
         {}",
        project_name,
        base,
        report.stats.files,
        report.stats.adds,
        report.stats.dels,
        report.stats.modules,
        report.stats.untested,
        if report.scope.is_empty() { "-".to_string() } else { report.scope.join(",") },
        ask,
        if commits.is_empty() { "- 无（未提交改动）\n".to_string() } else { commits },
        files,
        if api.is_empty() { "- 无\n".to_string() } else { api },
        if patch.is_empty() { "（无补丁文本）".to_string() } else { patch.to_string() },
        rules,
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

/// One line per file. The module leads because a chunk may now span several of
/// them, so the grouping has to be spelled out instead of being implied by the
/// call the file happens to sit in.
fn file_line(file: &change_report::FileChange) -> String {
    format!(
        "- [{}] {} [{}] +{} -{} 层={} 风险={} 已有测试={}{}\n",
        file.module,
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

/// A short label for progress messages and error text, where the full module list
/// would be noise.
fn chunk_label(chunk: &change_report::AiChunk) -> String {
    match chunk.modules.split_first() {
        None => "-".to_string(),
        Some((first, rest)) if rest.is_empty() => first.clone(),
        Some((first, rest)) => format!("{} 等 {} 个模块", first, rest.len() + 1),
    }
}

/// Map pass: one brief covering a batch of files, which may span several modules.
/// No patch on purpose — the chunk is small enough to judge from its own file and
/// interface lists, and pasting the same global patch into every call would only
/// bury them.
fn build_ai_chunk_prompt(
    project_name: &str,
    report: &ChangeReport,
    chunk: &change_report::AiChunk,
    requirement: &str,
) -> String {
    let files: String = chunk.files.iter().map(file_line).collect();
    let mut api = String::new();
    for change in &chunk.api {
        api.push_str(&format!("- [{}] {} @ {}\n", change.kind, change.name, change.path));
    }
    let ask = requirement_block(requirement, Some(AI_MAP_REQUIREMENT_CHARS));
    let modules = if chunk.modules.is_empty() {
        "-".to_string()
    } else {
        chunk.modules.join("、")
    };
    let module_count = chunk.modules.len().max(1);
    format!(
        "项目「{}」本次共改动 {} 个文件（+{} -{}），下面是 {} 个模块的改动明细：{}。\n\
         全量建议回归面（程序判定）：{}。\n\n\
         {}\
         改动文件（行首方括号是所属模块）：\n{}\n\
         公开接口变化：\n{}\n\
         请**按模块分节**总结，每节以「【模块名】」开头、不超过 5 行要点：改了什么、可能影响哪些功能、\
         有哪些兼容性或数据风险值得测试关注。若给了需求，额外说明该模块与需求中哪些点相关（无关就直接说无关）。\
         只依据上面内容，不确定的写「需与开发确认」；不要输出其它章节标题或解释文字。",
        project_name,
        report.stats.files,
        report.stats.adds,
        report.stats.dels,
        module_count,
        modules,
        if report.scope.is_empty() { "-".to_string() } else { report.scope.join(",") },
        ask,
        files,
        if api.is_empty() { "- 无\n".to_string() } else { api },
    )
}

/// Reduce pass: the tester-facing sections are assembled from the module
/// briefs, so no chunk's detail is dropped no matter how big the report is.
/// The full requirement is replayed here — this is the call that has to answer
/// "was the ask covered", and the map briefs only saw an excerpt of it.
fn build_ai_reduce_prompt(
    project_name: &str,
    report: &ChangeReport,
    summaries: &[String],
    requirement: &str,
) -> String {
    let base = if report.base.is_empty() { "未提交改动（相对 HEAD）".to_string() } else { report.base.clone() };
    let commits = commits_block(report);
    let ask = requirement_block(requirement, None);
    let rules = output_rules(!ask.is_empty());
    format!(
        "请把下面按模块整理的改动小结，汇总成一份面向测试同学的回归测试报告。\n\n\
         项目：{}\n基准：{}\n统计：{} 个文件，+{} -{}，涉及 {} 个模块，其中 {} 个改动没有配对测试\n\
         建议回归面（程序判定）：{}\n\
         {}\
         提交明细（短 sha、说明、改动文件）：\n{}\n\
         各模块改动小结：\n{}\n\n\
         {}",
        project_name,
        base,
        report.stats.files,
        report.stats.adds,
        report.stats.dels,
        report.stats.modules,
        report.stats.untested,
        if report.scope.is_empty() { "-".to_string() } else { report.scope.join(",") },
        ask,
        if commits.is_empty() { "- 无（未提交改动）\n".to_string() } else { commits },
        summaries.join("\n\n"),
        rules,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirement_block_is_empty_without_an_ask() {
        assert!(requirement_block("", None).is_empty());
        assert!(requirement_block("   \n\t ", None).is_empty());
    }

    #[test]
    fn requirement_block_keeps_the_text_and_marks_an_excerpt() {
        let full = requirement_block("新增退款审批流程", None);
        assert!(full.contains("新增退款审批流程"));
        assert!(!full.contains("节选"));

        let cut = requirement_block("abcdef", Some(3));
        assert!(cut.contains("abc"));
        assert!(!cut.contains("def"), "excerpt must not leak the tail");
        assert!(cut.contains("节选"));
    }

    #[test]
    fn output_rules_only_add_the_coverage_section_with_an_ask() {
        let without = output_rules(false);
        assert!(without.contains("## 必测场景"));
        assert!(!without.contains("需求覆盖对照"));
        // Dropped on purpose: the static scope chips above the AI output already
        // carry this, and restating it only spends generation time.
        assert!(!without.contains("## 建议回归范围"));

        let with = output_rules(true);
        assert!(with.contains("## 需求覆盖对照"));
        // The scenario line shape the panel parses back (`parseAiLine`) is pinned here.
        assert!(with.contains("｜"));
        assert!(with.contains("已覆盖"));
    }

    #[test]
    fn iso_date_accepts_only_the_input_shape() {
        assert!(is_iso_date("2026-09-23"));
        assert!(!is_iso_date("2026-9-23"));
        assert!(!is_iso_date("2026/09/23"));
        assert!(!is_iso_date("2026-09-23 10:00"));
        assert!(!is_iso_date(""));
        // A bound that could be read as a git option must never pass.
        assert!(!is_iso_date("--since=2026-09-23"));
    }

    #[test]
    fn date_bound_trims_and_rejects_junk() {
        assert_eq!(date_bound(None).unwrap(), None);
        assert_eq!(date_bound(Some("   ".to_string())).unwrap(), None);
        assert_eq!(
            date_bound(Some(" 2026-09-23 ".to_string())).unwrap(),
            Some("2026-09-23".to_string())
        );
        assert!(date_bound(Some("yesterday".to_string())).is_err());
        assert!(date_bound(Some("-d".to_string())).is_err());
    }

    #[test]
    fn scope_prefers_ref_then_count_then_window() {
        let by_ref = scope_of(Some("main".to_string()), Some(5), Some("2026-09-01".to_string()), None).unwrap();
        assert!(matches!(by_ref, ChangeScope::Ref(value) if value == "main"));

        let by_count = scope_of(None, Some(3), Some("2026-09-01".to_string()), None).unwrap();
        assert!(matches!(by_count, ChangeScope::Commits(3)));

        let by_window =
            scope_of(None, None, Some("2026-09-01".to_string()), Some("2026-09-23".to_string())).unwrap();
        match by_window {
            ChangeScope::Since { since, until } => {
                assert_eq!(since.as_deref(), Some("2026-09-01"));
                assert_eq!(until.as_deref(), Some("2026-09-23"));
            }
            other => panic!("expected a date window, got {:?}", other),
        }
    }

    #[test]
    fn scope_without_any_baseline_is_the_working_tree() {
        assert!(matches!(scope_of(None, None, None, None).unwrap(), ChangeScope::Uncommitted));
    }

    #[test]
    fn a_backwards_window_is_rejected() {
        let err = scope_of(None, None, Some("2026-09-23".to_string()), Some("2026-09-01".to_string()))
            .expect_err("a window ending before it starts must not be accepted");
        assert!(err.contains("晚于"), "unexpected message: {}", err);
    }

    #[test]
    fn date_window_label_names_the_window() {
        assert_eq!(
            date_window_label(&Some("2026-09-01".into()), &Some("2026-09-23".into())),
            "2026-09-01 ~ 2026-09-23"
        );
        assert_eq!(date_window_label(&Some("2026-09-01".into()), &None), ">= 2026-09-01");
        assert_eq!(date_window_label(&None, &Some("2026-09-23".into())), "<= 2026-09-23");
    }

    #[test]
    fn retryable_errors_are_told_apart_from_permanent_ones() {
        // The shapes the providers in play actually return under load.
        assert!(is_retryable("HTTP 429 Too Many Requests"));
        assert!(is_retryable("rate limit exceeded, please retry"));
        assert!(is_retryable("Concurrency limit reached"));
        assert!(is_retryable("请求并发数超过上限"));
        assert!(is_retryable("触发限流，请稍后重试"));
        assert!(is_retryable("request timed out"));
        assert!(is_retryable("503 Service Temporarily Unavailable"));

        // Retrying these only makes the tester wait longer for the same error.
        assert!(!is_retryable("401 Unauthorized: invalid api key"));
        assert!(!is_retryable("model not found"));
        assert!(!is_retryable("connection refused"));
    }
}
