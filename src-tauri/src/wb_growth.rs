//! Growth-plan status per account: streak ladder and task list.
//!
//! Strictly read-only — the growth rules forbid scripted redemption, so this
//! module never POSTs to /redeem, /claim or /accept; claiming stays a manual
//! action in the official client. Every endpoint is optional and fails
//! independently: a missing or broken one degrades to a reason string instead
//! of an error, because the panel is a nice-to-have readout.

use rusqlite::Connection;
use serde::Serialize;

use crate::codebuddy::adapter::{self, UpstreamProfile};
use crate::codebuddy::credential::{self, CredentialKind, ParsedCredential};
use crate::wb_commands;
use crate::DbState;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrowthTier {
    pub tier: i64,
    pub days_required: i64,
    pub claimed: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreakInfo {
    pub days: Option<i64>,
    pub tiers: Vec<GrowthTier>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrowthTask {
    pub code: Option<String>,
    pub title: Option<String>,
    /// None = the status key was not recognised; the UI shows a dash.
    pub done: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrowthInfoDto {
    pub account_id: i64,
    pub label: String,
    /// True when at least one endpoint answered 2xx.
    pub ok: bool,
    /// Per-endpoint failure reasons, for the grey-state hints.
    pub reasons: Vec<String>,
    pub streak: Option<StreakInfo>,
    pub tasks: Vec<GrowthTask>,
    pub at: i64,
}

/// Look up one account's id/label/credential the same way check-in does.
fn load_account(
    conn: &Connection,
    account_id: i64,
) -> Result<(i64, String, ParsedCredential), String> {
    let row: Result<(i64, String, String, String), _> = conn.query_row(
        "SELECT id, label, credential_type, credential FROM codebuddy_accounts WHERE id = ?1",
        [account_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    );
    let (id, label, kind, raw) = row.map_err(|_| format!("账号 {account_id} 不存在"))?;
    Ok((id, label, ParsedCredential::parse(CredentialKind::parse(&kind), &raw)))
}

// ---------------------------------------------------------------------------
// Parsers (pure, tolerant — the growth payload shapes were observed once)
// ---------------------------------------------------------------------------

/// The payload nests under `data` or sits at the top level; `streak.days` and
/// a sibling `tiers` array were the observed locations.
pub fn parse_streak(body: &str) -> Option<StreakInfo> {
    let json = serde_json::from_str::<serde_json::Value>(body.trim()).ok()?;
    let root = json.get("data").filter(|v| v.is_object()).unwrap_or(&json);
    let days = ["days", "streak_days", "streakDays"]
        .iter()
        .find_map(|k| root.get(*k).or_else(|| root.get("streak").and_then(|s| s.get(*k))))
        .and_then(|v| v.as_i64());
    let tiers = ["tiers", "tier_list", "tierList"]
        .iter()
        .find_map(|k| root.get(*k).or_else(|| root.get("streak").and_then(|s| s.get(*k))))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let tier = ["tier", "level", "id"]
                        .iter()
                        .find_map(|k| item.get(*k).and_then(|v| v.as_i64()))?;
                    let days_required = ["days_required", "daysRequired", "required_days", "days"]
                        .iter()
                        .find_map(|k| item.get(*k).and_then(|v| v.as_i64()))?;
                    let claimed = ["claimed", "redeemed"]
                        .iter()
                        .find_map(|k| item.get(*k).and_then(|v| v.as_bool()));
                    Some(GrowthTier { tier, days_required, claimed })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if days.is_none() && tiers.is_empty() {
        return None;
    }
    Some(StreakInfo { days, tiers })
}

pub fn parse_tasks(body: &str) -> Vec<GrowthTask> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body.trim()) else {
        return Vec::new();
    };
    let root = json.get("data").filter(|v| v.is_object()).unwrap_or(&json);
    let arr = ["tasks", "task_list", "taskList", "list"]
        .iter()
        .find_map(|k| root.get(*k))
        .or(if json.is_array() { Some(&json) } else { None })
        .and_then(|v| v.as_array());
    let Some(arr) = arr else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let code = ["code", "task_code", "taskCode"]
                .iter()
                .find_map(|k| item.get(*k).and_then(|v| v.as_str()))
                .map(str::to_string);
            let title = ["title", "name"]
                .iter()
                .find_map(|k| item.get(*k).and_then(|v| v.as_str()))
                .map(str::to_string);
            let done = ["claimed", "completed", "done"]
                .iter()
                .find_map(|k| item.get(*k).and_then(|v| v.as_bool()))
                .or_else(|| {
                    ["status", "state"]
                        .iter()
                        .find_map(|k| item.get(*k).and_then(|v| v.as_str()))
                        .map(|s| matches!(s, "claimed" | "completed" | "done" | "finished"))
                });
            (code.is_some() || title.is_some()).then(|| GrowthTask { code, title, done })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

async fn fetch_endpoint(
    profile: Option<&UpstreamProfile>,
    endpoint: Option<&adapter::Endpoint>,
    field: &str,
    parsed: &ParsedCredential,
) -> Result<String, String> {
    profile.ok_or_else(|| "未配置协议".to_string())?;
    let ep = endpoint.ok_or_else(|| format!("协议缺少 {field} 端点"))?;
    let cx = adapter::RenderContext {
        credential: &parsed.raw,
        token: &parsed.token,
        cookie: &parsed.cookie,
        model: "",
    };
    let request = adapter::build_request(ep, &cx).ok_or_else(|| format!("{field} 端点的 method 为空"))?;
    let resp = wb_commands::execute_outbound(&request)
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let note = body
            .chars()
            .take(120)
            .collect::<String>();
        return Err(if note.is_empty() {
            format!("上游 {status}")
        } else {
            format!("上游 {status}：{note}")
        });
    }
    Ok(body)
}

#[tauri::command]
pub async fn wb_growth_info(
    state: tauri::State<'_, DbState>,
    account_id: i64,
) -> Result<GrowthInfoDto, String> {
    let conn = std::sync::Arc::clone(&state.conn);
    let (profile, account) = {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        let profile = wb_commands::get_settings(&guard)
            .ok()
            .and_then(|s| s.adapter_json)
            .filter(|json| !json.trim().is_empty())
            .and_then(|json| UpstreamProfile::from_json(&json).ok());
        let account = load_account(&guard, account_id)?;
        (profile, account)
    };

    let (_id, label, parsed) = account;
    let mut dto = GrowthInfoDto {
        account_id,
        label,
        ok: false,
        reasons: Vec::new(),
        streak: None,
        tasks: Vec::new(),
        at: credential::now_unix() * 1000,
    };

    let streak_body = fetch_endpoint(
        profile.as_ref(),
        profile.as_ref().and_then(|p| p.growth_streak.as_ref()),
        "growthStreak",
        &parsed,
    )
    .await;
    match streak_body {
        Ok(body) => dto.streak = parse_streak(&body),
        Err(reason) => dto.reasons.push(reason),
    }

    let tasks_body = fetch_endpoint(
        profile.as_ref(),
        profile.as_ref().and_then(|p| p.growth_tasks.as_ref()),
        "growthTasks",
        &parsed,
    )
    .await;
    match tasks_body {
        Ok(body) => dto.tasks = parse_tasks(&body),
        Err(reason) => dto.reasons.push(reason),
    }

    dto.ok = dto.streak.is_some() || !dto.tasks.is_empty();
    Ok(dto)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streak_parsing_accepts_the_observed_shape_and_variants() {
        let measured = r#"{"code":0,"data":{"streak":{"days":10},"tiers":[{"tier":1,"days_required":7,"claimed":false},{"tier":2,"days_required":14,"claimed":false}]}}"#;
        let info = parse_streak(measured).unwrap();
        assert_eq!(info.days, Some(10));
        assert_eq!(info.tiers.len(), 2);
        assert_eq!(info.tiers[1].days_required, 14);
        assert_eq!(info.tiers[0].claimed, Some(false));

        let flat = r#"{"days":3,"tiers":[]}"#;
        let info = parse_streak(flat).unwrap();
        assert_eq!(info.days, Some(3));

        assert!(parse_streak("not json").is_none());
        assert!(parse_streak(r#"{"code":0}"#).is_none());
    }

    #[test]
    fn task_parsing_is_loose_and_skips_unrecognisable_items() {
        let body = r#"{"code":0,"data":{"tasks":[
            {"code":"playbook_prompt","title":"使用 Playbook","status":"todo"},
            {"code":"create_canvas","name":"创建画布","completed":true},
            {"foo":1}
        ]}}"#;
        let tasks = parse_tasks(body);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title.as_deref(), Some("使用 Playbook"));
        assert_eq!(tasks[0].done, Some(false), "status=todo maps to not done");
        assert_eq!(tasks[1].done, Some(true));
        assert!(parse_tasks("not json").is_empty());
        assert!(parse_tasks(r#"{"code":0}"#).is_empty());
    }
}
