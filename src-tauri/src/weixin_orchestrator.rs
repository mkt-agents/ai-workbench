//! 微信入站：长轮询 getupdates → 意图/联动/天玑 → sendmessage 切段回复。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

use crate::im_answer::{self, ApprovalSlot, ImTurnRequest};
use crate::wecom_orchestrator::WecomAgentConfig;
use crate::weixin_ilink::{self, IlinkClient, WeixinMessage};

#[derive(Clone)]
pub struct WeixinRuntime {
    pub token: String,
    pub base_url: String,
    #[allow(dead_code)]
    pub bot_id: String,
    pub bound_user_id: String,
    pub answer_mode: String,
    pub doc_dir: String,
    pub agent: Option<WecomAgentConfig>,
}

pub struct WeixinOrchestrator {
    pub stop: Arc<AtomicBool>,
    pub running: Arc<AtomicBool>,
    pub last_error: Arc<Mutex<String>>,
    pub approval: ApprovalSlot,
    pub busy: Arc<AtomicBool>,
}

impl WeixinOrchestrator {
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            last_error: Arc::new(Mutex::new(String::new())),
            approval: im_answer::new_approval_slot(),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub async fn set_error(&self, msg: impl Into<String>) {
        *self.last_error.lock().await = msg.into();
    }

    pub fn spawn_loop(self: &Arc<Self>, app: AppHandle, rt: WeixinRuntime, cursor_path: std::path::PathBuf) {
        self.stop.store(false, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        let orch = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            orch.poll_loop(app, rt, cursor_path).await;
            orch.running.store(false, Ordering::SeqCst);
        });
    }

    async fn poll_loop(&self, app: AppHandle, rt: WeixinRuntime, cursor_path: std::path::PathBuf) {
        let mut cursor = load_cursor(&cursor_path);
        let mut backoff_ms: u64 = 500;
        while !self.stop.load(Ordering::SeqCst) {
            let client = match IlinkClient::new(&rt.base_url, Some(rt.token.clone())) {
                Ok(c) => c,
                Err(e) => {
                    self.set_error(e).await;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
            };
            match client.get_updates(&cursor).await {
                Ok(resp) => {
                    backoff_ms = 500;
                    if let Some(err) = resp.errcode.filter(|c| *c != 0) {
                        if err == -14 {
                            self.set_error("微信会话超时（errcode=-14），请重新扫码绑定").await;
                            break;
                        }
                        self.set_error(format!(
                            "getupdates errcode={err} {}",
                            resp.errmsg.unwrap_or_default()
                        ))
                        .await;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                    if let Some(ret) = resp.ret.filter(|r| *r != 0) {
                        if ret == -14 {
                            self.set_error("微信会话超时（ret=-14），请重新扫码绑定").await;
                            break;
                        }
                        self.set_error(format!("getupdates ret={ret}")).await;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                    if let Some(buf) = resp
                        .get_updates_buf
                        .as_ref()
                        .map(|s| s.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        cursor = buf.to_string();
                        save_cursor(&cursor_path, &cursor);
                    }
                    let msgs = resp.msgs.unwrap_or_default();
                    for msg in msgs {
                        if self.stop.load(Ordering::SeqCst) {
                            break;
                        }
                        self.handle_msg(&app, &rt, &client, msg).await;
                    }
                }
                Err(e) => {
                    self.set_error(e).await;
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms = (backoff_ms * 2).min(8_000);
                }
            }
        }
    }

    async fn handle_msg(
        &self,
        app: &AppHandle,
        rt: &WeixinRuntime,
        client: &IlinkClient,
        msg: WeixinMessage,
    ) {
        // Only user messages (type 1); ignore bot echoes.
        if msg.message_type.unwrap_or(1) == 2 {
            return;
        }
        let from = msg.from_user_id.clone().unwrap_or_default();
        if from.is_empty() {
            return;
        }
        // Group chats out of scope for MVP.
        if msg
            .group_id
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            return;
        }
        // Only the scanning/bound user.
        if !rt.bound_user_id.is_empty() && from != rt.bound_user_id {
            let token = msg.context_token.clone().unwrap_or_default();
            if !token.is_empty() {
                let _ = client
                    .send_text(&from, "当前仅响应扫码绑定的微信用户。", &token)
                    .await;
            }
            return;
        }

        let text = weixin_ilink::extract_text(&msg);
        if text.trim().is_empty() {
            return;
        }
        let context_token = msg.context_token.clone().unwrap_or_default();
        if context_token.is_empty() {
            return;
        }

        // Approval reply for in-flight turn.
        if let Some(ack) = im_answer::try_resolve_approval(&self.approval, &text).await {
            let _ = client.send_text_chunked(&from, &ack, &context_token).await;
            return;
        }

        if self.busy.load(Ordering::SeqCst) {
            let _ = client
                .send_text(&from, "上一问还在处理，请稍候再发。", &context_token)
                .await;
            return;
        }

        let Some(agent) = rt.agent.clone() else {
            let _ = client
                .send_text(
                    &from,
                    "尚未配置可用模型，请先在模型广场设置天玑默认模型。",
                    &context_token,
                )
                .await;
            return;
        };

        self.busy.store(true, Ordering::SeqCst);
        let request_id = format!(
            "weixin-{}",
            msg.message_id
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| chrono_like())
        );
        let slot = Arc::clone(&self.approval);
        let from2 = from.clone();
        let token2 = context_token.clone();
        let client2 = match IlinkClient::new(&rt.base_url, Some(rt.token.clone())) {
            Ok(c) => c,
            Err(e) => {
                self.busy.store(false, Ordering::SeqCst);
                self.set_error(e).await;
                return;
            }
        };
        let turn = ImTurnRequest {
            request_id,
            user_text: text,
            where_from: "微信私聊".into(),
            answer_mode: rt.answer_mode.clone(),
            doc_dir: rt.doc_dir.clone(),
            agent,
        };
        let app2 = app.clone();
        let result = im_answer::run_turn(app2, turn, slot, move |partial| {
            let c = client2.clone();
            let u = from2.clone();
            let tok = token2.clone();
            tauri::async_runtime::spawn(async move {
                let _ = c.send_text_chunked(&u, &partial, &tok).await;
            });
        })
        .await;
        let _ = client
            .send_text_chunked(&from, &result.text, &context_token)
            .await;
        self.busy.store(false, Ordering::SeqCst);
        // Touch app state so status consumers can refresh.
        if let Some(st) = app.try_state::<crate::weixin_channel::WeixinState>() {
            let _ = st;
        }
    }
}

fn load_cursor(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default().trim().to_string()
}

fn save_cursor(path: &std::path::Path, cursor: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, cursor.as_bytes());
}

fn chrono_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    ms.to_string()
}

