//! Qoder（国内版 qoder.cn）额度：走 Qoder 客户端同款设备授权
//! （`/device/selectAccounts` + PKCE，轮询 `/api/v1/deviceToken/poll` 取令牌），
//! 令牌约 1 小时过期，用 `/api/v1/deviceToken/refresh` 续期后写回 `quota_accounts`；
//! 用量查 `/sash/api/v2/me/usage`。DTO 不含任何 token 字段。

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use crate::codex_quota::{http_client, now_unix, pkce_challenge, random_b64, with_db, REAUTH_MESSAGE};
use crate::quota_accounts::{
    arm_login_cancel, list_accounts, random_uuid, token_for, update_tokens, upsert_account, Provider,
    QuotaAccountDto,
};
use crate::DbState;

const AUTH_BASE: &str = "https://qoder.cn";
const OPENAPI_BASE: &str = "https://openapi.qoder.com.cn";
const CLIENT_ID: &str = "732aef47-9cf2-46a2-95fe-4cebb5d0d1fa";
const BIZ_VARIANT: &str = "qoder";
/// Qoder 桌面端的 `Cosy-ClientType`；usage 端点按它区分客户端。
const CLIENT_TYPE: &str = "10";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 上游不给过期时间时按 1 小时算（与 Qoder 客户端一致）。
const DEFAULT_TTL_SECS: i64 = 3600;
/// 离过期不到这么久就先续期，避免请求途中过期。
const REFRESH_LEEWAY_SECS: i64 = 120;
/// 上游用 9999-12-31 之类的哨兵表示「不过期」；2100 年之后一律当没有到期时间。
const NO_EXPIRY_AFTER_MS: i64 = 4_102_444_800_000;

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

pub fn qoder_login_url(challenge: &str, nonce: &str, machine_id: &str) -> String {
    let device = url::Url::parse_with_params(
        &format!("{AUTH_BASE}/device/selectAccounts"),
        &[
            ("challenge", challenge),
            ("challenge_method", "S256"),
            ("nonce", nonce),
            ("machine_id", machine_id),
            ("client_id", CLIENT_ID),
        ],
    )
    .map(|u| u.to_string())
    .unwrap_or_default();
    url::Url::parse_with_params(
        &format!("{AUTH_BASE}/users/sign-in"),
        &[("biz_variant", BIZ_VARIANT), ("oauth_callback", device.as_str())],
    )
    .map(|u| u.to_string())
    .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenPair {
    pub access: String,
    pub refresh: String,
    /// unix 秒。
    pub expires_at: i64,
}

fn str_field<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn num_field(v: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| {
        let x = v.get(*k)?;
        x.as_f64().or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

/// `expires_at`（ISO 时间或 epoch 秒/毫秒）优先，其次 `expires_in` 秒，都没有按 1 小时。
fn expiry_of(v: &serde_json::Value, now: i64) -> i64 {
    if let Some(x) = v.get("expires_at") {
        if let Some(s) = x.as_str() {
            if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s.trim()) {
                return t.timestamp();
            }
        }
        if let Some(n) = x.as_i64() {
            return if n > 1_000_000_000_000 { n / 1000 } else { n };
        }
    }
    match num_field(v, &["expires_in"]) {
        Some(secs) if secs > 0.0 => now + secs as i64,
        _ => now + DEFAULT_TTL_SECS,
    }
}

/// 轮询完成后的令牌；未完成时上游回 404，这里对缺字段的体也返回 None。
pub fn parse_poll_response(body: &str, now: i64) -> Option<TokenPair> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(TokenPair {
        access: str_field(&v, &["token"])?.to_string(),
        refresh: str_field(&v, &["refresh_token"])?.to_string(),
        expires_at: expiry_of(&v, now),
    })
}

pub fn parse_refresh_response(body: &str, now: i64) -> Option<TokenPair> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(TokenPair {
        access: str_field(&v, &["device_token", "token"])?.to_string(),
        refresh: str_field(&v, &["refresh_token"])?.to_string(),
        expires_at: expiry_of(&v, now),
    })
}

/// 上游有时包一层 `{ data: ... }`。
fn unwrap_data(v: serde_json::Value) -> serde_json::Value {
    match v.get("data") {
        Some(d) if d.is_object() => d.clone(),
        _ => v,
    }
}

/// userinfo → (主体 id, 展示名：邮箱优先，其次名字)。
pub fn parse_userinfo(body: &str) -> Option<(String, Option<String>)> {
    let v = unwrap_data(serde_json::from_str(body).ok()?);
    let id = ["id", "user_id", "uid"].iter().find_map(|k| {
        let x = v.get(*k)?;
        x.as_str().map(str::to_string).or_else(|| x.as_i64().map(|n| n.to_string()))
    })?;
    let label = str_field(&v, &["email"])
        .filter(|s| s.contains('@'))
        .or_else(|| str_field(&v, &["name", "nickname"]))
        .map(str::to_string);
    Some((id, label))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderQuota {
    pub total: f64,
    pub used: f64,
    pub remaining: f64,
    /// 已用百分比 0-100。
    pub pct: f64,
    pub unit: String,
}

fn parse_quota(v: Option<&serde_json::Value>, total_keys: &[&str]) -> Option<QoderQuota> {
    let o = v.filter(|x| x.is_object())?;
    let total = num_field(o, total_keys).unwrap_or(0.0).max(0.0);
    let used = num_field(o, &["used"]).unwrap_or(0.0).max(0.0);
    let pct = match num_field(o, &["percentage"]) {
        Some(p) if (0.0..=1.0).contains(&p) => p * 100.0,
        Some(p) => p,
        None if total > 0.0 => used / total * 100.0,
        None => 0.0,
    };
    let remaining = num_field(o, &["available"]).unwrap_or((total - used).max(0.0));
    Some(QoderQuota {
        total,
        used,
        remaining,
        pct,
        unit: str_field(o, &["unit"]).unwrap_or("credits").to_string(),
    })
}

fn pick<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a serde_json::Value> {
    keys.iter().find_map(|k| v.get(*k))
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct QoderUsage {
    pub user_type: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub upgrade_url: Option<String>,
    pub user: Option<QoderQuota>,
    pub add_on: Option<QoderQuota>,
    pub org: Option<QoderQuota>,
    /// 企业版：上游不给数字，只给详情页链接。
    pub enterprise_url: Option<String>,
}

pub fn parse_usage(body: &str) -> Option<QoderUsage> {
    let v = unwrap_data(serde_json::from_str(body).ok()?);
    let mode = str_field(&v, &["displayMode", "display_mode"]).unwrap_or("qoder");
    if mode == "enterprise" {
        let e = pick(&v, &["enterpriseUsage", "enterprise_usage"])?;
        let url = str_field(e, &["detailUrl", "detail_url"])
            .filter(|u| u.starts_with("https://"))
            .map(str::to_string);
        return Some(QoderUsage { enterprise_url: url, ..Default::default() });
    }
    let q = pick(&v, &["qoderUsage", "qoder_usage"]).filter(|x| x.is_object())?;
    let expires_at_ms = num_field(q, &["expiresAt", "expires_at"])
        .map(|n| n as i64)
        .filter(|n| *n > 0)
        .map(|n| if n < 1_000_000_000_000 { n * 1000 } else { n })
        .filter(|ms| *ms < NO_EXPIRY_AFTER_MS);
    Some(QoderUsage {
        user_type: str_field(q, &["userType", "user_type"]).map(str::to_string),
        expires_at_ms,
        upgrade_url: str_field(q, &["upgradeUrl", "upgrade_url"])
            .filter(|u| u.starts_with("https://"))
            .map(str::to_string),
        user: parse_quota(pick(q, &["userQuota", "user_quota"]), &["total"]),
        add_on: parse_quota(pick(q, &["addOnQuota", "add_on_quota"]), &["total"]),
        org: parse_quota(
            pick(q, &["orgResourcePackage", "org_resource_package", "sharedQuota", "shared_quota"]),
            &["cap", "total"],
        ),
        enterprise_url: None,
    })
}

/// 每日领积分活动（`CLAIM_BENEFIT` + `CREDITS`）的当前一轮。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderCampaign {
    pub campaign_id: String,
    pub claimable: bool,
    pub amount: f64,
    /// unix 秒；0 表示上游没给。
    pub end_at: i64,
    pub validity_days: Option<i64>,
}

/// 可领的优先；都已领时取结束最晚的一轮。
pub fn parse_campaigns(body: &str) -> Option<QoderCampaign> {
    let v = unwrap_data(serde_json::from_str(body).ok()?);
    let list = v.get("campaigns")?.as_array()?;
    let mut found: Vec<QoderCampaign> = list
        .iter()
        .filter(|c| str_field(c, &["actionType"]) == Some("CLAIM_BENEFIT"))
        .filter_map(|c| {
            let benefit = c.get("benefit")?;
            if str_field(benefit, &["kind"]) != Some("CREDITS") {
                return None;
            }
            let claimable = match str_field(c, &["claimStatus"])? {
                "CLAIMABLE" => true,
                "CLAIMED" => false,
                _ => return None,
            };
            Some(QoderCampaign {
                campaign_id: str_field(c, &["campaignId"])?.to_string(),
                claimable,
                amount: num_field(benefit, &["amount"]).unwrap_or(0.0),
                end_at: num_field(c, &["endAt"]).map(|n| n as i64).unwrap_or(0),
                validity_days: benefit
                    .get("validity")
                    .and_then(|x| num_field(x, &["days"]))
                    .map(|d| d as i64),
            })
        })
        .collect();
    found.sort_by_key(|c| (c.claimable, c.end_at));
    found.pop()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderClaimResult {
    pub ok: bool,
    pub error_code: Option<String>,
    pub message: Option<String>,
    /// 被风控拒绝：只带登录令牌领不了，需要回 Qoder 客户端领取。
    pub risk: bool,
    pub needs_reauth: bool,
}

impl QoderClaimResult {
    fn success() -> Self {
        Self { ok: true, error_code: None, message: None, risk: false, needs_reauth: false }
    }

    fn fail(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            ok: false,
            error_code: None,
            needs_reauth: message == REAUTH_MESSAGE,
            message: Some(message),
            risk: false,
        }
    }
}

/// 领取接口的非 2xx 响应；体形如 `{ errorCode, requestId }`。
pub fn parse_claim_error(status: u16, body: &str) -> QoderClaimResult {
    let code = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .map(unwrap_data)
        .and_then(|v| str_field(&v, &["errorCode", "error_code", "code"]).map(str::to_string));
    let Some(code) = code else {
        return QoderClaimResult::fail(format!("领取失败（HTTP {status}）"));
    };
    if code.contains("ALREADY") || code == "CLAIMED" {
        return QoderClaimResult::success();
    }
    let risk = code.starts_with("RISK_");
    let message = match code.as_str() {
        "RISK_BLOCKED" => "被风控拦截，需在 Qoder 客户端领取".to_string(),
        "RISK_DEPENDENCY_UNAVAILABLE" => "风控校验不可用，需在 Qoder 客户端领取".to_string(),
        "CAMPAIGN_NOT_ACTIVE" => "活动未开始或已结束".to_string(),
        "CAMPAIGN_NOT_FOUND" => "活动不存在".to_string(),
        "GRANT_RESULT_UNKNOWN" => "发放结果未知，稍后刷新确认".to_string(),
        "ACHIEVEMENT_NOT_COMPLETED" => "未满足领取条件".to_string(),
        "REDEMPTION_CODE_OUT_OF_STOCK" => "已领完".to_string(),
        other => format!("领取失败（{other}）"),
    };
    QoderClaimResult { ok: false, error_code: Some(code), message: Some(message), risk, needs_reauth: false }
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
    pub user_type: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub upgrade_url: Option<String>,
    pub user: Option<QoderQuota>,
    pub add_on: Option<QoderQuota>,
    pub org: Option<QoderQuota>,
    pub enterprise_url: Option<String>,
    pub campaign: Option<QoderCampaign>,
    pub needs_reauth: bool,
    pub at: i64,
}

impl QoderQuotaDto {
    fn err(id: i64, label: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            ok: false,
            needs_reauth: message == REAUTH_MESSAGE,
            message: Some(message),
            label,
            user_type: None,
            expires_at_ms: None,
            upgrade_url: None,
            user: None,
            add_on: None,
            org: None,
            enterprise_url: None,
            campaign: None,
            at: now_unix(),
        }
    }

    fn ok(id: i64, label: Option<String>, u: QoderUsage, campaign: Option<QoderCampaign>) -> Self {
        Self {
            id,
            ok: true,
            message: None,
            label,
            user_type: u.user_type,
            expires_at_ms: u.expires_at_ms,
            upgrade_url: u.upgrade_url,
            user: u.user,
            add_on: u.add_on,
            org: u.org,
            enterprise_url: u.enterprise_url,
            campaign,
            needs_reauth: false,
            at: now_unix(),
        }
    }
}

// ---------------------------------------------------------------------------
// 网络
// ---------------------------------------------------------------------------

enum RefreshError {
    Rejected,
    Transient(String),
}

async fn refresh_tokens(client: &reqwest::Client, refresh: &str) -> Result<TokenPair, RefreshError> {
    let resp = client
        .post(format!("{OPENAPI_BASE}/api/v1/deviceToken/refresh"))
        .header("Accept", "application/json")
        .json(&serde_json::json!({ "refresh_token": refresh }))
        .send()
        .await
        .map_err(|e| {
            RefreshError::Transient(crate::config::friendly_http_error(e, "续期请求失败"))
        })?;
    let status = resp.status().as_u16();
    if matches!(status, 400 | 401 | 403) {
        return Err(RefreshError::Rejected);
    }
    if !resp.status().is_success() {
        return Err(RefreshError::Transient(format!("续期返回 {status}")));
    }
    let body = resp.text().await.unwrap_or_default();
    parse_refresh_response(&body, now_unix()).ok_or(RefreshError::Transient("续期响应解析失败".into()))
}

/// 续期并写回；被拒时返回 `REAUTH_MESSAGE`。
async fn refresh_and_store(
    state: &DbState,
    client: &reqwest::Client,
    id: i64,
    refresh: &str,
) -> Result<String, String> {
    if refresh.is_empty() {
        return Err(REAUTH_MESSAGE.to_string());
    }
    let pair = match refresh_tokens(client, refresh).await {
        Ok(p) => p,
        Err(RefreshError::Rejected) => return Err(REAUTH_MESSAGE.to_string()),
        Err(RefreshError::Transient(m)) => return Err(m),
    };
    let access = pair.access.clone();
    with_db(Arc::clone(&state.conn), move |c| {
        update_tokens(c, id, &pair.access, &pair.refresh, pair.expires_at)
    })
    .await?;
    Ok(access)
}

enum UsageError {
    Unauthorized,
    Other(String),
}

async fn request_usage(client: &reqwest::Client, token: &str) -> Result<QoderUsage, UsageError> {
    let resp = client
        .get(format!("{OPENAPI_BASE}/sash/api/v2/me/usage"))
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("Cosy-ClientType", CLIENT_TYPE)
        .header("User-Agent", "Qoder")
        .send()
        .await
        .map_err(|e| UsageError::Other(crate::config::friendly_http_error(e, "请求失败")))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(UsageError::Unauthorized);
    }
    if !resp.status().is_success() {
        return Err(UsageError::Other(format!("上游返回 {status}")));
    }
    let body = resp.text().await.unwrap_or_default();
    parse_usage(&body).ok_or(UsageError::Other("用量响应解析失败".into()))
}

/// 活动查询失败不影响额度结果，一律当没有活动。
async fn request_campaigns(client: &reqwest::Client, token: &str) -> Option<QoderCampaign> {
    let resp = client
        .get(format!("{OPENAPI_BASE}/sash/api/v1/me/campaigns"))
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("Cosy-ClientType", CLIENT_TYPE)
        .header("User-Agent", "Qoder")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    parse_campaigns(&resp.text().await.ok()?)
}

async fn request_usage_and_campaigns(
    client: &reqwest::Client,
    token: &str,
) -> (Result<QoderUsage, UsageError>, Option<QoderCampaign>) {
    tokio::join!(request_usage(client, token), request_campaigns(client, token))
}

/// 读出账号令牌，临近过期先续期；返回 (标签, access token, 刷新令牌, 是否刚续期)。
async fn usable_token(
    state: &DbState,
    client: &reqwest::Client,
    id: i64,
) -> Result<(Option<String>, String, String, bool), (Option<String>, String)> {
    let stored = token_for(Arc::clone(&state.conn), id, Provider::Qoder)
        .await
        .map_err(|m| (None, m))?;
    let label = stored.label.clone();
    if stored.expires_at > 0 && stored.expires_at - now_unix() < REFRESH_LEEWAY_SECS {
        let token = refresh_and_store(state, client, id, &stored.refresh_token)
            .await
            .map_err(|m| (label.clone(), m))?;
        return Ok((label, token, stored.refresh_token, true));
    }
    Ok((label, stored.access_token, stored.refresh_token, false))
}

#[tauri::command]
pub async fn qoder_quota_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<QoderQuotaDto, String> {
    let client = http_client()?;
    let (label, token, refresh, refreshed) = match usable_token(&state, &client, id).await {
        Ok(t) => t,
        Err((label, m)) => return Ok(QoderQuotaDto::err(id, label, m)),
    };

    match request_usage_and_campaigns(&client, &token).await {
        (Ok(u), c) => Ok(QoderQuotaDto::ok(id, label, u, c)),
        (Err(UsageError::Other(m)), _) => Ok(QoderQuotaDto::err(id, label, m)),
        (Err(UsageError::Unauthorized), _) if refreshed => Ok(QoderQuotaDto::err(id, label, REAUTH_MESSAGE)),
        (Err(UsageError::Unauthorized), _) => {
            let token = match refresh_and_store(&state, &client, id, &refresh).await {
                Ok(t) => t,
                Err(m) => return Ok(QoderQuotaDto::err(id, label, m)),
            };
            Ok(match request_usage_and_campaigns(&client, &token).await {
                (Ok(u), c) => QoderQuotaDto::ok(id, label, u, c),
                (Err(UsageError::Unauthorized), _) => QoderQuotaDto::err(id, label, REAUTH_MESSAGE),
                (Err(UsageError::Other(m)), _) => QoderQuotaDto::err(id, label, m),
            })
        }
    }
}

enum ClaimOutcome {
    Done(QoderClaimResult),
    Unauthorized,
}

async fn request_claim(client: &reqwest::Client, token: &str, campaign_id: &str) -> ClaimOutcome {
    if campaign_id.is_empty() || !campaign_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return ClaimOutcome::Done(QoderClaimResult::fail("活动 ID 无效"));
    }
    let resp = client
        .post(format!("{OPENAPI_BASE}/sash/api/v1/me/campaigns/{campaign_id}/claim"))
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("Cosy-ClientType", CLIENT_TYPE)
        .header("User-Agent", "Qoder")
        .body("")
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            return ClaimOutcome::Done(QoderClaimResult::fail(crate::config::friendly_http_error(
                e,
                "请求失败",
            )))
        }
    };
    let status = resp.status().as_u16();
    if status == 401 {
        return ClaimOutcome::Unauthorized;
    }
    if resp.status().is_success() {
        return ClaimOutcome::Done(QoderClaimResult::success());
    }
    ClaimOutcome::Done(parse_claim_error(status, &resp.text().await.unwrap_or_default()))
}

/// 用户点击时直调一次领取；不重试（风控拒绝时反复请求只会更糟），失败由前端回退到 Qoder 客户端。
#[tauri::command]
pub async fn qoder_campaign_claim(
    state: tauri::State<'_, DbState>,
    id: i64,
    campaign_id: String,
) -> Result<QoderClaimResult, String> {
    let client = http_client()?;
    let (_, token, refresh, refreshed) = match usable_token(&state, &client, id).await {
        Ok(t) => t,
        Err((_, m)) => return Ok(QoderClaimResult::fail(m)),
    };
    match request_claim(&client, &token, &campaign_id).await {
        ClaimOutcome::Done(r) => Ok(r),
        ClaimOutcome::Unauthorized if refreshed => Ok(QoderClaimResult::fail(REAUTH_MESSAGE)),
        ClaimOutcome::Unauthorized => {
            let token = match refresh_and_store(&state, &client, id, &refresh).await {
                Ok(t) => t,
                Err(m) => return Ok(QoderClaimResult::fail(m)),
            };
            Ok(match request_claim(&client, &token, &campaign_id).await {
                ClaimOutcome::Done(r) => r,
                ClaimOutcome::Unauthorized => QoderClaimResult::fail(REAUTH_MESSAGE),
            })
        }
    }
}

const QODER_APP_NAME: &str = "Qoder CN";
const QODER_EXE: &str = "Qoder CN.exe";

/// 启动本机 Qoder CN；有可领活动时客户端会自己弹出领取页。
#[tauri::command]
pub async fn qoder_launch_app() -> Result<String, String> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(|| {
            let from_registry = crate::app_launcher_commands::find_install_dir(QODER_APP_NAME)
                .map(|dir| std::path::Path::new(&dir).join(QODER_EXE));
            let default = std::env::var_os("LOCALAPPDATA")
                .map(|d| std::path::PathBuf::from(d).join("Programs").join(QODER_APP_NAME).join(QODER_EXE));
            let exe = [from_registry, default]
                .into_iter()
                .flatten()
                .find(|p| p.is_file())
                .ok_or_else(|| "未找到本机 Qoder CN 客户端".to_string())?;
            crate::app_launcher_commands::launch_app_inner(exe.to_string_lossy().to_string(), None)
        })
        .await
        .map_err(|e| format!("启动任务失败: {e}"))?
    }
    #[cfg(not(windows))]
    {
        Err("当前系统不支持启动应用".into())
    }
}

async fn poll_login(client: &reqwest::Client, nonce: &str, verifier: &str) -> TokenPair {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let resp = client
            .get(format!("{OPENAPI_BASE}/api/v1/deviceToken/poll"))
            .query(&[("nonce", nonce), ("verifier", verifier), ("challenge_method", "S256")])
            .header("Accept", "application/json")
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        if !resp.status().is_success() {
            continue;
        }
        if let Some(pair) = parse_poll_response(&resp.text().await.unwrap_or_default(), now_unix()) {
            return pair;
        }
    }
}

async fn fetch_userinfo(client: &reqwest::Client, token: &str) -> Option<(String, Option<String>)> {
    let resp = client
        .get(format!("{OPENAPI_BASE}/api/v1/userinfo"))
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    parse_userinfo(&resp.text().await.ok()?)
}

/// 打开 Qoder 登录页并轮询结果；取消走 `cursor_oauth_cancel`（共用取消槽）。
#[tauri::command]
pub async fn qoder_oauth_start(state: tauri::State<'_, DbState>) -> Result<QuotaAccountDto, String> {
    let cancel_rx = arm_login_cancel();
    // 64 个 base64url 字符，都在 PKCE 的 unreserved 字符集内。
    let verifier = random_b64(48);
    let nonce = random_uuid();
    let machine_id = random_uuid();
    crate::cloudflared_commands::open_in_browser(qoder_login_url(&pkce_challenge(&verifier), &nonce, &machine_id))?;

    let client = http_client()?;
    let pair = tokio::select! {
        pair = poll_login(&client, &nonce, &verifier) => pair,
        _ = cancel_rx => return Err("已取消授权".to_string()),
        _ = tokio::time::sleep(LOGIN_TIMEOUT) => return Err("授权超时（5 分钟内未完成登录）".to_string()),
    };
    let (subject, label) = fetch_userinfo(&client, &pair.access).await.unwrap_or_default();

    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(c, Provider::Qoder, &subject, label.as_deref(), &pair.access, &pair.refresh, now_unix())?;
        update_tokens(c, id, &pair.access, &pair.refresh, pair.expires_at)?;
        list_accounts(c, Some(Provider::Qoder))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_url_wraps_device_flow_in_sign_in() {
        let u = url::Url::parse(&qoder_login_url("chal", "n1", "m1")).unwrap();
        assert_eq!(u.host_str(), Some("qoder.cn"));
        assert_eq!(u.path(), "/users/sign-in");
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["biz_variant"], "qoder");
        let inner = url::Url::parse(&q["oauth_callback"]).unwrap();
        assert_eq!(inner.path(), "/device/selectAccounts");
        let iq: std::collections::HashMap<_, _> = inner.query_pairs().into_owned().collect();
        assert_eq!(iq["challenge"], "chal");
        assert_eq!(iq["challenge_method"], "S256");
        assert_eq!(iq["nonce"], "n1");
        assert_eq!(iq["machine_id"], "m1");
        assert_eq!(iq["client_id"], CLIENT_ID);
    }

    #[test]
    fn poll_response_needs_both_tokens() {
        let p = parse_poll_response(r#"{"token":"a","refresh_token":"r","expires_in":7200}"#, 100).unwrap();
        assert_eq!(p, TokenPair { access: "a".into(), refresh: "r".into(), expires_at: 7300 });
        assert!(parse_poll_response(r#"{"token":"a"}"#, 0).is_none());
        assert!(parse_poll_response("", 0).is_none());
        let d = parse_poll_response(r#"{"token":"a","refresh_token":"r"}"#, 10).unwrap();
        assert_eq!(d.expires_at, 10 + DEFAULT_TTL_SECS);
    }

    #[test]
    fn refresh_response_accepts_device_token_and_iso_expiry() {
        let p = parse_refresh_response(
            r#"{"device_token":"d","refresh_token":"r2","expires_at":"2026-10-10T08:00:00Z"}"#,
            0,
        )
        .unwrap();
        assert_eq!(p.access, "d");
        assert_eq!(p.refresh, "r2");
        assert_eq!(p.expires_at, 1_791_619_200);
        assert_eq!(parse_refresh_response(r#"{"token":"t","refresh_token":"r"}"#, 0).unwrap().access, "t");
    }

    #[test]
    fn userinfo_prefers_email_and_tolerates_data_wrapper() {
        assert_eq!(
            parse_userinfo(r#"{"data":{"uid":42,"name":"n","email":"a@b.cn"}}"#),
            Some(("42".into(), Some("a@b.cn".into())))
        );
        assert_eq!(parse_userinfo(r#"{"id":"u1","name":"Neo"}"#), Some(("u1".into(), Some("Neo".into()))));
        assert!(parse_userinfo(r#"{"name":"x"}"#).is_none());
    }

    #[test]
    fn usage_reads_personal_quota_camel_case() {
        let body = r#"{"displayMode":"qoder","qoderUsage":{"userType":"pro","expiresAt":1793609325000,"upgradeUrl":"https://qoder.cn/pricing","userQuota":{"total":2000,"used":500,"percentage":0.25,"unit":"credits"},"addOnQuota":{"total":300,"used":0}}}"#;
        let u = parse_usage(body).unwrap();
        assert_eq!(u.user_type.as_deref(), Some("pro"));
        assert_eq!(u.expires_at_ms, Some(1_793_609_325_000));
        let q = u.user.unwrap();
        assert_eq!((q.total, q.used, q.remaining, q.pct), (2000.0, 500.0, 1500.0, 25.0));
        assert_eq!(u.add_on.unwrap().remaining, 300.0);
        assert!(u.org.is_none());
        assert!(u.enterprise_url.is_none());
    }

    #[test]
    fn usage_reads_snake_case_and_org_cap() {
        let body = r#"{"display_mode":"qoder","qoder_usage":{"user_type":"free","expires_at":1793609325,"user_quota":{"total":100,"used":40},"org_resource_package":{"used":10,"cap":50,"available":40}}}"#;
        let u = parse_usage(body).unwrap();
        assert_eq!(u.expires_at_ms, Some(1_793_609_325_000));
        let q = u.user.unwrap();
        assert_eq!(q.pct, 40.0);
        assert_eq!(q.unit, "credits");
        let o = u.org.unwrap();
        assert_eq!((o.total, o.used, o.remaining), (50.0, 10.0, 40.0));
    }

    #[test]
    fn usage_treats_far_future_expiry_as_none() {
        let body = r#"{"displayMode":"qoder","qoderUsage":{"userType":"personal_standard","expiresAt":253402271999000,"userQuota":{"total":0,"used":0},"addOnQuota":{"total":1700,"used":1066}}}"#;
        let u = parse_usage(body).unwrap();
        assert!(u.expires_at_ms.is_none());
        assert_eq!(u.add_on.unwrap().remaining, 634.0);
    }

    #[test]
    fn usage_enterprise_only_keeps_https_link() {
        let u = parse_usage(r#"{"displayMode":"enterprise","enterpriseUsage":{"openMode":"externalBrowser","detailUrl":"https://e.qoder.cn/usage"}}"#).unwrap();
        assert_eq!(u.enterprise_url.as_deref(), Some("https://e.qoder.cn/usage"));
        assert!(u.user.is_none());
        let deep = parse_usage(r#"{"displayMode":"enterprise","enterpriseUsage":{"openMode":"systemDeepLink","detailUrl":"dingtalk://x"}}"#).unwrap();
        assert!(deep.enterprise_url.is_none());
        assert!(parse_usage("nope").is_none());
        assert!(parse_usage(r#"{"displayMode":"qoder"}"#).is_none());
    }

    const CAMPAIGNS: &str = r#"{"uid":"u","showCampaign":true,"claimable":true,"campaigns":[
        {"campaignId":"old","actionType":"CLAIM_BENEFIT","endAt":1790000000,"claimStatus":"CLAIMED","benefit":{"kind":"CREDITS","amount":100}},
        {"campaignId":"c1","actionType":"CLAIM_BENEFIT","startAt":1791600000,"endAt":1791686340,"claimStatus":"CLAIMABLE","benefit":{"kind":"CREDITS","amount":100,"validity":{"mode":"RELATIVE_DAYS","days":30}}},
        {"campaignId":"v1","actionType":"VIEW_DETAILS","endAt":1799999999,"claimStatus":"CLAIMABLE"}]}"#;

    #[test]
    fn campaigns_prefer_claimable_credit_benefit() {
        let c = parse_campaigns(CAMPAIGNS).unwrap();
        assert_eq!(
            c,
            QoderCampaign { campaign_id: "c1".into(), claimable: true, amount: 100.0, end_at: 1_791_686_340, validity_days: Some(30) }
        );
    }

    #[test]
    fn campaigns_fall_back_to_latest_claimed() {
        let body = CAMPAIGNS.replace(r#""CLAIMABLE","benefit""#, r#""CLAIMED","benefit""#);
        let c = parse_campaigns(&body).unwrap();
        assert_eq!(c.campaign_id, "c1");
        assert!(!c.claimable);
    }

    #[test]
    fn campaigns_ignore_non_credit_and_bad_bodies() {
        let only_view = r#"{"campaigns":[{"campaignId":"v","actionType":"VIEW_DETAILS","claimStatus":"CLAIMABLE"},
            {"campaignId":"r","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMABLE","benefit":{"kind":"REDEMPTION_CODE"}}]}"#;
        assert!(parse_campaigns(only_view).is_none());
        assert!(parse_campaigns(r#"{"campaigns":[]}"#).is_none());
        assert!(parse_campaigns(r#"{"showCampaign":false}"#).is_none());
        assert!(parse_campaigns("<html>").is_none());
    }

    #[test]
    fn claim_error_flags_risk_codes() {
        let r = parse_claim_error(403, r#"{"errorCode":"RISK_BLOCKED","requestId":"x"}"#);
        assert!(!r.ok && r.risk);
        assert_eq!(r.error_code.as_deref(), Some("RISK_BLOCKED"));
        assert!(parse_claim_error(503, r#"{"errorCode":"RISK_DEPENDENCY_UNAVAILABLE"}"#).risk);

        let n = parse_claim_error(409, r#"{"data":{"errorCode":"CAMPAIGN_NOT_ACTIVE"}}"#);
        assert!(!n.ok && !n.risk);
        assert_eq!(n.message.as_deref(), Some("活动未开始或已结束"));
    }

    #[test]
    fn claim_error_handles_already_claimed_and_plain_bodies() {
        assert!(parse_claim_error(409, r#"{"errorCode":"ALREADY_CLAIMED"}"#).ok);
        let p = parse_claim_error(502, "Bad Gateway");
        assert!(!p.ok && !p.risk && p.error_code.is_none());
        assert_eq!(p.message.as_deref(), Some("领取失败（HTTP 502）"));
    }
}
