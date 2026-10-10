//! DeepSeek 额度：开放平台 API Key → `GET api.deepseek.com/user/balance`。
//! Key 存在 `quota_accounts`（provider = deepseek），DTO 只带掩码后的展示名。

use std::sync::Arc;

use serde::Serialize;

use crate::codex_quota::{http_client, now_unix};
use crate::quota_accounts::{token_for, Provider};
use crate::DbState;

const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSeekBalance {
    pub currency: String,
    pub total: f64,
    pub granted: f64,
    pub topped_up: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepSeekQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
    pub is_available: Option<bool>,
    pub balances: Vec<DeepSeekBalance>,
    /// Key 被拒（401/403）：前端据此给出「更换 Key」。
    pub needs_reauth: bool,
    pub at: i64,
}

const INVALID_KEY_MESSAGE: &str = "API Key 无效或已被撤销";

impl DeepSeekQuotaDto {
    fn err(id: i64, label: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            ok: false,
            needs_reauth: message == INVALID_KEY_MESSAGE,
            message: Some(message),
            label,
            is_available: None,
            balances: Vec::new(),
            at: now_unix(),
        }
    }
}

fn num(v: &serde_json::Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0.0)
}

pub fn parse_balance(body: &str) -> Option<(Option<bool>, Vec<DeepSeekBalance>)> {
    let root: serde_json::Value = serde_json::from_str(body).ok()?;
    let infos = root.get("balance_infos")?.as_array()?;
    let balances = infos
        .iter()
        .filter_map(|b| {
            Some(DeepSeekBalance {
                currency: b.get("currency")?.as_str()?.to_string(),
                total: b.get("total_balance").map(num).unwrap_or(0.0),
                granted: b.get("granted_balance").map(num).unwrap_or(0.0),
                topped_up: b.get("topped_up_balance").map(num).unwrap_or(0.0),
            })
        })
        .collect();
    Some((root.get("is_available").and_then(|v| v.as_bool()), balances))
}

pub(crate) async fn request_balance(
    client: &reqwest::Client,
    api_key: &str,
) -> Result<(Option<bool>, Vec<DeepSeekBalance>), String> {
    let resp = client
        .get(BALANCE_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(INVALID_KEY_MESSAGE.to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("上游返回 {status}"));
    }
    parse_balance(&resp.text().await.unwrap_or_default()).ok_or_else(|| "余额响应解析失败".to_string())
}

#[tauri::command]
pub async fn deepseek_balance_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<DeepSeekQuotaDto, String> {
    let stored = match token_for(Arc::clone(&state.conn), id, Provider::DeepSeek).await {
        Ok(t) => t,
        Err(m) => return Ok(DeepSeekQuotaDto::err(id, None, m)),
    };
    match request_balance(&http_client()?, &stored.access_token).await {
        Ok((is_available, balances)) => Ok(DeepSeekQuotaDto {
            id,
            ok: true,
            message: None,
            label: stored.label,
            is_available,
            balances,
            needs_reauth: false,
            at: now_unix(),
        }),
        Err(m) => Ok(DeepSeekQuotaDto::err(id, stored.label, m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_key_flags_needs_reauth() {
        assert!(DeepSeekQuotaDto::err(1, None, INVALID_KEY_MESSAGE).needs_reauth);
        assert!(!DeepSeekQuotaDto::err(1, None, "超时").needs_reauth);
    }

    #[test]
    fn parse_balance_reads_string_amounts() {
        let body = r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"}]}"#;
        let (avail, list) = parse_balance(body).expect("parse");
        assert_eq!(avail, Some(true));
        assert_eq!(
            list,
            vec![DeepSeekBalance { currency: "CNY".into(), total: 110.0, granted: 10.0, topped_up: 100.0 }]
        );
    }

    #[test]
    fn parse_balance_rejects_garbage() {
        assert!(parse_balance("").is_none());
        assert!(parse_balance(r#"{"error":{"message":"bad key"}}"#).is_none());
        let (_, empty) = parse_balance(r#"{"is_available":false,"balance_infos":[]}"#).unwrap();
        assert!(empty.is_empty());
    }
}
