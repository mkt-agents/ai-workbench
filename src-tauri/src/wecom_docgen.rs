//! 逆向文档的每日自动更新 + 多通道群通知。
//!
//! 到点 / cron / 远端轮询 / 手动：
//! 1. fetch 最新 release 分支（脏工作区则跳过，不强制 reset）；
//! 2. 取自上次更新以来的提交摘要 + 文件列表 + 有界 diff；
//! 3. 用 `agent_run`（编辑档）按变更类型策略更新 `doc/`；
//! 4. 可选质量自检（只读第二轮）；预览模式不落盘并清仓；
//! 5. 多通道 webhook 通知；历史写入 `docgen-history.jsonl`（30 条）。
//!
//! 游标：`%APPDATA%\com.ai-workbench.app\wecom-dsh\docgen.json`。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::wecom_channel::WecomState;

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(15);
const UPDATE_TURN_TIMEOUT: Duration = Duration::from_secs(600);
const DIFF_CHAR_CAP: usize = 24_000;
const HISTORY_CAP: usize = 30;
const LOG_LINE_CAP: usize = 30;
const FILE_LIST_CAP: usize = 80;

static RUNNING: AtomicBool = AtomicBool::new(false);

struct RunGuard;
impl RunGuard {
    fn acquire() -> Option<RunGuard> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(RunGuard)
        }
    }
}
impl Drop for RunGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// 设置页下发的自动更新配置快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocgenConfig {
    pub enabled: bool,
    /// `HH:MM`，空则按 23:00；`cron` 非空时被覆盖。
    pub time: String,
    /// 可选 5 段 unix cron（分 时 日 月 周）；非空则覆盖每日时刻。
    #[serde(default)]
    pub cron: String,
    pub release_branch: String,
    pub workspace: String,
    pub doc_dir: String,
    /// 企微群 webhook（兼容旧字段名）。
    pub webhook_url: String,
    #[serde(default)]
    pub webhook_feishu: String,
    #[serde(default)]
    pub webhook_dingtalk: String,
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    #[serde(default)]
    pub auto_push: bool,
    /// 预览：跑完 agent 后不 commit，清掉 doc_dir 改动再通知。
    #[serde(default)]
    pub preview_mode: bool,
    /// 更新后再跑一轮只读质量自检。
    #[serde(default)]
    pub quality_check: bool,
    /// 轮询 origin/release HEAD，超前则自动跑（不装 git hook）。
    #[serde(default)]
    pub poll_release: bool,
}

fn default_true() -> bool {
    true
}

impl Default for DocgenConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            time: String::new(),
            cron: String::new(),
            release_branch: String::new(),
            workspace: String::new(),
            doc_dir: String::new(),
            webhook_url: String::new(),
            webhook_feishu: String::new(),
            webhook_dingtalk: String::new(),
            auto_commit: true,
            auto_push: false,
            preview_mode: false,
            quality_check: false,
            poll_release: false,
        }
    }
}

impl DocgenConfig {
    pub fn time_or_default(&self) -> String {
        let t = self.time.trim();
        if t.is_empty() {
            "23:00".into()
        } else {
            t.into()
        }
    }
    pub fn branch_or_default(&self) -> String {
        let b = self.release_branch.trim();
        if b.is_empty() {
            "release".into()
        } else {
            b.into()
        }
    }
    pub fn doc_dir_or_default(&self) -> String {
        let d = self.doc_dir.trim();
        if d.is_empty() {
            "doc".into()
        } else {
            d.into()
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RunState {
    last_run_date: Option<String>,
    last_commit: Option<String>,
    /// 轮询防抖：上次已观察过的 origin/{branch}。
    #[serde(default)]
    last_poll_head: Option<String>,
}

fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = base.join("com.ai-workbench.app").join("wecom-dsh");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("docgen.json"))
}

fn history_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = base.join("com.ai-workbench.app").join("wecom-dsh");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("docgen-history.jsonl"))
}

fn load_state() -> RunState {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(st: &RunState) {
    if let Some(p) = state_path() {
        if let Ok(text) = serde_json::to_string_pretty(st) {
            let _ = std::fs::write(p, text);
        }
    }
}

/// 一次运行的触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunTrigger {
    Schedule,
    Poll,
    Manual,
    Preview,
}

impl RunTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Poll => "poll",
            Self::Manual => "manual",
            Self::Preview => "preview",
        }
    }
}

/// 历史条目（设置页表格）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocgenHistoryItem {
    pub at: String,
    pub trigger: String,
    pub branch: String,
    pub status: String,
    pub changed_files: usize,
    pub updated_docs: String,
    #[serde(default)]
    pub commit_note: String,
    #[serde(default)]
    pub quality: String,
    pub pushed: bool,
    #[serde(default)]
    pub error: String,
    pub preview: bool,
    #[serde(default)]
    pub webhook_errors: Vec<String>,
}

pub fn list_history() -> Vec<DocgenHistoryItem> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut items: Vec<DocgenHistoryItem> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    items.reverse(); // 新 → 旧
    items
}

fn append_history(item: &DocgenHistoryItem) {
    let Some(path) = history_path() else {
        return;
    };
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Ok(line) = serde_json::to_string(item) {
        lines.push(line);
    }
    while lines.len() > HISTORY_CAP {
        lines.remove(0);
    }
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

fn history_from_report(
    trigger: RunTrigger,
    report: &UpdateReport,
    quality: &str,
    webhook_errors: &[String],
) -> DocgenHistoryItem {
    let preview = trigger == RunTrigger::Preview || report.preview;
    let status = if report.skipped_reason.is_some() {
        "skip"
    } else {
        "ok"
    };
    DocgenHistoryItem {
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        trigger: trigger.as_str().into(),
        branch: report.branch.clone(),
        status: status.into(),
        changed_files: report.changed_files.len(),
        updated_docs: truncate_chars(&report.updated_docs, 800),
        commit_note: report.commit_note.clone(),
        quality: truncate_chars(quality, 800),
        pushed: report.pushed,
        error: report.skipped_reason.clone().unwrap_or_default(),
        preview,
        webhook_errors: webhook_errors.to_vec(),
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    format!("{}…", t.chars().take(max).collect::<String>())
}

/// 由 `daily_schedule::spawn` 每分钟调用。
pub async fn auto_tick(app: &AppHandle) -> Result<(), String> {
    let cfg = {
        let st = app.state::<WecomState>();
        let g = st.docgen.lock().await;
        g.clone()
    };
    if !cfg.enabled || cfg.workspace.trim().is_empty() {
        return Ok(());
    }

    let now = chrono::Local::now();
    let state = load_state();

    // 1) 定点 / cron
    let schedule_due = match schedule_due_today(&cfg, state.last_run_date.as_deref(), now) {
        Ok(v) => v,
        Err(e) => {
            crate::wecom_log::write("docgen", &format!("schedule parse fail-closed: {e}"));
            None
        }
    };
    if let Some(today) = schedule_due {
        let mut st = load_state();
        st.last_run_date = Some(today);
        save_state(&st);
        crate::wecom_log::write(
            "docgen",
            &format!("auto schedule start branch={}", cfg.branch_or_default()),
        );
        match run_update_with_trigger(app, &cfg, RunTrigger::Schedule).await {
            Ok(report) => {
                crate::wecom_log::write("docgen", &format!("auto schedule done: {}", report.summary_line()));
            }
            Err(e) => {
                crate::wecom_log::write("docgen", &format!("auto schedule failed: {e}"));
                let (pushed, errs) = notify_all(
                    &cfg,
                    &format!("⚠️ 逆向文档自动更新失败：{e}"),
                )
                .await;
                append_history(&DocgenHistoryItem {
                    at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    trigger: RunTrigger::Schedule.as_str().into(),
                    branch: cfg.branch_or_default(),
                    status: "fail".into(),
                    changed_files: 0,
                    updated_docs: String::new(),
                    commit_note: String::new(),
                    quality: String::new(),
                    pushed,
                    error: e.clone(),
                    preview: false,
                    webhook_errors: errs,
                });
                return Err(e);
            }
        }
    }

    // 2) 远端轮询（不挡定点；成功只推进 last_commit）
    if cfg.poll_release {
        if let Err(e) = maybe_poll_release(app, &cfg).await {
            crate::wecom_log::write("docgen", &format!("poll skip/fail: {e}"));
        }
    }
    Ok(())
}

fn schedule_due_today(
    cfg: &DocgenConfig,
    last_run_date: Option<&str>,
    now: chrono::DateTime<chrono::Local>,
) -> Result<Option<String>, String> {
    let cron = cfg.cron.trim();
    if !cron.is_empty() {
        let today = now.format("%Y-%m-%d").to_string();
        if last_run_date == Some(today.as_str()) {
            return Ok(None);
        }
        if cron_matches(cron, now)? {
            return Ok(Some(today));
        }
        return Ok(None);
    }
    Ok(crate::daily_schedule::due_today(
        &cfg.time_or_default(),
        last_run_date,
        now,
    ))
}

async fn maybe_poll_release(app: &AppHandle, cfg: &DocgenConfig) -> Result<(), String> {
    let workspace = cfg.workspace.trim().to_string();
    let branch = cfg.branch_or_default();
    crate::git_commands::ensure_git_repo(&workspace)?;

    let dirty = {
        let ws = workspace.clone();
        tokio::task::spawn_blocking(move || {
            crate::git_commands::git_output(&ws, &["status", "--porcelain"])
        })
        .await
        .map_err(|e| e.to_string())??
    };
    if !String::from_utf8_lossy(&dirty.stdout).trim().is_empty() {
        return Ok(());
    }

    let remote_head = {
        let ws = workspace.clone();
        let br = branch.clone();
        tokio::task::spawn_blocking(move || {
            let _ = crate::git_commands::git_output(&ws, &["fetch", "origin", &br]);
            crate::git_commands::git_stdout(&ws, &["rev-parse", &format!("origin/{br}")])
        })
        .await
        .map_err(|e| e.to_string())??
    };
    let remote_head = remote_head.trim().to_string();
    if remote_head.is_empty() {
        return Ok(());
    }

    let mut st = load_state();
    if st.last_poll_head.as_deref() == Some(remote_head.as_str()) {
        return Ok(());
    }
    if st.last_commit.as_deref() == Some(remote_head.as_str()) {
        st.last_poll_head = Some(remote_head);
        save_state(&st);
        return Ok(());
    }

    st.last_poll_head = Some(remote_head.clone());
    save_state(&st);

    crate::wecom_log::write(
        "docgen",
        &format!("auto poll start remote_head={}", &remote_head[..8.min(remote_head.len())]),
    );
    match run_update_with_trigger(app, cfg, RunTrigger::Poll).await {
        Ok(report) => {
            crate::wecom_log::write("docgen", &format!("auto poll done: {}", report.summary_line()));
            Ok(())
        }
        Err(e) => {
            let (pushed, errs) = notify_all(cfg, &format!("⚠️ 逆向文档轮询更新失败：{e}")).await;
            append_history(&DocgenHistoryItem {
                at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                trigger: RunTrigger::Poll.as_str().into(),
                branch,
                status: "fail".into(),
                changed_files: 0,
                updated_docs: String::new(),
                commit_note: String::new(),
                quality: String::new(),
                pushed,
                error: e.clone(),
                preview: false,
                webhook_errors: errs,
            });
            Err(e)
        }
    }
}

/// 一次更新的结果摘要。
pub struct UpdateReport {
    pub branch: String,
    pub new_commits: usize,
    pub changed_files: Vec<String>,
    pub updated_docs: String,
    pub commit_note: String,
    pub pushed: bool,
    pub skipped_reason: Option<String>,
    pub preview: bool,
    pub quality: String,
    pub webhook_errors: Vec<String>,
}

impl UpdateReport {
    fn summary_line(&self) -> String {
        if let Some(r) = &self.skipped_reason {
            return format!("skipped: {r}");
        }
        format!(
            "branch={} commits={} files={} pushed={} preview={}",
            self.branch,
            self.new_commits,
            self.changed_files.len(),
            self.pushed,
            self.preview
        )
    }
}

/// 手动「立即更新」。
pub async fn run_update(app: &AppHandle, cfg: &DocgenConfig) -> Result<UpdateReport, String> {
    let trigger = if cfg.preview_mode {
        RunTrigger::Preview
    } else {
        RunTrigger::Manual
    };
    run_update_with_trigger(app, cfg, trigger).await
}

pub async fn run_update_with_trigger(
    app: &AppHandle,
    cfg: &DocgenConfig,
    trigger: RunTrigger,
) -> Result<UpdateReport, String> {
    let Some(_guard) = RunGuard::acquire() else {
        return Err("已有一个文档更新流程在跑，请稍后再试".into());
    };
    let workspace = cfg.workspace.trim().to_string();
    let branch = cfg.branch_or_default();
    let doc_dir = cfg.doc_dir_or_default();
    let preview = cfg.preview_mode || trigger == RunTrigger::Preview;

    crate::git_commands::ensure_git_repo(&workspace)?;

    let dirty = {
        let ws = workspace.clone();
        tokio::task::spawn_blocking(move || {
            crate::git_commands::git_output(&ws, &["status", "--porcelain"])
        })
        .await
        .map_err(|e| e.to_string())??
    };
    if !dirty.status.success() {
        return Err("读取工作区状态失败".into());
    }
    if !String::from_utf8_lossy(&dirty.stdout).trim().is_empty() {
        let reason = "工作区有未提交改动，已跳过自动更新（请先提交或暂存）".to_string();
        let (pushed, errs) =
            notify_all(cfg, &format!("⚠️ 逆向文档自动更新已跳过：{reason}")).await;
        let report = UpdateReport {
            branch,
            new_commits: 0,
            changed_files: Vec::new(),
            updated_docs: String::new(),
            commit_note: String::new(),
            pushed,
            skipped_reason: Some(reason),
            preview,
            quality: String::new(),
            webhook_errors: errs,
        };
        append_history(&history_from_report(trigger, &report, "", &report.webhook_errors));
        return Ok(report);
    }

    {
        let ws = workspace.clone();
        let br = branch.clone();
        tokio::task::spawn_blocking(move || update_release_branch(&ws, &br))
            .await
            .map_err(|e| e.to_string())??;
    }

    let prev = load_state().last_commit;
    let delta = {
        let ws = workspace.clone();
        let prev2 = prev.clone();
        tokio::task::spawn_blocking(move || collect_delta(&ws, prev2.as_deref()))
            .await
            .map_err(|e| e.to_string())??
    };

    if delta.files.is_empty() {
        let mut st = load_state();
        st.last_commit = Some(delta.head.clone());
        save_state(&st);
        let report = UpdateReport {
            branch,
            new_commits: 0,
            changed_files: Vec::new(),
            updated_docs: String::new(),
            commit_note: String::new(),
            pushed: false,
            skipped_reason: Some("自上次更新以来无代码变更".into()),
            preview,
            quality: String::new(),
            webhook_errors: Vec::new(),
        };
        append_history(&history_from_report(trigger, &report, "", &[]));
        return Ok(report);
    }

    let updated_docs = update_docs_with_agent(app, &workspace, &doc_dir, &branch, &delta)
        .await
        .unwrap_or_else(|e| format!("（文档更新步骤出错：{e}）"));

    let quality = if cfg.quality_check {
        quality_check_with_agent(app, &workspace, &doc_dir, &branch, &delta.files)
            .await
            .unwrap_or_else(|e| format!("（质量自检失败：{e}）"))
    } else {
        String::new()
    };

    let mut commit_note = String::new();
    if preview {
        let ws = workspace.clone();
        let dd = doc_dir.clone();
        let _ = tokio::task::spawn_blocking(move || revert_doc_dir(&ws, &dd))
            .await
            .map_err(|e| e.to_string())?;
        commit_note = "预览模式：文档改动已丢弃，未提交".into();
    } else if cfg.auto_commit {
        let ws = workspace.clone();
        let dd = doc_dir.clone();
        let br = branch.clone();
        let push = cfg.auto_push;
        let n = delta.files.len();
        commit_note = tokio::task::spawn_blocking(move || commit_docs(&ws, &dd, &br, n, push))
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|e| format!("（文档提交失败：{e}）"));
    }

    let body = compose_notice(
        &branch,
        &delta.files,
        &updated_docs,
        &commit_note,
        &quality,
        preview,
    );
    let (pushed, webhook_errors) = notify_all(cfg, &body).await;

    let mut st = load_state();
    st.last_commit = Some(delta.head);
    save_state(&st);

    let report = UpdateReport {
        branch,
        new_commits: delta.commits.len().max(delta.files.len()),
        changed_files: delta.files,
        updated_docs,
        commit_note,
        pushed,
        skipped_reason: None,
        preview,
        quality: quality.clone(),
        webhook_errors: webhook_errors.clone(),
    };
    let hist_trigger = if preview {
        RunTrigger::Preview
    } else {
        trigger
    };
    append_history(&history_from_report(
        hist_trigger,
        &report,
        &quality,
        &webhook_errors,
    ));
    Ok(report)
}

fn commit_docs(
    workspace: &str,
    doc_dir: &str,
    branch: &str,
    code_changes: usize,
    push: bool,
) -> Result<String, String> {
    use crate::git_commands::git_output;
    let dir = doc_dir.trim_end_matches('/');
    let add = git_output(workspace, &["add", "--", dir])?;
    if !add.status.success() {
        return Err(format!(
            "git add {dir} 失败：{}",
            String::from_utf8_lossy(&add.stderr).trim()
        ));
    }
    let staged = git_output(workspace, &["diff", "--cached", "--name-only", "--", dir])?;
    let staged_list = String::from_utf8_lossy(&staged.stdout);
    let changed_docs: Vec<&str> = staged_list
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if changed_docs.is_empty() {
        return Ok("文档无改动，未提交".into());
    }
    let msg = format!(
        "docs: 自动同步逆向文档（{branch}，{code_changes} 处代码变更，{} 篇文档）",
        changed_docs.len()
    );
    let commit = git_output(workspace, &["commit", "-m", &msg, "--", dir])?;
    if !commit.status.success() {
        return Err(format!(
            "git commit 失败：{}",
            String::from_utf8_lossy(&commit.stderr).trim()
        ));
    }
    if push {
        let p = git_output(workspace, &["push", "origin", branch])?;
        if !p.status.success() {
            return Ok(format!(
                "已提交 {} 篇文档，但推送失败：{}",
                changed_docs.len(),
                String::from_utf8_lossy(&p.stderr).trim()
            ));
        }
        return Ok(format!("已提交并推送 {} 篇文档", changed_docs.len()));
    }
    Ok(format!("已提交 {} 篇文档（未推送）", changed_docs.len()))
}

/// 预览清仓：还原已跟踪 + 删除 doc_dir 下未跟踪新建。
fn revert_doc_dir(workspace: &str, doc_dir: &str) -> Result<(), String> {
    use crate::git_commands::git_output;
    let dir = doc_dir.trim_end_matches('/');
    let _ = git_output(workspace, &["checkout", "--", dir]);
    let _ = git_output(workspace, &["clean", "-fd", "--", dir]);
    Ok(())
}

fn update_release_branch(workspace: &str, branch: &str) -> Result<(), String> {
    use crate::git_commands::{git_output, git_stdout};
    let fetch = git_output(workspace, &["fetch", "origin", branch])?;
    if !fetch.status.success() {
        return Err(format!(
            "fetch origin {branch} 失败：{}",
            String::from_utf8_lossy(&fetch.stderr).trim()
        ));
    }
    let cur = git_stdout(workspace, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
    if cur != branch {
        let co = git_output(workspace, &["checkout", branch])?;
        if !co.status.success() {
            let co2 =
                git_output(workspace, &["checkout", "-B", branch, &format!("origin/{branch}")])?;
            if !co2.status.success() {
                return Err(format!(
                    "切换到 {branch} 失败：{}",
                    String::from_utf8_lossy(&co2.stderr).trim()
                ));
            }
        }
    }
    let pull = git_output(workspace, &["merge", "--ff-only", &format!("origin/{branch}")])?;
    if !pull.status.success() {
        return Err(format!(
            "快进到 origin/{branch} 失败（可能有本地提交分叉）：{}",
            String::from_utf8_lossy(&pull.stderr).trim()
        ));
    }
    Ok(())
}

/// 增量：文件列表 + commit 摘要 + 有界 diff。
#[derive(Debug, Clone)]
pub struct ChangeDelta {
    pub head: String,
    pub files: Vec<String>,
    pub commits: Vec<String>,
    pub diff_excerpt: String,
    pub diff_truncated: bool,
}

fn collect_delta(workspace: &str, prev: Option<&str>) -> Result<ChangeDelta, String> {
    use crate::git_commands::{git_output, git_stdout};
    let head = git_stdout(workspace, &["rev-parse", "HEAD"])?;
    let range = match prev {
        Some(p) if !p.trim().is_empty() => format!("{}..HEAD", p.trim()),
        _ => "HEAD~1..HEAD".to_string(),
    };

    let mut files = {
        let out = git_output(workspace, &["diff", "--name-only", &range])?;
        if out.status.success() {
            parse_names(&String::from_utf8_lossy(&out.stdout))
        } else {
            let out2 = git_output(workspace, &["diff", "--name-only", "HEAD~1..HEAD"])?;
            if out2.status.success() {
                parse_names(&String::from_utf8_lossy(&out2.stdout))
            } else {
                Vec::new()
            }
        }
    };
    if files.len() > FILE_LIST_CAP {
        files.truncate(FILE_LIST_CAP);
    }

    let mut commits = {
        let out = git_output(
            workspace,
            &["log", "--format=%h %s", &range, "--"],
        )?;
        if out.status.success() {
            parse_names(&String::from_utf8_lossy(&out.stdout))
        } else {
            Vec::new()
        }
    };
    if commits.len() > LOG_LINE_CAP {
        commits.truncate(LOG_LINE_CAP);
    }

    let (diff_excerpt, diff_truncated) = {
        let out = git_output(workspace, &["diff", &range])?;
        if out.status.success() {
            bound_diff(&String::from_utf8_lossy(&out.stdout), DIFF_CHAR_CAP)
        } else {
            (String::new(), false)
        }
    };

    Ok(ChangeDelta {
        head,
        files,
        commits,
        diff_excerpt,
        diff_truncated,
    })
}

fn parse_names(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// 有界 diff：超限截断并标记。
pub fn bound_diff(raw: &str, cap: usize) -> (String, bool) {
    if raw.chars().count() <= cap {
        return (raw.to_string(), false);
    }
    let cut: String = raw.chars().take(cap).collect();
    (format!("{cut}\n…（diff 已截断，共超 {cap} 字符上限）"), true)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeBuckets {
    pub api: bool,
    pub module: bool,
    pub arch: bool,
}

pub fn classify_changes(paths: &[String]) -> ChangeBuckets {
    let mut b = ChangeBuckets::default();
    for p in paths {
        let lower = p.replace('\\', "/").to_ascii_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        if name.contains("controller")
            || name.contains("api")
            || lower.contains("/routes/")
            || lower.contains("openapi")
            || lower.contains("swagger")
        {
            b.api = true;
        }
        if lower.starts_with("src/") || lower.contains("/src/") {
            b.module = true;
        }
        if name == "cargo.toml"
            || name == "package.json"
            || name == "agents.md"
            || name == "readme.md"
            || lower.starts_with("docs/")
            || lower == "agents.md"
        {
            b.arch = true;
        }
    }
    b
}

pub fn strategy_block(buckets: &ChangeBuckets) -> String {
    let mut parts = Vec::new();
    if buckets.api {
        parts.push(
            "【API 变更策略】优先更新接口入参/出参、错误码、鉴权与调用链相关文档；核对路径与方法是否仍一致。",
        );
    }
    if buckets.module {
        parts.push(
            "【模块变更策略】更新模块职责、关键类型/函数、依赖关系；文档结构对齐源码目录。",
        );
    }
    if buckets.arch {
        parts.push(
            "【架构变更策略】更新目录约定、分层、构建/依赖与顶层说明；避免与 AGENTS/README 冲突。",
        );
    }
    if parts.is_empty() {
        parts.push("【通用策略】按文件名/模块名定位对应逆向文档并增量修订。");
    }
    parts.join("\n")
}

/// 拼装 system 提示（纯函数，便于单测）。
pub fn build_docgen_prompt(
    doc_dir: &str,
    branch: &str,
    delta: &ChangeDelta,
) -> String {
    let buckets = classify_changes(&delta.files);
    let strategy = strategy_block(&buckets);
    let file_list = delta
        .files
        .iter()
        .take(FILE_LIST_CAP)
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    let commit_list = if delta.commits.is_empty() {
        "（无提交摘要）".to_string()
    } else {
        delta
            .commits
            .iter()
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let diff_section = if delta.diff_excerpt.trim().is_empty() {
        "（无 diff 摘录）".to_string()
    } else {
        let mark = if delta.diff_truncated {
            "\n（已截断）"
        } else {
            ""
        };
        format!("```\n{}{}\n```", delta.diff_excerpt.trim_end(), mark)
    };
    format!(
        "你是代码逆向文档维护助手。工作区是一个代码仓库，`{doc_dir}/` 下是基于代码的逆向工程文档。\n\
当前在 release 分支「{branch}」。\n\n\
## 提交摘要\n{commit_list}\n\n\
## 变更文件\n{file_list}\n\n\
## 变更 diff 摘录（定向线索；完整内容请用工具读文件）\n{diff_section}\n\n\
## 更新策略\n{strategy}\n\n\
任务：阅读这些变更涉及的代码，更新 `{doc_dir}/` 下**对应**的逆向文档，使其与最新代码一致。\n\
如何找对应文档（按顺序尝试）：\n\
- 先在 `{doc_dir}/` 下 `grep` 变更文件名、所在模块名、关键函数/类名，命中的文档就是要更新的；\n\
- 文件名/模块名找不到时，按业务功能关键词再搜一次；\n\
- 仍找不到且该变更重要时，才在 `{doc_dir}/` 下新建一篇，文件名贴合模块。\n\
硬规则：\n\
1. **只允许修改/新建 `{doc_dir}/` 目录内的文件**；绝对不要改任何业务代码、配置或其它目录（目录外的改动会被自动丢弃并记为违规）。\n\
2. 变更很小 / 与文档无关时可以不改，并在摘要里说明原因。\n\
3. 不要运行命令、不要删除或移动文件，只用读文件 / 搜索 / 写文件 / 改文件。\n\
4. 最后用 3-6 行中文总结「更新了哪些文档、对应哪些变更」，这段会推送到群里。"
    )
}

async fn update_docs_with_agent(
    app: &AppHandle,
    workspace: &str,
    doc_dir: &str,
    branch: &str,
    delta: &ChangeDelta,
) -> Result<String, String> {
    let agent_cfg = {
        let st = app.state::<WecomState>();
        let g = st.orch.agent.lock().await;
        g.clone()
    };
    let Some(cfg) = agent_cfg else {
        return Err("企微通道未绑定模型，无法自动更新文档".into());
    };

    let request_id = format!("wecom-docgen-{}", chrono::Utc::now().timestamp_millis());
    let system = build_docgen_prompt(doc_dir, branch, delta);

    let req = crate::agent::AgentRunRequest {
        config: cfg.config,
        fallback_config: cfg.fallback_config,
        system,
        user: "请根据本次代码变更更新对应的逆向文档，并给出更新摘要。".into(),
        request_id: request_id.clone(),
        repo_path: workspace.to_string(),
        services: cfg.services,
        history: Vec::new(),
        messages: Vec::new(),
        mode: "doc".into(),
        apps: Vec::new(),
        permission: "workspace".into(),
        tools: true,
        vision: false,
        diff_confirm: false,
        always_allow: Vec::new(),
        web_allow: cfg.web_allow,
        web_tier: cfg.web_tier,
        web_search_provider: cfg.web_search_provider,
        web_search_key: cfg.web_search_key,
        subagents: false,
        subagent_write: false,
        mcp: Vec::new(),
        parallel_reads: cfg.parallel_reads,
        retries: cfg.retries,
        grep_ignore: cfg.grep_ignore,
        plan_first: false,
        context_tokens: cfg.context_tokens,
        result_cap: cfg.result_cap,
        anthropic_stream: cfg.anthropic_stream,
        mcp_enabled: false,
    };

    let summary = run_agent_collect(app, request_id, req).await?;

    let reverted = {
        let ws = workspace.to_string();
        let dd = doc_dir.to_string();
        tokio::task::spawn_blocking(move || revert_outside(&ws, &dd))
            .await
            .map_err(|e| e.to_string())?
    };
    let mut summary = summary;
    if !reverted.is_empty() {
        crate::wecom_log::write(
            "docgen",
            &format!("reverted {} out-of-doc changes: {:?}", reverted.len(), reverted),
        );
        summary.push_str(&format!(
            "\n\n⚠️ 已自动丢弃 {} 处超出文档目录的改动（只允许改 {doc_dir}/）。",
            reverted.len()
        ));
    }
    Ok(summary.trim().to_string())
}

async fn quality_check_with_agent(
    app: &AppHandle,
    workspace: &str,
    doc_dir: &str,
    branch: &str,
    changed: &[String],
) -> Result<String, String> {
    let agent_cfg = {
        let st = app.state::<WecomState>();
        let g = st.orch.agent.lock().await;
        g.clone()
    };
    let Some(cfg) = agent_cfg else {
        return Err("企微通道未绑定模型，无法质量自检".into());
    };
    let file_list = changed
        .iter()
        .take(FILE_LIST_CAP)
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    let request_id = format!("wecom-docgen-qc-{}", chrono::Utc::now().timestamp_millis());
    let system = format!(
        "你是逆向文档质量审查员（只读）。工作区分支「{branch}」，文档目录 `{doc_dir}/`。\n\
本次代码变更文件：\n{file_list}\n\n\
任务：对照变更与 `{doc_dir}/` 现有文档，用 3–8 条中文检查结论指出：是否缺更、是否过时、是否矛盾。\n\
硬规则：只读文件/搜索，禁止写文件、改文件、跑命令、删除。"
    );
    let req = crate::agent::AgentRunRequest {
        config: cfg.config,
        fallback_config: cfg.fallback_config,
        system,
        user: "请输出质量自检结论列表。".into(),
        request_id: request_id.clone(),
        repo_path: workspace.to_string(),
        services: cfg.services,
        history: Vec::new(),
        messages: Vec::new(),
        mode: "doc".into(),
        apps: Vec::new(),
        permission: "read".into(),
        tools: true,
        vision: false,
        diff_confirm: false,
        always_allow: Vec::new(),
        web_allow: cfg.web_allow,
        web_tier: cfg.web_tier,
        web_search_provider: cfg.web_search_provider,
        web_search_key: cfg.web_search_key,
        subagents: false,
        subagent_write: false,
        mcp: Vec::new(),
        parallel_reads: cfg.parallel_reads,
        retries: cfg.retries,
        grep_ignore: cfg.grep_ignore,
        plan_first: false,
        context_tokens: cfg.context_tokens,
        result_cap: cfg.result_cap,
        anthropic_stream: cfg.anthropic_stream,
        mcp_enabled: false,
    };
    run_agent_collect(app, request_id, req).await
}

async fn run_agent_collect(
    app: &AppHandle,
    request_id: String,
    req: crate::agent::AgentRunRequest,
) -> Result<String, String> {
    let mut rx = crate::agent::subscribe_agent_bridge(&request_id);
    let cancel_id = request_id.clone();
    let collector = tauri::async_runtime::spawn(async move {
        let mut acc = String::new();
        loop {
            match tokio::time::timeout(UPDATE_TURN_TIMEOUT, rx.recv()).await {
                Ok(Some(crate::agent::AgentBridgeEvent::Chunk(t))) => acc.push_str(&t),
                Ok(Some(crate::agent::AgentBridgeEvent::Done { .. })) | Ok(None) => break,
                Ok(Some(crate::agent::AgentBridgeEvent::Err(e))) => {
                    if acc.is_empty() {
                        acc = format!("（出错：{e}）");
                    }
                    break;
                }
                Ok(Some(_)) => {}
                Err(_) => {
                    acc.push_str("\n（超时，已收尾）");
                    crate::cancellation::cancel_request(&cancel_id);
                    break;
                }
            }
        }
        acc
    });
    let run = crate::agent::agent_run(app.clone(), req).await;
    crate::agent::unsubscribe_agent_bridge(&request_id);
    let summary = collector.await.unwrap_or_default();
    run?;
    Ok(summary.trim().to_string())
}

fn revert_outside(workspace: &str, doc_dir: &str) -> Vec<String> {
    use crate::git_commands::git_output;
    let status = match git_output(
        workspace,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    ) {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&status);
    let mut reverted = Vec::new();
    for entry in text.split('\u{0}').filter(|e| !e.is_empty()) {
        if entry.len() < 3 {
            continue;
        }
        let xy = &entry[..2];
        let path = entry[3..].to_string();
        if !is_outside_doc_dir(&path, doc_dir) {
            continue;
        }
        let untracked = xy == "??";
        if untracked {
            let full = std::path::Path::new(workspace).join(&path);
            let _ = std::fs::remove_file(&full);
        } else {
            let _ = git_output(workspace, &["reset", "-q", "--", &path]);
            let _ = git_output(workspace, &["checkout", "--", &path]);
        }
        reverted.push(path);
    }
    reverted
}

fn is_outside_doc_dir(path: &str, doc_dir: &str) -> bool {
    let path = path.trim();
    if path.is_empty() {
        return false;
    }
    let dir = doc_dir.trim_end_matches('/');
    path != dir && !path.starts_with(&format!("{dir}/"))
}

fn compose_notice(
    branch: &str,
    changed: &[String],
    updated_docs: &str,
    commit_note: &str,
    quality: &str,
    preview: bool,
) -> String {
    let shown: Vec<String> = changed.iter().take(15).map(|f| format!("> {f}")).collect();
    let more = if changed.len() > 15 {
        format!("\n> …等共 {} 个文件", changed.len())
    } else {
        String::new()
    };
    let docs = if updated_docs.trim().is_empty() {
        "（无文档更新摘要）".to_string()
    } else {
        updated_docs.trim().to_string()
    };
    let commit_line = if commit_note.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n**提交：**{}", commit_note.trim())
    };
    let quality_line = if quality.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n**质量自检：**\n{}", quality.trim())
    };
    let title = if preview {
        "## 👀 逆向文档预览（未落盘）"
    } else {
        "## 📘 逆向文档已更新"
    };
    format!(
        "{title}\n\
**分支**：`{branch}`　**变更文件**：{} 个\n\n\
**本次代码变更：**\n{}{}\n\n\
**文档更新摘要：**\n{}{}{}",
        changed.len(),
        shown.join("\n"),
        more,
        docs,
        commit_line,
        quality_line
    )
}

#[derive(Debug, Clone, Copy)]
enum WebhookKind {
    Wecom,
    Feishu,
    DingTalk,
}

/// 对所有已配置 webhook 发通知。返回 `(任一成功, 失败列表)`。
pub async fn notify_all(cfg: &DocgenConfig, markdown: &str) -> (bool, Vec<String>) {
    let mut errors = Vec::new();
    let targets = [
        (WebhookKind::Wecom, cfg.webhook_url.as_str()),
        (WebhookKind::Feishu, cfg.webhook_feishu.as_str()),
        (WebhookKind::DingTalk, cfg.webhook_dingtalk.as_str()),
    ];
    let mut any = false;
    let mut any_ok = false;
    for (kind, url) in targets {
        if url.trim().is_empty() {
            continue;
        }
        any = true;
        match send_webhook_kind(kind, url, markdown).await {
            Ok(()) => any_ok = true,
            Err(e) => errors.push(e),
        }
    }
    if !any {
        errors.push("未配置任何群 webhook".into());
    }
    (any_ok, errors)
}

/// 兼容旧调用：只发企微。
pub async fn send_webhook(url: &str, markdown: &str) -> Result<(), String> {
    send_webhook_kind(WebhookKind::Wecom, url, markdown).await
}

async fn send_webhook_kind(kind: WebhookKind, url: &str, markdown: &str) -> Result<(), String> {
    let url = match kind {
        WebhookKind::Wecom => normalize_webhook_url(url)?,
        WebhookKind::Feishu | WebhookKind::DingTalk => {
            let raw = url.trim();
            if raw.is_empty() {
                return Err("未配置 webhook".into());
            }
            if !(raw.starts_with("http://") || raw.starts_with("https://")) {
                return Err("飞书/钉钉 webhook 需为完整 https URL".into());
            }
            raw.to_string()
        }
    };
    let payload = match kind {
        WebhookKind::Wecom => serde_json::json!({
            "msgtype": "markdown",
            "markdown": { "content": markdown }
        }),
        WebhookKind::Feishu => serde_json::json!({
            "msg_type": "text",
            "content": { "text": markdown }
        }),
        WebhookKind::DingTalk => serde_json::json!({
            "msgtype": "markdown",
            "markdown": {
                "title": "逆向文档更新",
                "text": markdown
            }
        }),
    };
    let label = match kind {
        WebhookKind::Wecom => "企微",
        WebhookKind::Feishu => "飞书",
        WebhookKind::DingTalk => "钉钉",
    };
    let resp = reqwest::Client::new()
        .post(&url)
        .timeout(WEBHOOK_TIMEOUT)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("{label} webhook 发送失败：{e}"))?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("{label} webhook 响应解析失败：{e}"))?;
    match kind {
        WebhookKind::Wecom => match v.get("errcode").and_then(|c| c.as_i64()) {
            Some(0) | None => Ok(()),
            Some(code) => Err(format!(
                "企微 webhook errcode={code} errmsg={}",
                v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("")
            )),
        },
        WebhookKind::Feishu => match v.get("code").and_then(|c| c.as_i64()).or_else(|| {
            v.get("StatusCode")
                .and_then(|c| c.as_i64())
        }) {
            Some(0) | None => Ok(()),
            Some(code) => Err(format!(
                "飞书 webhook code={code} msg={}",
                v.get("msg")
                    .or_else(|| v.get("StatusMessage"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
            )),
        },
        WebhookKind::DingTalk => match v.get("errcode").and_then(|c| c.as_i64()) {
            Some(0) | None => Ok(()),
            Some(code) => Err(format!(
                "钉钉 webhook errcode={code} errmsg={}",
                v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("")
            )),
        },
    }
}

fn normalize_webhook_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("未配置群 webhook".into());
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        Ok(raw.to_string())
    } else {
        Ok(format!(
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key={raw}"
        ))
    }
}

/// 5 段 unix cron（分 时 日 月 周）是否匹配 `now` 的本地时刻。
pub fn cron_matches(expr: &str, now: chrono::DateTime<chrono::Local>) -> Result<bool, String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!("cron 需 5 段，当前 {} 段", parts.len()));
    }
    let minute = now.minute() as u32;
    let hour = now.hour() as u32;
    let day = now.day();
    let month = now.month();
    let weekday = now.weekday().number_from_sunday() % 7; // 0=Sun .. 6=Sat
    Ok(field_matches(parts[0], minute, 0, 59)?
        && field_matches(parts[1], hour, 0, 23)?
        && field_matches(parts[2], day, 1, 31)?
        && field_matches(parts[3], month, 1, 12)?
        && field_matches(parts[4], weekday, 0, 6)?)
}

fn field_matches(field: &str, value: u32, min: u32, max: u32) -> Result<bool, String> {
    if field == "*" {
        return Ok(true);
    }
    for alt in field.split(',') {
        let alt = alt.trim();
        if alt.is_empty() {
            continue;
        }
        if let Some(step_s) = alt.strip_prefix("*/") {
            let step: u32 = step_s
                .parse()
                .map_err(|_| format!("非法 cron 步长：{alt}"))?;
            if step == 0 {
                return Err("cron 步长不能为 0".into());
            }
            if value >= min && (value - min) % step == 0 {
                return Ok(true);
            }
            continue;
        }
        if let Some((a, b)) = alt.split_once('-') {
            let lo: u32 = a.parse().map_err(|_| format!("非法 cron 区间：{alt}"))?;
            let hi: u32 = b.parse().map_err(|_| format!("非法 cron 区间：{alt}"))?;
            if lo < min || hi > max || lo > hi {
                return Err(format!("cron 区间越界：{alt}"));
            }
            if value >= lo && value <= hi {
                return Ok(true);
            }
            continue;
        }
        if let Some((base, step_s)) = alt.split_once('/') {
            let step: u32 = step_s
                .parse()
                .map_err(|_| format!("非法 cron 步长：{alt}"))?;
            if step == 0 {
                return Err("cron 步长不能为 0".into());
            }
            if base == "*" {
                if value >= min && (value - min) % step == 0 {
                    return Ok(true);
                }
            } else if let Some((a, b)) = base.split_once('-') {
                let lo: u32 = a.parse().map_err(|_| format!("非法 cron：{alt}"))?;
                let hi: u32 = b.parse().map_err(|_| format!("非法 cron：{alt}"))?;
                if value >= lo && value <= hi && (value - lo) % step == 0 {
                    return Ok(true);
                }
            } else {
                let start: u32 = base.parse().map_err(|_| format!("非法 cron：{alt}"))?;
                if value >= start && (value - start) % step == 0 {
                    return Ok(true);
                }
            }
            continue;
        }
        let n: u32 = alt.parse().map_err(|_| format!("非法 cron 字段：{alt}"))?;
        if n < min || n > max {
            return Err(format!("cron 值越界：{n}"));
        }
        if n == value {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_url_accepts_key_or_full_url() {
        assert_eq!(
            normalize_webhook_url("abc-123").unwrap(),
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abc-123"
        );
        assert_eq!(
            normalize_webhook_url("https://example.com/hook?key=x").unwrap(),
            "https://example.com/hook?key=x"
        );
        assert!(normalize_webhook_url("  ").is_err());
    }

    #[test]
    fn defaults_fill_in_when_blank() {
        let c = DocgenConfig::default();
        assert_eq!(c.time_or_default(), "23:00");
        assert_eq!(c.branch_or_default(), "release");
        assert_eq!(c.doc_dir_or_default(), "doc");
    }

    #[test]
    fn notice_lists_files_and_summary() {
        let n = compose_notice(
            "release",
            &["a.rs".into(), "b.rs".into()],
            "更新了 a 文档",
            "已提交 1 篇文档（未推送）",
            "",
            false,
        );
        assert!(n.contains("`release`"));
        assert!(n.contains("2 个"));
        assert!(n.contains("> a.rs"));
        assert!(n.contains("更新了 a 文档"));
        assert!(n.contains("已提交 1 篇文档"));
    }

    #[test]
    fn notice_preview_title() {
        let n = compose_notice("release", &["a.rs".into()], "摘要", "", "检查1", true);
        assert!(n.contains("预览"));
        assert!(n.contains("质量自检"));
        assert!(n.contains("检查1"));
    }

    #[test]
    fn parse_names_trims_and_drops_blanks() {
        let v = parse_names("a.rs\n\n  b/c.rs \n");
        assert_eq!(v, vec!["a.rs", "b/c.rs"]);
    }

    #[test]
    fn outside_doc_dir_detection() {
        assert!(is_outside_doc_dir("src/main.rs", "doc"));
        assert!(!is_outside_doc_dir("doc/a.md", "doc"));
        assert!(is_outside_doc_dir("docs/a.md", "doc"));
        assert!(!is_outside_doc_dir("reverse/a.md", "reverse/"));
    }

    #[test]
    fn bound_diff_marks_truncation() {
        let (out, truncated) = bound_diff("abcdefghij", 5);
        assert!(truncated);
        assert!(out.contains("已截断"));
        let (out2, t2) = bound_diff("abc", 10);
        assert!(!t2);
        assert_eq!(out2, "abc");
    }

    #[test]
    fn classify_and_strategy() {
        let b = classify_changes(&[
            "src/foo/UserController.rs".into(),
            "Cargo.toml".into(),
        ]);
        assert!(b.api && b.module && b.arch);
        let s = strategy_block(&b);
        assert!(s.contains("API"));
        assert!(s.contains("模块"));
        assert!(s.contains("架构"));
    }

    #[test]
    fn build_docgen_prompt_includes_log_and_diff() {
        let delta = ChangeDelta {
            head: "abc".into(),
            files: vec!["src/a.rs".into()],
            commits: vec!["deadbeef fix login".into()],
            diff_excerpt: "@@ -1 +1 @@\n-old\n+new".into(),
            diff_truncated: true,
        };
        let p = build_docgen_prompt("doc", "release", &delta);
        assert!(p.contains("提交摘要"));
        assert!(p.contains("deadbeef fix login"));
        assert!(p.contains("变更 diff 摘录"));
        assert!(p.contains("已截断"));
        assert!(p.contains("模块变更策略") || p.contains("通用策略") || p.contains("更新策略"));
    }

    #[test]
    fn cron_matches_basic() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 3, 23, 0, 0)
            .unwrap();
        assert!(cron_matches("0 23 * * *", now).unwrap());
        assert!(!cron_matches("0 22 * * *", now).unwrap());
        assert!(cron_matches("*/1 * * * *", now).unwrap());
        assert!(cron_matches("bad", now).is_err());
    }
}
