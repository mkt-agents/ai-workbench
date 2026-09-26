//! In-process OpenAI-compatible reverse proxy over the CodeBuddy account pool.
//!
//! It runs inside the Tauri process rather than as a child binary because the
//! pool, the keys and the log are all SQLite state this process already owns.
//! Binding defaults to 127.0.0.1; LAN mode is an explicit opt-in. Every request
//! passes the same gate order: peer address -> gateway key -> account pool.
//!
//! What "upstream" means is decided entirely by `wb_settings.adapter_json`, so
//! a protocol change is a settings edit, not a rebuild.

mod aggregate;
pub mod ip_filter;
pub mod pool;
mod sse;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::BoxStream;
use futures_util::{Stream, StreamExt};
use rusqlite::{params, Connection};
use serde::Serialize;
use tokio::sync::oneshot;

use crate::codebuddy::adapter::{self, RenderContext, UpstreamProfile};
use crate::codebuddy::credential::{self, CredentialKind, ParsedCredential};
use crate::wb_commands::{self, WbSettingsPatch};
use crate::wb_keys;
use crate::wb_logs::{self, LogEntryInput};
use crate::DbState;

pub use ip_filter::{IpRuleDto, NewIpRule};

use sse::{usage_from_body, Usage, UsageScan};

/// Chat payloads with attached images get large; this is the only ceiling
/// between one client request and an out-of-memory.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// Distinct accounts a single client request may burn through before the
/// failure is reported back.
const MAX_ATTEMPTS: usize = 3;

#[derive(Clone)]
struct Ctx {
    conn: Arc<Mutex<Connection>>,
    pool: Arc<Mutex<pool::Pool>>,
    lan_enabled: bool,
}

struct Running {
    shutdown: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<()>,
    bind: SocketAddr,
    started_at: i64,
}

/// Managed Tauri state; `None` inside the lock means not listening.
#[derive(Default)]
pub struct WbGatewayState {
    running: Mutex<Option<Running>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub bind_addr: Option<String>,
    pub port: i64,
    pub lan_enabled: bool,
    pub started_at: Option<i64>,
    pub key_count: i64,
    pub account_total: i64,
    pub account_eligible: i64,
}

// ---------------------------------------------------------------------------
// Shared DB reads
// ---------------------------------------------------------------------------

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn load_profile(conn: &Connection) -> Option<UpstreamProfile> {
    wb_commands::get_settings(conn)
        .ok()
        .and_then(|s| s.adapter_json)
        .filter(|json| !json.trim().is_empty())
        .and_then(|json| UpstreamProfile::from_json(&json).ok())
}

fn load_ip_rules(conn: &Connection) -> (Vec<ip_filter::RuleTarget>, Vec<ip_filter::RuleTarget>) {
    let mut rows: Vec<(String, bool)> = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT ip_or_cidr, kind FROM wb_ip_rules WHERE enabled = 1") {
        if let Ok(iter) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)? == "allow"))
        }) {
            rows = iter.flatten().collect();
        }
    }
    ip_filter::compile_rules(&rows)
}

/// Routing-eligible accounts. The stored `status` column lags reality (it is
/// only written by probe/check-in), so expiry is recomputed here — that is what
/// makes a token that aged out overnight stop receiving traffic immediately.
fn eligible_accounts(conn: &Connection, now_unix: i64) -> Vec<pool::PoolAccount> {
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, status, enabled, credential_type, credential FROM codebuddy_accounts ORDER BY id",
    ) else {
        return out;
    };
    let Ok(iter) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? != 0,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    }) else {
        return out;
    };
    for (id, status, enabled, kind, raw) in iter.flatten() {
        let parsed = ParsedCredential::parse(CredentialKind::parse(&kind), &raw);
        out.push(pool::PoolAccount {
            id,
            status,
            enabled,
            exp_unix: parsed.exp_unix,
        });
    }
    out.into_iter().filter(|a| a.eligible_at(now_unix)).collect()
}

fn credential_of(conn: &Connection, id: i64) -> Option<ParsedCredential> {
    conn.query_row(
        "SELECT credential_type, credential FROM codebuddy_accounts WHERE id = ?1",
        params![id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .ok()
    .map(|(kind, raw)| ParsedCredential::parse(CredentialKind::parse(&kind), &raw))
}

fn mark_expired(conn: &Connection, id: i64) {
    let _ = conn.execute(
        "UPDATE codebuddy_accounts SET status = 'expired', updated_at = ?2 WHERE id = ?1",
        params![id, credential::now_unix() * 1000],
    );
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(10))
        // No gzip/brotli/deflate features are enabled on reqwest, so the
        // upstream body reaches us exactly as the upstream sent it — which is
        // what SSE forwarding requires. Adding one would break framing here.
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(|e| e.to_string())
}

fn send_upstream(
    client: &reqwest::Client,
    request: &adapter::OutboundRequest,
    body: Bytes,
) -> Result<reqwest::RequestBuilder, String> {
    let method = request
        .method
        .parse::<reqwest::Method>()
        .map_err(|_| format!("非法 HTTP 方法: {}", request.method))?;
    let mut req = client.request(method, &request.url);
    for (k, v) in &request.headers {
        // Host and Content-Length belong to the hop, not to the template.
        if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) else {
            return Err(format!("协议配置里的 header 非法: {k}"));
        };
        req = req.header(name, value);
    }
    Ok(req.body(body))
}

// ---------------------------------------------------------------------------
// Gate + auth
// ---------------------------------------------------------------------------

fn openai_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": { "message": message, "type": "invalid_request_error", "code": status.as_u16() }
        })),
    )
        .into_response()
}

/// Address gate. Rejections are logged too: an unexplained 403 is the worst
/// thing a half-configured allowlist can produce.
fn gate(ctx: &Ctx, peer: SocketAddr) -> Result<(), Response> {
    let Ok(guard) = ctx.conn.lock() else {
        return Err(openai_error(StatusCode::INTERNAL_SERVER_ERROR, "内部状态不可用"));
    };
    let ip = peer.ip();
    if ip.is_loopback() {
        return Ok(());
    }
    let (allow, deny) = load_ip_rules(&guard);
    if ip_filter::ip_allowed(ip, ctx.lan_enabled, &allow, &deny) {
        return Ok(());
    }
    let _ = wb_logs::insert_log(
        &guard,
        &LogEntryInput {
            ts: now_millis(),
            key_id: None,
            account_id: None,
            model: None,
            stream: false,
            status_code: Some(StatusCode::FORBIDDEN.as_u16() as i64),
            prompt_tokens: None,
            completion_tokens: None,
            latency_ms: None,
            error: Some(format!("来源地址被拒绝: {ip}")),
        },
    );
    Err(openai_error(
        StatusCode::FORBIDDEN,
        &format!("来源地址不在允许范围: {ip}"),
    ))
}

/// Bearer token -> gateway key id, bumping the key's usage counters.
fn authorize(ctx: &Ctx, headers: &HeaderMap) -> Result<i64, Response> {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .unwrap_or("");
    if presented.is_empty() {
        return Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "缺少 Authorization: Bearer <wk-… 网关密钥>",
        ));
    }
    let Ok(guard) = ctx.conn.lock() else {
        return Err(openai_error(StatusCode::INTERNAL_SERVER_ERROR, "内部状态不可用"));
    };
    wb_keys::authenticate(&guard, presented, now_millis())
        .ok_or_else(|| openai_error(StatusCode::UNAUTHORIZED, "网关密钥无效或已禁用"))
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

async fn healthz(State(ctx): State<Ctx>, ConnectInfo(peer): ConnectInfo<SocketAddr>) -> Response {
    match gate(&ctx, peer) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "service": "ai-workbench-workbuddy" })),
        )
            .into_response(),
        Err(res) => res,
    }
}

async fn list_models(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err(res) = gate(&ctx, peer) {
        return res;
    }
    if let Err(res) = authorize(&ctx, &headers) {
        return res;
    }
    let Some(profile) = ctx.conn.lock().ok().and_then(|g| load_profile(&g)) else {
        return openai_error(StatusCode::SERVICE_UNAVAILABLE, "未配置上游协议");
    };

    // Prefer the real listing; a missing or failing endpoint is not an error,
    // the static fallback exists for exactly that case.
    if let Some(ep) = profile.models.clone().filter(|ep| !ep.url.trim().is_empty()) {
        if let Some(json) = fetch_models(&ctx, &ep).await {
            return Json(normalize_models(json, &profile.fallback_models)).into_response();
        }
    }
    Json(normalize_models(serde_json::Value::Null, &profile.fallback_models)).into_response()
}

async fn fetch_models(ctx: &Ctx, ep: &adapter::Endpoint) -> Option<serde_json::Value> {
    let now = credential::now_unix();
    let request = {
        let guard = ctx.conn.lock().ok()?;
        let accounts = eligible_accounts(&guard, now);
        let chosen = {
            let mut p = ctx.pool.lock().ok()?;
            p.pick(&accounts, &pool::StickKey { key_id: -1, model: "__models__".into() }, now)
        }?;
        let parsed = credential_of(&guard, chosen)?;
        let cx = RenderContext { credential: &parsed.raw, token: &parsed.token, cookie: &parsed.cookie, model: "" };
        adapter::build_request(ep, &cx)
    }?;
    let client = http_client().ok()?;
    let builder = send_upstream(&client, &request, Bytes::new()).ok()?;
    let resp = builder.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<serde_json::Value>().await.ok()
}

/// OpenAI clients expect `{object:"list",data:[{id,object:"model"}]}`. The
/// upstream may already do that, return a bare array of names, or be absent.
fn normalize_models(json: serde_json::Value, fallback: &[String]) -> serde_json::Value {
    let array = json
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .or_else(|| json.get("models").and_then(|v| v.as_array()).cloned())
        .or_else(|| json.as_array().cloned())
        .unwrap_or_default();

    let mut names: Vec<String> = array
        .into_iter()
        .filter_map(|item| match item {
            serde_json::Value::String(s) => Some(s),
            obj => obj
                .get("id")
                .or_else(|| obj.get("model"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
        .collect();
    if names.is_empty() {
        names = fallback.to_vec();
    }
    serde_json::json!({
        "object": "list",
        "data": names.into_iter().map(|id| serde_json::json!({
            "id": id, "object": "model", "owned_by": "codebuddy"
        })).collect::<Vec<_>>()
    })
}

async fn chat_completions(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let started = Instant::now();
    if let Err(res) = gate(&ctx, peer) {
        return res;
    }
    let key_id = match authorize(&ctx, &headers) {
        Ok(id) => id,
        Err(res) => {
            log_one(&ctx, LogEntryInput {
                ts: now_millis(),
                key_id: None,
                account_id: None,
                model: None,
                stream: false,
                status_code: Some(res.status().as_u16() as i64),
                prompt_tokens: None,
                completion_tokens: None,
                latency_ms: Some(started.elapsed().as_millis() as i64),
                error: Some("网关密钥校验失败".to_string()),
            });
            return res;
        }
    };

    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return openai_error(StatusCode::BAD_REQUEST, "请求体不是合法 JSON");
    };
    let Some(model) = parsed.get("model").and_then(|v| v.as_str()).filter(|m| !m.trim().is_empty()).map(str::to_string) else {
        return openai_error(StatusCode::BAD_REQUEST, "缺少 model 字段");
    };
    let stream = parsed.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // The upstream chat endpoint is stream-only, so a client that asked for a
    // plain JSON response still gets one — we force `stream:true` upstream and
    // re-assemble the frames (see aggregate_and_respond). This is the only
    // place the "SSE is observed, never rewritten" rule is deliberately broken.
    let body = if stream {
        body
    } else {
        match aggregate::force_stream_body(&parsed) {
            Ok(b) => Bytes::from(b),
            Err(e) => return openai_error(StatusCode::BAD_REQUEST, &e),
        }
    };

    let chat_ep = match ctx.conn.lock().ok().and_then(|g| load_profile(&g)) {
        None => {
            return unavailable(&ctx, key_id, &model, stream, started, "未配置上游协议（wb_settings.adapter_json）")
        }
        Some(profile) => match profile.chat.filter(|ep| !ep.url.trim().is_empty()) {
            Some(ep) => ep,
            None => return unavailable(&ctx, key_id, &model, stream, started, "协议缺少 chat 端点"),
        },
    };
    let Ok(client) = http_client() else {
        return openai_error(StatusCode::INTERNAL_SERVER_ERROR, "HTTP 客户端初始化失败");
    };

    let mut tried: HashSet<i64> = HashSet::new();
    let mut last_error = String::from("账号池没有可用账号");

    for _ in 0..MAX_ATTEMPTS {
        let Some((account_id, request)) = select_account(&ctx, &chat_ep, &model, &tried) else {
            break;
        };
        tried.insert(account_id);
        let builder = match send_upstream(&client, &request, body.clone()) {
            Ok(b) => b,
            Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
        };
        match builder.send().await {
            Err(e) => {
                last_error = e.to_string();
                if let Ok(mut p) = ctx.pool.lock() {
                    p.mark_unhealthy(account_id, pool::Failure::Transient, credential::now_unix());
                }
                continue;
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                if let Some(failure) = pool::failure_status_code(status) {
                    last_error = format!("上游返回 {status}");
                    let now = credential::now_unix();
                    if let Ok(mut p) = ctx.pool.lock() {
                        p.mark_unhealthy(account_id, failure, now);
                    }
                    if failure == pool::Failure::Auth {
                        if let Ok(guard) = ctx.conn.lock() {
                            mark_expired(&guard, account_id);
                        }
                    }
                    continue;
                }
                return if stream {
                    relay(ctx, resp, key_id, account_id, model, stream, started).await
                } else if resp.status().is_success() {
                    aggregate_and_respond(ctx, resp, key_id, account_id, model, started).await
                } else {
                    // Non-2xx (e.g. a 400 the pool does not treat as an account
                    // failure) passes through untouched via relay's JSON branch.
                    relay(ctx, resp, key_id, account_id, model, false, started).await
                };
            }
        }
    }

    unavailable(&ctx, key_id, &model, stream, started, &last_error)
}

fn unavailable(
    ctx: &Ctx,
    key_id: i64,
    model: &str,
    stream: bool,
    started: Instant,
    reason: &str,
) -> Response {
    log_one(
        ctx,
        LogEntryInput {
            ts: now_millis(),
            key_id: Some(key_id),
            account_id: None,
            model: Some(model.to_string()),
            stream,
            status_code: Some(StatusCode::SERVICE_UNAVAILABLE.as_u16() as i64),
            prompt_tokens: None,
            completion_tokens: None,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            error: Some(reason.to_string()),
        },
    );
    openai_error(
        StatusCode::SERVICE_UNAVAILABLE,
        &format!("账号池无可用账号: {reason}"),
    )
}

/// Choose an account and render its upstream request under one lock
/// acquisition — the two must agree, or one account's credential could be sent
/// under another account's log id.
fn select_account(
    ctx: &Ctx,
    chat_ep: &adapter::Endpoint,
    model: &str,
    tried: &HashSet<i64>,
) -> Option<(i64, adapter::OutboundRequest)> {
    let now = credential::now_unix();
    let guard = ctx.conn.lock().ok()?;
    let accounts = eligible_accounts(&guard, now);
    let chosen = {
        let mut p = ctx.pool.lock().ok()?;
        if tried.is_empty() {
            // First hop honours stickiness (prompt caching); retries walk the rest.
            let stick = pool::StickKey { key_id: 0, model: model.to_string() };
            p.pick(&accounts, &stick, now)
        } else {
            p.pick_next(&accounts, now, tried)
        }
    }?;
    let parsed = credential_of(&guard, chosen)?;
    let cx = RenderContext {
        credential: &parsed.raw,
        token: &parsed.token,
        cookie: &parsed.cookie,
        model,
    };
    adapter::build_request(chat_ep, &cx).map(|req| (chosen, req))
}

/// Return the upstream response to the client, logging once the body is done —
/// including when the client hangs up mid-stream.
async fn relay(
    ctx: Ctx,
    resp: reqwest::Response,
    key_id: i64,
    account_id: i64,
    model: String,
    stream: bool,
    started: Instant,
) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let is_sse = content_type.contains("text/event-stream") || stream;

    if !is_sse {
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                log_one(
                    &ctx,
                    LogEntryInput {
                        ts: now_millis(),
                        key_id: Some(key_id),
                        account_id: Some(account_id),
                        model: Some(model),
                        stream: false,
                        status_code: Some(status.as_u16() as i64),
                        prompt_tokens: None,
                        completion_tokens: None,
                        latency_ms: Some(started.elapsed().as_millis() as i64),
                        error: Some(format!("读取上游响应失败: {e}")),
                    },
                );
                return openai_error(StatusCode::BAD_GATEWAY, "读取上游响应失败");
            }
        };
        let usage = usage_from_body(&bytes);
        log_one(
            &ctx,
            LogEntryInput {
                ts: now_millis(),
                key_id: Some(key_id),
                account_id: Some(account_id),
                model: Some(model),
                stream: false,
                status_code: Some(status.as_u16() as i64),
                prompt_tokens: usage.as_ref().and_then(|u| u.prompt_tokens),
                completion_tokens: usage.as_ref().and_then(|u| u.completion_tokens),
                latency_ms: Some(started.elapsed().as_millis() as i64),
                error: None,
            },
        );
        if let Ok(mut p) = ctx.pool.lock() {
            p.mark_healthy(account_id);
        }
        return (status, [(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response();
    }

    let upstream: BoxStream<'static, Result<Bytes, reqwest::Error>> = resp.bytes_stream().boxed();
    let pump = StreamPump {
        upstream: Some(upstream),
        scan: UsageScan::default(),
        conn: Arc::clone(&ctx.conn),
        pool: Arc::clone(&ctx.pool),
        record: Some(LogRecord {
            key_id,
            account_id,
            model,
            status_code: status.as_u16(),
            started,
        }),
    };
    (
        status,
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (axum::http::header::CACHE_CONTROL, "no-cache".to_string()),
            (axum::http::header::CONNECTION, "keep-alive".to_string()),
        ],
        Body::from_stream(pump),
    )
        .into_response()
}

struct LogRecord {
    key_id: i64,
    account_id: i64,
    model: String,
    status_code: u16,
    started: Instant,
}

/// Client asked for `stream:false`, so the forced-stream upstream response is
/// consumed here and re-assembled into a single JSON completion instead of
/// being forwarded.
async fn aggregate_and_respond(
    ctx: Ctx,
    resp: reqwest::Response,
    key_id: i64,
    account_id: i64,
    model: String,
    started: Instant,
) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut agg = aggregate::SseAggregator::default();
    let mut upstream = resp.bytes_stream();
    let idle = std::time::Duration::from_secs(120);
    let mut stream_error: Option<String> = None;

    loop {
        match tokio::time::timeout(idle, upstream.next()).await {
            Err(_) => {
                stream_error = Some("上游流空闲超时".to_string());
                break;
            }
            Ok(None) => break,
            Ok(Some(Err(e))) => {
                stream_error = Some(format!("上游流中断: {e}"));
                break;
            }
            Ok(Some(Ok(bytes))) => agg.feed(&bytes),
        }
    }

    if let Some(error) = stream_error {
        log_one(
            &ctx,
            LogEntryInput {
                ts: now_millis(),
                key_id: Some(key_id),
                account_id: Some(account_id),
                model: Some(model),
                stream: false,
                status_code: Some(status.as_u16() as i64),
                prompt_tokens: None,
                completion_tokens: None,
                latency_ms: Some(started.elapsed().as_millis() as i64),
                error: Some(error.clone()),
            },
        );
        return openai_error(StatusCode::BAD_GATEWAY, &error);
    }

    match agg.into_completion(&model, now_millis() / 1000) {
        Err(e) => {
            let message = match e {
                aggregate::AggError::Overflow => "响应超出聚合上限".to_string(),
                aggregate::AggError::Empty => "上游流没有返回任何内容".to_string(),
                aggregate::AggError::Upstream(m) => m,
            };
            log_one(
                &ctx,
                LogEntryInput {
                    ts: now_millis(),
                    key_id: Some(key_id),
                    account_id: Some(account_id),
                    model: Some(model),
                    stream: false,
                    status_code: Some(StatusCode::BAD_GATEWAY.as_u16() as i64),
                    prompt_tokens: None,
                    completion_tokens: None,
                    latency_ms: Some(started.elapsed().as_millis() as i64),
                    error: Some(message.clone()),
                },
            );
            openai_error(StatusCode::BAD_GATEWAY, &message)
        }
        Ok(completion) => {
            let usage = completion
                .get("usage")
                .map(|u| sse::Usage {
                    prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_i64()),
                    completion_tokens: u.get("completion_tokens").and_then(|v| v.as_i64()),
                });
            log_one(
                &ctx,
                LogEntryInput {
                    ts: now_millis(),
                    key_id: Some(key_id),
                    account_id: Some(account_id),
                    model: Some(model),
                    stream: false,
                    status_code: Some(status.as_u16() as i64),
                    prompt_tokens: usage.as_ref().and_then(|u| u.prompt_tokens),
                    completion_tokens: usage.as_ref().and_then(|u| u.completion_tokens),
                    latency_ms: Some(started.elapsed().as_millis() as i64),
                    error: None,
                },
            );
            if let Ok(mut p) = ctx.pool.lock() {
                p.mark_healthy(account_id);
            }
            (StatusCode::OK, Json(completion)).into_response()
        }
    }
}

/// Forwards upstream bytes untouched and writes exactly one log row when the
/// body finishes — whether that is a clean end, an upstream error, or the
/// client disconnecting halfway.
struct StreamPump {
    upstream: Option<BoxStream<'static, Result<Bytes, reqwest::Error>>>,
    scan: UsageScan,
    conn: Arc<Mutex<Connection>>,
    pool: Arc<Mutex<pool::Pool>>,
    record: Option<LogRecord>,
}

impl StreamPump {
    fn settle(&mut self, error: Option<String>) {
        let Some(record) = self.record.take() else { return };
        self.scan.finish();
        let usage: Option<Usage> = self.scan.usage();
        let Ok(guard) = self.conn.lock() else { return };
        let _ = wb_logs::insert_log(
            &guard,
            &LogEntryInput {
                ts: now_millis(),
                key_id: Some(record.key_id),
                account_id: Some(record.account_id),
                model: Some(record.model.clone()),
                stream: true,
                status_code: Some(record.status_code as i64),
                prompt_tokens: usage.as_ref().and_then(|u| u.prompt_tokens),
                completion_tokens: usage.as_ref().and_then(|u| u.completion_tokens),
                latency_ms: Some(record.started.elapsed().as_millis() as i64),
                error: error.clone(),
            },
        );
        drop(guard);
        // A stream that ran to completion without an error frame means the
        // account served us: lift any earlier penalty.
        if error.is_none() {
            if let Ok(mut p) = self.pool.lock() {
                p.mark_healthy(record.account_id);
            }
        }
    }
}

impl Stream for StreamPump {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let me = self.get_mut();
        let Some(upstream) = me.upstream.as_mut() else {
            return Poll::Ready(None);
        };
        match upstream.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                me.scan.feed(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(e))) => {
                let message = e.to_string();
                me.settle(Some(format!("上游流中断: {message}")));
                me.upstream = None;
                Poll::Ready(Some(Err(std::io::Error::other(message))))
            }
            Poll::Ready(None) => {
                me.settle(None);
                me.upstream = None;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for StreamPump {
    fn drop(&mut self) {
        // Client went away mid-stream: still count the call.
        self.settle(Some("客户端提前断开".to_string()));
    }
}

fn log_one(ctx: &Ctx, input: LogEntryInput) {
    if let Ok(guard) = ctx.conn.lock() {
        let _ = wb_logs::insert_log(&guard, &input);
    }
}

fn build_router(ctx: Ctx) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route(
            "/v1/chat/completions",
            post(chat_completions).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        // Unknown paths get the OpenAI-shaped error rather than axum's text 404.
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error":{"message":"未知端点","type":"invalid_request_error"}})),
            )
                .into_response()
        })
        .with_state(ctx)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn wb_gateway_start(
    state: tauri::State<'_, DbState>,
    gateway: tauri::State<'_, WbGatewayState>,
    port: Option<i64>,
    lan: Option<bool>,
) -> Result<GatewayStatus, String> {
    let conn = Arc::clone(&state.conn);
    let settings = {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        wb_commands::get_settings(&guard).map_err(|e| e.to_string())?
    };
    let port = port.unwrap_or(settings.port).clamp(1, 65_535) as u16;
    let lan = lan.unwrap_or(settings.lan_enabled);

    if gateway.running.lock().map_err(|e| e.to_string())?.is_some() {
        return Err("网关已在运行，请先停止再改端口或局域网开关".to_string());
    }
    if lan {
        let keys = {
            let guard = conn.lock().map_err(|e| e.to_string())?;
            wb_keys::enabled_count(&guard)?
        };
        if keys == 0 {
            return Err("开启局域网访问前需要先创建一个启用的网关密钥".to_string());
        }
    }

    let bind_ip = if lan { "0.0.0.0" } else { "127.0.0.1" };
    let std_listener = std::net::TcpListener::bind((bind_ip, port))
        .map_err(|e| format!("绑定 {bind_ip}:{port} 失败（端口可能已被占用）: {e}"))?;
    std_listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let bind = std_listener.local_addr().map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::from_std(std_listener).map_err(|e| e.to_string())?;

    let ctx = Ctx {
        conn: Arc::clone(&conn),
        pool: Arc::new(Mutex::new(pool::Pool::default())),
        lan_enabled: lan,
    };

    let (tx, rx) = oneshot::channel::<()>();
    let task = tauri::async_runtime::spawn(async move {
        let server = axum::serve(
            listener,
            app_for(ctx).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = rx.await;
        });
        if let Err(e) = server.await {
            eprintln!("[workbuddy] gateway exited: {e}");
        }
    });

    {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        let _ = wb_commands::update_settings(
            &guard,
            &WbSettingsPatch { port: Some(port as i64), lan_enabled: Some(lan), ..Default::default() },
        );
    }
    gateway.running.lock().map_err(|e| e.to_string())?.replace(Running {
        shutdown: tx,
        task,
        bind,
        started_at: now_millis(),
    });

    report_status(&conn, &gateway)
}

fn app_for(ctx: Ctx) -> Router {
    build_router(ctx)
}

#[tauri::command]
pub async fn wb_gateway_stop(
    state: tauri::State<'_, DbState>,
    gateway: tauri::State<'_, WbGatewayState>,
) -> Result<GatewayStatus, String> {
    let taken = gateway.running.lock().map_err(|e| e.to_string())?.take();
    if let Some(running) = taken {
        let _ = running.shutdown.send(());
        if tokio::time::timeout(std::time::Duration::from_secs(3), running.task)
            .await
            .is_err()
        {
            eprintln!("[workbuddy] gateway did not exit within 3s");
        }
    }
    report_status(&Arc::clone(&state.conn), &gateway)
}

#[tauri::command]
pub async fn wb_gateway_status(
    state: tauri::State<'_, DbState>,
    gateway: tauri::State<'_, WbGatewayState>,
) -> Result<GatewayStatus, String> {
    report_status(&Arc::clone(&state.conn), &gateway)
}

fn report_status(
    conn: &Arc<Mutex<Connection>>,
    gateway: &tauri::State<'_, WbGatewayState>,
) -> Result<GatewayStatus, String> {
    let guard = conn.lock().map_err(|e| e.to_string())?;
    let settings = wb_commands::get_settings(&guard)?;
    let running = gateway.running.lock().map_err(|e| e.to_string())?;
    let info = running.as_ref().map(|r| (r.bind, r.started_at));
    let now = credential::now_unix();
    Ok(GatewayStatus {
        running: info.is_some(),
        bind_addr: info.map(|(b, _)| b.to_string()),
        port: settings.port,
        lan_enabled: settings.lan_enabled,
        started_at: info.map(|(_, at)| at),
        key_count: wb_keys::enabled_count(&guard).unwrap_or(0),
        account_total: guard
            .query_row("SELECT COUNT(*) FROM codebuddy_accounts", [], |r| r.get(0))
            .unwrap_or(0),
        account_eligible: eligible_accounts(&guard, now).len() as i64,
    })
}

/// LAN mode changes the bind address, so a running gateway is restarted.
#[tauri::command]
pub async fn wb_set_lan_mode(
    state: tauri::State<'_, DbState>,
    gateway: tauri::State<'_, WbGatewayState>,
    enabled: bool,
) -> Result<GatewayStatus, String> {
    let was_running = gateway.running.lock().map_err(|e| e.to_string())?.is_some();
    // Check before touching anything: stopping a healthy gateway and then
    // failing to bring it back would leave the user worse off than before they
    // clicked.
    if enabled {
        let conn = Arc::clone(&state.conn);
        let keys = {
            let guard = conn.lock().map_err(|e| e.to_string())?;
            wb_keys::enabled_count(&guard)?
        };
        if keys == 0 {
            return Err("开启局域网访问前需要先创建一个启用的网关密钥".to_string());
        }
    }
    {
        let conn = Arc::clone(&state.conn);
        let guard = conn.lock().map_err(|e| e.to_string())?;
        wb_commands::update_settings(
            &guard,
            &WbSettingsPatch { lan_enabled: Some(enabled), ..Default::default() },
        )?;
    }
    if !was_running {
        return wb_gateway_status(state, gateway).await;
    }
    wb_gateway_stop(state.clone(), gateway.clone()).await?;
    wb_gateway_start(state, gateway, None, Some(enabled)).await
}

// ---------------------------------------------------------------------------
// IP allow/deny rules
// ---------------------------------------------------------------------------

fn rule_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn map_rule(row: &rusqlite::Row) -> rusqlite::Result<IpRuleDto> {
    Ok(IpRuleDto {
        id: row.get(0)?,
        kind: ip_filter::kind_from_str(&row.get::<_, String>(1)?),
        ip_or_cidr: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        note: row.get(4)?,
    })
}

const RULE_COLS: &str = "id, kind, ip_or_cidr, enabled, note";

#[tauri::command]
pub async fn wb_ip_list(state: tauri::State<'_, DbState>) -> Result<Vec<IpRuleDto>, String> {
    let conn = Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        let sql = format!("SELECT {RULE_COLS} FROM wb_ip_rules ORDER BY kind, id");
        let mut stmt = guard.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], map_rule)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        rows
    })
    .await
    .map_err(|e| format!("读取失败: {}", e))?
}

#[tauri::command]
pub async fn wb_ip_add(state: tauri::State<'_, DbState>, rule: NewIpRule) -> Result<IpRuleDto, String> {
    // Validate before storing: a typo in an allow entry is how someone locks
    // themselves out of their own gateway.
    ip_filter::parse_target(&rule.ip_or_cidr)?;
    let conn = Arc::clone(&state.conn);
    let id = rule_id();
    let stored = NewIpRule {
        ip_or_cidr: rule.ip_or_cidr.trim().to_string(),
        ..rule
    };
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        guard
            .execute(
                "INSERT INTO wb_ip_rules (id, kind, ip_or_cidr, enabled, note, created_at)
                 VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                params![id, ip_filter::kind_str(stored.kind), stored.ip_or_cidr, stored.note, now_millis()],
            )
            .map_err(|e| e.to_string())?;
        let sql = format!("SELECT {RULE_COLS} FROM wb_ip_rules WHERE id = ?1");
        let mut stmt = guard.prepare(&sql).map_err(|e| e.to_string())?;
        stmt.query_row(params![id], map_rule).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("添加失败: {}", e))?
}

#[tauri::command]
pub async fn wb_ip_delete(state: tauri::State<'_, DbState>, id: String) -> Result<(), String> {
    let conn = Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        guard.execute("DELETE FROM wb_ip_rules WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| format!("删除失败: {}", e))?
}

#[tauri::command]
pub async fn wb_ip_set_enabled(
    state: tauri::State<'_, DbState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    let conn = Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        guard
            .execute(
                "UPDATE wb_ip_rules SET enabled = ?2 WHERE id = ?1",
                params![id, i64::from(enabled)],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| format!("更新失败: {}", e))?
}
