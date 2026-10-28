//! 企微渠道的 DeepSeek Harness（DSH）引擎。
//!
//! 与天玑引擎并列的第二种「谁来回话」：常驻一个 `dsh --profile acp` 子进程，
//! 通过 stdio 上的 ACP（JSON-RPC 2.0 / 换行分隔）驱动它跑完一轮。
//!
//! 设计要点：
//! - 一个进程常驻，按企微会话键复用 ACP `sessionId`（多轮上下文由 DSH 自己维护）；
//! - 渠道权限档位在启动时通过 `--patch` 覆盖层写死（沙箱 mode + 审批 policy），
//!   不动用户的 `~/.dsh` 配置；档位或工作区变化即重启进程；
//! - DSH 的 `session/request_permission` 交给编排层转成企微审批码；
//! - 文本更新来自 ACP 的「提交点」，不是逐 token 增量，节流交给编排层。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::dsh_commands;
use crate::wecom_dsh_profile::{self, MirrorReport};

/// ACP 协议版本（`@agentclientprotocol/sdk` 的 `PROTOCOL_VERSION`）。
const PROTOCOL_VERSION: u64 = 1;
/// `initialize` 是进程健康的第一个信号：Node 冷启动 + profile 组装通常 2-5 秒，
/// 首次初始化 profile 可能要装依赖，所以给得比普通 RPC 宽。
const INIT_TIMEOUT: Duration = Duration::from_secs(120);
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);
const STDERR_TAIL_LINES: usize = 24;

/// 渠道权限档位 → DSH 沙箱 mode + 审批 policy。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Read,
    Workspace,
    Review,
    Full,
}

impl Tier {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "workspace" => Self::Workspace,
            "review" => Self::Review,
            "full" => Self::Full,
            _ => Self::Read,
        }
    }

    /// `review` 没有等价物：DSH 只有「沙箱 + 审批」，没有「先让 AI 审一遍」，
    /// 所以它退化成 workspace（UI 上要写明）。
    fn sandbox_mode(self) -> &'static str {
        match self {
            Self::Read => "read-only",
            Self::Workspace | Self::Review => "workspace-write",
            Self::Full => "danger-full-access",
        }
    }

    fn approval_policy(self) -> &'static str {
        match self {
            Self::Full => "never",
            _ => "ask",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Workspace => "workspace",
            Self::Review => "review",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshStatus {
    /// stopped | starting | ready | error
    pub state: String,
    pub detail: String,
    pub version: Option<String>,
    /// 上一次 profile 镜像的结果（来源 / bundle 数 / 是否联接 / 同步时间）
    pub mirror: Option<MirrorReport>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshModelOption {
    /// ACP 的不透明取值（`["provider","model"]` 的 JSON）
    pub value: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshModelGroup {
    pub group: String,
    pub name: String,
    pub options: Vec<DshModelOption>,
}

/// 思考强度（`reasoning_effort`）；模型不支持时整栏缺席。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshEffort {
    pub current: String,
    pub options: Vec<DshModelOption>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshModelCatalog {
    pub current: String,
    pub groups: Vec<DshModelGroup>,
    pub effort: Option<DshEffort>,
}

impl DshModelCatalog {
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.options.is_empty())
    }
}


/// 一回合里编排层关心的事件。
pub enum DshEvent {
    /// 完整的 `update` 对象（`sessionUpdate` 在里面）。
    Update(Value),
    /// DSH 请求授权：`rpc_id` 用于回执，`params` 里带 options。
    Permission { rpc_id: u64, params: Value },
    /// `session/prompt` 的应答（或失败）。
    Done(Result<Value, String>),
}

/// JSON-RPC 共享状态：读线程写、回合与请求读。
struct RpcShared {
    pending: StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    /// sessionId → 当前回合的事件通道（一个会话同时只有一个 prompt）。
    turns: StdMutex<HashMap<String, mpsc::UnboundedSender<DshEvent>>>,
    /// `session/prompt` 的 rpc id → sessionId，用于把应答路由回回合。
    prompt_ids: StdMutex<HashMap<u64, String>>,
    next_id: AtomicU64,
    /// 子进程还活着吗（读线程 EOF 时置 false）。
    alive: AtomicBool,
}

impl RpcShared {
    fn new() -> Self {
        Self {
            pending: StdMutex::new(HashMap::new()),
            turns: StdMutex::new(HashMap::new()),
            prompt_ids: StdMutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            alive: AtomicBool::new(true),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}

struct Runtime {
    child: tokio::process::Child,
    writer: mpsc::UnboundedSender<Value>,
    shared: Arc<RuntimeShared>,
    version: String,
}

/// `Arc<RpcShared>` + stderr 尾巴，一起传给读线程。
struct RuntimeShared {
    rpc: Arc<RpcShared>,
    stderr: StdMutex<Vec<String>>,
}

impl RuntimeShared {
    fn push_stderr(&self, line: &str) {
        let line = line.trim_end();
        if line.trim().is_empty() {
            return;
        }
        let clipped: String = line.chars().take(400).collect();
        if let Ok(mut lines) = self.stderr.lock() {
            lines.push(clipped);
            let overflow = lines.len().saturating_sub(STDERR_TAIL_LINES);
            if overflow > 0 {
                lines.drain(0..overflow);
            }
        }
    }

    fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map(|lines| lines.join("\n"))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineState {
    Stopped,
    Starting,
    Ready,
    Error,
}

impl EngineState {
    fn label(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Error => "error",
        }
    }
}

struct EngineInner {
    state: EngineState,
    detail: String,
    tier: Tier,
    workspace: String,
    /// 用户在设置页选的模型（ACP 的不透明 value，空 = 用 DSH 的默认路由）
    model: String,
    /// 思考强度（空 = provider 默认）
    effort: String,
    runtime: Option<Runtime>,
    /// 企微会话键（`wecom:dm:u1`）→ ACP sessionId。落盘，重启后还能 resume。
    sessions: HashMap<String, String>,
    /// sessionId → 上一次成功应用的 (model, effort)，避免每轮重复下发
    applied: HashMap<String, (String, String)>,
    /// 列模型用的探针会话（不带用户对话，随时可重建）
    probe: Option<String>,
    /// 探针会话对应的模型目录缓存
    catalog: Option<DshModelCatalog>,
    /// 上一次 profile 镜像结果
    mirror: Option<MirrorReport>,
    /// ACP `initialize` 宣告的 `promptCapabilities.image`：当前模型是否支持图片输入。
    /// 换模型会重启进程 → 重新握手刷新；没宣告按 false（降级文本）。
    image_prompt: bool,
}

pub struct DshEngine {
    inner: Mutex<EngineInner>,
    /// 同一时刻只允许一个启动流程，避免两条消息各起一个 node。
    start_lock: Mutex<()>,
}

impl Default for DshEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DshEngine {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(EngineInner {
                state: EngineState::Stopped,
                detail: String::new(),
                tier: Tier::Read,
                workspace: String::new(),
                model: String::new(),
                effort: String::new(),
                runtime: None,
                sessions: load_sessions(),
                applied: HashMap::new(),
                probe: None,
                catalog: None,
                mirror: None,
                image_prompt: false,
            }),
            start_lock: Mutex::new(()),
        }
    }

    /// 写入本档配置。返回 `true` 表示档位/工作区变了，需要重启子进程
    /// （模型 / 思考强度只影响后续回合，不必重启）。
    pub async fn configure(
        &self,
        tier: Tier,
        workspace: String,
        model: String,
        effort: String,
    ) -> bool {
        let mut g = self.inner.lock().await;
        let changed = g.tier != tier || g.workspace != workspace;
        g.tier = tier;
        g.workspace = workspace;
        if g.model != model {
            g.model = model;
            g.applied.clear();
        }
        if g.effort != effort {
            g.effort = effort;
            g.applied.clear();
        }
        changed
    }

    pub async fn status(&self) -> DshStatus {
        let g = self.inner.lock().await;
        let version = g.runtime.as_ref().map(|r| r.version.clone());
        let mirror = g.mirror.clone();
        // 进程死了但状态还停在 Ready：如实报错，下一条消息会自动重启。
        if g.state == EngineState::Ready {
            if let Some(rt) = &g.runtime {
                if !rt.shared.rpc.alive.load(Ordering::SeqCst) {
                    return DshStatus {
                        state: "error".into(),
                        detail: "DSH 进程已退出，下一条消息会自动重启".into(),
                        version,
                        mirror,
                    };
                }
            }
        }
        DshStatus {
            state: g.state.label().into(),
            detail: g.detail.clone(),
            version,
            mirror,
        }
    }

    pub async fn stop(&self) {
        let _guard = self.start_lock.lock().await;
        self.teardown().await;
        let mut g = self.inner.lock().await;
        g.state = EngineState::Stopped;
        g.detail.clear();
    }

    /// 重新镜像 profile（设置页「重新同步」）：只写 acp 目录，不碰 web。
    pub async fn sync_profile(&self) -> Result<MirrorReport, String> {
        let root = wecom_dsh_profile::dsh_profiles_root()
            .ok_or_else(|| "找不到用户目录（USERPROFILE）".to_string())?;
        let report = tokio::task::spawn_blocking(move || {
            wecom_dsh_profile::mirror_profiles(&root)
        })
        .await
        .map_err(|e| format!("同步任务失败：{e}"))??;
        let mut g = self.inner.lock().await;
        g.mirror = Some(report.clone());
        Ok(report)
    }

    /// 列模型：用一条探针会话读 ACP 的 `configOptions`。
    /// `refresh` = 丢掉缓存与旧探针，重新建。
    pub async fn models(&self, refresh: bool) -> Result<DshModelCatalog, String> {
        let (shared, writer) = self.parts().await?;
        if !refresh {
            let cached = {
                let g = self.inner.lock().await;
                g.catalog.clone()
            };
            if let Some(catalog) = cached {
                return Ok(catalog);
            }
        }

        // 旧探针先关掉，免得会话越攒越多。
        let old = {
            let mut g = self.inner.lock().await;
            g.catalog = None;
            g.probe.take()
        };
        if let Some(session) = old {
            let _ = rpc_call(
                &shared,
                &writer,
                "session/close",
                json!({ "sessionId": session }),
                SESSION_TIMEOUT,
            )
            .await;
        }

        let workspace = self.workspace().await;
        let created = rpc_call(
            &shared,
            &writer,
            "session/new",
            json!({ "cwd": workspace, "mcpServers": [] }),
            SESSION_TIMEOUT,
        )
        .await?;
        let session_id = created
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "DSH 没有返回 sessionId".to_string())?
            .to_string();
        let catalog = parse_config_options(
            created
                .get("configOptions")
                .unwrap_or(&Value::Null),
        );
        {
            let mut g = self.inner.lock().await;
            g.probe = Some(session_id);
            g.catalog = Some(catalog.clone());
        }
        Ok(catalog)
    }

    /// 把设置页选的模型 / 思考强度应用到某个会话；值没变就跳过。
    /// 返回 `Err` 时调用方只记日志，不让整轮失败（会话会退回默认路由）。
    pub async fn apply_selection(&self, session_id: &str) -> Result<(), String> {
        let (mut model, effort) = {
            let g = self.inner.lock().await;
            (g.model.clone(), g.effort.clone())
        };
        if model.trim().is_empty() {
            // 没指定就挑一个真有凭据的路由：DSH 的默认值可能指向没 key 的 provider
            //（典型是镜像过来的 `agent-default-model`，provider 在 ACP 里不可用）。
            let catalog = self.models(false).await?;
            let available: Vec<String> = catalog
                .groups
                .iter()
                .flat_map(|g| g.options.iter().map(|o| o.value.clone()))
                .collect();
            if available.is_empty() {
                return Err(
                    "DSH 里没有可用模型：请在 Harness → 模型页配好模型与密钥/登录".into(),
                );
            }
            model = if available.iter().any(|v| v == &catalog.current) {
                catalog.current
            } else {
                available[0].clone()
            };
        }
        if model.trim().is_empty() && effort.trim().is_empty() {
            return Ok(());
        }
        {
            let g = self.inner.lock().await;
            if let Some((m, e)) = g.applied.get(session_id) {
                if m == &model && e == &effort {
                    return Ok(());
                }
            }
        }
        let (shared, writer) = self.parts().await?;
        if !model.trim().is_empty() {
            rpc_call(
                &shared,
                &writer,
                "session/set_config_option",
                json!({ "sessionId": session_id, "configId": "model", "value": model }),
                SESSION_TIMEOUT,
            )
            .await
            .map_err(|e| format!("应用模型失败（已退回 DSH 默认路由）：{e}"))?;
        }
        if !effort.trim().is_empty() {
            rpc_call(
                &shared,
                &writer,
                "session/set_config_option",
                json!({ "sessionId": session_id, "configId": "reasoning_effort", "value": effort }),
                SESSION_TIMEOUT,
            )
            .await
            .map_err(|e| format!("应用思考强度失败（保留模型选择）：{e}"))?;
        }
        let mut g = self.inner.lock().await;
        g.applied
            .insert(session_id.to_string(), (model, effort));
        Ok(())
    }

    /// 立即重启（换引擎/换档位/手动重连时用）；未配置工作区时报错。
    ///
    /// 注意：`start_lock` 不可重入，所以这里不能调 `stop()` / `ensure_ready()`
    /// 那两个带锁的公开方法，只能复用内部实现。
    pub async fn restart(&self) -> Result<(), String> {
        let _guard = self.start_lock.lock().await;
        self.teardown().await;
        self.ensure_ready_locked().await
    }

    /// 确保进程就绪；不满足条件（未装 DSH、工作区为空）时给出可操作文案。
    pub async fn ensure_ready(&self) -> Result<(), String> {
        let _guard = self.start_lock.lock().await;
        self.ensure_ready_locked().await
    }

    /// 已持有 `start_lock` 时的启动流程。
    async fn ensure_ready_locked(&self) -> Result<(), String> {
        let (tier, workspace) = {
            let g = self.inner.lock().await;
            if g.state == EngineState::Ready {
                if let Some(rt) = &g.runtime {
                    if rt.shared.rpc.alive.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                }
            }
            (g.tier, g.workspace.clone())
        };
        if workspace.trim().is_empty() {
            return Err("DSH 引擎需要工作区路径：请在「访问策略 → 工作区路径」填一个绝对路径".into());
        }
        self.teardown().await;
        self.set_state(EngineState::Starting, "正在启动 DeepSeek Harness…")
            .await;

        match self.start(tier, &workspace).await {
            Ok(_version) => {
                let mut g = self.inner.lock().await;
                g.state = EngineState::Ready;
                g.detail.clear();
                Ok(())
            }
            Err(e) => {
                self.set_state(EngineState::Error, &e).await;
                Err(e)
            }
        }
    }

    /// 取该企微会话对应的 ACP 会话：先 resume，失败（工作区变了 / 会话被清理）再新建。
    pub async fn acquire_session(&self, key: &str) -> Result<String, String> {
        let (workspace, existing) = {
            let g = self.inner.lock().await;
            (g.workspace.clone(), g.sessions.get(key).cloned())
        };
        let (shared, writer) = self.parts().await?;

        if let Some(id) = existing {
            let params = json!({
                "sessionId": id,
                "cwd": workspace,
                "mcpServers": [],
            });
            match rpc_call(&shared, &writer, "session/resume", params, SESSION_TIMEOUT).await {
                Ok(_) => return Ok(id),
                Err(e) => {
                    eprintln!("[wecom-dsh] resume {} 失败，改为新建：{e}", key);
                    crate::wecom_log::write(
                        "dsh",
                        &format!("resume {key} 失败（新建会话，上下文重置）：{e}"),
                    );
                }
            }
        }

        let created = rpc_call(
            &shared,
            &writer,
            "session/new",
            json!({ "cwd": workspace, "mcpServers": [] }),
            SESSION_TIMEOUT,
        )
        .await?;
        let id = created
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "DSH 没有返回 sessionId".to_string())?
            .to_string();
        {
            let mut g = self.inner.lock().await;
            g.sessions.insert(key.to_string(), id.clone());
        }
        self.persist_sessions().await;
        Ok(id)
    }

    /// 发起一轮 prompt；返回的 `DshTurn` 负责收流、回审批、取消。
    ///
    /// `prompt_blocks` 是 ACP 的 content block 数组（`text` / `image`）。纯文本回合也走这里
    /// （传一个 `{type:"text"}`）；vision 模型且宣告 image 能力时，编排层会内联 image block，
    /// 这样图片才真正进模型，而不是降级成文本 URL。
    pub async fn prompt(&self, session_id: &str, prompt_blocks: Vec<Value>) -> Result<DshTurn, String> {
        let (shared, writer) = self.parts().await?;
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let mut turns = shared.rpc.turns.lock().map_err(|e| e.to_string())?;
            turns.insert(session_id.to_string(), tx);
        }
        let rpc_id = shared.rpc.next_id();
        {
            let mut ids = shared.rpc.prompt_ids.lock().map_err(|e| e.to_string())?;
            ids.insert(rpc_id, session_id.to_string());
        }
        let frame = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": prompt_blocks,
            }
        });
        if writer.send(frame).is_err() {
            if let Ok(mut ids) = shared.rpc.prompt_ids.lock() {
                ids.remove(&rpc_id);
            }
            if let Ok(mut turns) = shared.rpc.turns.lock() {
                turns.remove(session_id);
            }
            return Err("DSH 发送通道已关闭".into());
        }
        Ok(DshTurn {
            session_id: session_id.to_string(),
            events: rx,
            responder: DshResponder {
                writer,
                session_id: session_id.to_string(),
            },
            shared: Arc::clone(&shared.rpc),
            prompt_id: rpc_id,
        })
    }

    /// 当前工作区（出站脱敏与提示文案用）。
    pub async fn workspace(&self) -> String {
        self.inner.lock().await.workspace.clone()
    }

    /// 当前模型是否支持图片输入（ACP `promptCapabilities.image`）。
    /// 编排层据此决定把企微图片内联成 image block，还是降级成文本线索。
    pub async fn image_prompt_enabled(&self) -> bool {
        self.inner.lock().await.image_prompt
    }

    async fn parts(&self) -> Result<(Arc<RuntimeShared>, mpsc::UnboundedSender<Value>), String> {
        let g = self.inner.lock().await;
        match &g.runtime {
            Some(rt) if rt.shared.rpc.alive.load(Ordering::SeqCst) => {
                Ok((Arc::clone(&rt.shared), rt.writer.clone()))
            }
            _ => Err("DSH 引擎尚未就绪".into()),
        }
    }

    async fn set_state(&self, state: EngineState, detail: &str) {
        let mut g = self.inner.lock().await;
        g.state = state;
        g.detail = detail.to_string();
    }

    async fn teardown(&self) {
        let runtime = {
            let mut g = self.inner.lock().await;
            // 进程换了，探针会话、模型缓存、已下发的选择全部作废。
            g.probe = None;
            g.catalog = None;
            g.applied.clear();
            g.image_prompt = false;
            g.runtime.take()
        };
        if let Some(mut rt) = runtime {
            rt.shared.rpc.alive.store(false, Ordering::SeqCst);
            let _ = rt.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), rt.child.wait()).await;
        }
    }

    /// 生成 patch、起进程、接上三条管道、握手。
    async fn start(&self, tier: Tier, workspace: &str) -> Result<String, String> {
        let workspace = workspace.to_string();
        let prepared = tokio::task::spawn_blocking(move || prepare_launch(tier, &workspace))
            .await
            .map_err(|e| format!("启动准备失败：{e}"))??;

        let mut cmd = tokio::process::Command::new(&prepared.program);
        cmd.args(&prepared.args)
            .current_dir(&prepared.workspace)
            .env("PATH", &prepared.path_value)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        {
            // tokio 的 Command 自带 creation_flags（Windows），不需要扩展 trait。
            cmd.creation_flags(0x0800_0000);
        }

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "启动 DSH 失败：找不到 Node.js / dsh，请先在「Harness」页安装 DeepSeek Harness".to_string()
            } else {
                format!("启动 DSH 失败：{e}")
            }
        })?;

        let stdin = child.stdin.take().ok_or("DSH 进程没有 stdin")?;
        let stdout = child.stdout.take().ok_or("DSH 进程没有 stdout")?;
        let stderr = child.stderr.take().ok_or("DSH 进程没有 stderr")?;

        let shared = Arc::new(RuntimeShared {
            rpc: Arc::new(RpcShared::new()),
            stderr: StdMutex::new(Vec::new()),
        });
        let (tx, rx) = mpsc::unbounded_channel::<Value>();

        spawn_writer(stdin, rx);
        spawn_stdout_reader(stdout, Arc::clone(&shared), tx.clone());
        spawn_stderr_reader(stderr, Arc::clone(&shared));

        // 握手：dsh 的 initialize 不校验入参，但它是「进程真的活了」的第一个回包。
        let init = rpc_call(
            &shared,
            &tx,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false } },
                "clientInfo": { "name": "ai-workbench-wecom", "version": env!("CARGO_PKG_VERSION") },
            }),
            INIT_TIMEOUT,
        )
        .await;
        let init = match init {
            Ok(value) => value,
            Err(e) => {
                // 握手失败要收掉子进程，否则会留一个没人管的 node。
                let tail = shared.stderr_tail();
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                return Err(if tail.is_empty() {
                    e
                } else {
                    format!("{e}\n{tail}")
                });
            }
        };

        let version = init
            .get("agentInfo")
            .and_then(|v| v.get("version"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // ACP 宣告的 promptCapabilities.image：决定能否把企微图片内联成 image block。
        // 它反映「当前配置的模型」是否支持图片输入（dsh-acp 在 initialize 时按模型算）。
        let image_prompt = init
            .get("agentCapabilities")
            .and_then(|c| c.get("promptCapabilities"))
            .and_then(|p| p.get("image"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut g = self.inner.lock().await;
        g.runtime = Some(Runtime {
            child,
            writer: tx,
            shared,
            version: version.clone(),
        });
        g.mirror = prepared.mirror;
        g.image_prompt = image_prompt;
        Ok(version)
    }

    /// 应用退出时同步收掉子进程（`RunEvent::Exit` 的闭包里不能 await）。
    /// 正常路径走 `stop()`；这里只是防止留下一只没人管的 node。
    pub fn kill_now(&self) {
        if let Ok(mut g) = self.inner.try_lock() {
            if let Some(mut rt) = g.runtime.take() {
                rt.shared.rpc.alive.store(false, Ordering::SeqCst);
                let _ = rt.child.start_kill();
            }
        }
    }

    async fn persist_sessions(&self) {
        let map = {
            let g = self.inner.lock().await;
            g.sessions.clone()
        };
        let _ = tokio::task::spawn_blocking(move || write_sessions(&map)).await;
    }
}

/// 可克隆的回执句柄：审批等待任务在回合主循环之外应答 ACP。
#[derive(Clone)]
pub struct DshResponder {
    writer: mpsc::UnboundedSender<Value>,
    session_id: String,
}

impl DshResponder {
    /// 取消当前回合（DSH 侧停止根活动）。
    pub fn cancel(&self) {
        let _ = self.writer.send(json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": { "sessionId": self.session_id },
        }));
    }

    /// 回执一次授权：`Some(optionId)` = 选它，`None` = cancelled。
    pub fn answer_permission(&self, rpc_id: u64, option_id: Option<&str>) {
        let result = match option_id {
            Some(id) => json!({ "outcome": { "outcome": "selected", "optionId": id } }),
            None => json!({ "outcome": { "outcome": "cancelled" } }),
        };
        let _ = self.writer.send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": result,
        }));
    }
}

/// 一回合：收 update / 审批 / 应答，并负责把会话从共享表里摘掉。
pub struct DshTurn {
    pub session_id: String,
    pub events: mpsc::UnboundedReceiver<DshEvent>,
    responder: DshResponder,
    shared: Arc<RpcShared>,
    prompt_id: u64,
}

impl DshTurn {
    pub async fn next(&mut self) -> Option<DshEvent> {
        self.events.recv().await
    }

    pub fn responder(&self) -> DshResponder {
        self.responder.clone()
    }

    /// 取消当前回合（DSH 侧停止根活动）。
    pub fn cancel(&self) {
        self.responder.cancel();
    }

    /// 回执一次授权：`Some(optionId)` = 选它，`None` = cancelled。
    #[allow(dead_code)]
    pub fn answer_permission(&self, rpc_id: u64, option_id: Option<&str>) {
        self.responder.answer_permission(rpc_id, option_id);
    }

    fn cleanup(&self) {
        if let Ok(mut ids) = self.shared.prompt_ids.lock() {
            ids.remove(&self.prompt_id);
        }
        if let Ok(mut turns) = self.shared.turns.lock() {
            turns.remove(&self.session_id);
        }
    }
}

impl Drop for DshTurn {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// 从 `session/request_permission` 的 params 里挑出「允许一次 / 拒绝」的选项 id。
/// dsh 固定发 `allow-once` / `reject-once`，这里按 kind 兜底，别写死字符串。
pub fn permission_options(params: &Value) -> (Option<String>, Option<String>) {
    let mut allow = None;
    let mut reject = None;
    if let Some(options) = params.get("options").and_then(|v| v.as_array()) {
        for opt in options {
            let kind = opt.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let id = opt.get("optionId").and_then(|v| v.as_str()).map(str::to_string);
            match kind {
                "allow_once" | "allow_always" if allow.is_none() => allow = id,
                "reject_once" | "reject_always" if reject.is_none() => reject = id,
                _ => {}
            }
        }
    }
    (allow, reject)
}

/// 审批提示里的一句话摘要：dsh 只发 toolCallId，有 title 就用 title。
pub fn permission_summary(params: &Value) -> String {
    params
        .get("toolCall")
        .and_then(|t| t.get("title"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "DeepSeek Harness 需要你确认一个操作".to_string())
}

/// 把企微下载到的字节内联成一个 ACP image content block（`{type,mimeType,data}`）。
/// dsh-acp 只认 png/jpeg/webp/gif，其余回退 jpeg（企微图片实际都是这四种之一）。
pub fn acp_image_block(media_type: &str, data_base64: &str) -> Value {
    json!({
        "type": "image",
        "mimeType": normalize_image_media_type(media_type),
        "data": data_base64,
    })
}

/// ACP 文本 content block。
pub fn acp_text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

fn normalize_image_media_type(raw: &str) -> &'static str {
    let base = raw.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "image/png" => "image/png",
        "image/webp" => "image/webp",
        "image/gif" => "image/gif",
        _ => "image/jpeg",
    }
}

struct PreparedLaunch {
    program: PathBuf,
    args: Vec<String>,
    workspace: PathBuf,
    path_value: String,
    mirror: Option<MirrorReport>,
}

/// 阻塞部分：解析 CLI、清陈旧凭证锁、写 patch、校验工作区。
fn prepare_launch(tier: Tier, workspace: &str) -> Result<PreparedLaunch, String> {
    let workspace = PathBuf::from(workspace.trim());
    if !workspace.is_absolute() {
        return Err("DSH 工作区必须是绝对路径".into());
    }
    if !workspace.is_dir() {
        return Err(format!("DSH 工作区不存在：{}", workspace.display()));
    }
    let path_value = dsh_commands::dsh_path_value();
    let version = dsh_commands::dsh_installed_version()
        .ok_or_else(|| "未检测到 DeepSeek Harness：请在「Harness」页安装后再启用 DSH 引擎".to_string())?;
    // ACP 档用托管凭证库；上一次异常退出留下的空锁会让 DSH 等到超时。
    dsh_commands::dsh_clear_stale_lock();
    // 先镜像 DSH 的组合：没有这一步，acp 档只有 deepseek-official，跑起来就是
    // 「no API key for provider route」。
    let mirror = wecom_dsh_profile::dsh_profiles_root()
        .ok_or_else(|| "找不到用户目录（USERPROFILE）".to_string())
        .and_then(|root| wecom_dsh_profile::mirror_profiles(&root))
        .map_err(|e| format!("镜像 DSH 配置失败：{e}"))?;
    if let Some(warn) = &mirror.warning {
        eprintln!("[wecom-dsh] profile 镜像提示：{warn}");
    }
    let cli = dsh_commands::dsh_cli(&path_value)?;
    let patch = write_permission_patch(tier, &workspace)?;

    let mut args = cli.args;
    args.push("--profile".into());
    args.push("acp".into());
    args.push("--patch".into());
    args.push(patch.to_string_lossy().to_string());
    eprintln!(
        "[wecom-dsh] 启动 dsh {version}：{} ({} {} · 镜像 {} 个 bundle)",
        cli.command_line,
        tier.label(),
        workspace.display(),
        mirror.bundles
    );
    Ok(PreparedLaunch {
        program: cli.program,
        args,
        workspace,
        path_value,
        mirror: Some(mirror),
    })
}

/// 把 ACP 的 `configOptions` 解析成「按 provider 分组的模型 + 顺便带出思考强度」。
pub fn parse_config_options(options: &Value) -> DshModelCatalog {
    let mut catalog = DshModelCatalog {
        current: String::new(),
        groups: Vec::new(),
        effort: None,
    };
    let Some(list) = options.as_array() else {
        return catalog;
    };
    for option in list {
        match option.get("id").and_then(|v| v.as_str()).unwrap_or("") {
            "model" => {
                catalog.current = option
                    .get("currentValue")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let Some(groups) = option.get("options").and_then(|v| v.as_array()) else {
                    continue;
                };
                for group in groups {
                    let group_id = group
                        .get("group")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let group_name = group
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(group_id.as_str())
                        .to_string();
                    let entries = group
                        .get("options")
                        .and_then(|v| v.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| {
                                    Some(DshModelOption {
                                        value: item.get("value")?.as_str()?.to_string(),
                                        name: item
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        description: item
                                            .get("description")
                                            .and_then(|v| v.as_str())
                                            .map(str::to_string),
                                    })
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    catalog.groups.push(DshModelGroup {
                        group: group_id,
                        name: group_name,
                        options: entries,
                    });
                }
            }
            "reasoning_effort" => {
                let entries = option
                    .get("options")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                Some(DshModelOption {
                                    value: item.get("value")?.as_str()?.to_string(),
                                    name: item
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    description: item
                                        .get("description")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string),
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                catalog.effort = Some(DshEffort {
                    current: option
                        .get("currentValue")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    options: entries,
                });
            }
            _ => {}
        }
    }
    catalog
}

/// 把 DSH 的原始报错翻成可操作的话（缺 key / 未登录是最常见的两种）。
pub fn friendly_turn_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("no api key")
        || lower.contains("missing_credential")
        || lower.contains("account_sign_in_required")
    {
        return format!(
            "DSH 里这个模型没有可用的密钥或登录：请在 Harness → 模型页给对应 provider 配好 key / 登录，或在上面的「DSH 模型」里换一个有凭据的模型。（原始错误：{raw}）"
        );
    }
    raw.to_string()
}

fn engine_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = base.join("com.ai-workbench.app").join("wecom-dsh");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 每次启动重写：`--patch` 是「追加覆盖」，但单个条目是整体赋值（config 不深合并），
/// 所以这里把该写的 config 写全。`permission` 预设整行关掉：ACP 面没有模式切换，
/// 留着只会让 `settings.permission.defaultPreset` 反过来覆盖我们钉死的档位。
fn write_permission_patch(tier: Tier, workspace: &std::path::Path) -> Result<PathBuf, String> {
    let dir = engine_dir().ok_or_else(|| "找不到应用数据目录".to_string())?;
    let path = dir.join("permission.patch.yml");
    let body = permission_patch_yaml(tier, &workspace.to_string_lossy());
    std::fs::write(&path, body).map_err(|e| format!("写入 DSH 覆盖层失败：{e}"))?;
    Ok(path)
}

/// 覆盖层内容（单独拆出来便于单测）：关掉权限预设整行，钉死沙箱 mode 与审批 policy。
fn permission_patch_yaml(tier: Tier, workspace: &str) -> String {
    let ws = workspace.replace('\\', "\\\\");
    format!(
        "# ai-workbench 生成：企微渠道的 DSH 启动覆盖层，每次启动重写，勿手改。\n\
# 渠道权限 = {tier}\n\
- id: permission\n  disabled: true\n\
- id: sandbox-policy\n  config:\n    mode: {mode}\n    workspaceRoot: \"{ws}\"\n\
- id: approval\n  config:\n    policy: {policy}\n",
        tier = tier.label(),
        mode = tier.sandbox_mode(),
        policy = tier.approval_policy(),
    )
}

fn sessions_path() -> Option<PathBuf> {
    engine_dir().map(|dir| dir.join("sessions.json"))
}

fn load_sessions() -> HashMap<String, String> {
    let Some(path) = sessions_path() else {
        return HashMap::new();
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_sessions(map: &HashMap<String, String>) {
    let Some(path) = sessions_path() else {
        return;
    };
    if let Ok(text) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(path, text);
    }
}

fn spawn_writer(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::UnboundedReceiver<Value>) {
    tauri::async_runtime::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let mut line = frame.to_string();
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdin.flush().await.is_err() {
                break;
            }
        }
    });
}

fn spawn_stdout_reader(
    stdout: tokio::process::ChildStdout,
    shared: Arc<RuntimeShared>,
    writer: mpsc::UnboundedSender<Value>,
) {
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => handle_frame(&line, &shared, &writer),
                Ok(None) => break,
                Err(e) => {
                    shared.push_stderr(&format!("读取 DSH 输出失败：{e}"));
                    break;
                }
            }
        }
        shared.rpc.alive.store(false, Ordering::SeqCst);
        // 进程没了（被杀 / 崩溃 / 被重启）：把在跑的回合立即判失败，
        // 否则只能等 900 秒看门狗，企微那边从头到尾一片空白。
        let sessions: Vec<String> = shared
            .rpc
            .turns
            .lock()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
        for session in sessions {
            route(
                &shared.rpc.turns,
                &session,
                DshEvent::Done(Err("DSH 进程已退出（可能被重启），本轮已中断".into())),
            );
        }
    });
}

fn spawn_stderr_reader(stderr: tokio::process::ChildStderr, shared: Arc<RuntimeShared>) {
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            shared.push_stderr(&line);
        }
    });
}

/// 一帧一行。分四类：请求应答、prompt 应答、会话通知、服务端请求。
fn handle_frame(line: &str, shared: &Arc<RuntimeShared>, writer: &mpsc::UnboundedSender<Value>) {
    let text = line.trim();
    if text.is_empty() {
        return;
    }
    let frame: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            shared.push_stderr(&format!("DSH 输出无法解析：{e}"));
            return;
        }
    };
    let method = frame.get("method").and_then(|v| v.as_str());
    let id = frame.get("id").and_then(|v| v.as_u64());

    // 请求应答（initialize / session new|resume|close …）
    if method.is_none() {
        if let Some(id) = id {
            // 先看是不是某一轮 prompt 的应答
            let prompt_session = shared
                .rpc
                .prompt_ids
                .lock()
                .ok()
                .and_then(|mut ids| ids.remove(&id));
            if let Some(session) = prompt_session {
                let result = match frame.get("error") {
                    Some(err) => Err(rpc_error_text(err)),
                    None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                };
                route(&shared.rpc.turns, &session, DshEvent::Done(result));
                return;
            }
            let waiter = shared
                .rpc
                .pending
                .lock()
                .ok()
                .and_then(|mut map| map.remove(&id));
            if let Some(tx) = waiter {
                let value = match frame.get("error") {
                    Some(err) => Err(rpc_error_text(err)),
                    None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(value);
            }
        }
        return;
    }

    match method.unwrap_or("") {
        "session/update" => {
            let params = frame.get("params").cloned().unwrap_or(Value::Null);
            let Some(session) = params.get("sessionId").and_then(|v| v.as_str()) else {
                return;
            };
            let update = params.get("update").cloned().unwrap_or(Value::Null);
            route(&shared.rpc.turns, session, DshEvent::Update(update));
        }
        "session/request_permission" => {
            let params = frame.get("params").cloned().unwrap_or(Value::Null);
            let session = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let routed = id.map(|rpc_id| {
                route(
                    &shared.rpc.turns,
                    &session,
                    DshEvent::Permission { rpc_id, params },
                )
            });
            if routed != Some(true) {
                // 没有回合在等：按 fail-closed 回 cancelled，别让 DSH 挂住。
                if let Some(id) = id {
                    let _ = writer.send(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "outcome": { "outcome": "cancelled" } },
                    }));
                }
            }
        }
        _ => {}
    }
}

fn route(
    turns: &StdMutex<HashMap<String, mpsc::UnboundedSender<DshEvent>>>,
    session: &str,
    event: DshEvent,
) -> bool {
    if let Ok(map) = turns.lock() {
        if let Some(tx) = map.get(session) {
            return tx.send(event).is_ok();
        }
    }
    false
}

fn rpc_error_text(err: &Value) -> String {
    let message = err
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("未知错误");
    match err.get("code").and_then(|v| v.as_i64()) {
        Some(code) => format!("{message}（code {code}）"),
        None => message.to_string(),
    }
}

async fn rpc_call(
    shared: &Arc<RuntimeShared>,
    writer: &mpsc::UnboundedSender<Value>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let id = shared.rpc.next_id();
    let (tx, rx) = oneshot::channel();
    {
        let mut map = shared
            .rpc
            .pending
            .lock()
            .map_err(|e| format!("锁失败：{e}"))?;
        map.insert(id, tx);
    }
    let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    if writer.send(frame).is_err() {
        if let Ok(mut map) = shared.rpc.pending.lock() {
            map.remove(&id);
        }
        return Err("DSH 发送通道已关闭".into());
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(e))) => Err(format!("{method} 失败：{e}")),
        Ok(Err(_)) => Err(format!("{method} 响应通道已关闭")),
        Err(_) => {
            if let Ok(mut map) = shared.rpc.pending.lock() {
                map.remove(&id);
            }
            Err(format!("{method} 超时（{timeout:?}）"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_mapping_covers_all_four() {
        assert_eq!(Tier::parse("read").sandbox_mode(), "read-only");
        assert_eq!(Tier::parse("workspace").sandbox_mode(), "workspace-write");
        assert_eq!(Tier::parse("review").sandbox_mode(), "workspace-write");
        assert_eq!(Tier::parse("full").sandbox_mode(), "danger-full-access");
        assert_eq!(Tier::parse("full").approval_policy(), "never");
        assert_eq!(Tier::parse("workspace").approval_policy(), "ask");
        // 未知值 fail-closed 到只读
        assert_eq!(Tier::parse("bogus").sandbox_mode(), "read-only");
    }

    #[test]
    fn permission_options_prefer_the_once_kinds() {
        let params = json!({
            "sessionId": "s1",
            "options": [
                { "optionId": "allow-once", "kind": "allow_once", "name": "Allow once" },
                { "optionId": "reject-once", "kind": "reject_once", "name": "Reject" },
            ]
        });
        let (allow, reject) = permission_options(&params);
        assert_eq!(allow.as_deref(), Some("allow-once"));
        assert_eq!(reject.as_deref(), Some("reject-once"));
    }

    #[test]
    fn permission_summary_falls_back_when_title_missing() {
        let params = json!({ "toolCall": { "toolCallId": "c1" } });
        assert!(permission_summary(&params).contains("DeepSeek Harness"));
    }

    #[test]
    fn acp_image_block_normalizes_media_type() {
        let b = acp_image_block("image/jpeg; charset=binary", "AAAA");
        assert_eq!(b.get("type").and_then(|v| v.as_str()), Some("image"));
        assert_eq!(b.get("mimeType").and_then(|v| v.as_str()), Some("image/jpeg"));
        assert_eq!(b.get("data").and_then(|v| v.as_str()), Some("AAAA"));
        // png / webp / gif 原样保留，未知类型回退 jpeg
        assert_eq!(normalize_image_media_type("image/PNG"), "image/png");
        assert_eq!(normalize_image_media_type("image/webp"), "image/webp");
        assert_eq!(normalize_image_media_type("image/gif"), "image/gif");
        assert_eq!(normalize_image_media_type("application/octet-stream"), "image/jpeg");
    }

    #[test]
    fn config_options_split_by_provider_and_effort() {
        let options = json!([
            {
                "id": "model",
                "currentValue": "[\"workbuddy\",\"deepseek-v4.1-flash\"]",
                "options": [
                    { "group": "workbuddy", "name": "WorkBuddy", "options": [
                        { "value": "[\"workbuddy\",\"deepseek-v4.1-flash\"]", "name": "Deepseek-V4.1-Flash" }
                    ] },
                    { "group": "mimo", "name": "MiMo", "options": [
                        { "value": "[\"mimo\",\"mimo-v2\"]", "name": "mimo-v2" }
                    ] }
                ]
            },
            {
                "id": "reasoning_effort",
                "currentValue": "high",
                "options": [
                    { "value": "", "name": "Provider default" },
                    { "value": "high", "name": "high" }
                ]
            }
        ]);
        let catalog = parse_config_options(&options);
        assert_eq!(catalog.current, "[\"workbuddy\",\"deepseek-v4.1-flash\"]");
        assert_eq!(catalog.groups.len(), 2);
        assert_eq!(catalog.groups[1].name, "MiMo");
        assert_eq!(catalog.groups[1].options[0].value, "[\"mimo\",\"mimo-v2\"]");
        let effort = catalog.effort.as_ref().expect("应有思考强度");
        assert_eq!(effort.current, "high");
        assert_eq!(effort.options.len(), 2);
        assert!(!catalog.is_empty());
    }

    #[test]
    fn config_options_tolerate_missing_fields() {
        let catalog = parse_config_options(&json!(null));
        assert!(catalog.is_empty());
        assert!(catalog.effort.is_none());
    }

    #[test]
    fn missing_key_error_becomes_actionable() {
        let raw = "Internal error: turn failed: llm-deepseek: no API key for provider route \"deepseek-official\" (code -32603)";
        let text = friendly_turn_error(raw);
        assert!(text.contains("Harness → 模型页"), "{text}");
        assert!(text.contains("-32603"), "原始错误要保留：{text}");
        // 其它错误原样返回
        assert_eq!(friendly_turn_error("boom"), "boom");
    }

    /// 覆盖层必须能被 YAML 解析，且 Windows 反斜杠路径不会把文档写坏。
    #[test]
    fn permission_patch_is_valid_yaml() {
        let body = permission_patch_yaml(Tier::Read, r"D:\ai_project\ai-workbench");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&body).expect("patch 应是合法 YAML");
        let rows = parsed.as_sequence().expect("顶层是行数组");
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[1]
                .get("config")
                .and_then(|c| c.get("mode"))
                .and_then(|m| m.as_str()),
            Some("read-only")
        );
        assert_eq!(
            rows[1]
                .get("config")
                .and_then(|c| c.get("workspaceRoot"))
                .and_then(|m| m.as_str()),
            Some(r"D:\ai_project\ai-workbench")
        );
        assert_eq!(
            rows[2]
                .get("config")
                .and_then(|c| c.get("policy"))
                .and_then(|m| m.as_str()),
            Some("ask")
        );
        assert_eq!(rows[0].get("disabled").and_then(|v| v.as_bool()), Some(true));
    }
}
