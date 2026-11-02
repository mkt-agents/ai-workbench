//! 自定义中转站额度：粘贴 API Key + 接口地址，逐个探测站点实际开放的余额接口。
//!
//! 这套接口约定来自 one-api / new-api 及其衍生站（sub2api、metapi 等多数商业中转站
//! 都套壳自它们），不同部署开的东西不一样，所以按「能拿到金额的最准接口 → 只能拿到
//! 额度单位的接口 → 什么都拿不到就只验 Key」的顺序探测，命中即停：
//!
//! 1. `GET {v1}/dashboard/billing/subscription` + `/usage`：OpenAI 兼容计费，直接是金额
//! 2. `GET {root}/api/usage/token`：one-api / new-api 令牌额度（额度单位，按
//!    `GET {root}/api/status` 的 `quota_per_unit` 折算，缺省 1 USD = 500000）
//! 3. `GET {root}/api/user/self`：用户级额度（系统访问令牌，部分站点也认 sk-）
//! 4. `GET {v1}/models`：只验 Key 与地址，站点不提供余额接口
//!
//! 401 / 403 统一映射成「API Key 无效」（前端据此给出更换 Key），404 / 解析失败只当
//! 该端点不存在，继续下一个。Key 与接口地址只存本机 SQLite 的 `quota_accounts`，DTO
//! 不含 Key。

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use crate::codex_quota::{http_client, now_unix, with_db};
use crate::quota_accounts::{mask_api_key, token_for, upsert_account_at, Provider, QuotaAccountDto};
use crate::DbState;

const INVALID_KEY: &str = "API Key 无效或已失效，请检查后重试";
/// one-api 默认 1 美元 = 500000 额度；站点在 `/api/status` 里给了实际值就以它为准。
const DEFAULT_QUOTA_PER_UNIT: f64 = 500_000.0;
/// one-api 系的用量接口要日期区间；给一个足够宽的固定窗口（实现上都只按令牌累计）。
const USAGE_RANGE: &str = "start_date=2020-01-01&end_date=2099-12-31";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomBalance {
    pub label: String,
    pub amount: f64,
    /// `USD` / `CNY`；额度单位换算不出来时是空串。
    pub currency: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
    /// 归一化后的接口地址（展示用，不含 Key）。
    pub base_url: String,
    /// 命中的接口族标签，如 `OpenAI 兼容计费` / `new-api`。
    pub source: Option<String>,
    pub balances: Vec<CustomBalance>,
    /// 查到了、但没有余额数字时的说明（如「该站点没有开放余额接口」）。
    pub note: Option<String>,
    pub needs_reauth: bool,
    pub at: i64,
}

impl CustomQuotaDto {
    fn err(id: i64, label: Option<String>, base_url: String, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            ok: false,
            needs_reauth: message == INVALID_KEY,
            message: Some(message),
            label,
            base_url,
            source: None,
            balances: Vec::new(),
            note: None,
            at: now_unix(),
        }
    }
}

// ---------------------------------------------------------------------------
// 接口地址归一化
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct BaseUrls {
    /// 用户填的地址（去尾斜杠、补 scheme）——当去重主体的一部分。
    pub base: String,
    /// 拼 `/models`、`/dashboard/billing/*` 用。
    pub v1: String,
    /// 拼 `/api/usage/token`、`/api/user/self` 用（去掉末尾的 `/v1`）。
    pub root: String,
    pub host: String,
}

/// `api.example.com`、`https://api.example.com/`、`https://api.example.com/v1/` 都认。
pub fn normalize_base(raw: &str) -> Result<BaseUrls, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("请填写接口地址".to_string());
    }
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let url = url::Url::parse(&with_scheme).map_err(|_| "接口地址不是合法的网址".to_string())?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err("接口地址只支持 http / https".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "接口地址缺少域名".to_string())?
        .to_ascii_lowercase();

    let mut base = format!("{scheme}://{host}");
    if let Some(port) = url.port() {
        base.push_str(&format!(":{port}"));
    }
    let path = url.path().trim_end_matches('/');
    if !path.is_empty() && path != "/" {
        base.push_str(path);
    }

    let v1 = if base.ends_with("/v1") {
        base.clone()
    } else {
        format!("{base}/v1")
    };
    let root = base.strip_suffix("/v1").unwrap_or(base.as_str()).to_string();
    Ok(BaseUrls { base, v1, root, host })
}

// ---------------------------------------------------------------------------
// 响应解析（纯函数，单测覆盖）
// ---------------------------------------------------------------------------

fn num_field(v: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(n) = v.get(*k).and_then(|x| x.as_f64()) {
            return Some(n);
        }
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            if let Ok(n) = s.trim().parse::<f64>() {
                return Some(n);
            }
        }
    }
    None
}

/// OpenAI 兼容计费：`{"hard_limit_usd":100.0,...}`；有的部署只给 system_hard_limit_usd。
pub fn parse_billing_subscription(body: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(body).ok()?;
    num_field(&v, &["hard_limit_usd", "system_hard_limit_usd"]).filter(|n| *n > 0.0)
}

/// `{"total_usage":1234}` 的单位是美分。
pub fn parse_billing_usage_usd(body: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(body).ok()?;
    num_field(&v, &["total_usage"]).map(|cents| cents / 100.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenQuota {
    pub granted: f64,
    pub used: f64,
    pub available: f64,
    pub unlimited: bool,
}

/// one-api / new-api：`{success,data:{total_granted,total_used,total_available,unlimited_quota}}`。
pub fn parse_token_usage(body: &str) -> Option<TokenQuota> {
    let v: Value = serde_json::from_str(body).ok()?;
    let data = v.get("data")?;
    let unlimited = data
        .get("unlimited_quota")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let available = num_field(data, &["total_available", "remain_quota", "quota"]);
    let granted = num_field(data, &["total_granted", "quota"]);
    let used = num_field(data, &["total_used", "used_quota"]).unwrap_or(0.0);
    if !unlimited && available.is_none() && granted.is_none() {
        return None;
    }
    Some(TokenQuota {
        granted: granted.unwrap_or(0.0),
        used,
        available: available.unwrap_or_else(|| (granted.unwrap_or(0.0) - used).max(0.0)),
        unlimited,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserQuota {
    pub remaining: f64,
    pub used: f64,
}

/// one-api / new-api 用户信息：`{success,data:{quota,used_quota,...}}`（额度单位）。
pub fn parse_user_self(body: &str) -> Option<UserQuota> {
    let v: Value = serde_json::from_str(body).ok()?;
    let data = v.get("data")?;
    let remaining = num_field(data, &["quota", "remain_quota"])?;
    Some(UserQuota {
        remaining,
        used: num_field(data, &["used_quota"]).unwrap_or(0.0),
    })
}

/// `/api/status` 里的额度换算比（1 美元 = 多少额度）。
pub fn parse_quota_per_unit(body: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(body).ok()?;
    let data = v.get("data").unwrap_or(&v);
    num_field(data, &["quota_per_unit", "quotaPerUnit"]).filter(|n| *n > 0.0)
}

// ---------------------------------------------------------------------------
// 探测
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct CustomQuotaBody {
    pub source: String,
    pub balances: Vec<CustomBalance>,
    pub note: Option<String>,
}

enum Attempt {
    Hit(CustomQuotaBody),
    /// 401 / 403：Key 或地址权限不对。
    Auth,
    /// 404 / 非 JSON / 字段对不上：这个端点在这个站点不存在。
    Miss,
}

async fn http_get(client: &reqwest::Client, url: &str, key: &str) -> Result<(u16, String), String> {
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| crate::config::friendly_http_error(e, "请求失败"))?;
    let status = resp.status().as_u16();
    Ok((status, resp.text().await.unwrap_or_default()))
}

fn money(label: &str, amount: f64) -> CustomBalance {
    CustomBalance {
        label: label.to_string(),
        amount: (amount * 100.0).round() / 100.0,
        currency: "USD".to_string(),
    }
}

async fn try_billing(client: &reqwest::Client, urls: &BaseUrls, key: &str) -> Result<Attempt, String> {
    let (status, body) = http_get(
        client,
        &format!("{}/dashboard/billing/subscription", urls.v1),
        key,
    )
    .await?;
    if matches!(status, 401 | 403) {
        return Ok(Attempt::Auth);
    }
    if !(200..300).contains(&status) {
        return Ok(Attempt::Miss);
    }
    let Some(hard) = parse_billing_subscription(&body) else {
        return Ok(Attempt::Miss);
    };
    let used = match http_get(
        client,
        &format!("{}/dashboard/billing/usage?{USAGE_RANGE}", urls.v1),
        key,
    )
    .await
    {
        Ok((s, b)) if (200..300).contains(&s) => parse_billing_usage_usd(&b).unwrap_or(0.0),
        _ => 0.0,
    };
    let remaining = (hard - used).max(0.0);
    Ok(Attempt::Hit(CustomQuotaBody {
        source: "OpenAI 兼容计费".to_string(),
        balances: vec![
            money("账户余额", remaining),
            money("已用", used),
            money("总额度", hard),
        ],
        note: None,
    }))
}

/// 额度单位 → 美元；站点没给换算比就按 one-api 默认值。
async fn quota_unit(client: &reqwest::Client, urls: &BaseUrls, key: &str) -> f64 {
    match http_get(client, &format!("{}/api/status", urls.root), key).await {
        Ok((s, b)) if (200..300).contains(&s) => {
            parse_quota_per_unit(&b).unwrap_or(DEFAULT_QUOTA_PER_UNIT)
        }
        _ => DEFAULT_QUOTA_PER_UNIT,
    }
}

async fn try_token_usage(client: &reqwest::Client, urls: &BaseUrls, key: &str) -> Result<Attempt, String> {
    let (status, body) = http_get(client, &format!("{}/api/usage/token", urls.root), key).await?;
    if matches!(status, 401 | 403) {
        return Ok(Attempt::Auth);
    }
    if !(200..300).contains(&status) {
        return Ok(Attempt::Miss);
    }
    let Some(q) = parse_token_usage(&body) else {
        return Ok(Attempt::Miss);
    };
    if q.unlimited {
        return Ok(Attempt::Hit(CustomQuotaBody {
            source: "new-api".to_string(),
            balances: Vec::new(),
            note: Some("该 Key 的额度不限".to_string()),
        }));
    }
    let unit = quota_unit(client, urls, key).await;
    let mut balances = vec![money("账户余额", q.available / unit), money("已用", q.used / unit)];
    if q.granted > 0.0 {
        balances.push(money("总额度", q.granted / unit));
    }
    Ok(Attempt::Hit(CustomQuotaBody {
        source: "new-api".to_string(),
        balances,
        note: None,
    }))
}

async fn try_user_self(client: &reqwest::Client, urls: &BaseUrls, key: &str) -> Result<Attempt, String> {
    let (status, body) = http_get(client, &format!("{}/api/user/self", urls.root), key).await?;
    if matches!(status, 401 | 403) {
        return Ok(Attempt::Auth);
    }
    if !(200..300).contains(&status) {
        return Ok(Attempt::Miss);
    }
    let Some(q) = parse_user_self(&body) else {
        return Ok(Attempt::Miss);
    };
    let unit = quota_unit(client, urls, key).await;
    let mut balances = vec![
        money("账户余额", q.remaining / unit),
        money("已用", q.used / unit),
    ];
    if q.used > 0.0 {
        balances.push(money("总额度", (q.remaining + q.used) / unit));
    }
    Ok(Attempt::Hit(CustomQuotaBody {
        source: "one-api".to_string(),
        balances,
        note: None,
    }))
}

async fn try_models(client: &reqwest::Client, urls: &BaseUrls, key: &str) -> Result<Attempt, String> {
    let (status, body) = http_get(client, &format!("{}/models", urls.v1), key).await?;
    if matches!(status, 401 | 403) {
        return Ok(Attempt::Auth);
    }
    if !(200..300).contains(&status) {
        return Ok(Attempt::Miss);
    }
    // 前面几关都没结果，这一关必须是真接口：有的站点对未知路径回落到首页 HTML，那也是 200。
    if serde_json::from_str::<Value>(&body).is_err() {
        return Ok(Attempt::Miss);
    }
    Ok(Attempt::Hit(CustomQuotaBody {
        source: "OpenAI 兼容".to_string(),
        balances: Vec::new(),
        note: Some("该站点没有开放余额接口，只确认了 Key 可用".to_string()),
    }))
}

/// 探测顺序见模块头注释；全部落空时用 `/v1/models` 区分「Key 无效」与「地址不对」。
pub async fn probe(client: &reqwest::Client, urls: &BaseUrls, key: &str) -> Result<CustomQuotaBody, String> {
    let mut auth_seen = false;
    // 逐个探测、命中即停：站点通常只开其中一种，不必每次都把三个接口打完。
    for step in 0..3 {
        let attempt = match step {
            0 => try_billing(client, urls, key).await?,
            1 => try_token_usage(client, urls, key).await?,
            _ => try_user_self(client, urls, key).await?,
        };
        match attempt {
            Attempt::Hit(body) => return Ok(body),
            Attempt::Auth => auth_seen = true,
            Attempt::Miss => {}
        }
    }
    match try_models(client, urls, key).await? {
        Attempt::Hit(body) => Ok(body),
        Attempt::Auth => Err(INVALID_KEY.to_string()),
        Attempt::Miss if auth_seen => Err(INVALID_KEY.to_string()),
        Attempt::Miss => Err(format!(
            "{} 上没有找到可用的接口：确认地址是站点根地址（如 https://api.example.com）",
            urls.base
        )),
    }
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

/// 先探测一次（地址 + Key 都对了才入库），label 留空时用域名当展示名。
#[tauri::command]
pub async fn custom_add_key(
    state: tauri::State<'_, DbState>,
    api_key: String,
    base_url: String,
    label: Option<String>,
) -> Result<QuotaAccountDto, String> {
    let key = api_key.trim().to_string();
    if key.is_empty() {
        return Err("请填写 API Key".to_string());
    }
    let urls = normalize_base(&base_url)?;
    let label = label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let display = label.unwrap_or_else(|| urls.host.clone());
    probe(&http_client()?, &urls, &key).await?;

    let masked = mask_api_key(&key);
    // 同一个 Key 在不同站点是不同账号，主体里带上地址。
    let subject = format!("{masked}@{}", urls.base);
    let base = urls.base;
    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account_at(
            c,
            Provider::Custom,
            &subject,
            Some(display.as_str()),
            &key,
            "",
            &base,
            now_unix(),
        )?;
        crate::quota_accounts::list_accounts(c, Some(Provider::Custom))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

#[tauri::command]
pub async fn custom_quota_fetch(
    state: tauri::State<'_, DbState>,
    id: i64,
) -> Result<CustomQuotaDto, String> {
    let stored = match token_for(Arc::clone(&state.conn), id, Provider::Custom).await {
        Ok(t) => t,
        Err(m) => return Ok(CustomQuotaDto::err(id, None, String::new(), m)),
    };
    let urls = match normalize_base(&stored.base_url) {
        Ok(u) => u,
        Err(m) => return Ok(CustomQuotaDto::err(id, stored.label, stored.base_url, m)),
    };
    match probe(&http_client()?, &urls, &stored.access_token).await {
        Ok(body) => Ok(CustomQuotaDto {
            id,
            ok: true,
            message: None,
            label: stored.label,
            base_url: urls.base,
            source: Some(body.source),
            balances: body.balances,
            note: body.note,
            needs_reauth: false,
            at: now_unix(),
        }),
        Err(m) => Ok(CustomQuotaDto::err(id, stored.label, urls.base, m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_base_adds_scheme_and_strips_slash() {
        let u = normalize_base("  api.example.com/  ").unwrap();
        assert_eq!(u.base, "https://api.example.com");
        assert_eq!(u.v1, "https://api.example.com/v1");
        assert_eq!(u.root, "https://api.example.com");
        assert_eq!(u.host, "api.example.com");
    }

    #[test]
    fn normalize_base_keeps_v1_and_port() {
        let u = normalize_base("http://127.0.0.1:3000/v1/").unwrap();
        assert_eq!(u.base, "http://127.0.0.1:3000/v1");
        assert_eq!(u.v1, "http://127.0.0.1:3000/v1");
        assert_eq!(u.root, "http://127.0.0.1:3000");

        let u = normalize_base("https://relay.example.com/openai/v1").unwrap();
        assert_eq!(u.root, "https://relay.example.com/openai");
        assert_eq!(u.v1, "https://relay.example.com/openai/v1");
    }

    #[test]
    fn normalize_base_rejects_junk() {
        assert!(normalize_base("   ").is_err());
        assert!(normalize_base("ftp://a.b").is_err());
        assert!(normalize_base("not a host").is_err());
    }

    #[test]
    fn parse_billing_pair() {
        let sub = r#"{"object":"billing_subscription","hard_limit_usd":100.0,"soft_limit_usd":80.0}"#;
        assert_eq!(parse_billing_subscription(sub), Some(100.0));
        // 只给 system_hard_limit_usd 的部署
        assert_eq!(parse_billing_subscription(r#"{"system_hard_limit_usd":20}"#), Some(20.0));
        assert_eq!(parse_billing_subscription(r#"{"hard_limit_usd":0}"#), None);
        assert_eq!(parse_billing_usage_usd(r#"{"object":"list","total_usage":1234.5}"#), Some(12.345));
        assert_eq!(parse_billing_usage_usd("{}"), None);
    }

    #[test]
    fn parse_newapi_token_usage() {
        let body = r#"{"code":true,"message":"","data":{"object":"token_usage","name":"default","total_granted":500000,"total_used":1000,"total_available":499000,"unlimited_quota":false,"expires_at":-1}}"#;
        assert_eq!(
            parse_token_usage(body),
            Some(TokenQuota { granted: 500000.0, used: 1000.0, available: 499000.0, unlimited: false })
        );
        assert_eq!(
            parse_token_usage(r#"{"data":{"unlimited_quota":true}}"#),
            Some(TokenQuota { granted: 0.0, used: 0.0, available: 0.0, unlimited: true })
        );
        assert!(parse_token_usage(r#"{"success":false,"message":"无权进行此操作"}"#).is_none());
        // 没有 data 包装、字段名是老版的 remain_quota 也认
        assert_eq!(
            parse_token_usage(r#"{"data":{"remain_quota":100,"used_quota":50}}"#),
            Some(TokenQuota { granted: 0.0, used: 50.0, available: 100.0, unlimited: false })
        );
    }

    #[test]
    fn parse_newapi_user_self() {
        let body = r#"{"success":true,"data":{"id":1,"username":"demo","quota":499000,"used_quota":1000,"request_count":7}}"#;
        assert_eq!(parse_user_self(body), Some(UserQuota { remaining: 499000.0, used: 1000.0 }));
        assert!(parse_user_self(r#"{"success":false,"message":"无权进行此操作"}"#).is_none());
    }

    #[test]
    fn parse_status_quota_per_unit() {
        assert_eq!(parse_quota_per_unit(r#"{"success":true,"data":{"quota_per_unit":200000}}"#), Some(200000.0));
        assert_eq!(parse_quota_per_unit(r#"{"quota_per_unit":"500000"}"#), Some(500000.0));
        assert_eq!(parse_quota_per_unit(r#"{"data":{"quota_per_unit":0}}"#), None);
    }

    #[test]
    fn invalid_key_flags_needs_reauth() {
        assert!(CustomQuotaDto::err(1, None, "https://a.b".into(), INVALID_KEY).needs_reauth);
        assert!(!CustomQuotaDto::err(1, None, "https://a.b".into(), "请求失败").needs_reauth);
    }

    #[test]
    fn dto_never_serializes_key() {
        let dto = CustomQuotaDto {
            id: 1,
            ok: true,
            message: None,
            label: Some("relay.example.com".into()),
            base_url: "https://relay.example.com".into(),
            source: Some("new-api".into()),
            balances: vec![money("账户余额", 9.5)],
            note: None,
            needs_reauth: false,
            at: 1,
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("sk-"), "{json}");
        assert!(json.contains("\"baseUrl\":\"https://relay.example.com\""), "{json}");
        // 金额保留两位
        assert!(json.contains("\"amount\":9.5"), "{json}");
    }
}
