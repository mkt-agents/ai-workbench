//! 小米 MiMo 额度：
//! - `sk-…` 按量：Bearer/api-key 查 `api.xiaomimimo.com/v1/user/balance`
//! - `tp-…` / `ttp-…` Token Plan：Key 只用于验证；用量走开放平台 Cookie
//!   `GET platform.xiaomimimo.com/api/v1/tokenPlan/usage`
//!   （需要 `api-platform_serviceToken` + `userId`，Cookie 存 `quota_accounts.refresh_token`）

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use url::Url;

use crate::codex_quota::{http_client, now_unix, with_db};
use crate::quota_accounts::{mask_api_key, token_for, upsert_account, Provider, QuotaAccountDto};
use crate::DbState;

const PAYG_BASE: &str = "https://api.xiaomimimo.com/v1";
const TOKEN_PLAN_CN: &str = "https://token-plan-cn.xiaomimimo.com/v1";
const TOKEN_PLAN_SGP: &str = "https://token-plan-sgp.xiaomimimo.com/v1";
const TOKEN_PLAN_AMS: &str = "https://token-plan-ams.xiaomimimo.com/v1";
const PLATFORM_API: &str = "https://platform.xiaomimimo.com/api/v1";
const PLATFORM_USAGE: &str = "https://platform.xiaomimimo.com/api/v1/tokenPlan/usage";
const PLATFORM_LOGIN: &str = "https://platform.xiaomimimo.com/#/console/plan-manage";
const WINDOW_LABEL: &str = "mimo-platform-login";
const CLOSE_DELAY: Duration = Duration::from_millis(250);

const INVALID_KEY: &str = "API Key 无效或已失效，请更换";
const SESSION_EXPIRED: &str = "开放平台登录已过期，请重新登录查询用量";
const NEED_SESSION: &str = "请登录开放平台以查询 Token Plan 用量";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MimoMode {
    Payg,
    TokenPlan,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MimoBalance {
    pub label: String,
    pub amount: f64,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MimoWindow {
    pub label: String,
    pub used: f64,
    pub limit: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MimoQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
    pub plan: Option<String>,
    pub mode: Option<MimoMode>,
    pub balances: Vec<MimoBalance>,
    pub windows: Vec<MimoWindow>,
    /// 套餐周期结束（epoch 毫秒），来自 `tokenPlan/detail.currentPeriodEnd`。
    pub period_end_ms: Option<i64>,
    /// 上游标记套餐已过期。
    pub expired: bool,
    /// 已绑定开放平台 Cookie（可查 Token Plan 用量）。
    pub has_session: bool,
    pub needs_reauth: bool,
    pub at: i64,
}

impl MimoQuotaDto {
    fn err(id: i64, label: Option<String>, has_session: bool, message: impl Into<String>) -> Self {
        let message = message.into();
        let needs_reauth = message == INVALID_KEY
            || message == SESSION_EXPIRED
            || message.contains("更换")
            || message.contains("重新登录");
        Self {
            id,
            ok: false,
            needs_reauth,
            message: Some(message),
            label,
            plan: None,
            mode: None,
            balances: Vec::new(),
            windows: Vec::new(),
            period_end_ms: None,
            expired: false,
            has_session,
            at: now_unix(),
        }
    }

    fn ok(
        id: i64,
        label: Option<String>,
        plan: Option<String>,
        mode: MimoMode,
        balances: Vec<MimoBalance>,
        windows: Vec<MimoWindow>,
        period_end_ms: Option<i64>,
        expired: bool,
        has_session: bool,
        message: Option<String>,
    ) -> Self {
        Self {
            id,
            ok: true,
            message,
            label,
            plan,
            mode: Some(mode),
            balances,
            windows,
            period_end_ms,
            expired,
            has_session,
            needs_reauth: false,
            at: now_unix(),
        }
    }
}

fn is_token_plan_key(key: &str) -> bool {
    let k = key.trim().to_ascii_lowercase();
    k.starts_with("tp-") || k.starts_with("ttp-")
}

fn parse_f64(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0.0)
}

/// 套餐 Credits 上游常是「个」量级（数十亿），展示换算成「亿」。
fn scale_credits(used: f64, limit: f64) -> (f64, f64, &'static str) {
    if limit >= 1_000_000.0 {
        (used / 100_000_000.0, limit / 100_000_000.0, "本月 Credits（亿）")
    } else {
        (used, limit, "本月 Credits")
    }
}

fn usage_items<'a>(root: &'a Value) -> Option<&'a Vec<Value>> {
    root.pointer("/data/monthUsage/items")
        .or_else(|| root.pointer("/data/usage/items"))
        .and_then(|v| v.as_array())
}

pub fn parse_platform_usage(body: &str) -> Result<(Option<String>, Vec<MimoWindow>), String> {
    let root: Value = serde_json::from_str(body).map_err(|_| "用量响应不是合法 JSON".to_string())?;
    if let Some(code) = root.get("code").and_then(|c| c.as_i64()) {
        if code == 401 {
            return Err(SESSION_EXPIRED.to_string());
        }
        if code != 0 {
            let msg = root
                .get("message")
                .or_else(|| root.get("msg"))
                .and_then(|v| v.as_str())
                .unwrap_or("上游返回错误");
            return Err(msg.to_string());
        }
    }
    let items = usage_items(&root).ok_or_else(|| "响应缺少用量 items".to_string())?;
    let mut windows = Vec::new();
    for item in items {
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("token");
        let used_raw = item.get("used").map(parse_f64).unwrap_or(0.0);
        let limit_raw = item.get("limit").map(parse_f64).unwrap_or(0.0);
        if limit_raw <= 0.0 {
            continue;
        }
        let (used, limit, label) = if matches!(name, "plan_total_token" | "month_total_token" | "tokens_monthly") {
            scale_credits(used_raw, limit_raw)
        } else {
            (used_raw, limit_raw, name)
        };
        windows.push(MimoWindow {
            label: label.to_string(),
            used,
            limit,
        });
    }
    if windows.is_empty() {
        return Err("用量窗口为空".to_string());
    }
    let plan = root
        .pointer("/data/planName")
        .or_else(|| root.pointer("/data/plan_name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| Some("Token Plan".to_string()));
    Ok((plan, windows))
}

pub fn parse_balance(body: &str, mode: MimoMode) -> Result<(Option<String>, Vec<MimoBalance>, Vec<MimoWindow>), String> {
    let root: Value = serde_json::from_str(body).map_err(|_| "余额响应不是合法 JSON".to_string())?;
    if let Some(code) = root.get("code").and_then(|c| c.as_i64()) {
        if code != 0 {
            let msg = root
                .get("message")
                .or_else(|| root.get("msg"))
                .and_then(|v| v.as_str())
                .unwrap_or("上游返回错误");
            return Err(msg.to_string());
        }
    }
    let data = root.get("data").unwrap_or(&root);
    let plan = data
        .get("plan_name")
        .or_else(|| data.get("planName"))
        .or_else(|| data.get("plan"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            Some(match mode {
                MimoMode::TokenPlan => "Token Plan".to_string(),
                MimoMode::Payg => "按量付费".to_string(),
            })
        });

    let mut windows = Vec::new();
    let mut balances = Vec::new();

    let tok_bal = data.get("token_balance").or_else(|| data.get("tokenBalance"));
    let tok_lim = data.get("token_limit").or_else(|| data.get("tokenLimit"));
    if let (Some(bal), Some(lim)) = (tok_bal, tok_lim) {
        let remaining = parse_f64(bal);
        let limit = parse_f64(lim);
        if limit > 0.0 {
            let (used, lim, label) = scale_credits((limit - remaining).max(0.0), limit);
            windows.push(MimoWindow {
                label: label.to_string(),
                used,
                limit: lim,
            });
        }
    }

    for (key, alt, label, currency) in [
        ("balance", "total_balance", "账户余额", "CNY"),
        ("charge_balance", "chargeBalance", "充值余额", "CNY"),
        ("cashBalance", "cash_balance", "充值余额", "CNY"),
        ("granted_balance", "grantedBalance", "赠送余额", "CNY"),
        ("giftBalance", "gift_balance", "赠送余额", "CNY"),
    ] {
        if let Some(v) = data.get(key).or_else(|| data.get(alt)) {
            let amount = parse_f64(v);
            if amount > 0.0 || key == "balance" {
                if !balances.iter().any(|b: &MimoBalance| b.label == label) {
                    balances.push(MimoBalance {
                        label: label.to_string(),
                        amount,
                        currency: currency.to_string(),
                    });
                }
            }
        }
    }

    if windows.is_empty() && balances.is_empty() {
        return Err("余额响应无可展示字段".to_string());
    }
    Ok((plan, balances, windows))
}

/// 抽出用量接口需要的 Cookie；缺 `api-platform_serviceToken` / `userId` 则失败。
pub fn normalize_cookie_header(raw: &str) -> Result<String, String> {
    let raw = raw.trim().trim_start_matches("Cookie:").trim_start_matches("cookie:").trim();
    let known = [
        "api-platform_serviceToken",
        "userId",
        "api-platform_ph",
        "api-platform_slh",
        "serviceToken",
    ];
    let mut pairs: Vec<(String, String)> = Vec::new();
    for chunk in raw.split(';') {
        let Some((name, value)) = chunk.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim().trim_matches('"');
        if known.contains(&name) && !value.is_empty() {
            if let Some(existing) = pairs.iter_mut().find(|(n, _)| n == name) {
                existing.1 = value.to_string();
            } else {
                pairs.push((name.to_string(), value.to_string()));
            }
        }
    }
    // 个别抓包只有 serviceToken：映射成 api-platform_serviceToken。
    if !pairs.iter().any(|(n, _)| n == "api-platform_serviceToken") {
        if let Some((_, v)) = pairs.iter().find(|(n, _)| n == "serviceToken") {
            pairs.push(("api-platform_serviceToken".to_string(), v.clone()));
        }
    }
    let has_token = pairs.iter().any(|(n, _)| n == "api-platform_serviceToken");
    let has_user = pairs.iter().any(|(n, _)| n == "userId");
    if !has_token || !has_user {
        return Err("Cookie 需包含 api-platform_serviceToken 与 userId（从开放平台用量请求里复制完整 Cookie）".to_string());
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs
        .into_iter()
        .filter(|(n, _)| n != "serviceToken")
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; "))
}

async fn get_json_key(client: &reqwest::Client, url: &str, key: &str) -> Result<(u16, String), String> {
    for (name, value) in [("api-key", key.to_string()), ("Authorization", format!("Bearer {key}"))] {
        let resp = client
            .get(url)
            .header(name, &value)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| crate::config::friendly_http_error(e, "请求失败"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 401 && status != 403 {
            return Ok((status, text));
        }
        if name == "Authorization" {
            return Ok((status, text));
        }
    }
    Ok((401, String::new()))
}

async fn get_json_cookie(client: &reqwest::Client, url: &str, cookie: &str) -> Result<(u16, String), String> {
    let resp = client
        .get(url)
        .header("Cookie", cookie)
        .header("Accept", "application/json")
        .header("Referer", "https://platform.xiaomimimo.com/")
        .send()
        .await
        .map_err(|e| crate::config::friendly_http_error(e, "请求失败"))?;
    Ok((resp.status().as_u16(), resp.text().await.unwrap_or_default()))
}

/// 解析 `currentPeriodEnd`：如 `2026-05-04 23:59 UTC` / RFC3339 / 日期。
/// 官方文案带 `(UTC)`；仅到分钟时按该分钟末秒（与平台「23:59:59」一致）。
pub fn parse_period_end_ms(raw: &str) -> Option<i64> {
    use chrono::Timelike;
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    let trimmed = s
        .trim_end_matches("UTC")
        .trim_end_matches("utc")
        .trim()
        .trim_end_matches('Z')
        .trim();
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
        return Some(naive.and_utc().timestamp_millis());
    }
    // `2026-11-07 23:59` → 23:59:59 UTC（开放平台展示形态）
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M") {
        return Some(naive.with_second(59)?.and_utc().timestamp_millis());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Some(d.and_hms_opt(23, 59, 59)?.and_utc().timestamp_millis());
    }
    None
}

struct PlanDetail {
    plan: Option<String>,
    period_end_ms: Option<i64>,
    expired: bool,
}

fn parse_plan_detail(body: &str) -> Option<PlanDetail> {
    let root: Value = serde_json::from_str(body).ok()?;
    let code = root.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    if code != 0 {
        return None;
    }
    let data = root.get("data")?;
    let plan = data
        .get("planCode")
        .or_else(|| data.get("plan_code"))
        .or_else(|| data.get("planName"))
        .or_else(|| data.get("plan_name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let period_end_ms = data
        .get("currentPeriodEnd")
        .or_else(|| data.get("current_period_end"))
        .and_then(|v| v.as_str())
        .and_then(parse_period_end_ms);
    let expired = data.get("expired").and_then(|v| v.as_bool()).unwrap_or(false);
    Some(PlanDetail {
        plan,
        period_end_ms,
        expired,
    })
}

struct CookieFetch {
    plan: Option<String>,
    windows: Vec<MimoWindow>,
    balances: Vec<MimoBalance>,
    period_end_ms: Option<i64>,
    expired: bool,
}

async fn fetch_usage_with_cookie(client: &reqwest::Client, cookie: &str) -> Result<CookieFetch, String> {
    let cookie = normalize_cookie_header(cookie)?;
    let (status, text) = get_json_cookie(client, PLATFORM_USAGE, &cookie).await?;
    if status == 401 || status == 403 {
        return Err(SESSION_EXPIRED.to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("用量接口返回 {status}"));
    }
    let (mut plan, windows) = parse_platform_usage(&text)?;

    let mut balances = Vec::new();
    if let Ok((st, body)) = get_json_cookie(client, &format!("{PLATFORM_API}/balance"), &cookie).await {
        if (200..300).contains(&st) {
            if let Ok((_, b, _)) = parse_balance(&body, MimoMode::TokenPlan) {
                balances = b;
            }
        }
    }

    let mut period_end_ms = None;
    let mut expired = false;
    if let Ok((st, body)) = get_json_cookie(client, &format!("{PLATFORM_API}/tokenPlan/detail"), &cookie).await {
        if (200..300).contains(&st) {
            if let Some(detail) = parse_plan_detail(&body) {
                if detail.plan.is_some() {
                    plan = detail.plan;
                }
                period_end_ms = detail.period_end_ms;
                expired = detail.expired;
            }
        }
    }

    Ok(CookieFetch {
        plan,
        windows,
        balances,
        period_end_ms,
        expired,
    })
}

async fn detect_token_plan_base(client: &reqwest::Client, key: &str) -> Result<Option<&'static str>, String> {
    for base in [TOKEN_PLAN_CN, TOKEN_PLAN_SGP, TOKEN_PLAN_AMS] {
        let (status, _) = get_json_key(client, &format!("{base}/models"), key).await?;
        if status == 200 {
            return Ok(Some(base));
        }
    }
    Ok(None)
}

struct FetchOk {
    mode: MimoMode,
    plan: Option<String>,
    balances: Vec<MimoBalance>,
    windows: Vec<MimoWindow>,
    period_end_ms: Option<i64>,
    expired: bool,
    message: Option<String>,
}

async fn fetch_quota(client: &reqwest::Client, key: &str, session_cookie: &str) -> Result<FetchOk, String> {
    if is_token_plan_key(key) {
        let _base = detect_token_plan_base(client, key)
            .await?
            .ok_or_else(|| INVALID_KEY.to_string())?;

        if !session_cookie.trim().is_empty() {
            match fetch_usage_with_cookie(client, session_cookie).await {
                Ok(c) => {
                    return Ok(FetchOk {
                        mode: MimoMode::TokenPlan,
                        plan: c.plan,
                        balances: c.balances,
                        windows: c.windows,
                        period_end_ms: c.period_end_ms,
                        expired: c.expired,
                        message: None,
                    });
                }
                Err(m) if m == SESSION_EXPIRED => {
                    return Err(SESSION_EXPIRED.to_string());
                }
                Err(_) => { /* 继续尝试其它路径 */ }
            }
        }

        // 少数集群可能对 Key 开放 usage/balance。
        for base in [TOKEN_PLAN_CN, TOKEN_PLAN_SGP, TOKEN_PLAN_AMS] {
            let (status, text) = get_json_key(client, &format!("{base}/user/balance"), key).await?;
            if (200..300).contains(&status) {
                if let Ok((plan, balances, windows)) = parse_balance(&text, MimoMode::TokenPlan) {
                    if !windows.is_empty() || !balances.is_empty() {
                        return Ok(FetchOk {
                            mode: MimoMode::TokenPlan,
                            plan,
                            balances,
                            windows,
                            period_end_ms: None,
                            expired: false,
                            message: None,
                        });
                    }
                }
            }
        }

        return Ok(FetchOk {
            mode: MimoMode::TokenPlan,
            plan: Some("Token Plan".to_string()),
            balances: Vec::new(),
            windows: Vec::new(),
            period_end_ms: None,
            expired: false,
            message: Some(NEED_SESSION.to_string()),
        });
    }

    let (status, text) = get_json_key(client, &format!("{PAYG_BASE}/user/balance"), key).await?;
    if status == 401 || status == 403 {
        return Err(INVALID_KEY.to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("上游返回 {status}"));
    }
    let (plan, balances, windows) = parse_balance(&text, MimoMode::Payg)?;
    Ok(FetchOk {
        mode: MimoMode::Payg,
        plan,
        balances,
        windows,
        period_end_ms: None,
        expired: false,
        message: None,
    })
}

async fn save_session_cookie(conn: Arc<std::sync::Mutex<rusqlite::Connection>>, id: i64, cookie: String) -> Result<(), String> {
    with_db(conn, move |c| {
        c.execute(
            "UPDATE quota_accounts SET refresh_token = ?1, updated_at = ?2 WHERE id = ?3 AND provider = ?4",
            rusqlite::params![cookie, now_unix(), id, Provider::Mimo.key()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

async fn clear_session_cookie(conn: Arc<std::sync::Mutex<rusqlite::Connection>>, id: i64) -> Result<(), String> {
    save_session_cookie(conn, id, String::new()).await
}

/// 先查一次额度 / 校验 Key，通过才入库。
#[tauri::command]
pub async fn mimo_add_key(state: tauri::State<'_, DbState>, api_key: String) -> Result<QuotaAccountDto, String> {
    let key = api_key.trim().to_string();
    if key.is_empty() {
        return Err("请填写 API Key".to_string());
    }
    fetch_quota(&http_client()?, &key, "").await?;
    let masked = mask_api_key(&key);
    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(c, Provider::Mimo, &masked, Some(&masked), &key, "", now_unix())?;
        crate::quota_accounts::list_accounts(c, Some(Provider::Mimo))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

/// 粘贴开放平台 Cookie，验证用量接口后写入该账号（refresh_token）。
#[tauri::command]
pub async fn mimo_set_cookie(
    state: tauri::State<'_, DbState>,
    id: i64,
    cookie: String,
) -> Result<MimoQuotaDto, String> {
    let client = http_client()?;
    let c = fetch_usage_with_cookie(&client, &cookie).await?;
    let normalized = normalize_cookie_header(&cookie)?;
    save_session_cookie(Arc::clone(&state.conn), id, normalized).await?;
    let stored = token_for(Arc::clone(&state.conn), id, Provider::Mimo).await.ok();
    Ok(MimoQuotaDto::ok(
        id,
        stored.as_ref().and_then(|s| s.label.clone()),
        c.plan,
        MimoMode::TokenPlan,
        c.balances,
        c.windows,
        c.period_end_ms,
        c.expired,
        true,
        None,
    ))
}

#[tauri::command]
pub async fn mimo_quota_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<MimoQuotaDto, String> {
    let stored = match token_for(Arc::clone(&state.conn), id, Provider::Mimo).await {
        Ok(t) => t,
        Err(m) => return Ok(MimoQuotaDto::err(id, None, false, m)),
    };
    let has_session = !stored.refresh_token.trim().is_empty();
    match fetch_quota(&http_client()?, &stored.access_token, &stored.refresh_token).await {
        Ok(ok) => Ok(MimoQuotaDto::ok(
            id,
            stored.label,
            ok.plan,
            ok.mode,
            ok.balances,
            ok.windows,
            ok.period_end_ms,
            ok.expired,
            has_session && ok.message.as_deref() != Some(NEED_SESSION),
            ok.message,
        )),
        Err(m) if m == SESSION_EXPIRED => {
            let _ = clear_session_cookie(Arc::clone(&state.conn), id).await;
            Ok(MimoQuotaDto::err(id, stored.label, false, m))
        }
        Err(m) => Ok(MimoQuotaDto::err(id, stored.label, has_session, m)),
    }
}

#[tauri::command]
pub async fn mimo_open_login_window(app: AppHandle) -> Result<String, String> {
    let parsed = Url::parse(PLATFORM_LOGIN).map_err(|e| format!("登录 URL 非法: {e}"))?;
    if let Some(existing) = app.get_webview_window(WINDOW_LABEL) {
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(PLATFORM_LOGIN.to_string());
    }
    let window = WebviewWindowBuilder::new(&app, WINDOW_LABEL, WebviewUrl::External(parsed))
        .title("小米 MiMo 开放平台登录")
        .inner_size(1080.0, 780.0)
        .min_inner_size(480.0, 560.0)
        .resizable(true)
        .decorations(true)
        .center()
        .focused(true)
        .build()
        .map_err(|e| format!("创建登录窗口失败: {e}"))?;
    let _ = window.set_focus();
    Ok(PLATFORM_LOGIN.to_string())
}

#[tauri::command]
pub async fn mimo_close_login_window(app: AppHandle) -> Result<(), String> {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CLOSE_DELAY).await;
        if let Some(win) = handle.get_webview_window(WINDOW_LABEL) {
            let _ = win.close();
        }
    });
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MimoCookieCapture {
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

/// 读登录窗 Cookie；具备用量所需字段时 `ready=true`。
#[tauri::command]
pub async fn mimo_capture_login_cookies(app: AppHandle) -> Result<MimoCookieCapture, String> {
    let handle = app.clone();
    let (page_url, host, cookies) = tokio::task::spawn_blocking(move || -> Result<(String, String, Vec<(String, String, String)>), String> {
        let win = handle
            .get_webview_window(WINDOW_LABEL)
            .ok_or_else(|| "登录窗已关闭，请重新打开".to_string())?;
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

    let mut pairs: Vec<(String, String)> = Vec::new();
    for (name, value, domain) in &cookies {
        if name.is_empty() || value.is_empty() {
            continue;
        }
        if !domain_matches(&host, domain)
            && !domain.to_ascii_lowercase().contains("xiaomi")
            && !domain.to_ascii_lowercase().contains("xiaomimimo")
        {
            continue;
        }
        if let Some(existing) = pairs.iter_mut().find(|(n, _)| n == name) {
            existing.1 = value.clone();
        } else {
            pairs.push((name.clone(), value.clone()));
        }
    }
    let joined = pairs
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    let ready = normalize_cookie_header(&joined).is_ok();
    Ok(MimoCookieCapture {
        page_url,
        ready,
        cookie_header: joined,
        names: pairs.into_iter().map(|(n, _)| n).collect(),
    })
}

/// 从已打开的登录窗抓 Cookie → 验证用量 → 绑定到账号 → 关窗。
#[tauri::command]
pub async fn mimo_bind_login_session(app: AppHandle, state: tauri::State<'_, DbState>, id: i64) -> Result<MimoQuotaDto, String> {
    let capture = mimo_capture_login_cookies(app.clone()).await?;
    if !capture.ready {
        return Err("尚未登录完成：请在打开的窗口里登录小米账号，进入套餐/用量页后再点「完成」".to_string());
    }
    let dto = mimo_set_cookie(state, id, capture.cookie_header).await?;
    let _ = mimo_close_login_window(app).await;
    Ok(dto)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_usage_reads_month_items() {
        let body = r#"{
          "code": 0,
          "data": {
            "monthUsage": {
              "items": [
                { "name": "month_total_token", "used": 1200, "limit": 10000 }
              ]
            }
          }
        }"#;
        let (plan, windows) = parse_platform_usage(body).unwrap();
        assert_eq!(plan.as_deref(), Some("Token Plan"));
        assert_eq!(windows.len(), 1);
        assert!((windows[0].used - 1200.0).abs() < 0.01);
    }

    #[test]
    fn parse_usage_scales_plan_total_token() {
        let body = r#"{
          "code": 0,
          "data": {
            "usage": {
              "items": [
                { "name": "plan_total_token", "used": 4100000000, "limit": 11000000000 }
              ]
            }
          }
        }"#;
        let (_, windows) = parse_platform_usage(body).unwrap();
        assert_eq!(windows[0].label, "本月 Credits（亿）");
        assert!((windows[0].used - 41.0).abs() < 0.01);
        assert!((windows[0].limit - 110.0).abs() < 0.01);
    }

    #[test]
    fn normalize_cookie_requires_token_and_user() {
        assert!(normalize_cookie_header("foo=1").is_err());
        let ok = normalize_cookie_header(
            r#"api-platform_serviceToken="tok"; userId=123; api-platform_ph=p"#,
        )
        .unwrap();
        assert!(ok.contains("api-platform_serviceToken=tok"));
        assert!(ok.contains("userId=123"));
    }

    #[test]
    fn parse_balance_reads_cny_and_tokens() {
        let body = r#"{ "data": { "balance": "12.5", "granted_balance": "1.0", "plan_name": "PAYG" } }"#;
        let (plan, balances, windows) = parse_balance(body, MimoMode::Payg).unwrap();
        assert_eq!(plan.as_deref(), Some("PAYG"));
        assert!(windows.is_empty());
        assert!(balances.iter().any(|b| (b.amount - 12.5).abs() < 0.01));
    }

    #[test]
    fn token_plan_key_prefix() {
        assert!(is_token_plan_key("tp-abc"));
        assert!(!is_token_plan_key("sk-abc"));
    }

    #[test]
    fn parse_period_end_utc_string() {
        // 官方：有效期至 2026-11-07 23:59:59 (UTC)；API 常给到分钟
        let ms = parse_period_end_ms("2026-11-07 23:59 UTC").unwrap();
        assert_eq!(ms, chrono::DateTime::parse_from_rfc3339("2026-11-07T23:59:59Z").unwrap().timestamp_millis());
        assert_eq!(
            parse_period_end_ms("2026-11-07 23:59:59 UTC"),
            Some(ms)
        );
        let d = parse_plan_detail(
            r#"{"code":0,"data":{"planCode":"Standard","currentPeriodEnd":"2026-11-07 23:59 UTC","expired":false}}"#,
        )
        .unwrap();
        assert_eq!(d.plan.as_deref(), Some("Standard"));
        assert_eq!(d.period_end_ms, Some(ms));
        assert!(!d.expired);
    }
}
