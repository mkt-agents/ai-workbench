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
use crate::wecom_dsh::{permission_options, permission_summary, DshEvent, DshResponder};
use crate::wecom_policy::{GateDecision, MsgDeduper, PolicyState};
use crate::wecom_proto::{self, EventCallback, MsgCallback};
use crate::wecom_sanitize::redact_local_paths;
use crate::wecom_session::{self, SessionKey};
use crate::wecom_ws::WecomSocket;

const MAX_GLOBAL_JOBS: usize = 2;
/// 全局并发上限的允许范围（设置页可调；fail-safe 到默认）。
const MIN_GLOBAL_JOBS: usize = 1;
const MAX_GLOBAL_JOBS_CAP: usize = 8;
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
    /// 问答模式：`doc`（文档优先）| `doc_code`（文档+代码回退）| `code`（仅代码）。
    pub answer_mode: Arc<Mutex<String>>,
    /// 逆向文档目录（相对工作区），默认 `doc`。
    pub doc_dir: Arc<Mutex<String>>,
    deduper: Arc<Mutex<MsgDeduper>>,
    /// 同会话串行
    session_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// 当前每个会话正在跑的那一轮（供「停止」指令真正取消 + 收尾卡住的气泡）。
    running: Arc<Mutex<HashMap<String, RunningTurn>>>,
    global_jobs: Arc<AtomicUsize>,
    /// 全局并发上限（设置页可调；默认 MAX_GLOBAL_JOBS）。
    max_jobs: Arc<AtomicUsize>,
}

/// 一个会话正在进行中的回合：记录出站目标 + 如何取消。
struct RunningTurn {
    /// 平台回帧用的 req_id 与当前正文流 id（取消后往这条补 finish 帧，收尾「正在思考…」）。
    req_id: String,
    stream_id: String,
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
            engine: Arc::new(Mutex::new("tianji".to_string())),
            answer_mode: Arc::new(Mutex::new("auto".to_string())),
            doc_dir: Arc::new(Mutex::new("doc".to_string())),
            deduper: Arc::new(Mutex::new(MsgDeduper::new(2048))),
            session_locks: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(Mutex::new(HashMap::new())),
            global_jobs: Arc::new(AtomicUsize::new(0)),
            max_jobs: Arc::new(AtomicUsize::new(MAX_GLOBAL_JOBS)),
        }
    }

    /// 设置全局并发上限（设置页下发）；越界回落默认值。
    pub fn set_max_jobs(&self, n: usize) {
        let v = if (MIN_GLOBAL_JOBS..=MAX_GLOBAL_JOBS_CAP).contains(&n) {
            n
        } else {
            MAX_GLOBAL_JOBS
        };
        self.max_jobs.store(v, Ordering::SeqCst);
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

        // 「停止」指令：在拿会话串行锁**之前**处理（上一轮没结束它拿不到锁）。
        // 真正取消那一轮并给卡住的「正在思考…」气泡补一帧 finish，而不是当普通消息答一句。
        if is_stop_command(&user_text) {
            let key = session.as_str();
            let running = {
                let mut map = self.running.lock().await;
                map.remove(&key)
            };
            match running {
                Some(rt) => {
                    rt.cancel();
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
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(
                            &msg.req_id,
                            &format!("stop-{}", msg.msgid),
                            "已停止当前任务。",
                            true,
                        ))
                        .await;
                }
                None => {
                    crate::wecom_log::write(
                        "stop",
                        &format!("msgid={} 会话 {key} 无在跑回合", msg.msgid),
                    );
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(
                            &msg.req_id,
                            &format!("stop-{}", msg.msgid),
                            "当前没有正在进行的任务。",
                            true,
                        ))
                        .await;
                }
            }
            return;
        }

        if self.global_jobs.load(Ordering::SeqCst) >= self.max_jobs.load(Ordering::SeqCst) {
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
        let user_msg = build_user_message(&user_text, &msg.media, agent_cfg.vision).await;

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
        let request = format!("wecom-preflight-{}", msg.msgid);
        let _guard = crate::cancellation::CancelGuard::new(&request);
        self.running.lock().await.insert(
            session.as_str(),
            RunningTurn {
                req_id: msg.req_id.clone(),
                stream_id: stream_id.to_string(),
                cancel: CancelHandle::Tianji {
                    request_id: request.clone(),
                },
            },
        );
        let result = tokio::select! {
            biased;
            _ = async {
                loop {
                    if crate::cancellation::is_cancelled(&request) { break; }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            } => Ok(()),
            _ = tokio::time::sleep(DSH_TURN_TIMEOUT) => {
                crate::cancellation::cancel_request(&request);
                if let Some(rt) = self.running.lock().await.get(&session.as_str()) { rt.cancel(); }
                send_finish(&self.sock, &msg.req_id, stream_id, "排障回合达到600秒期限，已停止；未完成项不视为查询无数据。").await;
                Ok(())
            },
            result = self.run_dsh_turn_inner(app, msg, session, user_text, stream_id, &request) => result,
        };
        self.unregister_running(session, stream_id).await;
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
        let redact = |s: &str| redact_local_paths(s, &repo);

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
            &msg.userid,
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
        if crate::cancellation::is_cancelled(preflight_request) {
            return Ok(());
        }
        // Source/parse failures are deterministic replies, never model-rewritten as empty logs.
        if source_failure_reply(&outcome) {
            send_finish(&self.sock, &req_id, stream_id, &redact(&outcome.context)).await;
            wecom_session::append_turn(session, user_text, &redact(&outcome.context));
            return Ok(());
        }
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

        let key = session.as_str();
        if crate::wecom_investigation_query::starts_new_investigation(user_text) {
            engine.reset_channel_session(&key).await;
        }
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
        let mut evidence = outcome.context.clone();
        let mut has_evidence = outcome.may_analyze;
        let mut assessed_level = "unknown".to_string();
        let mut assessed_gaps = outcome.decision_gaps.clone();
        let mut last_stop_reason = String::new();
        let mut forced_context_used = false;
        if outcome.source_ready {
            if let Some(original) = outcome.query.clone() {
                for round in 0..2 {
                    if outcome.budget.remaining() == 0 || outcome.collected_ms >= 120_000 {
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
                    let instruction = format!("{}\n原问题：{}\n固定查询范围：{} 至 {}\n已有证据：{}\n本次只输出JSON补查计划，不输出最终业务分析。没有日志时只查项目日志配置、服务标识、字段别名和日志模板；不要搜索本地.log，不要把业务ID在源码无命中当故障证据。",
                        crate::wecom_investigation_plan::planning_instruction(), user_text, original.start, original.end, evidence);
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
                        )
                        .await?;
                    let mut parsed = crate::wecom_investigation_plan::parse_plan(
                        &draft, &original, &evidence, &repo,
                    );
                    if let Err(error) = &parsed {
                        draft = self.dsh_plan_text(app, &engine, &acp_session, &format!("补查计划未通过本地校验：{error}。请修正一次，仅输出符合schema的JSON；不要执行计划。{}", crate::wecom_investigation_plan::planning_instruction()), preflight_request, &req_id, stream_id, turn_deadline).await?;
                        parsed = crate::wecom_investigation_plan::parse_plan(
                            &draft, &original, &evidence, &repo,
                        );
                    }
                    let mut plan = match parsed {
                        Ok(plan) => plan,
                        Err(_) => {
                            evidence.push_str(
                                "\n补查计划两次未通过校验，未执行；当前结论仅限已有证据。\n",
                            );
                            break;
                        }
                    };
                    assessed_level = plan.conclusion_level.clone();
                    assessed_gaps = plan.gaps.clone();
                    last_stop_reason = plan.stop_reason.clone();
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
                    let _ = self
                        .sock
                        .send_raw(wecom_proto::respond_stream(
                            &req_id,
                            stream_id,
                            &format!("正在执行第{}轮代码线索补查…", round + 1),
                            false,
                        ))
                        .await;
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
                        .replace("[L", &format!("[R{}L", round + 1))
                        .replace("[T", &format!("[R{}T", round + 1));
                    evidence.push_str(&format!(
                        "\n【补查轮次{}；来源候选：{}】\n{}",
                        round + 1,
                        plan.reason,
                        extra_context
                    ));
                    // The latest round is bounded, and only source metadata is retained across turns.
                    if evidence.chars().count() > 48_000 {
                        // Retain the newest targeted evidence instead of silently cutting it off after a large first batch.
                        let tail: String = extra_context.chars().take(24_000).collect();
                        let head: String = evidence.chars().take(23_500).collect();
                        evidence = format!("{head}\n【累计证据摘录：早期记录部分省略；以下优先保留最新补查】\n{tail}\n覆盖不完整，不据缺失记录断因。\n");
                        outcome.truncated_count = outcome.truncated_count.saturating_add(1);
                        outcome.status = crate::wecom_investigation::InvestigationStatus::Partial;
                        outcome
                            .decision_gaps
                            .push("累计证据超过48k，早期记录部分未完整展示".into());
                        crate::wecom_investigation::record_cumulative(&outcome, preflight_request);
                        break;
                    }
                    if !extra.source_ready {
                        break;
                    }
                }
            }
            if !has_evidence {
                let body = redact(&format!("本轮尚未取得该业务的真实日志证据，未生成业务根因结论。\n{}\n请核对环境、日志保留周期或提供可查的服务/请求标识。", evidence));
                send_finish(&self.sock, &req_id, stream_id, &body).await;
                wecom_session::append_turn(session, user_text, &body);
                return Ok(());
            }
        }
        for gap in &assessed_gaps {
            if !outcome.decision_gaps.contains(gap) {
                outcome.decision_gaps.push(gap.clone());
            }
        }
        crate::wecom_investigation::record_cumulative(&outcome, preflight_request);
        let evidence_rules = if has_evidence {
            let level = permitted_conclusion_level(&assessed_level, &assessed_gaps, &outcome);
            format!("\n结论分层评审：允许最高层级={level}；模型停止理由={last_stop_reason}；尚未解释的决策={:?}；未完成页={}；截断记录={}。必须依次列出结果、直接原因、具体触发原因。过滤为0/STATUS_FILTERED_EMPTY仅是失败出口，不等于已定位具体过滤器。未取得实际规则、判定值及WARN/异常证据时，写‘已确认失败出口，具体触发原因尚未确认’，禁止‘证据链已闭合/根因直接命中’。正常span仅表示没有APM技术错误，不能据此排除业务警告、被捕获异常或降级。先引用日志和时间，再用代码解释分支，再用补查验证。共用兜底文案不唯一指向去重/库存/价格；缺日志不等于分支未执行；说明分页、采样、异步与版本覆盖。", assessed_gaps, outcome.incomplete_pages, outcome.truncated_count)
        } else {
            String::new()
        };
        let prompt_blocks = build_dsh_prompt(
            user_text,
            &msg.media,
            engine.image_prompt_enabled().await,
            &format!("{mode_rules}{evidence_rules}"),
            &evidence,
        )
        .await;
        let mut turn = engine.prompt(&acp_session, prompt_blocks).await?;

        // 登记为该会话「在跑的回合」，供「停止」指令 session/cancel + 收尾气泡。
        {
            let mut map = self.running.lock().await;
            map.insert(
                session.as_str(),
                RunningTurn {
                    req_id: req_id.clone(),
                    stream_id: stream_id.to_string(),
                    cancel: CancelHandle::Dsh {
                        responder: turn.responder(),
                    },
                },
            );
        }

        let mut acc = String::new();
        let mut last_push = InstantApprox::now();
        let mut last_event = std::time::Instant::now();
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
                                    &req_id, stream_id, &out, false,
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
                    crate::wecom_log::write("perm", &format!("msgid={} {summary}", msg.msgid));
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
        // 注销「在跑回合」登记（仅当还是本回合时移除，避免误删后来者）。
        self.unregister_running(session, stream_id).await;
        wecom_session::append_turn(session, user_text, &final_text);
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
    ) -> Result<String, String> {
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
                    let (code, reply) = register_dsh_approval(app, summary.clone()).await;
                    cleanup.approvals.push((code.clone(), rpc_id));
                    let tip = redact_local_paths(&format!("日志取证需确认：{summary}\n回复「{code} 1」允许一次；「{code} 2」取消（5分钟有效）"), &engine.workspace().await);
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
                Some(DshEvent::Done(Ok(result))) => {
                    if result["stopReason"] == "cancelled" {
                        break Err("cancelled: DSH计划已取消".into());
                    }
                    break Ok(text);
                }
                Some(DshEvent::Done(Err(e))) => break Err(e),
                None => break Err("DSH计划会话意外结束".into()),
                _ => {}
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

        // 登记为该会话「在跑的回合」：「停止」指令据此 cancel_request + 收尾气泡。
        {
            let mut map = self.running.lock().await;
            map.insert(
                session.as_str(),
                RunningTurn {
                    req_id: msg.req_id.clone(),
                    stream_id: stream_id.clone(),
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
回复勿输出本机绝对路径，文件用相对路径或文件名。\n{mode_rules}{sky_rule}\n{}",
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
                    Some(AgentBridgeEvent::ApprovalNeeded {
                        call_ids,
                        summary,
                        can_remember,
                    }) => {
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

        // 注销「在跑回合」登记（仅当还是本回合时移除）。
        self.unregister_running(session, &stream_id).await;

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

/// DSH 档的 prompt 组装：返回 ACP content block 数组。
/// - `image_enabled`（ACP `promptCapabilities.image`）为真时，把企微图片下载并内联成
///   image block —— 图片才真正进模型，对齐原版 DSH 的图片理解能力；
/// - 否则（模型不支持图片，或下载失败）降级成文本线索，与旧行为一致。
/// 文件一律降级为文本线索（ACP 文件输入需要 attachment receipt，首期不做）。
async fn build_dsh_prompt(
    user_text: &str,
    media: &[wecom_proto::MediaRef],
    image_enabled: bool,
    mode_rules: &str,
    sky_context: &str,
) -> Vec<serde_json::Value> {
    let mut blocks: Vec<serde_json::Value> = Vec::new();
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
    for m in media {
        if m.kind == "image" && image_enabled {
            match fetch_base64(m.url.trim()).await {
                Ok((data, media_type)) => {
                    blocks.push(crate::wecom_dsh::acp_image_block(&media_type, &data));
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
        let label = if m.kind == "image" {
            "图片"
        } else {
            "文件"
        };
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
            let label = if m.kind == "image" {
                "图片"
            } else {
                "文件"
            };
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

fn merge_investigation_coverage(
    base: &mut crate::wecom_investigation::InvestigationOutcome,
    extra: &crate::wecom_investigation::InvestigationOutcome,
) {
    use crate::wecom_investigation::InvestigationStatus;
    base.collected_ms = base.collected_ms.saturating_add(extra.collected_ms);
    base.request_count = base.budget.used();
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

    #[test]
    fn unexecuted_or_failed_source_never_enters_business_model() {
        use crate::wecom_investigation::{InvestigationOutcome, InvestigationStatus};
        let mut o = InvestigationOutcome {
            status: InvestigationStatus::NotExecuted,
            context: "未执行：时间解析失败".into(),
            query: None,
            evidence_count: 0,
            request_count: 0,
            may_analyze: false,
            source_ready: false,
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
    fn set_max_jobs_clamps_out_of_range() {
        let orch = Orchestrator::new(Arc::new(WecomSocket::new()));
        orch.set_max_jobs(4);
        assert_eq!(orch.max_jobs.load(Ordering::SeqCst), 4);
        // 0 / 越界回落默认
        orch.set_max_jobs(0);
        assert_eq!(orch.max_jobs.load(Ordering::SeqCst), MAX_GLOBAL_JOBS);
        orch.set_max_jobs(99);
        assert_eq!(orch.max_jobs.load(Ordering::SeqCst), MAX_GLOBAL_JOBS);
        orch.set_max_jobs(8);
        assert_eq!(orch.max_jobs.load(Ordering::SeqCst), 8);
    }
}
