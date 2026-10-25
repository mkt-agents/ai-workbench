//! 企微智能机器人 WebSocket：订阅、心跳、收帧、发帧、重连。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tauri::AppHandle;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::wecom_proto::{self, EventCallback, InboundFrame, MsgCallback};

const PING_INTERVAL: Duration = Duration::from_secs(30);
const RECONNECT_BASE: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Error(String),
}

impl ConnState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Error(_) => "error",
        }
    }
}

pub enum Outbound {
    Raw(String),
    /// 停止当前连接循环（不重连）
    Stop,
}

pub type MsgHandler = Arc<dyn Fn(MsgCallback) + Send + Sync>;
pub type EventHandler = Arc<dyn Fn(EventCallback) + Send + Sync>;

struct Shared {
    state: RwLock<ConnState>,
    last_error: RwLock<String>,
    gen: AtomicU64,
    stop: AtomicBool,
}

pub struct WecomSocket {
    shared: Arc<Shared>,
    out_tx: Mutex<Option<mpsc::UnboundedSender<Outbound>>>,
}

impl WecomSocket {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: RwLock::new(ConnState::Disconnected),
                last_error: RwLock::new(String::new()),
                gen: AtomicU64::new(0),
                stop: AtomicBool::new(false),
            }),
            out_tx: Mutex::new(None),
        }
    }

    pub async fn status(&self) -> (String, String) {
        let st = self.shared.state.read().await.clone();
        let err = self.shared.last_error.read().await.clone();
        let detail = match &st {
            ConnState::Error(e) => e.clone(),
            _ => err,
        };
        (st.label().to_string(), detail)
    }

    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        let guard = self.out_tx.lock().await;
        let tx = guard.as_ref().ok_or_else(|| "企微长连接未建立".to_string())?;
        tx.send(Outbound::Raw(json))
            .map_err(|_| "企微发送通道已关闭".to_string())
    }

    pub async fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.gen.fetch_add(1, Ordering::SeqCst);
        if let Some(tx) = self.out_tx.lock().await.take() {
            let _ = tx.send(Outbound::Stop);
        }
        *self.shared.state.write().await = ConnState::Disconnected;
        emit_status_changed(&self.shared).await;
    }

    /// 启动（或重启）长连接。同一时刻只保留一代连接。
    pub fn start(
        self: &Arc<Self>,
        app: AppHandle,
        bot_id: String,
        secret: String,
        on_msg: MsgHandler,
        on_event: EventHandler,
    ) {
        self.shared.stop.store(false, Ordering::SeqCst);
        let gen = self.shared.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let this = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            this.run_loop(app, gen, bot_id, secret, on_msg, on_event)
                .await;
        });
    }

    async fn set_state(&self, st: ConnState) {
        *self.shared.state.write().await = st;
        emit_status_changed(&self.shared).await;
    }

    async fn run_loop(
        self: Arc<Self>,
        app: AppHandle,
        gen: u64,
        bot_id: String,
        secret: String,
        on_msg: MsgHandler,
        on_event: EventHandler,
    ) {
        let mut backoff = RECONNECT_BASE;
        while !self.shared.stop.load(Ordering::SeqCst)
            && self.shared.gen.load(Ordering::SeqCst) == gen
        {
            self.set_state(if backoff > RECONNECT_BASE {
                ConnState::Reconnecting
            } else {
                ConnState::Connecting
            })
            .await;

            match self
                .connect_once(
                    &app,
                    gen,
                    &bot_id,
                    &secret,
                    Arc::clone(&on_msg),
                    Arc::clone(&on_event),
                )
                .await
            {
                Ok(()) => {
                    // 正常 Stop 或代际作废
                    if self.shared.stop.load(Ordering::SeqCst)
                        || self.shared.gen.load(Ordering::SeqCst) != gen
                    {
                        break;
                    }
                    backoff = RECONNECT_BASE;
                }
                Err(e) => {
                    *self.shared.last_error.write().await = e.clone();
                    self.set_state(ConnState::Error(e.clone())).await;
                    eprintln!("[wecom] connect error: {e}");
                }
            }

            if self.shared.stop.load(Ordering::SeqCst)
                || self.shared.gen.load(Ordering::SeqCst) != gen
            {
                break;
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
        if self.shared.gen.load(Ordering::SeqCst) == gen {
            self.set_state(ConnState::Disconnected).await;
        }
    }

    async fn connect_once(
        &self,
        _app: &AppHandle,
        gen: u64,
        bot_id: &str,
        secret: &str,
        on_msg: MsgHandler,
        on_event: EventHandler,
    ) -> Result<(), String> {
        let (ws, _) = connect_async(wecom_proto::WS_URL)
            .await
            .map_err(|e| format!("WebSocket 连接失败: {e}"))?;
        let (mut write, mut read) = ws.split();

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Outbound>();
        {
            let mut slot = self.out_tx.lock().await;
            *slot = Some(out_tx.clone());
        }

        // 订阅
        let sub_id = format!("sub-{}", chrono::Utc::now().timestamp_millis());
        write
            .send(Message::Text(wecom_proto::subscribe(&sub_id, bot_id, secret).into()))
            .await
            .map_err(|e| format!("发送订阅失败: {e}"))?;

        // 等订阅响应（带超时）
        let subscribed = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(msg) = read.next().await {
                let msg = msg.map_err(|e| format!("读帧失败: {e}"))?;
                if let Message::Text(t) = msg {
                    let frame = wecom_proto::parse_inbound(&t)?;
                    if frame.headers.as_ref().map(|h| h.req_id.as_str()) == Some(sub_id.as_str()) {
                        let code = frame.errcode.unwrap_or(-1);
                        if code == 0 {
                            return Ok(());
                        }
                        return Err(format!(
                            "订阅失败 errcode={code} {}",
                            frame.errmsg.unwrap_or_default()
                        ));
                    }
                    // 订阅前也可能有其它帧，忽略
                }
            }
            Err("连接在订阅前关闭".to_string())
        })
        .await
        .map_err(|_| "订阅超时".to_string())??;

        let _ = subscribed;
        self.set_state(ConnState::Connected).await;
        *self.shared.last_error.write().await = String::new();

        let mut ping_tick = tokio::time::interval(PING_INTERVAL);
        ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            if self.shared.stop.load(Ordering::SeqCst)
                || self.shared.gen.load(Ordering::SeqCst) != gen
            {
                let _ = write.close().await;
                return Ok(());
            }
            tokio::select! {
                _ = ping_tick.tick() => {
                    let pid = format!("ping-{}", chrono::Utc::now().timestamp_millis());
                    if write.send(Message::Text(wecom_proto::ping(&pid).into())).await.is_err() {
                        return Err("心跳发送失败".into());
                    }
                }
                outbound = out_rx.recv() => {
                    match outbound {
                        None | Some(Outbound::Stop) => {
                            let _ = write.close().await;
                            return Ok(());
                        }
                        Some(Outbound::Raw(s)) => {
                            if write.send(Message::Text(s.into())).await.is_err() {
                                return Err("发送失败".into());
                            }
                        }
                    }
                }
                incoming = read.next() => {
                    match incoming {
                        None => return Err("连接已关闭".into()),
                        Some(Err(e)) => return Err(format!("读错误: {e}")),
                        Some(Ok(Message::Text(t))) => {
                            handle_text(&t, &on_msg, &on_event);
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = write.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(_))) => return Err("服务端关闭连接".into()),
                        Some(Ok(_)) => {}
                    }
                }
            }
        }
    }
}

fn handle_text(t: &str, on_msg: &MsgHandler, on_event: &EventHandler) {
    let frame = match wecom_proto::parse_inbound(t) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[wecom] bad frame: {e}");
            return;
        }
    };
    // 心跳 / 订阅响应
    if frame.cmd.is_none() && frame.errcode.is_some() {
        return;
    }
    if let Some(msg) = wecom_proto::parse_msg_callback(&frame) {
        on_msg(msg);
        return;
    }
    if let Some(ev) = wecom_proto::parse_event_callback(&frame) {
        on_event(ev);
        return;
    }
    let _ = frame;
}

async fn emit_status_changed(shared: &Shared) {
    // 状态变更由 channel 层统一 emit；这里只更新内存。
    let _ = shared;
}

/// 供 channel 轮询后广播
pub async fn current_state_payload(sock: &WecomSocket) -> serde_json::Value {
    let (state, detail) = sock.status().await;
    serde_json::json!({ "state": state, "detail": detail })
}

// 避免未使用警告：InboundFrame 在 handle 里用到了
#[allow(dead_code)]
fn _touch_frame(_: &InboundFrame) {}
