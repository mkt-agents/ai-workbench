//! 账号额度看板里「授权登录」得来的账号（Cursor / Grok Bot / DeepSeek）。
//!
//! - Cursor 与 Grok Bot 都是 Cursor 账号体系：走 Cursor 官方的浏览器登录
//!   （`cursor.com/loginDeepControl` + PKCE，后台轮询 `api2.cursor.sh/auth/poll`
//!   取令牌），同一套令牌分别查 DashboardService 的两个用量端点。
//! - DeepSeek 没有授权链接，存开放平台 API Key，查 `/user/balance`。
//!
//! 令牌只存本机 SQLite 的 `quota_accounts` 表（不进数据导出），DTO 不含任何令牌。
//! Codex 有自己的 OAuth 与续期逻辑，单独在 `codex_quota.rs`。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::codex_quota::{http_client, jwt_claims, now_unix, pkce_challenge, random_b64, with_db};
use crate::DbState;

const SCHEMA_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS quota_accounts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        provider TEXT NOT NULL,
        subject TEXT NOT NULL DEFAULT '',
        label TEXT,
        access_token TEXT NOT NULL,
        refresh_token TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
"#;

/// Idempotent; called from the setup block next to the other tables.
pub fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA_SQL).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Cursor,
    Grok,
    DeepSeek,
}

impl Provider {
    pub fn key(self) -> &'static str {
        match self {
            Provider::Cursor => "cursor",
            Provider::Grok => "grok-bot",
            Provider::DeepSeek => "deepseek",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "cursor" => Ok(Provider::Cursor),
            "grok-bot" => Ok(Provider::Grok),
            "deepseek" => Ok(Provider::DeepSeek),
            other => Err(format!("不支持的服务商: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaAccountDto {
    pub id: i64,
    pub provider: String,
    /// 邮箱（Cursor 系）或掩码后的 API Key（DeepSeek）。
    pub label: Option<String>,
    pub created_at: i64,
}

/// 查额度时后端内部用的完整行。
pub(crate) struct StoredToken {
    pub label: Option<String>,
    pub access_token: String,
}

/// 同一服务商下同一主体（JWT sub / API Key 掩码）重复授权只更新令牌。
pub fn upsert_account(
    conn: &Connection,
    provider: Provider,
    subject: &str,
    label: Option<&str>,
    access_token: &str,
    refresh_token: &str,
    now: i64,
) -> Result<i64, String> {
    let existing: Option<i64> = if subject.is_empty() {
        None
    } else {
        conn.query_row(
            "SELECT id FROM quota_accounts WHERE provider = ?1 AND subject = ?2",
            params![provider.key(), subject],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
    };
    if let Some(id) = existing {
        conn.execute(
            "UPDATE quota_accounts
                SET access_token = ?2, refresh_token = ?3,
                    label = COALESCE(?4, label), updated_at = ?5
              WHERE id = ?1",
            params![id, access_token, refresh_token, label, now],
        )
        .map_err(|e| e.to_string())?;
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO quota_accounts
            (provider, subject, label, access_token, refresh_token, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![provider.key(), subject, label, access_token, refresh_token, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

pub fn list_accounts(conn: &Connection, provider: Option<Provider>) -> Result<Vec<QuotaAccountDto>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider, label, created_at FROM quota_accounts
              WHERE ?1 IS NULL OR provider = ?1
              ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![provider.map(Provider::key)], |r| {
            Ok(QuotaAccountDto {
                id: r.get(0)?,
                provider: r.get(1)?,
                label: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub(crate) fn load_token(conn: &Connection, id: i64, provider: Provider) -> Result<StoredToken, String> {
    conn.query_row(
        "SELECT label, access_token FROM quota_accounts WHERE id = ?1 AND provider = ?2",
        params![id, provider.key()],
        |r| {
            Ok(StoredToken {
                label: r.get(0)?,
                access_token: r.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "该账号已被移除".to_string())
}

pub fn delete_account(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM quota_accounts WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 读出某账号令牌（命令层用）。
pub(crate) async fn token_for(
    conn: Arc<Mutex<Connection>>,
    id: i64,
    provider: Provider,
) -> Result<StoredToken, String> {
    with_db(conn, move |c| load_token(c, id, provider)).await
}

// ---------------------------------------------------------------------------
// Cursor 浏览器登录（Cursor / Grok Bot 共用）
// ---------------------------------------------------------------------------

const CURSOR_LOGIN_URL: &str = "https://cursor.com/loginDeepControl";
const CURSOR_POLL_URL: &str = "https://api2.cursor.sh/auth/poll";
const CURSOR_EMAIL_URL: &str = "https://api2.cursor.sh/aiserver.v1.AuthService/GetEmail";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub fn cursor_login_url(challenge: &str, uuid: &str) -> String {
    url::Url::parse_with_params(
        CURSOR_LOGIN_URL,
        &[("challenge", challenge), ("uuid", uuid), ("mode", "login")],
    )
    .map(|u| u.to_string())
    .unwrap_or_default()
}

/// poll 端点登录完成后返回的令牌对；未完成时是 404 / 空体。
pub fn parse_poll_response(body: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let access = v.get("accessToken")?.as_str()?.trim();
    if access.is_empty() {
        return None;
    }
    let refresh = v
        .get("refreshToken")
        .and_then(|r| r.as_str())
        .unwrap_or_default();
    Some((access.to_string(), refresh.to_string()))
}

fn random_uuid() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

pub(crate) fn email_field(v: &serde_json::Value) -> Option<String> {
    ["email", "Email", "userEmail"]
        .iter()
        .find_map(|k| v.get(*k))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| s.contains('@'))
        .map(str::to_string)
}

/// Cursor 访问令牌里没有邮箱声明，另查一次 AuthService/GetEmail；查不到不影响授权。
async fn fetch_cursor_email(client: &reqwest::Client, token: &str) -> Option<String> {
    let resp = client
        .post(CURSOR_EMAIL_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .body("{}")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    email_field(&v)
}

static LOGIN_CANCEL: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);

async fn poll_cursor_login(client: &reqwest::Client, uuid: &str, verifier: &str) -> (String, String) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let resp = client
            .get(CURSOR_POLL_URL)
            .query(&[("uuid", uuid), ("verifier", verifier)])
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        if !resp.status().is_success() {
            continue;
        }
        if let Some(pair) = parse_poll_response(&resp.text().await.unwrap_or_default()) {
            return pair;
        }
    }
}

/// 打开 Cursor 登录页并轮询结果；`provider` 为 `cursor` 或 `grok-bot`。
#[tauri::command]
pub async fn cursor_oauth_start(
    state: tauri::State<'_, DbState>,
    provider: String,
) -> Result<QuotaAccountDto, String> {
    let provider = Provider::parse(&provider)?;
    if provider == Provider::DeepSeek {
        return Err("DeepSeek 不走浏览器授权，请填写 API Key".to_string());
    }
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    if let Ok(mut slot) = LOGIN_CANCEL.lock() {
        if let Some(old) = slot.replace(cancel_tx) {
            let _ = old.send(());
        }
    }

    let verifier = random_b64(32);
    let uuid = random_uuid();
    crate::cloudflared_commands::open_in_browser(cursor_login_url(&pkce_challenge(&verifier), &uuid))?;

    let client = http_client()?;
    let (access, refresh) = tokio::select! {
        pair = poll_cursor_login(&client, &uuid, &verifier) => pair,
        _ = cancel_rx => return Err("已取消授权".to_string()),
        _ = tokio::time::sleep(LOGIN_TIMEOUT) => return Err("授权超时（5 分钟内未完成登录）".to_string()),
    };
    let email = fetch_cursor_email(&client, &access).await;
    let subject = jwt_claims(&access)
        .and_then(|c| c.get("sub").and_then(|s| s.as_str()).map(str::to_string))
        .unwrap_or_default();

    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(c, provider, &subject, email.as_deref(), &access, &refresh, now_unix())?;
        list_accounts(c, Some(provider))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

#[tauri::command]
pub fn cursor_oauth_cancel() {
    if let Ok(mut slot) = LOGIN_CANCEL.lock() {
        if let Some(tx) = slot.take() {
            let _ = tx.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// DeepSeek API Key
// ---------------------------------------------------------------------------

/// `sk-abcd…wxyz`：既当展示名，也当去重主体（不存原 Key 的任何派生哈希）。
pub fn mask_api_key(key: &str) -> String {
    let k = key.trim();
    let chars: Vec<char> = k.chars().collect();
    if chars.len() <= 10 {
        return "••••".to_string();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// 先用 Key 查一次余额，查得通才入库。
#[tauri::command]
pub async fn deepseek_add_key(
    state: tauri::State<'_, DbState>,
    api_key: String,
) -> Result<QuotaAccountDto, String> {
    let key = api_key.trim().to_string();
    if key.is_empty() {
        return Err("请填写 API Key".to_string());
    }
    crate::deepseek_quota::request_balance(&http_client()?, &key).await?;
    let masked = mask_api_key(&key);
    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(c, Provider::DeepSeek, &masked, Some(&masked), &key, "", now_unix())?;
        list_accounts(c, Some(Provider::DeepSeek))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

// ---------------------------------------------------------------------------
// 列表 / 删除
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn quota_accounts_list(state: tauri::State<'_, DbState>) -> Result<Vec<QuotaAccountDto>, String> {
    with_db(Arc::clone(&state.conn), |c| list_accounts(c, None)).await
}

#[tauri::command]
pub async fn quota_account_delete(state: tauri::State<'_, DbState>, id: i64) -> Result<(), String> {
    with_db(Arc::clone(&state.conn), move |c| delete_account(c, id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        ensure_schema(&c).unwrap();
        c
    }

    #[test]
    fn upsert_dedupes_per_provider_and_subject() {
        let c = mem();
        let a = upsert_account(&c, Provider::Cursor, "sub-1", Some("a@b.com"), "t1", "r1", 1).unwrap();
        assert_eq!(upsert_account(&c, Provider::Cursor, "sub-1", None, "t2", "r2", 2).unwrap(), a);
        // 同一个 Cursor 账号也可以单独授权给 Grok Bot 卡。
        let g = upsert_account(&c, Provider::Grok, "sub-1", Some("a@b.com"), "t3", "", 3).unwrap();
        assert_ne!(g, a);

        let tok = load_token(&c, a, Provider::Cursor).unwrap();
        assert_eq!(tok.access_token, "t2");
        assert_eq!(tok.label.as_deref(), Some("a@b.com"));
        assert!(load_token(&c, a, Provider::Grok).is_err(), "provider 不匹配不能取到令牌");

        assert_eq!(list_accounts(&c, Some(Provider::Cursor)).unwrap().len(), 1);
        assert_eq!(list_accounts(&c, None).unwrap().len(), 2);
        delete_account(&c, a).unwrap();
        assert_eq!(list_accounts(&c, None).unwrap().len(), 1);
    }

    #[test]
    fn dto_never_serializes_tokens() {
        let c = mem();
        upsert_account(&c, Provider::DeepSeek, "sk-abc…wxyz", Some("sk-abc…wxyz"), "SECRET_KEY", "", 1).unwrap();
        let json = serde_json::to_string(&list_accounts(&c, None).unwrap()).unwrap();
        assert!(!json.contains("SECRET"), "{json}");
    }

    #[test]
    fn cursor_login_url_carries_challenge_and_uuid() {
        let u = url::Url::parse(&cursor_login_url("chal", "uu")).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(u.host_str(), Some("cursor.com"));
        assert_eq!(q["challenge"], "chal");
        assert_eq!(q["uuid"], "uu");
        assert_eq!(q["mode"], "login");
    }

    #[test]
    fn parse_poll_response_waits_for_access_token() {
        assert_eq!(
            parse_poll_response(r#"{"accessToken":"a","refreshToken":"r","authId":"x"}"#),
            Some(("a".into(), "r".into()))
        );
        assert!(parse_poll_response(r#"{"accessToken":""}"#).is_none());
        assert!(parse_poll_response("").is_none());
    }

    #[test]
    fn random_uuid_is_v4_shaped() {
        let u = random_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_ne!(random_uuid(), u);
    }

    #[test]
    fn mask_api_key_keeps_only_edges() {
        assert_eq!(mask_api_key("sk-1234567890abcdef"), "sk-123…cdef");
        assert_eq!(mask_api_key("short"), "••••");
    }
}
