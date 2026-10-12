//! Grok Bot 额度：Grok Bot 用的是 Cursor 账号体系，令牌来自看板里的授权登录
//! （`quota_accounts`，provider = grok-bot）→ `DashboardService/GetSandUsageStatus`
//! 拉当前用量。DTO 不含任何 token 字段。

use std::sync::Arc;

use serde::Serialize;

use crate::codex_quota::{http_client, now_unix, REAUTH_MESSAGE};
use crate::quota_accounts::{token_for, Provider};
use crate::DbState;

const USAGE_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetSandUsageStatus";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokQuotaDto {
    pub id: i64,
    pub ok: bool,
    pub message: Option<String>,
    /// 授权时取到的账号邮箱（取不到时为 None）。
    pub email: Option<String>,
    pub grok_plan_label: Option<String>,
    pub cursor_plan_name: Option<String>,
    /// 已用百分比（0-100；上游是 f64）。
    pub usage_percent: Option<f64>,
    /// 下次重置时间（epoch 毫秒）。
    pub next_reset_ms: Option<i64>,
    pub period_start_ms: Option<i64>,
    pub has_available_usage: Option<bool>,
    pub is_team_seat: Option<bool>,
    /// 企业共享池：无个人百分比，官方客户端也不画用量条。
    pub uses_pooled_enterprise_allowance: Option<bool>,
    /// 是否有非零包含额度；false / includedLimitZero 时不应展示百分比。
    pub has_non_zero_included_limit: Option<bool>,
    pub included_limit_zero: Option<bool>,
    pub needs_reauth: bool,
    pub at: i64,
}

impl GrokQuotaDto {
    fn err(id: i64, email: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            id,
            ok: false,
            needs_reauth: message == REAUTH_MESSAGE,
            message: Some(message),
            email,
            grok_plan_label: None,
            cursor_plan_name: None,
            usage_percent: None,
            next_reset_ms: None,
            period_start_ms: None,
            has_available_usage: None,
            is_team_seat: None,
            uses_pooled_enterprise_allowance: None,
            has_non_zero_included_limit: None,
            included_limit_zero: None,
            at: now_unix(),
        }
    }
}

/// GetSandUsageStatusResponse 的 connect-json 字段（探针实测）。
#[derive(Debug, Clone, PartialEq)]
pub struct SandUsage {
    pub grok_plan_label: Option<String>,
    pub cursor_plan_name: Option<String>,
    pub usage_percent: Option<f64>,
    pub next_reset_ms: Option<i64>,
    pub period_start_ms: Option<i64>,
    pub has_available_usage: Option<bool>,
    pub is_team_seat: Option<bool>,
    pub uses_pooled_enterprise_allowance: Option<bool>,
    pub has_non_zero_included_limit: Option<bool>,
    pub included_limit_zero: Option<bool>,
}

fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn as_ms(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

pub fn parse_sand_usage_status(body: &str) -> Option<SandUsage> {
    let root: serde_json::Value = serde_json::from_str(body).ok()?;
    let usage_percent = root.get("usagePercent").and_then(as_f64);
    let next_reset_ms = root.get("nextResetTimestampUtc").and_then(as_ms);
    let uses_pooled = root
        .get("usesPooledEnterpriseAllowance")
        .and_then(|v| v.as_bool());
    let has_nonzero = root.get("hasNonZeroIncludedLimit").and_then(|v| v.as_bool());
    let included_zero = root.get("includedLimitZero").and_then(|v| v.as_bool());
    // 用量 / 重置 / 额度门控字段至少要有一个，否则当响应不对。
    if usage_percent.is_none()
        && next_reset_ms.is_none()
        && uses_pooled.is_none()
        && has_nonzero.is_none()
        && included_zero.is_none()
    {
        return None;
    }
    Some(SandUsage {
        grok_plan_label: root
            .get("grokPlanLabel")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        cursor_plan_name: root
            .get("cursorPlanName")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        usage_percent,
        next_reset_ms,
        period_start_ms: root.get("currentPeriodStart").and_then(as_ms),
        has_available_usage: root.get("hasAvailableUsage").and_then(|v| v.as_bool()),
        is_team_seat: root.get("isTeamSeat").and_then(|v| v.as_bool()),
        uses_pooled_enterprise_allowance: uses_pooled,
        has_non_zero_included_limit: has_nonzero,
        included_limit_zero: included_zero,
    })
}

#[tauri::command]
pub async fn grok_bot_quota_fetch(state: tauri::State<'_, DbState>, id: i64) -> Result<GrokQuotaDto, String> {
    let stored = match token_for(Arc::clone(&state.conn), id, Provider::Grok).await {
        Ok(t) => t,
        Err(message) => return Ok(GrokQuotaDto::err(id, None, message)),
    };
    let email = stored.label;

    let resp = http_client()?
        .post(USAGE_URL)
        .header("Authorization", format!("Bearer {}", stored.access_token))
        .header("Content-Type", "application/json")
        .header("User-Agent", "connectrpc/1.0 grok-bot/0.57.1")
        .body("{}")
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            return Ok(GrokQuotaDto::err(
                id,
                email,
                crate::config::friendly_http_error(e, "请求失败"),
            ))
        }
    };
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Ok(GrokQuotaDto::err(id, email, REAUTH_MESSAGE));
    }
    if !resp.status().is_success() {
        return Ok(GrokQuotaDto::err(id, email, format!("上游返回 {status}")));
    }
    let body = resp.text().await.unwrap_or_default();
    let Some(usage) = parse_sand_usage_status(&body) else {
        return Ok(GrokQuotaDto::err(id, email, "用量响应解析失败"));
    };

    Ok(GrokQuotaDto {
        id,
        ok: true,
        message: None,
        email,
        grok_plan_label: usage.grok_plan_label,
        cursor_plan_name: usage.cursor_plan_name,
        usage_percent: usage.usage_percent,
        next_reset_ms: usage.next_reset_ms,
        period_start_ms: usage.period_start_ms,
        has_available_usage: usage.has_available_usage,
        is_team_seat: usage.is_team_seat,
        uses_pooled_enterprise_allowance: usage.uses_pooled_enterprise_allowance,
        has_non_zero_included_limit: usage.has_non_zero_included_limit,
        included_limit_zero: usage.included_limit_zero,
        needs_reauth: false,
        at: now_unix(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sand_usage_status_reads_live_shape() {
        // 字段名来自 connect-json（lowerCamelCase），形状按 asar 里的 proto 描述。
        let body = r#"{"currentPeriodStart":"1790930925000","nextResetTimestampUtc":"1791800000000","usagePercent":100,"includedLimitZero":false,"availableBankedResetCount":0,"usesPooledEnterpriseAllowance":false,"hasAvailableUsage":false,"hasNonZeroIncludedLimit":true,"grokPlanLabel":"Pro","isTeamSeat":false,"cursorPlanName":"pro"}"#;
        let u = parse_sand_usage_status(body).expect("parse");
        assert_eq!(u.grok_plan_label.as_deref(), Some("Pro"));
        assert_eq!(u.cursor_plan_name.as_deref(), Some("pro"));
        assert_eq!(u.usage_percent, Some(100.0));
        assert_eq!(u.next_reset_ms, Some(1_791_800_000_000));
        assert_eq!(u.period_start_ms, Some(1_790_930_925_000));
        assert_eq!(u.has_available_usage, Some(false));
        assert_eq!(u.is_team_seat, Some(false));
        assert_eq!(u.uses_pooled_enterprise_allowance, Some(false));
        assert_eq!(u.has_non_zero_included_limit, Some(true));
        assert_eq!(u.included_limit_zero, Some(false));
    }

    #[test]
    fn parse_sand_usage_status_tolerates_numeric_epoch_and_missing_plans() {
        let body = r#"{"nextResetTimestampUtc":1791800000000,"usagePercent":42.5}"#;
        let u = parse_sand_usage_status(body).expect("parse");
        assert_eq!(u.usage_percent, Some(42.5));
        assert_eq!(u.next_reset_ms, Some(1_791_800_000_000));
        assert_eq!(u.grok_plan_label, None);
    }

    #[test]
    fn parse_sand_usage_status_reads_pooled_enterprise_without_percent() {
        let body = r#"{"usesPooledEnterpriseAllowance":true,"hasNonZeroIncludedLimit":false,"includedLimitZero":true,"hasAvailableUsage":true,"isTeamSeat":true,"cursorPlanName":"enterprise","grokPlanLabel":"Grok Bot Plan"}"#;
        let u = parse_sand_usage_status(body).expect("parse");
        assert_eq!(u.usage_percent, None);
        assert_eq!(u.uses_pooled_enterprise_allowance, Some(true));
        assert_eq!(u.has_non_zero_included_limit, Some(false));
        assert_eq!(u.included_limit_zero, Some(true));
    }

    #[test]
    fn parse_sand_usage_status_rejects_garbage() {
        assert!(parse_sand_usage_status("").is_none());
        assert!(parse_sand_usage_status("not json").is_none());
        assert!(parse_sand_usage_status(r#"{"error":"denied"}"#).is_none());
    }
}
