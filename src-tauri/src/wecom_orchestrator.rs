//! 企微入站 → 策略 → agent_run → 流式回写。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
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
use crate::wecom_dsh::{permission_options, permission_summary, DshEvent};
use crate::wecom_policy::{GateDecision, MsgDeduper, PolicyState};
use crate::wecom_proto::{self, EventCallback, MsgCallback};
use crate::wecom_sanitize::redact_local_paths;
use crate::wecom_session::{self, SessionKey};
use crate::wecom_ws::WecomSocket;

const MAX_GLOBAL_JOBS: usize = 2;
/// DSH 单回合上限：比天玑的 420s 看门狗宽，因为 ACP 侧可能连着跑很久的工具。
const DSH_TURN_TIMEOUT: Duration = Duration::from_secs(600);
/// 回合里超过这么久没有任何事件，就给聊天里补一句「还在处理」，别让人对着「思考中」干等。
const DSH_STALL_NOTE: Duration = Duration::from_secs(90);
/// 等同一会话上一轮结束时，超过这么久提示用户「在排队」。
const SESSION_QUEUE_NOTE: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, serde::Deserialize)]
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
    /// 回话引擎：`tianji` | `dsh`（由设置页下发）
    pub engine: Arc<Mutex<String>>,
    deduper: Arc<Mutex<MsgDeduper>>,
    /// 同会话串行
    session_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    global_jobs: Arc<AtomicUsize>,
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
            engine: Arc::new(Mutex::new("tianji".to_string())),
            deduper: Arc::new(Mutex::new(MsgDeduper::new(2048))),
            session_locks: Arc::new(Mutex::new(HashMap::new())),
            global_jobs: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn handle_msg(self: &Arc<Self>, app: AppHandle, msg: MsgCallback) {
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            this.on_msg(app, msg).await;
        });
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

    async fn on_msg(&self, app: AppHandle, msg: MsgCallback) {
        {
            let mut d = self.deduper.lock().await;
            if d.seen_or_insert(&msg.msgid) {
                return;
            }
        }

        // 企微内审批回灌：若本条是「审批码 + 选择」回复，直接放行/拒绝对应回合
        if let Some(answer) = self.try_resolve_approval(&app, &msg.text).await {
            let _ = self
                .sock
                .send_raw(wecom_proto::respond_stream(
                    &msg.req_id,
                    &format!("appr-{}", msg.msgid),
                    &answer,
                    true,
                ))
                .await;
            return;
        }

        let bot_name = self.bot_display_name.lock().await.clone();
        let (text, mentioned) = wecom_proto::strip_mention(&msg.text, &bot_name);
        let mut user_text = text;
        if let Some(q) = &msg.quote_text {
            if !q.trim().is_empty() {
                user_text = format!("（引用）{}\n\n{}", q.trim(), user_text);
            }
        }
        if msg.voice_empty {
            let tip = "没能识别语音内容，请直接发文字或图片。";
            let _ = self
                .sock
                .send_raw(wecom_proto::respond_stream(
                    &msg.req_id,
                    &format!("voice-{}", msg.msgid),
                    tip,
                    true,
                ))
                .await;
            return;
        }
        if user_text.trim().is_empty() && msg.media.is_empty() {
            // 既无文字也无媒体（表情/位置等）：礼貌提示，不再静默丢
            let tip = "暂不支持该消息类型，请发文字、图片或文件。";
            let _ = self
                .sock
                .send_raw(wecom_proto::respond_stream(
                    &msg.req_id,
                    &format!("s-{}", msg.msgid),
                    tip,
                    true,
                ))
                .await;
            return;
        }

        let is_group = msg.chattype == "group";
        let session = if is_group {
            let chatid = msg.chatid.clone().unwrap_or_default();
            // Platform already filters group callbacks to @-bot deliveries; treat as mentioned
            // so strip_mention false-negatives (stripped/@ not at start) do not silent-drop.
            let _ = mentioned;
            let decision = {
                let policy = self.policy.lock().await;
                policy.gate_group(&chatid, true)
            };
            if decision != GateDecision::Allow {
                return;
            }
            SessionKey::group(&chatid)
        } else {
            let decision = {
                let mut policy = self.policy.lock().await;
                policy.gate_dm(&msg.userid)
            };
            match decision {
                GateDecision::Allow => SessionKey::dm(&msg.userid),
                GateDecision::NeedPairing(code) => {
                    let tip = format!(
                        "首次私聊需要配对。请在 AI Workbench → 设置 → 联网扩展 → 企业微信机器人 中批准配对码：{code}"
                    );
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(
                            &msg.req_id,
                            &format!("pair-{}", msg.msgid),
                            &tip,
                            true,
                        ))
                        .await;
                    return;
                }
                GateDecision::DenySilent | GateDecision::DenyWith(_) => return,
            }
        };

        // 全局并发
        if self.global_jobs.load(Ordering::SeqCst) >= MAX_GLOBAL_JOBS {
            let tip = "当前还有任务在跑，请稍后再试。";
            let _ = self
                .sock
                .send_raw(wecom_proto::respond_stream(
                    &msg.req_id,
                    &format!("busy-{}", msg.msgid),
                    tip,
                    true,
                ))
                .await;
            return;
        }

        let lock = {
            let key = session.as_str();
            let mut map = self.session_locks.lock().await;
            map.entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        let stream_id = format!("stream-{}", msg.msgid);
        // 平台对「收到消息后的首次应答」有时间窗，而引擎冷启动 / 模型首 token 都可能更久；
        // 先无条件占位一条（同一个 stream id），后面的正文继续更新它，避免整轮被平台丢掉。
        // 注意：占位要在拿会话锁**之前**——同一会话上一轮如果卡住，这一条至少不会无声无息。
        self.ack(&msg, &stream_id).await;
        let engine_name = self.engine.lock().await.clone();
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

        // 引擎分流：DSH 档不读天玑的模型快照，直接交给常驻 ACP 进程。
        if engine_name == "dsh" {
            self.global_jobs.fetch_add(1, Ordering::SeqCst);
            let result = self
                .run_dsh_turn(&app, &msg, &session, &user_text, &stream_id)
                .await;
            self.global_jobs.fetch_sub(1, Ordering::SeqCst);
            if let Err(e) = result {
                eprintln!("[wecom] dsh turn failed: {e}");
                let repo = self.dsh_workspace(&app).await;
                let tip = redact_local_paths(
                    &format!("出错了：{}", crate::wecom_dsh::friendly_turn_error(&e)),
                    &repo,
                );
                let _ = self
                    .sock
                    .send_raw(wecom_proto::respond_stream(
                        &msg.req_id,
                        &stream_id,
                        &tip,
                        true,
                    ))
                    .await;
            }
            return;
        }

        let agent_cfg = match self.agent.lock().await.clone() {
            Some(c) => c,
            None => {
                let tip = "企微通道未绑定模型：请在设置里保存一次配置。";
                let _ = self
                    .sock
                    .send_raw(wecom_proto::respond_stream(
                        &msg.req_id,
                        &stream_id,
                        tip,
                        true,
                    ))
                    .await;
                return;
            }
        };

        // 把文字 + 图片/文件附件组装成一条带 vision 块的用户消息
        let user_msg =
            build_user_message(&user_text, &msg.media, agent_cfg.vision).await;

        // 记录当前回合的出站目标，供 wecom_send_image/file 把媒体发回同一条对话
        {
            let st = app.state::<WecomState>();
            *st.last_target.lock().await = Some(crate::wecom_channel::OutTarget {
                req_id: msg.req_id.clone(),
                stream_id: format!("s-{}", msg.msgid),
            });
        }

        self.global_jobs.fetch_add(1, Ordering::SeqCst);
        let result = self
            .run_agent_and_stream(&app, &msg, &session, &user_text, user_msg, agent_cfg)
            .await;
        self.global_jobs.fetch_sub(1, Ordering::SeqCst);

        // 回合结束，清掉出站目标
        {
            let st = app.state::<WecomState>();
            *st.last_target.lock().await = None;
        }

        if let Err(e) = result {
            eprintln!("[wecom] agent turn failed: {e}");
            let tip = {
                let repo = self
                    .agent
                    .lock()
                    .await
                    .as_ref()
                    .map(|c| c.repo_path.clone())
                    .unwrap_or_default();
                redact_local_paths(&format!("出错了：{e}"), &repo)
            };
            let _ = self
                .sock
                .send_raw(wecom_proto::respond_stream(
                    &msg.req_id,
                    &stream_id,
                    &tip,
                    true,
                ))
                .await;
        }
    }

    /// DSH（DeepSeek Harness）档的一轮：常驻 ACP 进程跑完 prompt，提交点节流回写。
    ///
    /// 与天玑档的差别：
    /// - 上下文由 DSH 的 session 自己维护，不把 `wecom_session` 历史塞进 prompt；
    /// - 授权走 `session/request_permission` → 企微审批码（只有一次性授权）；
    /// - 图片/文件首期降级为文本线索（ACP 图片要求 attachment store + 图像 route）。
    async fn run_dsh_turn(
        &self,
        app: &AppHandle,
        msg: &MsgCallback,
        session: &SessionKey,
        user_text: &str,
        stream_id: &str,
    ) -> Result<(), String> {
        let state = app.state::<WecomState>();
        let engine = Arc::clone(&state.dsh);
        let req_id = msg.req_id.clone();

        // 冷启动 / 首次初始化 profile 可能要几十秒：启动放在独立任务里跑（它要拿引擎的
        // start 锁），这里只等结果；超过 3 秒就在**同一个** stream id 上更新一句进度，
        // 不另外开消息（平台对首帧的时间窗已经用 on_msg 的占位帧满足了）。
        let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
        let dsh = Arc::clone(&engine);
        tauri::async_runtime::spawn(async move {
            let _ = ready_tx.send(dsh.ensure_ready().await);
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

        let repo = engine.workspace().await;
        let redact = |s: &str| redact_local_paths(s, &repo);

        let key = session.as_str();
        let acp_session = engine.acquire_session(&key).await?;
        crate::wecom_log::write(
            "dsh",
            &format!("msgid={} key={key} session={acp_session}", msg.msgid),
        );
        // 设置页选的模型 / 思考强度按轮下发；失败只记日志，别让整轮崩（会话会退回 DSH 默认路由）。
        // 列模型要走一次网络，卡住的话不能把整轮拖死，所以给个上限。
        match tokio::time::timeout(
            Duration::from_secs(20),
            engine.apply_selection(&acp_session),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => crate::wecom_log::write("dsh", &format!("msgid={} {e}", msg.msgid)),
            Err(_) => crate::wecom_log::write(
                "dsh",
                &format!("msgid={} apply_selection timeout", msg.msgid),
            ),
        }
        let prompt_blocks = build_dsh_prompt(user_text, &msg.media, engine.image_prompt_enabled().await).await;
        let mut turn = engine.prompt(&acp_session, prompt_blocks).await?;

        let mut acc = String::new();
        let mut last_push = InstantApprox::now();
        let mut last_event = std::time::Instant::now();
        // 最近一次动作（工具名），用来解释「为什么还在等」。
        let mut last_activity = String::new();
        let mut stall_notes = 0u32;
        let mut open_approvals: Vec<u64> = Vec::new();
        let deadline = tokio::time::Instant::now() + DSH_TURN_TIMEOUT;
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let final_text = loop {
            let event = tokio::select! {
                ev = turn.next() => ev,
                // 长时间没有任何事件：往同一个气泡里补一句「还在处理」，别让人对着「思考中」干等。
                // 提示按 90s / 180s / 270s… 递增间隔，最多 5 次，不刷屏。
                _ = tick.tick() => {
                    let due = DSH_STALL_NOTE + Duration::from_secs(90 * stall_notes as u64);
                    if last_event.elapsed() >= due && stall_notes < 5 {
                        stall_notes += 1;
                        let what = if last_activity.is_empty() {
                            "模型还在思考".to_string()
                        } else {
                            format!("最近一步：{last_activity}")
                        };
                        let note = redact(&if acc.trim().is_empty() {
                            format!("（已 {} 秒没有新输出，{what}）", last_event.elapsed().as_secs())
                        } else {
                            format!("{acc}\n\n（已 {} 秒没有新输出，{what}）", last_event.elapsed().as_secs())
                        });
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
                    let body = if acc.trim().is_empty() {
                        "（响应超时，已结束）".to_string()
                    } else {
                        redact(&format!("{}\n\n（长时间无响应，已自动收尾）", acc))
                    };
                    send_finish(&self.sock, &req_id, stream_id, &body).await;
                    break body;
                }
            };
            match event {
                Some(DshEvent::Update(update)) => {
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
                            acc.push_str(text);
                        }
                        if last_push.elapsed_ms() >= 400 || acc.chars().count() < 40 {
                            let out = redact(&acc);
                            if let Err(e) = self
                                .sock
                                .send_raw(wecom_proto::respond_stream(
                                    &req_id,
                                    stream_id,
                                    &out,
                                    false,
                                ))
                                .await
                            {
                                crate::wecom_log::write(
                                    "stream",
                                    &format!("msgid={} send failed: {e}", msg.msgid),
                                );
                            }
                            last_push = InstantApprox::now();
                        }
                    } else if kind == "tool_call" || kind == "tool_call_update" {
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
                    let (code, reply) = register_dsh_approval(app, summary.clone()).await;
                    let tip = format!(
                        "🔐 DeepSeek Harness 需要你确认：{summary}\n回复「{code} 1」同意；回复「{code} 2」拒绝（5 分钟内有效）。",
                        summary = summary,
                        code = code
                    );
                    // 审批提示单独发一条**已结束**的消息：正文流还在跑，而平台对未结束的
                    // 流式更新不一定立刻显示；这条必须让用户看见，否则只能干等 5 分钟超时。
                    crate::wecom_log::write(
                        "perm",
                        &format!("msgid={} {summary}", msg.msgid),
                    );
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(
                            &req_id,
                            &format!("appr-{}", msg.msgid),
                            &redact(&tip),
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
                            if acc.trim().is_empty() {
                                if stop == "cancelled" {
                                    "（本轮已取消）".to_string()
                                } else {
                                    "（无文本回复）".to_string()
                                }
                            } else {
                                format!("{}{}", redact(&acc), note)
                            }
                        }
                        Err(e) => {
                            let e = crate::wecom_dsh::friendly_turn_error(&e);
                            if acc.trim().is_empty() {
                                redact(&format!("出错了：{e}"))
                            } else {
                                redact(&format!("{acc}\n\n（中断：{e}）"))
                            }
                        }
                    };
                    send_finish(&self.sock, &req_id, stream_id, &body).await;
                    break body;
                }
                None => {
                    let body = if acc.trim().is_empty() {
                        "（已结束）".to_string()
                    } else {
                        redact(&acc)
                    };
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
        wecom_session::append_turn(session, user_text, &final_text);
        Ok(())
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
            Err(e) => crate::wecom_log::write(
                "ack",
                &format!("msgid={} send failed: {e}", msg.msgid),
            ),
        }
    }

    async fn dsh_workspace(&self, app: &AppHandle) -> String {
        app.state::<WecomState>().dsh.workspace().await
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
        let stream_id = format!("stream-{}", msg.msgid);
        let history = wecom_session::session_history(session);

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
        let system = format!(
            "你是 AI Workbench 的天玑助手，正在通过{where_from}回答。回复简洁、适合即时通讯；少用大段代码围栏。\n\
回复勿输出本机绝对路径，文件用相对路径或文件名。\n{}",
            agent_cfg.system_extra
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
        let sock = Arc::clone(&self.sock);
        let req_id = msg.req_id.clone();
        let stream_id_clone = stream_id.clone();
        let repo_for_redact = req.repo_path.clone();
        let pump_app = app.clone();
        let pump_request_id = request_id.clone();
        let pump = tauri::async_runtime::spawn(async move {
            let redact = |s: &str| redact_local_paths(s, &repo_for_redact);
            let mut acc = String::new();
            let mut last_push = InstantApprox::now();
            let mut approval_told = false;
            // 看门狗：比审批 5 分钟 TTL 和模型 120s 空闲超时都长；超过它没有任何事件就强制收尾
            let watchdog = std::time::Duration::from_secs(420);
            loop {
                let ev = match tokio::time::timeout(watchdog, rx.recv()).await {
                    Ok(ev) => ev,
                    Err(_) => {
                        let body = if acc.is_empty() {
                            "（响应超时，已结束）".to_string()
                        } else {
                            format!("{}\n\n（长时间无响应，已自动收尾）", redact(&acc))
                        };
                        send_finish(&sock, &req_id, &stream_id_clone, &body).await;
                        crate::cancellation::cancel_request(&pump_request_id);
                        break body;
                    }
                };
                match ev {
                    Some(AgentBridgeEvent::Chunk(t)) => {
                        acc.push_str(&t);
                        // 节流推流，避免每 token 打一帧
                        if last_push.elapsed_ms() >= 400 || acc.chars().count() < 40 {
                            let out = redact(&acc);
                            let _ = sock
                                .send_raw(wecom_proto::respond_stream(
                                    &req_id,
                                    &stream_id_clone,
                                    &out,
                                    false,
                                ))
                                .await;
                            last_push = InstantApprox::now();
                        }
                    }
                    Some(AgentBridgeEvent::ApprovalNeeded { call_ids, summary, can_remember }) => {
                        if !approval_told {
                            approval_told = true;
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
                            let out = redact(&tip);
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
                        let body = if acc.is_empty() {
                            "（无文本回复）".to_string()
                        } else {
                            redact(&acc)
                        };
                        send_finish(&sock, &req_id, &stream_id_clone, &body).await;
                        break body;
                    }
                    Some(AgentBridgeEvent::Err(e)) => {
                        let body = if acc.is_empty() {
                            format!("出错了：{e}")
                        } else {
                            format!("{acc}\n\n（中断：{e}）")
                        };
                        let body = redact(&body);
                        send_finish(&sock, &req_id, &stream_id_clone, &body).await;
                        break body;
                    }
                    None => {
                        // 通道关闭但没 Done：补 finish
                        let body = if acc.is_empty() {
                            "（已结束）".to_string()
                        } else {
                            redact(&acc)
                        };
                        send_finish(&sock, &req_id, &stream_id_clone, &body).await;
                        break body;
                    }
                }
            }
        });

        let run_result = crate::agent::agent_run(app.clone(), req).await;
        unsubscribe_agent_bridge(&request_id);
        let final_text = pump.await.unwrap_or_default();

        if let Err(e) = run_result {
            // pump 可能已经 finish；仍记历史
            if !final_text.is_empty() {
                wecom_session::append_turn(session, user_text, &final_text);
            }
            return Err(e);
        }
        wecom_session::append_turn(session, user_text, &final_text);
        Ok(())
    }

    /// 若 `text` 形如「AB12 1/2/3」，则视作对某个待决审批的回复：解析并回灌到 harness。
    /// 返回要给用户的回复文案（None = 不是审批回复，按普通消息继续）。
    async fn try_resolve_approval(&self, app: &AppHandle, text: &str) -> Option<String> {
        let t = text.trim();
        // 群聊回复可能带 @机器人 前缀：先剥掉整个 @token 再匹配审批码
        let t = if t.starts_with('@') {
            t.split_once(char::is_whitespace).map(|(_, rest)| rest.trim()).unwrap_or(t)
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

/// finish 帧必须送达：断线重连窗口里首发失败的会退避重试，避免企微端永远显示「输入中」。
async fn send_finish(sock: &WecomSocket, req_id: &str, stream_id: &str, body: &str) {
    let frame = wecom_proto::respond_stream(req_id, stream_id, body, true);
    let mut wait = 2u64;
    for _ in 0..7 {
        match sock.send_raw(frame.clone()).await {
            Ok(()) => {
                crate::wecom_log::write(
                    "finish",
                    &format!("stream={stream_id} len={}", body.chars().count()),
                );
                return;
            }
            Err(e) => {
                crate::wecom_log::write("finish", &format!("stream={stream_id} send failed: {e}"));
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        wait = (wait * 2).min(16);
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
    {
        let mut map = st.pending_approvals.lock().await;
        // 顺手清掉过期项，避免无限增长
        map.retain(|_, p| p.created.elapsed() <= APPROVAL_TTL);
        map.insert(
            code.clone(),
            PendingApproval {
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
) -> (String, tokio::sync::oneshot::Receiver<bool>) {
    let st = app.state::<WecomState>();
    let seq = st.approval_seq.fetch_add(1, Ordering::SeqCst);
    let code = encode_approval_code(seq);
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut map = st.pending_approvals.lock().await;
        map.retain(|_, p| p.created.elapsed() <= APPROVAL_TTL);
        map.insert(
            code.clone(),
            PendingApproval {
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

/// DSH 档的 prompt 组装：返回 ACP content block 数组。
/// - `image_enabled`（ACP `promptCapabilities.image`）为真时，把企微图片下载并内联成
///   image block —— 图片才真正进模型，对齐原版 DSH 的图片理解能力；
/// - 否则（模型不支持图片，或下载失败）降级成文本线索，与旧行为一致。
/// 文件一律降级为文本线索（ACP 文件输入需要 attachment receipt，首期不做）。
async fn build_dsh_prompt(
    user_text: &str,
    media: &[wecom_proto::MediaRef],
    image_enabled: bool,
) -> Vec<serde_json::Value> {
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let text = user_text.trim();
    if !text.is_empty() {
        blocks.push(crate::wecom_dsh::acp_text_block(text));
    }
    for m in media {
        if m.kind == "image" && image_enabled {
            match fetch_base64(m.url.trim()).await {
                Ok((data, media_type)) => {
                    blocks.push(crate::wecom_dsh::acp_image_block(&media_type, &data));
                    continue;
                }
                Err(e) => {
                    blocks.push(crate::wecom_dsh::acp_text_block(&format!("[图片获取失败：{e}]")));
                    continue;
                }
            }
        }
        let label = if m.kind == "image" { "图片" } else { "文件" };
        let name = m.filename.clone().unwrap_or_default();
        let note = if name.is_empty() {
            format!("（{label}：{}）", m.url)
        } else {
            format!("（{label}「{name}」：{}）", m.url)
        };
        blocks.push(crate::wecom_dsh::acp_text_block(&note));
    }
    // ACP 不接受空 prompt：既无文字也无可内联媒体时补一个占位，交给上游的空消息兜底。
    if blocks.is_empty() {
        blocks.push(crate::wecom_dsh::acp_text_block(user_text));
    }
    blocks
}

/// 把文字 + 图片/文件附件组装成一条用户消息。
/// 视觉模型且拿到图片 url 时，下载并内联为 Image 块；否则把文件/图片作为文本线索。
async fn build_user_message(
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
    for m in media {
        if m.kind == "image" && vision {
            match fetch_base64(m.url.trim()).await {
                Ok((data, media_type)) => blocks.push(Block::Image { media_type, data }),
                Err(e) => blocks.push(Block::Text {
                    text: format!("[图片获取失败：{e}]"),
                }),
            }
        } else {
            let label = if m.kind == "image" { "图片" } else { "文件" };
            let name = m.filename.clone().unwrap_or_default();
            let note = if name.is_empty() {
                format!("（{label}：{}）", m.url)
            } else {
                format!("（{label}「{name}」：{}）", m.url)
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

async fn fetch_base64(url: &str) -> Result<(String, String), String> {
    use base64::Engine;
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("图片下载失败：{e}"))?;
    let media_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("图片读取失败：{e}"))?;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok((data, media_type))
}

/// 轻量时钟，避免为节流再引 Instant 到热路径签名里也行，直接用 std。
struct InstantApprox(std::time::Instant);
impl InstantApprox {
    fn now() -> Self {
        Self(std::time::Instant::now())
    }
    fn elapsed_ms(&self) -> u128 {
        self.0.elapsed().as_millis()
    }
}
