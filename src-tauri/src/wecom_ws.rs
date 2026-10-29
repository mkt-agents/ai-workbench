//! 企微智能机器人 WebSocket：订阅、心跳、收帧、发帧、重连。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tauri::AppHandle;
use tokio::sync::{oneshot, Mutex, Notify, RwLock};
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

const OUTBOUND_CAPACITY: usize = 64;
// Conservative local pacing, not a claimed platform rate/ACK contract.
const OUTBOUND_INTERVAL: Duration = Duration::from_millis(200);

pub enum Outbound {
    Raw(String),
    Flush(oneshot::Sender<Result<(), String>>),
    Stop,
}

/// Only the exact stream shape produced by wecom_proto is coalescible.
fn stream_frame(raw: &str) -> Option<(String, String, bool)> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if v.get("cmd")?.as_str()? != "aibot_respond_msg"
        || v.pointer("/body/msgtype")?.as_str()? != "stream"
    {
        return None;
    }
    let req = v.pointer("/headers/req_id")?.as_str()?;
    let id = v.pointer("/body/stream/id")?.as_str()?;
    if req.is_empty() || id.is_empty() {
        return None;
    }
    Some((
        req.into(),
        id.into(),
        v.pointer("/body/stream/finish")?.as_bool()?,
    ))
}

#[derive(Default)]
struct QueueState {
    items: VecDeque<Outbound>,
    closed: bool,
}

impl QueueState {
    fn merge_progress(&mut self, raw: &str) -> bool {
        let Some((req, id, false)) = stream_frame(raw) else {
            return false;
        };
        for item in self.items.iter_mut().rev() {
            match item {
                // Never move new progress across a flush or final boundary.
                Outbound::Flush(_) | Outbound::Stop => break,
                Outbound::Raw(old) => {
                    if let Some((old_req, old_id, finish)) = stream_frame(old) {
                        if req == old_req && id == old_id {
                            if finish {
                                break;
                            }
                            *old = raw.to_string();
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

#[derive(Default)]
struct Outbox {
    state: Mutex<QueueState>,
    changed: Notify,
}

impl Outbox {
    async fn send(&self, item: Outbound) -> Result<(), String> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut q = self.state.lock().await;
                if q.closed {
                    return Err("企微发送通道已关闭".into());
                }
                if let Outbound::Raw(raw) = &item {
                    if q.merge_progress(raw) {
                        return Ok(());
                    }
                }
                if q.items.len() < OUTBOUND_CAPACITY {
                    q.items.push_back(item);
                    self.changed.notify_waiters();
                    return Ok(());
                }
            }
            changed.await;
        }
    }

    async fn recv(&self) -> Option<Outbound> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut q = self.state.lock().await;
                if let Some(item) = q.items.pop_front() {
                    self.changed.notify_waiters();
                    return Some(item);
                }
                if q.closed {
                    return None;
                }
            }
            changed.await;
        }
    }

    async fn close(&self) {
        let mut q = self.state.lock().await;
        q.closed = true;
        // Dropping flush senders tells waiters that the writer closed, not success.
        q.items.clear();
        self.changed.notify_waiters();
    }
}

struct Running {
    cancel: Option<oneshot::Sender<()>>,
    done: oneshot::Receiver<()>,
}

async fn run_leased<L, F: std::future::Future<Output = ()>>(
    lease: L,
    cancelled: oneshot::Receiver<()>,
    connection: F,
) {
    tokio::select! {
        biased;
        _ = cancelled => {},
        _ = connection => {},
    }
    // select's future (including both socket halves) has been destroyed here.
    drop(lease);
}

pub type MsgHandler = Arc<dyn Fn(MsgCallback) + Send + Sync>;
pub type EventHandler = Arc<dyn Fn(EventCallback) + Send + Sync>;

struct Shared {
    state: RwLock<ConnState>,
    last_error: RwLock<String>,
    /// 最近一次进入 Connected 的时间戳（ms since epoch）；0 = 从未连上/已断。
    connected_since: AtomicU64,
    gen: AtomicU64,
    stop: AtomicBool,
}

pub struct WecomSocket {
    shared: Arc<Shared>,
    out_tx: Mutex<Option<Arc<Outbox>>>,
    publication: Mutex<()>,
    lifecycle: Mutex<()>,
    running: Mutex<Option<Running>>,
}

impl WecomSocket {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: RwLock::new(ConnState::Disconnected),
                last_error: RwLock::new(String::new()),
                connected_since: AtomicU64::new(0),
                gen: AtomicU64::new(0),
                stop: AtomicBool::new(false),
            }),
            out_tx: Mutex::new(None),
            publication: Mutex::new(()),
            lifecycle: Mutex::new(()),
            running: Mutex::new(None),
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

    /// 连接详情：最近一次错误、已连接时长（毫秒，未连上为 None）。
    pub async fn detail(&self) -> (String, Option<u64>) {
        let last_error = self.shared.last_error.read().await.clone();
        let since = self.shared.connected_since.load(Ordering::SeqCst);
        let connected_ms = if since > 0 {
            Some(now_ms().saturating_sub(since))
        } else {
            None
        };
        (last_error, connected_ms)
    }

    /// Success means enqueued (possibly replacing pending progress), not delivery.
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        let tx = self
            .out_tx
            .lock()
            .await
            .clone()
            .ok_or_else(|| "企微长连接未建立".to_string())?;
        let metadata = stream_frame(&json);
        tx.send(Outbound::Raw(json)).await?;
        if let Some((req, id, finish)) = metadata {
            record_transport(&req, &id, finish, "enqueued");
        }
        Ok(())
    }

    /// Barrier for prior queued frames only; does not wait for platform ACK.
    pub async fn flush(&self) -> Result<(), String> {
        let tx = self
            .out_tx
            .lock()
            .await
            .clone()
            .ok_or_else(|| "企微长连接未建立".to_string())?;
        let (done, wait) = oneshot::channel();
        tx.send(Outbound::Flush(done)).await?;
        wait.await
            .map_err(|_| "企微 writer 已关闭，flush 未完成".to_string())?
    }

    /// Offline writer seam for orchestrator retirement tests. Caller awaits/aborts
    /// the returned task; stop closes its queue. No AppHandle or socket is created.
    #[cfg(test)]
    pub(crate) async fn capture_test_frames(&self) -> (
        tokio::sync::mpsc::UnboundedReceiver<String>, tokio::task::JoinHandle<()>,
    ) {
        let out = Arc::new(Outbox::default());
        {
            let _publication = self.publication.lock().await;
            self.shared.stop.store(false, Ordering::SeqCst);
            self.shared.gen.fetch_add(1, Ordering::SeqCst);
            *self.out_tx.lock().await = Some(Arc::clone(&out));
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            while let Some(item) = out.recv().await {
                match item {
                    Outbound::Raw(raw) => { if tx.send(raw).is_err() { break; } }
                    Outbound::Flush(done) => { let _ = done.send(Ok(())); }
                    Outbound::Stop => break,
                }
            }
            out.close().await;
        });
        (rx, task)
    }

    pub async fn stop(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_inner().await;
    }

    async fn stop_inner(&self) {
        {
            let _publication = self.publication.lock().await;
            self.shared.stop.store(true, Ordering::SeqCst);
            self.shared.gen.fetch_add(1, Ordering::SeqCst);
            if let Some(tx) = self.out_tx.lock().await.take() {
                tx.close().await;
            }
            *self.shared.state.write().await = ConnState::Disconnected;
            self.shared.connected_since.store(0, Ordering::SeqCst);
        }
        // Keep the completion receiver in self across cancellation of this caller.
        // Never await the task while holding publication (run_loop uses it).
        let mut running = self.running.lock().await;
        if let Some(task) = running.as_mut() {
            if let Some(cancel) = task.cancel.take() {
                let _ = cancel.send(());
            }
            let _ = (&mut task.done).await;
        }
        *running = None;
        emit_status_changed(&self.shared).await;
    }

    /// 启动（或重启）长连接。同一时刻只保留一代连接。
    pub async fn start(
        self: &Arc<Self>,
        app: AppHandle,
        bot_id: String,
        secret: String,
        on_msg: MsgHandler,
        on_event: EventHandler,
    ) -> Result<(), String> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_inner().await;
        let lease = match crate::wecom_bot_lease::BotLease::acquire(&bot_id) {
            Ok(lease) => lease,
            Err(e) => {
                *self.shared.last_error.write().await = e.clone();
                *self.shared.state.write().await = ConnState::Error(e.clone());
                return Err(e);
            }
        };
        let gen = {
            let _publication = self.publication.lock().await;
            self.shared.stop.store(false, Ordering::SeqCst);
            self.shared.gen.fetch_add(1, Ordering::SeqCst) + 1
        };
        let this = Arc::clone(self);
        let (cancel, cancelled) = oneshot::channel();
        let (done, completed) = oneshot::channel();
        let mut running = self.running.lock().await;
        tauri::async_runtime::spawn(async move {
            // select drops the losing future and its socket halves first. The lease
            // stays owned by this wrapper until then, even if stop's caller cancels.
            run_leased(
                lease,
                cancelled,
                Arc::clone(&this).run_loop(app, gen, bot_id, secret, on_msg, on_event),
            )
            .await;
            {
                let _publication = this.publication.lock().await;
                if this.shared.gen.load(Ordering::SeqCst) == gen {
                    if let Some(tx) = this.out_tx.lock().await.take() {
                        tx.close().await;
                    }
                }
            }
            let _ = done.send(());
        });
        *running = Some(Running {
            cancel: Some(cancel),
            done: completed,
        });
        Ok(())
    }

    fn is_current(&self, gen: u64) -> bool {
        !self.shared.stop.load(Ordering::SeqCst) && self.shared.gen.load(Ordering::SeqCst) == gen
    }

    async fn publish_sender(&self, gen: u64, sender: Arc<Outbox>) -> bool {
        let _publication = self.publication.lock().await;
        if !self.is_current(gen) {
            sender.close().await;
            return false;
        }
        *self.out_tx.lock().await = Some(sender);
        true
    }

    async fn clear_sender(&self, gen: u64) {
        let _publication = self.publication.lock().await;
        if self.shared.gen.load(Ordering::SeqCst) == gen {
            if let Some(tx) = self.out_tx.lock().await.take() {
                tx.close().await;
            }
        }
    }

    async fn set_state(&self, gen: u64, st: ConnState) {
        let _publication = self.publication.lock().await;
        if !self.is_current(gen) {
            return;
        }
        if let ConnState::Error(e) = &st {
            *self.shared.last_error.write().await = e.clone();
            crate::wecom_log::write("socket", &format!("state=error {e}"));
        } else {
            crate::wecom_log::write("socket", &format!("state={}", st.label()));
        }
        // 维护「已连接时长」的起点：进 Connected 记时间，离开清零。
        match &st {
            ConnState::Connected => {
                *self.shared.last_error.write().await = String::new();
                self.shared
                    .connected_since
                    .store(now_ms(), Ordering::SeqCst);
            }
            _ => {
                self.shared.connected_since.store(0, Ordering::SeqCst);
            }
        }
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
            self.set_state(
                gen,
                if backoff > RECONNECT_BASE {
                    ConnState::Reconnecting
                } else {
                    ConnState::Connecting
                },
            )
            .await;

            let result = self
                .connect_once(
                    &app,
                    gen,
                    &bot_id,
                    &secret,
                    Arc::clone(&on_msg),
                    Arc::clone(&on_event),
                )
                .await;
            self.clear_sender(gen).await;
            match result {
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
                    if !self.is_current(gen) {
                        break;
                    }
                    self.set_state(gen, ConnState::Error(e.clone())).await;
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
            self.set_state(gen, ConnState::Disconnected).await;
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

        let out_rx = Arc::new(Outbox::default());
        if !self.publish_sender(gen, Arc::clone(&out_rx)).await {
            let _ = write.close().await;
            return Ok(());
        }

        // 订阅
        let sub_id = format!("sub-{}", chrono::Utc::now().timestamp_millis());
        write
            .send(Message::Text(
                wecom_proto::subscribe(&sub_id, bot_id, secret).into(),
            ))
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
        self.set_state(gen, ConnState::Connected).await;

        let mut ping_tick = tokio::time::interval(PING_INTERVAL);
        ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut next_send = tokio::time::Instant::now();

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
                outbound = async {
                    tokio::time::sleep_until(next_send).await;
                    out_rx.recv().await
                } => {
                    match outbound {
                        None | Some(Outbound::Stop) => {
                            let _ = write.close().await;
                            return Ok(());
                        }
                        Some(Outbound::Flush(done)) => {
                            let result = write.flush().await.map_err(|_| "企微 socket flush 失败".to_string());
                            let failed = result.is_err();
                            let _ = done.send(result);
                            if failed { return Err("企微 socket flush 失败".into()); }
                        }
                        Some(Outbound::Raw(s)) => {
                            let metadata = stream_frame(&s);
                            if write.send(Message::Text(s.into())).await.is_err() {
                                return Err("发送失败".into());
                            }
                            next_send = tokio::time::Instant::now() + OUTBOUND_INTERVAL;
                            if let Some((req, id, finish)) = metadata {
                                record_transport(&req, &id, finish, "socket_written");
                            }
                        }
                    }
                }
                incoming = read.next() => {
                    match incoming {
                        None => return Err("连接已关闭".into()),
                        Some(Err(e)) => return Err(format!("读错误: {e}")),
                        Some(Ok(Message::Text(t))) => {
                            let _publication = self.publication.lock().await;
                            if self.is_current(gen) { handle_text(&t, &on_msg, &on_event); }
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

fn record_transport(req: &str, id: &str, finish: bool, phase: &str) {
    if let Some(request) = crate::wecom_diagnostics::transport_request(req) {
        crate::wecom_diagnostics::record(
            &request,
            phase,
            "observed",
            None,
            None,
            serde_json::json!({"streamId":id,"final":finish}),
        );
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
    // 平台对出站帧的回执：errcode != 0 说明这条回复被拒了（流式 id 过期 / req_id 失效…）。
    // 以前这里一律静默丢掉，排查「Bot 不回话」时完全没线索，所以至少留一行日志。
    if frame.errcode.is_some() {
        let cmd = frame.cmd.clone().unwrap_or_default();
        let req_id = frame
            .headers
            .as_ref()
            .map(|h| h.req_id.clone())
            .unwrap_or_default();
        crate::wecom_log::write(
            "errcode",
            &format!(
                "cmd={} req_id={} errcode={:?} errmsg={:?}",
                cmd, req_id, frame.errcode, frame.errmsg
            ),
        );
        if let Some(request) = crate::wecom_diagnostics::transport_request(&req_id) {
            let code = frame.errcode.unwrap_or(-1);
            // req_id is reused by progress and final. This ACK has no frame
            // identity: record the platform response but leave final delivery unknown.
            crate::wecom_diagnostics::record(
                &request,
                "platform_ack",
                if code == 0 { "accepted" } else { "rejected" },
                Some("final_frame_unknown"),
                None,
                serde_json::json!({"errcode":code}),
            );
        }
        if frame.cmd.is_none() {
            return;
        }
    }
    if let Some(msg) = wecom_proto::parse_msg_callback(&frame) {
        crate::wecom_log::write(
            "recv",
            &format!(
                "msgid={} user={} chat={} req_id={} text_len={} media={}",
                msg.msgid,
                msg.userid,
                msg.chattype,
                if msg.req_id.is_empty() {
                    "<empty>"
                } else {
                    msg.req_id.as_str()
                },
                msg.text.chars().count(),
                msg.media.len()
            ),
        );
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn latest_progress_is_bounded_and_final_and_other_commands_survive() {
        let out = Outbox::default();
        for i in 0..1000 {
            out.send(Outbound::Raw(wecom_proto::respond_stream(
                "r",
                "s",
                &i.to_string(),
                false,
            )))
            .await
            .unwrap();
        }
        assert_eq!(out.state.lock().await.items.len(), 1);
        let final_frame = wecom_proto::respond_stream("r", "s", "final", true);
        let welcome = wecom_proto::respond_welcome_text("w", "welcome");
        out.send(Outbound::Raw(final_frame.clone())).await.unwrap();
        out.send(Outbound::Raw(welcome.clone())).await.unwrap();
        assert!(
            matches!(out.recv().await, Some(Outbound::Raw(s)) if s == wecom_proto::respond_stream("r", "s", "999", false))
        );
        assert!(matches!(out.recv().await, Some(Outbound::Raw(s)) if s == final_frame));
        assert!(matches!(out.recv().await, Some(Outbound::Raw(s)) if s == welcome));
        for frame in [
            wecom_proto::respond_image("r", "s", "image/png", "fixture"),
            wecom_proto::respond_file("r", "s", "m", "file"),
            wecom_proto::ping("p"),
            "not-json".into(),
        ] {
            assert!(stream_frame(&frame).is_none());
            out.send(Outbound::Raw(frame.clone())).await.unwrap();
            assert!(matches!(out.recv().await, Some(Outbound::Raw(s)) if s == frame));
        }
    }

    #[tokio::test]
    async fn flush_barrier_prevents_later_progress_replacing_prior_frame() {
        let sock = WecomSocket::new();
        generation(&sock, 1).await;
        let out = Arc::new(Outbox::default());
        assert!(sock.publish_sender(1, Arc::clone(&out)).await);
        let before = wecom_proto::respond_stream("r", "s", "before", false);
        let after = wecom_proto::respond_stream("r", "s", "after", false);
        sock.send_raw(before.clone()).await.unwrap();
        let flush = sock.flush();
        tokio::pin!(flush);
        // Poll once: barrier is queued, completion still awaits writer.
        assert!(futures_util::poll!(&mut flush).is_pending());
        sock.send_raw(after.clone()).await.unwrap();
        assert!(matches!(out.recv().await, Some(Outbound::Raw(s)) if s == before));
        let Some(Outbound::Flush(done)) = out.recv().await else {
            panic!("missing barrier");
        };
        done.send(Ok(())).unwrap();
        flush.await.unwrap();
        assert!(matches!(out.recv().await, Some(Outbound::Raw(s)) if s == after));
        let flush = sock.flush();
        tokio::pin!(flush);
        assert!(futures_util::poll!(&mut flush).is_pending());
        out.close().await;
        assert!(flush.await.is_err());
    }

    #[tokio::test]
    async fn full_queue_backpressures_final_without_dropping_it_and_close_wakes_waiters() {
        let out = Outbox::default();
        for _ in 0..OUTBOUND_CAPACITY {
            out.send(Outbound::Raw("other-command".into()))
                .await
                .unwrap();
        }
        let final_frame = wecom_proto::respond_stream("r", "s", "final", true);
        let send = out.send(Outbound::Raw(final_frame.clone()));
        tokio::pin!(send);
        assert!(futures_util::poll!(&mut send).is_pending());
        out.recv().await.unwrap();
        send.await.unwrap();
        for _ in 1..OUTBOUND_CAPACITY {
            out.recv().await.unwrap();
        }
        assert!(matches!(out.recv().await, Some(Outbound::Raw(s)) if s == final_frame));
        out.close().await;
        assert!(out.send(Outbound::Raw("late".into())).await.is_err());
    }

    #[tokio::test]
    async fn cancellation_drops_connection_before_releasing_lease() {
        struct DropProbe {
            order: Arc<std::sync::Mutex<Vec<&'static str>>>,
            label: &'static str,
        }
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.order.lock().unwrap().push(self.label);
            }
        }
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let lease = DropProbe {
            order: Arc::clone(&order),
            label: "lease",
        };
        let socket = DropProbe {
            order: Arc::clone(&order),
            label: "socket",
        };
        let (cancel, cancelled) = oneshot::channel();
        let task = run_leased(lease, cancelled, async move {
            let _socket = socket;
            std::future::pending::<()>().await;
        });
        tokio::pin!(task);
        assert!(futures_util::poll!(&mut task).is_pending());
        assert!(order.lock().unwrap().is_empty());
        cancel.send(()).unwrap();
        task.await;
        assert_eq!(*order.lock().unwrap(), vec!["socket", "lease"]);
    }

    // No AppHandle, connector, network or profile process is needed: exercise the
    // exact production publication methods after a simulated connection completes.
    async fn generation(sock: &WecomSocket, value: u64) {
        let _publication = sock.publication.lock().await;
        sock.shared.stop.store(false, Ordering::SeqCst);
        sock.shared.gen.store(value, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn stale_sender_cannot_replace_current_sender() {
        let sock = WecomSocket::new();
        generation(&sock, 2).await;
        let current_rx = Arc::new(Outbox::default());
        let current_tx = Arc::clone(&current_rx);
        assert!(sock.publish_sender(2, current_tx).await);
        let stale_rx = Arc::new(Outbox::default());
        let stale_tx = Arc::clone(&stale_rx);
        assert!(!sock.publish_sender(1, stale_tx).await);
        assert!(stale_rx.recv().await.is_none());
        sock.send_raw("current".into()).await.unwrap();
        assert!(matches!(current_rx.recv().await, Some(Outbound::Raw(s)) if s == "current"));
    }

    #[tokio::test]
    async fn stale_state_keeps_current_status_error_and_connection_clock() {
        let sock = WecomSocket::new();
        generation(&sock, 2).await;
        // Seed observations directly to keep this test independent of the log sink.
        *sock.shared.state.write().await = ConnState::Connected;
        *sock.shared.last_error.write().await = "current-marker".into();
        sock.shared.connected_since.store(1234, Ordering::SeqCst);
        for state in [
            ConnState::Error("stale".into()),
            ConnState::Disconnected,
            ConnState::Connected,
        ] {
            sock.set_state(1, state).await;
        }
        assert_eq!(
            sock.status().await,
            ("connected".into(), "current-marker".into())
        );
        assert_eq!(sock.shared.connected_since.load(Ordering::SeqCst), 1234);
    }

    #[tokio::test]
    async fn stop_reclaims_sender_and_blocks_even_matching_stopped_generation() {
        let sock = WecomSocket::new();
        generation(&sock, 7).await;
        let rx = Arc::new(Outbox::default());
        let tx = Arc::clone(&rx);
        assert!(sock.publish_sender(7, tx).await);
        sock.shared.connected_since.store(1234, Ordering::SeqCst);
        sock.stop().await;
        assert!(rx.recv().await.is_none());
        assert!(sock.out_tx.lock().await.is_none());
        assert!(sock.send_raw("after-stop".into()).await.is_err());
        assert_eq!(sock.status().await.0, "disconnected");
        assert_eq!(sock.detail().await.1, None);
        let rejected_rx = Arc::new(Outbox::default());
        let tx = Arc::clone(&rejected_rx);
        let stopped_generation = sock.shared.gen.load(Ordering::SeqCst);
        assert!(!sock.publish_sender(stopped_generation, tx).await);
        sock.set_state(stopped_generation, ConnState::Connected)
            .await;
        assert!(rejected_rx.recv().await.is_none());
        assert_eq!(sock.status().await.0, "disconnected");
    }

    #[tokio::test]
    async fn barrier_reversed_connection_completion_preserves_new_generation() {
        let sock = Arc::new(WecomSocket::new());
        generation(&sock, 1).await;
        let started = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        let old_rx = Arc::new(Outbox::default());
        let old_tx = Arc::clone(&old_rx);
        let old = {
            let sock = Arc::clone(&sock);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                started.wait().await;
                release.wait().await;
                let published = sock.publish_sender(1, old_tx).await;
                sock.set_state(1, ConnState::Error("late old connection".into()))
                    .await;
                published
            })
        };
        started.wait().await;
        // Retire the pending connection, then complete the replacement first.
        sock.stop().await;
        generation(&sock, 3).await;
        let new_rx = Arc::new(Outbox::default());
        let new_tx = Arc::clone(&new_rx);
        assert!(sock.publish_sender(3, new_tx).await);
        *sock.shared.state.write().await = ConnState::Connected;
        release.wait().await;
        assert!(!old.await.unwrap());
        assert!(old_rx.recv().await.is_none());
        assert_eq!(sock.status().await, ("connected".into(), String::new()));
        for body in ["frame-one", "frame-two"] {
            sock.send_raw(body.into()).await.unwrap();
            assert!(matches!(new_rx.recv().await, Some(Outbound::Raw(s)) if s == body));
        }
    }
}
