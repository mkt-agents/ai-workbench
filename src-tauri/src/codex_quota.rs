//! Codex 额度：应用内链接授权（与 Codex CLI 同一套 auth.openai.com PKCE 流程，
//! 浏览器登录后回调本机 localhost:1455）→ 令牌存本机 SQLite `codex_accounts` →
//! `chatgpt.com/backend-api/wham/usage` 拉 5 小时 / 每周限额。
//! 令牌只在后端流转，DTO 不含任何 token 字段。授权出来的是本应用自己的一套令牌，
//! 续期不影响本机 Codex 的登录态。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::DbState;

const ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// OpenAI 侧只登记了这个回调地址，端口不能换。
const CALLBACK_ADDR: &str = "127.0.0.1:1455";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// access token 剩余不足该秒数就先续期再查。
const REFRESH_LEEWAY_SECS: i64 = 60;

const SCHEMA_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS codex_accounts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id TEXT NOT NULL DEFAULT '',
        email TEXT,
        plan_type TEXT,
        access_token TEXT NOT NULL,
        refresh_token TEXT NOT NULL DEFAULT '',
        id_token TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
"#;

/// Idempotent; called from the setup block next to the other tables.
pub fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA_SQL).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountDto {
    pub id: i64,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexWindow {
    /// 已用百分比（0-100）。
    pub used_percent: f64,
    /// 窗口长度（秒）：18000 = 5 小时，604800 = 每周。
    pub window_seconds: Option<i64>,
    /// 重置时刻（epoch 毫秒）。
    pub reset_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub primary: Option<CodexWindow>,
    pub secondary: Option<CodexWindow>,
    pub limit_reached: Option<bool>,
    pub credits_balance: Option<f64>,
    pub credits_unlimited: Option<bool>,
    /// 令牌失效、续期也救不回来：前端据此给出「重新授权」。
    pub needs_reauth: bool,
    pub at: i64,
}

/// 授权类额度（Codex / Cursor / Grok Bot）令牌失效时的统一文案；DTO 的
/// `needs_reauth` 按它判定，改文案不用改判定。
pub(crate) const REAUTH_MESSAGE: &str = "授权已过期";

impl CodexQuotaDto {
    fn err(id: i64, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            ok: false,
            needs_reauth: message == REAUTH_MESSAGE,
            message: Some(message),
            email: None,
            plan_type: None,
            primary: None,
            secondary: None,
            limit_reached: None,
            credits_balance: None,
            credits_unlimited: None,
            at: now_unix(),
        }
    }
}

pub(crate) fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 令牌与身份
// ---------------------------------------------------------------------------

/// OAuth token 端点的返回；续期时 id_token / refresh_token 可能缺省。
#[derive(Debug, Clone, PartialEq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
}

fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn parse_token_response(body: &str) -> Option<TokenSet> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(TokenSet {
        access_token: str_field(&v, "access_token")?,
        refresh_token: str_field(&v, "refresh_token"),
        id_token: str_field(&v, "id_token"),
    })
}

/// JWT payload（不校验签名，只读展示用声明）。
pub(crate) fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Identity {
    pub account_id: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

pub fn identity_from_id_token(id_token: &str) -> Identity {
    let Some(claims) = jwt_claims(id_token) else {
        return Identity::default();
    };
    let auth = claims.get("https://api.openai.com/auth");
    Identity {
        account_id: auth
            .and_then(|a| str_field(a, "chatgpt_account_id"))
            .unwrap_or_default(),
        email: str_field(&claims, "email"),
        plan_type: auth.and_then(|a| str_field(a, "chatgpt_plan_type")),
    }
}

fn token_exp(token: &str) -> Option<i64> {
    jwt_claims(token)?.get("exp")?.as_i64()
}

// ---------------------------------------------------------------------------
// 存储：`&Connection` 进、数据出，便于内存库单测。
// ---------------------------------------------------------------------------

struct StoredAccount {
    id: i64,
    account_id: String,
    email: Option<String>,
    plan_type: Option<String>,
    access_token: String,
    refresh_token: String,
}

/// 同一 ChatGPT 工作区（account_id）重复授权只更新令牌，不新增一行。
pub fn upsert_account(conn: &Connection, tokens: &TokenSet, now: i64) -> Result<i64, String> {
    let ident = tokens
        .id_token
        .as_deref()
        .map(identity_from_id_token)
        .unwrap_or_default();
    let existing: Option<i64> = if ident.account_id.is_empty() {
        None
    } else {
        conn.query_row(
            "SELECT id FROM codex_accounts WHERE account_id = ?1",
            params![ident.account_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
    };
    if let Some(id) = existing {
        update_tokens(conn, id, tokens, now)?;
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO codex_accounts
            (account_id, email, plan_type, access_token, refresh_token, id_token, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            ident.account_id,
            ident.email,
            ident.plan_type,
            tokens.access_token,
            tokens.refresh_token.clone().unwrap_or_default(),
            tokens.id_token.clone().unwrap_or_default(),
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

/// 续期后写回；端点没返回的字段保持原值。
pub fn update_tokens(conn: &Connection, id: i64, tokens: &TokenSet, now: i64) -> Result<(), String> {
    conn.execute(
        "UPDATE codex_accounts
            SET access_token = ?2,
                refresh_token = COALESCE(?3, refresh_token),
                id_token = COALESCE(?4, id_token),
                updated_at = ?5
          WHERE id = ?1",
        params![id, tokens.access_token, tokens.refresh_token, tokens.id_token, now],
    )
    .map_err(|e| e.to_string())?;
    if let Some(id_token) = &tokens.id_token {
        let ident = identity_from_id_token(id_token);
        conn.execute(
            "UPDATE codex_accounts
                SET email = COALESCE(?2, email),
                    plan_type = COALESCE(?3, plan_type),
                    account_id = CASE WHEN ?4 = '' THEN account_id ELSE ?4 END
              WHERE id = ?1",
            params![id, ident.email, ident.plan_type, ident.account_id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn list_accounts(conn: &Connection) -> Result<Vec<CodexAccountDto>, String> {
    let mut stmt = conn
        .prepare("SELECT id, email, plan_type, created_at FROM codex_accounts ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(CodexAccountDto {
                id: r.get(0)?,
                email: r.get(1)?,
                plan_type: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn load_account(conn: &Connection, id: i64) -> Result<StoredAccount, String> {
    conn.query_row(
        "SELECT id, account_id, email, plan_type, access_token, refresh_token
           FROM codex_accounts WHERE id = ?1",
        params![id],
        |r| {
            Ok(StoredAccount {
                id: r.get(0)?,
                account_id: r.get(1)?,
                email: r.get(2)?,
                plan_type: r.get(3)?,
                access_token: r.get(4)?,
                refresh_token: r.get(5)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "该 Codex 账号已被移除".to_string())
}

pub fn delete_account(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM codex_accounts WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) async fn with_db<T: Send + 'static>(
    conn: Arc<Mutex<Connection>>,
    f: impl FnOnce(&Connection) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        f(&guard)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// OAuth（PKCE + 本机回调）
// ---------------------------------------------------------------------------

pub(crate) fn random_b64(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn authorize_url(challenge: &str, state: &str) -> String {
    url::Url::parse_with_params(
        &format!("{ISSUER}/oauth/authorize"),
        &[
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", "openid profile email offline_access"),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("state", state),
            ("originator", "codex_cli_rs"),
        ],
    )
    .map(|u| u.to_string())
    .unwrap_or_default()
}

#[derive(Debug, PartialEq)]
pub enum Callback {
    /// 不是回调路径（favicon 等），回 404 继续等。
    Other,
    Code { code: String, state: String },
    Error(String),
}

/// 解析请求行 `GET /auth/callback?code=..&state=.. HTTP/1.1`。
pub fn parse_callback(request_line: &str) -> Callback {
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let Ok(url) = url::Url::parse(&format!("http://localhost{target}")) else {
        return Callback::Other;
    };
    if url.path() != "/auth/callback" {
        return Callback::Other;
    }
    let get = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    if let Some(err) = get("error") {
        let desc = get("error_description").unwrap_or_default();
        return Callback::Error(if desc.is_empty() { err } else { format!("{err}: {desc}") });
    }
    match (get("code"), get("state")) {
        (Some(code), Some(state)) if !code.is_empty() => Callback::Code { code, state },
        _ => Callback::Error("回调缺少授权码".to_string()),
    }
}

fn callback_page(ok: bool, detail: &str) -> String {
    let (title, text) = if ok {
        ("授权成功", "Codex 账号已添加到 AI Workbench，可以关闭此页面。")
    } else {
        ("授权失败", detail)
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family:system-ui;padding:48px;text-align:center\">\
         <h2>{title}</h2><p>{text}</p></body></html>"
    )
}

async fn respond(sock: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

async fn wait_for_code(listener: TcpListener, expected_state: &str) -> Result<String, String> {
    loop {
        let (mut sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = match tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => continue,
        };
        let req = String::from_utf8_lossy(&buf[..n]);
        match parse_callback(req.lines().next().unwrap_or("")) {
            Callback::Other => respond(&mut sock, "404 Not Found", "").await,
            Callback::Error(msg) => {
                respond(&mut sock, "400 Bad Request", &callback_page(false, &msg)).await;
                return Err(format!("授权失败: {msg}"));
            }
            Callback::Code { code, state } => {
                if state != expected_state {
                    let msg = "state 校验不通过，请重新发起授权";
                    respond(&mut sock, "400 Bad Request", &callback_page(false, msg)).await;
                    return Err(msg.to_string());
                }
                respond(&mut sock, "200 OK", &callback_page(true, "")).await;
                return Ok(code);
            }
        }
    }
}

pub(crate) fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())
}

async fn exchange_code(client: &reqwest::Client, code: &str, verifier: &str) -> Result<TokenSet, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", code)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code_verifier", verifier)
        .finish();
    let resp = client
        .post(format!("{ISSUER}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("换取令牌失败: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("换取令牌失败：上游返回 {status}"));
    }
    parse_token_response(&text).ok_or_else(|| "令牌响应解析失败".to_string())
}

async fn refresh_tokens(client: &reqwest::Client, refresh_token: &str) -> Result<TokenSet, String> {
    let resp = client
        .post(format!("{ISSUER}/oauth/token"))
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "scope": "openid profile email",
        }))
        .send()
        .await
        .map_err(|e| format!("续期请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if status == 400 || status == 401 {
        return Err(REAUTH_MESSAGE.to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("续期失败：上游返回 {status}"));
    }
    parse_token_response(&text).ok_or_else(|| "续期响应解析失败".to_string())
}

/// 进行中的授权的取消开关；新授权会先取消旧的（旧的还占着 1455 端口）。
static LOGIN_CANCEL: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);

async fn bind_callback() -> Result<TcpListener, String> {
    let mut last = String::new();
    for _ in 0..10 {
        match TcpListener::bind(CALLBACK_ADDR).await {
            Ok(l) => return Ok(l),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Err(format!(
        "本机 1455 端口被占用（可能正在运行 codex login），关闭后重试：{last}"
    ))
}

#[tauri::command]
pub async fn codex_oauth_start(state: tauri::State<'_, DbState>) -> Result<CodexAccountDto, String> {
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    if let Ok(mut slot) = LOGIN_CANCEL.lock() {
        if let Some(old) = slot.replace(cancel_tx) {
            let _ = old.send(());
        }
    }
    let listener = bind_callback().await?;

    let verifier = random_b64(64);
    let csrf = random_b64(32);
    let url = authorize_url(&pkce_challenge(&verifier), &csrf);
    crate::cloudflared_commands::open_in_browser(url)?;

    let code = tokio::select! {
        r = wait_for_code(listener, &csrf) => r?,
        _ = cancel_rx => return Err("已取消授权".to_string()),
        _ = tokio::time::sleep(LOGIN_TIMEOUT) => return Err("授权超时（5 分钟内未完成登录）".to_string()),
    };
    let tokens = exchange_code(&http_client()?, &code, &verifier).await?;

    let conn = Arc::clone(&state.conn);
    with_db(conn, move |c| {
        let id = upsert_account(c, &tokens, now_unix())?;
        list_accounts(c)?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

#[tauri::command]
pub fn codex_oauth_cancel() {
    if let Ok(mut slot) = LOGIN_CANCEL.lock() {
        if let Some(tx) = slot.take() {
            let _ = tx.send(());
        }
    }
}

#[tauri::command]
pub async fn codex_list_accounts(state: tauri::State<'_, DbState>) -> Result<Vec<CodexAccountDto>, String> {
    with_db(Arc::clone(&state.conn), list_accounts).await
}

#[tauri::command]
pub async fn codex_delete_account(state: tauri::State<'_, DbState>, id: i64) -> Result<(), String> {
    with_db(Arc::clone(&state.conn), move |c| delete_account(c, id)).await
}

// ---------------------------------------------------------------------------
// 用量
// ---------------------------------------------------------------------------

/// wham/usage 响应里我们关心的部分。
#[derive(Debug, Clone, PartialEq)]
pub struct CodexUsage {
    pub plan_type: Option<String>,
    pub primary: Option<CodexWindow>,
    pub secondary: Option<CodexWindow>,
    pub limit_reached: Option<bool>,
    pub credits_balance: Option<f64>,
    pub credits_unlimited: Option<bool>,
}

fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn as_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn parse_window(v: &serde_json::Value, now_secs: i64) -> Option<CodexWindow> {
    let used_percent = v.get("used_percent").and_then(as_f64)?;
    let reset_at_ms = v
        .get("reset_at")
        .and_then(as_i64)
        .or_else(|| {
            v.get("reset_after_seconds")
                .and_then(as_i64)
                .map(|s| now_secs + s)
        })
        .map(|s| s * 1000);
    Some(CodexWindow {
        used_percent,
        window_seconds: v.get("limit_window_seconds").and_then(as_i64),
        reset_at_ms,
    })
}

pub fn parse_usage(body: &str, now_secs: i64) -> Option<CodexUsage> {
    let root: serde_json::Value = serde_json::from_str(body).ok()?;
    let rate = root.get("rate_limit").filter(|r| r.is_object());
    let primary = rate
        .and_then(|r| r.get("primary_window"))
        .and_then(|w| parse_window(w, now_secs));
    let secondary = rate
        .and_then(|r| r.get("secondary_window"))
        .and_then(|w| parse_window(w, now_secs));
    let credits = root.get("credits").filter(|c| c.is_object());
    let plan_type = str_field(&root, "plan_type");
    if primary.is_none() && secondary.is_none() && credits.is_none() && plan_type.is_none() {
        return None;
    }
    Some(CodexUsage {
        plan_type,
        primary,
        secondary,
        limit_reached: rate
            .and_then(|r| r.get("limit_reached"))
            .and_then(|v| v.as_bool()),
        credits_balance: credits.and_then(|c| c.get("balance")).and_then(as_f64),
        credits_unlimited: credits
            .and_then(|c| c.get("unlimited"))
            .and_then(|v| v.as_bool()),
    })
}

enum UsageError {
    Unauthorized,
    Other(String),
}

async fn request_usage(client: &reqwest::Client, access_token: &str, account_id: &str) -> Result<CodexUsage, UsageError> {
    let mut req = client
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "codex_cli_rs");
    if !account_id.is_empty() {
        req = req.header("ChatGPT-Account-Id", account_id);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| UsageError::Other(format!("请求失败: {e}")))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(UsageError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        return Err(UsageError::Other(format!("上游返回 {status}")));
    }
    let body = resp.text().await.unwrap_or_default();
    parse_usage(&body, now_unix()).ok_or_else(|| UsageError::Other("用量响应解析失败".to_string()))
}

#[tauri::command]
pub async fn codex_quota_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<CodexQuotaDto, String> {
    let conn = Arc::clone(&state.conn);
    let account = match with_db(Arc::clone(&conn), move |c| load_account(c, id)).await {
        Ok(a) => a,
        Err(message) => return Ok(CodexQuotaDto::err(id, message)),
    };
    let client = http_client()?;

    let mut access_token = account.access_token.clone();
    let mut refreshed = false;
    let renew = |refresh_token: String| {
        let client = client.clone();
        let conn = Arc::clone(&conn);
        async move {
            if refresh_token.is_empty() {
                return Err(REAUTH_MESSAGE.to_string());
            }
            let tokens = refresh_tokens(&client, &refresh_token).await?;
            let saved = tokens.clone();
            with_db(conn, move |c| update_tokens(c, account.id, &saved, now_unix())).await?;
            Ok::<TokenSet, String>(tokens)
        }
    };

    let expiring = token_exp(&access_token).is_some_and(|exp| exp - now_unix() < REFRESH_LEEWAY_SECS);
    if expiring {
        match renew(account.refresh_token.clone()).await {
            Ok(t) => {
                access_token = t.access_token;
                refreshed = true;
            }
            Err(message) => return Ok(CodexQuotaDto::err(id, message)),
        }
    }

    let usage = match request_usage(&client, &access_token, &account.account_id).await {
        Ok(u) => u,
        Err(UsageError::Unauthorized) if !refreshed => {
            let t = match renew(account.refresh_token.clone()).await {
                Ok(t) => t,
                Err(message) => return Ok(CodexQuotaDto::err(id, message)),
            };
            match request_usage(&client, &t.access_token, &account.account_id).await {
                Ok(u) => u,
                Err(UsageError::Unauthorized) => {
                    return Ok(CodexQuotaDto::err(id, REAUTH_MESSAGE))
                }
                Err(UsageError::Other(m)) => return Ok(CodexQuotaDto::err(id, m)),
            }
        }
        Err(UsageError::Unauthorized) => {
            return Ok(CodexQuotaDto::err(id, REAUTH_MESSAGE))
        }
        Err(UsageError::Other(m)) => return Ok(CodexQuotaDto::err(id, m)),
    };

    Ok(CodexQuotaDto {
        id,
        ok: true,
        message: None,
        email: account.email,
        plan_type: usage.plan_type.or(account.plan_type),
        primary: usage.primary,
        secondary: usage.secondary,
        limit_reached: usage.limit_reached,
        credits_balance: usage.credits_balance,
        credits_unlimited: usage.credits_unlimited,
        needs_reauth: false,
        at: now_unix(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &str) -> String {
        let enc = |s: &str| URL_SAFE_NO_PAD.encode(s.as_bytes());
        format!("{}.{}.sig", enc(r#"{"alg":"RS256"}"#), enc(payload))
    }

    fn id_token(account: &str, email: &str, plan: &str) -> String {
        jwt(&format!(
            r#"{{"email":"{email}","https://api.openai.com/auth":{{"chatgpt_plan_type":"{plan}","chatgpt_account_id":"{account}"}}}}"#
        ))
    }

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_schema(&c).unwrap();
        c
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_carries_pkce_and_fixed_redirect() {
        let u = url::Url::parse(&authorize_url("chal", "st")).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(u.as_str().split('?').next(), Some("https://auth.openai.com/oauth/authorize"));
        assert_eq!(q["redirect_uri"], REDIRECT_URI);
        assert_eq!(q["code_challenge"], "chal");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "st");
        assert!(q["scope"].contains("offline_access"));
    }

    #[test]
    fn parse_callback_distinguishes_paths_and_errors() {
        assert_eq!(
            parse_callback("GET /auth/callback?code=abc&state=xyz HTTP/1.1"),
            Callback::Code { code: "abc".into(), state: "xyz".into() }
        );
        assert_eq!(parse_callback("GET /favicon.ico HTTP/1.1"), Callback::Other);
        assert_eq!(
            parse_callback("GET /auth/callback?error=access_denied&error_description=nope HTTP/1.1"),
            Callback::Error("access_denied: nope".into())
        );
        assert!(matches!(parse_callback("GET /auth/callback?state=x HTTP/1.1"), Callback::Error(_)));
    }

    #[test]
    fn identity_reads_id_token_claims() {
        let ident = identity_from_id_token(&id_token("acc-1", "a@b.com", "plus"));
        assert_eq!(ident.account_id, "acc-1");
        assert_eq!(ident.email.as_deref(), Some("a@b.com"));
        assert_eq!(ident.plan_type.as_deref(), Some("plus"));
        assert_eq!(identity_from_id_token("garbage"), Identity::default());
    }

    #[test]
    fn upsert_dedupes_by_account_and_refresh_keeps_missing_fields() {
        let c = mem();
        let first = TokenSet {
            access_token: "at1".into(),
            refresh_token: Some("rt1".into()),
            id_token: Some(id_token("acc-1", "a@b.com", "plus")),
        };
        let id = upsert_account(&c, &first, 1).unwrap();
        let again = TokenSet { access_token: "at2".into(), ..first.clone() };
        assert_eq!(upsert_account(&c, &again, 2).unwrap(), id);
        let other = TokenSet { id_token: Some(id_token("acc-2", "c@d.com", "pro")), ..first };
        assert_ne!(upsert_account(&c, &other, 3).unwrap(), id);
        assert_eq!(list_accounts(&c).unwrap().len(), 2);

        // 续期响应只带 access_token：refresh_token 与身份保持原值。
        let renewed = TokenSet { access_token: "at3".into(), refresh_token: None, id_token: None };
        update_tokens(&c, id, &renewed, 4).unwrap();
        let a = load_account(&c, id).unwrap();
        assert_eq!(a.access_token, "at3");
        assert_eq!(a.refresh_token, "rt1");
        assert_eq!(a.email.as_deref(), Some("a@b.com"));

        delete_account(&c, id).unwrap();
        assert!(load_account(&c, id).is_err());
    }

    #[test]
    fn only_the_reauth_message_flags_needs_reauth() {
        let expired = serde_json::to_value(CodexQuotaDto::err(1, REAUTH_MESSAGE)).unwrap();
        assert_eq!(expired["needsReauth"], true);
        let other = serde_json::to_value(CodexQuotaDto::err(1, "网络错误")).unwrap();
        assert_eq!(other["needsReauth"], false);
    }

    #[test]
    fn dto_never_serializes_tokens() {
        let c = mem();
        let t = TokenSet {
            access_token: "SECRET_AT".into(),
            refresh_token: Some("SECRET_RT".into()),
            id_token: Some(id_token("acc-1", "a@b.com", "plus")),
        };
        upsert_account(&c, &t, 1).unwrap();
        let json = serde_json::to_string(&list_accounts(&c).unwrap()).unwrap();
        assert!(!json.contains("SECRET"), "{json}");
    }

    #[test]
    fn parse_token_response_requires_access_token() {
        let t = parse_token_response(r#"{"access_token":"a","refresh_token":"r","id_token":"i"}"#).unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("r"));
        assert!(parse_token_response(r#"{"refresh_token":"r"}"#).is_none());
        assert!(parse_token_response("nope").is_none());
    }

    #[test]
    fn parse_usage_reads_both_windows() {
        let body = r#"{"plan_type":"plus","rate_limit":{"allowed":true,"limit_reached":false,
            "primary_window":{"used_percent":42,"limit_window_seconds":18000,"reset_after_seconds":3600,"reset_at":1790000000},
            "secondary_window":{"used_percent":7.5,"limit_window_seconds":604800,"reset_after_seconds":86400}},
            "credits":{"has_credits":false,"unlimited":false,"balance":"0"}}"#;
        let u = parse_usage(body, 1_000).expect("parse");
        assert_eq!(u.plan_type.as_deref(), Some("plus"));
        let p = u.primary.expect("primary");
        assert_eq!(p.used_percent, 42.0);
        assert_eq!(p.window_seconds, Some(18_000));
        assert_eq!(p.reset_at_ms, Some(1_790_000_000_000));
        let s = u.secondary.expect("secondary");
        assert_eq!(s.used_percent, 7.5);
        // 没有 reset_at 时用 now + reset_after_seconds。
        assert_eq!(s.reset_at_ms, Some((1_000 + 86_400) * 1000));
        assert_eq!(u.limit_reached, Some(false));
        assert_eq!(u.credits_balance, Some(0.0));
        assert_eq!(u.credits_unlimited, Some(false));
    }

    #[test]
    fn parse_usage_rejects_garbage() {
        assert!(parse_usage("", 0).is_none());
        assert!(parse_usage(r#"{"detail":"Unauthorized"}"#, 0).is_none());
        assert!(parse_usage(r#"{"rate_limit":null}"#, 0).is_none());
    }
}
