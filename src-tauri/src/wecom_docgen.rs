//! 逆向文档的每日自动更新 + 多通道群通知。
//!
//! 到点 / cron / 本地 HEAD 轮询 / 手动：
//! 1. 在原仓目标分支上 fetch + 仅快进（不造隔离 worktree）；
//! 2. 取自上次更新以来的提交摘要 + 完整文件列表 + 按仓有界 diff；
//! 3. 天玑与 DSH 直接写入原仓文档目录，不开放命令或自动批准权限升级；
//! 4. 清单超预算时按仓/文件自动分批；可选只读质量自检；正式轮再提交；
//! 5. 多通道 webhook 通知；历史写入 `docgen-history.jsonl`（30 条）。
//!
//! 游标：`%APPDATA%\com.ai-workbench.app\wecom-dsh\docgen.json`。

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::wecom_channel::WecomState;

#[path = "docgen_workspace.rs"]
mod workspace;

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(15);
/// 每个文档更新回合（一批）的墙钟上限；短路径多时一批文件数另受 `MAX_FILES_PER_BATCH` 约束。
/// 模型（尤其 High）偶发很慢，给足 60 分钟。
const UPDATE_TURN_TIMEOUT_SECS: u64 = 3_600;
const UPDATE_TURN_TIMEOUT: Duration = Duration::from_secs(UPDATE_TURN_TIMEOUT_SECS);
/// 整轮质检专用超时（仍短于更新回合；质检只跑一次）。
const QUALITY_TURN_TIMEOUT_SECS: u64 = 900;
const QUALITY_TURN_TIMEOUT: Duration = Duration::from_secs(QUALITY_TURN_TIMEOUT_SECS);
/// 单批最多文件数：仅按字符预算会把 200+ 短路径塞进一批，易撞回合超时。
const MAX_FILES_PER_BATCH: usize = 100;
/// 超时批拆半重试的下限：文件数不大于此则不再拆。
const RESPLIT_MIN_FILES: usize = 20;
/// 质检提示里变更文件列表上限，避免把整仓清单塞进上下文。
const QUALITY_FILE_LIST_CAP: usize = 200;
const DIFF_CHAR_CAP: usize = 24_000;
const HISTORY_CAP: usize = 30;
const LOG_LINE_CAP: usize = 30;

fn timeout_label(secs: u64) -> String {
    if secs % 60 == 0 {
        format!("{} 分钟", secs / 60)
    } else {
        format!("{secs} 秒")
    }
}

fn turn_timeout_label() -> String {
    timeout_label(UPDATE_TURN_TIMEOUT_SECS)
}

fn quality_timeout_label() -> String {
    timeout_label(QUALITY_TURN_TIMEOUT_SECS)
}

/// 质检用的变更文件列表：去重顺序保留，超长截断并留提示行。
pub(crate) fn cap_quality_files(files: &[String], cap: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut uniq = Vec::new();
    for f in files {
        let key = f.replace('\\', "/");
        if seen.insert(key) {
            uniq.push(f.clone());
        }
    }
    if uniq.len() <= cap {
        return uniq;
    }
    let total = uniq.len();
    let mut out: Vec<String> = uniq.into_iter().take(cap).collect();
    out.push(format!("…（已截断，共 {total} 个变更文件，仅列前 {cap}）"));
    out
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static RUN_CANCEL: AtomicBool = AtomicBool::new(false);

/// 是否有更新流程在跑（设置页据此决定要不要顺手关掉临时拉起的 DSH 进程）。
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

fn cancel_requested() -> bool {
    RUN_CANCEL.load(Ordering::SeqCst)
}

fn require_running() -> Result<(), String> {
    if cancel_requested() {
        return Err("文档任务已请求停止".into());
    }
    Ok(())
}

/// 停止当前文档任务；下一次 tick / 立即更新会收到明确错误，不会留下半发布状态。
pub fn cancel_running() {
    RUN_CANCEL.store(true, Ordering::SeqCst);
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
    pub quality_gate: bool,
    #[serde(default)]
    pub auto_push: bool,
    /// 预览：仍直写原仓文档目录，但不提交、不推送、不推进游标。
    #[serde(default)]
    pub preview_mode: bool,
    /// 全量建档：忽略采集点，把当前 HEAD 作为初始文档范围（默认关）。
    #[serde(default)]
    pub full_init: bool,
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
    /// 天玑档独立模型快照；空 = 配置错误，不允许启动。
    #[serde(default)]
    pub tianji_agent: Option<crate::wecom_orchestrator::WecomAgentConfig>,
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
            quality_gate: false,
            auto_push: false,
            preview_mode: false,
            full_init: false,
            quality_check: false,
            poll_release: false,
            sync_sources: false,
            lookback_days: 7,
            repo_allowlist: Vec::new(),
            system_prompt: String::new(),
            quality_prompt: String::new(),
            engine: "tianji".into(),
            tianji_agent: None,
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
    "你是代码逆向文档维护助手。工作区是一个代码仓库，有两类文档产物：\n\
- `{doc_dir}/`：按模块沉淀的逆向工程说明书（长期知识）\n\
- `{updates_dir}/{date}.md`：按日沉淀的「今天改了哪些功能」（面向人读的变更日志）\n\
当前在 release 分支「{branch}」，今日日期 `{date}`。\n\n\
## 提交摘要\n\
{commits}\n\n\
## 变更文件\n\
{files}\n\n\
## 变更 diff 摘录（定向线索；完整内容请用工具读文件）\n\
{diff}\n{mapping}\n\n\
任务：阅读这些变更涉及的代码，完成两类产物。\n\
一、更新 `{doc_dir}/` 下**对应**的逆向文档，使其与最新代码一致。\n\
如何找对应文档（按顺序尝试）：\n\
- 先在 `{doc_dir}/` 下 grep 变更文件名、所在模块名、关键函数/类名，命中的文档就是要更新的；\n\
- 文件名/模块名找不到时，按业务功能关键词再搜一次；\n\
- 仍找不到且该变更重要时，才在 `{doc_dir}/` 下新建一篇，文件名贴合模块（不要建在 updates/ 下）。\n\
证据与写法：\n\
- diff 只是线索；改文档前必须用工具读现行源码与目标 md；只写读到的行为，禁止补未证实的架构或流程。\n\
- 新建或大段改写时尽量覆盖：职责、关键入口与调用链、核心类型或表、边界与错误；已有文档保持原结构，只改过时处，删掉与代码不符的段落，不堆砌无关背景。\n\
- 删除或重命名：同步改文档或标明已移除；纯格式/依赖噪音且无行为变化 → 模块文档可跳过，日报记「无行为变更」。\n\
二、追加当日功能更新：打开或新建 `{updates_dir}/{date}.md`，在文末追加一节（不要整文件覆盖）。\n\
节标题示例：`## HH:MM · {branch}`；正文用条目列出本批涉及的功能点与触及的文档名（用人话，如「订单结算」「文档中心门户」）。只写本批出现的变更，禁止编造未出现的模块。\n\
硬规则：\n\
1. **只允许修改/新建 `{doc_dir}/` 目录内的文件**（含 `{updates_dir}/`）；绝对不要改任何业务代码、配置或其它目录（越界操作会被拒绝，产物不会发布）。\n\
2. 变更很小 / 与文档无关时可以不改模块文档，但须在当日更新文件记一笔或写明跳过原因。\n\
3. 不要运行命令、不要删除或移动文件，只用读文件 / 搜索 / 写文件 / 改文件。写当日更新前先读已有文件再追加。\n\
4. 本批只改**直接相关**文档与当日 updates 追加节；不要通读全库文档。\n\
5. 优先完成摘要与落盘，控制工具往返；无关文件跳过并在摘要说明。\n\
6. 收尾先写 3–6 条**面向业务**的中文要点（改了什么能力、对谁有影响、更新了哪几篇文档）；禁止行号、函数/符号名、JSON 字段、commit hash、工具过程（grep/读文件等）。另起「## 代码引用」，每行一条且必须用箭头（**仅供系统映射落库，不要写进业务摘要正文**；群通知会自动省略该节）：\n\
   - `源码路径:符号` → `文档相对路径`"
}

pub fn default_docgen_quality_prompt() -> &'static str {
    "你是逆向文档质量审查员（只读）。工作区分支「{branch}」，文档目录 `{doc_dir}/`，今日 `{date}`。\n\
本轮更新摘要：\n\
{summary}\n\n\
本次代码变更文件：\n\
{files}\n\n\
任务：只审**本轮更新**。优先根据摘要与文件列表下结论；需要时最多抽查 3 个相关文档（含当日 `{updates_dir}/{date}.md` 相关节）核对源码。用 3–8 条中文结论；禁止只复述摘要，禁止广搜或通读 `{doc_dir}/`，禁止把整库历史欠账当本轮事故。\n\
范围：\n\
- 只核对本轮改动触及的文档与源码；\n\
- 历史欠账、无关模块、总索引陈旧 → 一律 `[建议]`；\n\
- 吃不准就不要 `[阻断]`；超时前必须先给出结论列表。\n\
`[阻断]` 仅限（须同时满足「本轮新写/新改内容」）：\n\
- 与本轮 `{files}` 源码直接矛盾；\n\
- 引用了本轮文件中不存在的符号；\n\
- 日报编造了本轮未出现的模块。\n\
输出格式（每条单独一行）：\n\
- `[阻断] …`；\n\
- `[建议] …`；\n\
- 无本轮硬伤写：`未发现过时或矛盾`。\n\
硬规则：只读文件/搜索，禁止写文件、改文件、跑命令、删除。"
}

/// `{doc_dir}/updates`（正斜杠相对路径）。
pub fn updates_dir_rel(doc_dir: &str) -> String {
    let d = doc_dir.trim().trim_end_matches(['/', '\\']);
    if d.is_empty() {
        "doc/updates".into()
    } else {
        format!("{d}/updates")
    }
}

fn local_date_ymd() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn summary_fingerprint(summary: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let key: String = summary
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(400)
        .collect();
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// 代理已写过本轮内容，或系统补记标记已在文件中 → 不再追加。
fn daily_update_already_covered(existing: &str, summaries: &str) -> bool {
    if existing.is_empty() {
        return false;
    }
    let fp = summary_fingerprint(summaries);
    if existing.contains(&format!("docgen-ensure:{fp}")) {
        return true;
    }
    for line in summaries.lines() {
        let t = line.trim();
        if t.chars().count() < 24 {
            continue;
        }
        if t.starts_with('#') || t.starts_with("### 第") {
            continue;
        }
        if existing.contains(t) {
            return true;
        }
        break;
    }
    false
}

/// 全批成功后兜底：保证 `{doc_dir}/updates/{date}.md` 至少有本轮条目。
pub fn ensure_daily_update(
    workspace: &str,
    doc_dir: &str,
    date: &str,
    run_meta: &str,
    summaries: &str,
) -> Result<PathBuf, String> {
    let date = date.trim();
    if date.is_empty() {
        return Err("每日更新日期为空".into());
    }
    let path = Path::new(workspace)
        .join(doc_dir.trim())
        .join("updates")
        .join(format!("{date}.md"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 updates 目录失败：{e}"))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if daily_update_already_covered(&existing, summaries) {
        return Ok(path);
    }
    let fp = summary_fingerprint(summaries);
    let body = truncate_chars(summaries.trim(), 2_000);
    let section = if body.is_empty() {
        format!("## {run_meta}\n\n<!-- docgen-ensure:{fp} -->\n\n- （本轮无摘要；仅占位）\n")
    } else {
        format!(
            "## {run_meta}\n\n<!-- docgen-ensure:{fp} -->\n\n\
（系统补记：代理未写入或未完整覆盖当日功能更新，以下摘自本轮摘要）\n\n{body}\n"
        )
    };
    let new_content = if existing.trim().is_empty() {
        format!("# {date} 功能更新\n\n{section}")
    } else {
        let mut s = existing;
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push('\n');
        s.push_str(&section);
        s
    };
    std::fs::write(&path, new_content).map_err(|e| format!("写入每日更新失败：{e}"))?;
    Ok(path)
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

/// 把「文档目录」归一成相对 workspace 的正斜杠相对路径。
///
/// 目录浏览框 / 手输都可能给出绝对路径（`D:\repo\doc`）甚至反斜杠相对路径（`doc\sub`）；
/// 而 `git status` 给出的是正斜杠相对路径，越界保护（is_outside_doc_dir）按字符串前缀比对，
/// 不归一就会把整轮改动全部误判为越界并还原。
fn normalize_doc_dir(workspace: &str, doc_dir: &str) -> Result<String, String> {
    use crate::git_commands::strip_verbatim_prefix;
    // 先剥 `\\?\`，再统一正斜杠，避免 canonicalize 工作区与手填绝对路径对不上。
    let workspace = strip_verbatim_prefix(Path::new(workspace));
    let workspace = workspace.to_string_lossy();
    let raw = strip_verbatim_prefix(Path::new(doc_dir.trim()))
        .to_string_lossy()
        .replace('\\', "/");
    if raw.starts_with("//") || raw.split('/').any(|p| p == ".." || p.eq_ignore_ascii_case(".git") || p.ends_with('.') || p.ends_with(' ')) {
        return Err("文档目录不能包含 ..、.git 或网络共享路径".into());
    }
    if raw.contains(':') && !std::path::Path::new(&raw).is_absolute() {
        return Err("文档目录不能使用盘符相对路径".into());
    }
    let raw = raw.trim_end_matches('/').to_string();
    if raw.is_empty() {
        return Ok("doc".into());
    }
    if !std::path::Path::new(&raw).is_absolute() {
        return Ok(raw);
    }
    // 绝对路径：按路径组件剥掉 workspace 前缀，避免大小写、斜杠和末尾前缀误匹配。
    let ws_components: Vec<String> = Path::new(workspace.as_ref())
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let raw_components: Vec<String> = Path::new(&raw)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let is_same = raw_components.len() == ws_components.len()
        && raw_components.iter().zip(&ws_components).all(|(a, b)| a.to_lowercase() == *b);
    if is_same {
        return Ok("doc".into());
    }
    if raw_components.len() <= ws_components.len()
        || raw_components.iter().zip(&ws_components).any(|(a, b)| a.to_lowercase() != *b)
    {
        return Err(format!(
            "文档目录必须位于工作区内：{}（工作区 {}）",
            crate::git_commands::path_for_markdown(&raw),
            crate::git_commands::path_for_markdown(workspace.as_ref())
        ));
    }
    let rel_components = &raw_components[ws_components.len().min(raw_components.len())..];
    let rel = rel_components.join("/");
    if rel.is_empty() {
        return Ok("doc".into());
    }
    Ok(rel)
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
    /// 上一次已观察过的本地 HEAD。
    #[serde(default)]
    last_poll_head: Option<String>,
    /// 定点任务待补跑到哪一分钟（应用重启后仍尝试补跑）。
    #[serde(default)]
    pending_daily_slot: Option<String>,
    #[serde(default)]
    pending_publication: Option<PendingPublication>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingPublication {
    workspace: String,
    branch: String,
    head: String,
    sources: Vec<(String, String)>,
    changed_files: Vec<String>,
    summary: String,
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
pub fn reset_cursors(repos: Option<&[String]>) -> Result<usize, String> {
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
    save_state(&st)?;
    Ok(removed)
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

fn mapping_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = base.join("com.ai-workbench.app").join("wecom-dsh");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("docgen-mapping.json"))
}

/// 代码事实 → 文档章节的最小映射；只存可追溯引用，不推断调用关系。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocgenReference {
    pub repo: String,
    pub sha: String,
    pub source_path: String,
    pub symbol: String,
    pub doc_path: String,
    pub section: String,
}

fn load_references() -> Vec<DocgenReference> {
    mapping_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_references(refs: &[DocgenReference]) -> Result<(), String> {
    let p = mapping_path().ok_or("无法解析文档映射目录")?;
    let text = serde_json::to_string_pretty(refs).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("写入文档映射失败：{e}"))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("替换文档映射失败：{e}"))
}

fn upsert_references(new: Vec<DocgenReference>) -> Result<(), String> {
    let mut all = load_references();
    for r in new {
        all.retain(|old| !(old.repo == r.repo && old.source_path == r.source_path && old.symbol == r.symbol));
        all.push(r);
    }
    // 上限避免历史映射无限增长；保留最近 2000 条。
    if all.len() > 2000 { all.drain(0..all.len() - 2000); }
    save_references(&all)
}

fn extract_summary_references(summary: &str, repo: &str, sha: &str, doc_dir: &str) -> Vec<DocgenReference> {
    let mut out = Vec::new();
    for line in summary.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // 只识别显式格式 `path:symbol` / `path#section`，不把自由文本全量当事实。
        let source = if let Some((left, right)) = line.split_once("→") {
            let doc = right.trim().trim_matches(|c: char| c == '`' || c == '-').trim().to_string();
            (left.trim().trim_matches(|c: char| c == '`' || c == '-').trim(), doc)
        } else {
            (line.trim().trim_matches(|c: char| c == '`' || c == '-').trim(), format!("{}/{}", doc_dir.trim_matches('/'), line.trim().trim_matches(|c: char| c == '`' || c == '-').trim()))
        };
        let (path, doc_path) = source;
        let Some((path, symbol)) = path.split_once(':').or_else(|| path.split_once('#')) else { continue; };
        let path = path.trim().trim_matches('`');
        if path.contains(' ') || path.is_empty() || !path.contains(['/', '.']) { continue; }
        let symbol = symbol.trim().trim_matches('`').split_whitespace().next().unwrap_or("").to_string();
        if symbol.is_empty() || symbol.len() > 200 || symbol.contains(':') { continue; }
        let doc_path = doc_path.trim();
        if doc_path.is_empty() || !doc_path.starts_with(doc_dir.trim_matches('/')) { continue; }
        out.push(DocgenReference { repo: repo.into(), sha: sha.into(), source_path: path.into(), symbol, doc_path: doc_path.to_string(), section: String::new() });
    }
    out
}

fn load_state() -> RunState {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(st: &RunState) -> Result<(), String> {
    let p = state_path().ok_or("无法解析文档任务状态目录")?;
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    let temporary = p.with_extension("json.tmp");
    std::fs::write(&temporary, text).map_err(|e| format!("写入文档任务状态失败：{e}"))?;
    std::fs::rename(&temporary, &p).map_err(|e| format!("替换文档任务状态失败：{e}"))
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
    pub references: Vec<DocgenReference>,
    #[serde(default)]
    pub commit_note: String,
    #[serde(default)]
    pub quality: String,
    #[serde(default)]
    pub quality_blocking: usize,
    pub pushed: bool,
    /// Git 本地提交 / 远端推送 / 群通知分别成功与否；`None` = 未执行。
    #[serde(default)]
    pub commit_status: Option<String>,
    #[serde(default)]
    pub push_status: Option<String>,
    #[serde(default)]
    pub notify_status: Option<String>,
    #[serde(default)]
    pub error: String,
    pub preview: bool,
    #[serde(default)]
    pub webhook_errors: Vec<String>,
    /// 整轮耗时（毫秒）；老记录没有这个字段，读到 0 就显示「—」。
    #[serde(default)]
    pub duration_ms: u64,
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
    duration_ms: u64,
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
        references: report.references.clone(),
        commit_note: report.commit_note.clone(),
        quality: truncate_chars(quality, 800),
        quality_blocking: report.quality_blocking,
        pushed: report.pushed,
        commit_status: Some(report.commit_status.clone()),
        push_status: Some(report.push_status.clone()),
        notify_status: Some(if report.pushed { "sent".into() } else { "not-sent".into() }),
        error: if report.failed() {
            report.error.clone()
        } else {
            report.skipped_reason.clone().unwrap_or_default()
        },
        preview,
        webhook_errors: webhook_errors.to_vec(),
        duration_ms,
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    format!("{}…", t.chars().take(max).collect::<String>())
}

/// 由独立调度任务每分钟调用，不与其它每日 Job 串行等待。
pub async fn auto_tick(app: AppHandle) -> Result<(), String> {
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
    if let Some(pending) = state.pending_daily_slot.clone() {
        let _slot = ScheduleSlot::Daily(pending.clone());
        let mut st = load_state();
        st.last_daily_slot = Some(pending.clone());
        st.pending_daily_slot = None;
        save_state(&st)?;
        match run_update_with_trigger(&app, &cfg, RunTrigger::Schedule).await {
            Ok(report) => crate::wecom_log::write("docgen", &format!("补跑完成：{}", report.summary_line())),
            Err(e) => {
                crate::wecom_log::write("docgen", &format!("补跑失败：{e}"));
                let mut st = load_state();
                st.pending_daily_slot = Some(pending);
                save_state(&st)?;
                return Err(e);
            }
        }
    }
    if let Some(slot) = schedule_due {
        let mut st = load_state();
        match &slot {
            ScheduleSlot::Daily(minute) => st.last_daily_slot = Some(minute.clone()),
            ScheduleSlot::Cron(minute) => st.last_cron_minute = Some(minute.clone()),
        }
        save_state(&st)?;
        crate::wecom_log::write(
            "docgen",
            &format!("auto schedule start branch={}", cfg.branch_or_default()),
        );
        let run_started = std::time::Instant::now();
        match run_update_with_trigger(&app, &cfg, RunTrigger::Schedule).await {
            Ok(report) => {
                crate::wecom_log::write("docgen", &format!("auto schedule done: {}", report.summary_line()));
            }
            Err(e) => {
                crate::wecom_log::write("docgen", &format!("auto schedule failed: {e}"));
                if let ScheduleSlot::Daily(minute) = &slot {
                    let mut st = load_state();
                    st.pending_daily_slot = Some(minute.clone());
                    save_state(&st)?;
                }
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
                    references: Vec::new(),
                    commit_note: String::new(),
                    quality: String::new(),
                    quality_blocking: 0,
                    pushed,
                    commit_status: Some("not-run".into()),
                    push_status: Some("not-run".into()),
                    notify_status: Some(if pushed { "sent".into() } else { "not-sent".into() }),
                    error: e.clone(),
                    preview: false,
                    webhook_errors: errs,
                    duration_ms: run_started.elapsed().as_millis() as u64,
                });
                return Err(e);
            }
        }
    }

    // 2) 本地 HEAD 轮询（不挡定点；成功只推进 last_commit）
    if cfg.poll_release {
        if let Err(e) = maybe_poll_release(&app, &cfg).await {
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
        // 进程错过的 cron 分钟在本小时内补跑一次；跨小时不追历史。
        if let Some(missed) = missed_cron_minute(cfg, last_cron_minute, now)? {
            return Ok(Some(ScheduleSlot::Cron(missed)));
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

fn missed_cron_minute(
    cfg: &DocgenConfig,
    last_cron_minute: Option<&str>,
    now: chrono::DateTime<chrono::Local>,
) -> Result<Option<String>, String> {
    let reference = last_cron_minute
        .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").ok())
        .unwrap_or_else(|| now.date_naive().and_hms_opt(0, 0, 0).unwrap());
    let mut candidate = reference;
    // 从上一次记录的下分钟开始，避免把「刚成功执行的那一分钟」当成漏跑。
    candidate += chrono::Duration::minutes(1);
    while candidate < now.naive_local() {
        candidate += chrono::Duration::minutes(1);
        let stamp = candidate.format("%Y-%m-%d %H:%M").to_string();
        let naive = candidate;
        if cron_matches(cfg.cron.trim(), chrono::TimeZone::from_local_datetime(&chrono::Local, &naive).single().unwrap())? {
            return Ok(Some(stamp));
        }
    }
    Ok(None)
}

/// 下次计划时间（配置值或可计算的下一次 cron 分钟）；UI 直接显示后端结果。
pub fn next_run_at(cfg: &DocgenConfig, now: chrono::DateTime<chrono::Local>) -> Result<Option<String>, String> {
    if !cfg.enabled || cfg.workspace.trim().is_empty() { return Ok(None); }
    if !cfg.cron.trim().is_empty() {
        let mut candidate = now;
        for _ in 0..(24 * 60) {
            candidate += chrono::Duration::minutes(1);
            if cron_matches(cfg.cron.trim(), candidate)? {
                return Ok(Some(candidate.format("%Y-%m-%d %H:%M").to_string()));
            }
        }
        return Ok(None);
    }
    let time = cfg.time_or_default();
    let Some(minutes) = crate::daily_schedule::parse_hhmm(&time) else { return Ok(None); };
    let mut candidate = now.date_naive().and_hms_opt(minutes / 60, minutes % 60, 0).unwrap();
    if candidate <= now.naive_local() { candidate += chrono::Duration::days(1); }
    Ok(Some(candidate.format("%Y-%m-%d %H:%M").to_string()))
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
    save_state(&st)?;

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
    let run_started = std::time::Instant::now();
    match run_update_with_trigger(&app, cfg, RunTrigger::Poll).await {
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
                references: Vec::new(),
                commit_note: String::new(),
                quality: String::new(),
                    quality_blocking: 0,
                pushed,
                commit_status: Some("not-run".into()),
                push_status: Some("not-run".into()),
                notify_status: Some(if pushed { "sent".into() } else { "not-sent".into() }),
                error: e.clone(),
                preview: false,
                webhook_errors: errs,
                duration_ms: run_started.elapsed().as_millis() as u64,
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
    pub references: Vec<DocgenReference>,
    pub commit_note: String,
    pub pushed: bool,
    pub commit_status: String,
    pub push_status: String,
    pub skipped_reason: Option<String>,
    pub preview: bool,
    pub quality: String,
    pub quality_blocking: usize,
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
    /// 本轮开始时间（毫秒时间戳）：前端运行中拿它做实时计时。
    #[serde(default)]
    started_at_ms: Option<i64>,
    /// 结束事件才有：整轮耗时（毫秒）。
    #[serde(default)]
    duration_ms: Option<u64>,
}

/// 最近一次进度事件。页面切走再回来（组件重新挂载）时用它把进度卡恢复出来，
/// 否则只能显示一张空壳。
static LAST_PROGRESS: std::sync::Mutex<Option<DocgenProgressEvent>> = std::sync::Mutex::new(None);

/// 读最近一次进度快照；从没跑过时是 None。
pub fn last_progress() -> Option<DocgenProgressEvent> {
    LAST_PROGRESS.lock().ok().and_then(|slot| slot.clone())
}

/// 丢掉最近一次进度快照（设置页「重置采集点」后调用）。
/// 否则页面重新挂载时会把上一轮的「已跳过」补回进度卡，看着像重置没生效。
pub fn clear_last_progress() {
    if let Ok(mut slot) = LAST_PROGRESS.lock() {
        *slot = None;
    }
}

/// 当前有采集点的仓（诊断用：重置时没匹配上要知道后端记的是哪些路径）。
pub fn cursor_repos() -> Vec<String> {
    let st = load_state();
    let mut out: Vec<String> = st.last_commits.keys().cloned().collect();
    out.sort();
    out
}

/// 进度卡恢复用：最近一次事件 + 「现在是否真的在跑」。
/// 页面卸载期间的收尾事件收不到，光看事件状态会把按钮永久锁成「运行中」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocgenProgressSnapshot {
    pub progress: Option<DocgenProgressEvent>,
    pub running: bool,
    pub next_run_at: Option<String>,
    pub pending_daily_slot: Option<String>,
}

pub async fn progress_snapshot(app: &AppHandle) -> DocgenProgressSnapshot {
    let cfg = app.state::<WecomState>().docgen.lock().await.clone();
    DocgenProgressSnapshot {
        progress: last_progress(),
        running: is_running(),
        next_run_at: next_run_at(&cfg, chrono::Local::now()).ok().flatten(),
        pending_daily_slot: load_state().pending_daily_slot,
    }
}

struct RunProgress<'a> {
    app: &'a AppHandle,
    run_id: String,
    trigger: RunTrigger,
    started_ms: i64,
    /// 失败时进度条停在哪一档（0 = 用默认 55）。
    fail_at: std::sync::atomic::AtomicU8,
    /// 失败时前端步骤灯点亮的阶段（如 agent / quality），避免只发 phase=fail 误亮质检。
    fail_phase: std::sync::Mutex<String>,
}

impl<'a> RunProgress<'a> {
    fn new(app: &'a AppHandle, trigger: RunTrigger) -> Self {
        Self {
            app,
            run_id: format!("docgen-{}", chrono::Utc::now().timestamp_millis()),
            trigger,
            started_ms: chrono::Utc::now().timestamp_millis(),
            fail_at: std::sync::atomic::AtomicU8::new(0),
            fail_phase: std::sync::Mutex::new(String::new()),
        }
    }

    fn mark_fail_at(&self, phase: &str, percent: u8) {
        self.fail_at
            .store(percent.min(95).max(1), Ordering::Relaxed);
        if let Ok(mut slot) = self.fail_phase.lock() {
            if !phase.trim().is_empty() {
                *slot = phase.trim().to_string();
            }
        }
    }

    /// 整轮耗时（毫秒），写历史用。
    fn elapsed_ms(&self) -> u64 {
        (chrono::Utc::now().timestamp_millis() - self.started_ms).max(0) as u64
    }

    fn emit(&self, phase: &str, percent: u8, status: &str, detail: &str) {
        // 结束事件（ok / skip / fail）顺手把整轮耗时带上，前端直接展示。
        let duration_ms = if status == "running" {
            None
        } else {
            Some(self.elapsed_ms())
        };
        let event = DocgenProgressEvent {
            run_id: self.run_id.clone(),
            trigger: self.trigger.as_str().into(),
            phase: phase.into(),
            percent: percent.min(100),
            status: status.into(),
            detail: detail.trim().to_string(),
            started_at_ms: Some(self.started_ms),
            duration_ms,
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
        } else if !report.pushed {
            // 更新成功了但群里没消息：把原因写进卡片，别让人以为是「通过了却静悄悄」。
            let detail = match report.webhook_errors.first() {
                Some(e) => format!("群通知未送达：{e}"),
                None => "群通知未发送（未配置任何群 webhook）".to_string(),
            };
            self.emit("done", 100, "ok", &detail);
        } else {
            self.emit("done", 100, "ok", "");
        }
    }

    fn finish_err(&self, error: &str) {
        let pct = self.fail_at.load(Ordering::Relaxed);
        let pct = if pct == 0 { 55 } else { pct };
        let phase = self
            .fail_phase
            .lock()
            .ok()
            .map(|p| p.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "fail".into());
        self.emit(&phase, pct, "fail", error);
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
    run_update_with_trigger(&app, cfg, trigger).await
}

pub async fn run_update_with_trigger(
    app: &AppHandle,
    cfg: &DocgenConfig,
    trigger: RunTrigger,
) -> Result<UpdateReport, String> {
    let Some(_guard) = RunGuard::acquire() else {
        return Err("已有一个文档更新流程在跑，请稍后再试".into());
    };
    RUN_CANCEL.store(false, Ordering::SeqCst);
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
            crate::wecom_log::write("docgen", &format!("{} failed: {e}", trigger.as_str()));
            // 定时任务在 auto_tick 里统一发失败通知；手动 / 预览是用户点的，这里补一条，
            // 否则切分支 / 采集失败这种早退会一声不响（看着像「跑完了没通知」）。
            if matches!(trigger, RunTrigger::Manual | RunTrigger::Preview) {
                let _ = notify_all(cfg, &format!("⚠️ 逆向文档自动更新失败：{e}")).await;
            }
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
    let doc_dir_raw = cfg.doc_dir_or_default();
    let workspace = workspace::resolve_doc_repo(cfg.workspace.trim(), &doc_dir_raw)?;
    let configured_workspace = cfg.workspace.trim().to_string();
    if workspace != configured_workspace {
        crate::wecom_log::write(
            "docgen",
            &format!(
                "resolved workspace={} (from {})",
                crate::git_commands::path_for_markdown(&workspace),
                crate::git_commands::path_for_markdown(&configured_workspace)
            ),
        );
    }
    // 父目录可能只是多仓容器；文档目录必须解析到真实 Git 工作树后才继续。
    if !crate::git_commands::ensure_git_repo(&workspace).is_ok() {
        return Err(format!(
            "文档目录 {} 未解析到 Git 仓库；请检查工作区/文档目录配置（父目录 {} 不是仓库）",
            crate::git_commands::path_for_markdown(&doc_dir_raw),
            crate::git_commands::path_for_markdown(&configured_workspace)
        ));
    }
    let branch = cfg.branch_or_default();
    let preview = cfg.preview_mode || trigger == RunTrigger::Preview;
    // 落盘 / 越界保护 / 提交全都按「相对 workspace 的正斜杠路径」比对 doc_dir，
    // 绝对路径或反斜杠写法在这里统一归一（浏览框 / 手输都可能给出）。
    let doc_dir = normalize_doc_dir(&workspace, &doc_dir_raw)?;

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
    // 正式轮要求原仓干净（直写文档目录）；预览允许在脏仓上改 doc，由用户自行核对。
    if !String::from_utf8_lossy(&dirty.stdout).trim().is_empty() && !preview {
        let reason = "工作区有未提交改动，已跳过自动更新（请先提交或暂存）".to_string();
        progress.stage("notify", 90);
        let (pushed, errs) =
            notify_all(cfg, &format!("⚠️ 逆向文档自动更新已跳过：{reason}")).await;
        let report = UpdateReport {
            branch,
            new_commits: 0,
            changed_files: Vec::new(),
            updated_docs: String::new(),
            references: Vec::new(),
            commit_note: String::new(),
            pushed,
            commit_status: "not-run".into(),
            push_status: "not-run".into(),
            skipped_reason: Some(reason),
            preview,
            quality: String::new(),
                    quality_blocking: 0,
            webhook_errors: errs,
            error: String::new(),
        };
        append_history(&history_from_report(
            trigger,
            &report,
            "",
            &report.webhook_errors,
            progress.elapsed_ms(),
        ));
        return Ok(report);
    }

    progress.stage("branch", 20);
    if !preview {
        if let Some(pending) = load_state().pending_publication {
            if norm_repo_key(&pending.workspace) == norm_repo_key(&workspace) && pending.branch == branch {
                if !cfg.auto_push {
                    return Err("存在待发布提交，但自动推送已关闭；未发送，请恢复自动推送后重试".into());
                }
                let mut error = String::new();
                if let Err(e) = retry_publication(&pending) { error = e; }
                if error.is_empty() {
                    let mut st = load_state();
                    for (repo, head) in &pending.sources { set_cursor(&mut st, repo, head); }
                    st.pending_publication = None;
                    save_state(&st)?;
                }
                let summary = if error.is_empty() { "已重试发布成功（复用已有提交，未重新调用模型）".into() }
                    else { format!("发布重试失败：{error}") };
                let (pushed, webhook_errors) = notify_all(cfg, &summary).await;
                let report = UpdateReport {
                    branch, new_commits: 0, changed_files: pending.changed_files,
                    updated_docs: pending.summary, references: Vec::new(), commit_note: summary, pushed,
                    commit_status: "reused".into(), push_status: if error.is_empty() { "ok".into() } else { "fail".into() },
                    skipped_reason: None, preview: false, quality: String::new(), quality_blocking: 0, webhook_errors, error,
                };
                append_history(&history_from_report(trigger, &report, "", &report.webhook_errors, progress.elapsed_ms()));
                return Ok(report);
            }
            return Err("另一个文档目标有待发布提交，请先恢复原目标并重试，避免覆盖待发布状态".into());
        }
    }
    // 正式轮：原仓必须已在目标分支，fetch 后仅快进。预览同样要求分支正确，避免写错分支文档。
    {
        let ws = workspace.clone();
        let br = branch.clone();
        tokio::task::spawn_blocking(move || prepare_workspace_branch(&ws, &br))
            .await
            .map_err(|e| e.to_string())??;
    }
    let execution_workspace = workspace.clone();

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

    require_running()?;
    progress.stage("collect", 35);
    let sources = resolve_change_sources(&cfg.repo_allowlist, &workspace);
    let lookback_days = cfg.lookback_days.min(30);
    let st_snap = load_state();
    let mut source_prev: Vec<(String, Option<String>)> = sources
        .iter()
        .map(|s| (s.clone(), cursor_for(&st_snap, s)))
        .collect();
    if cfg.full_init {
        // 全量建档只对「当前没有采集点」的仓生效；已有游标的仓仍按增量，避免重复覆盖。
        source_prev.iter_mut().for_each(|(_, prev)| *prev = None);
    }
    // 本轮开始时还没有采集点的仓（新加 / 刚重置）：空采集时不能给它们落游标。
    let fresh_repos: Vec<String> = source_prev
        .iter()
        .filter(|(_, prev)| prev.is_none())
        .map(|(repo, _)| repo.clone())
        .collect();
    let doc_workspace = workspace.clone();
    let execution_repo = execution_workspace.clone();
    let parts = {
        tokio::task::spawn_blocking(move || -> Result<Vec<(String, ChangeDelta)>, String> {
            let mut out = Vec::new();
            // 快照必须活到本轮采集结束。若只 `.path.clone()` 就丢临时值，Drop 会立刻
            // `worktree remove`，随后 collect_delta 的 cwd 已不存在 → Windows os error 267。
            let mut snapshots = Vec::new();
            for (repo, prev) in source_prev {
                crate::git_commands::ensure_git_repo(&repo)?;
                let head = crate::git_commands::git_stdout(&repo, &["rev-parse", "HEAD"])?;
                let read_repo = if norm_repo_key(&repo) == norm_repo_key(&doc_workspace) {
                    execution_repo.clone()
                } else {
                    let snap = workspace::snapshot_source(&repo, head.trim())?;
                    let path = snap.path.clone();
                    snapshots.push(snap);
                    path
                };
                let d = collect_delta(&read_repo, prev.as_deref(), lookback_days)?;
                out.push((repo, d));
            }
            drop(snapshots);
            Ok(out)
        })
        .await
        .map_err(|e| e.to_string())??
    };
    let heads: Vec<(String, String)> = parts
        .iter()
        .map(|(r, d)| (r.clone(), d.head.clone()))
        .collect();
    let batches = split_deltas_for_budget(&parts, 24_000, DIFF_CHAR_CAP);
    let delta = merge_change_deltas(&parts, DIFF_CHAR_CAP);
    crate::wecom_log::write(
        "docgen",
        &format!(
            "collect_delta sources={} lookback={lookback_days}d files={} commits={} batches={}",
            sources.len(),
            delta.files.len(),
            delta.commits.len(),
            batches.len()
        ),
    );

    if delta.files.is_empty() {
        let mut st = load_state();
        for (repo, head) in &heads {
            // 本来就没采集点的仓（新加进白名单 / 刚点过「重置采集点」）这里不落游标：
            // 一旦落上，「空采集」就把刚空的采集点顶回 HEAD，之后每轮都判成无变更。
            if fresh_repos
                .iter()
                .any(|r| norm_repo_key(r) == norm_repo_key(repo))
            {
                continue;
            }
            set_cursor(&mut st, repo, head);
        }
        if !preview && !cancel_requested() {
            save_state(&st)?;
        }
        let reason = format!(
            "自上次更新以来无代码变更（本次监视 {} 个仓；未勾选的子仓不会被采集）",
            sources.len()
        );
        // 手动 / 预览是用户自己点的：跳过也给个回执，否则群里一片安静，像没跑过。
        let (pushed, webhook_errors) =
            if matches!(trigger, RunTrigger::Manual | RunTrigger::Preview) {
                notify_all(cfg, &format!("ℹ️ 逆向文档无需更新：{reason}")).await
            } else {
                (false, Vec::new())
            };
        let report = UpdateReport {
            branch,
            new_commits: 0,
            changed_files: Vec::new(),
            updated_docs: String::new(),
            references: Vec::new(),
            commit_note: String::new(),
            pushed,
            commit_status: "not-run".into(),
            push_status: "not-run".into(),
            skipped_reason: Some(reason),
            preview,
            quality: String::new(),
                    quality_blocking: 0,
            webhook_errors: webhook_errors.clone(),
            error: String::new(),
        };
        append_history(&history_from_report(
            trigger,
            &report,
            "",
            &webhook_errors,
            progress.elapsed_ms(),
        ));
        return Ok(report);
    }

    require_running()?;
    let batch_total = batches.len().max(1);
    let mut step_error = String::new();
    let mut batch_errors: Vec<String> = Vec::new();
    let mut updated_docs = String::new();
    let mut quality = String::new();
    let mut quality_blocking = 0usize;
    let mut qc_files: Vec<String> = Vec::new();
    for (idx, batch) in batches.iter().enumerate() {
        require_running()?;
        let batch_no = idx + 1;
        let pct = 55 + ((idx as u8).saturating_mul(15) / batch_total as u8);
        let agent_pct = pct.min(70);
        progress.emit(
            "agent",
            agent_pct,
            "running",
            &format!("第 {batch_no}/{batch_total} 批（{} 个文件）", batch.files.len()),
        );
        let append_batch = |updated_docs: &mut String, summary: &str| {
            if !updated_docs.is_empty() {
                updated_docs.push_str("\n\n");
            }
            if batch_total > 1 {
                updated_docs.push_str(&format!("### 第 {batch_no}/{batch_total} 批\n"));
            }
            updated_docs.push_str(summary);
        };
        match update_docs_with_agent(
            app,
            &execution_workspace,
            &doc_dir,
            &branch,
            batch,
            &cfg.system_prompt,
            &cfg.engine,
            &cfg.tianji_agent,
            cancel_requested,
        )
        .await
        {
            Ok(v) => {
                append_batch(&mut updated_docs, &v);
                qc_files.extend(batch.files.iter().cloned());
            }
            Err(e) => {
                progress.mark_fail_at("agent", agent_pct);
                // 超时且文件够多：拆半各重试一次；仍失败则记错并继续后续批。
                let mut rescued = false;
                let mut attempted_resplit = false;
                if is_turn_timeout_error(&e) {
                    if let Some((left, right)) = split_delta_half(batch) {
                        attempted_resplit = true;
                        progress.emit(
                            "agent",
                            agent_pct,
                            "running",
                            &format!(
                                "第 {batch_no}/{batch_total} 批超时，拆半重试（{}+{} 文件）",
                                left.files.len(),
                                right.files.len()
                            ),
                        );
                        let mut halves_ok = true;
                        for (hi, half) in [&left, &right].into_iter().enumerate() {
                            require_running()?;
                            match update_docs_with_agent(
                                app,
                                &execution_workspace,
                                &doc_dir,
                                &branch,
                                half,
                                &cfg.system_prompt,
                                &cfg.engine,
                                &cfg.tianji_agent,
                                cancel_requested,
                            )
                            .await
                            {
                                Ok(v) => {
                                    if !updated_docs.is_empty() {
                                        updated_docs.push_str("\n\n");
                                    }
                                    updated_docs.push_str(&format!(
                                        "### 第 {batch_no}/{batch_total} 批·拆半 {}\n",
                                        hi + 1
                                    ));
                                    updated_docs.push_str(&v);
                                    qc_files.extend(half.files.iter().cloned());
                                }
                                Err(e2) => {
                                    halves_ok = false;
                                    batch_errors.push(format!(
                                        "文档更新失败（第 {batch_no}/{batch_total} 批拆半 {}）：{e2}",
                                        hi + 1
                                    ));
                                }
                            }
                        }
                        rescued = halves_ok;
                    }
                }
                if !rescued && !attempted_resplit {
                    batch_errors.push(format!(
                        "文档更新失败（第 {batch_no}/{batch_total} 批）：{e}"
                    ));
                }
            }
        }
    }
    if !batch_errors.is_empty() {
        step_error = batch_errors.join("；");
    }
    if updated_docs.is_empty() && !step_error.is_empty() {
        updated_docs = format!("（{step_error}）");
    }

    // 整轮只跑一次质检（不再按批各跑一轮）。
    if step_error.is_empty() && cfg.quality_check {
        require_running()?;
        progress.emit("quality", 75, "running", "整轮质量自检");
        let files_for_qc = cap_quality_files(&qc_files, QUALITY_FILE_LIST_CAP);
        match quality_check_with_agent(
            app,
            &execution_workspace,
            &doc_dir,
            &branch,
            &files_for_qc,
            &updated_docs,
            &cfg.quality_prompt,
            &cfg.engine,
            &cfg.tianji_agent,
            cancel_requested,
        )
        .await
        {
            Ok(v) => {
                quality = v.clone();
                if cfg.quality_gate {
                    let verdict = parse_quality_verdict(&v);
                    quality_blocking = verdict.blocking.len();
                    if !verdict.blocking.is_empty() {
                        progress.mark_fail_at("quality", 75);
                        step_error = format_quality_gate_error("整轮质检", &verdict.blocking);
                    }
                }
            }
            Err(e) => {
                let warn = format!("质量自检未完成：{e}");
                quality = format!("（{warn}）");
                if cfg.quality_gate {
                    progress.mark_fail_at("quality", 75);
                    step_error = warn;
                }
                // 门禁关：质检失败只记警告，不挡后续提交。
            }
        }
    }

    let mut commit_note = String::new();
    let mut published = false;
    let mut commit_status = "not-run".to_string();
    let mut push_status = "not-run".to_string();
    // 已经失败时不要再把进度推到「提交」档（否则前端五段会被画满）。
    let publish_phase = if preview {
        "preview"
    } else if cfg.auto_commit {
        "commit"
    } else {
        "publish"
    };
    if step_error.is_empty() {
        progress.stage(publish_phase, 82);
        if preview {
            progress.stage("preview", 82);
        }
        require_running()?;
        let date = local_date_ymd();
        let time = chrono::Local::now().format("%H:%M").to_string();
        let run_meta = if batch_total > 1 {
            format!("{time} · {branch} · 共 {batch_total} 批")
        } else {
            format!("{time} · {branch}")
        };
        if let Err(e) = ensure_daily_update(
            &execution_workspace,
            &doc_dir,
            &date,
            &run_meta,
            &updated_docs,
        ) {
            progress.mark_fail_at(publish_phase, 82);
            step_error = e;
        }
    }
    if step_error.is_empty() {
        require_running()?;
        if let Err(e) = workspace::validate_changes(&execution_workspace, &doc_dir) {
            progress.mark_fail_at(publish_phase, 82);
            step_error = e;
        }
    }
    if !step_error.is_empty() {
        commit_note = format!(
            "本次失败，未提交；文档目录改动仍留在工作区（共 {batch_total} 批，已写入部分可能需人工核对）"
        );
    } else if preview || !cfg.auto_commit {
        commit_status = "not-run".into();
        push_status = "not-run".into();
        commit_note = if preview {
            format!("预览模式：已直写文档目录（{batch_total} 批），未提交、未推送，采集点未推进")
        } else {
            format!("已直写文档目录（{batch_total} 批），自动提交已关闭，未推送，采集点未推进")
        };
    } else {
        let ws = execution_workspace.clone();
        let dd = doc_dir.clone();
        let br = branch.clone();
        let n = delta.files.len();
        match tokio::task::spawn_blocking(move || commit_docs(&ws, &dd, &br, n))
            .await.map_err(|e| e.to_string())?
        {
            Err(e) => {
                progress.mark_fail_at("commit", 82);
                commit_status = "fail".into();
                push_status = "not-run".into();
                step_error = format!("文档提交失败：{e}")
            }
            Ok(note) => {
                commit_status = "ok".into();
                commit_note = if batch_total > 1 {
                    format!("{note}（分 {batch_total} 批生成）")
                } else {
                    note
                };
                published = true;
                if cfg.auto_push {
                    let pending = PendingPublication {
                        workspace: workspace.clone(),
                        branch: branch.clone(),
                        head: crate::git_commands::git_stdout(&workspace, &["rev-parse", "HEAD"])?,
                        sources: heads.clone(),
                        changed_files: delta.files.clone(),
                        summary: updated_docs.clone(),
                    };
                    let mut st = load_state();
                    st.pending_publication = Some(pending.clone());
                    save_state(&st)?;
                    match retry_publication(&pending) {
                        Ok(()) => {
                            commit_note.push_str("；远端推送成功");
                            push_status = "ok".into();
                        }
                        Err(e) => {
                            progress.mark_fail_at("commit", 85);
                            step_error =
                                format!("已提交到本地，但远端推送失败：{e}；下次仅重试推送");
                            push_status = "fail".into();
                        }
                    }
                }
            }
        }
        if !step_error.is_empty() {
            commit_note.push_str(&format!("；{step_error}"));
        }
    }
    let failed = !step_error.is_empty();

    let notice_summary = if step_error.is_empty() { updated_docs.clone() }
        else { format!("本次未完整成功：{step_error}\n{updated_docs}") };
    let body = compose_notice(
        &branch,
        &delta.files,
        &notice_summary,
        &commit_note,
        &quality,
        preview,
        &sync_note,
    );
    let body = if failed {
        format!("## ⚠️ 逆向文档更新失败\n{}", body.lines().skip(1).collect::<Vec<_>>().join("\n"))
    } else { body };
    if !failed {
        progress.stage("notify", 92);
    }
    let (pushed, webhook_errors) = notify_all(cfg, &body).await;

    if !failed {
        progress.stage("cursor", 98);
    }
    // 失败 / 预览 / 未提交：不推进游标，已写入的文档改动留在工作区。
    if should_advance_cursor(published, failed) {
        let mut st = load_state();
        for (repo, head) in &heads {
            set_cursor(&mut st, repo, head);
        }
        st.pending_publication = None;
        save_state(&st)?;
    } else {
        crate::wecom_log::write(
            "docgen",
            &format!("保留游标不推进（预览 / 未提交 / 失败）：{}", step_error),
        );
    }

    let updated_docs = format!("{commit_note}\n\n{updated_docs}");
    let mut references = Vec::new();
    for (repo, head) in &heads {
        references.extend(extract_summary_references(&updated_docs, repo, &head, &doc_dir));
    }
    if !references.is_empty() && step_error.is_empty() {
        if let Err(e) = upsert_references(references.clone()) {
            crate::wecom_log::write("docgen", &format!("文档映射写入失败：{e}"));
        }
    }
    let report = UpdateReport {
        branch,
        new_commits: delta.commits.len(),
        changed_files: delta.files,
        updated_docs,
        references,
        commit_note,
        commit_status,
        push_status,
        pushed,
        skipped_reason: None,
        preview,
        quality: quality.clone(),
        quality_blocking,
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
        progress.elapsed_ms(),
    ));
    Ok(report)
}

fn retry_publication(pending: &PendingPublication) -> Result<(), String> {
    use crate::git_commands::{git_output, git_stdout};
    let tip = git_stdout(&pending.workspace, &["rev-parse", &format!("refs/heads/{}", pending.branch)])?;
    if tip != pending.head {
        return Err("待发布分支已发生变化，拒绝将用户的新提交一起推送".into());
    }
    let out = git_output(&pending.workspace, &["push", "origin", &format!("{}:refs/heads/{}", pending.head, pending.branch)])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

fn commit_docs(
    workspace: &str,
    doc_dir: &str,
    branch: &str,
    code_changes: usize,
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
    Ok(format!("已提交 {} 篇文档（未推送）", changed_docs.len()))
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
        let current = git_stdout(repo, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
        if current != branch {
            notes.push(format!("{name}：当前分支不是 {branch}，跳过同步"));
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

/// 原仓准备：必须在目标分支上，fetch 后仅快进，失败明确报错（不造临时树）。
fn prepare_workspace_branch(repo: &str, branch: &str) -> Result<(), String> {
    use crate::git_commands::{git_output, git_stdout};
    git_stdout(repo, &["check-ref-format", "--branch", branch])?;
    let current = git_stdout(repo, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
    if current.trim() != branch {
        return Err(format!(
            "工作区当前分支是「{}」，请先切到「{branch}」再跑文档更新",
            if current.trim().is_empty() {
                "（游离 HEAD）"
            } else {
                current.trim()
            }
        ));
    }
    match git_output(repo, &["fetch", "origin", branch]) {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            return Err(format!(
                "fetch origin/{branch} 失败：{}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        Err(e) => return Err(format!("fetch origin/{branch} 失败：{e}")),
    }
    let track = format!("origin/{branch}");
    let remote = git_stdout(repo, &["rev-parse", "--verify", "-q", &track]).unwrap_or_default();
    if remote.trim().is_empty() {
        return Ok(());
    }
    match git_output(repo, &["merge", "--ff-only", &track]) {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "无法快进到 {track}：{}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("无法快进到 {track}：{e}")),
    }
}

fn is_turn_timeout_error(err: &str) -> bool {
    err.contains("期限") || err.contains("超时")
}

/// 超时批拆成两半（只拆一层）。文件数 ≤ `RESPLIT_MIN_FILES` 时返回 None。
pub(crate) fn split_delta_half(delta: &ChangeDelta) -> Option<(ChangeDelta, ChangeDelta)> {
    if delta.files.len() <= RESPLIT_MIN_FILES {
        return None;
    }
    let mid = delta.files.len() / 2;
    if mid == 0 || mid >= delta.files.len() {
        return None;
    }
    let left = ChangeDelta {
        head: delta.head.clone(),
        files: delta.files[..mid].to_vec(),
        commits: delta.commits.clone(),
        diff_excerpt: delta.diff_excerpt.clone(),
        diff_truncated: delta.diff_truncated,
    };
    let right = ChangeDelta {
        head: delta.head.clone(),
        files: delta.files[mid..].to_vec(),
        commits: Vec::new(),
        diff_excerpt: String::new(),
        diff_truncated: false,
    };
    Some((left, right))
}

/// 把多仓增量切成「文件清单字符数 ≤ file_list_cap 且文件数 ≤ MAX_FILES_PER_BATCH」的若干批；
/// 优先整仓同批，单仓仍超再按文件切。
pub(crate) fn split_deltas_for_budget(
    parts: &[(String, ChangeDelta)],
    file_list_cap: usize,
    diff_cap: usize,
) -> Vec<ChangeDelta> {
    let full = merge_change_deltas(parts, diff_cap);
    if full.files.is_empty() {
        return vec![full];
    }
    let under_chars = format_file_list(&full.files).chars().count() <= file_list_cap;
    if under_chars && full.files.len() <= MAX_FILES_PER_BATCH {
        return vec![full];
    }

    let mut batches: Vec<ChangeDelta> = Vec::new();
    let mut current: Vec<(String, ChangeDelta)> = Vec::new();

    let packed_chars = |batch: &[(String, ChangeDelta)]| -> usize {
        format_file_list(&merge_change_deltas(batch, diff_cap).files)
            .chars()
            .count()
    };
    let packed_files = |batch: &[(String, ChangeDelta)]| -> usize {
        merge_change_deltas(batch, diff_cap).files.len()
    };
    let fits = |batch: &[(String, ChangeDelta)]| -> bool {
        packed_chars(batch) <= file_list_cap && packed_files(batch) <= MAX_FILES_PER_BATCH
    };

    for (repo, delta) in parts {
        if delta.files.is_empty() && delta.commits.is_empty() && delta.diff_excerpt.is_empty() {
            continue;
        }
        let unit = (repo.clone(), delta.clone());
        let needs_chunk = packed_chars(&[unit.clone()]) > file_list_cap
            || delta.files.len() > MAX_FILES_PER_BATCH;
        if needs_chunk {
            if !current.is_empty() {
                batches.push(merge_change_deltas(&current, diff_cap));
                current.clear();
            }
            for chunk in chunk_repo_delta(delta, file_list_cap, repo, diff_cap) {
                batches.push(merge_change_deltas(&[(repo.clone(), chunk)], diff_cap));
            }
            continue;
        }
        let mut trial = current.clone();
        trial.push(unit.clone());
        if current.is_empty() || fits(&trial) {
            current.push(unit);
        } else {
            batches.push(merge_change_deltas(&current, diff_cap));
            current.clear();
            current.push(unit);
        }
    }
    if !current.is_empty() {
        batches.push(merge_change_deltas(&current, diff_cap));
    }
    if batches.is_empty() {
        vec![full]
    } else {
        batches
    }
}

fn chunk_repo_delta(
    delta: &ChangeDelta,
    file_list_cap: usize,
    repo: &str,
    diff_cap: usize,
) -> Vec<ChangeDelta> {
    if delta.files.is_empty() {
        return vec![delta.clone()];
    }
    let mut out = Vec::new();
    let mut buf: Vec<String> = Vec::new();
    let mut first = true;
    let flush = |buf: &mut Vec<String>, out: &mut Vec<ChangeDelta>, first: &mut bool| {
        if buf.is_empty() {
            return;
        }
        out.push(ChangeDelta {
            head: delta.head.clone(),
            files: std::mem::take(buf),
            commits: if *first {
                delta.commits.clone()
            } else {
                Vec::new()
            },
            diff_excerpt: if *first {
                delta.diff_excerpt.clone()
            } else {
                String::new()
            },
            diff_truncated: delta.diff_truncated && *first,
        });
        *first = false;
    };
    for f in &delta.files {
        let mut trial = buf.clone();
        trial.push(f.clone());
        let trial_delta = ChangeDelta {
            head: delta.head.clone(),
            files: trial.clone(),
            commits: Vec::new(),
            diff_excerpt: String::new(),
            diff_truncated: false,
        };
        let n = format_file_list(
            &merge_change_deltas(&[(repo.to_string(), trial_delta)], diff_cap).files,
        )
        .chars()
        .count();
        let over_files = trial.len() > MAX_FILES_PER_BATCH;
        if !buf.is_empty() && (n > file_list_cap || over_files) {
            flush(&mut buf, &mut out, &mut first);
        }
        buf.push(f.clone());
    }
    flush(&mut buf, &mut out, &mut first);
    if out.is_empty() {
        vec![delta.clone()]
    } else {
        out
    }
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
    // 各仓至少分到一份 diff 线索，完整元数据不受 diff 预算影响。
    let per_repo_cap = diff_cap / parts.iter().filter(|(_, d)| !d.diff_excerpt.trim().is_empty()).count().max(1);
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
            if block.chars().count() > per_repo_cap || diff.chars().count() + block.chars().count() > diff_cap {
                truncated = true;
                let remain = per_repo_cap.min(diff_cap.saturating_sub(diff.chars().count()));
                if remain > 0 {
                    diff.push_str(&block.chars().take(remain).collect::<String>());
                }
                continue;
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
    use crate::git_commands::git_stdout;
    let head = git_stdout(workspace, &["rev-parse", "HEAD"])?;
    let prev = clamp_delta_prev(workspace, prev, lookback_days)?;
    let base = match prev {
        Some(p) => p,
        None => {
            // 首个 root commit 没有 HEAD~1，用空树作为基线。
            git_stdout(workspace, &["rev-parse", "--verify", "HEAD~1"]).or_else(|_| {
                git_stdout(workspace, &["hash-object", "-t", "tree", "--stdin"])
            })?
        }
    };
    let range = format!("{}..{}", base.trim(), head.trim());
    collect_delta_range(workspace, &head, &range)
}

fn collect_delta_range(workspace: &str, head: &str, range: &str) -> Result<ChangeDelta, String> {
    use crate::git_commands::{git_output, git_stdout};
    // 比较失败必须报错，不能退回最近一条提交后把整个区间标成已处理。
    let files = parse_names(&git_stdout(workspace, &["-c", "core.quotepath=false", "diff", "--name-only", &range])?);

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
        head: head.to_string(),
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

/// 质量自检结论；只有显式 `[阻断]`（或英文等价标签）才进门禁，避免「过时」随口一词掐死整轮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityVerdict {
    pub blocking: Vec<String>,
    pub findings: Vec<String>,
}

pub fn parse_quality_verdict(raw: &str) -> QualityVerdict {
    let mut blocking = Vec::new();
    let mut findings = Vec::new();
    for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let text = line.trim_start_matches(['-', '*', '#', '>', '·']).trim();
        if text.is_empty() {
            continue;
        }
        findings.push(text.to_string());
        let lower = text.to_lowercase();
        if lower.contains("未发现") {
            continue;
        }
        let is_suggest = lower.contains("[建议]")
            || lower.contains("[suggest]")
            || (lower.contains("建议") && !lower.contains("[阻断]"));
        if is_suggest {
            continue;
        }
        let tagged_block = lower.contains("[阻断]")
            || lower.contains("[block]")
            || lower.contains("blocking:");
        if tagged_block {
            blocking.push(text.to_string());
        }
    }
    QualityVerdict { blocking, findings }
}

/// 门禁错误里只留前几条阻断，避免把整篇质检贴进进度卡。
fn format_quality_gate_error(scope: &str, blocking: &[String]) -> String {
    const MAX_ITEMS: usize = 5;
    const MAX_CHARS: usize = 160;
    let items: Vec<String> = blocking
        .iter()
        .take(MAX_ITEMS)
        .map(|s| {
            let t = s.trim();
            if t.chars().count() <= MAX_CHARS {
                t.to_string()
            } else {
                format!("{}…", t.chars().take(MAX_CHARS).collect::<String>())
            }
        })
        .collect();
    let more = blocking.len().saturating_sub(items.len());
    let mut body = items.join("；");
    if more > 0 {
        body.push_str(&format!("；另有 {more} 条…"));
    }
    format!("质量门禁阻断（{scope}）：{body}")
}

/// 拼装更新 system 提示：配置模板非空则用之，否则内置默认；再填占位符。
pub fn build_docgen_prompt(
    template: &str,
    doc_dir: &str,
    branch: &str,
    delta: &ChangeDelta,
    repo: &str,
) -> String {
    let tpl = normalize_prompt_template(template);
    let body = if tpl.is_empty() {
        default_docgen_system_prompt().to_string()
    } else {
        tpl
    };
    let mapped = load_references().into_iter()
        .filter(|r| r.repo == *repo)
        .map(|r| format!("- `{}:{}` → `{}`（{}）", r.source_path, r.symbol, r.doc_path, &r.sha[..r.sha.len().min(8)]))
        .take(120)
        .collect::<Vec<_>>()
        .join("\n");
    let mapping_section = if mapped.is_empty() { String::new() } else { format!("\n## 已有代码—文档映射（优先复用，仍须核对当前代码）\n{mapped}\n") };
    let commits = format_commit_list(&delta.commits);
    let files = format_file_list(&delta.files);
    let diff = format_diff_section(delta);
    let updates_dir = updates_dir_rel(doc_dir);
    let date = local_date_ymd();
    fill_docgen_template(
        &body,
        &[
            ("doc_dir", doc_dir),
            ("branch", branch),
            ("commits", &commits),
            ("files", &files),
            ("diff", &diff),
            ("mapping", &mapping_section),
            ("date", &date),
            ("updates_dir", &updates_dir),
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
    let updates_dir = updates_dir_rel(doc_dir);
    let date = local_date_ymd();
    fill_docgen_template(
        &body,
        &[
            ("doc_dir", doc_dir),
            ("branch", branch),
            ("files", &file_list),
            ("summary", summary_text),
            ("date", &date),
            ("updates_dir", &updates_dir),
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
    tianji_agent: &Option<crate::wecom_orchestrator::WecomAgentConfig>,
    cancel: impl Fn() -> bool + Send + 'static,
) -> Result<String, String> {
    let system = build_docgen_prompt(system_template, doc_dir, branch, delta, workspace);
    let user = "请根据本次代码变更更新对应的逆向文档，追加当日功能更新，并给出更新摘要。";

    let summary = if engine.trim().eq_ignore_ascii_case("dsh") {
        dsh_turn(
            app,
            workspace,
            doc_dir,
            DOCGEN_SESSION,
            &system,
            user,
            cancel_requested,
            UPDATE_TURN_TIMEOUT,
            &turn_timeout_label(),
        )
        .await?
    } else {
        let Some(agent_cfg) = tianji_agent.as_ref() else {
            return Err("文档任务未绑定独立天玑模型；请在「逆向工程 → 引擎提示词」选择模型后再运行".into());
        };

        let request_id = format!("wecom-docgen-{}", chrono::Utc::now().timestamp_millis());

        let req = crate::agent::AgentRunRequest {
            config: agent_cfg.config.clone(),
            fallback_config: agent_cfg.fallback_config.clone(),
            system,
            user: user.into(),
            request_id: request_id.clone(),
            repo_path: workspace.to_string(),
            services: agent_cfg.services.clone(),
            history: Vec::new(),
            messages: Vec::new(),
            mode: "doc".into(),
            apps: Vec::new(),
            permission: "workspace".into(),
            tools: true,
            vision: false,
            diff_confirm: false,
            always_allow: Vec::new(),
            web_allow: agent_cfg.web_allow.clone(),
            web_tier: agent_cfg.web_tier.clone(),
            web_search_provider: agent_cfg.web_search_provider.clone(),
            web_search_key: agent_cfg.web_search_key.clone(),
            subagents: false,
            subagent_write: false,
            mcp: Vec::new(),
            parallel_reads: agent_cfg.parallel_reads,
            retries: agent_cfg.retries,
            grep_ignore: agent_cfg.grep_ignore,
            plan_first: false,
            context_tokens: agent_cfg.context_tokens,
            result_cap: agent_cfg.result_cap,
            anthropic_stream: agent_cfg.anthropic_stream,
            mcp_enabled: false,
        };

        run_agent_collect(
            app,
            request_id,
            req,
            doc_dir,
            cancel,
            UPDATE_TURN_TIMEOUT,
            &turn_timeout_label(),
        )
        .await?
    };

    workspace::validate_changes(workspace, doc_dir)?;
    completed_summary(summary, false)
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
    tianji_agent: &Option<crate::wecom_orchestrator::WecomAgentConfig>,
    cancel: impl Fn() -> bool + Send + 'static,
) -> Result<String, String> {
    let user = "请尽快输出质量自检结论列表（优先摘要；最多抽查 3 个文件）。";
    let system = build_quality_prompt(quality_template, doc_dir, branch, changed, summary);
    let qc_label = quality_timeout_label();

    if engine.trim().eq_ignore_ascii_case("dsh") {
        return dsh_turn(
            app,
            workspace,
            doc_dir,
            DOCGEN_QC_SESSION,
            &system,
            user,
            cancel_requested,
            QUALITY_TURN_TIMEOUT,
            &qc_label,
        )
        .await;
    }

    let Some(agent_cfg) = tianji_agent.as_ref() else {
        return Err("文档任务未绑定独立天玑模型；请在「逆向工程 → 引擎提示词」选择模型后再运行".into());
    };
    let request_id = format!("wecom-docgen-qc-{}", chrono::Utc::now().timestamp_millis());
    let req = crate::agent::AgentRunRequest {
        config: agent_cfg.config.clone(),
        fallback_config: agent_cfg.fallback_config.clone(),
        system,
        user: user.into(),
        request_id: request_id.clone(),
        repo_path: workspace.to_string(),
        services: agent_cfg.services.clone(),
        history: Vec::new(),
        messages: Vec::new(),
        mode: "doc".into(),
        apps: Vec::new(),
        permission: "read".into(),
        tools: true,
        vision: false,
        diff_confirm: false,
        always_allow: Vec::new(),
        web_allow: agent_cfg.web_allow.clone(),
        web_tier: agent_cfg.web_tier.clone(),
        web_search_provider: agent_cfg.web_search_provider.clone(),
        web_search_key: agent_cfg.web_search_key.clone(),
        subagents: false,
        subagent_write: false,
        mcp: Vec::new(),
        parallel_reads: agent_cfg.parallel_reads,
        retries: agent_cfg.retries,
        grep_ignore: agent_cfg.grep_ignore,
        plan_first: false,
        context_tokens: agent_cfg.context_tokens,
        result_cap: agent_cfg.result_cap,
        anthropic_stream: agent_cfg.anthropic_stream,
        mcp_enabled: false,
    };
    run_agent_collect(
        app,
        request_id,
        req,
        doc_dir,
        cancel,
        QUALITY_TURN_TIMEOUT,
        &qc_label,
    )
    .await
}

/// DSH 档的会话键：每轮开始都重置，别让上下文一直滚。
const DOCGEN_SESSION: &str = "docgen";
const DOCGEN_QC_SESSION: &str = "docgen-qc";

/// 跑一轮 DSH 回合并把正文收成字符串，跑完就关进程（不常驻）。
///
/// 原仓文档目录 + 宿主工具闸门；只读质检降到 Read，任何权限升级请求均拒绝。
/// `timeout` / `timeout_label`：更新与质检用不同墙钟。
async fn dsh_turn(
    app: &AppHandle,
    workspace: &str,
    doc_dir: &str,
    session_key: &str,
    system: &str,
    user: &str,
    cancel: impl Fn() -> bool + Send + 'static,
    timeout: Duration,
    timeout_label: &str,
) -> Result<String, String> {
    let dsh = {
        let st = app.state::<WecomState>();
        std::sync::Arc::clone(&st.docgen_dsh)
    };
    // 工作区用这一轮解析出来的仓库路径（设置页填的可能是父目录）。
    dsh.configure_docgen(workspace.to_string(), doc_dir.to_string(), session_key == DOCGEN_QC_SESSION).await;
    let label = timeout_label.to_string();
    let result = match tokio::time::timeout(
        timeout,
        dsh_turn_inner(&dsh, session_key, system, user, cancel, timeout, timeout_label),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!("DSH 文档任务超过整轮 {label} 期限，未确认完成")),
    };
    dsh.stop().await;
    result
}

async fn dsh_turn_inner(
    dsh: &crate::wecom_dsh::DshEngine,
    session_key: &str,
    system: &str,
    user: &str,
    cancel: impl Fn() -> bool + Send + 'static,
    timeout: Duration,
    timeout_label: &str,
) -> Result<String, String> {
    dsh.reset_channel_session(session_key).await;
    dsh.ensure_ready().await?;
    dsh.verify_docgen_guard().await?;
    let session = dsh.acquire_session(session_key).await?;
    dsh.apply_selection(&session).await.map_err(|e| format!("文档任务模型选择失败，未继续：{e}"))?;
    let prompt = format!("{system}\n\n---\n\n{user}");
    let mut turn = dsh
        .prompt(&session, vec![crate::wecom_dsh::acp_text_block(&prompt)])
        .await?;

    // 审批请求代表需要超出当前沙箱；无人值守任务不自动升级权限。
    let mut acc = String::new();
    // 整轮共享一个期限（不是每个事件各刷一次），和天玑档的收流语义对齐。
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let event = match tokio::time::timeout_at(deadline, turn.next()).await {
            Ok(ev) => ev,
            Err(_) => {
                turn.cancel();
                crate::wecom_log::write(
                    "docgen",
                    &format!("dsh 回合超过 {timeout_label}，已取消"),
                );
                return Err(format!(
                    "DSH 文档回合超过 {timeout_label} 超时，产物未确认完成，未提交、未推进采集点"
                ));
            }
        };
        match event {
            _ if cancel() => {
                turn.cancel();
                return Err("DSH 文档回合已请求停止".into());
            }
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
                let (_, reject) = crate::wecom_dsh::permission_options(&params);
                turn.answer_permission(rpc_id, reject.as_deref());
                turn.cancel();
                return Err(format!("文档任务拒绝权限升级：{}", crate::wecom_dsh::permission_summary(&params)));
            }
            Some(crate::wecom_dsh::DshEvent::Done(Err(e))) => {
                return Err(crate::wecom_dsh::friendly_turn_error(&e));
            }
            Some(crate::wecom_dsh::DshEvent::Done(Ok(result))) => {
                let reason = result.get("stopReason").and_then(|v| v.as_str()).unwrap_or("");
                if reason != "end_turn" {
                    return Err(format!("DSH 回合未完整结束（stopReason={reason}）"));
                }
                break;
            }
            None => return Err("DSH 事件流提前关闭，未收到完成确认".into()),
        }
    }
    completed_summary(acc, false)
}

async fn run_agent_collect(
    app: &AppHandle,
    request_id: String,
    req: crate::agent::AgentRunRequest,
    doc_dir: &str,
    cancel: impl Fn() -> bool + Send + 'static,
    timeout: Duration,
    timeout_label: &str,
) -> Result<String, String> {
    let mut rx = crate::agent::subscribe_agent_bridge(&request_id);
    let deadline = tokio::time::Instant::now() + timeout;
    let cancel_id = request_id.clone();
    let collector = tauri::async_runtime::spawn(async move {
        let mut acc = String::new();
        loop {
            match rx.recv().await {
                Some(crate::agent::AgentBridgeEvent::Chunk(t)) => acc.push_str(&t),
                Some(crate::agent::AgentBridgeEvent::Done { limited }) => {
                    return completed_summary(acc, limited);
                }
                Some(crate::agent::AgentBridgeEvent::Err(e)) => return Err(e),
                Some(_) => {
                    if cancel() {
                        crate::cancellation::cancel_request(&cancel_id);
                        return Err("天玑文档回合已请求停止".into());
                    }
                }
                None => return Err("天玑事件流提前关闭，未收到完成确认".into()),
            }
        }
    });
    let label = timeout_label.to_string();
    let run = match tokio::time::timeout_at(
        deadline,
        crate::agent::agent_run_document(app.clone(), req, doc_dir.to_string()),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            crate::cancellation::cancel_request(&request_id);
            Err(format!("天玑文档回合超过整轮 {label} 期限，未确认完成"))
        }
    };
    crate::agent::unsubscribe_agent_bridge(&request_id);
    let summary = collector.await.map_err(|e| format!("文档收流失败：{e}"))?;
    run?;
    summary
}

fn should_advance_cursor(published: bool, failed: bool) -> bool {
    published && !failed
}

fn completed_summary(text: String, limited: bool) -> Result<String, String> {
    if limited {
        return Err("已达到执行上限，文档更新未完成".into());
    }
    if text.trim().is_empty() {
        return Err("引擎未返回文档更新结论".into());
    }
    Ok(text.trim().to_string())
}

fn is_outside_doc_dir(path: &str, doc_dir: &str) -> bool {
    let path = path.trim();
    if path.is_empty() {
        return false;
    }
    let dir = doc_dir.trim_end_matches('/');
    path != dir && !path.starts_with(&format!("{dir}/"))
}

fn is_code_reference_heading(line: &str) -> bool {
    let t = line.trim().to_lowercase();
    t.starts_with("## 代码引用")
        || t.starts_with("## 代码映射")
        || t.starts_with("## code reference")
        || t.starts_with("## code refs")
}

/// 群通知只给人读：丢掉各段 `## 代码引用` 正文，但保留后续批次摘要
///（多批时原文是「批1摘要 + 引用 + 批2摘要 + 引用」，不能一刀切到文末）。
fn strip_code_reference_section(text: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if is_code_reference_heading(trimmed) {
            skipping = true;
            continue;
        }
        if skipping
            && (trimmed.starts_with("### ")
                || (trimmed.starts_with("## ") && !is_code_reference_heading(trimmed)))
        {
            skipping = false;
        }
        if skipping {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out.trim_end().to_string()
}

/// 质量全文压成一行：有阻断数 / 通过；空串表示不展示。
fn summarize_quality(quality: &str) -> String {
    let q = quality.trim();
    if q.is_empty() {
        return String::new();
    }
    let blocking = q
        .lines()
        .filter(|l| l.contains("[阻断]"))
        .count();
    if blocking > 0 {
        format!("有 {blocking} 条阻断")
    } else {
        "通过".into()
    }
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
    let docs = {
        let stripped = strip_code_reference_section(updated_docs);
        if stripped.trim().is_empty() {
            "（无更新摘要）".to_string()
        } else {
            stripped.trim().to_string()
        }
    };
    let commit_line = if commit_note.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n**提交：**{}", commit_note.trim())
    };
    let quality_line = {
        let s = summarize_quality(quality);
        if s.is_empty() {
            String::new()
        } else {
            format!("\n\n**质量：**{s}")
        }
    };
    let title = if preview {
        "## 👀 逆向文档预览（已写本地文档，未提交）"
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
**本次更新：**\n{}{}{}{}",
        changed.len(),
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

/// 企微 / 钉钉的 markdown 都按字节算长度、上限 4096：超了不是截断而是整条拒收
/// （企微 errcode=40055 「markdown.content exceed max length」）。
/// 发送前按 UTF-8 字符边界裁一刀、尾部补截断说明；飞书 text 上限宽得多，不用裁。
fn clamp_markdown(kind: WebhookKind, markdown: &str) -> String {
    const NOTE: &str = "\n\n> （通知过长，已截断）";
    match kind {
        WebhookKind::Feishu => return markdown.to_string(),
        WebhookKind::Wecom | WebhookKind::DingTalk => {}
    }
    if markdown.len() <= 4096 {
        return markdown.to_string();
    }
    let mut end = 4096 - NOTE.len();
    while end > 0 && !markdown.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &markdown[..end], NOTE)
}

async fn send_webhook_kind(kind: WebhookKind, url: &str, markdown: &str) -> Result<(), String> {
    let markdown = clamp_markdown(kind, markdown);
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
    use chrono::TimeZone;

    #[test]
    fn merge_keeps_later_repo_metadata_when_diff_budget_is_exhausted() {
        let delta = |head: &str, file: &str, diff: &str| ChangeDelta {
            head: head.into(),
            files: vec![file.into()],
            commits: vec![format!("{head} change")],
            diff_excerpt: diff.into(),
            diff_truncated: false,
        };
        let merged = merge_change_deltas(
            &[
                ("first".into(), delta("aaa", "a.rs", &"x".repeat(100))),
                ("second".into(), delta("bbb", "b.rs", "second diff")),
            ],
            24,
        );
        assert_eq!(merged.files, ["first:a.rs", "second:b.rs"]);
        assert_eq!(merged.commits.len(), 2);
        assert_eq!(merged.head, "bbb");
        assert!(merged.diff_truncated);
        assert!(merged.diff_excerpt.chars().count() <= 24);
    }

    #[test]
    fn file_manifest_never_silently_drops_files_after_eighty() {
        let files: Vec<String> = (0..161).map(|n| format!("src/file{n}.rs")).collect();
        let manifest = format_file_list(&files);
        assert!(manifest.contains("src/file160.rs"));
        assert_eq!(manifest.lines().count(), files.len());
    }

    #[test]
    fn incomplete_generation_and_failed_publication_never_advance() {
        assert!(completed_summary("partial output".into(), true).is_err());
        assert!(completed_summary("  ".into(), false).is_err());
        assert_eq!(completed_summary(" done ".into(), false).unwrap(), "done");
        assert!(!should_advance_cursor(false, false)); // 预览 / 未提交
        assert!(!should_advance_cursor(true, true)); // 已提交但推送失败
        assert!(!should_advance_cursor(false, true));
        assert!(should_advance_cursor(true, false));
    }

    #[test]
    fn missed_cron_minute_is_retried_within_the_hour() {
        let cfg = DocgenConfig { cron: "*/15 * * * *".into(), enabled: true, workspace: "D:/repo".into(), ..Default::default() };
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 7, 10, 22, 0).single().unwrap();
        let last = "2026-11-07 10:00";
        let missed = missed_cron_minute(&cfg, Some(last), now).unwrap();
        assert_eq!(missed.as_deref(), Some("2026-11-07 10:15"));
        let next = next_run_at(&cfg, now).unwrap();
        assert_eq!(next.as_deref(), Some("2026-11-07 10:30"));
    }

    #[test]
    fn next_run_uses_real_daily_slot_after_the_time_passes() {
        let cfg = DocgenConfig { time: "09:30".into(), enabled: true, workspace: "D:/repo".into(), ..Default::default() };
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 7, 10, 0, 0).single().unwrap();
        assert_eq!(next_run_at(&cfg, now).unwrap().as_deref(), Some("2026-11-08 09:30"));
    }

    #[test]
    fn summary_references_only_accept_explicit_source_lines() {
        let refs = extract_summary_references("- `src/a.rs:LoginService` → `doc/login.md`\n- 自由文本不算引用\n- `docs/a.md:概述`", "D:/repo", "abcdef1234", "doc");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].source_path, "src/a.rs");
        assert_eq!(refs[0].symbol, "LoginService");
        assert_eq!(refs[0].doc_path, "doc/login.md");
        assert!(!refs.iter().any(|r| r.source_path == "自由文本不算引用"));
    }

    #[test]
    fn docgen_config_keeps_its_own_tianji_model() {
        let cfg = DocgenConfig {
            engine: "tianji".into(),
            ..Default::default()
        };
        assert!(cfg.tianji_agent.is_none());
        assert_eq!(cfg.engine, "tianji");
    }

    #[test]
    fn quality_gate_blocks_only_explicit_block_tags() {
        let untagged = parse_quality_verdict(
            "- 服务调用关系与代码矛盾\n- 引用失效：错误码 404\n- 文档过时：index\n- 建议再补一个示例",
        );
        assert!(
            untagged.blocking.is_empty(),
            "untagged 过时/矛盾 must not block: {:?}",
            untagged.blocking
        );
        assert!(untagged.findings.len() >= 3);

        let tagged = parse_quality_verdict(
            "- [阻断] 本批登录文档与代码矛盾\n- [建议] 文档过时：总索引\n- 未发现阻塞项",
        );
        assert_eq!(tagged.blocking.len(), 1);
        assert!(tagged.blocking[0].contains("[阻断]"));
        assert!(!tagged.blocking.iter().any(|s| s.contains("[建议]")));

        let clean = parse_quality_verdict("- 未发现过时或矛盾\n- [建议] 可选：增加部署说明");
        assert!(clean.blocking.is_empty());
    }

    #[test]
    fn format_quality_gate_error_trims_long_lists() {
        let many: Vec<String> = (0..8).map(|i| format!("[阻断] 问题{i} {}", "x".repeat(200))).collect();
        let err = format_quality_gate_error("整轮质检", &many);
        assert!(err.contains("整轮质检"));
        assert!(err.contains("另有 3 条"));
        assert!(err.chars().count() < many.join("；").chars().count());
    }

    #[test]
    fn cap_quality_files_dedups_and_truncates() {
        let files: Vec<String> = (0..10)
            .flat_map(|i| [format!("a/{i}.rs"), format!("a\\{i}.rs")])
            .collect();
        let capped = cap_quality_files(&files, 5);
        assert_eq!(capped.len(), 6); // 5 + truncation note
        assert!(capped.last().unwrap().contains("已截断"));
        assert!(capped.iter().filter(|f| !f.starts_with('…')).count() <= 5);
    }

    #[test]
    fn quality_timeout_is_shorter_than_update() {
        assert!(QUALITY_TURN_TIMEOUT_SECS < UPDATE_TURN_TIMEOUT_SECS);
        assert_eq!(UPDATE_TURN_TIMEOUT_SECS, 3_600);
        assert_eq!(QUALITY_TURN_TIMEOUT_SECS, 900);
        assert_eq!(MAX_FILES_PER_BATCH, 100);
        assert_eq!(turn_timeout_label(), "60 分钟");
        assert_eq!(quality_timeout_label(), "15 分钟");
    }

    #[test]
    fn split_delta_half_splits_large_batches() {
        let files: Vec<String> = (0..41).map(|i| format!("f{i}.rs")).collect();
        let d = ChangeDelta {
            head: "abc".into(),
            files: files.clone(),
            commits: vec!["c1".into()],
            diff_excerpt: "diff".into(),
            diff_truncated: true,
        };
        let (left, right) = split_delta_half(&d).expect("should split");
        assert_eq!(left.files.len() + right.files.len(), files.len());
        assert_eq!(left.files.len(), 20);
        assert_eq!(right.files.len(), 21);
        assert_eq!(left.commits, vec!["c1".to_string()]);
        assert!(right.commits.is_empty());
        assert!(left.diff_excerpt.contains("diff"));
        assert!(right.diff_excerpt.is_empty());
        let small = ChangeDelta {
            head: "abc".into(),
            files: (0..RESPLIT_MIN_FILES).map(|i| format!("s{i}.rs")).collect(),
            commits: Vec::new(),
            diff_excerpt: String::new(),
            diff_truncated: false,
        };
        assert!(split_delta_half(&small).is_none());
        assert!(is_turn_timeout_error("DSH 文档任务超过整轮 60 分钟期限，未确认完成"));
        assert!(is_turn_timeout_error("天玑文档回合超过整轮 60 分钟超时"));
        assert!(!is_turn_timeout_error("权限被拒绝"));
    }

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
    fn split_deltas_keeps_single_batch_under_budget() {
        let a = ChangeDelta {
            head: "a".into(),
            files: vec!["one.rs".into()],
            commits: vec!["c1".into()],
            diff_excerpt: "d".into(),
            diff_truncated: false,
        };
        let batches = split_deltas_for_budget(&[(r"D:\x\svc".into(), a)], 24_000, 24_000);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].files.len(), 1);
    }

    #[test]
    fn split_deltas_splits_by_repo_when_over_budget() {
        let mk = |name: &str, n: usize| ChangeDelta {
            head: name.into(),
            files: (0..n).map(|i| format!("{name}/f{i}.rs")).collect(),
            commits: vec![format!("{name} commit")],
            diff_excerpt: format!("diff {name}"),
            diff_truncated: false,
        };
        // 每仓约 40 个带前缀文件行，合并后远超 400 字符预算 → 至少 2 批
        let parts = [
            (r"D:\x\repo_a".into(), mk("a", 40)),
            (r"D:\x\repo_b".into(), mk("b", 40)),
        ];
        let batches = split_deltas_for_budget(&parts, 400, 24_000);
        assert!(batches.len() >= 2, "got {} batches", batches.len());
        for b in &batches {
            assert!(
                format_file_list(&b.files).chars().count() <= 400
                    || b.files.len() == 1,
                "batch list too long: {} chars / {} files",
                format_file_list(&b.files).chars().count(),
                b.files.len()
            );
        }
        let total_files: usize = batches.iter().map(|b| b.files.len()).sum();
        assert_eq!(total_files, 80);
    }

    #[test]
    fn split_deltas_chunks_single_repo_oversized_list() {
        let files: Vec<String> = (0..80).map(|i| format!("path/to/quite/long/file_name_{i}.rs")).collect();
        let d = ChangeDelta {
            head: "h".into(),
            files,
            commits: vec!["only once".into()],
            diff_excerpt: "big".into(),
            diff_truncated: false,
        };
        let batches = split_deltas_for_budget(&[(r"D:\x\big".into(), d)], 500, 24_000);
        assert!(batches.len() > 1, "expected file chunks, got {}", batches.len());
        assert!(batches[0].commits.iter().any(|c| c.contains("only once")));
        for b in batches.iter().skip(1) {
            assert!(b.commits.is_empty());
        }
        for b in &batches {
            let n = format_file_list(&b.files).chars().count();
            assert!(n <= 500 || b.files.len() == 1, "chars={n} files={}", b.files.len());
            assert!(
                b.files.len() <= MAX_FILES_PER_BATCH,
                "files={} > {}",
                b.files.len(),
                MAX_FILES_PER_BATCH
            );
        }
    }

    #[test]
    fn split_deltas_caps_file_count_even_under_char_budget() {
        // 短路径总字符远低于 24k，但超过 MAX_FILES_PER_BATCH 必须切开。
        let n_files = MAX_FILES_PER_BATCH * 2 + 10;
        let files: Vec<String> = (0..n_files).map(|i| format!("f{i}.rs")).collect();
        let d = ChangeDelta {
            head: "h".into(),
            files,
            commits: vec!["c".into()],
            diff_excerpt: "d".into(),
            diff_truncated: false,
        };
        let list_chars = format_file_list(&d.files).chars().count();
        assert!(list_chars < 24_000, "precondition: under char budget ({list_chars})");
        let batches = split_deltas_for_budget(&[(r"D:\x\many".into(), d)], 24_000, 24_000);
        assert!(batches.len() >= 3, "got {} batches", batches.len());
        let total: usize = batches.iter().map(|b| b.files.len()).sum();
        assert_eq!(total, n_files);
        for b in &batches {
            assert!(
                b.files.len() <= MAX_FILES_PER_BATCH,
                "batch has {} files",
                b.files.len()
            );
        }
        assert!(
            batches[0].commits.iter().any(|c| c.contains('c')),
            "commits={:?}",
            batches[0].commits
        );
    }

    #[test]
    fn turn_timeout_label_tracks_constant() {
        let label = turn_timeout_label();
        assert!(label.contains("分钟") || label.contains("秒"), "{label}");
        assert!(!label.contains("600"));
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
        assert!(n.contains("本次更新"));
        assert!(!n.contains("> a.rs"), "群通知不再罗列源码路径");
        assert!(!n.contains("本次代码变更"));
        assert!(n.contains("更新了 a 文档"));
        assert!(n.contains("已提交 1 篇文档"));
        assert!(n.contains("来源仓同步"));
    }

    #[test]
    fn notice_preview_title() {
        let n = compose_notice(
            "release",
            &["a.rs".into()],
            "摘要",
            "",
            "- [建议] 可补示例\n未发现过时或矛盾",
            true,
            "",
        );
        assert!(n.contains("预览"));
        assert!(n.contains("**质量：**通过"));
        assert!(!n.contains("可补示例"));
        assert!(!n.contains("质量自检"));
    }

    #[test]
    fn notice_strips_code_refs_but_mapping_keeps_them() {
        let full = "\
- 门户支持按模块打开文档\n\
- 触及文档：文档中心门户\n\
\n\
## 代码引用\n\
- `doc/index.html:docs-data` → `doc/文档中心门户.md`\n\
- `doc/index.html:mdRender` → `doc/文档中心门户.md`\n";
        let n = compose_notice(
            "release",
            &["doc/index.html".into()],
            full,
            "",
            "- [阻断] 模块文档与代码矛盾\n- [阻断] 日报编造了未出现模块",
            false,
            "",
        );
        assert!(n.contains("门户支持按模块打开文档"));
        assert!(n.contains("触及文档：文档中心门户"));
        assert!(!n.contains("## 代码引用"));
        assert!(!n.contains("docs-data"));
        assert!(!n.contains("mdRender"));
        assert!(n.contains("**质量：**有 2 条阻断"));
        let refs = extract_summary_references(full, "demo", "abc", "doc");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].symbol, "docs-data");
    }

    #[test]
    fn strip_code_reference_section_cuts_at_heading() {
        let s = strip_code_reference_section("业务要点\n\n## 代码引用\n- a:b → doc/x.md");
        assert_eq!(s, "业务要点");
        assert_eq!(strip_code_reference_section("只有摘要"), "只有摘要");
    }

    #[test]
    fn strip_code_reference_keeps_later_batches() {
        let full = "\
### 第 1/2 批\n\
- 第一批业务要点\n\
\n\
## 代码引用\n\
- `a.rs:Foo` → `doc/a.md`\n\
\n\
### 第 2/2 批\n\
- 第二批业务要点\n\
\n\
## 代码引用\n\
- `b.rs:Bar` → `doc/b.md`\n";
        let s = strip_code_reference_section(full);
        assert!(s.contains("### 第 1/2 批"));
        assert!(s.contains("第一批业务要点"));
        assert!(s.contains("### 第 2/2 批"));
        assert!(s.contains("第二批业务要点"));
        assert!(!s.contains("## 代码引用"));
        assert!(!s.contains("a.rs:Foo"));
        assert!(!s.contains("b.rs:Bar"));
        let n = compose_notice("release", &["a.rs".into(), "b.rs".into()], full, "预览模式：已直写文档目录（2 批）", "", true, "");
        assert!(n.contains("第二批业务要点"));
        assert!(n.contains("第 2/2 批"));
        assert_eq!(extract_summary_references(full, "demo", "abc", "doc").len(), 2);
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
    fn normalize_doc_dir_handles_absolute_and_backslash() {
        let ws = r"D:\repo";
        // 绝对路径 + 反斜杠：剥掉 workspace 前缀、统一正斜杠
        assert_eq!(normalize_doc_dir(ws, r"D:\repo\doc").unwrap(), "doc");
        assert_eq!(
            normalize_doc_dir(r"D:\d_project\tc\prod", r"D:\d_project\tc\prod\pmys.saas.parent.pom\doc").unwrap(),
            "pmys.saas.parent.pom/doc"
        );
        assert_eq!(normalize_doc_dir(ws, "D:/repo/doc/sub").unwrap(), "doc/sub");
        assert_eq!(normalize_doc_dir(ws, "d:/REPO/Doc").unwrap(), "Doc");
        // 相对路径反斜杠 → 正斜杠；正斜杠原样
        assert_eq!(normalize_doc_dir(ws, r"doc\sub").unwrap(), "doc/sub");
        assert_eq!(normalize_doc_dir(ws, "doc/sub/").unwrap(), "doc/sub");
        // 空值回落默认；指向 workspace 本身视为 doc
        assert_eq!(normalize_doc_dir(ws, "  ").unwrap(), "doc");
        assert_eq!(normalize_doc_dir(ws, r"D:\repo").unwrap(), "doc");
        // canonicalize 后的 `\\?\` 工作区也能剥前缀对齐手填绝对路径
        assert_eq!(
            normalize_doc_dir(r"\\?\D:\repo", r"D:\repo\doc").unwrap(),
            "doc"
        );
        assert_eq!(
            normalize_doc_dir(r"\\?\D:\d_project\tc\prod\pmys.saas.parent.pom", r"D:/d_project/tc/prod/pmys.saas.parent.pom/doc").unwrap(),
            "doc"
        );
        // 工作区之外的绝对路径直接报错
        assert!(normalize_doc_dir(ws, r"E:\elsewhere\doc").is_err());
        assert!(normalize_doc_dir(ws, "../outside").is_err());
        assert!(normalize_doc_dir(ws, "D:/repository/doc").is_err());
        assert!(normalize_doc_dir(ws, "doc/.git/config").is_err());
    }

    #[test]
    fn fill_docgen_template_replaces_placeholders() {
        let out = fill_docgen_template(
            "dir={doc_dir} br={branch} x={files} d={date} u={updates_dir}",
            &[
                ("doc_dir", "doc"),
                ("branch", "release"),
                ("files", "- a.rs"),
                ("date", "2026-11-08"),
                ("updates_dir", "doc/updates"),
            ],
        );
        assert_eq!(out, "dir=doc br=release x=- a.rs d=2026-11-08 u=doc/updates");
    }

    #[test]
    fn updates_dir_rel_joins_doc_dir() {
        assert_eq!(updates_dir_rel("doc"), "doc/updates");
        assert_eq!(updates_dir_rel("doc/"), "doc/updates");
        assert_eq!(updates_dir_rel("reverse"), "reverse/updates");
        assert_eq!(updates_dir_rel(""), "doc/updates");
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
        let p = build_docgen_prompt("", "doc", "release", &delta, "D:/repo");
        assert!(p.contains("提交摘要"));
        assert!(p.contains("deadbeef fix login"));
        assert!(p.contains("变更 diff 摘录") || p.contains("diff"));
        assert!(p.contains("已截断"));
        assert!(p.contains("`doc/`") || p.contains("doc/"));
        assert!(p.contains("doc/updates"));
        assert!(p.contains("功能更新") || p.contains("当日功能更新"));
        assert!(!p.contains("{date}"));
        assert!(!p.contains("{updates_dir}"));
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
            "只看 {files} 改 {doc_dir} @ {branch} day={date} upd={updates_dir}",
            "docs",
            "main",
            &delta,
            "D:/repo",
        );
        assert!(p.contains("- x.rs"));
        assert!(p.contains("docs"));
        assert!(p.contains("main"));
        assert!(p.contains("docs/updates"));
        assert!(!p.contains("{files}"));
        assert!(!p.contains("{date}"));
        assert!(!p.contains("{updates_dir}"));
    }

    #[test]
    fn build_quality_prompt_injects_summary() {
        let p = build_quality_prompt("", "doc", "release", &["a.rs".into()], "已更新 a");
        assert!(p.contains("已更新 a"));
        assert!(p.contains("- a.rs"));
        assert!(p.contains("doc/updates"));
        assert!(!p.contains("{summary}"));
        assert!(!p.contains("{date}"));
        assert!(!p.contains("{updates_dir}"));
    }

    #[test]
    fn default_update_prompt_pins_reference_arrow_contract() {
        let p = default_docgen_system_prompt();
        assert!(p.contains("→"), "code refs must use arrow for extract_summary_references");
        assert!(p.contains("`源码路径:符号` → `文档相对路径`") || p.contains("源码路径:符号"));
        assert!(p.contains("面向业务"));
        assert!(p.contains("仅供系统映射落库"));
        assert!(p.contains("证据与写法"));
        assert!(p.contains("无行为变更"));
        assert!(p.contains("不要通读全库"));
        assert!(p.contains("优先完成摘要与落盘"));
    }

    #[test]
    fn default_quality_prompt_pins_batch_scope() {
        let p = default_docgen_quality_prompt();
        assert!(p.contains("本轮"));
        assert!(p.contains("抽查 3"));
        assert!(p.contains("吃不准"));
        assert!(p.contains("[阻断]"));
        assert!(p.contains("[建议]"));
        assert!(p.contains("未发现过时或矛盾"));
        let blocked = parse_quality_verdict("- [阻断] 模块文档与代码矛盾\n- [建议] 可补部署说明");
        assert_eq!(blocked.blocking.len(), 1);
        assert!(blocked.blocking[0].contains("矛盾"));
        let clean = parse_quality_verdict("- 未发现过时或矛盾\n- [建议] 可选：加示例");
        assert!(clean.blocking.is_empty());
    }

    #[test]
    fn ensure_daily_update_creates_and_dedups() {
        let root = std::env::temp_dir().join(format!(
            "docgen-daily-{}",
            chrono::Utc::now().timestamp_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let ws = root.to_string_lossy().to_string();
        let summary = "更新了登录模块文档，对应本次鉴权改动。\n## 代码引用\n`auth/login.rs:LoginService`";
        let path = ensure_daily_update(
            &ws,
            "doc",
            "2026-11-08",
            "12:00 · release",
            summary,
        )
        .expect("create daily");
        assert!(path.exists());
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("# 2026-11-08 功能更新"));
        assert!(first.contains("系统补记"));
        assert!(first.contains("登录模块"));

        ensure_daily_update(&ws, "doc", "2026-11-08", "12:01 · release", summary)
            .expect("dedup");
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            second.matches("系统补记").count(),
            1,
            "same summary must not append twice"
        );

        let other = "另一批：支付回调文档已同步。\n## 代码引用\n`pay/cb.rs:onPay`";
        ensure_daily_update(&ws, "doc", "2026-11-08", "13:00 · release", other)
            .expect("append other");
        let third = std::fs::read_to_string(&path).unwrap();
        assert_eq!(third.matches("系统补记").count(), 2);
        assert!(third.contains("支付回调"));

        // 代理已写入实质内容时，不再系统补记
        let agent_line = "功能点：新增发票导出；涉及模块文档：`doc/invoice.md`";
        let mut agent_file = format!("# 2026-11-09 功能更新\n\n## 09:00 · release\n\n- {agent_line}\n");
        let agent_path = Path::new(&ws).join("doc").join("updates").join("2026-11-09.md");
        std::fs::write(&agent_path, &agent_file).unwrap();
        ensure_daily_update(&ws, "doc", "2026-11-09", "09:05 · release", &agent_line)
            .expect("agent covered");
        agent_file = std::fs::read_to_string(&agent_path).unwrap();
        assert!(!agent_file.contains("系统补记"));
        assert_eq!(agent_file.matches("发票导出").count(), 1);

        let _ = std::fs::remove_dir_all(&root);
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
