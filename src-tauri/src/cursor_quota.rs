//! Cursor 额度：用看板里授权登录得来的令牌（`quota_accounts`）→
//! `aiserver.v1.DashboardService/GetCurrentPeriodUsage` 拉当前账期用量。
//! DTO 不含任何 token 字段。

use std::sync::Arc;

use serde::Serialize;

use crate::codex_quota::REAUTH_MESSAGE;
use crate::quota_accounts::{token_for, Provider};
use crate::DbState;

const USAGE_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
const PROFILE_URL: &str = "https://api2.cursor.sh/auth/full_stripe_profile";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    pub email: Option<String>,
    pub membership_type: Option<String>,
    pub team_name: Option<String>,
    pub cycle_start_ms: Option<i64>,
    pub cycle_end_ms: Option<i64>,
    pub total_spend: Option<f64>,
    pub included_spend: Option<f64>,
    pub remaining: Option<f64>,
    pub limit: Option<f64>,
    /// "Cursor Models" 池已用百分比（服务端口径，含 Cursor Grok 与 Composer）。
    pub auto_percent_used: Option<f64>,
    /// "Other Models" 池已用百分比。
    pub api_percent_used: Option<f64>,
    /// 总已用百分比——Cursor 自己的主读数（"used N% of your included total usage"）。
    pub total_percent_used: Option<f64>,
    /// 套餐外赠送用量（美分）；totalSpend = includedSpend + bonusSpend。
    pub bonus_spend: Option<f64>,
    /// "team" = 企业席位，limit 口径是团队共享池；"individual" = 个人。
    pub limit_type: Option<String>,
    pub pooled_used: Option<f64>,
    pub display_message: Option<String>,
    pub needs_reauth: bool,
    pub at: i64,
}

impl CursorQuotaDto {
    fn err(id: i64, email: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            ok: false,
            needs_reauth: message == REAUTH_MESSAGE,
            message: Some(message),
            email,
            membership_type: None,
            team_name: None,
            cycle_start_ms: None,
            cycle_end_ms: None,
            total_spend: None,
            included_spend: None,
            remaining: None,
            limit: None,
            auto_percent_used: None,
            api_percent_used: None,
            total_percent_used: None,
            bonus_spend: None,
            limit_type: None,
            pooled_used: None,
            display_message: None,
            at: now_unix(),
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `auth/full_stripe_profile` 里的套餐类型（pro / free_trial / enterprise…）；缺省不影响用量。
pub fn parse_membership(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("membershipType")
        .and_then(|m| m.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn fetch_membership(client: &reqwest::Client, token: &str) -> Option<String> {
    let resp = client
        .get(PROFILE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    parse_membership(&resp.text().await.ok()?)
}

/// GetCurrentPeriodUsage 响应（connect-json：数值是数字，epoch 是毫秒数字符串）。
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodUsage {
    pub cycle_start_ms: Option<i64>,
    pub cycle_end_ms: Option<i64>,
    pub total_spend: Option<f64>,
    pub included_spend: Option<f64>,
    pub remaining: Option<f64>,
    pub limit: Option<f64>,
    pub auto_percent_used: Option<f64>,
    pub api_percent_used: Option<f64>,
    pub total_percent_used: Option<f64>,
    pub bonus_spend: Option<f64>,
    pub limit_type: Option<String>,
    pub pooled_used: Option<f64>,
    pub display_message: Option<String>,
}

fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn as_ms(v: &serde_json::Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    v.as_str()?.trim().parse().ok()
}

pub fn parse_current_period_usage(body: &str) -> Option<PeriodUsage> {
    let root: serde_json::Value = serde_json::from_str(body).ok()?;
    let plan = root.get("planUsage")?;
    let total_spend = plan.get("totalSpend").and_then(as_f64);
    let limit = plan.get("limit").and_then(as_f64);
    // 至少要有 totalSpend / limit 之一才算解析成功。
    if total_spend.is_none() && limit.is_none() {
        return None;
    }
    Some(PeriodUsage {
        cycle_start_ms: root.get("billingCycleStart").and_then(as_ms),
        cycle_end_ms: root.get("billingCycleEnd").and_then(as_ms),
        total_spend,
        included_spend: plan.get("includedSpend").and_then(as_f64),
        remaining: plan.get("remaining").and_then(as_f64),
        limit,
        auto_percent_used: plan.get("autoPercentUsed").and_then(as_f64),
        api_percent_used: plan.get("apiPercentUsed").and_then(as_f64),
        total_percent_used: plan.get("totalPercentUsed").and_then(as_f64),
        bonus_spend: plan.get("bonusSpend").and_then(as_f64),
        limit_type: root
            .get("spendLimitUsage")
            .and_then(|s| s.get("limitType"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        pooled_used: root
            .get("spendLimitUsage")
            .and_then(|s| s.get("pooledUsed"))
            .and_then(as_f64),
        display_message: root
            .get("displayMessage")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

#[tauri::command]
pub async fn cursor_quota_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<CursorQuotaDto, String> {
    let stored = match token_for(Arc::clone(&state.conn), id, Provider::Cursor).await {
        Ok(t) => t,
        Err(message) => return Ok(CursorQuotaDto::err(id, None, message)),
    };
    let email = stored.label.clone();
    let token = stored.access_token;

    let client = crate::codex_quota::http_client()?;
    let resp = client
        .post(USAGE_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "connectrpc/1.0 cursor/3.24.9")
        .body("{}")
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return Ok(CursorQuotaDto::err(id, email, format!("请求失败: {e}"))),
    };
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Ok(CursorQuotaDto::err(id, email, REAUTH_MESSAGE));
    }
    if !resp.status().is_success() {
        return Ok(CursorQuotaDto::err(id, email, format!("上游返回 {status}")));
    }
    let body = resp.text().await.unwrap_or_default();
    let Some(usage) = parse_current_period_usage(&body) else {
        return Ok(CursorQuotaDto::err(id, email, "用量响应解析失败"));
    };
    let membership_type = fetch_membership(&client, &token).await;

    Ok(CursorQuotaDto {
        id,
        ok: true,
        message: None,
        email,
        membership_type,
        team_name: None,
        cycle_start_ms: usage.cycle_start_ms,
        cycle_end_ms: usage.cycle_end_ms,
        total_spend: usage.total_spend,
        included_spend: usage.included_spend,
        remaining: usage.remaining,
        limit: usage.limit,
        auto_percent_used: usage.auto_percent_used,
        api_percent_used: usage.api_percent_used,
        total_percent_used: usage.total_percent_used,
        bonus_spend: usage.bonus_spend,
        limit_type: usage.limit_type,
        pooled_used: usage.pooled_used,
        display_message: usage.display_message,
        needs_reauth: false,
        at: now_unix(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_BODY: &str = r#"{"billingCycleStart":"1790930925000","billingCycleEnd":"1793609325000","planUsage":{"totalSpend":786,"includedSpend":786,"remaining":1214,"limit":2000,"remainingBonus":false,"autoPercentUsed":0,"apiPercentUsed":9.825,"totalPercentUsed":9.825},"spendLimitUsage":{"pooledUsed":0,"limitType":"team"},"displayThreshold":200,"enabled":true,"displayMessage":"You've used 39% of your included usage"}"#;

    #[test]
    fn parse_current_period_usage_reads_plan_and_cycle() {
        let p = parse_current_period_usage(LIVE_BODY).expect("parse");
        assert_eq!(p.cycle_start_ms, Some(1_790_930_925_000));
        assert_eq!(p.cycle_end_ms, Some(1_793_609_325_000));
        assert_eq!(p.total_spend, Some(786.0));
        assert_eq!(p.limit, Some(2000.0));
        assert_eq!(p.remaining, Some(1214.0));
        // 双模型池 + 团队池口径（Cursor Models / Other Models）。
        assert_eq!(p.auto_percent_used, Some(0.0));
        assert_eq!(p.api_percent_used, Some(9.825));
        assert_eq!(p.limit_type.as_deref(), Some("team"));
        assert_eq!(p.pooled_used, Some(0.0));
        assert_eq!(
            p.display_message.as_deref(),
            Some("You've used 39% of your included usage")
        );
    }

    #[test]
    fn parse_current_period_usage_splits_bonus_from_included() {
        // 2026-10-10 实测 Pro 账号：套餐 $20 用满后上游另给赠送用量，totalSpend 把两者加在一起。
        let body = r#"{"billingCycleStart":"1789539533000","billingCycleEnd":"1792131533000","planUsage":{"totalSpend":17158,"includedSpend":2000,"bonusSpend":15158,"limit":2000,"remainingBonus":false,"autoPercentUsed":38.12888888888889,"apiPercentUsed":0,"totalPercentUsed":36.31322751322752},"spendLimitUsage":{"limitType":"user"},"displayMessage":"You've hit your usage limit"}"#;
        let p = parse_current_period_usage(body).expect("parse");
        assert_eq!(p.total_spend, Some(17158.0));
        assert_eq!(p.included_spend, Some(2000.0));
        assert_eq!(p.bonus_spend, Some(15158.0));
        assert_eq!(p.limit, Some(2000.0));
        let total = p.total_percent_used.expect("totalPercentUsed");
        assert!((total - 36.313).abs() < 0.01, "total={total}");
        assert_eq!(p.limit_type.as_deref(), Some("user"));
    }

    #[test]
    fn parse_current_period_usage_rejects_garbage() {
        assert!(parse_current_period_usage("").is_none());
        assert!(parse_current_period_usage("not json").is_none());
        assert!(parse_current_period_usage(r#"{"planUsage":{}}"#).is_none());
        assert!(parse_current_period_usage(r#"{"error":"nope"}"#).is_none());
    }

    #[test]
    fn parse_current_period_usage_tolerates_epoch_as_number() {
        let body = r#"{"billingCycleStart":1790930925000,"billingCycleEnd":1793609325000,"planUsage":{"totalSpend":"12.5","limit":"100"}}"#;
        let p = parse_current_period_usage(body).expect("parse");
        assert_eq!(p.cycle_start_ms, Some(1_790_930_925_000));
        assert_eq!(p.total_spend, Some(12.5));
        assert_eq!(p.limit, Some(100.0));
    }

    #[test]
    fn parse_membership_reads_type_only() {
        assert_eq!(
            parse_membership(r#"{"membershipType":"pro","daysRemainingOnTrial":0}"#).as_deref(),
            Some("pro")
        );
        assert!(parse_membership(r#"{"membershipType":""}"#).is_none());
        assert!(parse_membership("nope").is_none());
    }
}
