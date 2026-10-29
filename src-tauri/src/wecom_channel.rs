//! 企微智能机器人通道：生命周期、Tauri 命令、与设置页对接。

use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{oneshot, Mutex};

use crate::wecom_dsh::{DshEngine, DshStatus, Tier};
use crate::wecom_orchestrator::{Orchestrator, WecomAgentConfig};
use crate::wecom_policy::{DmPolicy, GroupPolicy, PendingPair, PolicyState};
use crate::wecom_ws::WecomSocket;

/// 待决审批的来源：两个引擎的回执通道完全不同。
pub enum ApprovalKind {
    /// 天玑：回灌 `agent_decide`。
    Tianji {
        request_id: String,
        call_ids: Vec<String>,
    },
    /// DSH：只留一句摘要，真正的回执由等待方（握有 ACP 请求 id）完成。
    Dsh { summary: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalOwner {
    pub bot: String,
    pub chat: String,
    pub user: String,
    pub session: String,
}

impl ApprovalOwner {
    pub fn matches(&self, other: &Self) -> bool {
        !self.bot.is_empty()
            && !self.user.is_empty()
            && !self.chat.is_empty()
            && !self.session.is_empty()
            && self == other
    }
}

/// 一次企微审批待决项：用户用聊天里的「审批码」回复来放行/拒绝。
pub struct PendingApproval {
    pub kind: ApprovalKind,
    pub owner: ApprovalOwner,
    pub request_id: String,
    /// 天玑档才有意义：「本轮同类都允许」；DSH 只有一次性授权。
    /// 目前提示文案在 emit 时就已经写好，这里只留档备查。
    #[allow(dead_code)]
    pub can_remember: bool,
    pub created: Instant,
    /// DSH 档的等待端：用户作答后从这里回传 allow/deny。
    pub reply: Option<oneshot::Sender<bool>>,
}

/// 当前回合的企微出站目标（用于把媒体发回同一条对话流）。
pub struct OutTarget {
    pub req_id: String,
    pub stream_id: String,
}

pub struct WecomState {
    pub sock: Arc<WecomSocket>,
    pub orch: Arc<Orchestrator>,
    /// 上次成功 apply 的 bot 凭据指纹，避免无变化反复踢线
    pub applied: Mutex<Option<AppliedCreds>>,
    /// 业务 API（文档/日程/智能表格）的 access_token 缓存，避免每调一次都换 token
    pub api_token: Mutex<Option<(String, Instant)>>,
    /// 企微内审批：审批码 -> 待决项
    pub pending_approvals: Mutex<HashMap<String, PendingApproval>>,
    /// 自增审批码序号
    pub approval_seq: AtomicU32,
    /// 当前回合的出站目标（wecom_send_image/file 工具用）
    pub out_targets: Mutex<HashMap<String, OutTarget>>,
    pub approval_owners: Mutex<HashMap<String, ApprovalOwner>>,
    /// DSH 引擎（常驻 `dsh --profile acp` 子进程）；天玑档时保持 Stopped。
    pub dsh: Arc<DshEngine>,
    /// SkyWalking 排障：`(OAP 地址, 可选 "Key: Value" header)`，空地址 = 不下发 skywalking 工具。
    pub skywalking: Mutex<(String, String)>,
    pub skywalking_timezone: Mutex<Option<i32>>,
    /// 逆向文档自动更新 + 群通知的配置快照（设置页下发）。
    pub docgen: Mutex<crate::wecom_docgen::DocgenConfig>,
}

/// 审批码有效期（与 harness 的 APPROVAL_WAIT 对齐）
pub const APPROVAL_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, PartialEq, Eq)]
pub struct AppliedCreds {
    pub bot_id: String,
    pub secret: String,
    /// 业务 API（文档/日程/智能表格）用的自建应用凭证；None = 未配置
    pub corp_id: Option<String>,
    pub corp_secret: Option<String>,
}

impl WecomState {
    pub fn new() -> Self {
        let sock = Arc::new(WecomSocket::new());
        let orch = Arc::new(Orchestrator::new(Arc::clone(&sock)));
        Self {
            sock,
            orch,
            applied: Mutex::new(None),
            api_token: Mutex::new(None),
            pending_approvals: Mutex::new(HashMap::new()),
            approval_seq: AtomicU32::new(0),
            out_targets: Mutex::new(HashMap::new()),
            approval_owners: Mutex::new(HashMap::new()),
            dsh: Arc::new(DshEngine::new()),
            skywalking: Mutex::new((String::new(), String::new())),
            skywalking_timezone: Mutex::new(None),
            docgen: Mutex::new(crate::wecom_docgen::DocgenConfig::default()),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomApplyRequest {
    pub enabled: bool,
    pub bot_id: String,
    /// 明文 Secret（前端在保存时经 protect 存档；apply 时传解密后的明文或仍带 enc:v1:）
    pub bot_secret: String,
    /// 自建应用 Corpid + Secret（选填）：文档/日程/智能表格等业务 Open API 用；
    /// 机器人 Bot ID/Secret 不能当 corpid 用（企微会回 40013）。
    #[serde(default)]
    pub corp_id: String,
    /// enc:v1: 密文或明文，与 bot_secret 同规则
    #[serde(default)]
    pub corp_secret: String,
    #[serde(default)]
    pub bot_display_name: String,
    #[serde(default)]
    pub dm_policy: String,
    #[serde(default)]
    pub group_policy: String,
    #[serde(default = "default_true")]
    pub require_mention: bool,
    #[serde(default)]
    pub allow_users: Vec<String>,
    #[serde(default)]
    pub allow_groups: Vec<String>,
    #[serde(default)]
    pub paired_users: Vec<String>,
    /// 绑定天玑跑回合所需的模型与权限；天玑档 enabled 时必填
    #[serde(default)]
    pub agent: Option<WecomAgentConfig>,
    /// 回话引擎：`tianji`（默认）| `dsh`
    #[serde(default)]
    pub engine: String,
    /// 渠道权限档位（read/workspace/review/full）；天玑档仍以 agent.permission 为准，
    /// 这份顶层副本给 DSH 档用（它不需要天玑的模型快照）。
    #[serde(default)]
    pub permission: String,
    /// DSH 引擎的工作区绝对路径；空则回落 agent.repo_path
    #[serde(default)]
    pub workspace: String,
    /// DSH 引擎选中的模型（ACP 的不透明 value：`["provider","model"]`）；空 = DSH 默认路由
    #[serde(default)]
    pub dsh_model: String,
    /// DSH 引擎的思考强度（空 = provider 默认）
    #[serde(default)]
    pub dsh_effort: String,
    /// 问答模式：`doc`（文档优先）| `doc_code`（文档+代码回退）| `code`（仅代码）。空 = doc_code。
    #[serde(default)]
    pub answer_mode: String,
    /// 逆向文档目录（相对工作区），默认 `doc`。
    #[serde(default)]
    pub doc_dir: String,
    /// SkyWalking OAP 地址（空 = 不下发排障工具）。
    #[serde(default)]
    pub skywalking_addr: String,
    /// SkyWalking 可选 header（`Key: Value`，鉴权版 OAP 用）。
    #[serde(default)]
    pub skywalking_header: String,
    /// OAP fixed UTC offset in minutes; absent uses the local clock.
    #[serde(default)]
    pub skywalking_timezone_offset_minutes: Option<i32>,
    /// 每日自动更新逆向文档：开关。
    #[serde(default)]
    pub auto_update_enabled: bool,
    /// 每日自动更新时间（`HH:MM`），默认 23:00。
    #[serde(default)]
    pub auto_update_time: String,
    /// 要跟踪的 release 分支名，默认 `release`。
    #[serde(default)]
    pub release_branch: String,
    /// 企微群机器人 webhook URL（或仅 key）；变动通知推到这里。可为 enc:v1: 密文。
    #[serde(default)]
    pub webhook_url: String,
    /// 文档更新后自动 commit（默认开）。
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    /// commit 后 push 到远端（默认关）。
    #[serde(default)]
    pub auto_push: bool,
    /// 全局并发回合上限（默认 2，范围 1–8；0/越界回落默认）。
    #[serde(default)]
    pub max_jobs: usize,
}

fn default_true() -> bool {
    true
}

/// 引擎名归一化：只认 `dsh`，其余一律天玑（旧配置缺省不会漂移）。
pub fn normalize_engine(raw: &str) -> String {
    if raw.trim().eq_ignore_ascii_case("dsh") {
        "dsh".into()
    } else {
        "tianji".into()
    }
}

/// 问答模式归一化：`doc` / `code` / `doc_code` / `auto` 原样；空或未知回落 `auto`
/// （auto = 按本条消息意图分流：排障走监控+代码、要代码走代码、其余文档优先）。
pub fn normalize_answer_mode(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "doc" => "doc".into(),
        "code" => "code".into(),
        "doc_code" => "doc_code".into(),
        _ => "auto".into(),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomStatus {
    pub state: String,
    pub detail: String,
    pub enabled: bool,
    pub engine: String,
    pub dsh: DshStatus,
    /// 最近一次长连接错误（空串 = 无）。
    pub last_error: String,
    /// 已连接时长（毫秒）；未连上为 None。
    pub connected_ms: Option<u64>,
    pub process_id: u32,
    pub build_label: String,
    pub last_investigation: Option<serde_json::Value>,
    pub last_performance: Option<serde_json::Value>,
    pub active_jobs: usize,
    pub waiting_jobs: usize,
    pub max_jobs: usize,
    pub shared_tasks: usize,
}

/// 列 DSH 引擎可用模型（ACP `configOptions`）：设置页「DSH 模型」下拉用。
/// `refresh = true` 时丢掉缓存与旧探针会话重新拉一遍。
#[tauri::command]
pub async fn wecom_dsh_models(
    state: State<'_, WecomState>,
    refresh: Option<bool>,
) -> Result<crate::wecom_dsh::DshModelCatalog, String> {
    state.dsh.ensure_ready().await?;
    state.dsh.models(refresh.unwrap_or(false)).await
}

/// 重新镜像 DSH 的 profile（把 web 的组合带到 acp），然后重连引擎让新配置生效。
#[tauri::command]
pub async fn wecom_dsh_sync(
    app: AppHandle,
    state: State<'_, WecomState>,
) -> Result<crate::wecom_dsh_profile::MirrorReport, String> {
    let report = state.dsh.sync_profile().await?;
    kick_dsh(&app, true);
    Ok(report)
}

/// 连通自检：校验 release 分支是否存在、webhook 是否可达、SkyWalking 是否可查。
/// 返回一段人类可读的多行结果（每项一行，✅/⚠️/❌）。设置页「测试连通」按钮用。
#[tauri::command]
pub async fn wecom_docgen_test(state: State<'_, WecomState>) -> Result<String, String> {
    let (cfg, sky) = {
        let docgen = state.docgen.lock().await.clone();
        let sky = state.skywalking.lock().await.clone();
        (docgen, sky)
    };
    let mut lines: Vec<String> = Vec::new();

    // 1) 工作区 + release 分支
    let workspace = cfg.workspace.trim().to_string();
    if workspace.is_empty() {
        lines.push("⚠️ 工作区未配置（自动更新依赖它）".into());
    } else if crate::git_commands::ensure_git_repo(&workspace).is_err() {
        lines.push(format!("❌ 工作区不是 Git 仓库：{workspace}"));
    } else {
        let branch = cfg.branch_or_default();
        let ws = workspace.clone();
        let br = branch.clone();
        let exists = tokio::task::spawn_blocking(move || {
            // 本地或远端跟踪分支任一存在即可
            let local = crate::git_commands::git_output(&ws, &["rev-parse", "--verify", "-q", &br]);
            let remote = crate::git_commands::git_output(
                &ws,
                &["rev-parse", "--verify", "-q", &format!("origin/{br}")],
            );
            local.map(|o| o.status.success()).unwrap_or(false)
                || remote.map(|o| o.status.success()).unwrap_or(false)
        })
        .await
        .unwrap_or(false);
        if exists {
            lines.push(format!("✅ release 分支「{branch}」存在"));
        } else {
            lines.push(format!(
                "⚠️ 未找到分支「{branch}」（本地与 origin 都没有，自动更新会失败）"
            ));
        }
    }

    // 2) webhook
    if cfg.webhook_url.trim().is_empty() {
        lines.push("⚠️ 群 webhook 未配置（不会推送变动通知）".into());
    } else {
        match crate::wecom_docgen::send_webhook(
            &cfg.webhook_url,
            "✅ AI Workbench 连通测试：逆向文档通道可用。",
        )
        .await
        {
            Ok(()) => lines.push("✅ 群 webhook 可达（已发一条测试消息）".into()),
            Err(e) => lines.push(format!("❌ 群 webhook 失败：{e}")),
        }
    }

    // 3) SkyWalking
    if sky.0.trim().is_empty() {
        lines.push("ℹ️ 未配置 SkyWalking（线上排障不启用）".into());
    } else {
        let offset = *state.skywalking_timezone.lock().await;
        let mut probe = serde_json::json!({ "keyword": "", "minutes": 5 });
        if let Some(offset) = offset {
            probe["timezone_offset_minutes"] = serde_json::json!(offset);
        }
        match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            crate::wecom_skywalking::run(&sky.0, &sky.1, &probe),
        )
        .await
        {
            Ok(Ok(_)) => lines.push("✅ SkyWalking 可查（GraphQL 正常响应）".into()),
            Ok(Err(e)) => lines.push(format!("❌ SkyWalking 查询失败：{e}")),
            Err(_) => lines.push("❌ SkyWalking 查询超时（检查地址/网络）".into()),
        }
    }

    Ok(lines.join("\n"))
}

/// 「立即更新一次」逆向文档（设置页按钮）：手动触发一轮，便于验证配置。
/// 返回一句人类可读的结果摘要。
#[tauri::command]
pub async fn wecom_docgen_run_now(
    app: AppHandle,
    state: State<'_, WecomState>,
) -> Result<String, String> {
    let cfg = state.docgen.lock().await.clone();
    if cfg.workspace.trim().is_empty() {
        return Err("请先填写工作区路径（即逆向文档所在的 Git 仓库）".into());
    }
    let report = crate::wecom_docgen::run_update(&app, &cfg).await?;
    if let Some(reason) = report.skipped_reason {
        return Ok(format!("已跳过：{reason}"));
    }
    Ok(format!(
        "已更新：分支 {}，{} 个变更文件，群通知{}。\n{}",
        report.branch,
        report.new_commits,
        if report.pushed {
            "已推送"
        } else {
            "未推送（检查 webhook）"
        },
        if report.updated_docs.is_empty() {
            "（无摘要）".to_string()
        } else {
            report.updated_docs
        }
    ))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomPairingView {
    pub userid: String,
    pub code: String,
    pub created_ms: u64,
}

#[tauri::command]
pub async fn wecom_channel_status(
    app: AppHandle,
    state: State<'_, WecomState>,
) -> Result<WecomStatus, String> {
    let bot = state
        .applied
        .lock()
        .await
        .as_ref()
        .map(|c| c.bot_id.clone())
        .unwrap_or_default();
    let db = app.state::<crate::DbState>().conn.clone();
    let shared_tasks = tokio::task::spawn_blocking(move || {
        let c = db.lock().map_err(|_| "task db lock".to_string())?;
        crate::wecom_tasks::active_count(&c, &bot, chrono::Utc::now().timestamp_millis())
    })
    .await
    .map_err(|e| e.to_string())??;
    let (s, detail) = state.sock.status().await;
    let (last_error, connected_ms) = state.sock.detail().await;
    let enabled = state.applied.lock().await.is_some();
    let engine = state.orch.engine.lock().await.clone();
    let dsh = state.dsh.status().await;
    Ok(WecomStatus {
        state: s,
        detail,
        enabled,
        engine,
        dsh,
        last_error,
        connected_ms,
        process_id: std::process::id(),
        build_label: format!(
            "{}-{} / report-v2-readable-concurrency32",
            env!("CARGO_PKG_VERSION"),
            if cfg!(debug_assertions) {
                "dev"
            } else {
                "release"
            }
        ),
        last_investigation: crate::wecom_investigation::last_diagnostic(),
        last_performance: crate::wecom_performance::latest(),
        active_jobs: state.orch.admission.active(),
        waiting_jobs: state.orch.admission.waiting(),
        max_jobs: state.orch.admission.limit(),
        shared_tasks,
    })
}

#[tauri::command]
pub async fn wecom_channel_apply(
    app: AppHandle,
    state: State<'_, WecomState>,
    req: WecomApplyRequest,
) -> Result<WecomStatus, String> {
    if req
        .skywalking_timezone_offset_minutes
        .is_some_and(|v| !(-720..=840).contains(&v))
    {
        return Err("OAP时区偏移须在UTC-12至UTC+14之间".into());
    }
    // 解密 Secret（机器人长连接 + 业务 API 各一套，后者选填）
    let secret = crate::secret_protect::unprotect_secret(req.bot_secret.trim())
        .map_err(|e| format!("Secret 解密失败: {e}"))?;
    let corp_secret = if req.corp_secret.trim().is_empty() {
        String::new()
    } else {
        crate::secret_protect::unprotect_secret(req.corp_secret.trim())
            .map_err(|e| format!("应用 Secret 解密失败: {e}"))?
    };

    // 更新策略与 agent 配置
    {
        let mut policy = state.orch.policy.lock().await;
        policy.dm_policy = DmPolicy::parse(&req.dm_policy);
        policy.group_policy = GroupPolicy::parse(&req.group_policy);
        policy.require_mention = req.require_mention;
        policy.allow_users = req
            .allow_users
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        policy.allow_groups = req
            .allow_groups
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // 合并前端持久化的已配对用户（不丢内存里尚未同步的）
        for u in req.paired_users {
            let u = u.trim().to_string();
            if !u.is_empty() && !policy.paired_users.iter().any(|x| x == &u) {
                policy.paired_users.push(u);
            }
        }
    }
    {
        *state.orch.bot_display_name.lock().await = req.bot_display_name.trim().to_string();
    }
    // 引擎与档位在 agent 被 move 之前取出来：DSH 档不需要天玑的模型快照。
    let engine = normalize_engine(&req.engine);
    let tier = if req.permission.trim().is_empty() {
        req.agent
            .as_ref()
            .map(|a| a.permission.clone())
            .unwrap_or_default()
    } else {
        req.permission.clone()
    };
    let workspace = if req.workspace.trim().is_empty() {
        req.agent
            .as_ref()
            .map(|a| a.repo_path.clone())
            .unwrap_or_default()
    } else {
        req.workspace.trim().to_string()
    };
    // DSH 档随会话下发的 MCP 服务：从 agent 配置里取（总开关关掉则不带）。
    let dsh_mcp = req
        .agent
        .as_ref()
        .filter(|a| a.mcp_enabled)
        .map(|a| a.mcp.clone())
        .unwrap_or_default();
    if let Some(agent) = req.agent {
        *state.orch.agent.lock().await = Some(agent);
    }
    *state.orch.engine.lock().await = engine.clone();
    // 全局并发上限（0/越界在 set_max_jobs 内回落默认）。
    state.orch.set_max_jobs(req.max_jobs);

    // Changing query namespace invalidates short-lived investigation cursors, including switching back.
    let old_sky = state.skywalking.lock().await.clone();
    let old_zone = *state.skywalking_timezone.lock().await;
    let old_bot = state
        .applied
        .lock()
        .await
        .as_ref()
        .map(|c| c.bot_id.clone())
        .unwrap_or_default();
    let old_workspace = state.dsh.workspace().await;
    if old_sky
        != (
            req.skywalking_addr.trim().to_string(),
            req.skywalking_header.trim().to_string(),
        )
        || old_zone != req.skywalking_timezone_offset_minutes
        || old_bot != req.bot_id.trim()
        || old_workspace != workspace
    {
        crate::wecom_investigation::clear_contexts();
    }
    // SkyWalking 排障地址（空 = 问答时不下发 skywalking 工具）。
    *state.skywalking.lock().await = (
        req.skywalking_addr.trim().to_string(),
        req.skywalking_header.trim().to_string(),
    );

    *state.skywalking_timezone.lock().await = req.skywalking_timezone_offset_minutes;

    // 问答模式写进 orchestrator 的 agent 配置副本里（供 run_agent_and_stream 读）。
    {
        let mut g = state.orch.answer_mode.lock().await;
        *g = normalize_answer_mode(&req.answer_mode);
    }
    {
        let d = req.doc_dir.trim();
        *state.orch.doc_dir.lock().await = if d.is_empty() {
            "doc".into()
        } else {
            d.to_string()
        };
    }

    // 逆向文档自动更新配置（webhook 可能是 enc:v1: 密文）。
    let webhook_plain = if req.webhook_url.trim().is_empty() {
        String::new()
    } else {
        crate::secret_protect::unprotect_secret(req.webhook_url.trim()).unwrap_or_default()
    };
    *state.docgen.lock().await = crate::wecom_docgen::DocgenConfig {
        enabled: req.auto_update_enabled,
        time: req.auto_update_time.trim().to_string(),
        release_branch: req.release_branch.trim().to_string(),
        workspace: workspace.clone(),
        doc_dir: req.doc_dir.trim().to_string(),
        webhook_url: webhook_plain,
        auto_commit: req.auto_commit,
        auto_push: req.auto_push,
    };

    if !req.enabled {
        state.sock.stop().await;
        stop_dsh_async(&app);
        *state.applied.lock().await = None;
        let st = wecom_channel_status(app.clone(), state).await?;
        emit_status(&app, &st);
        return Ok(st);
    }

    let bot_id = req.bot_id.trim().to_string();
    if bot_id.is_empty() || secret.is_empty() {
        return Err("请填写 Bot ID 与 Secret".into());
    }
    if engine == "dsh" {
        // 这里同步校验，错误直接弹在设置页；进程启动放在后台，别卡住 UI。
        if workspace.trim().is_empty() {
            return Err("DSH 引擎需要工作区路径：请填「工作区路径」或先选一个 Git 仓库".into());
        }
        if !std::path::Path::new(&workspace).is_absolute() {
            return Err("DSH 工作区必须是绝对路径".into());
        }
        if crate::dsh_commands::dsh_installed_version().is_none() {
            return Err(
                "未检测到 DeepSeek Harness：请先在「Harness」页安装后再启用 DSH 引擎".into(),
            );
        }
        let changed = state
            .dsh
            .configure(
                Tier::parse(&tier),
                workspace.clone(),
                req.dsh_model.trim().to_string(),
                req.dsh_effort.trim().to_string(),
            )
            .await;
        // MCP 服务随会话下发（ACP mcpServers）；总开关关掉时 dsh_mcp 为空。
        state.dsh.set_mcp(dsh_mcp).await;
        // 档位/工作区变了就带 restart 标记；否则 ensure_ready 命中 Ready 会直接返回。
        kick_dsh(&app, changed);
    } else {
        // 切回天玑：DSH 进程不再需要，顺手回收（放后台，别让保存卡在启动流程上）。
        stop_dsh_async(&app);
        if state.orch.agent.lock().await.is_none() {
            return Err("请先配置天玑模型，再启用企微通道".into());
        }
    }

    let corp_id = req.corp_id.trim().to_string();
    let creds = AppliedCreds {
        bot_id: bot_id.clone(),
        secret: secret.clone(),
        corp_id: if corp_id.is_empty() {
            None
        } else {
            Some(corp_id)
        },
        corp_secret: if corp_secret.is_empty() {
            None
        } else {
            Some(corp_secret)
        },
    };
    let prev = state.applied.lock().await.clone();
    let bot_same = matches!(
        &prev,
        Some(c) if c.bot_id == creds.bot_id && c.secret == creds.secret
    );
    let corp_changed = prev
        .as_ref()
        .map(|c| (c.corp_id.clone(), c.corp_secret.clone()))
        != Some((creds.corp_id.clone(), creds.corp_secret.clone()));
    if !bot_same {
        // 换凭据：断旧连再开
        state.sock.stop().await;
        *state.applied.lock().await = Some(creds);
        *state.api_token.lock().await = None;

        let orch = Arc::clone(&state.orch);
        let app_msg = app.clone();
        let app_ev = app.clone();
        let on_msg = Arc::new(move |msg| {
            orch.handle_msg(app_msg.clone(), msg);
        });
        let orch2 = Arc::clone(&state.orch);
        let on_event = Arc::new(move |ev| {
            orch2.handle_event(app_ev.clone(), ev);
        });
        state
            .sock
            .start(app.clone(), bot_id, secret, on_msg, on_event);
    } else if corp_changed {
        // 业务凭证变化不影响长连接：只更新指纹并作废业务 token 缓存
        *state.applied.lock().await = Some(creds);
        *state.api_token.lock().await = None;
    }

    // 等一小会儿让 Connecting → Connected/Error 有机会更新
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let st = wecom_channel_status(app.clone(), state).await?;
    emit_status(&app, &st);
    Ok(st)
}

/// 后台回收 DSH 进程：`stop()` 会等启动流程释放锁，不能在 `apply` 里同步等。
fn stop_dsh_async(app: &AppHandle) {
    let dsh = Arc::clone(&app.state::<WecomState>().dsh);
    tauri::async_runtime::spawn(async move {
        dsh.stop().await;
    });
}

/// 后台拉起 DSH 引擎（`restart` = 先杀掉现有进程），完成后主动推一次状态，
/// 设置页不必等下一个 5 秒轮询。
fn kick_dsh(app: &AppHandle, restart: bool) {
    let dsh = Arc::clone(&app.state::<WecomState>().dsh);
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = if restart {
            dsh.restart().await
        } else {
            dsh.ensure_ready().await
        };
        if let Err(e) = result {
            eprintln!("[wecom] DSH 引擎启动失败：{e}");
        }
        let st = wecom_channel_status(app_handle.clone(), app_handle.state::<WecomState>()).await;
        if let Ok(st) = st {
            emit_status(&app_handle, &st);
        }
    });
}

#[tauri::command]
pub async fn wecom_channel_restart(
    app: AppHandle,
    state: State<'_, WecomState>,
) -> Result<WecomStatus, String> {
    let creds = state
        .applied
        .lock()
        .await
        .clone()
        .ok_or_else(|| "通道未启用".to_string())?;
    let engine = state.orch.engine.lock().await.clone();
    if engine == "dsh" {
        kick_dsh(&app, true);
    }
    state.sock.stop().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let orch = Arc::clone(&state.orch);
    let app_msg = app.clone();
    let app_ev = app.clone();
    let on_msg = Arc::new(move |msg| {
        orch.handle_msg(app_msg.clone(), msg);
    });
    let orch2 = Arc::clone(&state.orch);
    let on_event = Arc::new(move |ev| {
        orch2.handle_event(app_ev.clone(), ev);
    });
    state.sock.start(
        app.clone(),
        creds.bot_id.clone(),
        creds.secret.clone(),
        on_msg,
        on_event,
    );
    *state.applied.lock().await = Some(creds);

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let st = wecom_channel_status(app.clone(), state).await?;
    emit_status(&app, &st);
    Ok(st)
}
#[tauri::command]
pub async fn wecom_pairing_list(
    state: State<'_, WecomState>,
) -> Result<Vec<WecomPairingView>, String> {
    let mut policy = state.orch.policy.lock().await;
    policy.prune_pending();
    Ok(policy
        .pending
        .iter()
        .map(|p| WecomPairingView {
            userid: p.userid.clone(),
            code: p.code.clone(),
            created_ms: p.created_ms,
        })
        .collect())
}

#[tauri::command]
pub async fn wecom_pairing_approve(
    state: State<'_, WecomState>,
    code: String,
) -> Result<String, String> {
    let mut policy = state.orch.policy.lock().await;
    policy.approve_pair(&code)
}

#[tauri::command]
pub async fn wecom_pairing_revoke(
    state: State<'_, WecomState>,
    userid: String,
) -> Result<(), String> {
    let mut policy = state.orch.policy.lock().await;
    policy.revoke_user(userid.trim());
    Ok(())
}

#[tauri::command]
pub async fn wecom_paired_users(state: State<'_, WecomState>) -> Result<Vec<String>, String> {
    let policy = state.orch.policy.lock().await;
    Ok(policy.paired_users.clone())
}

#[tauri::command]
pub fn wecom_protect_secret(plain: String) -> Result<String, String> {
    crate::secret_protect::protect_secret(&plain)
}

#[tauri::command]
pub fn wecom_reveal_secret(stored: String) -> Result<String, String> {
    crate::secret_protect::unprotect_secret(stored.trim())
}

/// 读通道排障日志最后 N 行（设置页「通道日志」折叠区）。N 缺省 200，封顶 2000。
#[tauri::command]
pub fn wecom_log_tail(lines: Option<usize>) -> Result<String, String> {
    let n = lines.unwrap_or(200).clamp(1, 2000);
    Ok(crate::wecom_log::tail(n))
}

/// 读最近若干条企微会话旁证（设置页「会话日志」折叠区）。
/// 不传 key 则汇总所有会话文件；按时间倒序，最多 `limit`（缺省 30，封顶 200）条。
#[tauri::command]
pub fn wecom_session_recent(
    key: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<crate::wecom_session::SessionLogEntry>, String> {
    let n = limit.unwrap_or(30).clamp(1, 200);
    Ok(crate::wecom_session::recent_entries(key.as_deref(), n))
}

fn emit_status(app: &AppHandle, st: &WecomStatus) {
    let _ = app.emit("wecom-channel-status", st);
}

/// setup 时挂上 State；真正连线等前端 apply。
pub fn init_state() -> WecomState {
    WecomState::new()
}

// silence unused import in some builds
#[allow(dead_code)]
fn _touch_policy(_: &PolicyState, _: &PendingPair) {}
