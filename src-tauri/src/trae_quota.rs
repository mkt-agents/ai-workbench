//! Trae CN（trae.cn）额度与每日签到：
//! - 账号来自粘贴 refreshToken（形如 `xxx=.18dxxx`，从 Trae 客户端提取），subject 存 JWT uid，
//!   refreshToken 只存本机 `quota_accounts.refresh_token`
//! - accessToken 过期（JWT exp）或上游 401 时用 `ExchangeToken` 续期；
//!   新版接口（20405）要求设备证明（ECDSA P-256 签名），优先走 v3
//! - 用量查 `POST /trae/api/v2/pay/user_current_entitlement_list`（usage_summary）
//! - 签到查 `POST /trae/api/v2/ug/checkin_credits/status`，领 `…/claim`（req_source=1）
//!
//! 参考：Trae 客户端同款请求（ClientID `ono9krqynydwx5` 桌面端 / `en1oxy7wnw8j9n` 旧客户端）。

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};

use crate::codex_quota::{http_client, now_unix, with_db};
use crate::quota_accounts::{token_for, update_tokens, upsert_account, Provider, QuotaAccountDto};
use crate::DbState;

const EP_EXCHANGE: &str = "https://api.trae.com.cn/cloudide/api/v3/trae/oauth/ExchangeToken";
const EP_EXCHANGE_V3: &str = "https://api.trae.cn/trae/api/v3/oauth/ExchangeToken";
const EP_ENTITLE: &str = "https://api.trae.cn/trae/api/v2/pay/user_current_entitlement_list";
const EP_CHECKIN_STATUS: &str = "https://api.trae.cn/trae/api/v2/ug/checkin_credits/status";
const EP_CHECKIN_CLAIM: &str = "https://api.trae.cn/trae/api/v2/ug/checkin_credits/claim";

/// 桌面端实测 ClientID（新版 v3 续期必须）。
const CLIENT_ID_IDE: &str = "ono9krqynydwx5";
/// 旧客户端 ClientID（v2 续期 / 兜底）。
const CLIENT_ID_LEGACY: &str = "en1oxy7wnw8j9n";
/// 客户端签到请求体固定带 `{"req_source":1}`，空体会被拒成 9074。
const REQ_BODY: &str = r#"{"req_source": 1}"#;

/// 业务码：今日已签。
const ALREADY_CODE: i64 = 9095;
/// 业务码：参与用户太多（排队限流，可重试）；当前签到实现不自动重试，保留供排障参考。
#[allow(dead_code)]
const RATE_CODE: i64 = 9074;

const REAUTH_MESSAGE: &str = "refreshToken 已失效或被轮换，请重新提取并更换";

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

/// JWT `data.id`（Trae 的用户 id 声明在 `data` 里），取不到返回空串。
pub fn jwt_uid(token: &str) -> String {
    jwt_payload(token)
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .unwrap_or_default()
}

fn jwt_payload(token: &str) -> Value {
    let Some((_, payload)) = token.split_once('.') else {
        return Value::Null;
    };
    // 取「头.体.签名」三段式的第二段：签名段可能也带 '.'（如 refreshToken 的 `.18dxxx`），
    // 所以先按 '.' 切开取 index 1；再剥掉已有 padding 统一补齐。
    let seg = payload.split('.').next().unwrap_or_default();
    let raw = seg.trim_end_matches('=');
    let padded = match raw.len() % 4 {
        2 => format!("{raw}=="),
        3 => format!("{raw}="),
        _ => raw.to_string(),
    };
    base64::engine::general_purpose::URL_SAFE
        .decode(padded)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// JWT exp（unix 秒），解析不了返回 0（视为需要续期）。
pub fn jwt_exp(token: &str) -> i64 {
    jwt_payload(token)
        .get("exp")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

/// refreshToken 形如 `BASE64=.18dxxx`；提取句点前的主体部分做去重键。
pub fn refresh_subject(refresh_token: &str) -> String {
    let rt = refresh_token.trim();
    let head = rt.split('.').next().unwrap_or(rt);
    if head.len() <= 8 {
        return String::new();
    }
    let head: String = head.chars().take(4).collect();
    let rt_chars: Vec<char> = rt.chars().collect();
    let tail: String = rt_chars[rt_chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraeWindow {
    /// 套餐名（Lite / Pro / …）。
    pub plan: String,
    pub used: f64,
    pub limit: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraeQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
    /// 积分余额（签到 status 里的 credits）。
    pub credits: Option<f64>,
    /// 套餐用量窗口（entitlement_list）。
    pub windows: Vec<TraeWindow>,
    /// 今日是否已签到（查询成功时才有意义）。
    pub checked_in: Option<bool>,
    /// 今日签到成功的本地时刻（unix 秒）；未签 / 跨天为 None。
    pub checked_in_at: Option<i64>,
    /// 签到功能是否开放。
    pub checkin_enabled: Option<bool>,
    pub needs_reauth: bool,
    pub at: i64,
}

impl TraeQuotaDto {
    fn err(id: i64, label: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        let needs_reauth = message == REAUTH_MESSAGE || message.contains("重新提取");
        Self {
            id,
            ok: false,
            needs_reauth,
            message: Some(message),
            label,
            credits: None,
            windows: Vec::new(),
            checked_in: None,
            checked_in_at: None,
            checkin_enabled: None,
            at: now_unix(),
        }
    }
}

/// entitlement_list → 套餐用量窗口。容多条：取带 usage 的主套餐。
pub fn parse_entitlement(body: &str) -> Vec<TraeWindow> {
    let Ok(root) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let data = root.get("data").filter(|d| d.is_array()).unwrap_or(&root);
    let Some(items) = data.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let base = item.get("entitlement_base_info");
        let plan = base
            .and_then(|b| b.get("resource_name").or_else(|| b.get("resourceName")))
            .or_else(|| item.get("resource_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("套餐")
            .to_string();
        let limit = base
            .and_then(|b| b.get("quota"))
            .and_then(|q| q.get("credits_limit").or_else(|| q.get("creditsLimit")))
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0.0);
        let used = item
            .get("usage")
            .and_then(|u| u.get("credits_amount").or_else(|| u.get("creditsAmount")))
            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0.0);
        if limit <= 0.0 {
            continue;
        }
        out.push(TraeWindow { plan, used, limit });
    }
    out
}

/// usage_summary：`{ usage_summary: { total_amount, consumed_amount } }`。
pub fn parse_usage_summary(body: &str) -> Option<(f64, f64)> {
    let root: Value = serde_json::from_str(body).ok()?;
    let s = root.get("usage_summary").or_else(|| root.pointer("/data/usage_summary"))?;
    let total = num_of(s, &["total_amount", "totalAmount"])?;
    let used = num_of(s, &["consumed_amount", "consumedAmount"]).unwrap_or(0.0);
    Some((total, used))
}

fn num_of(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| {
        let x = v.get(*k)?;
        x.as_f64().or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

fn json_code(root: &Value) -> Option<i64> {
    root.get("code")
        .and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.parse().ok())))
}

/// 签到 status → (checked_in, credits, enable)。
#[allow(dead_code)]
pub fn parse_checkin_status(body: &str) -> Option<(bool, Option<f64>, bool)> {
    let root: Value = serde_json::from_str(body).ok()?;
    parse_checkin_status_value(&root)
}

fn parse_checkin_status_value(root: &Value) -> Option<(bool, Option<f64>, bool)> {
    if json_code(root) == Some(0) {
        return Some((
            root.get("checked_in").and_then(|v| v.as_bool()).unwrap_or(false),
            root.get("credits").and_then(|v| v.as_f64()),
            root.get("enable").and_then(|v| v.as_bool()).unwrap_or(false),
        ));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraeCheckinResult {
    pub id: i64,
    pub label: Option<String>,
    pub ok: bool,
    /// 今日已签 / 功能未开放，本轮跳过。
    pub skipped: bool,
    /// 本次领取到的积分。
    pub points: Option<f64>,
    /// 积分余额。
    pub credits: Option<f64>,
    pub message: Option<String>,
    pub needs_reauth: bool,
}

impl TraeCheckinResult {
    fn fail(id: i64, label: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            label,
            ok: false,
            skipped: false,
            points: None,
            credits: None,
            message: Some(message.clone()),
            needs_reauth: message == REAUTH_MESSAGE || message.contains("重新提取"),
        }
    }
}

// ---------------------------------------------------------------------------
// 设备证明（v3 ExchangeToken 需要 ECDSA P-256 签名）
// ---------------------------------------------------------------------------

/// 从 Trae 客户端 storage.json 里导出的设备密钥材料（PEM），粘贴账号时一并保存。
#[derive(Debug, Clone, Default)]
pub struct DeviceKeys {
    pub device_id: String,
    pub private_pem: String,
    pub public_pem: String,
    pub machine_id: String,
}

impl DeviceKeys {
    pub fn is_complete(&self) -> bool {
        !self.device_id.is_empty() && !self.private_pem.is_empty() && !self.public_pem.is_empty()
    }
}

/// ECDSA P-256 设备签名：canonical = `POST\n<path>\n<client_id>\n<refresh_token>\n<ts>\n<nonce>`。
/// 返回 DER 编码签名；私钥不可用（空 / 解析失败）时返回 None。
fn device_proof_signature(private_pem: &str, canonical: &str) -> Option<String> {
    use p256::ecdsa::{signature::Signer, Signature, SigningKey};
    use p256::pkcs8::DecodePrivateKey;
    let pem = private_pem.trim();
    if pem.is_empty() || !pem.contains("PRIVATE KEY") {
        return None;
    }
    // 客户端存的是 PKCS#8 PEM；万一给到 SEC1 也按裸字节解析兜底。
    let key = match SigningKey::from_pkcs8_pem(pem) {
        Ok(k) => k,
        Err(_) => {
            // SEC1: "PRIVATE EC KEY" 走 SecretKey::from_sec1_pem。
            use p256::SecretKey;
            let sk = SecretKey::from_sec1_pem(pem).ok()?;
            SigningKey::from(&sk)
        }
    };
    let sig: Signature = key.sign(canonical.as_bytes());
    Some(base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()))
}

/// 构造 DeviceProof + DeviceInfo（v3 ExchangeToken 必带）。
fn device_proof_parts(
    identity: &TraeDeviceIdentity,
    path: &str,
    token_for_signing: &str,
    client_id: &str,
) -> (Value, Value) {
    let ts = now_unix();
    let nonce = {
        let mut h = Sha256::new();
        h.update(token_for_signing);
        h.update(ts.to_le_bytes());
        h.update(client_id);
        let d = h.finalize();
        d[..16].iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let canonical = format!("POST\n{path}\n{client_id}\n{token_for_signing}\n{ts}\n{nonce}");
    let signature = device_proof_signature(&identity.private_key_pem, &canonical).unwrap_or_default();
    let machine_id = if identity.machine_id.is_empty() {
        derived_machine_id(&identity.device_id)
    } else {
        identity.machine_id.clone()
    };
    let proof = serde_json::json!({
        "Timestamp": ts,
        "Nonce": nonce,
        "Signature": signature,
    });
    let info = serde_json::json!({
        "DeviceID": identity.device_id,
        "MachineID": machine_id,
        "PlatformCode": "IDE_PC",
        "DeviceType": "PC",
        "DeviceName": "PC",
        "DeviceModel": "",
        "ClientVersion": APP_VERSION,
        "DevicePublicKey": identity.public_key_pem,
        "DeviceBrand": "",
        "DeviceCPU": "",
        "OSInfo": "Windows",
        "OSVersion": "",
    });
    (proof, info)
}

/// ECDSA 设备签名的 canonical 串构造（供测试钉住格式）。
#[allow(dead_code)]
fn canonical_string(path: &str, refresh_token: &str, ts: i64, nonce: &str, client_id: &str) -> String {
    format!("POST\n{path}\n{client_id}\n{refresh_token}\n{ts}\n{nonce}")
}

// ---------------------------------------------------------------------------
// 网络
// ---------------------------------------------------------------------------

fn client_err(e: reqwest::Error, fallback: &str) -> String {
    crate::config::friendly_http_error(e, fallback)
}

async fn post_json(client: &reqwest::Client, url: &str, headers: &[(&str, String)], body: &str) -> Result<(u16, String), String> {
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.to_string());
    for (k, v) in headers {
        req = req.header(*k, v.clone());
    }
    let resp = req.send().await.map_err(|e| client_err(e, "请求失败"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}

fn ug_headers(token: &str, device_id: &str) -> Vec<(&'static str, String)> {
    let mut h: Vec<(&'static str, String)> = vec![
        ("Authorization", format!("Cloud-IDE-JWT {token}")),
        ("X-User-Region", "cn".to_string()),
        ("User-Agent", "Trae/1.107.1".to_string()),
    ];
    if !device_id.trim().is_empty() {
        h.push(("x-device-id", device_id.trim().to_string()));
    }
    h
}

#[derive(Debug, Clone)]
struct ExchangePair {
    access: String,
    refresh: String,
}

fn parse_exchange(body: &str) -> Option<ExchangePair> {
    let v: Value = serde_json::from_str(body).ok()?;
    let res = v.get("Result")?;
    let access = res.get("Token").and_then(|t| t.as_str())?.trim().to_string();
    if access.is_empty() {
        return None;
    }
    let refresh = res
        .get("RefreshToken")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    Some(ExchangePair { access, refresh })
}

fn exchange_err_message(status: u16, body: &str) -> String {
    let code = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/ResponseMetadata/Error/Code")
                .and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.parse().ok())))
        });
    match code {
        Some(20101) => REAUTH_MESSAGE.to_string(),
        Some(10101) => "refreshToken 与客户端不匹配（可能被别处轮换过），请重新提取".to_string(),
        Some(20405) => "上游要求设备证明，请补全设备密钥（deviceKeyPem / devicePubPem / deviceId）".to_string(),
        Some(20403) => "设备与 token 不匹配：设备密钥需来自签发该 token 的同一台机器".to_string(),
        _ => format!("续期失败（HTTP {status}）"),
    }
}

/// 续期：优先 v3（带设备证明），没有设备密钥或 v3 不可用时回退 v2。
async fn exchange_token(
    client: &reqwest::Client,
    refresh_token: &str,
    keys: Option<&DeviceKeys>,
    _tag: &str,
) -> Result<ExchangePair, String> {
    // v2 简单流程：无需设备证明（部分旧 token 仍可用）。
    let body = serde_json::json!({
        "ClientID": CLIENT_ID_LEGACY,
        "RefreshToken": refresh_token,
        "ClientSecret": "-",
        "UserID": "",
    });
    let (status, text) = post_json(client, EP_EXCHANGE, &[], &body.to_string()).await?;
    if (200..300).contains(&status) {
        if let Some(p) = parse_exchange(&text) {
            return Ok(p);
        }
    }
    let v2_err = exchange_err_message(status, &text);
    // v2 拒绝时若拿着设备密钥对，再试 v3（带真实 ECDSA DeviceProof）。
    if let Some(k) = keys.filter(|k| k.is_complete()) {
        let identity = TraeDeviceIdentity {
            device_id: k.device_id.clone(),
            public_key_pem: k.public_pem.clone(),
            private_key_pem: k.private_pem.clone(),
            machine_id: k.machine_id.clone(),
        };
        let (proof, info) = device_proof_parts(&identity, "/trae/api/v3/oauth/ExchangeToken", refresh_token, CLIENT_ID_IDE);
        let body = serde_json::json!({
            "ClientID": CLIENT_ID_IDE,
            "ClientSecret": "",
            "RefreshToken": refresh_token,
            "DeviceInfo": info,
            "DeviceProof": proof,
            "IDEVersion": APP_VERSION,
        });
        let (status, text) = post_json(client, EP_EXCHANGE_V3, &[("x-cloudide-token", String::new()), ("x-app-id", OAUTH_APP_ID.to_string()), ("x-platform-code", "IDE_PC".to_string()), ("User-Agent", format!("Trae/{APP_VERSION}"))], &body.to_string()).await?;
        if (200..300).contains(&status) {
            if let Some(p) = parse_exchange(&text) {
                return Ok(p);
            }
        }
        return Err(exchange_err_message(status, &text));
    }
    Err(v2_err)
}

enum UsageError {
    Unauthorized,
    Other(String),
}

async fn request_entitlement(client: &reqwest::Client, token: &str, device_id: &str) -> Result<String, UsageError> {
    let (status, text) =
        post_json(client, EP_ENTITLE, &ug_headers(token, device_id), r#"{"require_usage": true, "full_data": true}"#)
            .await
            .map_err(UsageError::Other)?;
    if status == 401 || status == 403 {
        return Err(UsageError::Unauthorized);
    }
    if !(200..300).contains(&status) {
        return Err(UsageError::Other(format!("上游返回 {status}")));
    }
    Ok(text)
}

async fn request_checkin(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    device_id: &str,
) -> Result<(u16, Value), String> {
    let (status, text) = post_json(client, url, &ug_headers(token, device_id), REQ_BODY).await?;
    let v: Value = serde_json::from_str(&text).map_err(|_| format!("响应解析失败: {}", &text[..text.len().min(120)]))?;
    Ok((status, v))
}

// ---------------------------------------------------------------------------
// 账号操作
// ---------------------------------------------------------------------------

struct UsableToken {
    label: Option<String>,
    access: String,
    refresh: String,
    device_id: String,
    /// token 是否来自刚续期（401 后不再重复续期）。
    refreshed: bool,
}

async fn usable_token(state: &DbState, client: &reqwest::Client, id: i64) -> Result<UsableToken, String> {
    let stored = token_for(Arc::clone(&state.conn), id, Provider::Trae).await?;
    let refresh = stored.refresh_token.clone();
    if refresh.trim().is_empty() {
        return Err(REAUTH_MESSAGE.to_string());
    }
    let device_id = stored_tag_device(&stored.refresh_token);
    // access 有效（JWT exp 未到）就直接用。
    let exp = jwt_exp(&stored.access_token);
    if exp > now_unix() + 30 {
        return Ok(UsableToken {
            label: stored.label,
            access: stored.access_token,
            refresh,
            device_id,
            refreshed: false,
        });
    }
    let keys = stored_device_keys(&stored.refresh_token);
    let pair = exchange_token(client, &refresh, keys.as_ref(), "").await?;
    store_pair(state, id, &pair).await?;
    // 上游没回新 refreshToken 时视为未轮换，沿用旧值。
    let new_refresh = if pair.refresh.trim().is_empty() { refresh } else { pair.refresh };
    Ok(UsableToken {
        label: stored.label,
        access: pair.access,
        device_id,
        refresh: new_refresh,
        refreshed: true,
    })
}

/// refresh_token 存储格式：`<refreshToken>|<deviceId>|<machineId>|<privPemB64>|<pubPemB64>`（device 字段可空）。
fn stored_tag_device(refresh_token: &str) -> String {
    refresh_token.split('|').nth(1).unwrap_or_default().trim().to_string()
}

fn stored_device_keys(refresh_token: &str) -> Option<DeviceKeys> {
    let parts: Vec<&str> = refresh_token.split('|').collect();
    if parts.len() < 5 {
        return None;
    }
    let dec = |s: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
    };
    Some(DeviceKeys {
        device_id: parts[1].trim().to_string(),
        private_pem: dec(parts[3]),
        public_pem: dec(parts[4]),
        machine_id: parts[2].trim().to_string(),
    })
}

fn store_format(refresh_token: &str, keys: &DeviceKeys) -> String {
    let b64 = |s: &str| {
        if s.trim().is_empty() {
            String::new()
        } else {
            base64::engine::general_purpose::STANDARD.encode(s.trim())
        }
    };
    format!(
        "{}|{}|{}|{}|{}",
        refresh_token.trim(),
        keys.device_id.trim(),
        keys.machine_id.trim(),
        b64(&keys.private_pem),
        b64(&keys.public_pem)
    )
}

async fn store_pair(state: &DbState, id: i64, pair: &ExchangePair) -> Result<(), String> {
    // 沿用库里那份设备信息，只替换 refreshToken 本体（第一段）。
    let keys = token_for(Arc::clone(&state.conn), id, Provider::Trae)
        .await
        .ok()
        .and_then(|s| stored_device_keys(&s.refresh_token))
        .unwrap_or_default();
    let combined = store_format(&pair.refresh_or_empty(), &keys);
    let access = pair.access.clone();
    let exp = jwt_exp(&pair.access);
    with_db(Arc::clone(&state.conn), move |c| update_tokens(c, id, &access, &combined, exp)).await
}

impl ExchangePair {
    /// 上游没回新 refreshToken 时视为未轮换，保留空串由调用方决定沿用旧值。
    fn refresh_or_empty(&self) -> String {
        self.refresh.trim().to_string()
    }
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

/// 粘贴 refreshToken 新增账号：先续期一次验证可用，成功才入库。
#[tauri::command]
pub async fn trae_add_refresh_token(
    state: tauri::State<'_, DbState>,
    refresh_token: String,
    device_id: Option<String>,
    machine_id: Option<String>,
    device_key_pem: Option<String>,
    device_pub_pem: Option<String>,
) -> Result<QuotaAccountDto, String> {
    let rt = refresh_token.trim().to_string();
    if rt.is_empty() {
        return Err("请填写 refreshToken".to_string());
    }
    let client = http_client()?;
    let keys = DeviceKeys {
        device_id: device_id.unwrap_or_default().trim().to_string(),
        private_pem: device_key_pem.unwrap_or_default().trim().to_string(),
        public_pem: device_pub_pem.unwrap_or_default().trim().to_string(),
        machine_id: machine_id.unwrap_or_default().trim().to_string(),
    };
    let pair = exchange_token(&client, &rt, Some(&keys), "").await?;
    // 用新 access 查一次 entitlement 验证可用性；失败也不拦入库（额度字段可能是空套餐）。
    let uid = jwt_uid(&pair.access);
    let subject = if uid.is_empty() { refresh_subject(&rt) } else { format!("uid:{uid}") };
    let refresh_body = if pair.refresh.is_empty() { rt.clone() } else { pair.refresh.clone() };
    let combined = store_format(&refresh_body, &keys);
    let label = if uid.is_empty() { refresh_subject(&rt) } else { format!("uid:{uid}") };
    let id = with_db(Arc::clone(&state.conn), move |c| {
        upsert_account(c, Provider::Trae, &subject, Some(&label), &pair.access, &combined, now_unix())
    })
    .await?;
    with_db(Arc::clone(&state.conn), move |c| {
        crate::quota_accounts::list_accounts(c, Some(Provider::Trae))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

#[tauri::command]
pub async fn trae_quota_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<TraeQuotaDto, String> {
    let client = http_client()?;
    let mut t = match usable_token(&state, &client, id).await {
        Ok(t) => t,
        Err(m) => return Ok(TraeQuotaDto::err(id, None, m)),
    };

    let body = match request_entitlement(&client, &t.access, &t.device_id).await {
        Ok(b) => b,
        Err(UsageError::Unauthorized) if !t.refreshed => {
            let keys = stored_device_keys(&t.refresh);
            match exchange_token(&client, &t.refresh, keys.as_ref(), "").await {
                Ok(pair) => {
                    store_pair(&state, id, &pair).await?;
                    t.access = pair.access;
                    t.refreshed = true;
                    match request_entitlement(&client, &t.access, &t.device_id).await {
                        Ok(b) => b,
                        Err(UsageError::Unauthorized) => return Ok(TraeQuotaDto::err(id, t.label, REAUTH_MESSAGE)),
                        Err(UsageError::Other(m)) => return Ok(TraeQuotaDto::err(id, t.label, m)),
                    }
                }
                Err(m) => return Ok(TraeQuotaDto::err(id, t.label, m)),
            }
        }
        Err(UsageError::Unauthorized) => return Ok(TraeQuotaDto::err(id, t.label, REAUTH_MESSAGE)),
        Err(UsageError::Other(m)) => return Ok(TraeQuotaDto::err(id, t.label, m)),
    };

    // entitlement 为主体；usage_summary 可能嵌在响应里。
    let mut windows = parse_entitlement(&body);
    if windows.is_empty() {
        if let Some((total, used)) = parse_usage_summary(&body) {
            if total > 0.0 {
                windows.push(TraeWindow { plan: "套餐".to_string(), used, limit: total });
            }
        }
    }

    // 签到状态顺带查（失败不影响额度）。
    let (checked_in, credits, checkin_enabled) =
        match request_checkin(&client, EP_CHECKIN_STATUS, &t.access, &t.device_id).await {
            Ok((status, v)) if (200..300).contains(&status) => parse_checkin_status_value(&v)
                .map_or((None, None, None), |(ci, cr, en)| (Some(ci), cr, Some(en))),
            _ => (None, None, None),
        };
    // 今日打卡时刻来自本地签到记录（上游 status 不带时间）。
    let checked_in_at = if checked_in == Some(true) {
        with_db(Arc::clone(&state.conn), move |c| Ok(checkin_ok_at(c, id))).await?
    } else {
        None
    };

    Ok(TraeQuotaDto {
        id,
        ok: true,
        message: None,
        label: t.label,
        credits,
        windows,
        checked_in,
        checked_in_at,
        checkin_enabled,
        needs_reauth: false,
        at: now_unix(),
    })
}

#[tauri::command]
pub async fn trae_checkin_now(state: tauri::State<'_, DbState>, id: i64) -> Result<TraeCheckinResult, String> {
    let label = token_for(Arc::clone(&state.conn), id, Provider::Trae)
        .await
        .ok()
        .and_then(|s| s.label.clone());
    Ok(trae_checkin_shared(&state, id, label).await)
}

// ---------------------------------------------------------------------------
// 授权登录（浏览器 OAuth + 本地回调，2026-09 官网授权页抓包固化）
// ---------------------------------------------------------------------------

const AUTHORIZE_URL: &str = "https://www.trae.cn/authorization";
const CALLBACK_PORT: u16 = 17388;
const CALLBACK_PATH: &str = "/authorize";
/// 授权页带的 OAuth 应用 id（抓包固定值）。
const OAUTH_APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8";
/// 客户端版本兜底；换协议时可被环境变量覆盖。
const APP_VERSION: &str = "3.3.100";
const PLUGIN_VERSION: &str = "2.3.83560";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// 本机 Trae 客户端身份：device_id / 公钥 / 私钥 / machineId。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraeDeviceIdentity {
    pub device_id: String,
    pub public_key_pem: String,
    /// 设备私钥（签名 DeviceProof 用）；DTO 序列化时跳过，不出进程。
    #[serde(skip_serializing)]
    pub private_key_pem: String,
    pub machine_id: String,
}

/// machineId 缺失时的兜底：由 device_id 派生稳定值（URL 与请求体同源）。
pub fn derived_machine_id(device_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("trae-oauth-machine:{device_id}"));
    let d = h.finalize();
    d[..16].iter().map(|b| format!("{b:02x}")).collect::<String>()
}

// ---------------------------------------------------------------------------
// storage.json 加密块解密（客户端同源 AES-128-CBC，密钥由固定 salt 异或派生）
// ---------------------------------------------------------------------------

/// 客户端固定盐（与 Trae 客户端解密逻辑逐字节一致，勿按语义改写）。
const SALT_A: [u8; 64] = [
    82, 9, 106, 213, 48, 54, 165, 56, 191, 64, 163, 158, 129, 243, 215, 251, 124, 227, 57, 130, 155,
    47, 255, 135, 52, 142, 67, 68, 196, 222, 233, 203, 84, 123, 148, 50, 166, 194, 35, 61, 238, 76,
    149, 11, 66, 250, 195, 78, 8, 46, 161, 102, 40, 217, 36, 178, 118, 91, 162, 73, 109, 139, 209, 37,
];
const SALT_B: [u8; 64] = [
    31, 221, 168, 51, 136, 7, 199, 49, 177, 18, 16, 89, 39, 128, 236, 95, 96, 81, 127, 169, 25, 181,
    74, 13, 45, 229, 122, 159, 147, 201, 156, 239, 160, 224, 59, 77, 174, 42, 245, 176, 200, 235,
    187, 60, 131, 83, 153, 97, 23, 43, 4, 126, 186, 119, 214, 38, 225, 105, 20, 99, 85, 33, 12, 125,
];

fn aes_salt() -> [u8; 64] {
    let mut out = [0u8; 64];
    for i in 0..64 {
        out[i] = SALT_A[i] ^ SALT_B[i];
    }
    out
}

// ---- AES-128 软件实现（加密方向做轮密钥展开，解密用等价逆轮）----

fn aes_sbox() -> [u8; 256] {
    // 标准 AES S-box，运行时生成避免 256 项手写表出错。
    let mut inv = [0u8; 256];
    // 求 GF(2^8) 乘法逆元表（p * a == 1）。
    for (i, slot) in inv.iter_mut().enumerate().skip(1) {
        for j in 1..256usize {
            if gf_mul(i as u8, j as u8) == 1 {
                *slot = j as u8;
                break;
            }
        }
    }
    let mut sbox = [0u8; 256];
    for i in 0..256usize {
        let b = inv[i];
        // 仿射变换
        const AFFINE: [u8; 8] = [1, 1, 0, 0, 0, 1, 1, 0];
        let mut r = 0u8;
        for bit in 0..8u32 {
            let fb = b.bit(bit) ^ b.bit((bit + 4) % 8) ^ b.bit((bit + 5) % 8) ^ b.bit((bit + 6) % 8) ^ b.bit((bit + 7) % 8) ^ (AFFINE[bit as usize] != 0);
            r |= (fb as u8) << bit;
        }
        sbox[i] = r;
    }
    sbox
}

trait Bit1 {
    fn bit(self, n: u32) -> bool;
}
impl Bit1 for u8 {
    fn bit(self, n: u32) -> bool {
        (self >> n) & 1 == 1
    }
}

fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

/// AES-128-CBC 解密（no padding 处理由调用方做，与客户端一致先剥 64 字节前缀）。
fn aes128_cbc_decrypt(key: [u8; 16], iv: [u8; 16], data: &[u8]) -> Vec<u8> {
    let round_keys = aes128_expand_key(key);
    let mut prev = iv;
    let mut out = Vec::with_capacity(data.len());
    for block in data.chunks(16) {
        if block.len() < 16 {
            break;
        }
        let mut blk = [0u8; 16];
        blk.copy_from_slice(block);
        let dec = aes_decrypt_block(blk, &round_keys);
        out.extend(dec.iter().zip(prev.iter()).map(|(a, b)| a ^ b));
        prev = blk;
    }
    out
}

fn aes128_expand_key(key: [u8; 16]) -> [[u8; 16]; 11] {
    let sbox = aes_sbox();
    let mut rcon = 0x01u8;
    // 44 个 32 位字；round[r] = words[4r..4r+4]。
    let mut words = [[0u8; 4]; 44];
    for i in 0..4 {
        words[i] = [key[i * 4], key[i * 4 + 1], key[i * 4 + 2], key[i * 4 + 3]];
    }
    for i in 4..44 {
        let mut temp = words[i - 1];
        if i % 4 == 0 {
            // RotWord + SubWord + Rcon
            temp = [sbox[temp[1] as usize] ^ rcon, sbox[temp[2] as usize], sbox[temp[3] as usize], sbox[temp[0] as usize]];
            rcon = gf_mul(rcon, 2);
        }
        let p = words[i - 4];
        words[i] = [p[0] ^ temp[0], p[1] ^ temp[1], p[2] ^ temp[2], p[3] ^ temp[3]];
    }
    let mut rounds = [[0u8; 16]; 11];
    for r in 0..11 {
        for c in 0..4 {
            rounds[r][c * 4..c * 4 + 4].copy_from_slice(&words[r * 4 + c]);
        }
    }
    rounds
}

fn aes_decrypt_block(block: [u8; 16], rounds: &[[u8; 16]; 11]) -> [u8; 16] {
    let sbox = aes_sbox();
    let mut s = block;
    // 最后一轮轮密钥加
    xor16(&mut s, &rounds[10]);
    for r in (1..10).rev() {
        // InvShiftRows
        inv_shift_rows(&mut s);
        // InvSubBytes
        for b in s.iter_mut() {
            *b = inv_sbox(*b, &sbox);
        }
        // AddRoundKey
        xor16(&mut s, &rounds[r]);
        // InvMixColumns
        inv_mix_columns(&mut s);
    }
    inv_shift_rows(&mut s);
    for b in s.iter_mut() {
        *b = inv_sbox(*b, &sbox);
    }
    xor16(&mut s, &rounds[0]);
    s
}

fn xor16(s: &mut [u8; 16], k: &[u8; 16]) {
    for i in 0..16 {
        s[i] ^= k[i];
    }
}

fn inv_sbox(b: u8, sbox: &[u8; 256]) -> u8 {
    // 用 S-box 反查（O(256) 但每次解密调用有限，可接受）。
    sbox.iter().position(|&x| x == b).unwrap_or(0) as u8
}

fn inv_shift_rows(s: &mut [u8; 16]) {
    // 列主序 state[r + 4c]，行 r 左移 r（加密）→ 解密右移 r。
    let c = *s;
    for r in 1..4usize {
        for cc in 0..4usize {
            s[r + 4 * cc] = c[r + 4 * ((cc + 4 - r) % 4)];
        }
    }
}

fn inv_mix_columns(s: &mut [u8; 16]) {
    for c in 0..4usize {
        let a = [s[4 * c], s[4 * c + 1], s[4 * c + 2], s[4 * c + 3]];
        s[4 * c] = gf_mul(a[0], 14) ^ gf_mul(a[1], 11) ^ gf_mul(a[2], 13) ^ gf_mul(a[3], 9);
        s[4 * c + 1] = gf_mul(a[0], 9) ^ gf_mul(a[1], 14) ^ gf_mul(a[2], 11) ^ gf_mul(a[3], 13);
        s[4 * c + 2] = gf_mul(a[0], 13) ^ gf_mul(a[1], 9) ^ gf_mul(a[2], 14) ^ gf_mul(a[3], 11);
        s[4 * c + 3] = gf_mul(a[0], 11) ^ gf_mul(a[1], 13) ^ gf_mul(a[2], 9) ^ gf_mul(a[3], 14);
    }
}

/// 客户端加密块格式：`[6 字节头][32 字节随机][密文]`；密钥 = sha512(sha512(rand))[..16]，
/// iv = 接着 16 字节；明文先剥 64 字节前缀再按 PKCS7 去填充。
pub fn decrypt_storage_value(b64: &str) -> Vec<u8> {
    use base64::Engine;
    let Ok(buf) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return Vec::new();
    };
    if buf.len() < 38 {
        return Vec::new();
    }
    let rb: [u8; 32] = buf[6..38].try_into().unwrap_or([0u8; 32]);
    let enc = &buf[38..];
    let mut h = Sha512::new();
    h.update(rb);
    let h1 = h.finalize();
    let mut h2 = Sha512::new();
    h2.update(h1);
    h2.update(aes_salt());
    let fh = h2.finalize();
    let key: [u8; 16] = fh[..16].try_into().unwrap_or([0u8; 16]);
    let iv: [u8; 16] = fh[16..32].try_into().unwrap_or([0u8; 16]);
    let mut pt = aes128_cbc_decrypt(key, iv, enc);
    if pt.len() > 64 {
        pt.drain(..64);
    }
    if let Some(&pad) = pt.last() {
        if (1..=16).contains(&pad) && pt.len() > pad as usize {
            pt.truncate(pt.len() - pad as usize);
        }
    }
    pt
}

/// 读本机 Trae 客户端 storage.json：`iCubeAuthInfo://icube-dc:<did>` 的 key 取 device_id，
/// 值是客户端 AES-128-CBC 加密的设备信息（publicKeyPEM / privateKeyPEM）；machineId 用 telemetry.machineId。
pub fn read_device_identity() -> Option<TraeDeviceIdentity> {
    let appdata = std::env::var("APPDATA").ok()?;
    for name in ["Trae CN", "TRAE SOLO CN", "TRAE SOLO", "Trae"] {
        let p = std::path::Path::new(&appdata).join(name).join("User").join("globalStorage").join("storage.json");
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(obj) = v.as_object() else { continue };
        let did = obj.keys().find_map(|k| {
            k.strip_prefix("iCubeAuthInfo://icube-dc:")
                .filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
                .map(str::to_string)
        });
        let Some(did) = did else { continue };
        let machine_id = obj
            .get("telemetry.machineId")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        // 解密 icube-dc 块拿设备密钥对（v3 兑换/续期需要 DevicePublicKey）。
        let (public_pem, private_pem) = obj
            .get(&format!("iCubeAuthInfo://icube-dc:{did}"))
            .and_then(|enc| enc.as_str())
            .map(|s| decrypt_storage_value(s.trim()))
            .map(|pt| {
                let dev: Value = serde_json::from_slice(&pt).unwrap_or(Value::Null);
                (
                    dev.get("publicKeyPEM").and_then(|s| s.as_str()).unwrap_or_default().trim().to_string(),
                    dev.get("privateKeyPEM").and_then(|s| s.as_str()).unwrap_or_default().trim().to_string(),
                )
            })
            .unwrap_or_default();
        return Some(TraeDeviceIdentity {
            device_id: did,
            public_key_pem: public_pem,
            private_key_pem: private_pem,
            machine_id,
        });
    }
    None
}

/// 构造授权页 URL（与官网 IDE 授权流程同参）。回调打到本机 17388 端口。
pub fn trae_authorize_url(
    identity: &TraeDeviceIdentity,
    challenge: &str,
    verifier_hint: &str,
    trace_id: &str,
) -> String {
    let _ = verifier_hint;
    let machine_id = if identity.machine_id.is_empty() {
        derived_machine_id(&identity.device_id)
    } else {
        identity.machine_id.clone()
    };
    let machine_id2 = machine_id.clone();
    let params = [
        ("login_version", "1".to_string()),
        ("auth_from", "trae".to_string()),
        ("login_channel", "native_ide".to_string()),
        ("plugin_version", PLUGIN_VERSION.to_string()),
        ("auth_type", "local".to_string()),
        ("client_id", CLIENT_ID_IDE.to_string()),
        ("redirect", "0".to_string()),
        ("login_trace_id", trace_id.to_string()),
        ("auth_callback_url", format!("http://127.0.0.1:{CALLBACK_PORT}{CALLBACK_PATH}")),
        ("machine_id", machine_id),
        ("device_id", identity.device_id.clone()),
        ("x_device_id", identity.device_id.clone()),
        ("x_machine_id", machine_id2),
        ("x_device_brand", String::new()),
        ("x_device_type", "Windows".to_string()),
        ("x_os_version", String::new()),
        ("x_env", String::new()),
        ("x_app_version", APP_VERSION.to_string()),
        ("x_app_type", "stable".to_string()),
        ("code_challenge", challenge.to_string()),
        ("code_challenge_method", "S256".to_string()),
        ("channel_name", "common".to_string()),
    ];
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlenc(v)))
        .collect();
    format!("{AUTHORIZE_URL}?{}", query.join("&"))
}

fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 回调里解析出的凭据与账号信息。
#[derive(Debug, Clone, Default)]
pub struct TraeCallback {
    pub auth_code: String,
    pub refresh_token: String,
    pub host: String,
    /// 账号展示名（回调 userInfo JSON 里的 ScreenName / NickName / email 等）。
    pub name: String,
}

/// 回调查询串解析。宽松处理官方的二次编码与 authCodeInfo / userInfo JSON 包裹。
pub fn parse_callback_query(raw: &str) -> TraeCallback {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for seg in raw.split('&') {
        if seg.is_empty() {
            continue;
        }
        if let Some((k, v)) = seg.split_once('=') {
            pairs.push((decURIComponent(k), decURIComponent(v)));
        }
    }
    // 官方可能对 auth_callback_url 的值二次编码：再解一轮。
    let again: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (k.clone(), decURIComponent(v)))
        .collect();
    let get = |list: &[(String, String)], keys: &[&str]| -> String {
        keys.iter()
            .find_map(|key| list.iter().find(|(k, _)| k == key))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let mut cb = TraeCallback {
        auth_code: get(&again, &["code", "authCode", "auth_code"]),
        refresh_token: get(&again, &["refreshToken", "refresh_token", "RefreshToken"]),
        host: get(&again, &["host", "accountHost"]),
        name: String::new(),
    };
    // 回调 userInfo JSON（ScreenName / NickName / nickname / email …）。
    let raw_user = get(&again, &["userInfo", "user_info", "UserInfo"]);
    if !raw_user.is_empty() {
        if let Ok(v) = serde_json::from_str::<Value>(&raw_user) {
            cb.name = dig_string(&v, &["ScreenName", "NickName", "nickname", "nickName", "name", "userName", "email"]);
        }
    }
    if cb.name.is_empty() {
        cb.name = get(&again, &["userName", "user_name", "nickname", "nickName", "ScreenName"]);
    }
    // 官网回跳常把凭据包在 authCodeInfo JSON 里：`authCodeInfo={"AuthCode":"…","RefreshToken":"…","host":"…"}`。
    if cb.auth_code.is_empty() {
        let raw_json = get(&again, &["authCodeInfo", "auth_code_info"]);
        if !raw_json.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(&raw_json) {
                cb.auth_code = dig_string(&v, &["AuthCode", "authCode", "auth_code", "Code", "code"]);
                if cb.refresh_token.is_empty() {
                    cb.refresh_token = dig_string(&v, &["RefreshToken", "refresh_token", "refreshToken"]);
                }
                if cb.host.is_empty() {
                    cb.host = dig_string(&v, &["host", "accountHost", "Host"]);
                }
                if cb.name.is_empty() {
                    cb.name = dig_string(&v, &["ScreenName", "NickName", "nickname", "nickName", "name", "userName", "email"]);
                }
            } else {
                // JSON 解析失败（可能被截断）：兜底用子串抽取 "Key":"…"。
                let extract = |key: &str| -> String {
                    let pat = format!("\"{key}\":");
                    if let Some(pos) = raw_json.find(&pat) {
                        let rest = &raw_json[pos + pat.len()..];
                        let rest = rest.trim_start();
                        if let Some(rest) = rest.strip_prefix('"') {
                            if let Some(end) = rest.find('"') {
                                return rest[..end].to_string();
                            }
                        }
                    }
                    String::new()
                };
                cb.auth_code = [extract("AuthCode"), extract("authCode"), extract("Code")]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .unwrap_or_default();
                if cb.refresh_token.is_empty() {
                    cb.refresh_token = extract("RefreshToken");
                }
                if cb.host.is_empty() {
                    cb.host = extract("host");
                }
                if cb.name.is_empty() {
                    cb.name = [extract("ScreenName"), extract("NickName"), extract("nickname"), extract("userName"), extract("email")]
                        .into_iter()
                        .find(|s| !s.is_empty())
                        .unwrap_or_default();
                }
            }
        }
    }
    cb
}

/// 递归在 JSON 里按键名找第一个非空字符串。
fn dig_string(v: &Value, keys: &[&str]) -> String {
    fn dig<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
        match v {
            Value::Object(m) => keys
                .iter()
                .find_map(|k| m.get(*k).and_then(|x| x.as_str()))
                .filter(|s| !s.is_empty())
                .or_else(|| m.values().find_map(|c| dig(c, keys))),
            Value::Array(a) => a.iter().find_map(|c| dig(c, keys)),
            _ => None,
        }
    }
    dig(v, keys).unwrap_or_default().to_string()
}

#[allow(non_snake_case)]
fn decURIComponent(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 <= bytes.len() - 1 + 1 {
            let hex = |b: u8| -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                }
            };
            if i + 2 < bytes.len() {
                if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 通知授权页「回跳已收到」的简单结果页。
/// 授权结果页：居中卡片 + 状态圆标 + 主次两级文案；HTML 转义调用方文案。
fn callback_html(ok: bool, title: &str, lines: &[&str]) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    // 有意写死：独立回调页无主题系统，配色对齐品牌绿 / 错误红的固定值。
    let (accent, accent_soft, ring) = if ok { ("#3fbf72", "rgba(63,191,114,0.12)", "rgba(63,191,114,0.35)") } else { ("#ef5b5b", "rgba(239,91,91,0.12)", "rgba(239,91,91,0.35)") };
    let icon = if ok {
        // 对勾
        r#"<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>"#
    } else {
        // 叉
        r#"<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>"#
    };
    let body: Vec<String> = lines.iter().map(|l| format!("<p>{}</p>", esc(l))).collect();
    let title = esc(title);
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  html, body {{ height: 100%; }}
  body {{
    display: flex; align-items: center; justify-content: center;
    font-family: "Segoe UI", system-ui, "Microsoft YaHei", sans-serif;
    background: radial-gradient(1200px 700px at 20% -10%, #1a2438 0%, #0b111e 55%, #070b14 100%);
    color: #e7ecf3;
  }}
  .card {{
    width: min(400px, calc(100vw - 48px));
    padding: 44px 40px 36px;
    text-align: center;
    background: linear-gradient(180deg, #151d2e 0%, #101728 100%);
    border: 1px solid #263149;
    border-radius: 18px;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.45), inset 0 1px 0 rgba(255, 255, 255, 0.05);
    animation: rise 0.45s cubic-bezier(0.2, 0.7, 0.3, 1) both;
  }}
  @keyframes rise {{ from {{ opacity: 0; transform: translateY(14px) scale(0.985); }} to {{ opacity: 1; transform: none; }} }}
  .badge {{
    width: 64px; height: 64px; margin: 0 auto 20px;
    display: flex; align-items: center; justify-content: center;
    border-radius: 50%;
    color: {accent};
    background: {accent_soft};
    border: 1px solid {ring};
    animation: pop 0.5s 0.12s cubic-bezier(0.2, 0.9, 0.3, 1.4) both;
  }}
  @keyframes pop {{ from {{ opacity: 0; transform: scale(0.6); }} to {{ opacity: 1; transform: scale(1); }} }}
  h1 {{ font-size: 20px; font-weight: 600; letter-spacing: 0.02em; color: #f2f5f9; }}
  .sub {{ margin-top: 12px; }}
  .sub p {{ font-size: 13.5px; line-height: 1.75; color: #97a3b6; }}
  .hint {{ margin-top: 26px; padding-top: 18px; border-top: 1px solid #212c42; font-size: 12px; color: #5d6b81; }}
</style>
</head>
<body>
  <div class="card">
    <div class="badge">{icon}</div>
    <h1>{title}</h1>
    <div class="sub">{}</div>
    <div class="hint">可以关闭此页面了 · AI Workbench</div>
  </div>
</body>
</html>"#,
        body.join("")
    )
}

/// 临时监听 127.0.0.1:17388 等官网回跳；拿到回调后关窗并返回查询串。
async fn wait_for_callback(timeout: Duration) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
        .await
        .map_err(|e| format!("回调端口 {CALLBACK_PORT} 被占用（可能有别的 Trae 授权在进行）: {e}"))?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let stream = match tokio::time::timeout_at(deadline, listener.accept()).await {
            Ok(result) => match result {
                Ok((s, _)) => s,
                Err(_) => continue,
            },
            Err(_) => return Err("授权超时（5 分钟内未完成登录）".to_string()),
        };
        let mut stream = stream;
        // 读请求头（回调就是 GET，无需读 body）。
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        })
        .await;
        let text = String::from_utf8_lossy(&buf);
        // 请求行：GET /authorize?... HTTP/1.1
        let query = text
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.split_once('?'))
            .map(|(_, q)| q.to_string())
            .unwrap_or_default();
        if query.is_empty() {
            let _ = stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
            continue;
        }
        let code = parse_callback_query(&query).auth_code;
        let (ok, title, lines): (bool, &str, Vec<&str>) = if code.is_empty() {
            (false, "授权失败", vec!["回调里没有拿到 AuthCode，请关闭本页并重试。"])
        } else {
            (true, "授权成功", vec!["已收到授权码，请回到 AI Workbench 查看结果。"])
        };
        let html = callback_html(ok, title, &lines);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(resp.as_bytes()).await;
        let _ = stream.flush().await;
        if ok {
            return Ok(query);
        }
        return Err("回调缺少 AuthCode".to_string());
    }
}

fn parse_exchange_tokens(body: &str) -> Option<ExchangePair> {
    // Result.Token / Result.RefreshToken 与嵌套变体都试一遍。
    if let Some(p) = parse_exchange(body) {
        return Some(p);
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let find = |keys: &[&str]| -> Option<String> {
        fn dig<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
            match v {
                Value::Object(m) => {
                    for k in keys {
                        if let Some(s) = m.get(*k).and_then(|x| x.as_str()) {
                            if !s.is_empty() {
                                return Some(s);
                            }
                        }
                    }
                    for (_, child) in m {
                        if let Some(s) = dig(child, keys) {
                            return Some(s);
                        }
                    }
                    None
                }
                Value::Array(a) => a.iter().find_map(|c| dig(c, keys)),
                _ => None,
            }
        }
        dig(&v, keys).map(str::to_string)
    };
    let access = find(&["AccessToken", "access_token", "Token"])?;
    if access.is_empty() {
        return None;
    }
    let refresh = find(&["RefreshToken", "refresh_token"]).unwrap_or_default();
    Some(ExchangePair { access, refresh })
}

fn exchange_variant_err(status: u16, body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = v.pointer("/ResponseMetadata/Error/Code").and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.parse().ok())));
    match code {
        Some(20101) => REAUTH_MESSAGE.to_string(),
        Some(c) => {
            let msg = v.pointer("/ResponseMetadata/Error/Message").and_then(|m| m.as_str()).unwrap_or("");
            format!("兑换失败（code {c}）{msg}")
        }
        None => format!("兑换失败（HTTP {status}）"),
    }
}

/// 三段兑换链：主端点（DeviceProof + DeviceInfo，需设备密钥对）→ 旧端点 AuthCode → 旧端点 Code。
async fn exchange_auth_code(
    client: &reqwest::Client,
    auth_code: &str,
    verifier: &str,
    identity: &TraeDeviceIdentity,
) -> Result<ExchangePair, String> {
    // v3 主端点：签名用 AuthCode 做 token 材料（客户端对 AuthCode 兑换即签它）。
    let (proof, info) = device_proof_parts(identity, "/trae/api/v3/oauth/ExchangeToken", auth_code, CLIENT_ID_IDE);
    // EP_EXCHANGE_V3 已是完整 URL（回调里的 host 仅是提示，不参与拼接）。
    let main_url = EP_EXCHANGE_V3.to_string();
    // (名字, URL, 主 body, 额外 header)：候选签名变体表，一行一个变体
    #[allow(clippy::type_complexity)]
    let variants: Vec<(String, String, Value, Vec<(&'static str, String)>)> = vec![
        (
            "DeviceInfo".to_string(),
            main_url,
            serde_json::json!({
                "ClientID": CLIENT_ID_IDE,
                "AuthCode": auth_code,
                "CodeVerifier": verifier,
                "DeviceInfo": info,
                "DeviceProof": proof,
                "IDEVersion": APP_VERSION,
            }),
            vec![
                ("x-device-id", identity.device_id.clone()),
                ("x-app-id", OAUTH_APP_ID.to_string()),
                ("x-platform-code", "IDE_PC".to_string()),
                ("x-cloudide-token", String::new()),
                ("User-Agent", format!("Trae/{APP_VERSION}")),
            ],
        ),
        (
            "AuthCode".to_string(),
            EP_EXCHANGE.to_string(),
            serde_json::json!({
                "ClientID": CLIENT_ID_IDE,
                "AuthCode": auth_code,
                "CodeVerifier": verifier,
                "DeviceID": identity.device_id,
                "PlatformCode": "IDE_PC",
            }),
            vec![
                ("x-device-id", identity.device_id.clone()),
                ("x-app-id", OAUTH_APP_ID.to_string()),
                ("x-platform-code", "IDE_PC".to_string()),
                ("User-Agent", format!("Trae/{APP_VERSION}")),
            ],
        ),
        (
            "Code".to_string(),
            EP_EXCHANGE.to_string(),
            serde_json::json!({
                "ClientID": CLIENT_ID_IDE,
                "Code": auth_code,
                "CodeVerifier": verifier,
                "DeviceID": identity.device_id,
                "PlatformCode": "IDE_PC",
            }),
            vec![
                ("x-device-id", identity.device_id.clone()),
                ("x-app-id", OAUTH_APP_ID.to_string()),
                ("x-platform-code", "IDE_PC".to_string()),
                ("User-Agent", format!("Trae/{APP_VERSION}")),
            ],
        ),
    ];
    let mut errors = Vec::new();
    for (tag, url, payload, headers) in variants {
        let body = payload.to_string();
        let (status, text) = post_json(client, &url, &headers, &body).await?;
        if (200..300).contains(&status) {
            if let Some(p) = parse_exchange_tokens(&text) {
                return Ok(p);
            }
        }
        errors.push(format!("{tag}: {}", exchange_variant_err(status, &text)));
    }
    Err(errors.join(" | "))
}

/// 授权登录入口：打开官网授权页 → 本地回调拿 AuthCode → 三段兑换 → 验证后入库。
#[tauri::command]
pub async fn trae_oauth_start(state: tauri::State<'_, DbState>) -> Result<QuotaAccountDto, String> {
    let cancel_rx = crate::quota_accounts::arm_login_cancel();
    let identity = read_device_identity().unwrap_or_default();
    if identity.device_id.is_empty() {
        return Err("未找到本机 Trae 客户端设备信息（需先安装并启动过 Trae CN）。可改用粘贴 refreshToken 方式。".to_string());
    }
    let verifier = crate::codex_quota::random_b64(48);
    let challenge = crate::codex_quota::pkce_challenge(&verifier);
    let trace_id = crate::quota_accounts::random_uuid();

    let url = trae_authorize_url(&identity, &challenge, &verifier, &trace_id);
    crate::cloudflared_commands::open_in_browser(url)?;

    let query = tokio::select! {
        q = wait_for_callback(LOGIN_TIMEOUT) => q?,
        _ = cancel_rx => return Err("已取消授权".to_string()),
    };

    let cb = parse_callback_query(&query);
    let auth_code = cb.auth_code;
    if auth_code.is_empty() {
        return Err("回调缺少 AuthCode".to_string());
    }
    // 回调直接带 refreshToken（官网登录态有效时）就不用再兑换；否则走三段兑换链。
    let client = http_client()?;
    let pair = if !cb.refresh_token.is_empty() {
        // access 未知：先用 refreshToken 换一份 access。
        let keys = stored_device_keys(&store_format(&cb.refresh_token, &identity_keys(&identity))).unwrap_or_default();
        exchange_token(&client, &cb.refresh_token, Some(&keys), "").await?
    } else {
        exchange_auth_code(&client, &auth_code, &verifier, &identity).await?
    };
    if pair.refresh.is_empty() {
        return Err("兑换响应缺少 RefreshToken（无法续期），已拒绝入库".to_string());
    }

    let uid = jwt_uid(&pair.access);
    let subject = if uid.is_empty() { refresh_subject(&pair.refresh) } else { format!("uid:{uid}") };
    // 展示名优先级：回调 userInfo 的昵称/邮箱 → JWT 昵称 → uid 主体串。
    let label = {
        let from_jwt = jwt_payload(&pair.access)
            .pointer("/data")
            .map(|d| {
                dig_string(
                    d,
                    &["nickname", "nickName", "NickName", "ScreenName", "screenName", "username", "userName", "name", "email"],
                )
            })
            .unwrap_or_default();
        [cb.name.clone(), from_jwt].into_iter().find(|s| !s.is_empty()).unwrap_or_else(|| subject.clone())
    };
    let keys = identity_keys(&identity);
    let combined = store_format(&pair.refresh, &keys);
    let id = with_db(Arc::clone(&state.conn), move |c| {
        upsert_account(c, Provider::Trae, &subject, Some(&label), &pair.access, &combined, now_unix())
    })
    .await?;
    with_db(Arc::clone(&state.conn), move |c| {
        crate::quota_accounts::list_accounts(c, Some(Provider::Trae))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

/// 本机 Trae 客户端设备身份（前端提示安装状态）。
#[tauri::command]
pub async fn trae_device_identity() -> Result<TraeDeviceIdentity, String> {
    Ok(read_device_identity().unwrap_or_default())
}

fn identity_keys(identity: &TraeDeviceIdentity) -> DeviceKeys {
    DeviceKeys {
        device_id: identity.device_id.clone(),
        machine_id: identity.machine_id.clone(),
        private_pem: identity.private_key_pem.clone(),
        public_pem: identity.public_key_pem.clone(),
    }
}



use std::sync::Mutex;

const SETTINGS_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS trae_settings (
        id INTEGER PRIMARY KEY CHECK(id = 1),
        checkin_enabled INTEGER NOT NULL DEFAULT 0,
        checkin_time TEXT NOT NULL DEFAULT '09:30',
        last_auto_checkin_date TEXT
    );
    INSERT OR IGNORE INTO trae_settings (id) VALUES (1);
    CREATE TABLE IF NOT EXISTS trae_checkin_log (
        account_id INTEGER PRIMARY KEY,
        last_ok_at INTEGER NOT NULL DEFAULT 0,
        last_date TEXT NOT NULL DEFAULT ''
    );
"#;

pub fn ensure_schema(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch(SETTINGS_SQL).map_err(|e| e.to_string())
}

/// 记录签到成功时刻（unix 秒）；只在跨天时覆盖，同日重签不刷新时间。
fn record_checkin_ok(conn: &rusqlite::Connection, account_id: i64, now: i64) {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let _ = conn.execute(
        "INSERT INTO trae_checkin_log (account_id, last_ok_at, last_date) VALUES (?1, ?2, ?3)
         ON CONFLICT(account_id) DO UPDATE SET
            last_ok_at = CASE WHEN excluded.last_date <> trae_checkin_log.last_date THEN excluded.last_ok_at ELSE trae_checkin_log.last_ok_at END,
            last_date = excluded.last_date",
        rusqlite::params![account_id, now, date],
    );
}

/// 今日已签的打卡时刻（秒）；跨天后失效返回 None。
fn checkin_ok_at(conn: &rusqlite::Connection, account_id: i64) -> Option<i64> {
    let (at, date): (i64, String) = conn
        .query_row(
            "SELECT last_ok_at, last_date FROM trae_checkin_log WHERE account_id = ?1",
            rusqlite::params![account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    (date == today && at > 0).then_some(at)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraeSettings {
    pub checkin_enabled: bool,
    pub checkin_time: String,
    pub last_auto_checkin_date: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraeSettingsPatch {
    pub checkin_enabled: Option<bool>,
    pub checkin_time: Option<String>,
}

pub fn get_settings(conn: &rusqlite::Connection) -> Result<TraeSettings, String> {
    conn.query_row(
        "SELECT checkin_enabled, checkin_time, last_auto_checkin_date FROM trae_settings WHERE id = 1",
        [],
        |row| {
            Ok(TraeSettings {
                checkin_enabled: row.get::<_, i64>(0)? != 0,
                checkin_time: row.get(1)?,
                last_auto_checkin_date: row.get(2)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

pub fn update_settings(conn: &rusqlite::Connection, patch: &TraeSettingsPatch) -> Result<TraeSettings, String> {
    conn.execute(
        "UPDATE trae_settings SET
            checkin_enabled = COALESCE(?1, checkin_enabled),
            checkin_time = COALESCE(?2, checkin_time),
            last_auto_checkin_date = COALESCE(?3, last_auto_checkin_date)
          WHERE id = 1",
        rusqlite::params![
            patch.checkin_enabled.map(i64::from),
            patch.checkin_time.as_deref(),
            Option::<String>::None,
        ],
    )
    .map_err(|e| e.to_string())?;
    get_settings(conn)
}

#[tauri::command]
pub async fn trae_get_settings(state: tauri::State<'_, DbState>) -> Result<TraeSettings, String> {
    with_db(Arc::clone(&state.conn), get_settings).await
}

#[tauri::command]
pub async fn trae_update_settings(
    state: tauri::State<'_, DbState>,
    patch: TraeSettingsPatch,
) -> Result<TraeSettings, String> {
    with_db(Arc::clone(&state.conn), move |c| update_settings(c, &patch)).await
}

async fn run_checkin_round(conn: Arc<Mutex<rusqlite::Connection>>) -> Result<Vec<TraeCheckinResult>, String> {
    let accounts = {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        crate::quota_accounts::list_accounts(&guard, Some(Provider::Trae))?
    };
    let mut results = Vec::new();
    for a in accounts {
        let state = DbState { conn: Arc::clone(&conn) };
        results.push(trae_checkin_shared(&state, a.id, a.label).await);
        tokio::time::sleep(crate::daily_schedule::BETWEEN_ACCOUNTS).await;
    }
    Ok(results)
}

async fn trae_checkin_shared(state: &DbState, id: i64, label: Option<String>) -> TraeCheckinResult {
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => return TraeCheckinResult::fail(id, label, e),
    };
    let t = match usable_token(state, &client, id).await {
        Ok(t) => t,
        Err(m) => return TraeCheckinResult::fail(id, label, m),
    };
    if let Ok((status, v)) = request_checkin(&client, EP_CHECKIN_STATUS, &t.access, &t.device_id).await {
        if (200..300).contains(&status) {
            if let Some((true, credits, _)) = parse_checkin_status_value(&v) {
                return TraeCheckinResult {
                    id,
                    label,
                    ok: true,
                    skipped: true,
                    points: None,
                    credits,
                    message: None,
                    needs_reauth: false,
                };
            }
        }
    }
    let (status, v) = match request_checkin(&client, EP_CHECKIN_CLAIM, &t.access, &t.device_id).await {
        Ok(r) => r,
        Err(m) => return TraeCheckinResult::fail(id, label, m),
    };
    let code = json_code(&v).unwrap_or(-1);
    if (200..300).contains(&status) && code == 0 {
        let points = v.pointer("/data/points").and_then(|p| p.as_f64());
        let credits = match request_checkin(&client, EP_CHECKIN_STATUS, &t.access, &t.device_id).await {
            Ok((s2, v2)) if (200..300).contains(&s2) => parse_checkin_status_value(&v2).and_then(|(_, c, _)| c),
            _ => None,
        };
        // 记录打卡时刻（今日首签时写入，跨天刷新）。
        let _ = with_db(Arc::clone(&state.conn), move |c| {
            record_checkin_ok(c, id, now_unix());
            Ok(())
        })
        .await;
        return TraeCheckinResult { id, label, ok: true, skipped: false, points, credits, message: None, needs_reauth: false };
    }
    if code == ALREADY_CODE {
        return TraeCheckinResult { id, label, ok: true, skipped: true, points: None, credits: None, message: None, needs_reauth: false };
    }
    let message = v
        .get("message")
        .or_else(|| v.get("msg"))
        .and_then(|m| m.as_str())
        .unwrap_or("签到失败")
        .to_string();
    TraeCheckinResult::fail(id, label, format!("签到失败（code {code}）：{message}"))
}

/// 手动「全部签到」。
#[tauri::command]
pub async fn trae_checkin_all(state: tauri::State<'_, DbState>) -> Result<Vec<TraeCheckinResult>, String> {
    run_checkin_round(Arc::clone(&state.conn)).await
}

/// 由 `daily_schedule::spawn` 每分钟调用一次。
pub async fn auto_tick(app: &tauri::AppHandle) -> Result<(), String> {
    use crate::daily_schedule::{self, ScheduleState};
    use tauri::Emitter;

    let conn = daily_schedule::conn_of(app);
    let app = app.clone();
    let _ran = daily_schedule::run_if_due(
        Arc::clone(&conn),
        "trae-checkin",
        |c| {
            let s = get_settings(c)?;
            Ok(ScheduleState {
                enabled: s.checkin_enabled,
                time: s.checkin_time,
                last_run_date: s.last_auto_checkin_date,
            })
        },
        |c, due| {
            c.execute(
                "UPDATE trae_settings SET last_auto_checkin_date = ?1 WHERE id = 1",
                rusqlite::params![due],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        },
        move |conn| {
            let app = app.clone();
            async move {
                match run_checkin_round(Arc::clone(&conn)).await {
                    Ok(results) => {
                        let _ = app.emit("trae-auto-checkin-done", &results);
                        Ok(())
                    }
                    Err(e) => {
                        let _ = with_db(conn, |c| {
                            c.execute(
                                "UPDATE trae_settings SET last_auto_checkin_date = '' WHERE id = 1",
                                [],
                            )
                            .map_err(|e| e.to_string())
                        })
                        .await;
                        Err(e)
                    }
                }
            }
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_uid_reads_data_id() {
        // {"exp":1900000000,"data":{"id":"u42"}} 的 URL-safe payload
        let payload = base64::engine::general_purpose::URL_SAFE.encode(r#"{"exp":1900000000,"data":{"id":"u42"}}"#);
        eprintln!("payload={payload}");
        let token = format!("head.{payload}.sig");
        let p = jwt_payload(&token);
        eprintln!("decoded={p}");
        assert_eq!(jwt_uid(&token), "u42");
        assert_eq!(jwt_exp(&token), 1_900_000_000);
    }

    #[test]
    fn jwt_uid_tolerates_missing_padding() {
        // 长度 % 4 == 2 的 payload 需要补 ==
        let raw = r#"{"data":{"id":7}}"#;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let token = format!("h.{payload}.s");
        assert_eq!(jwt_uid(&token), "7");
    }

    #[test]
    fn jwt_uid_empty_for_refresh_style_token() {
        assert_eq!(jwt_uid("AbCdEfGhIjKlMnOpQrStUvWxYz0123456789ABCD=.0000000000000000"), "");
    }

    #[test]
    fn refresh_subject_masks() {
        // 尾部取整个 refreshToken 串（含轮换段）的最后 4 个字符。
        let s = refresh_subject("AbCdEfGhIjKlMnOpQrStUvWxYz0123456789ABCD=.0000000000000000");
        assert_eq!(s, "AbCd…0000");
        assert!(refresh_subject("short=.x").is_empty());
    }

    #[test]
    fn parse_entitlement_reads_quota_and_usage() {
        let body = r#"{"code":0,"data":[
            {"entitlement_base_info":{"resource_name":"Pro","quota":{"credits_limit":"43.81"}},
             "usage":{"credits_amount":"12.5"}}
        ]}"#;
        let w = parse_entitlement(body);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].plan, "Pro");
        assert!((w[0].limit - 43.81).abs() < 0.01);
        assert!((w[0].used - 12.5).abs() < 0.01);
        assert!(parse_entitlement("nope").is_empty());
    }

    #[test]
    fn parse_usage_summary_reads_amounts() {
        let body = r#"{"usage_summary":{"total_amount":100,"consumed_amount":40}}"#;
        assert_eq!(parse_usage_summary(body), Some((100.0, 40.0)));
        assert_eq!(parse_usage_summary("{}"), None);
    }

    #[test]
    fn parse_checkin_status_shape() {
        let ok = parse_checkin_status(r#"{"code":0,"checked_in":true,"credits":1500,"enable":true}"#).unwrap();
        assert!(ok.0);
        assert_eq!(ok.1, Some(1500.0));
        assert!(ok.2);
        assert!(parse_checkin_status(r#"{"code":9074}"#).is_none());
    }

    #[test]
    fn store_format_roundtrip_keeps_device_info() {
        let keys = DeviceKeys {
            device_id: "123456789012345".into(),
            machine_id: "mid".into(),
            private_pem: "-----BEGIN PRIVATE KEY-----".into(),
            public_pem: "-----BEGIN PUBLIC KEY-----".into(),
        };
        let combined = store_format("RTTOKEN=.18dxxx", &keys);
        assert!(combined.starts_with("RTTOKEN=.18dxxx|"));
        let got = stored_device_keys(&combined).unwrap();
        assert_eq!(got.device_id, "123456789012345");
        assert_eq!(got.machine_id, "mid");
        assert_eq!(got.private_pem, keys.private_pem);
        assert_eq!(got.public_pem, keys.public_pem);
        assert_eq!(stored_tag_device(&combined), "123456789012345");
    }

    #[test]
    fn exchange_error_translates_codes() {
        assert_eq!(
            exchange_err_message(200, r#"{"ResponseMetadata":{"Error":{"Code":20101,"Message":"x"}}}"#),
            REAUTH_MESSAGE
        );
        assert!(exchange_err_message(500, "boom").contains("500"));
    }

    #[test]
    fn parse_exchange_reads_result() {
        let p = parse_exchange(r#"{"Result":{"Token":"eyJx","RefreshToken":"rt2"}}"#).unwrap();
        assert_eq!(p.access, "eyJx");
        assert_eq!(p.refresh, "rt2");
        assert!(parse_exchange(r#"{"Result":{"Token":""}}"#).is_none());
    }

    #[test]
    fn canonical_string_shape() {
        let s = canonical_string("/trae/api/v3/oauth/ExchangeToken", "RT", 1000, "n", "cid");
        assert_eq!(s, "POST\n/trae/api/v3/oauth/ExchangeToken\ncid\nRT\n1000\nn");
    }

    #[test]
    fn device_proof_signature_signs_with_local_key() {
        // 用本机客户端真实设备私钥试签（无私钥时应返回 None）。
        let identity = read_device_identity();
        match identity {
            Some(id) if !id.private_key_pem.is_empty() => {
                let sig = device_proof_signature(&id.private_key_pem, "POST\n/x\ncid\nrt\n1\nn");
                assert!(sig.is_some(), "本机私钥应能签名");
            }
            _ => {
                // 没有本机客户端的环境：空私钥必须返回 None，不产生假签名。
                assert_eq!(device_proof_signature("", "x"), None);
            }
        }
    }

    #[test]
    fn decrypt_storage_value_rejects_short_input() {
        assert!(decrypt_storage_value("aGVsbG8=").is_empty());
        assert!(decrypt_storage_value("!!!").is_empty());
    }

    #[test]
    fn local_storage_decrypt_produces_pem_keys() {
        // 本机客户端环境：解密 icube-dc 块应得到 PEM 密钥对；否则跳过。
        let Some(identity) = read_device_identity() else { return };
        eprintln!("device_id={}", identity.device_id);
        eprintln!("pub_sha256={:x?}", {
            let mut h = Sha256::new();
            h.update(identity.public_key_pem.as_bytes());
            h.finalize()
        });
        assert!(!identity.public_key_pem.is_empty(), "解密失败：AES 实现或盐有误");
        assert!(!identity.private_key_pem.is_empty());
        assert!(identity.public_key_pem.contains("BEGIN PUBLIC KEY"));
        assert!(identity.private_key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn aes128_sbox_known_values() {
        // FIPS-197 标准测试向量。
        let sbox = aes_sbox();
        assert_eq!(sbox[0x00], 0x63);
        assert_eq!(sbox[0x53], 0xED);
        assert_eq!(sbox[0xFF], 0x16);
    }

    #[test]
    fn aes128_expand_key_fips_vectors() {
        // FIPS-197 Appendix A.1：key 2b7e1516… → w4..w7 = a0fafe17 88542cb1 23a33939 2a6c7605。
        let key_a: [u8; 16] = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f, 0x3c,
        ];
        let rounds_a = aes128_expand_key(key_a);
        assert_eq!(&rounds_a[1][..4], &[0xa0, 0xfa, 0xfe, 0x17]);
        assert_eq!(&rounds_a[1][4..8], &[0x88, 0x54, 0x2c, 0xb1]);
        assert_eq!(&rounds_a[1][8..12], &[0x23, 0xa3, 0x39, 0x39]);
        assert_eq!(&rounds_a[1][12..16], &[0x2a, 0x6c, 0x76, 0x05]);
        // B.1 例：key 000102…0f → w4 = d6aa74fd。
        let key_b: [u8; 16] = (0..16u8).collect::<Vec<_>>().try_into().unwrap();
        let rounds_b = aes128_expand_key(key_b);
        assert_eq!(&rounds_b[1][..4], &[0xd6, 0xaa, 0x74, 0xfd]);
    }

    #[test]
    fn callback_html_escapes_and_carries_title() {
        let html = callback_html(true, "授权成功", &["已收到 <b>授权码</b>"]);
        assert!(html.contains("授权成功"));
        assert!(html.contains("&lt;b&gt;授权码&lt;/b&gt;"), "文案必须 HTML 转义");
        assert!(html.contains("svg"));
        let err = callback_html(false, "授权失败", &["缺少 AuthCode"]);
        assert!(err.contains("授权失败"));
    }

    #[test]
    fn callback_query_extracts_code_and_host_with_double_encoding() {
        let raw = "code=AbCd12%3D&host=https%3A%2F%2Fapi.trae.cn&loginTraceID=t1";
        let cb = parse_callback_query(raw);
        assert_eq!(cb.auth_code, "AbCd12=");
        assert_eq!(cb.host, "https://api.trae.cn");
        assert_eq!(cb.refresh_token, "");
    }

    #[test]
    fn callback_query_handles_second_round_encoding() {
        // 官方会把值再编码一层：%253A → %3A → ':'
        let raw = "code=abc%252Bdef&refreshToken=rt%253D.18d";
        let cb = parse_callback_query(raw);
        assert_eq!(cb.auth_code, "abc+def");
        assert_eq!(cb.refresh_token, "rt=.18d");
    }

    #[test]
    fn callback_query_reads_auth_code_info_json() {
        // 官网实际回跳形态：authCodeInfo={"AuthCode":"…","RefreshToken":"…","host":"…"}
        let raw = "isRedirect=true&scope=trae&authCodeInfo=%7B%22AuthCode%22%3A%22AC123%22%2C%22RefreshToken%22%3A%22rt%3D.18d%22%2C%22host%22%3A%22https%3A%2F%2Fapi.trae.cn%22%7D";
        let cb = parse_callback_query(raw);
        assert_eq!(cb.auth_code, "AC123");
        assert_eq!(cb.refresh_token, "rt=.18d");
        assert_eq!(cb.host, "https://api.trae.cn");
    }

    #[test]
    fn callback_query_reads_user_info_name() {
        // 回调带 userInfo JSON 时提取展示名。
        let raw = "code=c1&userInfo=%7B%22UserID%22%3A%221008080228%22%2C%22ScreenName%22%3A%22%E9%98%BF%E6%98%8E%22%7D";
        let cb = parse_callback_query(raw);
        assert_eq!(cb.auth_code, "c1");
        assert_eq!(cb.name, "阿明");
    }

    #[test]
    fn callback_query_falls_back_to_substring_extract() {
        // JSON 被截断时仍能抽出 AuthCode。
        let raw = "authCodeInfo=%7B%22AuthCode%22%3A%22AC9%22%2C%22Refresh";
        let cb = parse_callback_query(raw);
        assert_eq!(cb.auth_code, "AC9");
    }

    #[test]
    fn authorize_url_carries_pkce_and_callback() {
        let id = TraeDeviceIdentity {
            device_id: "123456789012345".into(),
            public_key_pem: String::new(),
            private_key_pem: String::new(),
            machine_id: String::new(),
        };
        let u = trae_authorize_url(&id, "chal123", "verifier", "trace1");
        assert!(u.starts_with("https://www.trae.cn/authorization?"));
        assert!(u.contains("code_challenge=chal123"));
        assert!(u.contains("code_challenge_method=S256"));
        assert!(u.contains(&format!("auth_callback_url=http%3A%2F%2F127.0.0.1%3A{CALLBACK_PORT}%2Fauthorize")));
        assert!(u.contains("client_id=ono9krqynydwx5"));
        // machineId 缺失时由 device_id 派生，且 machine_id 与 x_machine_id 同源。
        assert!(u.contains(&format!("machine_id={}", derived_machine_id("123456789012345"))));
    }

    #[test]
    fn derived_machine_id_is_stable_hex32() {
        let a = derived_machine_id("42");
        assert_eq!(a.len(), 32);
        assert_eq!(a, derived_machine_id("42"));
    }

    #[test]
    fn exchange_tokens_digs_nested_variants() {
        let p = parse_exchange_tokens(r#"{"Result":{"AccessToken":"a1","RefreshToken":"r1"}}"#).unwrap();
        assert_eq!(p.access, "a1");
        assert_eq!(p.refresh, "r1");
        // 深层变体：嵌套对象里找 access_token / refresh_token。
        let deep = parse_exchange_tokens(r#"{"data":{"session":{"access_token":"j","refresh_token":"rr"}}}"#).unwrap();
        assert_eq!(deep.access, "j");
        assert_eq!(deep.refresh, "rr");
        assert!(parse_exchange_tokens("nope").is_none());
    }

    #[test]
    fn mask_helper_unused_path() {
        // refresh_subject 与 mask_api_key 是两套掩码，前者只用于无 uid 时的兜底 subject。
        assert!(refresh_subject("AbCdEfGh=.0000").starts_with("AbCd"));
    }
}
