//! ZCode（智谱 BigModel）额度：粘贴开放平台 API Key，
//! 查 `GET open.bigmodel.cn/api/monitor/usage/quota/limit`。
//! Authorization 头直接传 Key（不是 Bearer）。
//! 响应字段语义有陷阱（实测 Lite 套餐 + cc-switch #1588 新老套餐样例）：
//! `usage` 是窗口总量、`currentValue` 才是已用、`percentage` 是已用百分比；
//! 新套餐 TOKENS_LIMIT 条目只有 percentage；`level` 是套餐等级。

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use crate::codex_quota::{http_client, now_unix, with_db};
use crate::quota_accounts::{mask_api_key, token_for, upsert_account, Provider, QuotaAccountDto};
use crate::DbState;

const QUOTA_URL: &str = "https://open.bigmodel.cn/api/monitor/usage/quota/limit";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZcodeLimit {
    pub label: String,
    /// 已用（上游 currentValue，缺失时由总量-剩余推导）。
    pub used: f64,
    /// 窗口总量（上游 usage）；只给百分比的条目为 0。
    pub total: f64,
    pub remaining: Option<f64>,
    /// 已用百分比（上游 percentage，缺失时由 used/total 推导）。
    pub used_percent: Option<f64>,
    /// 窗口重置时间（毫秒时间戳）。
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZcodeQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
    pub level: Option<String>,
    pub limits: Vec<ZcodeLimit>,
    pub needs_reauth: bool,
    pub at: i64,
}

const INVALID_KEY: &str = "API Key 无效或已失效，请更换";

impl ZcodeQuotaDto {
    fn err(id: i64, label: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            ok: false,
            needs_reauth: message == INVALID_KEY || message.contains("更换"),
            message: Some(message),
            label,
            level: None,
            limits: Vec::new(),
            at: now_unix(),
        }
    }
}

fn num_field(v: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        if let Some(n) = v.get(*k).and_then(|x| x.as_f64()) {
            return Some(n);
        }
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            if let Ok(n) = s.parse::<f64>() {
                return Some(n);
            }
        }
    }
    None
}

fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// unit 编码取自线上响应实测：3 = 小时窗（number 为小时数），6 = 周窗（number 为周数）。
fn limit_label(limit_type: &str, unit: Option<f64>, number: Option<f64>) -> String {
    match limit_type.to_ascii_uppercase().as_str() {
        "CREDIT_LIMIT" | "TOKENS_LIMIT" | "TOKEN_LIMIT" => match (unit, number) {
            (Some(3.0), Some(n)) if n > 0.0 => format!("{} 小时额度", n as i64),
            (Some(6.0), Some(1.0)) => "每周额度".to_string(),
            (Some(6.0), Some(n)) if n > 0.0 => format!("{} 周额度", n as i64),
            _ => {
                if limit_type.eq_ignore_ascii_case("CREDIT_LIMIT") {
                    "积分额度".to_string()
                } else {
                    "Tokens".to_string()
                }
            }
        },
        "TIME_LIMIT" | "REQUESTS_LIMIT" | "REQUEST_LIMIT" => "次数额度".to_string(),
        other if other.is_empty() => "额度".to_string(),
        other => other.to_string(),
    }
}

fn parse_limit_item(item: &Value) -> Option<ZcodeLimit> {
    let limit_type = str_field(item, &["type", "limitType", "limit_type"]).unwrap_or_default();
    let unit = num_field(item, &["unit"]);
    let number = num_field(item, &["number"]);
    let total = num_field(item, &["usage", "total", "limit", "quota", "max"]);
    let remaining = num_field(item, &["remaining"]);
    let used = num_field(item, &["currentValue", "used"]).or_else(|| match (total, remaining) {
        (Some(t), Some(r)) if t > 0.0 => Some((t - r).max(0.0)),
        _ => None,
    });
    let remaining = remaining.or_else(|| match (total, used) {
        (Some(t), Some(u)) if t > 0.0 => Some((t - u).max(0.0)),
        _ => None,
    });
    let used_percent =
        num_field(item, &["percentage", "usedPercent", "used_percent"]).or_else(|| match (used, total) {
            (Some(u), Some(t)) if t > 0.0 => Some(u / t * 100.0),
            _ => None,
        });
    // 一个用量数字都没有的条目没有展示价值
    if used.is_none() && used_percent.is_none() && remaining.is_none() {
        return None;
    }
    let reset_at = num_field(item, &["nextResetTime", "next_reset_time"]).map(|ms| ms as i64);
    Some(ZcodeLimit {
        label: limit_label(&limit_type, unit, number),
        used: used.unwrap_or(0.0),
        total: total.unwrap_or(0.0),
        remaining,
        used_percent,
        reset_at,
    })
}

#[derive(Debug)]
pub struct ZcodeQuotaBody {
    pub level: Option<String>,
    pub limits: Vec<ZcodeLimit>,
}

pub fn parse_quota(body: &str) -> Result<ZcodeQuotaBody, String> {
    let root: Value = serde_json::from_str(body).map_err(|_| "配额响应不是合法 JSON".to_string())?;
    // 形态：{ code, msg, data: { limits: [...], level }, success }
    let code = root.get("code").and_then(|c| c.as_i64()).or_else(|| {
        root.get("code")
            .and_then(|c| c.as_str())
            .and_then(|s| s.parse().ok())
    });
    if let Some(c) = code {
        if c != 200 && c != 0 {
            let msg = root
                .get("msg")
                .or_else(|| root.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("上游返回错误");
            return Err(msg.to_string());
        }
    }
    let data = root.get("data").unwrap_or(&root);
    let level = str_field(data, &["level"]);
    let limits_val = data
        .get("limits")
        .or_else(|| data.get("limitList"))
        .or_else(|| data.as_array().map(|_| data))
        .ok_or_else(|| "响应里没有 limits".to_string())?;
    let arr = limits_val
        .as_array()
        .ok_or_else(|| "limits 不是数组".to_string())?;
    let limits = arr.iter().filter_map(parse_limit_item).collect();
    Ok(ZcodeQuotaBody { level, limits })
}

/// 上游认证失败常以 HTTP 200 + 业务码返回（如 `{"code":1000,"msg":"身份验证失败"}`）。
fn is_auth_failure(msg: &str) -> bool {
    let m = msg.trim();
    m.contains("身份验证失败")
        || m.contains("令牌已过期")
        || m.contains("令牌无效")
        || m.contains("未授权")
        || m.eq_ignore_ascii_case("unauthorized")
}

pub async fn request_quota(client: &reqwest::Client, api_key: &str) -> Result<ZcodeQuotaBody, String> {
    let resp = client
        .get(QUOTA_URL)
        .header("Authorization", api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| crate::config::friendly_http_error(e, "请求失败"))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(INVALID_KEY.to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("上游返回 {status}"));
    }
    let body = resp.text().await.unwrap_or_default();
    match parse_quota(&body) {
        Ok(v) => Ok(v),
        // 认证失败统一映射成 INVALID_KEY，前端才会给出「更换 Key」引导
        Err(m) if is_auth_failure(&m) => Err(INVALID_KEY.to_string()),
        Err(m) => Err(m),
    }
}

/// 先查一次配额验证 Key，通过才入库。label 为选填账号备注；
/// 同一 Key 重复添加时按 subject 原位更新，备注留空则回退为掩码 Key。
#[tauri::command]
pub async fn zcode_add_key(
    state: tauri::State<'_, DbState>,
    api_key: String,
    label: Option<String>,
) -> Result<QuotaAccountDto, String> {
    let key = api_key.trim().to_string();
    if key.is_empty() {
        return Err("请填写 API Key".to_string());
    }
    let label = label
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    request_quota(&http_client()?, &key).await?;
    let masked = mask_api_key(&key);
    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(
            c,
            Provider::Zcode,
            &masked,
            label.as_deref().or(Some(masked.as_str())),
            &key,
            "",
            now_unix(),
        )?;
        crate::quota_accounts::list_accounts(c, Some(Provider::Zcode))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

#[tauri::command]
pub async fn zcode_quota_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<ZcodeQuotaDto, String> {
    let stored = match token_for(Arc::clone(&state.conn), id, Provider::Zcode).await {
        Ok(t) => t,
        Err(m) => return Ok(ZcodeQuotaDto::err(id, None, m)),
    };
    match request_quota(&http_client()?, &stored.access_token).await {
        Ok(body) => Ok(ZcodeQuotaDto {
            id,
            ok: true,
            message: None,
            label: stored.label,
            level: body.level,
            limits: body.limits,
            needs_reauth: false,
            at: now_unix(),
        }),
        Err(m) => Ok(ZcodeQuotaDto::err(id, stored.label, m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测 Lite 套餐响应（2026-10-16 线上原样，含 5 小时窗 + 每周窗）。
    const LITE_BODY: &str = r#"{"code":200,"msg":"操作成功","data":{"limits":[{"type":"CREDIT_LIMIT","unit":3,"number":5,"usage":2000,"currentValue":385,"remaining":1614,"percentage":19,"nextResetTime":1792139990212},{"type":"CREDIT_LIMIT","unit":6,"number":1,"usage":2000,"currentValue":385,"remaining":1614,"percentage":19,"nextResetTime":1792689917965}],"level":"lite"},"success":true}"#;

    #[test]
    fn parse_credit_limit_lite() {
        let body = parse_quota(LITE_BODY).unwrap();
        assert_eq!(body.level.as_deref(), Some("lite"));
        assert_eq!(body.limits.len(), 2);
        assert_eq!(body.limits[0].label, "5 小时额度");
        assert_eq!(body.limits[1].label, "每周额度");
        assert!((body.limits[0].used - 385.0).abs() < 0.01);
        assert!((body.limits[0].total - 2000.0).abs() < 0.01);
        assert!((body.limits[0].remaining.unwrap() - 1614.0).abs() < 0.01);
        assert!((body.limits[0].used_percent.unwrap() - 19.0).abs() < 0.01);
        assert_eq!(body.limits[0].reset_at, Some(1792139990212));
    }

    /// cc-switch #1588 新套餐样例：TIME_LIMIT 带绝对量，TOKENS_LIMIT 只有百分比。
    #[test]
    fn parse_new_plan_percentage_only() {
        let body = r#"{"code":200,"msg":"操作成功","data":{"limits":[
            {"type":"TIME_LIMIT","percentage":7,"usage":1000,"currentValue":72,"remaining":928},
            {"type":"TOKENS_LIMIT","percentage":44},
            {"type":"TOKENS_LIMIT","percentage":53}],"level":"pro"},"success":true}"#;
        let parsed = parse_quota(body).unwrap();
        assert_eq!(parsed.level.as_deref(), Some("pro"));
        assert_eq!(parsed.limits.len(), 3);
        assert_eq!(parsed.limits[0].label, "次数额度");
        assert!((parsed.limits[0].used - 72.0).abs() < 0.01);
        assert!((parsed.limits[0].total - 1000.0).abs() < 0.01);
        assert!((parsed.limits[1].used_percent.unwrap() - 44.0).abs() < 0.01);
        assert_eq!(parsed.limits[1].total, 0.0);
        assert_eq!(parsed.limits[2].used_percent.unwrap(), 53.0);
    }

    /// 老套餐样例：TOKENS_LIMIT 用 unit/number 标 5 小时窗，只有百分比 + 重置时间。
    #[test]
    fn parse_old_plan_window() {
        let body = r#"{"code":200,"data":{"limits":[
            {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":44,"nextResetTime":1774967594803}],"level":"pro"}}"#;
        let parsed = parse_quota(body).unwrap();
        assert_eq!(parsed.limits.len(), 1);
        assert_eq!(parsed.limits[0].label, "5 小时额度");
        assert!((parsed.limits[0].used_percent.unwrap() - 44.0).abs() < 0.01);
        assert_eq!(parsed.limits[0].reset_at, Some(1774967594803));
    }

    #[test]
    fn auth_failure_body_maps_to_invalid_key_message() {
        // 上游认证失败走 HTTP 200 + 业务码，解析层透传 msg，请求层再映射为 INVALID_KEY
        let body = r#"{"code":1000,"msg":"身份验证失败。","success":false}"#;
        assert_eq!(parse_quota(body).unwrap_err(), "身份验证失败。");
        assert!(is_auth_failure("身份验证失败。"));
        assert!(is_auth_failure("令牌已过期或验证不正确"));
        assert!(!is_auth_failure("操作成功"));
        assert!(!is_auth_failure("余额不足"));
    }

    #[test]
    fn upstream_error_code_returns_msg() {
        let body = r#"{"code":1113,"msg":"余额不足","success":false}"#;
        assert_eq!(parse_quota(body).unwrap_err(), "余额不足");
    }

    #[test]
    fn item_without_any_usage_is_skipped() {
        let body = r#"{"code":200,"data":{"limits":[{"type":"CREDIT_LIMIT","unit":3,"number":5}],"level":"lite"}}"#;
        let parsed = parse_quota(body).unwrap();
        assert!(parsed.limits.is_empty());
    }
}
