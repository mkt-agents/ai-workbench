//! LongCat quota: web Cookie against longcat.chat platform usage.
//!
//! Fetch order (CodexBar #2670):
//! 1. GET /api/v1/user-current (required)
//! 2. POST /api/pay/quota/metering/token-packs/summary (primary, best-effort)
//! 3. Fallback GET /api/lc-platform/v1/tokenUsage when no usable lot
//! 4. GET /api/lc-platform/v1/pending-fuel-packages (secondary, best-effort)
//!
//! Full Cookie is stored in quota_accounts.access_token; subject prefers userId.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::codex_quota::{http_client, now_unix, with_db};
use crate::quota_accounts::{list_accounts, token_for, upsert_account, Provider, QuotaAccountDto};
use crate::DbState;

const HOST: &str = "https://longcat.chat";
const USER_CURRENT: &str = "/api/v1/user-current";
const TOKEN_PACKS_SUMMARY: &str = "/api/pay/quota/metering/token-packs/summary";
const TOKEN_USAGE: &str = "/api/lc-platform/v1/tokenUsage";
const PENDING_FUEL: &str = "/api/lc-platform/v1/pending-fuel-packages";
const LOGIN_ENTRY: &str = "https://longcat.chat/platform/";
const WINDOW_LABEL: &str = "longcat-login";
const CLOSE_DELAY: Duration = Duration::from_millis(250);

const SESSION_EXPIRED: &str = "登录已过期，请重新登录或粘贴 Cookie";
const EMPTY_COOKIE: &str = "Cookie 为空";
const USER_MISMATCH: &str = "登录的 LongCat 账号与这张卡片不是同一个，请换回原账号";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongcatQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
    pub account_name: Option<String>,
    pub total_token: Option<f64>,
    pub used_token: Option<f64>,
    pub available_token: Option<f64>,
    pub fuel_total: Option<f64>,
    pub fuel_available: Option<f64>,
    pub fuel_expire_at: Option<i64>,
    pub needs_reauth: bool,
    pub at: i64,
}

impl LongcatQuotaDto {
    fn err(id: i64, label: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        let needs_reauth = message == SESSION_EXPIRED
            || message.contains("过期")
            || message.contains("更换")
            || message.contains("重新登录");
        Self {
            id,
            ok: false,
            message: Some(message),
            label,
            account_name: None,
            total_token: None,
            used_token: None,
            available_token: None,
            fuel_total: None,
            fuel_available: None,
            fuel_expire_at: None,
            needs_reauth,
            at: now_unix(),
        }
    }

    fn ok_from(id: i64, label: Option<String>, snap: QuotaSnapshot) -> Self {
        Self {
            id,
            ok: true,
            message: None,
            label,
            account_name: snap.account_name,
            total_token: snap.total_token,
            used_token: snap.used_token,
            available_token: snap.available_token,
            fuel_total: snap.fuel_total,
            fuel_available: snap.fuel_available,
            fuel_expire_at: snap.fuel_expire_at,
            needs_reauth: false,
            at: now_unix(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct QuotaSnapshot {
    account_name: Option<String>,
    subject: String,
    total_token: Option<f64>,
    used_token: Option<f64>,
    available_token: Option<f64>,
    fuel_total: Option<f64>,
    fuel_available: Option<f64>,
    fuel_expire_at: Option<i64>,
}

fn strip_cookie_prefix(raw: &str) -> &str {
    raw.trim()
        .trim_start_matches("Cookie:")
        .trim_start_matches("cookie:")
        .trim()
}

pub fn normalize_cookie_header(raw: &str) -> Result<String, String> {
    let header = strip_cookie_prefix(raw).to_string();
    if header.is_empty() {
        return Err(EMPTY_COOKIE.to_string());
    }
    Ok(header)
}

#[cfg_attr(not(test), allow(dead_code))]
fn cookie_value(raw: &str, name: &str) -> Option<String> {
    strip_cookie_prefix(raw)
        .split(';')
        .filter_map(|chunk| chunk.trim().split_once('='))
        .filter(|(n, v)| n.trim() == name && !v.trim().is_empty())
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
        .next_back()
}

fn envelope_code(value: &Value) -> Option<i64> {
    value
        .get("code")
        .and_then(|c| c.as_i64())
        .or_else(|| value.get("status").and_then(|c| c.as_i64()))
}

fn envelope_data(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

fn json_f64(value: &Value, key: &str) -> Option<f64> {
    let v = value.get(key)?;
    v.as_f64()
        .or_else(|| v.as_i64().map(|i| i as f64))
        .or_else(|| v.as_u64().map(|i| i as f64))
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}

fn json_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            value.get(key).and_then(|v| {
                v.as_i64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
        })
}

fn is_auth_failure(status: u16, body: &Value) -> bool {
    status == 401 || status == 403 || matches!(envelope_code(body), Some(401 | 403))
}

async fn request_json(
    client: &reqwest::Client,
    method: reqwest::Method,
    path: &str,
    cookie: &str,
) -> Result<(u16, Value), String> {
    let url = format!("{HOST}{path}");
    let mut req = client
        .request(method, &url)
        .header("Cookie", cookie)
        .header("Accept", "application/json");
    if path == TOKEN_PACKS_SUMMARY {
        req = req.header("Content-Type", "application/json").body("{}");
    }
    let resp = req
        .send()
        .await
        .map_err(|e| crate::config::friendly_http_error(e, "请求失败"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((status, body))
}

fn parse_account(body: &Value) -> Result<(String, Option<String>), String> {
    if is_auth_failure(200, body) {
        return Err(SESSION_EXPIRED.to_string());
    }
    let data = envelope_data(body);
    let subject = json_str(data, "userId")
        .or_else(|| json_str(data, "id"))
        .or_else(|| json_str(data, "uid"))
        .or_else(|| json_str(data, "user_id"))
        .ok_or_else(|| "未能识别 LongCat 用户标识".to_string())?;
    let name = json_str(data, "name")
        .or_else(|| json_str(data, "nickname"))
        .or_else(|| json_str(data, "userName"))
        .or_else(|| json_str(data, "username"));
    Ok((subject, name))
}

/// Token Pack primary: currentLot.totalToken + consumedToken.
pub fn parse_token_packs_summary(body: &Value) -> Option<(f64, f64, Option<f64>)> {
    let data = envelope_data(body);
    let lot = data
        .get("currentLot")
        .or_else(|| data.get("current_lot"))
        .filter(|v| v.is_object())?;
    let total = json_f64(lot, "totalToken").or_else(|| json_f64(lot, "total"))?;
    if total <= 0.0 {
        return None;
    }
    let used = json_f64(lot, "consumedToken")
        .or_else(|| json_f64(lot, "usedToken"))
        .or_else(|| json_f64(lot, "used"))
        .unwrap_or(0.0)
        .max(0.0);
    let available = json_f64(lot, "availableToken")
        .or_else(|| json_f64(lot, "remainingToken"))
        .or_else(|| Some((total - used).max(0.0)));
    Some((total, used, available))
}

/// Legacy aggregate: usage.totalToken + availableToken.
pub fn parse_token_usage(body: &Value) -> Option<(f64, f64, Option<f64>)> {
    let outer = envelope_data(body);
    let usage = outer
        .get("usage")
        .filter(|u| u.is_object())
        .unwrap_or(outer);
    let total = json_f64(usage, "totalToken").or_else(|| json_f64(usage, "total"))?;
    if !total.is_finite() {
        return None;
    }
    let available = json_f64(usage, "availableToken").or_else(|| json_f64(usage, "remainingToken"));
    let used = json_f64(usage, "usedToken")
        .or_else(|| json_f64(usage, "consumedToken"))
        .or_else(|| available.map(|r| (total - r).max(0.0)))
        .unwrap_or(0.0);
    Some((total, used.max(0.0), available))
}

fn parse_expire_ms(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(if n > 1_000_000_000_000 { n } else { n * 1000 });
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Some(naive.and_utc().timestamp_millis());
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(naive.and_utc().timestamp_millis());
    }
    None
}

/// Pending fuel packs: total / remaining / nearest expiry (ms).
pub fn parse_fuel_packages(body: &Value) -> Option<(f64, f64, Option<i64>)> {
    let data = envelope_data(body);
    let packages = data
        .get("packages")
        .and_then(|p| p.as_array())
        .or_else(|| data.as_array())?;

    let mut total = 0.0;
    let mut remaining = 0.0;
    let mut saw_remaining = false;
    let mut nearest: Option<i64> = None;

    for pkg in packages {
        if let Some(t) = json_f64(pkg, "totalToken")
            .or_else(|| json_f64(pkg, "total"))
            .or_else(|| json_f64(pkg, "amount"))
        {
            total += t.max(0.0);
        }
        if let Some(r) = json_f64(pkg, "availableToken")
            .or_else(|| json_f64(pkg, "remainingToken"))
            .or_else(|| json_f64(pkg, "remaining"))
        {
            remaining += r.max(0.0);
            saw_remaining = true;
        }
        let expire_raw = json_str(pkg, "expireTime")
            .or_else(|| json_str(pkg, "expireAt"))
            .or_else(|| json_str(pkg, "expiresAt"))
            .or_else(|| {
                json_f64(pkg, "expireTime")
                    .or_else(|| json_f64(pkg, "expireAt"))
                    .map(|n| n.to_string())
            });
        if let Some(ms) = expire_raw.as_deref().and_then(parse_expire_ms) {
            nearest = Some(match nearest {
                Some(n) if n < ms => n,
                _ => ms,
            });
        }
    }

    if total <= 0.0 && !saw_remaining {
        return None;
    }
    if total <= 0.0 {
        total = remaining;
    }
    let remaining = if saw_remaining { remaining } else { total };
    Some((total, remaining, nearest))
}

async fn fetch_snapshot(client: &reqwest::Client, cookie: &str) -> Result<QuotaSnapshot, String> {
    let cookie = normalize_cookie_header(cookie)?;
    let (status, account_body) =
        request_json(client, reqwest::Method::GET, USER_CURRENT, &cookie).await?;
    if is_auth_failure(status, &account_body) {
        return Err(SESSION_EXPIRED.to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("user-current 返回 HTTP {status}"));
    }
    let (subject, account_name) = parse_account(&account_body)?;

    let mut total_token = None;
    let mut used_token = None;
    let mut available_token = None;

    if let Ok((st, body)) =
        request_json(client, reqwest::Method::POST, TOKEN_PACKS_SUMMARY, &cookie).await
    {
        if is_auth_failure(st, &body) {
            return Err(SESSION_EXPIRED.to_string());
        }
        if (200..300).contains(&st) {
            if let Some((total, used, avail)) = parse_token_packs_summary(&body) {
                total_token = Some(total);
                used_token = Some(used);
                available_token = avail;
            }
        }
    }

    if total_token.is_none() {
        let (st, body) = request_json(client, reqwest::Method::GET, TOKEN_USAGE, &cookie).await?;
        if is_auth_failure(st, &body) {
            return Err(SESSION_EXPIRED.to_string());
        }
        if !(200..300).contains(&st) {
            return Err(format!("tokenUsage 返回 HTTP {st}"));
        }
        if let Some((total, used, avail)) = parse_token_usage(&body) {
            total_token = Some(total);
            used_token = Some(used);
            available_token = avail;
        }
    }

    let mut fuel_total = None;
    let mut fuel_available = None;
    let mut fuel_expire_at = None;
    if let Ok((st, body)) = request_json(client, reqwest::Method::GET, PENDING_FUEL, &cookie).await {
        if is_auth_failure(st, &body) {
            return Err(SESSION_EXPIRED.to_string());
        }
        if (200..300).contains(&st) {
            if let Some((t, a, exp)) = parse_fuel_packages(&body) {
                fuel_total = Some(t);
                fuel_available = Some(a);
                fuel_expire_at = exp;
            }
        }
    }

    Ok(QuotaSnapshot {
        account_name,
        subject,
        total_token,
        used_token,
        available_token,
        fuel_total,
        fuel_available,
        fuel_expire_at,
    })
}

fn ensure_same_user(subject: Option<&str>, snap_subject: &str) -> Result<(), String> {
    match subject {
        Some(expected) if !expected.is_empty() && expected != snap_subject => {
            Err(USER_MISMATCH.to_string())
        }
        _ => Ok(()),
    }
}

async fn account_subject(
    conn: Arc<std::sync::Mutex<rusqlite::Connection>>,
    id: i64,
) -> Option<String> {
    use rusqlite::OptionalExtension;
    with_db(conn, move |c| {
        c.query_row(
            "SELECT subject FROM quota_accounts WHERE id = ?1 AND provider = ?2",
            rusqlite::params![id, Provider::Longcat.key()],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())
    })
    .await
    .ok()
    .flatten()
}

fn display_label(name: Option<&str>, subject: &str) -> String {
    name.filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("LongCat {subject}"))
}

#[tauri::command]
pub async fn longcat_add_cookie(
    state: tauri::State<'_, DbState>,
    cookie: String,
) -> Result<QuotaAccountDto, String> {
    let client = http_client()?;
    let snap = fetch_snapshot(&client, &cookie).await?;
    let full = normalize_cookie_header(&cookie)?;
    let label = display_label(snap.account_name.as_deref(), &snap.subject);
    let subject = snap.subject.clone();
    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(
            c,
            Provider::Longcat,
            &subject,
            Some(&label),
            &full,
            "",
            now_unix(),
        )?;
        list_accounts(c, Some(Provider::Longcat))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

#[tauri::command]
pub async fn longcat_set_cookie(
    state: tauri::State<'_, DbState>,
    id: i64,
    cookie: String,
) -> Result<LongcatQuotaDto, String> {
    let client = http_client()?;
    let expected = account_subject(Arc::clone(&state.conn), id).await;
    let snap = fetch_snapshot(&client, &cookie).await?;
    ensure_same_user(expected.as_deref(), &snap.subject)?;
    let full = normalize_cookie_header(&cookie)?;
    let label = display_label(snap.account_name.as_deref(), &snap.subject);
    let subject = snap.subject.clone();
    let label_db = label.clone();
    with_db(Arc::clone(&state.conn), move |c| {
        upsert_account(
            c,
            Provider::Longcat,
            &subject,
            Some(&label_db),
            &full,
            "",
            now_unix(),
        )?;
        Ok(())
    })
    .await?;
    let stored = token_for(Arc::clone(&state.conn), id, Provider::Longcat)
        .await
        .ok();
    Ok(LongcatQuotaDto::ok_from(
        id,
        stored.and_then(|s| s.label).or(Some(label)),
        snap,
    ))
}

#[tauri::command]
pub async fn longcat_quota_fetch(
    state: tauri::State<'_, DbState>,
    id: i64,
) -> Result<LongcatQuotaDto, String> {
    let stored = match token_for(Arc::clone(&state.conn), id, Provider::Longcat).await {
        Ok(t) => t,
        Err(m) => return Ok(LongcatQuotaDto::err(id, None, m)),
    };
    match fetch_snapshot(&http_client()?, &stored.access_token).await {
        Ok(snap) => Ok(LongcatQuotaDto::ok_from(id, stored.label, snap)),
        Err(m) => Ok(LongcatQuotaDto::err(id, stored.label, m)),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongcatCookieCapture {
    pub page_url: String,
    pub ready: bool,
    pub cookie_header: String,
    pub names: Vec<String>,
}

fn domain_matches(host: &str, cookie_domain: &str) -> bool {
    let d = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
    let h = host.to_ascii_lowercase();
    !d.is_empty() && (h == d || h.ends_with(&format!(".{d}")))
}

fn join_longcat_cookies(host: &str, cookies: &[(String, String, String)]) -> (String, Vec<String>) {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (name, value, domain) in cookies {
        if name.is_empty() || value.is_empty() {
            continue;
        }
        let domain_l = domain.to_ascii_lowercase();
        if !domain_matches(host, domain) && !domain_l.contains("longcat") {
            continue;
        }
        if let Some(existing) = pairs.iter_mut().find(|(n, _)| n == name) {
            existing.1 = value.clone();
        } else {
            pairs.push((name.clone(), value.clone()));
        }
    }
    let names: Vec<String> = pairs.iter().map(|(n, _)| n.clone()).collect();
    let joined = pairs
        .into_iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    (joined, names)
}

async fn capture_window_cookies(app: &AppHandle, label: &str) -> Result<LongcatCookieCapture, String> {
    let handle = app.clone();
    let label = label.to_string();
    #[allow(clippy::type_complexity)]
    let (page_url, host, cookies) =
        tokio::task::spawn_blocking(move || -> Result<(String, String, Vec<(String, String, String)>), String> {
            let win = handle
                .get_webview_window(&label)
                .ok_or_else(|| format!("窗口 {label} 已关闭"))?;
            let url = win.url().map_err(|e| e.to_string())?;
            let host = url.host_str().unwrap_or_default().to_string();
            let cookies = win
                .cookies()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|c| {
                    (
                        c.name().to_string(),
                        c.value().to_string(),
                        c.domain().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            Ok((url.to_string(), host, cookies))
        })
        .await
        .map_err(|e| e.to_string())??;

    let (joined, names) = join_longcat_cookies(&host, &cookies);
    let ready = !joined.is_empty() && names.len() >= 2;
    Ok(LongcatCookieCapture {
        page_url,
        ready,
        cookie_header: joined,
        names,
    })
}

#[tauri::command]
pub async fn longcat_open_login_window(app: AppHandle) -> Result<String, String> {
    let parsed = url::Url::parse(LOGIN_ENTRY).map_err(|e| format!("登录 URL 非法: {e}"))?;
    if let Some(existing) = app.get_webview_window(WINDOW_LABEL) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(LOGIN_ENTRY.to_string());
    }
    let window = WebviewWindowBuilder::new(&app, WINDOW_LABEL, WebviewUrl::External(parsed))
        .title("LongCat 平台登录")
        .inner_size(1080.0, 780.0)
        .min_inner_size(480.0, 560.0)
        .resizable(true)
        .decorations(true)
        .center()
        .focused(true)
        .build()
        .map_err(|e| format!("创建登录窗口失败: {e}"))?;
    let _ = window.set_focus();
    Ok(LOGIN_ENTRY.to_string())
}

#[tauri::command]
pub async fn longcat_close_login_window(app: AppHandle) -> Result<(), String> {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CLOSE_DELAY).await;
        if let Some(win) = handle.get_webview_window(WINDOW_LABEL) {
            let _ = win.close();
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn longcat_capture_login_cookies(app: AppHandle) -> Result<LongcatCookieCapture, String> {
    capture_window_cookies(&app, WINDOW_LABEL).await.map_err(|e| {
        if e.contains("已关闭") {
            "登录窗已关闭，请重新点「打开登录」".to_string()
        } else {
            e
        }
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongcatLoginResult {
    pub account: QuotaAccountDto,
    pub quota: LongcatQuotaDto,
}

#[tauri::command]
pub async fn longcat_poll_login(
    app: AppHandle,
    state: tauri::State<'_, DbState>,
    id: Option<i64>,
) -> Result<Option<LongcatLoginResult>, String> {
    let capture = longcat_capture_login_cookies(app.clone()).await?;
    if !capture.ready {
        return Ok(None);
    }
    let cookie = capture.cookie_header;
    let snap = match fetch_snapshot(&http_client()?, &cookie).await {
        Ok(s) => s,
        Err(m) if m == SESSION_EXPIRED => return Ok(None),
        Err(m) => return Err(m),
    };
    if let Some(existing_id) = id {
        let expected = account_subject(Arc::clone(&state.conn), existing_id).await;
        ensure_same_user(expected.as_deref(), &snap.subject)?;
    }
    let full = normalize_cookie_header(&cookie)?;
    let label = display_label(snap.account_name.as_deref(), &snap.subject);
    let subject = snap.subject.clone();
    let account = with_db(Arc::clone(&state.conn), move |c| {
        let account_id = upsert_account(
            c,
            Provider::Longcat,
            &subject,
            Some(&label),
            &full,
            "",
            now_unix(),
        )?;
        list_accounts(c, Some(Provider::Longcat))?
            .into_iter()
            .find(|a| a.id == account_id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await?;

    let _ = longcat_close_login_window(app).await;
    let quota = LongcatQuotaDto::ok_from(account.id, account.label.clone(), snap);
    Ok(Some(LongcatLoginResult { account, quota }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_cookie_prefix() {
        assert_eq!(
            normalize_cookie_header("Cookie: a=1; b=2").unwrap(),
            "a=1; b=2"
        );
        assert!(normalize_cookie_header("  ").is_err());
    }

    #[test]
    fn parse_packs_lot() {
        let body = json!({
            "code": 0,
            "data": {
                "currentLot": {
                    "totalToken": 1000,
                    "consumedToken": 250,
                    "availableToken": 750
                }
            }
        });
        let (total, used, avail) = parse_token_packs_summary(&body).unwrap();
        assert_eq!(total, 1000.0);
        assert_eq!(used, 250.0);
        assert_eq!(avail, Some(750.0));
    }

    #[test]
    fn parse_packs_ignores_empty_lot() {
        let body = json!({ "code": 0, "data": { "currentLot": { "totalToken": 0 } } });
        assert!(parse_token_packs_summary(&body).is_none());
    }

    #[test]
    fn parse_usage_aggregate() {
        let body = json!({
            "code": 0,
            "data": {
                "usage": { "totalToken": 1000, "availableToken": 250 }
            }
        });
        let (total, used, avail) = parse_token_usage(&body).unwrap();
        assert_eq!(total, 1000.0);
        assert_eq!(used, 750.0);
        assert_eq!(avail, Some(250.0));
    }

    #[test]
    fn parse_fuel_sum() {
        let body = json!({
            "code": 0,
            "data": {
                "packages": [
                    { "totalToken": 200, "availableToken": 50, "expireTime": "2026-08-01T00:00:00Z" },
                    { "totalToken": 100, "availableToken": 100, "expireTime": "2026-07-01 00:00:00" }
                ]
            }
        });
        let (total, avail, exp) = parse_fuel_packages(&body).unwrap();
        assert_eq!(total, 300.0);
        assert_eq!(avail, 150.0);
        assert!(exp.is_some());
    }

    #[test]
    fn auth_envelope_flags_reauth() {
        assert!(is_auth_failure(200, &json!({ "code": 401 })));
        assert!(LongcatQuotaDto::err(1, None, SESSION_EXPIRED).needs_reauth);
    }

    #[test]
    fn cookie_value_last_wins() {
        assert_eq!(
            cookie_value("a=1; userId=old; userId=new", "userId").as_deref(),
            Some("new")
        );
    }
}
