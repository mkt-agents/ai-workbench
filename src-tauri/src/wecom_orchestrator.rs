//! 企微入站 → 策略 → agent_run → 流式回写。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::Mutex;

use crate::agent::{
    subscribe_agent_bridge, unsubscribe_agent_bridge, AgentBridgeEvent, AgentRunRequest,
};
use crate::ai_commands::AIModelConfig;
use crate::mcp::McpServerSpec;
use crate::wecom_policy::{GateDecision, MsgDeduper, PolicyState};
use crate::wecom_proto::{self, EventCallback, MsgCallback};
use crate::wecom_sanitize::redact_local_paths;
use crate::wecom_session::{self, SessionKey};
use crate::wecom_ws::WecomSocket;

const MAX_GLOBAL_JOBS: usize = 2;
const APPROVAL_TIP: &str = "需要你在桌面端「天玑」确认后才能继续，请切换到 AI Workbench。";

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

        let bot_name = self.bot_display_name.lock().await.clone();
        let (text, mentioned) = wecom_proto::strip_mention(&msg.text, &bot_name);
        let mut user_text = text;
        if let Some(q) = &msg.quote_text {
            if !q.trim().is_empty() {
                user_text = format!("（引用）{}\n\n{}", q.trim(), user_text);
            }
        }
        if user_text.trim().is_empty() && msg.msgtype != "text" && msg.msgtype != "voice" {
            let tip = "暂不支持该消息类型，请发文字。";
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
        if user_text.trim().is_empty() {
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
        let _session_guard = lock.lock().await;

        let agent_cfg = match self.agent.lock().await.clone() {
            Some(c) => c,
            None => {
                let tip = "企微通道未绑定模型：请在设置里保存一次配置。";
                let _ = self
                    .sock
                    .send_raw(wecom_proto::respond_stream(
                        &msg.req_id,
                        &format!("cfg-{}", msg.msgid),
                        tip,
                        true,
                    ))
                    .await;
                return;
            }
        };

        self.global_jobs.fetch_add(1, Ordering::SeqCst);
        let result = self
            .run_agent_and_stream(&app, &msg, &session, &user_text, agent_cfg)
            .await;
        self.global_jobs.fetch_sub(1, Ordering::SeqCst);

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
                    &format!("err-{}", msg.msgid),
                    &tip,
                    true,
                ))
                .await;
        }
    }

    async fn run_agent_and_stream(
        &self,
        app: &AppHandle,
        msg: &MsgCallback,
        session: &SessionKey,
        user_text: &str,
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
            user: user_text.to_string(),
            request_id: request_id.clone(),
            repo_path: agent_cfg.repo_path,
            services: agent_cfg.services,
            history,
            messages: Vec::new(),
            mode: agent_cfg.mode,
            apps: Vec::new(),
            permission,
            tools: agent_cfg.tools,
            vision: agent_cfg.vision,
            diff_confirm: agent_cfg.diff_confirm,
            always_allow: agent_cfg.always_allow,
            web_allow: agent_cfg.web_allow,
            subagents: agent_cfg.subagents,
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
        let pump = tauri::async_runtime::spawn(async move {
            let redact = |s: &str| redact_local_paths(s, &repo_for_redact);
            let mut acc = String::new();
            let mut last_push = InstantApprox::now();
            let mut approval_told = false;
            while let Some(ev) = rx.recv().await {
                match ev {
                    AgentBridgeEvent::Chunk(t) => {
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
                    AgentBridgeEvent::ApprovalNeeded { .. } => {
                        if !approval_told {
                            approval_told = true;
                            let tip = if acc.is_empty() {
                                APPROVAL_TIP.to_string()
                            } else {
                                format!("{acc}\n\n—\n{APPROVAL_TIP}")
                            };
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
                    AgentBridgeEvent::Done { .. } => {
                        let body = if acc.is_empty() {
                            "（无文本回复）".to_string()
                        } else {
                            redact(&acc)
                        };
                        let _ = sock
                            .send_raw(wecom_proto::respond_stream(
                                &req_id,
                                &stream_id_clone,
                                &body,
                                true,
                            ))
                            .await;
                        return body;
                    }
                    AgentBridgeEvent::Err(e) => {
                        let body = if acc.is_empty() {
                            format!("出错了：{e}")
                        } else {
                            format!("{acc}\n\n（中断：{e}）")
                        };
                        let body = redact(&body);
                        let _ = sock
                            .send_raw(wecom_proto::respond_stream(
                                &req_id,
                                &stream_id_clone,
                                &body,
                                true,
                            ))
                            .await;
                        return body;
                    }
                }
            }
            // 通道关闭但没 Done：补 finish
            let body = if acc.is_empty() {
                "（已结束）".to_string()
            } else {
                redact(&acc)
            };
            let _ = sock
                .send_raw(wecom_proto::respond_stream(
                    &req_id,
                    &stream_id_clone,
                    &body,
                    true,
                ))
                .await;
            body
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
