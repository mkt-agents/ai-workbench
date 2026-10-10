//! ZCode（智谱 BigModel）额度：粘贴开放平台 API Key，
//! 查 `GET open.bigmodel.cn/api/monitor/usage/quota/limit`。
//! Authorization 头直接传 Key（不是 Bearer）。

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
    pub usage: f64,
    pub limit: f64,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZcodeQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
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
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if let Ok(n) = s.parse::<f64>() {
                return Some(n);
            }
        }
    }
    None
}

fn limit_label(limit_type: &str) -> String {
    match limit_type.to_ascii_uppercase().as_str() {
        "TOKENS_LIMIT" | "TOKEN_LIMIT" => "Tokens".to_string(),
        "REQUESTS_LIMIT" | "REQUEST_LIMIT" => "请求次数".to_string(),
        other if other.is_empty() => "额度".to_string(),
        other => other.to_string(),
    }
}

pub fn parse_quota(body: &str) -> Result<Vec<ZcodeLimit>, String> {
    let root: Value = serde_json::from_str(body).map_err(|_| "配额响应不是合法 JSON".to_string())?;
    // 常见形态：{ code, data: { limits: [...] } } 或 data 直接是数组 / limits 在根上。
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
    let limits_val = data
        .get("limits")
        .or_else(|| data.get("limitList"))
        .or_else(|| data.as_array().map(|_| data))
        .ok_or_else(|| "响应里没有 limits".to_string())?;
    let arr = limits_val
        .as_array()
        .ok_or_else(|| "limits 不是数组".to_string())?;
    let mut out = Vec::new();
    for item in arr {
        let limit_type = item
            .get("limitType")
            .or_else(|| item.get("limit_type"))
            .or_else(|| item.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let usage = num_field(item, &["usage", "used", "currentUsage"]).unwrap_or(0.0);
        let limit = match num_field(item, &["limit", "total", "quota", "max"]) {
            Some(n) if n > 0.0 => n,
            _ => continue,
        };
        let unit = item
            .get("unit")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        out.push(ZcodeLimit {
            label: limit_label(limit_type),
            usage,
            limit,
            unit,
        });
    }
    Ok(out)
}

pub async fn request_quota(client: &reqwest::Client, api_key: &str) -> Result<Vec<ZcodeLimit>, String> {
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
    parse_quota(&resp.text().await.unwrap_or_default())
}

/// 先查一次配额验证 Key，通过才入库。
#[tauri::command]
pub async fn zcode_add_key(state: tauri::State<'_, DbState>, api_key: String) -> Result<QuotaAccountDto, String> {
    let key = api_key.trim().to_string();
    if key.is_empty() {
        return Err("请填写 API Key".to_string());
    }
    request_quota(&http_client()?, &key).await?;
    let masked = mask_api_key(&key);
    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(c, Provider::Zcode, &masked, Some(&masked), &key, "", now_unix())?;
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
        Ok(limits) => Ok(ZcodeQuotaDto {
            id,
            ok: true,
            message: None,
            label: stored.label,
            limits,
            needs_reauth: false,
            at: now_unix(),
        }),
        Err(m) => Ok(ZcodeQuotaDto::err(id, stored.label, m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tokens_limit() {
        let body = r#"{
          "code": 200,
          "data": {
            "limits": [
              { "limitType": "TOKENS_LIMIT", "usage": 1200, "limit": 10000, "unit": "tokens" },
              { "limitType": "REQUESTS_LIMIT", "usage": 3, "limit": 0 }
            ]
          }
        }"#;
        let limits = parse_quota(body).unwrap();
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].label, "Tokens");
        assert!((limits[0].usage - 1200.0).abs() < 0.01);
        assert!((limits[0].limit - 10000.0).abs() < 0.01);
    }
}
