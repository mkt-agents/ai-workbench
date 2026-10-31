//! 企微入站 → 策略 → agent_run → 流式回写。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use crate::agent::{
    agent_decide, subscribe_agent_bridge, unsubscribe_agent_bridge, AgentBridgeEvent,
    AgentRunRequest,
};
use crate::agent_protocol::{Block, Message};
use crate::ai_commands::AIModelConfig;
use crate::mcp::McpServerSpec;
use crate::wecom_channel::{ApprovalKind, PendingApproval, WecomState, APPROVAL_TTL};
use crate::wecom_dsh::{permission_options, permission_summary, DshEvent, DshResponder};
use crate::wecom_policy::{GateDecision, MsgDeduper, PolicyState};
use crate::wecom_proto::{self, EventCallback, MsgCallback};
use crate::wecom_session::{self, SessionKey};
use crate::wecom_ws::WecomSocket;

/// DSH 单回合上限：比天玑的 420s 看门狗宽，因为 ACP 侧可能连着跑很久的工具。
const DSH_TURN_TIMEOUT: Duration = Duration::from_secs(600);
/// 回合里超过这么久没有任何事件，就给聊天里补一句「还在处理」，别让人对着「思考中」干等。
const DSH_STALL_NOTE: Duration = Duration::from_secs(90);
/// 等同一会话上一轮结束时，超过这么久提示用户「在排队」。
const SESSION_QUEUE_NOTE: Duration = Duration::from_secs(8);
/// 非排障 Markdown 真流式：累计正文节流推送到企微 stream（与路线图 300–400ms 对齐）。
const ANSWER_STREAM_THROTTLE: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WecomAgentConfig {
    pub config: AIModelConfig,
    #[serde(default)]
    pub fallback_config: Option<AIModelConfig>,
    #[serde(default)]
    pub repo_path: String,
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub permission: String,
    #[serde(default = "default_true")]
    pub tools: bool,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub diff_confirm: bool,
    #[serde(default)]
    pub always_allow: Vec<String>,
    #[serde(default)]
    pub web_allow: Vec<String>,
    #[serde(default)]
    pub web_tier: String,
    #[serde(default)]
    pub web_search_provider: String,
    #[serde(default)]
    pub web_search_key: String,
    #[serde(default)]
    pub subagents: bool,
    #[serde(default = "default_true")]
    pub parallel_reads: bool,
    #[serde(default = "default_true")]
    pub retries: bool,
    #[serde(default = "default_true")]
    pub grep_ignore: bool,
    #[serde(default)]
    pub plan_first: bool,
    #[serde(default)]
    pub context_tokens: usize,
    #[serde(default)]
    pub result_cap: usize,
    #[serde(default = "default_true")]
    pub anthropic_stream: bool,
    #[serde(default = "default_true")]
    pub mcp_enabled: bool,
    #[serde(default)]
    pub mcp: Vec<McpServerSpec>,
    #[serde(default)]
    pub system_extra: String,
}

fn default_true() -> bool {
    true
}

pub struct Orchestrator {
    pub sock: Arc<WecomSocket>,
    pub policy: Arc<Mutex<PolicyState>>,
    pub agent: Arc<Mutex<Option<WecomAgentConfig>>>,
    pub bot_display_name: Arc<Mutex<String>>,
    /// Optional self-introduction returned verbatim for full-sentence identity prompts.
    pub self_intro: Arc<Mutex<String>>,
    /// 回话引擎：`tianji` | `dsh`（由设置页下发）
    pub engine: Arc<Mutex<String>>,
    /// 问答模式：`doc`（文档优先）| `doc_code`（文档+代码回退）| `code`（仅代码）。
    pub answer_mode: Arc<Mutex<String>>,
    /// 逆向文档目录（相对工作区），默认 `doc`。
    pub doc_dir: Arc<Mutex<String>>,
    deduper: Arc<Mutex<MsgDeduper>>,
    /// 同会话串行
    session_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// 当前每个会话正在跑的那一轮（供「停止」指令真正取消 + 收尾卡住的气泡）。
    running: Arc<Mutex<HashMap<String, RunningTurn>>>,
    pub admission: crate::wecom_admission::Admission,
    /// Invalidates every queued/in-flight ingress on channel reconfiguration.
    pub generation: tokio::sync::watch::Sender<u64>,
    /// Serializes detached DSH startup with channel transitions.
    pub dsh_lifecycle: Arc<Mutex<()>>,
    retiring: AtomicBool,
    /// Only requests that have been accepted for a visible reply (including queues).
    reply_targets: Arc<Mutex<HashMap<String, (String, String)>>>,

}

/// 一个会话正在进行中的回合：记录出站目标 + 如何取消。
struct RunningTurn {
    /// 平台回帧用的 req_id 与当前正文流 id（取消后往这条补 finish 帧，收尾「正在思考…」）。
    req_id: String,
    stream_id: String,
    initiator: String,
    cancel: CancelHandle,
}

/// 取消句柄：两档引擎各一套。
enum CancelHandle {
    /// 天玑档：按 request_id 走 `cancellation::cancel_request`。
    Tianji { request_id: String },
    /// DSH 档：发 `session/cancel`。
    Dsh { responder: DshResponder },
}

impl RunningTurn {
    /// 真正中断这一轮。返回后调用方还要给 `stream_id` 补一帧 finish 收尾气泡。
    fn cancel(&self) {
        match &self.cancel {
            CancelHandle::Tianji { request_id } => {
                crate::cancellation::cancel_request(request_id);
            }
            CancelHandle::Dsh { responder } => {
                responder.cancel();
            }
        }
    }
}

impl Orchestrator {
    pub fn new(sock: Arc<WecomSocket>) -> Self {
        Self {
            sock,
            policy: Arc::new(Mutex::new(PolicyState {
                require_mention: true,
                ..Default::default()
            })),
            agent: Arc::new(Mutex::new(None)),
            bot_display_name: Arc::new(Mutex::new(String::new())),
            self_intro: Arc::new(Mutex::new(String::new())),
            engine: Arc::new(Mutex::new("tianji".to_string())),
            answer_mode: Arc::new(Mutex::new("auto".to_string())),
            doc_dir: Arc::new(Mutex::new("doc".to_string())),
            deduper: Arc::new(Mutex::new(MsgDeduper::new(2048))),
            session_locks: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(Mutex::new(HashMap::new())),
            admission: crate::wecom_admission::Admission::default(),
            generation: tokio::sync::watch::channel(0).0,
            dsh_lifecycle: Arc::new(Mutex::new(())),
            retiring: AtomicBool::new(false),
            reply_targets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 设置全局并发上限（设置页下发）；越界回落默认值。
    pub fn set_max_jobs(&self, n: usize) {
        self.admission.set_limit(n);
    }

    pub fn handle_msg(self: &Arc<Self>, app: AppHandle, msg: MsgCallback) {
        let generation = self.current_generation();
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            this.on_msg(app, msg, generation).await;
        });
    }

    pub fn current_generation(&self) -> u64 {
        *self.generation.borrow()
    }

    pub async fn invalidate(&self) {
        self.generation.send_modify(|g| *g = g.wrapping_add(1));
        for rt in self.running.lock().await.values() {
            rt.cancel();
        }
    }

    pub fn resume_ingress(&self) { self.retiring.store(false, Ordering::SeqCst); }

    /// Freeze final replies while the original socket/bot still owns their request IDs.
    /// The transport drain is bounded and never claims platform delivery.
    pub async fn retire(&self, reason: &str) {
        self.retiring.store(true, Ordering::SeqCst);
        let targets = self.reply_targets.lock().await.clone();
        crate::wecom_diagnostics::record("wecom-channel", "retirement", "started", Some(reason), None,
            serde_json::json!({"count":targets.len(),"channelGeneration":self.current_generation()}));
        let mut frames = Vec::new();
        for (request, (req_id, stream)) in &targets {
            crate::wecom_progress::finish(request, "cancelled", "channel_changed");
            if let Some(body) = crate::wecom_reply::finish(stream, "通道配置或连接发生变化，本次排查已中断；请查看任务进度后重新发起。") {
                frames.push((request.clone(), wecom_proto::respond_stream(req_id, stream, &body, true)));
            }
            crate::cancellation::cancel_request(request);
            if let Some(id) = request.strip_prefix("wecom-") { crate::cancellation::cancel_request(&format!("wecom-preflight-{id}")); }
        }
        self.invalidate().await;
        let delivery = tokio::time::timeout(Duration::from_secs(2), async {
            for (_, frame) in &frames { self.sock.send_raw(frame.clone()).await?; }
            self.sock.flush().await
        }).await;
        let status = if matches!(delivery, Ok(Ok(()))) { "socket_drained" } else { "delivery_unavailable" };
        for (request, _) in &frames {
            crate::wecom_diagnostics::record(request, "retirement", status, Some(reason), None, serde_json::json!({"final":true}));
        }
    }

    pub fn handle_event(self: &Arc<Self>, app: AppHandle, ev: EventCallback) {
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            this.on_event(app, ev).await;
        });
    }

    async fn on_event(&self, _app: AppHandle, ev: EventCallback) {
        if ev.eventtype == "disconnected_event" {
            eprintln!("[wecom] disconnected_event — will reconnect");
            return;
        }
        if ev.eventtype != "enter_chat" {
            return;
        }
        let require_mention = self.policy.lock().await.require_mention;
        let welcome = if require_mention {
            "你好，我是 AI Workbench 天玑助手。直接发问题即可；群聊里请先 @我。"
        } else {
            "你好，我是 AI Workbench 天玑助手。直接发问题即可。"
        };
        let payload = wecom_proto::respond_welcome_text(&ev.req_id, welcome);
        let _ = self.sock.send_raw(payload).await;
    }

    async fn on_msg(&self, app: AppHandle, msg: MsgCallback, generation: u64) {
        let mut changes = self.generation.subscribe();
        if self.retiring.load(Ordering::SeqCst) || *changes.borrow_and_update() != generation {
            return;
        }
        let stream_id = format!("stream-{}", msg.msgid);
        if crate::wecom_reply::is_finished(&stream_id) {
            return;
        }
        {
            let mut d = self.deduper.lock().await;
            if d.seen_or_insert(&msg.msgid) { return; }
        }
        if !crate::wecom_reply::register(&stream_id) {
            let _ = self
                .sock
                .send_raw(wecom_proto::respond_stream(
                    &msg.req_id,
                    &stream_id,
                    "当前任务登记已满，请稍后再试。\n\n⏱ 本次耗时：0秒",
                    true,
                ))
                .await;
            return;
        }
        let _registration = crate::wecom_reply::RegistrationGuard::new(&stream_id);
        let diagnostic_request = format!("wecom-{}", msg.msgid);
        crate::wecom_diagnostics::bind_transport(&msg.req_id, &diagnostic_request);
        crate::wecom_diagnostics::record(&diagnostic_request, "ingress", "received", None, None,
            serde_json::json!({"msgId":msg.msgid,"streamId":stream_id,"channelGeneration":generation,"processId":std::process::id()}));
        let processing = self.process_msg(app, &msg, &stream_id, generation);
        tokio::pin!(processing);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
        loop { tokio::select! {
            biased;
            _ = changes.changed() => {
                // Close locally: old req_id belongs to the retired socket/bot.
                crate::cancellation::cancel_request(&format!("wecom-{}", msg.msgid));
                crate::cancellation::cancel_request(&format!("wecom-preflight-{}", msg.msgid));
                unsubscribe_agent_bridge(&format!("wecom-{}", msg.msgid));
                crate::wecom_diagnostics::record(&diagnostic_request, "request", "cancelled", Some("channel_changed"), None, serde_json::json!({}));
                let mut running = self.running.lock().await;
                running.retain(|_, rt| {
                    if rt.stream_id == stream_id { rt.cancel(); false } else { true }
                });
                break;
            }
            _ = &mut processing => {
                crate::wecom_diagnostics::record(&diagnostic_request, "request", "ended", None, None,
                    serde_json::json!({"final":crate::wecom_reply::is_finished(&stream_id)}));
                break;
            }
            _ = heartbeat.tick() => {
                if !self.retiring.load(Ordering::SeqCst) && !crate::wecom_reply::is_finished(&stream_id) {
                    if let Some(note) = crate::wecom_progress::heartbeat(&diagnostic_request) {
                        // A slow/closed socket must not block the cancellation or work future.
                        let _ = tokio::time::timeout(Duration::from_millis(200), self.sock.send_raw(
                            wecom_proto::respond_stream(&msg.req_id, &stream_id, &note, false))).await;
                    }
                }
            }
        } }
        self.reply_targets.lock().await.remove(&diagnostic_request);
    }

    /// Runs after access checks and optional reset, before any AI work.
    async fn reply_self_intro(&self, req_id: &str, stream_id: &str, text: &str) -> bool {
        if !is_self_intro_command(text) { return false; }
        let configured = self.self_intro.lock().await.clone();
        let is_default = configured.trim().is_empty();
        let intro = if is_default { DEFAULT_SELF_INTRO.to_string() } else { configured };
        if let Some(id) = stream_id.strip_prefix("stream-") {
            crate::wecom_diagnostics::record(&format!("wecom-{id}"), "self_intro",
                if is_default { "default" } else { "configured" }, None, None, serde_json::json!({}));
        }
        send_finish(&self.sock, req_id, stream_id, &intro).await;
        true
    }

    async fn process_msg(&self, app: AppHandle, msg: &MsgCallback, stream_id: &String, generation: u64) {
        let bot_name = self.bot_display_name.lock().await.clone();
        let (text, mentioned) = wecom_proto::strip_mention(&msg.text, &bot_name);
        let mut user_text = text;
        let new_session = crate::wecom_investigation_query::new_session_remainder(&user_text);
        if let Some(remainder) = &new_session {
            user_text = remainder.clone();
        }
        if let Some(q) = &msg.quote_text {
            if !q.trim().is_empty() {
                user_text = format!("（引用）{}\n\n{}", q.trim(), user_text);
            }
        }
        if msg.voice_empty {
            let tip = "没能识别语音内容，请直接发文字或图片。";
            send_finish(&self.sock, &msg.req_id, &stream_id, tip).await;
            return;
        }
        if user_text.trim().is_empty() && msg.media.is_empty() && new_session.is_none() {
            // 既无文字也无媒体（表情/位置等）：礼貌提示，不再静默丢
            let tip = "暂不支持该消息类型，请发文字、图片或文件。";
            send_finish(&self.sock, &msg.req_id, &stream_id, tip).await;
            return;
        }

        let is_group = msg.chattype == "group";
        let bot = app
            .state::<WecomState>()
            .applied
            .lock()
            .await
            .as_ref()
            .map(|c| c.bot_id.clone())
            .unwrap_or_default();
        if bot.is_empty()
            || msg.userid.trim().is_empty()
            || (is_group && msg.chatid.as_deref().unwrap_or("").trim().is_empty())
        {
            send_finish(
                &self.sock,
                &msg.req_id,
                &stream_id,
                "消息缺少机器人、用户或群身份，未创建公共会话。",
            )
            .await;
            return;
        }
        let mut session = if is_group {
            let chatid = msg.chatid.clone().unwrap_or_default();
            // @ 判定优先用平台显式字段（body_mentioned）；该字段缺失时回落到
            // 「平台已过滤非@投递」的宽松假设（兼容老策略，避免 strip_mention 漏判导致静默丢）。
            let is_mentioned = match msg.body_mentioned {
                Some(b) => b,
                None => {
                    if !mentioned {
                        crate::wecom_log::write(
                            "mention",
                            &format!(
                                "msgid={} 无显式@字段且未命中 strip_mention，按平台已过滤假设放行",
                                msg.msgid
                            ),
                        );
                    }
                    true
                }
            };
            let decision = {
                let policy = self.policy.lock().await;
                policy.gate_group(&chatid, is_mentioned)
            };
            if decision != GateDecision::Allow {
                crate::wecom_diagnostics::record(&format!("wecom-{}",msg.msgid), "policy", "filtered", Some("group_policy"), None, serde_json::json!({}));
                return;
            }
            SessionKey::group_user(&bot, &chatid, &msg.userid)
        } else {
            let decision = {
                let mut policy = self.policy.lock().await;
                policy.gate_dm(&msg.userid)
            };
            match decision {
                GateDecision::Allow => SessionKey::dm_for(&bot, &msg.userid),
                GateDecision::NeedPairing(code) => {
                    let tip = format!(
                        "首次私聊需要配对。请在 AI Workbench → 设置 → 联网扩展 → 企业微信机器人 中批准配对码：{code}"
                    );
                    send_finish(&self.sock, &msg.req_id, &stream_id, &tip).await;
                    return;
                }
                GateDecision::DenySilent | GateDecision::DenyWith(_) => {
                    crate::wecom_diagnostics::record(&format!("wecom-{}",msg.msgid), "policy", "filtered", Some("dm_policy"), None, serde_json::json!({}));
                    return;
                },
            }
        };

        let mut task_id: Option<String> = None;
        let mut task_owner: Option<String> = None;
        if is_group {
            let db = app.state::<crate::DbState>().conn.clone();
            let (b, g, u, t) = (
                bot.clone(),
                msg.chatid.clone().unwrap(),
                msg.userid.clone(),
                user_text.clone(),
            );
            let control = is_stop_command(&user_text) || is_approval_text(&user_text)
                || crate::wecom_progress::is_status_command(&user_text)
                || is_self_intro_command(&user_text);
            let routed = tokio::task::spawn_blocking(move || {
                let conn = db.lock().map_err(|_| "共享任务数据库锁失败".to_string())?;
                if control {
                    crate::wecom_tasks::lookup_route(
                        &conn,
                        &b,
                        &g,
                        &u,
                        chrono::Utc::now().timestamp_millis(),
                    )
                } else {
                    crate::wecom_tasks::route(
                        &conn,
                        &b,
                        &g,
                        &u,
                        &t,
                        chrono::Utc::now().timestamp_millis(),
                    )
                }
            })
            .await;
            let routed = match routed {
                Ok(Ok(r)) => r,
                other => {
                    send_finish(
                        &self.sock,
                        &msg.req_id,
                        &stream_id,
                        &format!("共享任务路由失败：{other:?}"),
                    )
                    .await;
                    return;
                }
            };
            if let Some(ended) = &routed.ended_task_id {
                let ended_session =
                    SessionKey::shared_task(&bot, msg.chatid.as_deref().unwrap(), ended);
                let ended_key = ended_session.as_str();
                closed_sessions().lock().unwrap().insert(ended_key.clone());
                if let Some(rt) = self.running.lock().await.remove(&ended_key) {
                    rt.cancel();
                    send_finish(
                        &self.sock,
                        &rt.req_id,
                        &rt.stream_id,
                        "共享任务已结束，本轮停止。",
                    )
                    .await;
                }
                app.state::<WecomState>()
                    .pending_approvals
                    .lock()
                    .await
                    .retain(|_, p| p.owner.session != ended_key);
                app.state::<WecomState>()
                    .dsh
                    .reset_channel_session(&ended_key)
                    .await;
                // Task identity is never reused; preserve archived reply history, remove live metadata after the current turn settles.
                let cleanup_session = ended_session.clone();
                match tokio::task::spawn_blocking(move || {
                    wecom_session::clear_session_checked(&cleanup_session)
                })
                .await
                {
                    Ok(Ok(())) => {}
                    other => crate::wecom_log::write(
                        "task-cleanup",
                        &format!("history cleanup incomplete: {other:?}"),
                    ),
                }
            }
            task_owner = routed.owner_id;
            task_id = routed.task_id;
            if let Some(reply) = routed.reply {
                send_finish(&self.sock, &msg.req_id, &stream_id, &reply).await;
                return;
            }
            if let Some(task) = &task_id {
                session = SessionKey::shared_task(&bot, msg.chatid.as_deref().unwrap(), task);
            }
        } else if crate::wecom_tasks::is_task_command(&user_text) {
            send_finish(
                &self.sock,
                &msg.req_id,
                &stream_id,
                "共享任务仅在群内创建和加入；私聊保持个人会话。",
            )
            .await;
            return;
        }
        let owner = crate::wecom_channel::ApprovalOwner {
            bot: bot.clone(),
            chat: if is_group {
                msg.chatid.clone().unwrap()
            } else {
                format!("dm:{}", msg.userid)
            },
            user: msg.userid.clone(),
            session: session.as_str(),
        };
        if let Some(answer) = self.try_resolve_approval(&app, &user_text, &owner).await {
            send_finish(&self.sock, &msg.req_id, &stream_id, &answer).await;
            return;
        }
        if new_session.is_some() {
            if task_id.is_some() && task_owner.as_deref() != Some(msg.userid.as_str()) {
                send_finish(&self.sock, &msg.req_id, stream_id, "共享任务仅创建者可重置会话；退出任务后可开启个人新会话。").await;
                return;
            }
            let key = session.as_str();
            let lock = self.session_locks.lock().await.entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(()))).clone();
            // Never clear history while an older turn can still append to it.
            let Ok(_reset_guard) = lock.try_lock_owned() else {
                send_finish(&self.sock, &msg.req_id, stream_id, "当前会话仍有任务运行，请先停止或等待完成，再开启新会话。").await;
                return;
            };
            let cleanup = session.clone();
            if !matches!(tokio::task::spawn_blocking(move || wecom_session::clear_session_checked(&cleanup)).await, Ok(Ok(()))) {
                send_finish(&self.sock, &msg.req_id, stream_id, "会话历史重置失败，未开始新会话；请查看本机日志后重试。").await;
                return;
            }
            let state = app.state::<WecomState>();
            let repo = state.dsh.workspace().await;
            let (addr, header) = state.skywalking.lock().await.clone();
            let offset = *state.skywalking_timezone.lock().await;
            let query_key = crate::wecom_investigation::context_key(&bot, &key, &key, &repo, &addr, &header, offset);
            crate::wecom_investigation::clear_context_for(&query_key);
            state.dsh.reset_channel_session(&key).await;
            crate::wecom_diagnostics::record(&format!("wecom-{}", msg.msgid), "session", "reset", None, None,
                serde_json::json!({"sessionKey":key}));
            if user_text.trim().is_empty() {
                send_finish(&self.sock, &msg.req_id, stream_id, "新会话已开始。直接发送问题即可；回复「你是谁」可查看助手介绍。").await;
                return;
            }
        }
        if msg.media.is_empty() && self.reply_self_intro(&msg.req_id, stream_id, &user_text).await {
            return;
        }
        // 「停止」指令：在拿会话串行锁**之前**处理（上一轮没结束它拿不到锁）。
        // 真正取消那一轮并给卡住的「正在思考…」气泡补一帧 finish，而不是当普通消息答一句。
        if is_stop_command(&user_text) {
            let key = session.as_str();
            let running = {
                let mut map = self.running.lock().await;
                if map.get(&key).is_some_and(|rt| {
                    rt.initiator != msg.userid && task_owner.as_deref() != Some(msg.userid.as_str())
                }) {
                    drop(map);
                    send_finish(
                        &self.sock,
                        &msg.req_id,
                        &stream_id,
                        "共享任务仅当前轮发起人或任务创建者可停止。",
                    )
                    .await;
                    return;
                }
                map.remove(&key)
            };
            match running {
                Some(rt) => {
                    rt.cancel();
                    if let Some(id) = rt.stream_id.strip_prefix("stream-") {
                        crate::wecom_progress::finish(&format!("wecom-{id}"), "cancelled", "user_cancelled");
                    }
                    crate::wecom_log::write(
                        "stop",
                        &format!("msgid={} 取消会话 {key} 的在跑回合", msg.msgid),
                    );
                    // 收尾原气泡：往上一轮的 stream 补 finish（幂等，多发无害）。
                    send_finish(
                        &self.sock,
                        &rt.req_id,
                        &rt.stream_id,
                        "已停止，本轮已取消。",
                    )
                    .await;
                    // 再对「停止」这条本身回执一句。
                    send_finish(&self.sock, &msg.req_id, &stream_id, "已停止当前任务。").await;
                }
                None => {
                    crate::wecom_log::write(
                        "stop",
                        &format!("msgid={} 会话 {key} 无在跑回合", msg.msgid),
                    );
                    send_finish(
                        &self.sock,
                        &msg.req_id,
                        &stream_id,
                        "当前没有正在进行的任务。",
                    )
                    .await;
                }
            }
            return;
        }

        if crate::wecom_progress::is_status_command(&user_text) {
            let reply = crate::wecom_progress::status_reply(&session.as_str());
            send_finish(&self.sock, &msg.req_id, stream_id, &reply).await;
            return;
        }
        let progress_request = format!("wecom-{}", msg.msgid);
        {
            // Retirement sets the flag before locking/snapshotting this same map.
            let mut targets = self.reply_targets.lock().await;
            if self.retiring.load(Ordering::SeqCst) || self.current_generation() != generation { return; }
            targets.insert(progress_request.clone(), (msg.req_id.clone(), stream_id.clone()));
        }
        crate::wecom_progress::begin(&session.as_str(), &progress_request);
        struct ProgressGuard(String);
        impl Drop for ProgressGuard {
            fn drop(&mut self) { crate::wecom_progress::finish(&self.0, "ended", "unknown"); }
        }
        let _progress_guard = ProgressGuard(progress_request.clone());
        let Some(queued) = self.admission.queue(&session.as_str()) else {
            crate::wecom_progress::finish(&progress_request, "busy", "busy");
            send_finish(
                &self.sock,
                &msg.req_id,
                &stream_id,
                "本会话已有8条消息等待处理，请稍后再试。",
            )
            .await;
            return;
        };

        let lock = {
            let key = session.as_str();
            let mut map = self.session_locks.lock().await;
            map.entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        // 平台对「收到消息后的首次应答」有时间窗，而引擎冷启动 / 模型首 token 都可能更久；
        // 先无条件占位一条（同一个 stream id），后面的正文继续更新它，避免整轮被平台丢掉。
        // 注意：占位要在拿会话锁**之前**——同一会话上一轮如果卡住，这一条至少不会无声无息。
        self.ack(&msg, &stream_id).await;
        let engine_name = self.engine.lock().await.clone();
        crate::wecom_diagnostics::record(&format!("wecom-{}",msg.msgid), "admission", "queued", None, None,
            serde_json::json!({"engine":engine_name,"sessionKey":session.as_str(),"channelGeneration":generation}));
        crate::wecom_log::write(
            "turn",
            &format!(
                "msgid={} user={} chat={} text_len={} engine={}",
                msg.msgid,
                msg.userid,
                msg.chattype,
                user_text.chars().count(),
                engine_name
            ),
        );

        // 同会话串行：上一轮如果卡住，这里先说明在排队，别让用户以为消息丢了。
        let queue_started = Instant::now();
        let _session_guard = match tokio::time::timeout(SESSION_QUEUE_NOTE, lock.lock()).await {
            Ok(guard) => guard,
            Err(_) => {
                let _ = self
                    .sock
                    .send_raw(wecom_proto::respond_stream(
                        &msg.req_id,
                        &stream_id,
                        "上一条还在处理，本条已排队；处理完会接着回。",
                        false,
                    ))
                    .await;
                lock.lock().await
            }
        };

        crate::wecom_performance::mark_queue(
            &format!("wecom-preflight-{}", msg.msgid),
            queue_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        );
        drop(queued);
        if self.current_generation() != generation {
            return;
        }
        if let Some(task) = &task_id {
            let db = app.state::<crate::DbState>().conn.clone();
            let (b, g, u, t) = (
                bot.clone(),
                msg.chatid.clone().unwrap(),
                msg.userid.clone(),
                task.clone(),
            );
            let valid = tokio::task::spawn_blocking(move || {
                let c = db.lock().map_err(|_| "db lock".to_string())?;
                crate::wecom_tasks::is_member(
                    &c,
                    &b,
                    &g,
                    &t,
                    &u,
                    chrono::Utc::now().timestamp_millis(),
                )
            })
            .await;
            if !matches!(valid, Ok(Ok(true))) {
                send_finish(
                    &self.sock,
                    &msg.req_id,
                    &stream_id,
                    "共享任务已结束、过期或已退出，本条待处理消息已取消。",
                )
                .await;
                return;
            }
        }
        let Some(_execution_permit) = self.admission.try_acquire() else {
            send_finish(
                &self.sock,
                &msg.req_id,
                &stream_id,
                "当前全局并发已满，请稍后再试。",
            )
            .await;
            return;
        };

        let owner_keys = [
            format!("wecom-{}", msg.msgid),
            format!("wecom-preflight-{}", msg.msgid),
        ];
        {
            let st = app.state::<WecomState>();
            let mut owners = st.approval_owners.lock().await;
            for k in &owner_keys {
                owners.insert(k.clone(), owner.clone());
            }
        }
        struct OwnerCleanup {
            app: AppHandle,
            keys: [String; 2],
        }
        impl Drop for OwnerCleanup {
            fn drop(&mut self) {
                let app = self.app.clone();
                let keys = self.keys.clone();
                tauri::async_runtime::spawn(async move {
                    let st = app.state::<WecomState>();
                    let mut map = st.approval_owners.lock().await;
                    for k in keys {
                        map.remove(&k);
                        st.out_targets.lock().await.remove(&k);
                        st.pending_approvals
                            .lock()
                            .await
                            .retain(|_, p| p.request_id != k);
                    }
                });
            }
        }
        let _owner_cleanup = OwnerCleanup {
            app: app.clone(),
            keys: owner_keys,
        };
        if let Some(task) = &task_id {
            if let Ok(mut labels) = reply_task_labels().lock() {
                labels.insert(stream_id.clone(), task.clone());
            }
            user_text = format!("【共享任务 {task}；发起用户 {}】\n{user_text}", msg.userid);
        }
        // 引擎分流：DSH 档不读天玑的模型快照，直接交给常驻 ACP 进程。
        if engine_name == "dsh" {
            // 与天玑档对齐：登记出站目标，供 WECOM_ATTACH / wecom_media 把文件发回本回合。
            {
                let st = app.state::<WecomState>();
                st.out_targets.lock().await.insert(
                    format!("wecom-{}", msg.msgid),
                    crate::wecom_channel::OutTarget {
                        req_id: msg.req_id.clone(),
                        stream_id: stream_id.clone(),
                    },
                );
            }
            let result = self
                .run_dsh_turn(&app, &msg, &session, &user_text, &stream_id)
                .await;
            {
                let st = app.state::<WecomState>();
                st.out_targets
                    .lock()
                    .await
                    .remove(&format!("wecom-{}", msg.msgid));
            }
            if let Err(e) = result {
                crate::wecom_progress::finish(&progress_request, "failed", "failed");
                crate::wecom_diagnostics::record(&format!("wecom-{}",msg.msgid), "engine", "failed", Some("dsh_turn_failed"), None, serde_json::json!({"engine":"dsh"}));
                eprintln!("[wecom] dsh turn failed: {e}");
                let tip = format!("出错了：{}", crate::wecom_dsh::friendly_turn_error(&e));
                send_finish(&self.sock, &msg.req_id, &stream_id, &tip).await;
            }
            return;
        }

        let agent_cfg = match self.agent.lock().await.clone() {
            Some(c) => c,
            None => {
                let tip = "企微通道未绑定模型：请在设置里保存一次配置。";
                send_finish(&self.sock, &msg.req_id, &stream_id, tip).await;
                return;
            }
        };

        // 把文字 + 图片/文件附件组装成一条带 vision 块的用户消息
        let user_msg =
            build_user_message(&app, &msg.msgid, &user_text, &msg.media, agent_cfg.vision).await;

        // 记录当前回合的出站目标，供 wecom_send_image/file 把媒体发回同一条对话
        {
            let st = app.state::<WecomState>();
            st.out_targets.lock().await.insert(
                format!("wecom-{}", msg.msgid),
                crate::wecom_channel::OutTarget {
                    req_id: msg.req_id.clone(),
                    stream_id: format!("s-{}", msg.msgid),
                },
            );
        }
        let result = self
            .run_agent_and_stream(&app, &msg, &session, &user_text, user_msg, agent_cfg)
            .await;

        // 回合结束，清掉出站目标
        {
            let st = app.state::<WecomState>();
            st.out_targets
                .lock()
                .await
                .remove(&format!("wecom-{}", msg.msgid));
        }

        if let Err(e) = result {
            crate::wecom_progress::finish(&progress_request, "failed", "failed");
            crate::wecom_diagnostics::record(&format!("wecom-{}",msg.msgid), "engine", "failed", Some("tianji_turn_failed"), None, serde_json::json!({"engine":"tianji"}));
            eprintln!("[wecom] agent turn failed: {e}");
            let tip = format!("出错了：{e}");
            send_finish(&self.sock, &msg.req_id, &stream_id, &tip).await;
        }
    }

    /// DSH（DeepSeek Harness）档的一轮：常驻 ACP 进程跑完 prompt，提交点节流回写。
    ///
    /// 与天玑档的差别：
    /// - 上下文由 DSH 的 session 自己维护，不把 `wecom_session` 历史塞进 prompt；
    /// - 授权走 `session/request_permission` → 企微审批码（只有一次性授权）；
    /// - 入站图片/文件经 wecom_media 落盘；ACP 支持 image 时内联，否则给路径/文本预览；
    /// - 出站用 WECOM_ATTACH:path（宿主 out_targets），非排障正文 350ms 累计流式。
    async fn run_dsh_turn(
        &self,
        app: &AppHandle,
        msg: &MsgCallback,
        session: &SessionKey,
        user_text: &str,
        stream_id: &str,
    ) -> Result<(), String> {
        let request = format!("wecom-preflight-{}", msg.msgid);
        if session_closed(&session.as_str()) {
            return Ok(());
        }
        let _total_phase = crate::wecom_performance::Phase::new(&request, "total", 0);
        let _guard = crate::cancellation::CancelGuard::new(&request);
        self.running.lock().await.insert(
            session.as_str(),
            RunningTurn {
                req_id: msg.req_id.clone(),
                stream_id: stream_id.to_string(),
                initiator: msg.userid.clone(),
                cancel: CancelHandle::Tianji {
                    request_id: request.clone(),
                },
            },
        );
        if session_closed(&session.as_str()) {
            crate::cancellation::cancel_request(&request);
        }
        let result = tokio::select! {
            biased;
            _ = async {
                loop {
                    if crate::cancellation::is_cancelled(&request) || session_closed(&session.as_str()) { break; }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            } => {
                crate::wecom_progress::finish(&request, "cancelled", "user_cancelled");
                crate::wecom_diagnostics::record(&request, "engine", "cancelled", Some("user_cancelled"), None, serde_json::json!({}));
                send_finish(&self.sock, &msg.req_id, stream_id, "已停止，本轮已取消。").await;
                append_reply_history(session,user_text,stream_id,"");
                Ok(())
            },
            _ = tokio::time::sleep(DSH_TURN_TIMEOUT) => {
                crate::cancellation::cancel_request(&request);
                if let Some(rt) = self.running.lock().await.get(&session.as_str()) { rt.cancel(); }
                crate::wecom_progress::finish(&request, "timeout", "timeout");
                crate::wecom_diagnostics::record(&request, "engine", "timeout", Some("turn_deadline"), None, serde_json::json!({}));
                send_finish(&self.sock, &msg.req_id, stream_id, "排障回合达到600秒期限，已停止；未完成项不视为查询无数据。").await;
                append_reply_history(session,user_text,stream_id,"");
                Ok(())
            },
            result = self.run_dsh_turn_inner(app, msg, session, user_text, stream_id, &request) => result,
        };
        self.unregister_running(session, stream_id).await;
        if session_closed(&session.as_str()) {
            app.state::<WecomState>()
                .dsh
                .reset_channel_session(&session.as_str())
                .await;
        }
        result
    }

    async fn run_dsh_turn_inner(
        &self,
        app: &AppHandle,
        msg: &MsgCallback,
        session: &SessionKey,
        user_text: &str,
        stream_id: &str,
        preflight_request: &str,
    ) -> Result<(), String> {
        let state = app.state::<WecomState>();
        let engine = Arc::clone(&state.dsh);
        let req_id = msg.req_id.clone();
        let turn_deadline = tokio::time::Instant::now() + DSH_TURN_TIMEOUT;

        let repo = engine.workspace().await;

        // 问答模式规则也带进 DSH 档（它用 DSH 自己的工具读文档/代码，这里用 prompt 引导先查文档）。
        let answer_mode = self.answer_mode.lock().await.clone();
        let doc_dir = {
            let d = self.doc_dir.lock().await.clone();
            if d.trim().is_empty() {
                "doc".to_string()
            } else {
                d
            }
        };
        let mode_rules = answer_mode_rules(&answer_mode, &doc_dir, user_text, true);
        // 排障类消息 + 已配 SkyWalking 时，Rust 先预取日志/调用链，作为上下文注入 prompt，
        // 让 DSH 结合代码排查——这是 dsh 档的「排障能力」等价实现。
        let (addr, header) = state.skywalking.lock().await.clone();
        let offset = *state.skywalking_timezone.lock().await;
        let bot = state
            .applied
            .lock()
            .await
            .as_ref()
            .map(|c| c.bot_id.clone())
            .unwrap_or_default();
        let context_key = crate::wecom_investigation::context_key(
            &bot,
            &session.as_str(),
            &session.as_str(),
            &repo,
            &addr,
            &header,
            offset,
        );
        let zone = chrono::FixedOffset::east_opt(
            offset.unwrap_or_else(|| chrono::Local::now().offset().local_minus_utc() / 60) * 60,
        )
        .ok_or("OAP时区配置无效")?;
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let investigation = crate::wecom_investigation::investigate(
            &addr,
            &header,
            &context_key,
            user_text,
            chrono::Utc::now().with_timezone(&zone),
            preflight_request,
            progress_tx,
        );
        tokio::pin!(investigation);
        let mut progress_clock = Instant::now() - Duration::from_secs(1);
        let mut progress_open = true;
        let collect_phase =
            crate::wecom_performance::Phase::new(preflight_request, "acquisition", 0);
        let mut outcome = loop {
            tokio::select! {
                result = &mut investigation => break result?,
                note = progress_rx.recv(), if progress_open => {
                    match note {
                        Some(note) if progress_clock.elapsed() >= Duration::from_millis(800) => {
                            let _ = self.sock.send_raw(wecom_proto::respond_stream(&req_id, stream_id, &note, false)).await;
                            progress_clock = Instant::now();
                        }
                        None => progress_open = false,
                        _ => {}
                    }
                }
            }
        };
        drop(collect_phase);
        if crate::cancellation::is_cancelled(preflight_request) {
            return Ok(());
        }
        // Source/parse failures are deterministic replies, never model-rewritten as empty logs.
        if source_failure_reply(&outcome) {
            crate::wecom_progress::finish(preflight_request, "failed", "no_evidence");
            let body = crate::wecom_report::fallback_with_reason(
                outcome.query.as_ref(),
                "",
                "not_executed",
                outcome.clarification.as_deref(),
            );
            send_finish(&self.sock, &req_id, stream_id, &body).await;
            append_reply_history(session, user_text, stream_id, &body);
            return Ok(());
        }
        // 冷启动 / 首次初始化 profile 可能要几十秒：启动放在独立任务里跑（它要拿引擎的
        // start 锁），这里只等结果；超过 3 秒就在**同一个** stream id 上更新一句进度，
        // 不另外开消息（平台对首帧的时间窗已经用 on_msg 的占位帧满足了）。
        let ready_phase =
            crate::wecom_performance::Phase::new(preflight_request, "engine_ready", 0);
        let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
        let dsh = Arc::clone(&engine);
        let lifecycle = Arc::clone(&self.dsh_lifecycle);
        let generation = self.current_generation();
        let generations = self.generation.subscribe();
        tauri::async_runtime::spawn(async move {
            let _lifecycle = lifecycle.lock().await;
            if *generations.borrow() != generation || ready_tx.is_closed() {
                return;
            }
            let result = dsh.ensure_ready().await;
            // A detached startup must not resurrect a disabled/reconfigured channel.
            if *generations.borrow() != generation {
                dsh.stop().await;
            }
            let _ = ready_tx.send(result);
        });
        let ready = tokio::select! {
            res = &mut ready_rx => decode_ready(res),
            _ = tokio::time::sleep(Duration::from_secs(3)) => {
                if let Err(e) = self
                    .sock
                    .send_raw(wecom_proto::respond_stream(
                        &req_id,
                        stream_id,
                        "正在启动 DeepSeek Harness（首次使用需要初始化，可能要几十秒）…",
                        false,
                    ))
                    .await
                {
                    crate::wecom_log::write("stream", &format!("msgid={} send failed: {e}", msg.msgid));
                }
                decode_ready(ready_rx.await)
            }
        };
        ready.map_err(|e| format!("DSH 引擎未就绪：{e}"))?;
        drop(ready_phase);
        let session_phase =
            crate::wecom_performance::Phase::new(preflight_request, "session_setup", 0);

        let key = session.as_str();
        if crate::wecom_investigation_query::starts_new_investigation(user_text) {
            engine.reset_channel_session(&key).await;
        }
        let (acp_session, session_action) = engine.acquire_session_with_action(&key).await?;
        crate::wecom_performance::record_session(
            preflight_request,
            &session_action,
            "unobserved",
            "configured",
        );
        crate::wecom_log::write(
            "dsh",
            &format!("msgid={} key={key} session={acp_session}", msg.msgid),
        );
        // A model/effort acknowledgement is required before prompting; errors never select a silent fallback.
        // Catalog/configuration work shares a 20-second deadline independent of the full turn.
        match tokio::time::timeout(
            Duration::from_secs(20),
            engine.apply_selection(&acp_session),
        )
        .await
        {
            Ok(Ok((model, effort))) => {
                crate::wecom_performance::record_session(preflight_request, &session_action, &model,
                    if effort.is_empty() { "unknown" } else { &effort });
                let public_model = serde_json::from_str::<Vec<String>>(&model).ok()
                    .filter(|v| v.len() == 2).map(|v| v.join("/")).unwrap_or(model);
                crate::wecom_diagnostics::record(preflight_request, "model_selection", "acknowledged", None, None,
                    serde_json::json!({"model":public_model,"effort":effort,"selectionState":"applied","sessionAction":session_action}));
            }
            Ok(Err(e)) => {
                crate::wecom_diagnostics::record(preflight_request, "model_selection", "failed", Some("model_config_failed"), None, serde_json::json!({}));
                return Err(e);
            }
            Err(_) => {
                crate::wecom_diagnostics::record(preflight_request, "model_selection", "timeout", Some("model_config_timeout"), None, serde_json::json!({}));
                return Err("模型配置等待超过20秒，本轮未提交模型任务；请检查模型与思考强度后重试。".into());
            },
        }
        drop(session_phase);
        let mut store = crate::wecom_evidence_store::EvidenceStore::new();
        store.insert(&outcome.context);
        let mut evidence = store.full_context();
        let mut next_payload = store.bounded_prompt(16_000);
        if outcome.may_analyze {
            let source_evidence = evidence.clone();
            let source_repo = repo.clone();
            let hints = tokio::task::spawn_blocking(move || {
                crate::wecom_source_hints::hints(&source_evidence, &source_repo)
            })
            .await
            .unwrap_or_default();
            if !hints.is_empty() {
                let clipped: String = hints.chars().take(8_000).collect();
                next_payload.push_str(&format!("\n【日志对应的定点源码线索；已验证当前文件内容，优先核对而非全库扫描】\n{clipped}"));
                if hints.chars().count() > 8_000 {
                    next_payload.push_str(
                        "\n源码线索展示已截断，按相对路径定点展开；不可据省略内容断因。\n",
                    );
                }
            }
        }
        let mut final_report: Option<crate::wecom_report::DiagnosticReport> = None;
        let mut has_evidence = outcome.may_analyze;
        let mut assessed_level = "unknown".to_string();
        let mut assessed_gaps = outcome.decision_gaps.clone();
        let mut last_stop_reason = String::new();
        let mut forced_context_used = false;
        if outcome.source_ready {
            if let Some(original) = outcome.query.clone() {
                let mut query_round = 0usize;
                let mut expansion_round = 0usize;
                for _analysis_pass in 0..4 {
                    if tokio::time::Instant::now() >= turn_deadline {
                        break;
                    }
                    let note = if has_evidence {
                        "已取得日志，正在按日志定点核对代码并制定补查…"
                    } else {
                        "日志源正常但业务未命中，正在核对日志字段与服务映射，不生成业务根因结论…"
                    };
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(&req_id, stream_id, note, false))
                        .await;
                    let budget_hint = format!("宿主剩余HTTP={}，剩余采集={}ms，已补查={}轮。余额不足时不要再生成查询计划，直接输出当前已验证的result/direct_cause层级diagnostic_report并将影响当前结论与扩展取证的缺口分开列出。",outcome.budget.remaining(),120_000u64.saturating_sub(outcome.collected_ms),query_round);
                    let instruction = format!("{budget_hint}\n{}\n原问题：{}\n固定查询范围：{} 至 {}\n已有证据：{}\n本次只输出协议JSON：需要查日志给queries，需要展开已采集摘录给read_evidence_ids，已有证据则直接给diagnostic_report，用summary/时间线/分层原因/代码核对/误判线索/缺口构成完整报告，禁止只给机器式claims清单，不要先输出停止计划再重复总结。以下仅为本轮新增证据/索引，之前证据在同会话内沿用；已核验且未改变的代码不要重复读。没有日志时只查项目日志配置、服务标识、字段别名和日志模板；不要搜索本地.log，不要把业务ID在源码无命中当故障证据。",
                        crate::wecom_investigation_plan::planning_instruction(), user_text, original.start, original.end, next_payload);
                    let mut draft = self
                        .dsh_plan_text(
                            app,
                            &engine,
                            &acp_session,
                            &instruction,
                            preflight_request,
                            &req_id,
                            stream_id,
                            turn_deadline,
                            if _analysis_pass == 0 {
                                "planning_initial"
                            } else if expansion_round > 0 && query_round == 0 {
                                "evidence_expand"
                            } else {
                                "planning_followup"
                            },
                        )
                        .await?;
                    let mut parsed = crate::wecom_investigation_plan::parse_plan(
                        &draft, &original, &evidence, &repo,
                    );
                    if let Err(error) = &parsed {
                        crate::wecom_performance::note_validation(
                            preflight_request,
                            crate::wecom_investigation_plan::validation_error_code(error),
                        );
                        draft = self.dsh_plan_text(app, &engine, &acp_session, &format!("补查计划未通过本地校验：{error}。请修正一次，仅输出符合schema的JSON；不要执行计划。{}", crate::wecom_investigation_plan::planning_instruction()), preflight_request, &req_id, stream_id, turn_deadline, "planning_repair").await?;
                        parsed = crate::wecom_investigation_plan::parse_plan(
                            &draft, &original, &evidence, &repo,
                        );
                    }
                    let mut plan = match parsed {
                        Ok(plan) => plan,
                        Err(error) => {
                            crate::wecom_performance::note_validation(preflight_request,
                                crate::wecom_investigation_plan::validation_error_code(&error));
                            evidence.push_str(
                                "\n补查计划两次未通过校验，未执行；当前结论仅限已有证据。\n",
                            );
                            break;
                        }
                    };
                    assessed_level = plan.conclusion_level.clone();
                    assessed_gaps = plan.gaps.clone();
                    last_stop_reason = plan.stop_reason.clone();
                    let candidate = plan.diagnostic_report.clone().or_else(|| {
                        plan.final_answer.as_ref().map(|a| {
                            crate::wecom_report::from_legacy(
                                a,
                                &assessed_level,
                                &assessed_gaps,
                                &last_stop_reason,
                            )
                        })
                    });
                    if let Some(report) = candidate {
                        let allowed = if plan.diagnostic_report.is_some()
                            && !report.gaps.iter().any(|g| g.blocking)
                            && outcome.may_analyze
                        {
                            report.level.as_str()
                        } else {
                            permitted_conclusion_level(&assessed_level, &assessed_gaps, &outcome)
                        };
                        if can_render_verified_answer(&assessed_level, allowed, has_evidence) {
                            // Direct rendering avoids a second model summary without bypassing coverage limits.
                            final_report = Some(report);
                            break;
                        }
                        assessed_gaps
                            .push("结构化结论超过当前证据覆盖允许层级，需要保留未完成项".into());
                    }
                    if !plan.read_evidence_ids.is_empty() {
                        if expansion_round >= 1 {
                            last_stop_reason = "已达本轮摘录展开次数上限".into();
                            break;
                        }
                        next_payload = store.expand(&plan.read_evidence_ids, 12_000)?;
                        expansion_round += 1;
                        continue;
                    }
                    let missing_decision = !assessed_gaps.is_empty()
                        || !outcome.decision_gaps.is_empty()
                        || outcome.incomplete_pages > 0
                        || outcome.truncated_count > 0;
                    if plan.queries.is_empty() && missing_decision && !forced_context_used {
                        forced_context_used = true;
                        let mandatory = crate::wecom_investigation_plan::decision_context_plan(
                            &evidence, &original,
                        )?;
                        if !mandatory.queries.is_empty() {
                            evidence.push_str("\n【结束检查】当前仅定位结果/失败出口，仍有WARN或决策上下文缺口，先执行证据锚定回溯。\n");
                            plan = mandatory;
                        }
                    }
                    if plan.queries.is_empty() {
                        break;
                    }
                    if query_round >= 2
                        || outcome.budget.remaining() == 0
                        || outcome.collected_ms >= 120_000
                    {
                        last_stop_reason = "日志补查已达次数/预算上限，保留当前证据".into();
                        break;
                    }
                    query_round += 1;
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(
                            &req_id,
                            stream_id,
                            &format!("正在执行第{}轮代码线索补查…", query_round),
                            false,
                        ))
                        .await;
                    crate::wecom_progress::phase(preflight_request, "supplemental");
                    let extra = crate::wecom_investigation::supplemental(
                        &addr,
                        &header,
                        &original,
                        &plan.queries,
                        preflight_request,
                        &outcome.budget,
                        120_000u64.saturating_sub(outcome.collected_ms),
                    )
                    .await?;
                    merge_investigation_coverage(&mut outcome, &extra);
                    crate::wecom_investigation::record_cumulative(&outcome, preflight_request);
                    has_evidence |= extra.may_analyze;
                    let extra_context = extra
                        .context
                        .replace("[L", &format!("[R{}L", query_round))
                        .replace("[T", &format!("[R{}T", query_round));
                    next_payload = store.insert(&format!(
                        "【补查轮次{}；来源候选：{}】\n{}",
                        query_round, plan.reason, extra_context
                    ));
                    evidence = store.full_context();
                    if next_payload.chars().count() > 20_000 {
                        next_payload = format!("本轮新增证据较多，先看优先摘录及索引，缺少记录可read_evidence_ids展开。\n{}", store.bounded_prompt(20_000));
                    }
                    if !extra.source_ready {
                        break;
                    }
                }
            }
            if !has_evidence {
                crate::wecom_progress::finish(preflight_request, "ended", "no_evidence");
                let body = crate::wecom_report::fallback_with_reason(
                    outcome.query.as_ref(),
                    &store.full_context(),
                    "empty",
                    outcome.clarification.as_deref(),
                );
                send_finish(&self.sock, &req_id, stream_id, &body).await;
                append_reply_history(session, user_text, stream_id, &body);
                return Ok(());
            }
        }
        for gap in &assessed_gaps {
            if !outcome.decision_gaps.contains(gap) {
                outcome.decision_gaps.push(gap.clone());
            }
        }
        crate::wecom_investigation::record_cumulative(&outcome, preflight_request);
        if has_evidence {
            if let (Some(report), Some(original)) = (final_report, outcome.query.as_ref()) {
                let rendered =
                    crate::wecom_report::render(&report, original, &store.full_context());
                let body = rendered;
                let mut phase = crate::wecom_performance::Phase::new(
                    preflight_request,
                    "render_verified_answer",
                    0,
                );
                phase.output(&body);
                crate::wecom_progress::finish(preflight_request, "completed", "completed");
                crate::wecom_diagnostics::record(preflight_request, "report", "validated", None, None, serde_json::json!({}));
                send_finish(&self.sock, &req_id, stream_id, &body).await;
                append_reply_history(session, user_text, stream_id, &body);
                return Ok(());
            }
        }
        let protected_answer = !outcome.context.is_empty();
        let answer_level =
            permitted_conclusion_level(&assessed_level, &assessed_gaps, &outcome).to_string();
        let evidence_rules = if has_evidence {
            let level = if has_evidence { "trigger_verified（仅限真实规则/判定值/代码引用齐备且无阻断性缺口的局部结论；否则direct_cause或result）" } else { "unknown" };
            format!("\n结论分层评审：允许最高层级={level}；模型停止理由={last_stop_reason}；尚未解释的决策={:?}；未完成页={}；截断记录={}。必须依次列出结果、直接原因、具体触发原因。过滤为0/STATUS_FILTERED_EMPTY仅是失败出口，不等于已定位具体过滤器。未取得实际规则、判定值及WARN/异常证据时，写‘已确认失败出口，具体触发原因尚未确认’，禁止‘证据链已闭合/根因直接命中’。正常span仅表示没有APM技术错误，不能据此排除业务警告、被捕获异常或降级。先引用日志和时间，再用代码解释分支，再用补查验证。共用兜底文案不唯一指向去重/库存/价格；缺日志不等于分支未执行；说明分页、采样、异步与版本覆盖。", assessed_gaps, outcome.incomplete_pages, outcome.truncated_count)
        } else {
            String::new()
        };
        let prompt_blocks = build_dsh_prompt(
            app,
            &msg.msgid,
            user_text,
            &msg.media,
            engine.image_prompt_enabled().await,
            &format!("{mode_rules}{evidence_rules}{}", if protected_answer { format!("\n最终输出只用于内部验证和统一渲染，用户不会看到JSON。输出{{\"queries\":[],\"reason\":\"简短停止理由\",\"conclusion_level\":\"与报告相同\",\"gaps\":[],\"diagnostic_report\":<下列契约>}}；完整报告的时间/原文由宿主证据提供，勿发自由Markdown或旧claims代替。耗时由宿主追加。\n{}",crate::wecom_report::schema_instruction()) } else { String::new() }),
            &if has_evidence { format!("沿用同会话已返回的证据与核验源码，不要重复全库检索。按最新补查结果直接回答；需要的证据编号如下：\n{}", store.render_index()) } else { evidence.clone() },
        )
        .await;
        let mut answer_phase = crate::wecom_performance::Phase::new(
            preflight_request,
            "final_model",
            prompt_blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .map(|s| s.chars().count())
                .sum(),
        );
        let mut turn = engine.prompt(&acp_session, prompt_blocks).await?;

        // 登记为该会话「在跑的回合」，供「停止」指令 session/cancel + 收尾气泡。
        {
            let mut map = self.running.lock().await;
            map.insert(
                session.as_str(),
                RunningTurn {
                    req_id: req_id.clone(),
                    stream_id: stream_id.to_string(),
                    initiator: msg.userid.clone(),
                    cancel: CancelHandle::Dsh {
                        responder: turn.responder(),
                    },
                },
            );
        }

        let mut acc = String::new();
        let mut answer_buffer = crate::wecom_answer::AnswerBuffer::with_request(protected_answer, preflight_request);
        let mut last_event = std::time::Instant::now();
        // 非排障路径：允许立刻推第一帧，之后按 ANSWER_STREAM_THROTTLE 节流。
        let mut last_answer_stream =
            Instant::now().checked_sub(ANSWER_STREAM_THROTTLE).unwrap_or_else(Instant::now);
        // 最近一次动作（工具名），用来解释「为什么还在等」。
        let mut last_activity = String::new();
        let mut stall_notes = 0u32;
        let mut open_approvals: Vec<u64> = Vec::new();
        let deadline = turn_deadline;
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let final_text = loop {
            let event = tokio::select! {
                ev = turn.next() => ev,
                // 长时间没有任何事件：往同一个气泡里补一句「还在处理」，别让人对着「思考中」干等。
                // 提示按 90s / 180s / 270s… 递增间隔，最多 5 次，不刷屏。
                _ = tick.tick() => {
                    let due = DSH_STALL_NOTE + Duration::from_secs(90 * stall_notes as u64);
                    if last_event.elapsed() >= due && stall_notes < 5 && !crate::wecom_reply::is_finished(stream_id) {
                        stall_notes += 1;
                        let what = if last_activity.is_empty() {
                            "模型还在思考".to_string()
                        } else {
                            format!("最近一步：{last_activity}")
                        };
                        let note = format!("**仍在处理**：已 {} 秒没有新事件，正在等待模型或工具返回。", last_event.elapsed().as_secs());
                        crate::wecom_log::write(
                            "stall",
                            &format!("msgid={} {:?} {}", msg.msgid, last_event.elapsed(), what),
                        );
                        let _ = self
                            .sock
                            .send_raw(wecom_proto::respond_stream(
                                &req_id,
                                stream_id,
                                &note,
                                false,
                            ))
                            .await;
                    }
                    continue;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    turn.cancel();
                    let body = "**本轮已超时**，未完成的模型正文不展示。".to_string();
                    send_finish(&self.sock, &req_id, stream_id, &body).await;
                    break body;
                }
            };
            match event {
                Some(DshEvent::Update(update)) => {
                    answer_phase.event();
                    last_event = std::time::Instant::now();
                    let kind = update
                        .get("sessionUpdate")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // 只播报正文；thought / tool_call / usage 不逐步刷屏，但要留进排障日志：
                    // 「卡住时最后在做什么」只能从这里看出来。
                    if kind == "agent_message_chunk" {
                        if let Some(text) = update
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(|v| v.as_str())
                        {
                            answer_phase.output(text);
                            // protected（排障）路径：push 返回 None，正文全缓冲至 Done 再校验渲染。
                            // 普通问答：push 返回累计快照，节流写入同一 stream（企微 stream 语义=整段替换）。
                            if let Some(snapshot) = answer_buffer.push(text) {
                                acc.push_str(text);
                                if last_answer_stream.elapsed() >= ANSWER_STREAM_THROTTLE
                                    && !crate::wecom_reply::is_finished(stream_id)
                                {
                                    last_answer_stream = Instant::now();
                                    let visible = prepare_wecom_visible_markdown(&snapshot);
                                    let _ = self
                                        .sock
                                        .send_raw(wecom_proto::respond_stream(
                                            &req_id,
                                            stream_id,
                                            &visible,
                                            false,
                                        ))
                                        .await;
                                }
                            }
                        }
                    } else if kind == "tool_call" || kind == "tool_call_update" {
                        if kind == "tool_call" {
                            answer_phase.tool();
                        }
                        if let Some(title) = update
                            .get("title")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            if title != last_activity {
                                crate::wecom_log::write(
                                    "tool",
                                    &format!("msgid={} {kind} {title}", msg.msgid),
                                );
                                last_activity = title.to_string();
                            }
                        }
                    } else if kind == "agent_thought_chunk" {
                        // 思考链不逐步刷屏（聊天里一堆「思考中」反而吵），但要进排障日志，
                        // 并更新「最近一步」——卡住时 stall 提示能说清是在想还是在等工具。
                        if let Some(text) = update
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            last_activity = "模型正在思考".to_string();
                            let head: String = text.chars().take(80).collect();
                            crate::wecom_log::write(
                                "thought",
                                &format!("msgid={} {head}", msg.msgid),
                            );
                        }
                    }
                }
                Some(DshEvent::Permission { rpc_id, params }) => {
                    open_approvals.push(rpc_id);
                    let summary = permission_summary(&params);
                    let (allow_option, reject_option) = permission_options(&params);
                    let (code, reply) =
                        register_dsh_approval(app, summary.clone(), preflight_request).await;
                    let tip = format!(
                        "🔐 DeepSeek Harness 需要你确认：{summary}\n回复「{code} 1」同意；回复「{code} 2」拒绝（5 分钟内有效）。",
                        summary = summary,
                        code = code
                    );
                    // 审批提示单独发一条**已结束**的消息：正文流还在跑，而平台对未结束的
                    // 流式更新不一定立刻显示；这条必须让用户看见，否则只能干等 5 分钟超时。
                    crate::wecom_log::write("perm", &format!("msgid={} {summary}", msg.msgid));
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(
                            &req_id,
                            &format!("appr-{}", msg.msgid),
                            &tip,
                            true,
                        ))
                        .await;

                    // 等待用户作答不阻塞主循环：独立的等待端负责回执 ACP。
                    let responder = turn.responder();
                    tauri::async_runtime::spawn(async move {
                        let decision = match tokio::time::timeout(APPROVAL_TTL, reply).await {
                            Ok(Ok(allow)) => Some(allow),
                            _ => None,
                        };
                        match decision {
                            Some(true) => {
                                responder.answer_permission(rpc_id, allow_option.as_deref())
                            }
                            Some(false) => {
                                responder.answer_permission(rpc_id, reject_option.as_deref())
                            }
                            None => responder.answer_permission(rpc_id, None),
                        }
                    });
                }
                Some(DshEvent::Done(result)) => {
                    let status = match &result {
                        Ok(v) if v["stopReason"] == "cancelled" => "cancelled",
                        Ok(_) => "completed",
                        Err(_) => "failed",
                    };
                    crate::wecom_progress::finish(preflight_request, status,
                        match status { "cancelled" => "user_cancelled", "failed" => "failed", _ => "completed" });
                    crate::wecom_diagnostics::record(preflight_request, "model", status, None, None, serde_json::json!({}));
                    crate::wecom_log::write(
                        "dsh",
                        &format!(
                            "msgid={} done ok={} acc_len={}",
                            msg.msgid,
                            result.is_ok(),
                            acc.chars().count()
                        ),
                    );
                    let body = match result {
                        Ok(value) => {
                            let stop = value
                                .get("stopReason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let note = match stop {
                                "max_tokens" => "\n\n（达到长度上限，回答被截断）",
                                "max_turn_requests" => "\n\n（达到工具步数上限）",
                                "refusal" => "\n\n（模型拒绝了这次请求）",
                                "cancelled" => "\n\n（本轮已取消）",
                                _ => "",
                            };
                            if stop == "cancelled" {
                                "**本轮已取消**，未完成的模型正文不展示。".to_string()
                            } else if protected_answer {
                                let shown = answer_buffer.finish_with_report_ceiling(
                                    outcome.query.as_ref(),
                                    &store.full_context(),
                                    &repo,
                                    &answer_level,
                                    if has_evidence {
                                        "trigger_verified"
                                    } else {
                                        "unknown"
                                    },
                                );
                                format!("{shown}{note}")
                            } else if acc.trim().is_empty() {
                                if stop == "cancelled" {
                                    "（本轮已取消）".to_string()
                                } else {
                                    "（无文本回复）".to_string()
                                }
                            } else {
                                let (visible, attachments) = split_wecom_attach(&acc);
                                if !attachments.is_empty() {
                                    let request_id = format!("wecom-{}", msg.msgid);
                                    deliver_wecom_attachments(app, &request_id, &attachments).await;
                                }
                                format!("{visible}{note}")
                            }
                        }
                        Err(e) => {
                            let e = crate::wecom_dsh::friendly_turn_error(&e);
                            format!("**本轮中断**：{e}。未完成的模型正文不展示。")
                        }
                    };
                    send_finish(&self.sock, &req_id, stream_id, &body).await;
                    break body;
                }
                None => {
                    let body = "**响应意外结束**，未完成的模型正文不展示。".to_string();
                    send_finish(&self.sock, &req_id, stream_id, &body).await;
                    break body;
                }
            }
        };

        // 回合收尾：还挂着的授权统一回 cancelled，别让 DSH 的工具调用悬着。
        if !open_approvals.is_empty() {
            let responder = turn.responder();
            for rpc_id in open_approvals {
                responder.answer_permission(rpc_id, None);
            }
        }
        // 注销「在跑回合」登记（仅当还是本回合时移除，避免误删后来者）。
        self.unregister_running(session, stream_id).await;
        append_reply_history(session, user_text, &stream_id, &final_text);
        Ok(())
    }

    /// Internal read/code planning turn: hold JSON privately; approvals still use the existing channel.
    async fn dsh_plan_text(
        &self,
        app: &AppHandle,
        engine: &Arc<crate::wecom_dsh::DshEngine>,
        acp_session: &str,
        prompt: &str,
        request: &str,
        req_id: &str,
        stream_id: &str,
        deadline: tokio::time::Instant,
        phase_name: &'static str,
    ) -> Result<String, String> {
        let mut phase =
            crate::wecom_performance::Phase::new(request, phase_name, prompt.chars().count());
        let mut turn = engine
            .prompt(acp_session, vec![crate::wecom_dsh::acp_text_block(prompt)])
            .await?;
        struct PlanCancelOnDrop {
            cancel: Option<DshResponder>,
            responder: DshResponder,
            app: AppHandle,
            approvals: Vec<(String, u64)>,
        }
        impl Drop for PlanCancelOnDrop {
            fn drop(&mut self) {
                if let Some(r) = &self.cancel {
                    r.cancel();
                }
                let approvals = std::mem::take(&mut self.approvals);
                if !approvals.is_empty() {
                    let app = self.app.clone();
                    let responder = self.responder.clone();
                    tauri::async_runtime::spawn(async move {
                        for (code, id) in approvals {
                            app.state::<WecomState>()
                                .pending_approvals
                                .lock()
                                .await
                                .remove(&code);
                            responder.answer_permission(id, None);
                        }
                    });
                }
            }
        }
        let mut cleanup = PlanCancelOnDrop {
            cancel: Some(turn.responder()),
            responder: turn.responder(),
            app: app.clone(),
            approvals: Vec::new(),
        };
        let mut text = String::new();
        let mut tick = tokio::time::interval(Duration::from_millis(100));

        let result = loop {
            let event = tokio::select! {
                biased;
                _ = tick.tick() => {
                    if crate::cancellation::is_cancelled(request) {
                        turn.cancel(); break Err("cancelled: 已停止日志补查计划".into());
                    }
                    if tokio::time::Instant::now() >= deadline {
                        turn.cancel(); break Err("timeout: 排障回合已达到600秒期限".into());
                    }
                    continue;
                }
                event = turn.next() => event,
            };
            match event {
                Some(DshEvent::Update(update))
                    if update["sessionUpdate"] == "agent_message_chunk" =>
                {
                    if let Some(chunk) = update["content"]["text"].as_str() {
                        phase.output(chunk);
                        text.push_str(chunk);
                        if text.len() > 64 * 1024 {
                            turn.cancel();
                            break Err("补查计划超过64KiB，已停止".into());
                        }
                    }
                }
                Some(DshEvent::Permission { rpc_id, params }) => {
                    let summary = permission_summary(&params);
                    let (allow, reject) = permission_options(&params);
                    let (code, reply) = register_dsh_approval(app, summary.clone(), request).await;
                    cleanup.approvals.push((code.clone(), rpc_id));
                    let tip = format!("日志取证需确认：{summary}\n回复「{code} 1」允许一次；「{code} 2」取消（5分钟有效）");
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(
                            req_id,
                            &format!("{stream_id}-plan-{code}"),
                            &tip,
                            true,
                        ))
                        .await;
                    let responder = turn.responder();
                    tauri::async_runtime::spawn(async move {
                        let answer = match tokio::time::timeout(APPROVAL_TTL, reply).await {
                            Ok(Ok(true)) => allow,
                            Ok(Ok(false)) => reject,
                            _ => None,
                        };
                        responder.answer_permission(rpc_id, answer.as_deref());
                    });
                }
                Some(DshEvent::Update(update)) => {
                    phase.event();
                    if update["sessionUpdate"] == "tool_call" {
                        phase.tool();
                    }
                }
                Some(DshEvent::Done(Ok(result))) => {
                    if result["stopReason"] == "cancelled" {
                        break Err("cancelled: DSH计划已取消".into());
                    }
                    break Ok(text);
                }
                Some(DshEvent::Done(Err(e))) => break Err(e),
                None => break Err("DSH计划会话意外结束".into()),
            }
        };
        if result.is_ok() {
            cleanup.cancel = None;
        }
        for (code, id) in std::mem::take(&mut cleanup.approvals) {
            app.state::<WecomState>()
                .pending_approvals
                .lock()
                .await
                .remove(&code);
            turn.responder().answer_permission(id, None);
        }
        result
    }

    /// 注销某会话的在跑回合（仅当登记的 stream_id 与本回合一致时移除）。
    async fn unregister_running(&self, session: &SessionKey, stream_id: &str) {
        let key = session.as_str();
        let mut map = self.running.lock().await;
        if let Some(rt) = map.get(&key) {
            if rt.stream_id == stream_id {
                map.remove(&key);
            }
        }
    }

    /// 收到消息先占位一条，满足平台对「首次应答」的时间窗；后面的正文更新同一个 stream id。
    /// 失败只记日志——这一帧丢了也还有后续帧，但日志能说明是不是根本没发出去。
    async fn ack(&self, msg: &MsgCallback, stream_id: &str) {
        match self
            .sock
            .send_raw(wecom_proto::respond_stream(
                &msg.req_id,
                stream_id,
                "正在思考…",
                false,
            ))
            .await
        {
            Ok(()) => crate::wecom_log::write(
                "ack",
                &format!(
                    "msgid={} req_id={} stream={stream_id}",
                    msg.msgid,
                    if msg.req_id.is_empty() {
                        "<empty>"
                    } else {
                        msg.req_id.as_str()
                    }
                ),
            ),
            Err(e) => {
                crate::wecom_log::write("ack", &format!("msgid={} send failed: {e}", msg.msgid))
            }
        }
    }

    async fn run_agent_and_stream(
        &self,
        app: &AppHandle,
        msg: &MsgCallback,
        session: &SessionKey,
        user_text: &str,
        user_msg: crate::agent_protocol::Message,
        agent_cfg: WecomAgentConfig,
    ) -> Result<(), String> {
        let request_id = format!("wecom-{}", msg.msgid);
        crate::wecom_progress::phase(&request_id, "final_model");
        let stream_id = format!("stream-{}", msg.msgid);
        if session_closed(&session.as_str()) {
            return Ok(());
        }
        let history = wecom_session::session_history(session);

        // 登记为该会话「在跑的回合」：「停止」指令据此 cancel_request + 收尾气泡。
        {
            let mut map = self.running.lock().await;
            map.insert(
                session.as_str(),
                RunningTurn {
                    req_id: msg.req_id.clone(),
                    stream_id: stream_id.clone(),
                    initiator: msg.userid.clone(),
                    cancel: CancelHandle::Tianji {
                        request_id: request_id.clone(),
                    },
                },
            );
        }

        let mut permission = agent_cfg.permission.clone();
        if permission.trim().is_empty() {
            permission = "read".into();
        }
        // 渠道默认不悄悄升到 full
        if permission.eq_ignore_ascii_case("full") {
            // 允许用户显式配置；不在这里改写
        }

        let where_from = if session.kind == "group" {
            "企业微信群聊"
        } else {
            "企业微信私聊"
        };
        // 问答模式 + 文档目录：决定「先查文档还是先读代码」。
        let answer_mode = self.answer_mode.lock().await.clone();
        let doc_dir = {
            let d = self.doc_dir.lock().await.clone();
            if d.trim().is_empty() {
                "doc".to_string()
            } else {
                d
            }
        };
        let mode_rules = answer_mode_rules(&answer_mode, &doc_dir, user_text, false);
        let skywalking_ready = {
            let st = app.state::<WecomState>();
            let empty = st.skywalking.lock().await.0.trim().is_empty();
            !empty
        };
        // 已配 SkyWalking 且本条是排障意图：把抽到的线索直接喂进 system，让模型第一刀就带参数查。
        let sky_rule = if skywalking_ready {
            let a = crate::wecom_intent::analyze(user_text);
            if a.intent == crate::wecom_intent::QaIntent::Troubleshoot {
                let clues = troubleshoot_clue_line(&a);
                format!(
                    "\n排查线上问题时：{clues}用 `skywalking_query` 按这些线索查日志（服务名/关键词/时间窗），看 [ERROR] 行的 traceId 再查调用链定位根因，必要时 `kind=metrics` 看趋势，最后结合代码确认。线索是推测，以实际查询为准。"
                )
            } else {
                "\n若要排查线上问题，用 `skywalking_query` 查日志/调用链（给服务名或关键词；有 traceId 就查调用链），再结合代码定位根因。".to_string()
            }
        } else {
            String::new()
        };
        let system = format!(
            "你是 AI Workbench 的天玑助手，正在通过{where_from}回答。回复简洁、适合即时通讯；少用大段代码围栏。\n\
{reply_rules}\n{mode_rules}{sky_rule}\n{}",
            agent_cfg.system_extra,
            reply_rules = reply_output_rules()
        );

        let req = AgentRunRequest {
            config: agent_cfg.config,
            fallback_config: agent_cfg.fallback_config,
            system,
            user: String::new(),
            request_id: request_id.clone(),
            repo_path: agent_cfg.repo_path,
            services: agent_cfg.services,
            history: Vec::new(),
            messages: {
                let mut msgs: Vec<crate::agent_protocol::Message> = history
                    .iter()
                    .map(|t| crate::agent_protocol::Message::text(&t.role, &t.content))
                    .collect();
                msgs.push(user_msg);
                msgs
            },
            mode: agent_cfg.mode,
            apps: Vec::new(),
            permission,
            tools: agent_cfg.tools,
            vision: agent_cfg.vision,
            diff_confirm: agent_cfg.diff_confirm,
            always_allow: agent_cfg.always_allow,
            web_allow: agent_cfg.web_allow,
            web_tier: agent_cfg.web_tier,
            web_search_provider: agent_cfg.web_search_provider,
            web_search_key: agent_cfg.web_search_key,
            subagents: agent_cfg.subagents,
            subagent_write: false,
            mcp: agent_cfg.mcp,
            parallel_reads: agent_cfg.parallel_reads,
            retries: agent_cfg.retries,
            grep_ignore: agent_cfg.grep_ignore,
            plan_first: agent_cfg.plan_first,
            context_tokens: agent_cfg.context_tokens,
            result_cap: agent_cfg.result_cap,
            anthropic_stream: agent_cfg.anthropic_stream,
            mcp_enabled: agent_cfg.mcp_enabled,
        };

        let mut rx = subscribe_agent_bridge(&request_id);
        struct BridgeGuard(String);
        impl Drop for BridgeGuard {
            fn drop(&mut self) { unsubscribe_agent_bridge(&self.0); }
        }
        let _bridge_guard = BridgeGuard(request_id.clone());
        let sock = Arc::clone(&self.sock);
        let req_id = msg.req_id.clone();
        let stream_id_clone = stream_id.clone();
        let pump_app = app.clone();
        let pump_request_id = request_id.clone();
        let mut channel_changes = self.generation.subscribe();
        let _ = channel_changes.borrow_and_update();
        let pump = tauri::async_runtime::spawn(async move {
            let mut acc = String::new();
            let mut seen_approvals = std::collections::HashSet::new();
            // 看门狗：比审批 5 分钟 TTL 和模型 120s 空闲超时都长；超过它没有任何事件就强制收尾
            let watchdog = std::time::Duration::from_secs(420);
            loop {
                let next = tokio::select! {
                    biased;
                    _ = channel_changes.changed() => {
                        crate::cancellation::cancel_request(&pump_request_id);
                        break String::new();
                    }
                    next = tokio::time::timeout(watchdog, rx.recv()) => next,
                };
                let ev = match next {
                    Ok(ev) => ev,
                    Err(_) => {
                        let body = "**本轮已超时**，未完成的模型正文不展示。".to_string();
                        send_finish(&sock, &req_id, &stream_id_clone, &body).await;
                        crate::cancellation::cancel_request(&pump_request_id);
                        break body;
                    }
                };
                match ev {
                    Some(AgentBridgeEvent::Chunk(t)) => {
                        crate::wecom_progress::activity(&pump_request_id);
                        acc.push_str(&t);
                        // Match DSH: keep incomplete output private until successful completion.
                    }
                    Some(AgentBridgeEvent::ApprovalNeeded {
                        call_ids,
                        summary,
                        can_remember,
                    }) => {
                        if seen_approvals.insert(call_ids.clone())
                            && !crate::wecom_reply::is_finished(&stream_id_clone)
                        {
                            let code = register_approval(
                                &pump_app,
                                &pump_request_id,
                                call_ids,
                                can_remember,
                            )
                            .await;
                            let remember_hint = if can_remember {
                                format!("；回复「{code} 3」同意并本轮同类都允许")
                            } else {
                                String::new()
                            };
                            let tip = format!(
                                "🔐 需要你在企微里确认：{summary}\n回复「{code} 1」同意{remember_hint}；回复「{code} 2」拒绝（5 分钟内有效）。也可切到桌面端「天玑」点卡片。",
                                summary = summary,
                                code = code,
                                remember_hint = remember_hint
                            );
                            let out = tip;
                            let _ = sock
                                .send_raw(wecom_proto::respond_stream(
                                    &req_id,
                                    &stream_id_clone,
                                    &out,
                                    false,
                                ))
                                .await;
                        }
                    }
                    Some(AgentBridgeEvent::Done { .. }) => {
                        crate::wecom_progress::finish(&pump_request_id, "completed", "completed");
                        let body = if acc.is_empty() {
                            "（无文本回复）".to_string()
                        } else {
                            acc
                        };
                        send_finish(&sock, &req_id, &stream_id_clone, &body).await;
                        break body;
                    }
                    Some(AgentBridgeEvent::Err(e)) => {
                        crate::wecom_progress::finish(&pump_request_id, "failed", "failed");
                        let body = format!("**本轮中断**：{e}。未完成的模型正文不展示。");
                        send_finish(&sock, &req_id, &stream_id_clone, &body).await;
                        break body;
                    }
                    None => {
                        // 通道关闭但没 Done：补 finish
                        let body = "**响应意外结束**，未完成的模型正文不展示。".to_string();
                        send_finish(&sock, &req_id, &stream_id_clone, &body).await;
                        break body;
                    }
                }
            }
        });

        let run_result = tokio::select! {
            biased;
            _=async {loop {if session_closed(&session.as_str()){break;}tokio::time::sleep(Duration::from_millis(100)).await;}} => {
                crate::cancellation::cancel_request(&request_id);
                Err("共享任务已结束".to_string())
            },
            result=crate::agent::agent_run(app.clone(), req)=>result,
        };
        unsubscribe_agent_bridge(&request_id);
        let final_text = pump.await.unwrap_or_default();

        // 注销「在跑回合」登记（仅当还是本回合时移除）。
        self.unregister_running(session, &stream_id).await;

        if let Err(e) = run_result {
            // pump 可能已经 finish；仍记历史
            if !final_text.is_empty() {
                append_reply_history(session, user_text, &stream_id, &final_text);
            }
            return Err(e);
        }
        append_reply_history(session, user_text, &stream_id, &final_text);
        Ok(())
    }

    /// 若 `text` 形如「AB12 1/2/3」，则视作对某个待决审批的回复：解析并回灌到 harness。
    /// 返回要给用户的回复文案（None = 不是审批回复，按普通消息继续）。
    async fn try_resolve_approval(
        &self,
        app: &AppHandle,
        text: &str,
        owner: &crate::wecom_channel::ApprovalOwner,
    ) -> Option<String> {
        let t = text.trim();
        // 群聊回复可能带 @机器人 前缀：先剥掉整个 @token 再匹配审批码
        let t = if t.starts_with('@') {
            t.split_once(char::is_whitespace)
                .map(|(_, rest)| rest.trim())
                .unwrap_or(t)
        } else {
            t
        };
        let mut it = t.split_whitespace();
        let code = it.next()?.to_uppercase();
        let choice = it.next()?;
        if it.next().is_some() {
            return None;
        }
        let choice = match choice {
            "1" => 1u8,
            "2" => 2u8,
            "3" => 3u8,
            _ => return None,
        };
        let st = app.state::<WecomState>();
        // 先探一下：DSH 只有一次性授权，没有「3 本轮同类都允许」。
        {
            let map = st.pending_approvals.lock().await;
            let Some(pending) = map.get(&code) else {
                return None;
            };
            if session_closed(&pending.owner.session) {
                return Some("共享任务已结束，该审批已失效。".into());
            }
            if !pending.owner.matches(owner) {
                return Some("该审批仅限原聊天中的动作发起人处理。".into());
            }
            let expired = pending.created.elapsed() > APPROVAL_TTL;
            let dsh = matches!(pending.kind, ApprovalKind::Dsh { .. });
            drop(map);
            if expired {
                st.pending_approvals.lock().await.remove(&code);
                return Some("该审批已超时（5 分钟内有效），请重新发起操作。".into());
            }
            if choice == 3 && dsh {
                return Some(format!(
                    "DeepSeek Harness 只有一次性授权：回复「{code} 1」同意或「{code} 2」拒绝。"
                ));
            }
        }
        let pending = {
            let mut map = st.pending_approvals.lock().await;
            map.remove(&code)?
        };
        match pending.kind {
            ApprovalKind::Tianji {
                request_id,
                call_ids,
            } => {
                let allow = choice != 2;
                // 审批码语义必须和提示文案一一对应（提示里 1=同意一次、3=本轮同类都允许）：
                // - choice 1 → scope None：只放行这一次（对齐桌面端的 once）；
                // - choice 3 → scope "turn"：本轮同类都放行（对齐桌面端的 allow-turn）；
                // 绝不映射成 "always"（那是桌面端「以后都允许」，会静默写进 alwaysAllow）。
                let scope = if choice == 3 {
                    Some("turn".to_string())
                } else {
                    None
                };
                match agent_decide(request_id, call_ids, allow, scope, None) {
                    Ok(()) => {
                        let msg = if choice == 2 {
                            "已拒绝该操作。"
                        } else if choice == 3 {
                            "已同意，本轮同类操作都放行。"
                        } else {
                            "已同意（仅此一次）。"
                        };
                        Some(msg.into())
                    }
                    Err(e) => Some(format!("审批回灌失败：{e}")),
                }
            }
            ApprovalKind::Dsh { summary } => {
                let allow = choice != 2;
                // 等待端收到后回执 ACP；这里只负责交付决定。
                if let Some(tx) = pending.reply {
                    let _ = tx.send(allow);
                }
                Some(if allow {
                    format!("已同意：{summary}")
                } else {
                    format!("已拒绝：{summary}")
                })
            }
        }
    }
}

fn is_approval_text(text: &str) -> bool {
    let mut words = text.split_whitespace();
    matches!((words.next(),words.next(),words.next()),(Some(code),Some("1"|"2"|"3"),None) if code.len()==4 && code.bytes().all(|b|b.is_ascii_alphanumeric()))
}

fn closed_sessions() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static CLOSED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    CLOSED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}
fn session_closed(key: &str) -> bool {
    closed_sessions()
        .lock()
        .map(|set| set.contains(key))
        .unwrap_or(true)
}

fn append_reply_history(session: &SessionKey, user: &str, stream: &str, fallback: &str) {
    if session_closed(&session.as_str()) {
        return;
    }
    if let Some(shown) = crate::wecom_reply::final_text(stream) {
        let request = stream.strip_prefix("stream-").map(|id| format!("wecom-{id}"));
        wecom_session::append_turn_with_request(session, user, &shown, request.as_deref());
    } else if !crate::wecom_reply::is_finished(stream) {
        let request = stream.strip_prefix("stream-").map(|id| format!("wecom-{id}"));
        wecom_session::append_turn_with_request(session, user, fallback, request.as_deref());
    }
}

fn reply_task_labels() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static LABELS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    LABELS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// finish 帧必须送达：断线重连窗口里首发失败的会退避重试，避免企微端永远显示「输入中」。
async fn send_finish(sock: &WecomSocket, req_id: &str, stream_id: &str, body: &str) {
    let task = reply_task_labels()
        .lock()
        .ok()
        .and_then(|mut labels| labels.remove(stream_id));
    // 出站可见正文：剥 WECOM_ATTACH，并把 mermaid 等流程图围栏压成编号步骤（企微不渲染 DSL）。
    let visible = prepare_wecom_visible_markdown(body);
    let labeled = task
        .map(|task| format!("【共享任务 {task}】\n{visible}"))
        .unwrap_or(visible);
    // Freeze the same Markdown that will be displayed and persisted in reply history.
    let Some(final_body) = crate::wecom_reply::finish(stream_id, &labeled) else {
        return;
    };
    let frame = wecom_proto::respond_stream(req_id, stream_id, &final_body, true);
    let request = crate::wecom_diagnostics::transport_request(req_id);
    if let Some(request) = &request {
        crate::wecom_diagnostics::record(request, "delivery", "prepared", None, None, serde_json::json!({"streamId":stream_id,"final":true}));
    }
    let mut wait = 2u64;
    for attempt in 0..7 {
        match sock.send_raw(frame.clone()).await {
            Ok(()) => {
                crate::wecom_log::write(
                    "finish",
                    &format!("stream={stream_id} len={}", body.chars().count()),
                );
                if let Some(request) = &request {
                    crate::wecom_diagnostics::record(request, "delivery", "enqueued", None, None, serde_json::json!({"attempt":attempt + 1,"final":true}));
                }
                return;
            }
            Err(e) => {
                crate::wecom_log::write("finish", &format!("stream={stream_id} send failed: {e}"));
                if let Some(request) = &request {
                    crate::wecom_diagnostics::record(request, "delivery", "retry", Some("send_failed"), None, serde_json::json!({"attempt":attempt + 1}));
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        wait = (wait * 2).min(16);
    }
    if let Some(request) = &request {
        crate::wecom_diagnostics::record(request, "delivery", "failed", Some("retry_exhausted"), None, serde_json::json!({"attempt":7}));
    }
    eprintln!("[wecom] finish 帧多次发送失败 stream_id={stream_id}");
}

/// 企微内审批：把一条待决审批登记为「审批码」，等用户在聊天里用「码 + 选择」回复来放行/拒绝。
/// 自由函数而非方法：泵闭包是 'static 的，不能借用 `self`。
async fn register_approval(
    app: &AppHandle,
    request_id: &str,
    call_ids: Vec<String>,
    can_remember: bool,
) -> String {
    let st = app.state::<WecomState>();
    let seq = st.approval_seq.fetch_add(1, Ordering::SeqCst);
    let code = encode_approval_code(seq);
    let now = Instant::now();
    let owner = st.approval_owners.lock().await.get(request_id).cloned();
    let Some(owner) = owner else {
        return "已结束".into();
    };
    if session_closed(&owner.session) {
        return "已结束".into();
    }
    {
        let mut map = st.pending_approvals.lock().await;
        // 顺手清掉过期项，避免无限增长
        map.retain(|_, p| p.created.elapsed() <= APPROVAL_TTL);
        map.insert(
            code.clone(),
            PendingApproval {
                request_id: request_id.to_string(),
                owner,
                kind: ApprovalKind::Tianji {
                    request_id: request_id.to_string(),
                    call_ids,
                },
                can_remember,
                created: now,
                reply: None,
            },
        );
    }
    code
}

/// DSH 档的待决授权：返回审批码与「用户决定」的接收端。
async fn register_dsh_approval(
    app: &AppHandle,
    summary: String,
    request_id: &str,
) -> (String, tokio::sync::oneshot::Receiver<bool>) {
    let st = app.state::<WecomState>();
    let seq = st.approval_seq.fetch_add(1, Ordering::SeqCst);
    let code = encode_approval_code(seq);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let owner = st.approval_owners.lock().await.get(request_id).cloned();
    let Some(owner) = owner else {
        drop(tx);
        return ("已结束".into(), rx);
    };
    if session_closed(&owner.session) {
        drop(tx);
        return ("已结束".into(), rx);
    }
    {
        let mut map = st.pending_approvals.lock().await;
        map.retain(|_, p| p.created.elapsed() <= APPROVAL_TTL);
        map.insert(
            code.clone(),
            PendingApproval {
                request_id: request_id.to_string(),
                owner,
                kind: ApprovalKind::Dsh { summary },
                can_remember: false,
                created: Instant::now(),
                reply: Some(tx),
            },
        );
    }
    (code, rx)
}

/// 把自增序号编码成 4 位大写字母数字审批码（36^4 循环）。
fn encode_approval_code(seq: u32) -> String {
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut n = seq % 1679616; // 36^4
    let mut out = [b'0'; 4];
    for i in (0..4).rev() {
        out[i] = CHARS[(n % 36) as usize];
        n /= 36;
    }
    String::from_utf8(out.to_vec()).unwrap()
}

/// DSH 启动任务的回报：`oneshot` 两层 Result 摊平。
fn decode_ready(
    res: Result<Result<(), String>, tokio::sync::oneshot::error::RecvError>,
) -> Result<(), String> {
    match res {
        Ok(inner) => inner,
        Err(_) => Err("启动任务意外结束".into()),
    }
}

/// 按问答模式 + 文档目录生成一段系统提示规则。
/// `auto`（默认）：用 `wecom_intent::analyze` 做意图分类 + 槽位抽取，排障时把识别到的
/// 服务/错误特征/时间窗/traceId 直接写进 SOP，让模型第一刀就带着具体参数查。
/// `is_dsh`：DSH 档模型拿不到 `skywalking_query` 工具（日志已由 Rust 预取注入在上文），
/// 排障 SOP 要改成「据上文已附的跨服务日志/调用链定位」，不让它去调拿不到的工具。
/// 固定模式（doc/doc_code/code）保留钉住语义；doc/doc_code 仍在用户要读代码时临时放行。
fn answer_mode_rules(mode: &str, doc_dir: &str, user_text: &str, is_dsh: bool) -> String {
    use crate::wecom_intent::{analyze, QaIntent};
    let wants_code = crate::wecom_intent::mentions_code(user_text);
    match mode {
        "auto" => {
            let a = analyze(user_text);
            match a.intent {
                QaIntent::Troubleshoot => {
                    let clues = troubleshoot_clue_line(&a);
                    let hedge = if a.confidence < 40 { "疑似" } else { "" };
                    if is_dsh {
                        format!(
                            "回答规则（自动判定：{hedge}排查线上问题）。{clues}上文已附**跨服务**的 SkyWalking 日志/调用链（若查到）。步骤：① 先看这些日志里的 [ERROR] 行与调用链中报错的 span，判断是哪个上游/下游服务出问题；② 结合 `{doc_dir}/` 文档与各服务代码定位根因；③ 若上文没有日志或信息不足，直接说明还需要哪个 traceId / 订单号 / 用户ID / 关键词 / 时间窗，让用户补充再查。不要先翻文档而耽误排障。"
                        )
                    } else {
                        format!(
                            "回答规则（自动判定：{hedge}排查线上问题）。{clues}步骤：① 用 `skywalking_query` 查日志（默认**不限定服务、跨全部服务查**，只有明确指向某服务时才传 service；问题里带订单号/用户ID/手机号/requestId 等关键信息时，优先把它们作为 keyword 精确匹配，可逗号分隔传多个联合收窄；带时间窗）；② 日志结果带 [ERROR] 行与 traceId 摘要，挑错误行的 traceId 用 `trace_id` 查调用链（跨服务，看哪个服务的 span 报错）定位根因；③ 必要时 `kind=metrics` 看端点/实例趋势；④ 结合 `{doc_dir}/` 文档与工作区代码确认。线索是推测，以实际查询为准；不要先翻文档而耽误排障。"
                        )
                    }
                }
                QaIntent::ReadCode => format!(
                    "回答规则（自动判定：用户要看代码）：直接阅读工作区相关源码核对后回答（`read_file`/`grep`/`symbols`）。`{doc_dir}/` 下有逆向文档可作参考。"
                ),
                QaIntent::DocLookup => format!(
                    "回答规则（自动判定：实现/原理类问题，文档优先，必要时读代码）：先用 `grep`/`read_file` 查 `{doc_dir}/` 下的逆向文档；文档能答就基于文档回答。文档答不了时再阅读工作区相关源码来回答。"
                ),
            }
        }
        "code" => format!(
            "回答规则：直接阅读工作区代码来回答（`read_file`/`grep`/`symbols`）。`{doc_dir}/` 下有逆向文档可作参考。"
        ),
        "doc" => {
            if wants_code {
                format!(
                    "回答规则（文档优先，但用户这次要求看代码）：先用 `grep`/`read_file` 查 `{doc_dir}/` 下的逆向文档；用户已明确要从代码读，可直接阅读相关源码核对后回答。"
                )
            } else {
                format!(
                    "回答规则（文档优先）：先用 `grep`/`read_file` 在 `{doc_dir}/` 下的逆向文档里找答案；能答就基于文档回答，不要去读业务代码。只有文档确实没有时，才说明「文档未覆盖」并可读代码补充。"
                )
            }
        }
        // doc_code
        _ => format!(
            "回答规则（文档优先，必要时读代码）：先用 `grep`/`read_file` 查 `{doc_dir}/` 下的逆向文档；文档能答就基于文档回答。文档答不了、或用户要求从代码读时，再阅读工作区相关源码来回答。"
        ),
    }
}

/// 是否是「停止当前任务」的指令。仅当整条消息≈纯停止词时才算，
/// 含其它实质内容（如「停车场接口超时」「停止服务一直报错」）的排障问句不拦。
fn is_stop_command(text: &str) -> bool {
    // 去掉首尾标点 / 空白，取纯净文本
    let t = text
        .trim()
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(c, '，' | ',' | '。' | '.' | '、' | '！' | '!' | '~' | '～')
        })
        .trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_lowercase();
    // 整条精确匹配（中文无空格，按整句判断）
    const EXACT: &[&str] = &[
        "停止",
        "停",
        "停下",
        "停一下",
        "停止任务",
        "停止排查",
        "别查了",
        "别跑了",
        "别找了",
        "不用查了",
        "取消",
        "取消任务",
        "结束",
        "中止",
        "stop",
        "stop it",
        "cancel",
        "abort",
        "halt",
    ];
    if EXACT.iter().any(|w| lower == *w) {
        return true;
    }
    // 英文 "stop"/"cancel"/"abort" 开头的纯英文短句（≤3 词、全 ASCII）也算，如 "stop now"。
    // 含中文（如「stop service 为什么挂了」）说明是排障描述，不拦。
    let words: Vec<&str> = lower.split_whitespace().collect();
    let all_ascii = t.is_ascii();
    if all_ascii
        && words.len() <= 3
        && matches!(
            words.first().copied(),
            Some("stop") | Some("cancel") | Some("abort")
        )
    {
        return true;
    }
    false
}

/// 把排障分析里抽到的槽位拼成一行「已识别线索」（为空时返回空串）。
fn troubleshoot_clue_line(a: &crate::wecom_intent::QaAnalysis) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = &a.service {
        parts.push(format!("服务≈{s}"));
    }
    if !a.keywords.is_empty() {
        parts.push(format!("关键信息≈{}", a.keywords.join("、")));
    }
    if let Some(m) = a.minutes {
        parts.push(format!("时间窗≈最近{m}分钟"));
    }
    if let Some(t) = &a.trace_id {
        parts.push(format!("traceId={t}"));
    }
    if let Some(l) = &a.level {
        parts.push(format!("级别={l}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("已识别线索：{}。", parts.join("、"))
    }
}

const DEFAULT_SELF_INTRO: &str =
    "你好，我是 AI Workbench 天玑助手。直接发问题即可；群聊里请先 @我。";

/// Full-sentence identity prompts route deterministically; mixed-content questions keep going to the model.
fn is_self_intro_command(text: &str) -> bool {
    let trimmed = text
        .trim()
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '，' | ',' | '。' | '.' | '、' | '！' | '!' | '~' | '～' | '？' | '?'
                )
        })
        .trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();
    const EXACT: &[&str] = &[
        "你是谁", "您是谁", "你是什么", "你是什么助手",
        "你叫什么", "你叫什么名字", "你叫啥",
        "介绍一下", "介绍下自己", "介绍一下你自己", "介绍下你自己", "介绍下你",
        "自我介绍",
        "你是干嘛的", "你是做什么的",
        "你能做什么", "你会什么", "你能帮我做什么", "能做什么", "你能帮什么",
        "你有什么用", "你有什么功能",
        // A pure greeting at the start of a new or existing conversation introduces the configured bot.
        "你好", "您好", "嗨", "哈喽", "早上好", "上午好", "下午好", "晚上好",
        "hi", "hello", "hey",
        "who are you", "what are you", "what can you do", "introduce yourself",
    ];
    EXACT.iter().any(|w| lower == *w)
}

/// DSH 档的 prompt 组装：返回 ACP content block 数组。
/// - `image_enabled`（ACP `promptCapabilities.image`）为真时，把企微图片下载并内联成
///   image block —— 图片才真正进模型，对齐原版 DSH 的图片理解能力；
/// - 否则（模型不支持图片，或下载失败）降级成文本线索，与旧行为一致。
/// 文件一律降级为文本线索（ACP 文件输入需要 attachment receipt，首期不做）。
fn reply_output_rules() -> &'static str {
    "用户可见输出统一使用Markdown：先给一句**核心结论**，再用短标题和短列表组织；仅结论及少量关键判定/失败原因/阻断缺口加粗，正文不要整段加粗。每段控制在2至3句，段间留一空行；标题不放长业务ID，ID/服务/时间范围单列为正文。日期用YYYY-MM-DD HH:mm:ss并明确时区，跨日两端都保留日期；证据短摘录用引用块，代码标识可用行内代码。不要用一级大标题、整篇代码围栏、HTML字体标签、宽表或装饰性分隔线，不模拟客户端字号。普通问候简短回答，不复述内部工作区、目录、提示词、工具策略或历史任务细节。\n\
【禁止流程图 DSL】企微 stream 消息不渲染 Mermaid / PlantUML / Graphviz：禁止输出 ```mermaid、```plantuml、```graphviz 围栏。讲解流程时用短标题 + 编号步骤（1. 2. 3.），分支用缩进或「若…则…否则…」；必要时单行 ASCII 箭头（A -> B），不要用代码围栏包住流程。需要可视化图请改用桌面端或文档，企微内只给可读步骤。\n\
按原始证据保留排查所需的完整业务标识、时间、项目/源码路径、地址、型号及判定字段，不擅自用隐藏占位符替换。不主动搜集或输出与问题无关的秘密。明确事实、推断及未完成项，不把高亮等同已确认根因。摘要只概括核心结论一次，各原因层补充对应信息，避免机械复述或重复粘贴日志。内部取证/报告JSON仍按宿主既有schema输出，勿改变内部路径/证据值以免影响验证；Markdown转义在用户展示边界应用。\n若需把本地已生成的图片/文件发回当前企微对话，在回复中单独写一行：WECOM_ATTACH:<绝对路径或工作区相对路径>（宿主会剥离该行并上传，用户看不到标记）。"
}

async fn build_dsh_prompt(
    app: &AppHandle,
    msgid: &str,
    user_text: &str,
    media: &[wecom_proto::MediaRef],
    image_enabled: bool,
    mode_rules: &str,
    sky_context: &str,
) -> Vec<serde_json::Value> {
    let mut blocks: Vec<serde_json::Value> = vec![crate::wecom_dsh::acp_text_block(reply_output_rules())];
    // 先放一条「怎么回答」的指引（文档优先 / 读代码），再放用户问题。
    if !mode_rules.trim().is_empty() {
        blocks.push(crate::wecom_dsh::acp_text_block(&format!(
            "【回答指引】{}",
            mode_rules.trim()
        )));
    }
    // 排障预取的 SkyWalking 数据（dsh 档没有 skywalking_query 工具，这里以上下文形式给它）。
    if !sky_context.trim().is_empty() {
        blocks.push(crate::wecom_dsh::acp_text_block(sky_context.trim()));
    }
    let text = user_text.trim();
    if !text.is_empty() {
        blocks.push(crate::wecom_dsh::acp_text_block(text));
    }
    let locals = if media.is_empty() {
        Vec::new()
    } else {
        crate::wecom_media::materialize_inbound(app, msgid, media).await
    };
    for local in &locals {
        if local.kind == "image" && image_enabled {
            match read_file_base64(&local.path).await {
                Ok(data) => {
                    blocks.push(crate::wecom_dsh::acp_image_block(&local.media_type, &data));
                    continue;
                }
                Err(e) => {
                    blocks.push(crate::wecom_dsh::acp_text_block(&format!(
                        "[图片获取失败：{e}]"
                    )));
                    continue;
                }
            }
        }
        // 文本类文件：读入正文；二进制：给绝对路径，便于 DSH 工具定点读取。
        if let Ok(text) = tokio::fs::read_to_string(&local.path).await {
            let preview: String = if text.chars().count() > 8000 {
                format!(
                    "{}\n…(已截断)",
                    text.chars().take(8000).collect::<String>()
                )
            } else {
                text
            };
            blocks.push(crate::wecom_dsh::acp_text_block(&format!(
                "（附件「{}」本地路径：{}）\n```\n{preview}\n```",
                local.filename,
                local.path.display()
            )));
        } else {
            blocks.push(crate::wecom_dsh::acp_text_block(&format!(
                "（{}附件「{}」已落盘：{}；请用工具按路径读取）",
                if local.kind == "image" { "图片" } else { "文件" },
                local.filename,
                local.path.display()
            )));
        }
    }
    // 落盘失败时仍保留 url/media_id 线索，避免用户以为消息丢失。
    if locals.is_empty() {
        for m in media {
            let label = if m.kind == "image" { "图片" } else { "文件" };
            let name = m.filename.clone().unwrap_or_default();
            let src = if !m.url.trim().is_empty() {
                m.url.clone()
            } else {
                m.media_id.clone().unwrap_or_default()
            };
            let note = if name.is_empty() {
                format!("（{label}：{src}）")
            } else {
                format!("（{label}「{name}」：{src}）")
            };
            blocks.push(crate::wecom_dsh::acp_text_block(&note));
        }
    }
    // ACP 不接受空 prompt：既无文字也无可内联媒体时补一个占位，交给上游的空消息兜底。
    if blocks.is_empty() {
        blocks.push(crate::wecom_dsh::acp_text_block(user_text));
    }
    blocks
}

/// 把文字 + 图片/文件附件组装成一条用户消息（天玑档）。
async fn build_user_message(
    app: &AppHandle,
    msgid: &str,
    user_text: &str,
    media: &[wecom_proto::MediaRef],
    vision: bool,
) -> Message {
    let mut blocks: Vec<Block> = Vec::new();
    if !user_text.trim().is_empty() {
        blocks.push(Block::Text {
            text: user_text.to_string(),
        });
    }
    let locals = if media.is_empty() {
        Vec::new()
    } else {
        crate::wecom_media::materialize_inbound(app, msgid, media).await
    };
    for local in &locals {
        if local.kind == "image" && vision {
            match read_file_base64(&local.path).await {
                Ok(data) => blocks.push(Block::Image {
                    media_type: local.media_type.clone(),
                    data,
                }),
                Err(e) => blocks.push(Block::Text {
                    text: format!("[图片获取失败：{e}]"),
                }),
            }
        } else if let Ok(text) = tokio::fs::read_to_string(&local.path).await {
            let preview: String = if text.chars().count() > 8000 {
                format!(
                    "{}\n…(已截断)",
                    text.chars().take(8000).collect::<String>()
                )
            } else {
                text
            };
            blocks.push(Block::Text {
                text: format!(
                    "（附件「{}」：{}）\n{}",
                    local.filename,
                    local.path.display(),
                    preview
                ),
            });
        } else {
            blocks.push(Block::Text {
                text: format!(
                    "（{}附件「{}」已落盘：{}）",
                    if local.kind == "image" { "图片" } else { "文件" },
                    local.filename,
                    local.path.display()
                ),
            });
        }
    }
    if locals.is_empty() {
        for m in media {
            let label = if m.kind == "image" { "图片" } else { "文件" };
            let name = m.filename.clone().unwrap_or_default();
            let src = if !m.url.trim().is_empty() {
                m.url.clone()
            } else {
                m.media_id.clone().unwrap_or_default()
            };
            let note = if name.is_empty() {
                format!("（{label}：{src}）")
            } else {
                format!("（{label}「{name}」：{src}）")
            };
            blocks.push(Block::Text { text: note });
        }
    }
    Message {
        role: "user".into(),
        blocks,
        meta: None,
    }
}

async fn read_file_base64(path: &std::path::Path) -> Result<String, String> {
    use base64::Engine;
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("图片读取失败：{e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// 流式 / 终帧共用：剥附件标记 + 压平流程图 DSL 围栏。
fn prepare_wecom_visible_markdown(text: &str) -> String {
    rewrite_diagram_fences(&strip_wecom_attach_markers(text))
}

/// 流式中间帧：去掉 WECOM_ATTACH 行，避免用户看到宿主指令。
fn strip_wecom_attach_markers(text: &str) -> String {
    split_wecom_attach(text).0
}

/// 拆出可见正文与附件路径（`WECOM_ATTACH:path`，一行一条）。
fn split_wecom_attach(text: &str) -> (String, Vec<String>) {
    let mut visible = Vec::new();
    let mut attachments = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("WECOM_ATTACH:")
            .or_else(|| trimmed.strip_prefix("wecom_attach:"))
        {
            let path = rest.trim();
            if !path.is_empty() {
                attachments.push(path.to_string());
            }
            continue;
        }
        visible.push(line);
    }
    let body = visible.join("\n").trim().to_string();
    (body, attachments)
}

/// 企微不渲染 Mermaid/PlantUML/Graphviz：把对应围栏压成编号步骤，其它语言代码块原样保留。
fn rewrite_diagram_fences(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut rewritten = false;
    while i < bytes.len() {
        if let Some((lang, content, next)) = take_fence_at(text, i) {
            let lower = lang.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "mermaid" | "plantuml" | "graphviz" | "dot" | "puml"
            ) {
                if !rewritten {
                    out.push_str("（企微不支持流程图渲染，已改为步骤列表）\n");
                    rewritten = true;
                } else {
                    out.push('\n');
                }
                out.push_str(&diagram_fence_to_steps(&content));
                out.push('\n');
                i = next;
                continue;
            }
            // 普通代码块：原样拷贝（含围栏行）
            out.push_str(&text[i..next]);
            i = next;
            continue;
        }
        // 拷到下一个可能的围栏起点或文末
        let rest = &text[i..];
        if let Some(rel) = rest.find("```") {
            out.push_str(&text[i..i + rel]);
            i += rel;
        } else {
            out.push_str(rest);
            break;
        }
    }
    out.trim().to_string()
}

/// 从 `at` 起若是完整 ```lang … ``` 围栏，返回 (语言, 内容, 结束后的字节下标)。
fn take_fence_at(text: &str, at: usize) -> Option<(String, String, usize)> {
    let rest = text.get(at..)?;
    if !rest.starts_with("```") {
        return None;
    }
    let after_ticks = &rest[3..];
    let nl = after_ticks.find('\n')?;
    let lang = after_ticks[..nl].trim().to_string();
    let body_start = at + 3 + nl + 1;
    let body = text.get(body_start..)?;
    // 结束围栏：行首 ```
    let mut search = 0usize;
    while let Some(rel) = body[search..].find("```") {
        let abs = search + rel;
        let line_start = body[..abs].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let prefix = body[line_start..abs].trim();
        if prefix.is_empty() {
            let content = body[..abs].trim_end_matches(['\r', '\n']).to_string();
            let mut end = body_start + abs + 3;
            if text.as_bytes().get(end) == Some(&b'\r') {
                end += 1;
            }
            if text.as_bytes().get(end) == Some(&b'\n') {
                end += 1;
            }
            return Some((lang, content, end));
        }
        search = abs + 3;
    }
    None
}

/// 从 flowchart 源码里抽节点标签，压成编号步骤；抽不到则给只读摘要。
fn diagram_fence_to_steps(src: &str) -> String {
    let mut steps: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("flowchart")
            || trimmed.starts_with("graph ")
            || trimmed.starts_with("sequenceDiagram")
            || trimmed.starts_with("classDiagram")
            || trimmed.starts_with("%%")
            || trimmed.starts_with("@startuml")
            || trimmed.starts_with("@enduml")
            || trimmed.starts_with("digraph")
        {
            continue;
        }
        for label in extract_diagram_labels(trimmed) {
            let label = label.trim().to_string();
            if label.is_empty() || !seen.insert(label.clone()) {
                continue;
            }
            steps.push(label);
        }
    }
    if steps.is_empty() {
        let brief: String = src.chars().take(200).collect();
        return format!("1. （原流程图内容摘要）{brief}");
    }
    steps
        .into_iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {s}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_diagram_labels(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // -->|分支文案| / --|分支|
        if let Some((consumed, edge)) = take_edge_label(&line[i..]) {
            out.push(format!("分支：{edge}"));
            i += consumed;
            continue;
        }
        let open = bytes[i] as char;
        let close = match open {
            '[' => ']',
            '{' => '}',
            '(' => ')',
            _ => {
                i += 1;
                continue;
            }
        };
        if let Some(end) = find_matching(line, i, open, close) {
            let label = line[i + 1..end]
                .trim()
                .trim_matches('"')
                .trim()
                .to_string();
            if diagram_label_useful(&label) {
                out.push(label);
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    out
}

fn take_edge_label(s: &str) -> Option<(usize, String)> {
    for prefix in ["-->|", "--|", "-.->|", "==>|"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let end = rest.find('|')?;
            let edge = rest[..end].trim();
            if edge.is_empty() {
                return None;
            }
            return Some((prefix.len() + end + 1, edge.to_string()));
        }
    }
    None
}

fn diagram_label_useful(label: &str) -> bool {
    if label.is_empty() || label.starts_with('|') {
        return false;
    }
    // 丢掉纯节点 id（如 A、B1）；保留中文/空白/标点或较长文案。
    label.chars().count() > 2 || label.chars().any(|c| !c.is_ascii_alphanumeric())
}

fn find_matching(s: &str, open_idx: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;
    for (idx, ch) in s[open_idx..].char_indices() {
        let abs = open_idx + idx;
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(abs);
            }
        }
    }
    None
}

async fn deliver_wecom_attachments(app: &AppHandle, request_id: &str, paths: &[String]) {
    for path in paths {
        let lower = path.to_ascii_lowercase();
        let is_image = [".png", ".jpg", ".jpeg", ".gif", ".webp"]
            .iter()
            .any(|ext| lower.ends_with(ext));
        let result = if is_image {
            crate::wecom_media::send_image(app, path, request_id).await
        } else {
            crate::wecom_media::send_file(app, path, request_id).await
        };
        match result {
            Ok(msg) => crate::wecom_log::write("media-out", &format!("{request_id} {msg}")),
            Err(e) => crate::wecom_log::write(
                "media-out",
                &format!("{request_id} send failed path={path}: {e}"),
            ),
        }
    }
}

fn can_render_verified_answer(proposed: &str, allowed: &str, has_evidence: bool) -> bool {
    has_evidence
        && proposed == allowed
        && matches!(allowed, "result" | "direct_cause" | "trigger_verified")
}

fn merge_investigation_coverage(
    base: &mut crate::wecom_investigation::InvestigationOutcome,
    extra: &crate::wecom_investigation::InvestigationOutcome,
) {
    use crate::wecom_investigation::InvestigationStatus;
    base.collected_ms = base.collected_ms.saturating_add(extra.collected_ms);
    base.request_count = base.budget.used();
    base.cache_hits = base.budget.cache_hits();
    base.evidence_count = base.evidence_count.saturating_add(extra.evidence_count);
    base.shown_count = base.shown_count.saturating_add(extra.shown_count);
    base.warn_count = base.warn_count.saturating_add(extra.warn_count);
    base.truncated_count = base.truncated_count.saturating_add(extra.truncated_count);
    base.incomplete_pages = base.incomplete_pages.saturating_add(extra.incomplete_pages);
    base.may_analyze |= extra.may_analyze;
    for gap in &extra.decision_gaps {
        if !base.decision_gaps.contains(gap) {
            base.decision_gaps.push(gap.clone());
        }
    }
    if base.status == InvestigationStatus::Partial
        || matches!(
            extra.status,
            InvestigationStatus::Partial | InvestigationStatus::SourceError
        )
        || base.incomplete_pages > 0
        || base.truncated_count > 0
    {
        base.status = InvestigationStatus::Partial;
    }
    base.detail = format!(
        "累计采集（含重复查询）={} 展示={} WARN={} 截断={} 未完成页={}；最新补查：{}",
        base.evidence_count,
        base.shown_count,
        base.warn_count,
        base.truncated_count,
        base.incomplete_pages,
        extra.detail
    );
}

fn permitted_conclusion_level<'a>(
    proposed: &'a str,
    gaps: &[String],
    outcome: &crate::wecom_investigation::InvestigationOutcome,
) -> &'a str {
    if proposed == "trigger_verified"
        && (!gaps.is_empty() || outcome.incomplete_pages > 0 || outcome.truncated_count > 0)
    {
        "direct_cause"
    } else if proposed == "trigger_verified" && !outcome.may_analyze {
        "unknown"
    } else {
        proposed
    }
}

fn source_failure_reply(outcome: &crate::wecom_investigation::InvestigationOutcome) -> bool {
    !outcome.context.is_empty()
        && (!outcome.source_ready
            || outcome.status == crate::wecom_investigation::InvestigationStatus::SourceError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn final_reply_freezes_original_markdown_in_history_and_transport() {
        let sock = Arc::new(WecomSocket::new());
        let (mut frames, writer) = sock.capture_test_frames().await;
        let stream = "stream-sensitive-markdown-fixture";
        crate::wecom_reply::register(stream);
        let text = "## 结论\n**未自动报价**\n- 路径 `fixture.parent.pom\\doc/`\n- matchId=1454805448406827008\n> WARN count=0 [L1]";
        send_finish(&sock, "fixture-req", stream, text).await;
        sock.flush().await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(&frames.try_recv().unwrap()).unwrap();
        let shown = frame["body"]["stream"]["content"].as_str().unwrap();
        assert!(shown.starts_with(text));
        assert!(shown.contains("fixture.parent.pom"));
        assert!(shown.contains("1454805448406827008"));
        assert_eq!(shown.matches("⏱ 本次耗时：").count(), 1);
        assert!(shown.contains("**未自动报价**"));
        assert!(shown.contains("WARN count=0 [L1]"));
        assert_eq!(crate::wecom_reply::final_text(stream).as_deref(), Some(shown));
        sock.stop().await;
        writer.await.unwrap();
    }

    #[test]
    fn streamed_protocol_becomes_one_readable_timed_reply() {
        let stream = "integration-protocol-timed";
        crate::wecom_reply::begin(stream);
        let q = crate::wecom_investigation_query::parse_query(
            "查日志",
            None,
            chrono::DateTime::parse_from_rfc3339("2026-10-28T09:00:00+08:00").unwrap(),
        )
        .unwrap();
        let raw=serde_json::json!({"queries":[],"reason":"观察[L1]","conclusion_level":"result","gaps":[],"final_answer":{"summary":"发现业务警告","claims":[{"text":"结果为零","evidence_refs":["L1"]}]}}).to_string();
        let mut answer = crate::wecom_answer::AnswerBuffer::new(true);
        for ch in raw.chars() {
            assert!(answer.push(&ch.to_string()).is_none());
        }
        // Producer-shaped synthetic evidence exercises renderer -> freeze -> protocol.
        let evidence = "[L1] service=orders logger=com.fixture.Decision level=WARN ts=2026-10-29T10:00:01+08:00\n  matchId=1454805448406827008 nlpIdOriginal=4209234431769610323 addTime=1793159847926 6EP1334-2BA20 库存不足 autoQuotationSuccessFlag=false http://10.0.0.1/log token=fixture-secret C:\\work\\rule.rs\n";
        let body = answer.finish(Some(&q), evidence, ".", "result");
        let final_body = crate::wecom_reply::finish(stream, &body).unwrap();
        let frame: serde_json::Value = serde_json::from_str(&wecom_proto::respond_stream("fixture-report", stream, &final_body, true)).unwrap();
        assert_eq!(frame["body"]["stream"]["content"], final_body);
        for value in ["1454805448406827008", "4209234431769610323", "1793159847926", "6EP1334-2BA20", "库存不足", "autoQuotationSuccessFlag=false", "http://10.0.0.1/log", "token=fixture-secret", r"C:\\work\\rule.rs"] {
            assert!(final_body.contains(value), "missing {value}: {final_body}");
        }
        assert!(!final_body.contains("已隐藏"));
        assert_eq!(final_body.matches("> [L1] ").count(), 1);
        assert!(final_body.contains("排障结论"));
        assert!(final_body.contains("一、结果（现象）"));
        assert!(final_body.contains("二、直接原因"));
        assert!(!final_body.contains("final_answer"));
        assert_eq!(final_body.matches("⏱ 本次耗时：").count(), 1);
        assert!(crate::wecom_reply::finish(stream, "迟到原始JSON").is_none());
        assert_eq!(crate::wecom_reply::final_text(stream).unwrap(), final_body);
    }

    #[test]
    fn verified_answer_route_skips_summary_only_at_permitted_level() {
        assert!(can_render_verified_answer(
            "direct_cause",
            "direct_cause",
            true
        ));
        assert!(can_render_verified_answer(
            "trigger_verified",
            "trigger_verified",
            true
        ));
        assert!(!can_render_verified_answer(
            "trigger_verified",
            "direct_cause",
            true
        ));
        assert!(!can_render_verified_answer("unknown", "unknown", true));
        assert!(!can_render_verified_answer("result", "result", false));
    }

    #[test]
    fn unexecuted_or_failed_source_never_enters_business_model() {
        use crate::wecom_investigation::{InvestigationOutcome, InvestigationStatus};
        let mut o = InvestigationOutcome {
            status: InvestigationStatus::NotExecuted,
            context: "未执行：时间解析失败".into(),
            query: None,
            evidence_count: 0,
            request_count: 0,
            cache_hits: 0,
            may_analyze: false,
            source_ready: false,
            clarification: None,
            budget: crate::wecom_skywalking::QueryBudget::new(),
            collected_ms: 0,
            detail: String::new(),
            shown_count: 0,
            warn_count: 0,
            truncated_count: 0,
            incomplete_pages: 0,
            decision_gaps: Vec::new(),
        };
        assert!(source_failure_reply(&o));
        o.status = InvestigationStatus::SourceError;
        o.source_ready = true;
        assert!(source_failure_reply(&o));
        o.status = InvestigationStatus::Empty;
        assert!(!source_failure_reply(&o)); // only bounded source/code query planning, not business summary
        assert!(!o.may_analyze);
        assert_eq!(
            permitted_conclusion_level("trigger_verified", &[], &o),
            "unknown"
        );
        o.may_analyze = true;
        o.incomplete_pages = 1;
        assert_eq!(
            permitted_conclusion_level("trigger_verified", &[], &o),
            "direct_cause"
        );
        let mut cumulative = o.clone();
        cumulative.status = InvestigationStatus::Partial;
        let mut extra = o.clone();
        extra.status = InvestigationStatus::Evidence;
        extra.incomplete_pages = 0;
        extra.evidence_count = 30;
        merge_investigation_coverage(&mut cumulative, &extra);
        assert_eq!(cumulative.status, InvestigationStatus::Partial);
        assert_eq!(cumulative.incomplete_pages, 1);
        o.context.clear();
        o.source_ready = false;
        o.status = InvestigationStatus::NotExecuted;
        assert!(!source_failure_reply(&o)); // unrelated conversation continues as before
    }

    #[test]
    fn answer_mode_doc_blocks_code_unless_asked() {
        let r = answer_mode_rules("doc", "doc", "这个功能怎么实现的", false);
        assert!(r.contains("文档优先"));
        assert!(r.contains("不要去读业务代码"));
        // 用户说读代码 → doc 模式也放行
        let r2 = answer_mode_rules("doc", "doc", "帮我看源码里怎么写的", false);
        assert!(r2.contains("可直接阅读相关源码"));
    }

    #[test]
    fn answer_mode_code_always_reads_source() {
        let r = answer_mode_rules("code", "doc", "随便问", false);
        assert!(r.contains("直接阅读工作区代码"));
    }

    #[test]
    fn answer_mode_auto_routes_by_intent() {
        // 排障 → SkyWalking + 代码，不被文档优先挡住（tianji 档用工具）
        let r = answer_mode_rules("auto", "doc", "帮我排查下线上 500 报错", false);
        assert!(r.contains("skywalking_query"), "{r}");
        assert!(r.contains("排查线上问题"), "{r}");
        assert!(!r.contains("不要去读业务代码"), "{r}");
        // 要看代码 → 直接读源码
        let r2 = answer_mode_rules("auto", "doc", "看下源码里是怎么写的", false);
        assert!(r2.contains("直接阅读工作区相关源码"), "{r2}");
        // 实现/原理 → 文档优先、必要时读代码
        let r3 = answer_mode_rules("auto", "doc", "登录模块怎么实现的", false);
        assert!(r3.contains("文档优先"), "{r3}");
        assert!(r3.contains("文档答不了"), "{r3}");
    }

    #[test]
    fn auto_troubleshoot_dsh_vs_tianji_sop_differs() {
        // DSH 档：日志已注入在上文，不让它调 skywalking_query 工具
        let dsh = answer_mode_rules("auto", "doc", "帮我排查下线上 500 报错", true);
        assert!(dsh.contains("上文已附"), "{dsh}");
        assert!(!dsh.contains("skywalking_query"), "DSH 档不该提工具：{dsh}");
        // tianji 档：用工具查
        let tj = answer_mode_rules("auto", "doc", "帮我排查下线上 500 报错", false);
        assert!(tj.contains("skywalking_query"), "{tj}");
        assert!(tj.contains("跨全部服务"), "{tj}");
    }

    #[test]
    fn auto_troubleshoot_embeds_clues() {
        // 抽到的服务/错误特征应出现在排障 SOP 的「已识别线索」里
        let r = answer_mode_rules(
            "auto",
            "doc",
            "排查 OrderService 的 NullPointerException",
            false,
        );
        assert!(r.contains("已识别线索"), "{r}");
        assert!(r.contains("OrderService"), "{r}");
        assert!(r.contains("NullPointerException"), "{r}");
    }

    #[test]
    fn stop_command_detection() {
        // 纯停止指令判真
        for q in [
            "停止",
            "停",
            "别查了",
            "取消",
            "停止任务",
            "stop",
            "cancel",
            "abort now",
            "停。",
            " 停止 ",
        ] {
            assert!(is_stop_command(q), "应判停止：{q}");
        }
        // 含实质排障内容的不拦
        for q in [
            "停止服务一直报错",
            "停车场接口超时排查下",
            "订单停滞了怎么回事",
            "介绍下停机流程",
            "stop service 为什么挂了",
        ] {
            assert!(!is_stop_command(q), "不该判停止：{q}");
        }
        assert!(!is_stop_command(""));
    }

    #[test]
    fn approvals_are_bound_to_bot_user_chat_and_session() {
        let a = crate::wecom_channel::ApprovalOwner {
            bot: "b".into(),
            chat: "g".into(),
            user: "u".into(),
            session: "t".into(),
        };
        assert!(a.matches(&a));
        for b in [
            crate::wecom_channel::ApprovalOwner {
                user: "v".into(),
                ..a.clone()
            },
            crate::wecom_channel::ApprovalOwner {
                chat: "g2".into(),
                ..a.clone()
            },
            crate::wecom_channel::ApprovalOwner {
                bot: "b2".into(),
                ..a.clone()
            },
            crate::wecom_channel::ApprovalOwner {
                session: "t2".into(),
                ..a.clone()
            },
        ] {
            assert!(!a.matches(&b));
        }
        assert!(is_approval_text("AB12 1"));
        assert!(!is_approval_text("AB12 1 然后继续"));
    }

    #[tokio::test]
    async fn retirement_after_planning_closes_original_stream_before_socket_stop() {
        let sock = Arc::new(WecomSocket::new());
        let (mut frames, writer) = sock.capture_test_frames().await;
        let orch = Orchestrator::new(sock.clone());
        let request = "wecom-retire-real-pattern";
        let stream = "stream-retire-real-pattern";
        crate::wecom_reply::register(stream);
        crate::wecom_progress::begin("retire-pattern-session", request);
        crate::wecom_progress::phase(request, "planning");
        orch.reply_targets.lock().await.insert(request.into(), ("old-bot-request".into(), stream.into()));
        let previous = orch.current_generation();
        orch.retire("channel_changed").await;
        assert_ne!(previous, orch.current_generation());
        assert!(orch.retiring.load(Ordering::SeqCst));
        let raw = frames.try_recv().expect("old request must have an actual finish frame");
        let frame: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(frame["headers"]["req_id"], "old-bot-request");
        assert_eq!(frame["body"]["stream"]["id"], stream);
        assert_eq!(frame["body"]["stream"]["finish"], true);
        assert!(frame["body"]["stream"]["content"].as_str().unwrap().contains("已中断"));
        assert!(crate::wecom_reply::finish(stream, "late model").is_none());
        assert!(crate::wecom_progress::status_reply("retire-pattern-session").contains("通道已变更"));
        sock.stop().await;
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn retirement_preserves_already_frozen_completion_and_closes_queued_reply() {
        let sock = Arc::new(WecomSocket::new());
        let (mut frames, writer) = sock.capture_test_frames().await;
        let orch = Orchestrator::new(sock.clone());
        crate::wecom_reply::register("stream-retire-done");
        crate::wecom_reply::finish("stream-retire-done", "completed first");
        crate::wecom_reply::register("stream-retire-queued");
        orch.reply_targets.lock().await.insert("wecom-retire-done".into(), ("old-1".into(), "stream-retire-done".into()));
        orch.reply_targets.lock().await.insert("wecom-retire-queued".into(), ("old-2".into(), "stream-retire-queued".into()));
        orch.retire("channel_restart").await;
        let raw = frames.try_recv().unwrap();
        let frame: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(frame["headers"]["req_id"], "old-2");
        assert!(frames.try_recv().is_err());
        assert!(crate::wecom_reply::final_text("stream-retire-done").unwrap().contains("completed first"));
        sock.stop().await;
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn configured_intro_replies_to_new_conversation_greeting_without_ai() {
        let sock = Arc::new(WecomSocket::new());
        let (mut frames, writer) = sock.capture_test_frames().await;
        let orch = Orchestrator::new(sock.clone());
        let configured = "你好！我是腾采通智能助手-超级玛丽 👋\n\n· 业务/功能\n· 技术/代码\n· 线上日志查询";
        *orch.self_intro.lock().await = configured.into();
        for (i, input) in ["新对话，你好", "新会话，你是谁？", "你好", "你是谁"].iter().enumerate() {
            let text = crate::wecom_investigation_query::new_session_remainder(input).unwrap_or_else(|| input.to_string());
            let stream = format!("stream-intro-fixture-{i}");
            crate::wecom_reply::register(&stream);
            assert!(orch.reply_self_intro("intro-old-req", &stream, &text).await);
            sock.flush().await.unwrap();
            let frame: serde_json::Value = serde_json::from_str(&frames.try_recv().unwrap()).unwrap();
            assert_eq!(frame["body"]["stream"]["finish"], true);
            let body = frame["body"]["stream"]["content"].as_str().unwrap();
            assert!(body.starts_with(configured));
            assert!(body[configured.len()..].starts_with("\n\n⏱"));
            assert_eq!(orch.admission.active(), 0);
            assert!(orch.running.lock().await.is_empty());
        }
        for input in ["你好，帮我查日志", "新对话，你好，帮我查订单", "介绍一下采购流程"] {
            let text = crate::wecom_investigation_query::new_session_remainder(input).unwrap_or_else(|| input.to_string());
            assert!(!orch.reply_self_intro("unused", "stream-intro-business", &text).await);
        }
        assert!(frames.try_recv().is_err());
        sock.stop().await;
        writer.await.unwrap();
    }

    #[test]
    fn self_intro_short_phrases_route_without_model() {
        for phrase in [
            "你是谁", "您是谁", "你是什么助手", "你叫什么", "你叫啥",
            "你好", "您好！", "嗨", "哈喽", "hi", "HELLO?", "早上好",
            "介绍一下", "介绍下自己", "介绍一下你自己", "自我介绍",
            "你是干嘛的", "你能做什么", "你会什么", "能做什么",
            "你有什么用", "你有什么功能",
            "who are you", "WHAT CAN YOU DO", "Introduce yourself",
            // 包含常见标点/空白包围的完整短句仍然命中
            "你是谁？", " 你是谁！", "介绍一下！！", "你能做什么？？？",
        ] {
            assert!(is_self_intro_command(phrase), "应判为自我介绍：{phrase}");
        }
        for phrase in [
            "",
            " ",
            "你是谁开发的能给我讲讲吗",
            "你好，帮我查日志",
            "hello 查一下接口",
            "您好 请问采购流程",
            "新对话，你好",
            "介绍一下腾采通的采购流程",
            "你能做什么样的接口压测",
            "stop",
            "进度",
            "任务状态",
            "whoareyou",
            "介绍一下这个订单",
            "/你是谁",
            "@bot 你是谁 顺便查一下日志",
        ] {
            assert!(!is_self_intro_command(phrase), "不该判为自我介绍：{phrase}");
        }
    }

    #[test]
    fn set_max_jobs_clamps_out_of_range() {
        let orch = Orchestrator::new(Arc::new(WecomSocket::new()));
        orch.set_max_jobs(4);
        assert_eq!(orch.admission.limit(), 4);
        // 0 / 越界回落默认
        orch.set_max_jobs(0);
        assert_eq!(
            orch.admission.limit(),
            crate::wecom_admission::DEFAULT_LIMIT
        );
        orch.set_max_jobs(99);
        assert_eq!(
            orch.admission.limit(),
            crate::wecom_admission::DEFAULT_LIMIT
        );
        orch.set_max_jobs(32);
        assert_eq!(orch.admission.limit(), 32);
        orch.set_max_jobs(33);
        assert_eq!(orch.admission.limit(), 2);
    }

    #[test]
    fn wecom_attach_markers_are_stripped_from_visible_markdown() {
        let raw = "## 结果\n已生成报告\nWECOM_ATTACH:D:/tmp/a.png\nwecom_attach: ./notes.txt\n收尾";
        let (visible, paths) = split_wecom_attach(raw);
        assert_eq!(visible, "## 结果\n已生成报告\n收尾");
        assert_eq!(paths, vec!["D:/tmp/a.png".to_string(), "./notes.txt".to_string()]);
        assert_eq!(
            strip_wecom_attach_markers("hello\nWECOM_ATTACH:x.png\nworld"),
            "hello\nworld"
        );
        assert!(split_wecom_attach("no markers").1.is_empty());
    }

    #[test]
    fn reply_output_rules_forbid_mermaid_and_require_numbered_steps() {
        let rules = reply_output_rules();
        assert!(rules.contains("禁止") && rules.to_ascii_lowercase().contains("mermaid"), "{rules}");
        assert!(rules.contains("编号步骤"), "{rules}");
        assert!(
            rules.to_ascii_lowercase().contains("plantuml")
                || rules.to_ascii_lowercase().contains("graphviz"),
            "{rules}"
        );
    }

    #[test]
    fn rewrite_diagram_fences_turns_mermaid_into_steps_keeps_other_code() {
        let raw = r#"## 二、主流程图

```mermaid
flowchart TD
  A[客户在渠道发消息 / 企微]
  B[渠道接收]
  C{拦截闸门}
  A --> B --> C
  C -->|命中黑名单| X[消息丢弃]
  C -->|通过| D[AI 识别拆词]
```

```rust
fn ok() {}
```
"#;
        let out = rewrite_diagram_fences(raw);
        assert!(out.contains("企微不支持流程图渲染"), "{out}");
        assert!(out.contains("1. 客户在渠道发消息"), "{out}");
        assert!(out.contains("拦截闸门"), "{out}");
        assert!(out.contains("分支：命中黑名单") || out.contains("消息丢弃"), "{out}");
        assert!(!out.contains("```mermaid"), "{out}");
        assert!(out.contains("```rust"), "普通代码块应保留：{out}");
        assert!(out.contains("fn ok()"), "{out}");
    }

    #[test]
    fn prepare_visible_markdown_strips_attach_and_mermaid() {
        let raw = "说明\nWECOM_ATTACH:D:/a.png\n```mermaid\nflowchart TD\n  A[步骤甲]\n```\n";
        let out = prepare_wecom_visible_markdown(raw);
        assert!(!out.contains("WECOM_ATTACH"), "{out}");
        assert!(!out.contains("```mermaid"), "{out}");
        assert!(out.contains("步骤甲"), "{out}");
    }
}
