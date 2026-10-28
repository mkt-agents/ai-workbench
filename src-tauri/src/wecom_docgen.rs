//! 逆向文档的每日自动更新 + 企微群通知。
//!
//! 每天到点（默认 23:00）：
//! 1. fetch 最新 release 分支（只读安全：脏工作区则跳过，不强制 reset）；
//! 2. 取自上次更新以来的新提交与改动文件；无新提交则不改文档、不推送；
//! 3. 用 `agent_run`（编辑档、限定工作区）按变更更新 `doc/` 下逆向文档；
//! 4. 把「更新了哪些文档 + 关键变更」POST 到企微群机器人 webhook。
//!
//! 调度复用 `daily_schedule` 的 `due_today`（补跑漏跑 + 防重复），last-run 落
//! `%APPDATA%\com.ai-workbench.app\wecom-dsh\docgen.json`。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::wecom_channel::WecomState;

/// 一次 webhook 发送的超时。
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(15);
/// 自动更新回合的看门狗（文档更新可能跑多步工具）。
const UPDATE_TURN_TIMEOUT: Duration = Duration::from_secs(600);

/// 同一时刻只允许一个更新流程（自动 tick 与「立即更新」按钮不会并跑，避免同时动 git / 游标）。
static RUNNING: AtomicBool = AtomicBool::new(false);

/// 进入更新临界区；退出时自动释放。`None` = 已有一个在跑。
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
    /// `HH:MM`，空则按 23:00。
    pub time: String,
    /// release 分支名，空则 `release`。
    pub release_branch: String,
    /// 工作区绝对路径（= 问答/文档所在仓库）。
    pub workspace: String,
    /// 逆向文档目录（相对工作区），空则 `doc`。
    pub doc_dir: String,
    /// 企微群 webhook URL（明文，已在 apply 时解密）。
    pub webhook_url: String,
    /// 文档更新后是否自动 commit（默认开）。关掉则工作区留未提交改动，
    /// 下次自动更新会因「脏工作区」跳过——除非你手动提交。
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    /// commit 后是否 push 到远端（默认关，push 需要远端凭证）。
    #[serde(default)]
    pub auto_push: bool,
}

fn default_true() -> bool {
    true
}

impl Default for DocgenConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            time: String::new(),
            release_branch: String::new(),
            workspace: String::new(),
            doc_dir: String::new(),
            webhook_url: String::new(),
            auto_commit: true,
            auto_push: false,
        }
    }
}

impl DocgenConfig {
    pub fn time_or_default(&self) -> String {
        let t = self.time.trim();
        if t.is_empty() { "23:00".into() } else { t.into() }
    }
    pub fn branch_or_default(&self) -> String {
        let b = self.release_branch.trim();
        if b.is_empty() { "release".into() } else { b.into() }
    }
    pub fn doc_dir_or_default(&self) -> String {
        let d = self.doc_dir.trim();
        if d.is_empty() { "doc".into() } else { d.into() }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RunState {
    /// 上次成功跑的日期（`YYYY-MM-DD`），防一天重复跑。
    last_run_date: Option<String>,
    /// 上次更新停在哪个提交（下次从这里取增量）。
    last_commit: Option<String>,
}

fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = base.join("com.ai-workbench.app").join("wecom-dsh");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("docgen.json"))
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

/// 由 `daily_schedule::spawn` 每分钟调用：到点且今天没跑过就跑一轮。
pub async fn auto_tick(app: &AppHandle) -> Result<(), String> {
    let cfg = {
        let st = app.state::<WecomState>();
        let g = st.docgen.lock().await;
        g.clone()
    };
    if !cfg.enabled || cfg.workspace.trim().is_empty() {
        return Ok(());
    }
    let state = load_state();
    let due = crate::daily_schedule::due_today(
        &cfg.time_or_default(),
        state.last_run_date.as_deref(),
        chrono::Local::now(),
    );
    let Some(today) = due else {
        return Ok(());
    };
    // 先占日期，避免这一分钟内重复触发。
    let mut st = load_state();
    st.last_run_date = Some(today);
    save_state(&st);

    crate::wecom_log::write("docgen", &format!("auto run start branch={}", cfg.branch_or_default()));
    match run_update(app, &cfg).await {
        Ok(report) => {
            crate::wecom_log::write("docgen", &format!("auto run done: {}", report.summary_line()));
            Ok(())
        }
        Err(e) => {
            crate::wecom_log::write("docgen", &format!("auto run failed: {e}"));
            // 失败也尝试给群里一个提示，便于值班知道没更新成功。
            let _ = send_webhook(&cfg.webhook_url, &format!("⚠️ 逆向文档自动更新失败：{e}")).await;
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
    pub pushed: bool,
    pub skipped_reason: Option<String>,
}

impl UpdateReport {
    fn summary_line(&self) -> String {
        if let Some(r) = &self.skipped_reason {
            return format!("skipped: {r}");
        }
        format!(
            "branch={} commits={} files={} pushed={}",
            self.branch,
            self.new_commits,
            self.changed_files.len(),
            self.pushed
        )
    }
}

/// 执行一次更新（供自动 tick 与「立即更新一次」命令共用）。
pub async fn run_update(app: &AppHandle, cfg: &DocgenConfig) -> Result<UpdateReport, String> {
    // 并发锁：自动 tick 与手动按钮不会同时动 git / last_commit 游标。
    let Some(_guard) = RunGuard::acquire() else {
        return Err("已有一个文档更新流程在跑，请稍后再试".into());
    };
    let workspace = cfg.workspace.trim().to_string();
    let branch = cfg.branch_or_default();
    let doc_dir = cfg.doc_dir_or_default();

    crate::git_commands::ensure_git_repo(&workspace)?;

    // 1) 脏工作区就不动它（避免把用户未提交的改动卷进自动流程）。
    let dirty = {
        let ws = workspace.clone();
        tokio::task::spawn_blocking(move || crate::git_commands::git_output(&ws, &["status", "--porcelain"]))
            .await
            .map_err(|e| e.to_string())??
    };
    if !dirty.status.success() {
        return Err("读取工作区状态失败".into());
    }
    if !String::from_utf8_lossy(&dirty.stdout).trim().is_empty() {
        let reason = "工作区有未提交改动，已跳过自动更新（请先提交或暂存）".to_string();
        let _ = send_webhook(&cfg.webhook_url, &format!("⚠️ 逆向文档自动更新已跳过：{reason}")).await;
        return Ok(UpdateReport {
            branch,
            new_commits: 0,
            changed_files: Vec::new(),
            updated_docs: String::new(),
            pushed: false,
            skipped_reason: Some(reason),
        });
    }

    // 2) 切到 release 分支并拉取最新（只读网络 + 本地切换，无 reset --hard）。
    {
        let ws = workspace.clone();
        let br = branch.clone();
        tokio::task::spawn_blocking(move || update_release_branch(&ws, &br))
            .await
            .map_err(|e| e.to_string())??;
    }

    // 3) 取增量：上次 last_commit..HEAD 的改动文件；首跑则取最近一段。
    let prev = load_state().last_commit;
    let (head, changed_files) = {
        let ws = workspace.clone();
        let prev2 = prev.clone();
        tokio::task::spawn_blocking(move || changed_since(&ws, prev2.as_deref()))
            .await
            .map_err(|e| e.to_string())??
    };

    if changed_files.is_empty() {
        // 无代码变更：不改文档、不推送，只推进 last_commit 游标。
        let mut st = load_state();
        st.last_commit = Some(head);
        save_state(&st);
        return Ok(UpdateReport {
            branch,
            new_commits: 0,
            changed_files: Vec::new(),
            updated_docs: String::new(),
            pushed: false,
            skipped_reason: Some("自上次更新以来无代码变更".into()),
        });
    }

    // 4) 让 AI 按变更更新 doc 目录下的逆向文档。
    let updated_docs = update_docs_with_agent(app, &workspace, &doc_dir, &branch, &changed_files)
        .await
        .unwrap_or_else(|e| format!("（文档更新步骤出错：{e}）"));

    // 5) 提交闭环：改完若不提交，工作区留未提交改动 → 下次自动更新会因「脏工作区」跳过，
    //    于是第二天起自动更新就哑了。所以默认把 doc_dir 的改动提交掉（可选 push）。
    let mut commit_note = String::new();
    if cfg.auto_commit {
        let ws = workspace.clone();
        let dd = doc_dir.clone();
        let br = branch.clone();
        let push = cfg.auto_push;
        let n = changed_files.len();
        commit_note = tokio::task::spawn_blocking(move || commit_docs(&ws, &dd, &br, n, push))
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|e| format!("（文档提交失败：{e}）"));
    }

    // 6) 推送群通知。
    let body = compose_notice(&branch, &changed_files, &updated_docs, &commit_note);
    let pushed = send_webhook(&cfg.webhook_url, &body).await.is_ok();

    // 游标推进（无论推送成功与否，文档已按这批变更更新过）。
    let mut st = load_state();
    st.last_commit = Some(head);
    save_state(&st);

    Ok(UpdateReport {
        branch,
        new_commits: changed_files.len(),
        changed_files,
        updated_docs,
        pushed,
        skipped_reason: None,
    })
}

/// 提交 doc_dir 下的改动（只 add 文档目录，不碰其它）；可选 push。
/// 返回一句人类可读的提交结果；没有改动则说明「文档无改动」。
fn commit_docs(
    workspace: &str,
    doc_dir: &str,
    branch: &str,
    code_changes: usize,
    push: bool,
) -> Result<String, String> {
    use crate::git_commands::git_output;
    let dir = doc_dir.trim_end_matches('/');
    // 只暂存文档目录；AI 若在目录外留了改动，前面的 revert_outside 已清掉。
    let add = git_output(workspace, &["add", "--", dir])?;
    if !add.status.success() {
        return Err(format!(
            "git add {dir} 失败：{}",
            String::from_utf8_lossy(&add.stderr).trim()
        ));
    }
    // 没有已暂存改动就不提交（AI 这轮没动文档）。
    let staged = git_output(workspace, &["diff", "--cached", "--name-only", "--", dir])?;
    let staged_list = String::from_utf8_lossy(&staged.stdout);
    let changed_docs: Vec<&str> = staged_list.lines().filter(|l| !l.trim().is_empty()).collect();
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

/// fetch + 切到 release 分支 + 快进拉取（阻塞，内部复用 git_commands 的底层 runner）。
fn update_release_branch(workspace: &str, branch: &str) -> Result<(), String> {
    use crate::git_commands::{git_output, git_stdout};
    // 远端最新
    let fetch = git_output(workspace, &["fetch", "origin", branch])?;
    if !fetch.status.success() {
        return Err(format!(
            "fetch origin {branch} 失败：{}",
            String::from_utf8_lossy(&fetch.stderr).trim()
        ));
    }
    // 切分支（已在则 no-op）
    let cur = git_stdout(workspace, &["symbolic-ref", "--short", "-q", "HEAD"]).unwrap_or_default();
    if cur != branch {
        let co = git_output(workspace, &["checkout", branch])?;
        if !co.status.success() {
            // 本地没有该分支时按远端跟踪建一个
            let co2 = git_output(workspace, &["checkout", "-B", branch, &format!("origin/{branch}")])?;
            if !co2.status.success() {
                return Err(format!(
                    "切换到 {branch} 失败：{}",
                    String::from_utf8_lossy(&co2.stderr).trim()
                ));
            }
        }
    }
    // 快进到远端（--ff-only：有分叉宁可失败也不自动 merge）
    let pull = git_output(workspace, &["merge", "--ff-only", &format!("origin/{branch}")])?;
    if !pull.status.success() {
        return Err(format!(
            "快进到 origin/{branch} 失败（可能有本地提交分叉）：{}",
            String::from_utf8_lossy(&pull.stderr).trim()
        ));
    }
    Ok(())
}

/// 取 `prev..HEAD` 改动的文件列表与新 HEAD。prev 为空（首跑）时取最近 1 个提交的改动，
/// 避免首跑就把整库文件喂给 AI。
fn changed_since(workspace: &str, prev: Option<&str>) -> Result<(String, Vec<String>), String> {
    use crate::git_commands::{git_output, git_stdout};
    let head = git_stdout(workspace, &["rev-parse", "HEAD"])?;
    let range = match prev {
        Some(p) if !p.trim().is_empty() => format!("{}..HEAD", p.trim()),
        _ => "HEAD~1..HEAD".to_string(),
    };
    let out = git_output(workspace, &["diff", "--name-only", &range])?;
    if !out.status.success() {
        // prev 不在历史里（force-push / 换分支）：回退为最近一个提交
        let out2 = git_output(workspace, &["diff", "--name-only", "HEAD~1..HEAD"])?;
        if !out2.status.success() {
            return Ok((head, Vec::new()));
        }
        let files = parse_names(&String::from_utf8_lossy(&out2.stdout));
        return Ok((head, files));
    }
    let files = parse_names(&String::from_utf8_lossy(&out.stdout));
    Ok((head, files))
}

fn parse_names(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// 让天玑按变更更新 doc 目录（编辑档，工作区边界内；只允许写 doc_dir 由提示词约束 + 受保护路径兜底）。
async fn update_docs_with_agent(
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
        return Err("企微通道未绑定模型，无法自动更新文档".into());
    };

    let file_list = changed
        .iter()
        .take(80)
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    let request_id = format!("wecom-docgen-{}", chrono::Utc::now().timestamp_millis());
    let system = format!(
        "你是代码逆向文档维护助手。工作区是一个代码仓库，`{doc_dir}/` 下是基于代码的逆向工程文档。\n\
当前在 release 分支「{branch}」。本次有以下代码文件发生变更：\n{file_list}\n\n\
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
    );

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
        // 编辑档：能写工作区内文件；命令/删除/系统仍需审批（但自动流程无人应答，故靠提示词只写 doc）。
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

    // 收尾摘要：订阅 bridge 收最终文本。
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
                        acc = format!("（更新出错：{e}）");
                    }
                    break;
                }
                Ok(Some(_)) => {}
                Err(_) => {
                    acc.push_str("\n（更新超时，已收尾）");
                    crate::cancellation::cancel_request(&cancel_id);
                    break;
                }
            }
        }
        acc
    });

    let run = crate::agent::agent_run(app.clone(), req).await;
    crate::agent::unsubscribe_agent_bridge(&request_id);
    let mut summary = collector.await.unwrap_or_default();
    run?;

    // 硬边界：无论提示词怎么说，把 doc_dir 之外的改动一律回退，确保自动流程只动文档。
    let reverted = {
        let ws = workspace.to_string();
        let dd = doc_dir.to_string();
        tokio::task::spawn_blocking(move || revert_outside(&ws, &dd))
            .await
            .map_err(|e| e.to_string())?
    };
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

/// 把工作区里 `doc_dir/` 之外的改动全部回退：已跟踪文件 `checkout`，未跟踪文件删除。
/// 返回被回退的路径列表（用于日志与摘要）。只读不到工作区状态时返回空（不阻断更新）。
fn revert_outside(workspace: &str, doc_dir: &str) -> Vec<String> {
    use crate::git_commands::git_output;
    let status = match git_output(workspace, &["status", "--porcelain", "-z", "--untracked-files=all"]) {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&status);
    let mut reverted = Vec::new();
    // `-z` 用 NUL 分隔；每条记录形如 "XY path"（重命名有第二段，这里保守只看主路径）。
    for entry in text.split('\u{0}').filter(|e| !e.is_empty()) {
        if entry.len() < 3 {
            continue;
        }
        let xy = &entry[..2];
        let path = entry[3..].to_string();
        if !is_outside_doc_dir(&path, doc_dir) {
            continue; // 文档目录内的改动是允许的
        }
        let untracked = xy == "??";
        if untracked {
            let full = std::path::Path::new(workspace).join(&path);
            let _ = std::fs::remove_file(&full);
        } else {
            // 先从暂存区撤出再还原工作区，覆盖 AI 可能 git add 过的情况。
            let _ = git_output(workspace, &["reset", "-q", "--", &path]);
            let _ = git_output(workspace, &["checkout", "--", &path]);
        }
        reverted.push(path);
    }
    reverted
}

/// 这条改动路径是否在文档目录之外（需要回退）。
fn is_outside_doc_dir(path: &str, doc_dir: &str) -> bool {
    let path = path.trim();
    if path.is_empty() {
        return false;
    }
    let dir = doc_dir.trim_end_matches('/');
    path != dir && !path.starts_with(&format!("{dir}/"))
}

/// 组一条群通知的 markdown。
fn compose_notice(branch: &str, changed: &[String], updated_docs: &str, commit_note: &str) -> String {
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
    format!(
        "## 📘 逆向文档已更新\n\
**分支**：`{branch}`　**变更文件**：{} 个\n\n\
**本次代码变更：**\n{}{}\n\n\
**文档更新摘要：**\n{}{}",
        changed.len(),
        shown.join("\n"),
        more,
        docs,
        commit_line
    )
}

/// POST 一条 markdown 到企微群机器人 webhook。`url` 可以是完整 webhook URL，也可以只是 key。
pub async fn send_webhook(url: &str, markdown: &str) -> Result<(), String> {
    let url = normalize_webhook_url(url)?;
    let payload = serde_json::json!({
        "msgtype": "markdown",
        "markdown": { "content": markdown }
    });
    let resp = reqwest::Client::new()
        .post(&url)
        .timeout(WEBHOOK_TIMEOUT)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("webhook 发送失败：{e}"))?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("webhook 响应解析失败：{e}"))?;
    match v.get("errcode").and_then(|c| c.as_i64()) {
        Some(0) | None => Ok(()),
        Some(code) => Err(format!(
            "webhook 返回错误 errcode={code} errmsg={}",
            v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("")
        )),
    }
}

/// 允许用户只填 key：补成完整 webhook URL。已是完整 URL 则原样用。
fn normalize_webhook_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("未配置群 webhook".into());
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        Ok(raw.to_string())
    } else {
        // 只给了 key
        Ok(format!(
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key={raw}"
        ))
    }
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
        let n = compose_notice("release", &["a.rs".into(), "b.rs".into()], "更新了 a 文档", "已提交 1 篇文档（未推送）");
        assert!(n.contains("`release`"));
        assert!(n.contains("2 个"));
        assert!(n.contains("> a.rs"));
        assert!(n.contains("更新了 a 文档"));
        assert!(n.contains("已提交 1 篇文档"));
    }

    #[test]
    fn parse_names_trims_and_drops_blanks() {
        let v = parse_names("a.rs\n\n  b/c.rs \n");
        assert_eq!(v, vec!["a.rs", "b/c.rs"]);
    }

    #[test]
    fn outside_doc_dir_detection() {
        assert!(is_outside_doc_dir("src/main.rs", "doc"));
        assert!(is_outside_doc_dir("README.md", "doc"));
        assert!(!is_outside_doc_dir("doc/a.md", "doc"));
        assert!(!is_outside_doc_dir("doc/sub/b.md", "doc"));
        // 前缀相近但不同目录要算在外面
        assert!(is_outside_doc_dir("docs/a.md", "doc"));
        assert!(is_outside_doc_dir("documentation/a.md", "doc"));
        // 自定义文档目录 + 末尾斜杠
        assert!(!is_outside_doc_dir("reverse/a.md", "reverse/"));
        assert!(is_outside_doc_dir("", "doc") == false);
    }
}
