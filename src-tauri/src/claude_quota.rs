//! Claude（Claude Code / Claude Desktop 共用）额度：
//! 读本机 `~/.claude/.credentials.json` 的 OAuth，或粘贴 access token，
//! 查未公开的 `GET api.anthropic.com/api/oauth/usage`（与 Claude Code `/usage` 同源）。

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codex_quota::{http_client, now_unix, with_db};
use crate::quota_accounts::{mask_api_key, token_for, upsert_account, Provider, QuotaAccountDto};
use crate::DbState;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const REFRESH_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const OAUTH_BETA: &str = "oauth-2025-04-20";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeWindow {
    pub label: String,
    /// 已用百分比 0–100（与上游 utilization 一致）。
    pub used_percent: f64,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub label: Option<String>,
    pub plan: Option<String>,
    pub windows: Vec<ClaudeWindow>,
    pub needs_reauth: bool,
    pub at: i64,
}

const INVALID_TOKEN: &str = "OAuth 令牌无效或已过期，请重新导入";

impl ClaudeQuotaDto {
    fn err(id: i64, label: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            ok: false,
            needs_reauth: message == INVALID_TOKEN || message.contains("重新"),
            message: Some(message),
            label,
            plan: None,
            windows: Vec::new(),
            at: now_unix(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeOauthBlock {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_at: i64,
    #[serde(default)]
    subscription_type: Option<String>,
}

fn credentials_path() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let dir = PathBuf::from(home).join(".claude");
    for name in [".credentials.json", "credentials.json"] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn normalize_expires_at(raw: i64) -> i64 {
    // ≥ 1e11 → 毫秒；否则当秒。
    if raw >= 100_000_000_000 {
        raw / 1000
    } else {
        raw
    }
}

fn parse_resets_at(v: &Value) -> Option<i64> {
    let s = v.get("resets_at")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

fn parse_window(label: &str, v: Option<&Value>) -> Option<ClaudeWindow> {
    let obj = v?.as_object()?;
    let used = obj.get("utilization")?.as_f64()?;
    Some(ClaudeWindow {
        label: label.to_string(),
        used_percent: used.clamp(0.0, 100.0),
        resets_at: parse_resets_at(v?),
    })
}

pub fn parse_usage(body: &str) -> Option<Vec<ClaudeWindow>> {
    let root: Value = serde_json::from_str(body).ok()?;
    let mut windows = Vec::new();
    for (key, label) in [
        ("five_hour", "5 小时"),
        ("seven_day", "每周"),
        ("seven_day_opus", "每周 Opus"),
        ("seven_day_sonnet", "每周 Sonnet"),
    ] {
        if let Some(w) = parse_window(label, root.get(key)) {
            windows.push(w);
        }
    }
    Some(windows)
}

async fn request_usage(client: &reqwest::Client, access: &str) -> Result<Vec<ClaudeWindow>, String> {
    let resp = client
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {access}"))
        .header("anthropic-beta", OAUTH_BETA)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| crate::config::friendly_http_error(e, "请求失败"))?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(INVALID_TOKEN.to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("上游返回 {status}"));
    }
    parse_usage(&resp.text().await.unwrap_or_default()).ok_or_else(|| "用量响应解析失败".to_string())
}

async fn refresh_access(client: &reqwest::Client, refresh: &str) -> Result<(String, String, i64), String> {
    if refresh.is_empty() {
        return Err(INVALID_TOKEN.to_string());
    }
    let resp = client
        .post(REFRESH_URL)
        .header("Content-Type", "application/json")
        .header("anthropic-beta", OAUTH_BETA)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh,
        }))
        .send()
        .await
        .map_err(|e| crate::config::friendly_http_error(e, "刷新令牌失败"))?;
    let status = resp.status().as_u16();
    if status == 400 || status == 401 || status == 403 {
        return Err(INVALID_TOKEN.to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("刷新令牌上游返回 {status}"));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|_| "刷新令牌响应解析失败".to_string())?;
    let access = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "刷新响应缺少 access_token".to_string())?
        .to_string();
    let refresh_new = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or(refresh)
        .to_string();
    let expires_in = body.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(3600);
    Ok((access, refresh_new, now_unix() + expires_in))
}

fn read_local_oauth() -> Result<ClaudeOauthBlock, String> {
    let path = credentials_path().ok_or_else(|| "未找到本机 Claude Code 凭证（~/.claude/credentials.json）".to_string())?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读取凭证失败: {e}"))?;
    // 文件字段是 camelCase；也兼容部分蛇形导出。
    let root: Value = serde_json::from_str(&text).map_err(|_| "凭证文件不是合法 JSON".to_string())?;
    let block = root
        .get("claudeAiOauth")
        .or_else(|| root.get("claude_ai_oauth"))
        .ok_or_else(|| "凭证文件里没有 claudeAiOauth（当前可能是 API Key 模式）".to_string())?;
    let access = block
        .get("accessToken")
        .or_else(|| block.get("access_token"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "凭证缺少 accessToken".to_string())?
        .to_string();
    let refresh = block
        .get("refreshToken")
        .or_else(|| block.get("refresh_token"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let expires_raw = block
        .get("expiresAt")
        .or_else(|| block.get("expires_at"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let plan = block
        .get("subscriptionType")
        .or_else(|| block.get("subscription_type"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(ClaudeOauthBlock {
        access_token: access,
        refresh_token: refresh,
        expires_at: normalize_expires_at(expires_raw),
        subscription_type: plan,
    })
}

/// 从本机 Claude Code 凭证导入（验证用量接口通过才入库）。
#[tauri::command]
pub async fn claude_import_local(state: tauri::State<'_, DbState>) -> Result<QuotaAccountDto, String> {
    let oauth = read_local_oauth()?;
    let client = http_client()?;
    let mut access = oauth.access_token.clone();
    let mut refresh = oauth.refresh_token.clone();
    let mut expires = oauth.expires_at;
    if expires > 0 && expires <= now_unix() + 30 {
        let (a, r, e) = refresh_access(&client, &refresh).await?;
        access = a;
        refresh = r;
        expires = e;
    }
    request_usage(&client, &access).await?;
    let label = oauth
        .subscription_type
        .as_deref()
        .map(|p| format!("Claude · {p}"))
        .unwrap_or_else(|| mask_api_key(&access));
    let subject = mask_api_key(&access);
    let access_c = access.clone();
    let refresh_c = refresh.clone();
    let label_c = label.clone();
    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(
            c,
            Provider::Claude,
            &subject,
            Some(&label_c),
            &access_c,
            &refresh_c,
            now_unix(),
        )?;
        // expires_at 列：复用 upsert 后的 UPDATE
        c.execute(
            "UPDATE quota_accounts SET expires_at = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![expires, now_unix(), id],
        )
        .map_err(|e| e.to_string())?;
        crate::quota_accounts::list_accounts(c, Some(Provider::Claude))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

/// 粘贴 OAuth access token（可选 refresh）；验证通过才入库。
#[tauri::command]
pub async fn claude_add_token(
    state: tauri::State<'_, DbState>,
    access_token: String,
    refresh_token: Option<String>,
) -> Result<QuotaAccountDto, String> {
    let access = access_token.trim().to_string();
    if access.is_empty() {
        return Err("请填写 OAuth access token".to_string());
    }
    let refresh = refresh_token.unwrap_or_default().trim().to_string();
    let client = http_client()?;
    request_usage(&client, &access).await?;
    let subject = mask_api_key(&access);
    let label = mask_api_key(&access);
    with_db(Arc::clone(&state.conn), move |c| {
        let id = upsert_account(
            c,
            Provider::Claude,
            &subject,
            Some(&label),
            &access,
            &refresh,
            now_unix(),
        )?;
        crate::quota_accounts::list_accounts(c, Some(Provider::Claude))?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| "保存账号失败".to_string())
    })
    .await
}

#[tauri::command]
pub async fn claude_quota_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<ClaudeQuotaDto, String> {
    let stored = match token_for(Arc::clone(&state.conn), id, Provider::Claude).await {
        Ok(t) => t,
        Err(m) => return Ok(ClaudeQuotaDto::err(id, None, m)),
    };
    let client = http_client()?;
    let mut access = stored.access_token.clone();
    let mut refresh = stored.refresh_token.clone();
    if stored.expires_at > 0 && stored.expires_at <= now_unix() + 30 {
        match refresh_access(&client, &refresh).await {
            Ok((a, r, e)) => {
                access = a.clone();
                refresh = r.clone();
                let a2 = a;
                let r2 = r;
                let _ = with_db(Arc::clone(&state.conn), move |c| {
                    c.execute(
                        "UPDATE quota_accounts SET access_token = ?1, refresh_token = ?2, expires_at = ?3, updated_at = ?4 WHERE id = ?5",
                        rusqlite::params![a2, r2, e, now_unix(), id],
                    )
                    .map_err(|e| e.to_string())?;
                    Ok(())
                })
                .await;
            }
            Err(m) => return Ok(ClaudeQuotaDto::err(id, stored.label, m)),
        }
    }
    match request_usage(&client, &access).await {
        Ok(windows) => Ok(ClaudeQuotaDto {
            id,
            ok: true,
            message: None,
            label: stored.label,
            plan: None,
            windows,
            needs_reauth: false,
            at: now_unix(),
        }),
        Err(m) if m == INVALID_TOKEN && !refresh.is_empty() => {
            match refresh_access(&client, &refresh).await {
                Ok((a, r, e)) => {
                    let a2 = a.clone();
                    let r2 = r;
                    let _ = with_db(Arc::clone(&state.conn), move |c| {
                        c.execute(
                            "UPDATE quota_accounts SET access_token = ?1, refresh_token = ?2, expires_at = ?3, updated_at = ?4 WHERE id = ?5",
                            rusqlite::params![a2, r2, e, now_unix(), id],
                        )
                        .map_err(|e| e.to_string())?;
                        Ok(())
                    })
                    .await;
                    match request_usage(&client, &a).await {
                        Ok(windows) => Ok(ClaudeQuotaDto {
                            id,
                            ok: true,
                            message: None,
                            label: stored.label,
                            plan: None,
                            windows,
                            needs_reauth: false,
                            at: now_unix(),
                        }),
                        Err(m2) => Ok(ClaudeQuotaDto::err(id, stored.label, m2)),
                    }
                }
                Err(m2) => Ok(ClaudeQuotaDto::err(id, stored.label, m2)),
            }
        }
        Err(m) => Ok(ClaudeQuotaDto::err(id, stored.label, m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_usage_reads_windows() {
        let body = r#"{
          "five_hour": { "utilization": 6.0, "resets_at": "2026-04-08T18:59:59Z" },
          "seven_day": { "utilization": 35.0, "resets_at": "2026-04-14T16:59:59Z" },
          "seven_day_opus": null
        }"#;
        let w = parse_usage(body).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].label, "5 小时");
        assert!((w[0].used_percent - 6.0).abs() < 0.01);
        assert!(w[0].resets_at.is_some());
    }

    #[test]
    fn expires_at_ms_normalized() {
        assert_eq!(normalize_expires_at(1_759_700_000_000), 1_759_700_000);
        assert_eq!(normalize_expires_at(1_759_700_000), 1_759_700_000);
    }
}
