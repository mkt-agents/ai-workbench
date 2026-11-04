//! 逆向文档的每日自动更新 + 多通道群通知。
//!
//! 到点 / cron / 本地 HEAD 轮询 / 手动：
//! 1. fetch 最新 release 分支（脏工作区则跳过，不强制 reset）；
//! 2. 取自上次更新以来的提交摘要 + 文件列表 + 有界 diff；
//! 3. 按「更新引擎」跑一轮 AI 更新 `doc/`：天玑走 `agent_run`（编辑档），
//!    DSH 另起一个编辑档进程（跑完即停）；
//! 4. 可选质量自检（只读第二轮）；预览模式不落盘并清仓；
//! 5. 多通道 webhook 通知；历史写入 `docgen-history.jsonl`（30 条）。
//!
//! 游标：`%APPDATA%\com.ai-workbench.app\wecom-dsh\docgen.json`。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::wecom_channel::WecomState;

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(15);
const UPDATE_TURN_TIMEOUT: Duration = Duration::from_secs(600);
const DIFF_CHAR_CAP: usize = 24_000;
const HISTORY_CAP: usize = 30;
const LOG_LINE_CAP: usize = 30;
const FILE_LIST_CAP: usize = 80;

static RUNNING: AtomicBool = AtomicBool::new(false);

/// 是否有更新流程在跑（设置页据此决定要不要顺手关掉临时拉起的 DSH 进程）。
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

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
    /// 轮询各来源仓本地 HEAD，前进则自动跑（不装 git hook）。
    #[serde(default)]
    pub poll_release: bool,
    /// 采集前先同步来源仓（fetch + 仅快进，不 rebase/reset）。默认关。
    #[serde(default)]
    pub sync_sources: bool,
    /// 补跑时变更回溯天数（0=不限制；默认 7，上限 30）。
    #[serde(default = "default_lookback_days")]
    pub lookback_days: u32,
    /// 变更来源子仓白名单；空=仅文档目录所在仓。
    #[serde(default)]
    pub repo_allowlist: Vec<String>,
    /// 更新 system 提示词模板；空=内置默认。
    #[serde(default)]
    pub system_prompt: String,
    /// 质检 system 提示词模板；空=内置默认。
    #[serde(default)]
    pub quality_prompt: String,
    /// 跑更新用哪个引擎：`tianji` | `dsh`（apply 时已把 `inherit` 解析成具体值）。
    #[serde(default)]
    pub engine: String,
}

fn default_true() -> bool {
    true
}

fn default_lookback_days() -> u32 {
    7
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
            sync_sources: false,
            lookback_days: 7,
            repo_allowlist: Vec::new(),
            system_prompt: String::new(),
            quality_prompt: String::new(),
            engine: "tianji".into(),
        }
    }
}

/// 与前端 `DOCGEN_PROMPT_CHAR_CAP` 一致。
pub const PROMPT_CHAR_CAP: usize = 12_000;

/// trim + 字符上限；空串表示走默认模板。
pub fn normalize_prompt_template(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    t.chars().take(PROMPT_CHAR_CAP).collect()
}

pub fn default_docgen_system_prompt() -> &'static str {
    "你是代码逆向文档维护助手。工作区是一个代码仓库，`{doc_dir}/` 下是基于代码的逆向工程文档。\n\
当前在 release 分支「{branch}」。\n\n\
## 提交摘要\n\
{commits}\n\n\
## 变更文件\n\
{files}\n\n\
## 变更 diff 摘录（定向线索；完整内容请用工具读文件）\n\
{diff}\n\n\
任务：阅读这些变更涉及的代码，更新 `{doc_dir}/` 下**对应**的逆向文档，使其与最新代码一致。\n\
如何找对应文档（按顺序尝试）：\n\
- 先在 `{doc_dir}/` 下 grep 变更文件名、所在模块名、关键函数/类名，命中的文档就是要更新的；\n\
- 文件名/模块名找不到时，按业务功能关键词再搜一次；\n\
- 仍找不到且该变更重要时，才在 `{doc_dir}/` 下新建一篇，文件名贴合模块。\n\
硬规则：\n\
1. **只允许修改/新建 `{doc_dir}/` 目录内的文件**；绝对不要改任何业务代码、配置或其它目录（目录外的改动会被自动丢弃并记为违规）。\n\
2. 变更很小 / 与文档无关时可以不改，并在摘要里说明原因。\n\
3. 不要运行命令、不要删除或移动文件，只用读文件 / 搜索 / 写文件 / 改文件。\n\
4. 最后用 3-6 行中文总结「更新了哪些文档、对应哪些变更」，这段会推送到群里。"
}

pub fn default_docgen_quality_prompt() -> &'static str {
    "你是逆向文档质量审查员（只读）。工作区分支「{branch}」，文档目录 `{doc_dir}/`。\n\
上一轮更新摘要：\n\
{summary}\n\n\
本次代码变更文件：\n\
{files}\n\n\
任务：对照变更与 `{doc_dir}/` 现有文档，用 3–8 条中文检查结论指出：是否缺更、是否过时、是否矛盾。\n\
硬规则：只读文件/搜索，禁止写文件、改文件、跑命令、删除。"
}

/// 简单占位符替换：`{key}` → value（按出现顺序，不做嵌套展开）。
pub fn fill_docgen_template(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        let needle = format!("{{{k}}}");
        out = out.replace(&needle, v);
    }
    out
}

impl DocgenConfig {
    /// 本轮是否交给 DeepSeek Harness 跑。
    pub(crate) fn uses_dsh(&self) -> bool {
        self.engine.trim().eq_ignore_ascii_case("dsh")
    }
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
    /// 每日计划已占用的槽位（`YYYY-MM-DD HH:MM`）；修改计划时间后按新槽位重新判断。
    #[serde(default)]
    last_daily_slot: Option<String>,
    /// Cron 已占用的分钟（`YYYY-MM-DD HH:MM`），用于允许一天多次触发并避免同分钟重复。
    #[serde(default)]
    last_cron_minute: Option<String>,
    /// 旧单仓 cursor；迁移到 `last_commits` 后仍可读。
    last_commit: Option<String>,
    /// 各变更来源仓的 HEAD cursor（key=规范化路径）。
    #[serde(default)]
    last_commits: std::collections::HashMap<String, String>,
    /// 轮询防抖：上次已观察过的本地 HEAD。
    #[serde(default)]
    last_poll_head: Option<String>,
}

fn norm_repo_key(path: &str) -> String {
    path.trim().replace('/', "\\").to_ascii_lowercase()
}

fn cursor_for(st: &RunState, repo: &str) -> Option<String> {
    let key = norm_repo_key(repo);
    if let Some(h) = st.last_commits.get(&key) {
        let t = h.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    // 兼容：仅当查询的是「唯一旧 cursor」且 map 为空时退回 last_commit
    if st.last_commits.is_empty() {
        return st
            .last_commit
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    None
}

fn set_cursor(st: &mut RunState, repo: &str, head: &str) {
    let key = norm_repo_key(repo);
    st.last_commits.insert(key, head.trim().to_string());
    st.last_commit = Some(head.trim().to_string());
}

/// 清掉采集点（设置页「重置采集点」）：`None` = 全部，`Some(list)` = 指定仓。
///
/// 同时清掉轮询去重键，否则重置了也不会立刻触发（去重记的是「HEAD + 旧游标」）。
/// 返回实际清掉的仓数。
pub fn reset_cursors(repos: Option<&[String]>) -> usize {
    let mut st = load_state();
    let removed = match repos {
        None => {
            let n = st.last_commits.len();
            st.last_commits.clear();
            n
        }
        Some(list) => {
            let keys: Vec<String> = list.iter().map(|p| norm_repo_key(p)).collect();
            let before = st.last_commits.len();
            st.last_commits.retain(|key, _| !keys.contains(key));
            before - st.last_commits.len()
        }
    };
    // 旧字段只在 last_commits 为空时兜底，一起清掉，别让它把游标“还回来”。
    if st.last_commits.is_empty() {
        st.last_commit = None;
    }
    st.last_poll_head = None;
    save_state(&st);
    removed
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScheduleSlot {
    Daily(String),
    Cron(String),
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
    } else if report.failed() {
        "fail"
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
        // 失败原因优先于跳过原因：这两者不会同时出现
        error: if report.failed() {
            report.error.clone()
        } else {
            report.skipped_reason.clone().unwrap_or_default()
        },
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
    let schedule_due = match schedule_due_today(
        &cfg,
        state.last_daily_slot.as_deref(),
        state.last_cron_minute.as_deref(),
        now,
    ) {
        Ok(v) => v,
        Err(e) => {
            crate::wecom_log::write("docgen", &format!("schedule parse fail-closed: {e}"));
            None
        }
    };
    if let Some(slot) = schedule_due {
        let mut st = load_state();
        match &slot {
            ScheduleSlot::Daily(minute) => st.last_daily_slot = Some(minute.clone()),
            ScheduleSlot::Cron(minute) => st.last_cron_minute = Some(minute.clone()),
        }
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

    // 2) 本地 HEAD 轮询（不挡定点；成功只推进 last_commit）
    if cfg.poll_release {
        if let Err(e) = maybe_poll_release(app, &cfg).await {
            crate::wecom_log::write("docgen", &format!("poll skip/fail: {e}"));
        }
    }
    Ok(())
}

fn schedule_due_today(
    cfg: &DocgenConfig,
    last_daily_slot: Option<&str>,
    last_cron_minute: Option<&str>,
    now: chrono::DateTime<chrono::Local>,
) -> Result<Option<ScheduleSlot>, String> {
    let cron = cfg.cron.trim();
    if !cron.is_empty() {
        let minute = now.format("%Y-%m-%d %H:%M").to_string();
        if last_cron_minute == Some(minute.as_str()) {
            return Ok(None);
        }
        if cron_matches(cron, now)? {
            return Ok(Some(ScheduleSlot::Cron(minute)));
        }
        return Ok(None);
    }
    let time = cfg.time_or_default();
    let Some(day) = crate::daily_schedule::due_today(&time, None, now) else {
        return Ok(None);
    };
    let slot = format!("{day} {time}");
    if last_daily_slot == Some(slot.as_str()) {
        return Ok(None);
    }
    Ok(Some(ScheduleSlot::Daily(slot)))
}

async fn maybe_poll_release(app: &AppHandle, cfg: &DocgenConfig) -> Result<(), String> {
    let workspace = crate::git_commands::resolve_git_workspace(
        cfg.workspace.trim(),
        &cfg.doc_dir_or_default(),
    )?;
    if workspace != cfg.workspace.trim() {
        crate::wecom_log::write(
            "docgen",
            &format!(
                "resolved workspace={} (from {})",
                crate::git_commands::path_for_markdown(&workspace),
                crate::git_commands::path_for_markdown(cfg.workspace.trim())
            ),
        );
    }
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
    // 脏仓只看文档仓，避免服务仓本地改动挡住轮询。
    if !String::from_utf8_lossy(&dirty.stdout).trim().is_empty() {
        return Ok(());
    }

    let sources = resolve_change_sources(&cfg.repo_allowlist, &workspace);
    let st = load_state();
    // 返回「第一个落后的仓」及其 (HEAD, 旧游标)，去重键要带上游标：
    // 只按 HEAD 去重时，改白名单 / 重新勾上某个仓会被上一次的 tip 直接挡掉，等不到新提交。
    let ahead = {
        tokio::task::spawn_blocking(move || -> Result<Option<(String, String, String)>, String> {
            for repo in &sources {
                if crate::git_commands::ensure_git_repo(repo).is_err() {
                    continue;
                }
                let local = crate::git_commands::git_stdout(repo, &["rev-parse", "HEAD"])
                    .unwrap_or_default();
                let local = local.trim().to_string();
                if local.is_empty() {
                    continue;
                }
                let cur = cursor_for(&st, repo);
                if cur.as_deref() != Some(local.as_str()) {
                    return Ok(Some((repo.clone(), local, cur.unwrap_or_default())));
                }
            }
            Ok(None)
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let Some((ahead_repo, local_tip, ahead_cursor)) = ahead? else {
        return Ok(());
    };

    let poll_key = format!("{local_tip}|{ahead_cursor}");
    let mut st = load_state();
    if st.last_poll_head.as_deref() == Some(poll_key.as_str()) {
        // 同一 tip + 同一游标已触发过一轮仍没推进（例如那次跑失败）时，
        // 等游标或 tip 再变，别每分钟重试。
        return Ok(());
    }
    st.last_poll_head = Some(poll_key);
    save_state(&st);

    crate::wecom_log::write(
        "docgen",
        &format!(
            "auto poll start repo={} local_head={} cursor={}",
            repo_short_name(&ahead_repo),
            &local_tip[..8.min(local_tip.len())],
            if ahead_cursor.is_empty() {
                "-".to_string()
            } else {
                ahead_cursor[..8.min(ahead_cursor.len())].to_string()
            }
        ),
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
    /// 更新 / 质检步骤的失败原因（空 = 成功）。有值时：算失败、不提交、不推进游标。
    pub error: String,
}

impl UpdateReport {
    pub fn failed(&self) -> bool {
        !self.error.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocgenProgressEvent {
    run_id: String,
    trigger: String,
    phase: String,
    percent: u8,
    status: String,
    detail: String,
}

/// 最近一次进度事件。页面切走再回来（组件重新挂载）时用它把进度卡恢复出来，
/// 否则只能显示一张空壳。
static LAST_PROGRESS: std::sync::Mutex<Option<DocgenProgressEvent>> = std::sync::Mutex::new(None);

/// 读最近一次进度快照；从没跑过时是 None。
pub fn last_progress() -> Option<DocgenProgressEvent> {
    LAST_PROGRESS.lock().ok().and_then(|slot| slot.clone())
}

/// 进度卡恢复用：最近一次事件 + 「现在是否真的在跑」。
/// 页面卸载期间的收尾事件收不到，光看事件状态会把按钮永久锁成「运行中」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocgenProgressSnapshot {
    pub progress: Option<DocgenProgressEvent>,
    pub running: bool,
}

pub fn progress_snapshot() -> DocgenProgressSnapshot {
    DocgenProgressSnapshot {
        progress: last_progress(),
        running: is_running(),
    }
}

struct RunProgress<'a> {
    app: &'a AppHandle,
    run_id: String,
    trigger: RunTrigger,
}

impl<'a> RunProgress<'a> {
    fn new(app: &'a AppHandle, trigger: RunTrigger) -> Self {
        Self {
            app,
            run_id: format!("docgen-{}", chrono::Utc::now().timestamp_millis()),
            trigger,
        }
    }

    fn emit(&self, phase: &str, percent: u8, status: &str, detail: &str) {
        let event = DocgenProgressEvent {
            run_id: self.run_id.clone(),
            trigger: self.trigger.as_str().into(),
            phase: phase.into(),
            percent: percent.min(100),
            status: status.into(),
            detail: detail.trim().to_string(),
        };
        if let Ok(mut slot) = LAST_PROGRESS.lock() {
            *slot = Some(event.clone());
        }
        let _ = self.app.emit("wecom-docgen-progress", event);
    }

    fn stage(&self, phase: &str, percent: u8) {
        self.emit(phase, percent, "running", "");
    }

    fn finish_ok(&self, report: &UpdateReport) {
        if let Some(reason) = report.skipped_reason.as_deref() {
            self.emit("skipped", 100, "skip", reason);
        } else {
            self.emit("done", 100, "ok", "");
        }
    }

    fn finish_err(&self, error: &str) {
        self.emit("fail", 100, "fail", error);
    }
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
    let progress = RunProgress::new(app, trigger);
    let result = run_update_inner(app, cfg, trigger, &progress).await;
    match result {
        // 流程走完但 AI 步骤失败：进度要显示失败，不能亮绿灯
        Ok(report) if report.failed() => {
            progress.finish_err(&report.error);
            Ok(report)
        }
        Ok(report) => {
            progress.finish_ok(&report);
            Ok(report)
        }
        Err(e) => {
            progress.finish_err(&e);
            Err(e)
        }
    }
}

async fn run_update_inner(
    app: &AppHandle,
    cfg: &DocgenConfig,
    trigger: RunTrigger,
    progress: &RunProgress<'_>,
) -> Result<UpdateReport, String> {
    progress.stage("workspace", 5);
    let doc_dir = cfg.doc_dir_or_default();
    let workspace = crate::git_commands::resolve_git_workspace(cfg.workspace.trim(), &doc_dir)?;
    if workspace != cfg.workspace.trim() {
        crate::wecom_log::write(
            "docgen",
            &format!(
                "resolved workspace={} (from {})",
                crate::git_commands::path_for_markdown(&workspace),
                crate::git_commands::path_for_markdown(cfg.workspace.trim())
            ),
        );
    }
    let branch = cfg.branch_or_default();
    let preview = cfg.preview_mode || trigger == RunTrigger::Preview;

    crate::git_commands::ensure_git_repo(&workspace)?;
    progress.stage("status", 12);

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
        progress.stage("notify", 90);
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
            error: String::new(),
        };
        append_history(&history_from_report(trigger, &report, "", &report.webhook_errors));
        return Ok(report);
    }

    progress.stage("branch", 20);
    {
        let ws = workspace.clone();
        let br = branch.clone();
        tokio::task::spawn_blocking(move || update_release_branch(&ws, &br))
            .await
            .map_err(|e| e.to_string())??;
    }

    // 可选的「先同步来源仓」：只 fetch + 快进，绝不 rebase/reset/强推。
    let mut sync_note = String::new();
    if cfg.sync_sources {
        progress.stage("sync", 28);
        let repos = resolve_change_sources(&cfg.repo_allowlist, &workspace);
        let ws = workspace.clone();
        let br = branch.clone();
        let notes = tokio::task::spawn_blocking(move || sync_source_repos(&ws, &repos, &br))
            .await
            .map_err(|e| e.to_string())?;
        if !notes.is_empty() {
            crate::wecom_log::write("docgen", &format!("sync sources: {}", notes.join("；")));
            sync_note = notes.join("；");
        }
    }

    progress.stage("collect", 35);
    let sources = resolve_change_sources(&cfg.repo_allowlist, &workspace);
    let lookback_days = cfg.lookback_days.min(30);
    let st_snap = load_state();
    let source_prev: Vec<(String, Option<String>)> = sources
        .iter()
        .map(|s| (s.clone(), cursor_for(&st_snap, s)))
        .collect();
    let parts = {
        tokio::task::spawn_blocking(move || -> Result<Vec<(String, ChangeDelta)>, String> {
            let mut out = Vec::new();
            for (repo, prev) in source_prev {
                crate::git_commands::ensure_git_repo(&repo)?;
                // 文档仓已在上面 ff；其它来源仓一律读取本地 HEAD，不替用户切分支或拉远端。
                let d = collect_delta(&repo, prev.as_deref(), lookback_days)?;
                out.push((repo, d));
            }
            Ok(out)
        })
        .await
        .map_err(|e| e.to_string())??
    };
    let heads: Vec<(String, String)> = parts
        .iter()
        .map(|(r, d)| (r.clone(), d.head.clone()))
        .collect();
    let delta = merge_change_deltas(&parts, DIFF_CHAR_CAP);
    crate::wecom_log::write(
        "docgen",
        &format!(
            "collect_delta sources={} lookback={lookback_days}d files={} commits={}",
            sources.len(),
            delta.files.len(),
            delta.commits.len()
        ),
    );

    if delta.files.is_empty() {
        let mut st = load_state();
        for (repo, head) in &heads {
            set_cursor(&mut st, repo, head);
        }
        save_state(&st);
        let report = UpdateReport {
            branch,
            new_commits: 0,
            changed_files: Vec::new(),
            updated_docs: String::new(),
            commit_note: String::new(),
            pushed: false,
            skipped_reason: Some(format!(
                "自上次更新以来无代码变更（本次监视 {} 个仓；未勾选的子仓不会被采集）",
                sources.len()
            )),
            preview,
            quality: String::new(),
            webhook_errors: Vec::new(),
            error: String::new(),
        };
        append_history(&history_from_report(trigger, &report, "", &[]));
        return Ok(report);
    }

    progress.stage("agent", 55);
    // 失败要留痕：这一轮算失败（不提交、不推进游标），下一轮还能重跑同一批变更。
    let mut step_error = String::new();
    let updated_docs = match update_docs_with_agent(
        app,
        &workspace,
        &doc_dir,
        &branch,
        &delta,
        &cfg.system_prompt,
        &cfg.engine,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            step_error = format!("文档更新失败：{e}");
            format!("（{step_error}）")
        }
    };

    let quality = if cfg.quality_check {
        progress.stage("quality", 72);
        match quality_check_with_agent(
            app,
            &workspace,
            &doc_dir,
            &branch,
            &delta.files,
            &updated_docs,
            &cfg.quality_prompt,
            &cfg.engine,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                if step_error.is_empty() {
                    step_error = format!("质量自检失败：{e}");
                }
                format!("（质量自检失败：{e}）")
            }
        }
    } else {
        String::new()
    };
    let failed = !step_error.is_empty();

    progress.stage(
        if preview {
            "preview"
        } else if cfg.auto_commit {
            "commit"
        } else {
            "publish"
        },
        82,
    );
    let mut commit_note = String::new();
    if preview {
        let ws = workspace.clone();
        let dd = doc_dir.clone();
        let _ = tokio::task::spawn_blocking(move || revert_doc_dir(&ws, &dd))
            .await
            .map_err(|e| e.to_string())?;
        commit_note = "预览模式：文档改动已丢弃，未提交".into();
    } else if failed {
        // 更新没成功就别把半成品提交进去（下一轮还要重跑这批变更）。
        commit_note = "（本次更新失败，未提交）".into();
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
        &sync_note,
    );
    progress.stage("notify", 92);
    let (pushed, webhook_errors) = notify_all(cfg, &body).await;

    progress.stage("cursor", 98);
    // 失败时不推进游标：否则这批变更被永久跳过，下一轮也补不回来。
    if !failed {
        let mut st = load_state();
        for (repo, head) in &heads {
            set_cursor(&mut st, repo, head);
        }
        save_state(&st);
    } else {
        crate::wecom_log::write(
            "docgen",
            &format!("保留游标不推进（本轮失败）：{}", step_error),
        );
    }

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
        error: step_error,
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

/// 采集前同步来源仓：`fetch` + **仅快进**。
///
/// 保守到底：脏工作区、本地领先/分叉、fetch 失败、快进失败 —— 一律跳过并记一句，
/// 不 rebase、不 reset、不强推，用户的本地提交原样保留。
fn sync_source_repos(workspace: &str, repos: &[String], branch: &str) -> Vec<String> {
    use crate::git_commands::{ensure_git_repo, git_output, git_stdout};
    let doc_key = norm_repo_key(workspace);
    let track = format!("origin/{branch}");
    let mut notes = Vec::new();
    for repo in repos {
        // 文档仓上面已经 fetch + ff 过了，别重复
        if norm_repo_key(repo) == doc_key {
            continue;
        }
        let name = repo_short_name(repo);
        if ensure_git_repo(repo).is_err() {
            notes.push(format!("{name}：不是 Git 仓库，跳过"));
            continue;
        }
        let dirty = git_output(repo, &["status", "--porcelain"])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);
        if dirty {
            notes.push(format!("{name}：有未提交改动，跳过同步"));
            continue;
        }
        match git_output(repo, &["fetch", "origin", branch]) {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                notes.push(format!(
                    "{name}：fetch 失败（{}）",
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
                continue;
            }
            Err(e) => {
                notes.push(format!("{name}：fetch 失败（{e}）"));
                continue;
            }
        }
        let local = git_stdout(repo, &["rev-parse", "HEAD"]).unwrap_or_default();
        let local = local.trim().to_string();
        let remote = git_stdout(repo, &["rev-parse", "--verify", "-q", &track]).unwrap_or_default();
        let remote = remote.trim().to_string();
        if local.is_empty() || remote.is_empty() {
            notes.push(format!("{name}：缺少本地 HEAD 或 {track}"));
            continue;
        }
        if local == remote {
            continue;
        }
        let fast_forwardable = git_output(repo, &["merge-base", "--is-ancestor", &local, &remote])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !fast_forwardable {
            notes.push(format!("{name}：本地领先或已分叉，跳过（不自动 rebase）"));
            continue;
        }
        match git_output(repo, &["merge", "--ff-only", &track]) {
            Ok(o) if o.status.success() => {
                let count = git_stdout(repo, &["rev-list", "--count", &format!("{local}..{remote}")])
                    .unwrap_or_default();
                notes.push(format!("{name}：快进 {} 个提交", count.trim()));
            }
            Ok(o) => notes.push(format!(
                "{name}：快进失败（{}）",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => notes.push(format!("{name}：快进失败（{e}）")),
        }
    }
    notes
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

/// 变更来源列表：白名单非空用白名单，否则仅文档仓。
pub(crate) fn resolve_change_sources(allowlist: &[String], doc_repo: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let push = |out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>, p: &str| {
        let t = p.trim();
        if t.is_empty() {
            return;
        }
        if !seen.insert(norm_repo_key(t)) {
            return;
        }
        out.push(t.to_string());
    };
    if allowlist.iter().any(|s| !s.trim().is_empty()) {
        for s in allowlist {
            push(&mut out, &mut seen, s);
        }
    } else {
        push(&mut out, &mut seen, doc_repo);
    }
    out
}

fn repo_short_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// 合并多仓 delta；files/commits 加 `仓名:` 前缀；diff 按仓拼接并截断。
pub(crate) fn merge_change_deltas(
    parts: &[(String, ChangeDelta)],
    diff_cap: usize,
) -> ChangeDelta {
    let mut files = Vec::new();
    let mut commits = Vec::new();
    let mut diff = String::new();
    let mut truncated = false;
    // 用「最后一仓」的 head 仅作占位；多仓 cursor 另存。
    let mut head = String::new();
    for (repo, d) in parts {
        let label = repo_short_name(repo);
        head = d.head.clone();
        for f in &d.files {
            files.push(format!("{label}:{f}"));
        }
        for c in &d.commits {
            commits.push(format!("{label}:{c}"));
        }
        if !d.diff_excerpt.trim().is_empty() {
            let block = format!("### {label}\n{}\n", d.diff_excerpt.trim_end());
            if diff.chars().count() + block.chars().count() > diff_cap {
                truncated = true;
                let remain = diff_cap.saturating_sub(diff.chars().count());
                if remain > 0 {
                    diff.push_str(&block.chars().take(remain).collect::<String>());
                }
                break;
            }
            diff.push_str(&block);
            truncated |= d.diff_truncated;
        } else {
            truncated |= d.diff_truncated;
        }
    }
    ChangeDelta {
        head,
        files,
        commits,
        diff_excerpt: diff,
        diff_truncated: truncated,
    }
}

/// 纯函数：在已知边界 B 与「B 是否为 prev 祖先」时选出有效 prev。
/// - lookback=0 或无边界 → 保留 prev
/// - prev 在窗口内（B 是 prev 祖先）→ 保留 prev
/// - 否则用边界 B（截断超长缺口）
pub(crate) fn pick_clamped_prev(
    prev: Option<&str>,
    boundary: Option<&str>,
    boundary_is_ancestor_of_prev: bool,
) -> Option<String> {
    let prev = prev.map(str::trim).filter(|p| !p.is_empty());
    let boundary = boundary.map(str::trim).filter(|b| !b.is_empty());
    match (prev, boundary) {
        (p, None) => p.map(str::to_string),
        (Some(p), Some(b)) if boundary_is_ancestor_of_prev && p != b => Some(p.to_string()),
        (_, Some(b)) => Some(b.to_string()),
    }
}

/// 按回溯天数收紧 `prev..HEAD` 的起点。
pub(crate) fn clamp_delta_prev(
    workspace: &str,
    prev: Option<&str>,
    lookback_days: u32,
) -> Result<Option<String>, String> {
    use crate::git_commands::{git_output, git_stdout};
    if lookback_days == 0 {
        return Ok(prev.map(str::trim).filter(|p| !p.is_empty()).map(str::to_string));
    }
    let before = format!("{lookback_days} days ago");
    let boundary = git_stdout(
        workspace,
        &["log", "-1", "--before", &before, "--format=%H", "HEAD"],
    )
    .unwrap_or_default();
    let boundary = boundary.trim();
    if boundary.is_empty() {
        return Ok(prev.map(str::trim).filter(|p| !p.is_empty()).map(str::to_string));
    }
    let prev_clean = prev.map(str::trim).filter(|p| !p.is_empty());
    let ancestor = match prev_clean {
        Some(p) => {
            let out = git_output(workspace, &["merge-base", "--is-ancestor", boundary, p])?;
            out.status.success()
        }
        None => false,
    };
    let chosen = pick_clamped_prev(prev, Some(boundary), ancestor);
    // 新仓（还没有游标）恰好落在「窗口内没有提交」这一档：HEAD 本身就是 N 天前那条，
    // 截出来的区间是空的 —— 仓刚加进来却采不到任何东西，游标还会被直接推到 HEAD。
    // 退一步用最近一次提交，保证新增的仓至少能采到一次改动。
    if prev_clean.is_none() && chosen.as_deref() == Some(boundary) {
        let head = git_stdout(workspace, &["rev-parse", "HEAD"]).unwrap_or_default();
        if head.trim() == boundary {
            let parent = git_stdout(workspace, &["rev-parse", "--verify", "-q", "HEAD~1"])
                .unwrap_or_default();
            let parent = parent.trim();
            if !parent.is_empty() {
                return Ok(Some(parent.to_string()));
            }
        }
    }
    Ok(chosen)
}

fn collect_delta(
    workspace: &str,
    prev: Option<&str>,
    lookback_days: u32,
) -> Result<ChangeDelta, String> {
    use crate::git_commands::{git_output, git_stdout};
    let head = git_stdout(workspace, &["rev-parse", "HEAD"])?;
    let prev = clamp_delta_prev(workspace, prev, lookback_days)?;
    let range = match prev.as_deref() {
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

fn format_file_list(files: &[String]) -> String {
    if files.is_empty() {
        "（无变更文件）".into()
    } else {
        files
            .iter()
            .take(FILE_LIST_CAP)
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn format_commit_list(commits: &[String]) -> String {
    if commits.is_empty() {
        "（无提交摘要）".into()
    } else {
        commits
            .iter()
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn format_diff_section(delta: &ChangeDelta) -> String {
    if delta.diff_excerpt.trim().is_empty() {
        "（无 diff 摘录）".into()
    } else {
        let mark = if delta.diff_truncated {
            "\n（已截断）"
        } else {
            ""
        };
        format!("```\n{}{}\n```", delta.diff_excerpt.trim_end(), mark)
    }
}

/// 拼装更新 system 提示：配置模板非空则用之，否则内置默认；再填占位符。
pub fn build_docgen_prompt(
    template: &str,
    doc_dir: &str,
    branch: &str,
    delta: &ChangeDelta,
) -> String {
    let tpl = normalize_prompt_template(template);
    let body = if tpl.is_empty() {
        default_docgen_system_prompt().to_string()
    } else {
        tpl
    };
    let commits = format_commit_list(&delta.commits);
    let files = format_file_list(&delta.files);
    let diff = format_diff_section(delta);
    fill_docgen_template(
        &body,
        &[
            ("doc_dir", doc_dir),
            ("branch", branch),
            ("commits", &commits),
            ("files", &files),
            ("diff", &diff),
        ],
    )
}

/// 拼装质检 system 提示。
pub fn build_quality_prompt(
    template: &str,
    doc_dir: &str,
    branch: &str,
    files: &[String],
    summary: &str,
) -> String {
    let tpl = normalize_prompt_template(template);
    let body = if tpl.is_empty() {
        default_docgen_quality_prompt().to_string()
    } else {
        tpl
    };
    let file_list = format_file_list(files);
    let summary_text = {
        let t = summary.trim();
        if t.is_empty() {
            "（无）"
        } else {
            t
        }
    };
    fill_docgen_template(
        &body,
        &[
            ("doc_dir", doc_dir),
            ("branch", branch),
            ("files", &file_list),
            ("summary", summary_text),
        ],
    )
}

async fn update_docs_with_agent(
    app: &AppHandle,
    workspace: &str,
    doc_dir: &str,
    branch: &str,
    delta: &ChangeDelta,
    system_template: &str,
    engine: &str,
) -> Result<String, String> {
    let system = build_docgen_prompt(system_template, doc_dir, branch, delta);
    let user = "请根据本次代码变更更新对应的逆向文档，并给出更新摘要。";

    let summary = if engine.trim().eq_ignore_ascii_case("dsh") {
        dsh_turn(app, workspace, DOCGEN_SESSION, &system, user).await?
    } else {
        let agent_cfg = {
            let st = app.state::<WecomState>();
            let g = st.orch.agent.lock().await;
            g.clone()
        };
        let Some(cfg) = agent_cfg else {
            return Err("企微通道未绑定模型，无法自动更新文档".into());
        };

        let request_id = format!("wecom-docgen-{}", chrono::Utc::now().timestamp_millis());

        let req = crate::agent::AgentRunRequest {
            config: cfg.config,
            fallback_config: cfg.fallback_config,
            system,
            user: user.into(),
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

        run_agent_collect(app, request_id, req).await?
    };

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
    summary: &str,
    quality_template: &str,
    engine: &str,
) -> Result<String, String> {
    let user = "请输出质量自检结论列表。";
    let system = build_quality_prompt(quality_template, doc_dir, branch, changed, summary);

    if engine.trim().eq_ignore_ascii_case("dsh") {
        // DSH 一套进程只有一个档位（编辑档，要写文档），没有「只读第二轮」；
        // 这一轮靠提示词钉死只读语义，改动仍会被 revert_outside 兜住文档目录以外。
        let readonly = format!(
            "{system}\n\n# 本轮约束\n这是只读自检：不要修改、创建或删除任何文件，也不要执行会改变工作区的命令；只输出结论列表。"
        );
        let text = dsh_turn(app, workspace, DOCGEN_QC_SESSION, &readonly, user).await?;
        let ws = workspace.to_string();
        let dd = doc_dir.to_string();
        let reverted = tokio::task::spawn_blocking(move || revert_outside(&ws, &dd))
            .await
            .map_err(|e| e.to_string())?;
        if !reverted.is_empty() {
            crate::wecom_log::write(
                "docgen",
                &format!(
                    "质检轮丢弃 {} 处超出文档目录的改动：{:?}",
                    reverted.len(),
                    reverted
                ),
            );
        }
        return Ok(text);
    }

    let agent_cfg = {
        let st = app.state::<WecomState>();
        let g = st.orch.agent.lock().await;
        g.clone()
    };
    let Some(cfg) = agent_cfg else {
        return Err("企微通道未绑定模型，无法质量自检".into());
    };
    let request_id = format!("wecom-docgen-qc-{}", chrono::Utc::now().timestamp_millis());
    let req = crate::agent::AgentRunRequest {
        config: cfg.config,
        fallback_config: cfg.fallback_config,
        system,
        user: user.into(),
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

/// DSH 档的会话键：每轮开始都重置，别让上下文一直滚。
const DOCGEN_SESSION: &str = "docgen";
const DOCGEN_QC_SESSION: &str = "docgen-qc";

/// 跑一轮 DSH 回合并把正文收成字符串，跑完就关进程（不常驻）。
///
/// 这是无人值守任务：审批按「允许一次」自动应答并写进通道日志（沙箱是编辑档 +
/// 工作区范围，文档目录以外的改动仍会被 `revert_outside` 丢弃）。
async fn dsh_turn(
    app: &AppHandle,
    workspace: &str,
    session_key: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let dsh = {
        let st = app.state::<WecomState>();
        std::sync::Arc::clone(&st.docgen_dsh)
    };
    // 工作区用这一轮解析出来的仓库路径（设置页填的可能是父目录）。
    dsh.configure_docgen(workspace.to_string()).await;
    let result = dsh_turn_inner(&dsh, session_key, system, user).await;
    dsh.stop().await;
    result
}

async fn dsh_turn_inner(
    dsh: &crate::wecom_dsh::DshEngine,
    session_key: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    dsh.reset_channel_session(session_key).await;
    dsh.ensure_ready().await?;
    let session = dsh.acquire_session(session_key).await?;
    if let Err(e) = dsh.apply_selection(&session).await {
        // 选模型失败不是致命问题（会按 provider 默认继续），但要留在日志里。
        crate::wecom_log::write(
            "docgen",
            &format!("dsh 下发模型/思考强度失败，按默认继续：{e}"),
        );
    }
    let prompt = format!("{system}\n\n---\n\n{user}");
    let mut turn = dsh
        .prompt(&session, vec![crate::wecom_dsh::acp_text_block(&prompt)])
        .await?;

    let mut acc = String::new();
    let mut approvals = 0usize;
    // 整轮共享一个期限（不是每个事件各 600 秒），和天玑档的收流语义对齐。
    let deadline = tokio::time::Instant::now() + UPDATE_TURN_TIMEOUT;
    loop {
        let event = match tokio::time::timeout_at(deadline, turn.next()).await {
            Ok(ev) => ev,
            Err(_) => {
                turn.cancel();
                crate::wecom_log::write("docgen", "dsh 回合超过 600 秒，已取消");
                acc.push_str("\n（超时，已收尾）");
                break;
            }
        };
        match event {
            Some(crate::wecom_dsh::DshEvent::Update(update)) => {
                let kind = update
                    .get("sessionUpdate")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match kind {
                    "agent_message_chunk" => {
                        if let Some(text) = update
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(|v| v.as_str())
                        {
                            acc.push_str(text);
                        }
                    }
                    "tool_call" => {
                        if let Some(title) = update
                            .get("title")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            crate::wecom_log::write("docgen", &format!("dsh 工具：{title}"));
                        }
                    }
                    _ => {}
                }
            }
            Some(crate::wecom_dsh::DshEvent::Permission { rpc_id, params }) => {
                let (allow, reject) = crate::wecom_dsh::permission_options(&params);
                let summary = crate::wecom_dsh::permission_summary(&params);
                approvals += 1;
                crate::wecom_log::write(
                    "docgen",
                    &format!(
                        "dsh 审批 #{approvals}：{summary} → {}",
                        if allow.is_some() { "自动允许一次" } else { "无允许项，拒绝" }
                    ),
                );
                if let Some(id) = allow.as_deref() {
                    turn.answer_permission(rpc_id, Some(id));
                } else {
                    turn.answer_permission(rpc_id, reject.as_deref());
                }
            }
            Some(crate::wecom_dsh::DshEvent::Done(Err(e))) => {
                return Err(crate::wecom_dsh::friendly_turn_error(&e));
            }
            Some(crate::wecom_dsh::DshEvent::Done(Ok(_))) | None => break,
        }
    }
    if approvals > 0 {
        crate::wecom_log::write(
            "docgen",
            &format!("dsh 本轮自动应答 {approvals} 次审批（均为允许一次）"),
        );
    }
    Ok(acc.trim().to_string())
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
    sync_note: &str,
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
    let sync_line = if sync_note.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n**来源仓同步：**{}", sync_note.trim())
    };
    format!(
        "{title}\n\
**分支**：`{branch}`　**变更文件**：{} 个\n\n\
**本次代码变更：**\n{}{}\n\n\
**文档更新摘要：**\n{}{}{}{}",
        changed.len(),
        shown.join("\n"),
        more,
        docs,
        commit_line,
        quality_line,
        sync_line
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
    // 企微 markdown 会把 `\` 当转义吃掉（D:\a\b → D:ab）；统一换成正斜杠。
    let markdown = crate::git_commands::path_for_markdown(markdown);
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
        match send_webhook_kind(kind, url, &markdown).await {
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
        assert_eq!(c.lookback_days, 7);
    }

    #[test]
    fn pick_clamped_prev_keeps_prev_when_no_boundary() {
        assert_eq!(
            pick_clamped_prev(Some("aaa"), None, false).as_deref(),
            Some("aaa")
        );
        assert_eq!(pick_clamped_prev(None, None, false), None);
    }

    #[test]
    fn pick_clamped_prev_keeps_newer_prev_inside_window() {
        // B 是 prev 祖先 → prev 在窗口内，保留 prev
        assert_eq!(
            pick_clamped_prev(Some("prev111"), Some("bound000"), true).as_deref(),
            Some("prev111")
        );
    }

    #[test]
    fn pick_clamped_prev_uses_boundary_when_gap_exceeds_window() {
        // prev 不在窗口内（B 不是其祖先）→ 截到 B
        assert_eq!(
            pick_clamped_prev(Some("old999"), Some("bound000"), false).as_deref(),
            Some("bound000")
        );
        assert_eq!(
            pick_clamped_prev(None, Some("bound000"), false).as_deref(),
            Some("bound000")
        );
    }

    #[test]
    fn resolve_change_sources_falls_back_to_doc_repo_when_allowlist_empty() {
        let s = resolve_change_sources(&[], r"D:\doc\repo");
        assert_eq!(s, vec![r"D:\doc\repo".to_string()]);
        let s2 = resolve_change_sources(&["".into(), "  ".into()], r"D:\doc\repo");
        assert_eq!(s2, vec![r"D:\doc\repo".to_string()]);
    }

    #[test]
    fn resolve_change_sources_uses_allowlist_and_dedupes() {
        let s = resolve_change_sources(
            &[
                r"D:\release\a".into(),
                r"D:/release/a".into(),
                r"D:\release\b".into(),
            ],
            r"D:\doc\repo",
        );
        assert_eq!(s.len(), 2);
        assert!(s.iter().any(|p| p.ends_with('a') || p.ends_with("a")));
        assert!(s.iter().any(|p| p.contains('b') || p.ends_with("b")));
    }

    #[test]
    fn cursor_for_falls_back_to_legacy_last_commit_when_map_empty() {
        let st = RunState {
            last_commit: Some("abc123".into()),
            last_commits: std::collections::HashMap::new(),
            ..Default::default()
        };
        assert_eq!(
            cursor_for(&st, r"D:\release\svc").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn cursor_for_prefers_per_repo_map_over_legacy() {
        let mut map = std::collections::HashMap::new();
        map.insert(norm_repo_key(r"D:\release\a"), "head-a".into());
        map.insert(norm_repo_key(r"D:\release\b"), "head-b".into());
        let st = RunState {
            last_commit: Some("legacy".into()),
            last_commits: map,
            ..Default::default()
        };
        assert_eq!(cursor_for(&st, r"D:\release\a").as_deref(), Some("head-a"));
        assert_eq!(cursor_for(&st, r"D:\release\b").as_deref(), Some("head-b"));
        assert_eq!(cursor_for(&st, r"D:\release\c"), None);
    }

    #[test]
    fn merge_change_deltas_prefixes_repo_names() {
        let a = ChangeDelta {
            head: "aaa".into(),
            files: vec!["src/A.java".into()],
            commits: vec!["aaaa feat a".into()],
            diff_excerpt: "diff a".into(),
            diff_truncated: false,
        };
        let b = ChangeDelta {
            head: "bbb".into(),
            files: vec!["src/B.java".into()],
            commits: vec!["bbbb feat b".into()],
            diff_excerpt: "diff b".into(),
            diff_truncated: false,
        };
        let m = merge_change_deltas(
            &[
                (r"D:\x\match.service".into(), a),
                (r"D:\x\quote.service".into(), b),
            ],
            24_000,
        );
        assert!(m.files.iter().any(|f| f.starts_with("match.service:")));
        assert!(m.files.iter().any(|f| f.starts_with("quote.service:")));
        assert!(m.commits.iter().any(|c| c.contains("match.service:")));
        assert!(m.diff_excerpt.contains("### match.service"));
        assert!(m.diff_excerpt.contains("### quote.service"));
        assert_eq!(m.head, "bbb");
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
            "account.sdk：快进 2 个提交",
        );
        assert!(n.contains("`release`"));
        assert!(n.contains("2 个"));
        assert!(n.contains("> a.rs"));
        assert!(n.contains("更新了 a 文档"));
        assert!(n.contains("已提交 1 篇文档"));
    }

    #[test]
    fn notice_preview_title() {
        let n = compose_notice("release", &["a.rs".into()], "摘要", "", "检查1", true, "");
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
    fn normalize_prompt_template_trims_and_caps() {
        assert_eq!(normalize_prompt_template("  "), "");
        assert_eq!(normalize_prompt_template("  hi  "), "hi");
        let long: String = "字".repeat(PROMPT_CHAR_CAP + 50);
        let n = normalize_prompt_template(&long);
        assert_eq!(n.chars().count(), PROMPT_CHAR_CAP);
    }

    #[test]
    fn fill_docgen_template_replaces_placeholders() {
        let out = fill_docgen_template(
            "dir={doc_dir} br={branch} x={files}",
            &[("doc_dir", "doc"), ("branch", "release"), ("files", "- a.rs")],
        );
        assert_eq!(out, "dir=doc br=release x=- a.rs");
    }

    #[test]
    fn build_docgen_prompt_empty_template_uses_default_without_strategy() {
        let delta = ChangeDelta {
            head: "abc".into(),
            files: vec!["src/a.rs".into()],
            commits: vec!["deadbeef fix login".into()],
            diff_excerpt: "@@ -1 +1 @@\n-old\n+new".into(),
            diff_truncated: true,
        };
        let p = build_docgen_prompt("", "doc", "release", &delta);
        assert!(p.contains("提交摘要"));
        assert!(p.contains("deadbeef fix login"));
        assert!(p.contains("变更 diff 摘录") || p.contains("diff"));
        assert!(p.contains("已截断"));
        assert!(p.contains("`doc/`") || p.contains("doc/"));
        assert!(!p.contains("更新策略"));
        assert!(!p.contains("模块变更策略"));
        assert!(!p.contains("【API 变更策略】"));
    }

    #[test]
    fn build_docgen_prompt_custom_template() {
        let delta = ChangeDelta {
            head: "abc".into(),
            files: vec!["x.rs".into()],
            commits: vec!["c1".into()],
            diff_excerpt: "d".into(),
            diff_truncated: false,
        };
        let p = build_docgen_prompt(
            "只看 {files} 改 {doc_dir} @ {branch}",
            "docs",
            "main",
            &delta,
        );
        assert!(p.contains("- x.rs"));
        assert!(p.contains("docs"));
        assert!(p.contains("main"));
        assert!(!p.contains("{files}"));
    }

    #[test]
    fn build_quality_prompt_injects_summary() {
        let p = build_quality_prompt("", "doc", "release", &["a.rs".into()], "已更新 a");
        assert!(p.contains("已更新 a"));
        assert!(p.contains("- a.rs"));
        assert!(!p.contains("{summary}"));
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

    #[test]
    fn cron_slots_allow_multiple_runs_per_day_and_dedupe_same_minute() {
        use chrono::TimeZone;
        let mut cfg = DocgenConfig::default();
        cfg.cron = "0 9,17 * * *".into();
        let morning = chrono::Local
            .with_ymd_and_hms(2026, 11, 3, 9, 0, 0)
            .unwrap();
        let evening = chrono::Local
            .with_ymd_and_hms(2026, 11, 3, 17, 0, 0)
            .unwrap();

        assert_eq!(
            schedule_due_today(&cfg, None, None, morning).unwrap(),
            Some(ScheduleSlot::Cron("2026-11-03 09:00".into()))
        );
        assert_eq!(
            schedule_due_today(&cfg, None, Some("2026-11-03 09:00"), morning).unwrap(),
            None
        );
        assert_eq!(
            schedule_due_today(&cfg, None, Some("2026-11-03 09:00"), evening).unwrap(),
            Some(ScheduleSlot::Cron("2026-11-03 17:00".into()))
        );
    }

    #[test]
    fn daily_slot_remains_once_per_day() {
        use chrono::TimeZone;
        let mut cfg = DocgenConfig::default();
        cfg.time = "09:00".into();
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 3, 10, 0, 0)
            .unwrap();

        assert_eq!(
            schedule_due_today(&cfg, None, None, now).unwrap(),
            Some(ScheduleSlot::Daily("2026-11-03 09:00".into()))
        );
        assert_eq!(
            schedule_due_today(&cfg, Some("2026-11-03 09:00"), None, now).unwrap(),
            None
        );
    }

    #[test]
    fn changing_daily_time_creates_a_new_slot_on_the_same_day() {
        use chrono::TimeZone;
        let mut cfg = DocgenConfig::default();
        cfg.time = "17:29".into();
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 3, 17, 35, 0)
            .unwrap();

        assert_eq!(
            schedule_due_today(&cfg, Some("2026-11-03 09:00"), None, now).unwrap(),
            Some(ScheduleSlot::Daily("2026-11-03 17:29".into()))
        );
        assert_eq!(
            schedule_due_today(&cfg, Some("2026-11-03 17:29"), None, now).unwrap(),
            None
        );
    }
}
