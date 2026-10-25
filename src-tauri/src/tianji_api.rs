//! 天玑 OpenAI 兼容端点：`/v1/chat/completions` → 一个完整天玑回合。
//!
//! 链路：微信 ClawBot → OpenClaw（自定义 provider）→ 本端点 → agent_run，
//! 天玑的工具 / 审批 / 会话能力全部保留。同时可供任何 OpenAI 兼容客户端使用。
//!
//! 无状态：请求里的 messages 就是完整上下文（OpenClaw 每轮带全量历史）。
//! 审批沿用桌面端流程（agent.rs 的审批卡 + 5 分钟超时自动拒绝）。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::agent::{
    subscribe_agent_bridge, unsubscribe_agent_bridge, AgentBridgeEvent, AgentRunRequest,
};
use crate::agent_protocol::{Block, Message};
use crate::wecom_orchestrator::WecomAgentConfig;

const DEFAULT_PORT: u16 = 8792;
const MODEL_ID: &str = "tianji";
const TURN_TIMEOUT: Duration = Duration::from_secs(900);
const MAX_IN_FLIGHT: usize = 4;

fn default_port() -> u16 {
    DEFAULT_PORT
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TianjiApiConfig {
    /// Bearer 鉴权；空 = 不鉴权（仅本机监听，风险可控但不建议）
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub agent: Option<WecomAgentConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TianjiApiApplyRequest {
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub agent: Option<WecomAgentConfig>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TianjiApiStatusView {
    pub state: String,
    pub detail: String,
    pub enabled: bool,
    pub port: u16,
    pub base_url: String,
}

struct TianjiApiState {
    cfg: Mutex<Option<TianjiApiConfig>>,
    enabled: AtomicBool,
    server: Mutex<Option<tokio::task::JoinHandle<()>>>,
    status: Mutex<(String, String)>,
    seq: AtomicU64,
    in_flight: AtomicUsize,
}

impl TianjiApiState {
    fn new() -> Self {
        Self {
            cfg: Mutex::new(None),
            enabled: AtomicBool::new(false),
            server: Mutex::new(None),
            status: Mutex::new(("disabled".into(), "未启用".into())),
            seq: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
        }
    }

    async fn set_status(&self, s: &str, detail: &str) {
        *self.status.lock().await = (s.to_string(), detail.to_string());
    }
}

static API: std::sync::OnceLock<Arc<TianjiApiState>> = std::sync::OnceLock::new();

fn shared() -> Arc<TianjiApiState> {
    API.get_or_init(|| Arc::new(TianjiApiState::new())).clone()
}

#[tauri::command]
pub async fn tianji_api_status() -> Result<TianjiApiStatusView, String> {
    status_of(&shared()).await
}

async fn status_of(state: &Arc<TianjiApiState>) -> Result<TianjiApiStatusView, String> {
    let (s, detail) = state.status.lock().await.clone();
    let port = state
        .cfg
        .lock()
        .await
        .as_ref()
        .map(|c| c.port)
        .unwrap_or_else(default_port);
    Ok(TianjiApiStatusView {
        state: s,
        detail,
        enabled: state.enabled.load(Ordering::SeqCst),
        port,
        base_url: format!("http://127.0.0.1:{port}/v1"),
    })
}

#[tauri::command]
pub async fn tianji_api_apply(
    app: AppHandle,
    req: TianjiApiApplyRequest,
) -> Result<TianjiApiStatusView, String> {
    let state = shared();
    if let Some(handle) = state.server.lock().await.take() {
        handle.abort();
    }

    if !req.enabled {
        state.enabled.store(false, Ordering::SeqCst);
        *state.cfg.lock().await = None;
        state.set_status("disabled", "未启用").await;
        return status_of(&state).await;
    }

    // 前端可能传来 enc:v1: 密文（保存过的），解回明文做 Bearer 比对
    let api_key = if req.api_key.trim().is_empty() {
        String::new()
    } else {
        crate::secret_protect::unprotect_secret(req.api_key.trim())?
    };

    let cfg = TianjiApiConfig {
        api_key,
        port: req.port,
        agent: req.agent,
    };

    let addr = format!("127.0.0.1:{}", cfg.port);
    // 绑定失败重试几次：热重载/重启时旧监听刚释放会有短暂竞态
    let mut listener = None;
    let mut last_err = String::new();
    for _ in 0..5 {
        match TcpListener::bind(&addr).await {
            Ok(l) => {
                listener = Some(l);
                break;
            }
            Err(e) => {
                last_err = format!("{e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    let Some(listener) = listener else {
        state
            .set_status("error", &format!("端口监听失败：{last_err}"))
            .await;
        return Err(format!(
            "端口 {addr} 监听失败：{last_err}（被占用就换一个端口，或稍后点「重启服务」重试）"
        ));
    };

    state.enabled.store(true, Ordering::SeqCst);
    *state.cfg.lock().await = Some(cfg);

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let app2 = app.clone();
                    tokio::spawn(async move {
                        let _ = handle_conn(app2, stream).await;
                    });
                }
                Err(e) => {
                    eprintln!("[tianji-api] accept 失败：{e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });
    *state.server.lock().await = Some(handle);
    state
        .set_status("listening", &format!("OpenAI 兼容端点已监听 {addr}"))
        .await;
    status_of(&state).await
}

// ---------- HTTP 层（迷你实现，同企微/公众号通道风格） ----------

struct Request {
    method: String,
    path: String,
    authorization: String,
    body: String,
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end;
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            header_end = pos;
            break;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
    }
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    let mut content_length = 0usize;
    let mut authorization = String::new();
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = lower.strip_prefix("authorization:") {
            authorization = line[line.len() - v.len()..].trim().to_string();
        }
    }
    if content_length > 8 * 1024 * 1024 {
        return None;
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Request {
        method,
        path,
        authorization,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

async fn handle_conn(app: AppHandle, mut stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let Some(req) = read_request(&mut stream).await else {
        return;
    };
    let state = shared();
    let cfg = state.cfg.lock().await.clone();
    let Some(cfg) = cfg else {
        let _ = respond_json(&mut stream, 503, &err_json("端点未启用")).await;
        return;
    };

    // Bearer 鉴权（配置了 key 才校验）
    if !cfg.api_key.is_empty() {
        let presented = req
            .authorization
            .strip_prefix("Bearer ")
            .unwrap_or(&req.authorization)
            .trim();
        if presented != cfg.api_key {
            let _ = respond_json(&mut stream, 401, &err_json("鉴权失败：api_key 不匹配")).await;
            return;
        }
    }

    // 宽容路由：按路径后缀匹配。客户端常把 base_url 配成带 /chat/completions 的
    // 完整地址（此时刷新模型列表会打到 .../chat/completions/models），后缀匹配可兜住。
    let path = req.path.trim_end_matches('/');
    if req.method == "GET" && path.ends_with("/models") {
        let body = serde_json::json!({
            "object": "list",
            "data": [{ "id": MODEL_ID, "object": "model", "owned_by": "ai-workbench" }]
        });
        let _ = respond_json(&mut stream, 200, &body).await;
    } else if req.method == "POST" && path.ends_with("/chat/completions") {
        handle_chat(&state, &app, &mut stream, &req.body).await;
    } else if req.method == "GET" && (path == "/v1" || path == "" ) {
        let _ = respond_json(
            &mut stream,
            200,
            &serde_json::json!({
                "status": "ok",
                "base_url": format!("http://127.0.0.1:{}/v1", cfg.port),
                "model": MODEL_ID
            }),
        )
        .await;
    } else {
        let hint = format!(
            "not found: {} {} —— base_url 应为 http://127.0.0.1:{}/v1（不要带 /chat/completions）",
            req.method, req.path, cfg.port
        );
        let _ = respond_json(&mut stream, 404, &err_json(&hint)).await;
    }
}

// ---------- OpenAI 请求映射 ----------

#[derive(Debug, Deserialize)]
struct OaiMessage {
    role: String,
    #[serde(default)]
    content: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OaiRequest {
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    messages: Vec<OaiMessage>,
}

fn err_json(msg: &str) -> serde_json::Value {
    serde_json::json!({ "error": { "message": msg, "type": "tianji_api_error" } })
}

/// 把 OpenAI messages 映射为 (天玑 system 附加段, 历史消息)。
/// 返回 Err = 没有可用的用户消息。
async fn map_messages(messages: &[OaiMessage]) -> Result<(String, Vec<Message>), String> {
    let mut client_system = String::new();
    let mut out: Vec<Message> = Vec::new();
    for m in messages {
        match m.role.as_str() {
            "system" | "developer" => {
                if let Some(s) = m.content.as_str() {
                    if !s.trim().is_empty() {
                        if !client_system.is_empty() {
                            client_system.push('\n');
                        }
                        client_system.push_str(s.trim());
                    }
                }
            }
            "user" => {
                let mut blocks: Vec<Block> = Vec::new();
                match &m.content {
                    serde_json::Value::String(s) => {
                        if !s.trim().is_empty() {
                            blocks.push(Block::Text { text: s.clone() });
                        }
                    }
                    serde_json::Value::Array(parts) => {
                        for part in parts {
                            let ptype = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match ptype {
                                "text" => {
                                    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                        if !t.trim().is_empty() {
                                            blocks.push(Block::Text { text: t.to_string() });
                                        }
                                    }
                                }
                                "image_url" => {
                                    let url = part
                                        .get("image_url")
                                        .and_then(|i| i.get("url"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    if url.starts_with("data:") {
                                        // data:<mime>;base64,<data>
                                        if let Some((mime, data)) = parse_data_url(url) {
                                            blocks.push(Block::Image { media_type: mime, data });
                                        }
                                    } else if url.starts_with("http://")
                                        || url.starts_with("https://")
                                    {
                                        match fetch_base64(url).await {
                                            Ok((mime, data)) => {
                                                blocks.push(Block::Image { media_type: mime, data })
                                            }
                                            Err(e) => blocks.push(Block::Text {
                                                text: format!("[图片获取失败：{e}]"),
                                            }),
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                if blocks.is_empty() {
                    blocks.push(Block::Text {
                        text: "（空消息）".into(),
                    });
                }
                out.push(Message {
                    role: "user".into(),
                    blocks,
                    meta: None,
                });
            }
            "assistant" => {
                if let Some(s) = m.content.as_str() {
                    if !s.trim().is_empty() {
                        out.push(Message::text("assistant", s));
                    }
                }
            }
            "tool" => {
                let s = m.content.as_str().unwrap_or("");
                if !s.trim().is_empty() {
                    out.push(Message::text("user", &format!("[工具结果]\n{s}")));
                }
            }
            _ => {}
        }
    }
    let last_is_user = out.last().map(|m| m.role == "user").unwrap_or(false);
    if !last_is_user {
        return Err("最后一条消息不是用户消息，无法发起回合".into());
    }
    Ok((client_system, out))
}

async fn fetch_base64(url: &str) -> Result<(String, String), String> {
    use base64::Engine;
    let resp = reqwest::get(url)
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

fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (mime_part, data) = rest.split_once(',')?;
    // 只支持 base64 数据（OpenClaw 转发的图片都是 base64 data URL）
    let mime = mime_part.strip_suffix(";base64")?;
    if mime.is_empty() || data.is_empty() {
        return None;
    }
    Some((mime.to_string(), data.to_string()))
}

/// 跑一个完整天玑回合（非流式），返回最终文本。
async fn run_turn_collect(
    state: &Arc<TianjiApiState>,
    app: &AppHandle,
    client_system: &str,
    msgs: Vec<Message>,
) -> Result<String, String> {
    let cfg = state.cfg.lock().await.clone();
    let Some(cfg) = cfg else {
        return Err("端点未启用".into());
    };
    let Some(agent) = cfg.agent else {
        return Err("未绑定模型：请在设置里保存一次天玑配置".into());
    };
    let seq = state.seq.fetch_add(1, Ordering::SeqCst);
    let request_id = format!("tianji-api-{seq}-{}", chrono::Utc::now().timestamp_millis());
    let req = build_agent_request(&agent, &request_id, client_system, msgs);
    let mut rx = subscribe_agent_bridge(&request_id);
    let run = tokio::spawn(crate::agent::agent_run(app.clone(), req));
    let mut acc = String::new();
    let result = loop {
        match tokio::time::timeout(TURN_TIMEOUT, rx.recv()).await {
            Err(_) => {
                crate::cancellation::cancel_request(&request_id);
                break if acc.is_empty() {
                    Err("响应超时".into())
                } else {
                    Ok(acc)
                };
            }
            Ok(None) => {
                break if acc.is_empty() {
                    Err("回合异常结束".into())
                } else {
                    Ok(acc)
                };
            }
            Ok(Some(AgentBridgeEvent::Chunk(t))) => acc.push_str(&t),
            Ok(Some(AgentBridgeEvent::Done { .. })) => {
                break if acc.is_empty() {
                    Ok("（无文本回复）".into())
                } else {
                    Ok(acc)
                };
            }
            Ok(Some(AgentBridgeEvent::Err(e))) => break Err(e),
            // API 场景没有聊天回复对象；审批等桌面端卡片
            Ok(Some(AgentBridgeEvent::ApprovalNeeded { .. })) => {}
        }
    };
    unsubscribe_agent_bridge(&request_id);
    let _ = run.await;
    result
}

fn build_agent_request(
    agent: &WecomAgentConfig,
    request_id: &str,
    client_system: &str,
    msgs: Vec<Message>,
) -> AgentRunRequest {
    let mut permission = agent.permission.clone();
    if permission.trim().is_empty() {
        permission = "read".into();
    }
    let mut system = "你是 AI Workbench 的天玑助手。回复简洁、适合在聊天工具里阅读；\
         少用大段代码围栏，勿输出本机绝对路径。"
        .to_string();
    if !client_system.trim().is_empty() {
        system.push_str("\n调用方附加指令：\n");
        system.push_str(client_system.trim());
    }
    AgentRunRequest {
        config: agent.config.clone(),
        fallback_config: agent.fallback_config.clone(),
        system,
        user: String::new(),
        request_id: request_id.to_string(),
        repo_path: agent.repo_path.clone(),
        services: agent.services.clone(),
        history: Vec::new(),
        messages: msgs,
        mode: agent.mode.clone(),
        apps: Vec::new(),
        permission,
        tools: agent.tools,
        vision: agent.vision,
        diff_confirm: agent.diff_confirm,
        always_allow: agent.always_allow.clone(),
        web_allow: agent.web_allow.clone(),
        subagents: agent.subagents,
        mcp: agent.mcp.clone(),
        parallel_reads: agent.parallel_reads,
        retries: agent.retries,
        grep_ignore: agent.grep_ignore,
        plan_first: agent.plan_first,
        context_tokens: agent.context_tokens,
        result_cap: agent.result_cap,
        anthropic_stream: agent.anthropic_stream,
        mcp_enabled: agent.mcp_enabled,
    }
}

fn rough_tokens(s: &str) -> u64 {
    // 中文按 ~1.6 字符/token，混排取 chars/2 的粗估，仅作台账用
    (s.chars().count() as u64 / 2).max(1)
}

async fn handle_chat(
    state: &Arc<TianjiApiState>,
    app: &AppHandle,
    stream: &mut TcpStream,
    body: &str,
) {
    if state.in_flight.load(Ordering::SeqCst) >= MAX_IN_FLIGHT {
        let _ = respond_json(stream, 429, &err_json("并发已达上限，请稍后重试")).await;
        return;
    }
    let parsed: OaiRequest = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let _ = respond_json(stream, 400, &err_json(&format!("请求体非法：{e}"))).await;
            return;
        }
    };
    let (client_system, msgs) = match map_messages(&parsed.messages).await {
        Ok(v) => v,
        Err(e) => {
            let _ = respond_json(stream, 400, &err_json(&e)).await;
            return;
        }
    };

    state.in_flight.fetch_add(1, Ordering::SeqCst);
    let result = if parsed.stream {
        handle_stream(state, app, stream, client_system, msgs).await
    } else {
        match run_turn_collect(state, app, &client_system, msgs).await {
            Ok(text) => {
                let body = serde_json::json!({
                    "id": format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis()),
                    "object": "chat.completion",
                    "created": chrono::Utc::now().timestamp(),
                    "model": MODEL_ID,
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": text },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": rough_tokens(&client_system),
                        "completion_tokens": rough_tokens(&text),
                        "total_tokens": rough_tokens(&client_system) + rough_tokens(&text)
                    }
                });
                let _ = respond_json(stream, 200, &body).await;
                Ok(())
            }
            Err(e) => {
                let _ = respond_json(stream, 500, &err_json(&e)).await;
                Err(e)
            }
        }
    };
    state.in_flight.fetch_sub(1, Ordering::SeqCst);
    if let Err(e) = result {
        eprintln!("[tianji-api] 回合失败：{e}");
    }
}

/// 流式：桥接 Chunk → SSE delta（桥接已过滤 <think>）。
async fn handle_stream(
    state: &Arc<TianjiApiState>,
    app: &AppHandle,
    stream: &mut TcpStream,
    client_system: String,
    msgs: Vec<Message>,
) -> Result<(), String> {
    let cfg = state.cfg.lock().await.clone();
    let Some(cfg) = cfg else {
        return Err("端点未启用".into());
    };
    let Some(agent) = cfg.agent else {
        let _ = respond_json(stream, 500, &err_json("未绑定模型：请在设置里保存一次天玑配置")).await;
        return Err("未绑定模型".into());
    };
    let seq = state.seq.fetch_add(1, Ordering::SeqCst);
    let request_id = format!("tianji-api-{seq}-{}", chrono::Utc::now().timestamp_millis());
    let req = build_agent_request(&agent, &request_id, &client_system, msgs);
    let mut rx = subscribe_agent_bridge(&request_id);
    let run = tokio::spawn(crate::agent::agent_run(app.clone(), req));

    let chunk_id = format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis());
    let sse = |delta: serde_json::Value, finish: Option<&str>| -> String {
        let mut choice = serde_json::json!({ "index": 0, "delta": delta, "finish_reason": finish });
        if finish.is_some() {
            choice["delta"] = serde_json::json!({});
        }
        format!(
            "data: {}\n\n",
            serde_json::json!({
                "id": chunk_id,
                "object": "chat.completion.chunk",
                "created": chrono::Utc::now().timestamp(),
                "model": MODEL_ID,
                "choices": [choice]
            })
        )
    };

    // SSE 响应头
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).await.is_err() {
        unsubscribe_agent_bridge(&request_id);
        let _ = run.await;
        return Err("客户端已断开".into());
    }
    let _ = stream
        .write_all(sse(serde_json::json!({"role": "assistant"}), None).as_bytes())
        .await;
    let _ = stream.flush().await;

    let mut acc = String::new();
    let result: Result<String, String> = loop {
        match tokio::time::timeout(TURN_TIMEOUT, rx.recv()).await {
            Err(_) => {
                crate::cancellation::cancel_request(&request_id);
                break Err("响应超时".into());
            }
            Ok(None) => {
                break if acc.is_empty() {
                    Err("回合异常结束".into())
                } else {
                    Ok(acc)
                };
            }
            Ok(Some(AgentBridgeEvent::Chunk(t))) => {
                acc.push_str(&t);
                if stream
                    .write_all(sse(serde_json::json!({"content": t}), None).as_bytes())
                    .await
                    .is_err()
                {
                    crate::cancellation::cancel_request(&request_id);
                    break Err("客户端已断开".into());
                }
                let _ = stream.flush().await;
            }
            Ok(Some(AgentBridgeEvent::Done { .. })) => {
                let text = if acc.is_empty() {
                    "（无文本回复）".to_string()
                } else {
                    acc.clone()
                };
                break Ok(text);
            }
            Ok(Some(AgentBridgeEvent::Err(e))) => break Err(e),
            Ok(Some(AgentBridgeEvent::ApprovalNeeded { .. })) => {}
        }
    };
    unsubscribe_agent_bridge(&request_id);
    let _ = run.await;

    match result {
        Ok(_) => {
            let _ = stream
                .write_all(sse(serde_json::json!({}), Some("stop")).as_bytes())
                .await;
            let _ = stream.write_all(b"data: [DONE]\n\n").await;
            let _ = stream.flush().await;
            Ok(())
        }
        Err(e) => {
            // 已开始流式就无法改状态码：把错误作为文本收尾
            let _ = stream
                .write_all(sse(serde_json::json!({"content": format!("\n\n（出错：{e}）")}), None).as_bytes())
                .await;
            let _ = stream
                .write_all(sse(serde_json::json!({}), Some("stop")).as_bytes())
                .await;
            let _ = stream.write_all(b"data: [DONE]\n\n").await;
            let _ = stream.flush().await;
            Err(e)
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn respond_json(stream: &mut TcpStream, code: u16, body: &serde_json::Value) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    };
    let text = body.to_string();
    let resp = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_parses() {
        let (mime, data) = parse_data_url("data:image/png;base64,AAAA").unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(data, "AAAA");
        assert!(parse_data_url("data:image/png,raw").is_none());
    }

    #[tokio::test]
    async fn maps_plain_messages() {
        let msgs = vec![
            OaiMessage { role: "system".into(), content: serde_json::json!("be nice") },
            OaiMessage { role: "user".into(), content: serde_json::json!("hi") },
        ];
        let (sys, out) = map_messages(&msgs).await.unwrap();
        assert_eq!(sys, "be nice");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "user");
    }

    #[tokio::test]
    async fn rejects_empty_tail() {
        let msgs = vec![OaiMessage {
            role: "assistant".into(),
            content: serde_json::json!("ok"),
        }];
        assert!(map_messages(&msgs).await.is_err());
    }
}
