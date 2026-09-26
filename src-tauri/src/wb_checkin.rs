//! CodeBuddy daily check-in: the manual command, the schedule loop, and the
//! history table.
//!
//! Check-in is the one thing in this domain that earns the account something,
//! so it is deliberately conservative: strictly serial, jittered between calls,
//! never retried inside one round, and an account that fails `FAIL_LIMIT`
//! rounds in a row is taken out of rotation rather than kept being poked — that
//! pattern is what gets an IP flagged.
//!
//! The decision logic (`due_today`, `apply_result`) is pure and unit-tested;
//! only the HTTP call and the loop live on the async side.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, Timelike};
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::Emitter;

use crate::codebuddy::adapter::{self, UpstreamProfile};
use crate::codebuddy::credential::{self, CredentialKind, ParsedCredential};
use crate::wb_commands::{self, WbSettingsPatch};
use crate::DbState;

/// History kept per plan; a row is tiny, this is ~one year of daily rounds for
/// a small pool.
const MAX_LOG_ROWS: i64 = 500;
/// Consecutive failures before the account is disabled.
const FAIL_LIMIT: i64 = 5;
/// Pause between accounts, so a pool of ten does not look like one client
/// hammering the endpoint.
const BETWEEN_ACCOUNTS: Duration = Duration::from_secs(2);
/// Schedule granularity the UI offers is "daily at HH:MM", so a minute tick is
/// plenty.
const TICK_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResult {
    pub account_id: i64,
    pub label: String,
    pub ok: bool,
    pub message: String,
    pub at: i64,
    /// Set when this failure crossed `FAIL_LIMIT` and disabled the account.
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinLogEntry {
    pub id: i64,
    pub account_id: i64,
    pub label: Option<String>,
    pub ok: bool,
    pub message: Option<String>,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// Schedule decision (pure)
// ---------------------------------------------------------------------------

/// Minutes since midnight for "HH:MM"; None when malformed, which keeps a typo
/// in the settings row from panicking the loop.
pub fn parse_hhmm(raw: &str) -> Option<u32> {
    let (h, m) = raw.trim().split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if h < 24 && m < 60 {
        Some(h * 60 + m)
    } else {
        None
    }
}

/// Local date key used for "already ran today" (`YYYY-MM-DD`).
pub fn date_key(now: DateTime<Local>) -> String {
    now.format("%Y-%m-%d").to_string()
}

/// Today's schedule is due when the clock has passed `time` and the stored
/// last-run date is not today. Comparing ">= the scheduled minute" rather than
/// "== it" is what makes an app that was closed at 09:30 still check in at 10:00.
pub fn due_today(time: &str, last_run_date: Option<&str>, now: DateTime<Local>) -> Option<String> {
    let scheduled = parse_hhmm(time)?;
    let today = date_key(now);
    if last_run_date == Some(today.as_str()) {
        return None;
    }
    let minutes_now = now.hour() * 60 + now.minute();
    (minutes_now >= scheduled).then_some(today)
}

// ---------------------------------------------------------------------------
// Result bookkeeping (pure over &Connection)
// ---------------------------------------------------------------------------

/// Record one check-in: history row + account counters + the FIFO trim.
/// Returns whether this failure took the account out of rotation.
///
/// `counts=false` 用于配置缺失（没配协议 / 缺 checkin 端点）：这不是凭证的错，
/// 只记日志、不累计连续失败——否则没配端点时点两次签到就会把有效账号自动停用。
pub fn apply_result(
    conn: &Connection,
    account_id: i64,
    ok: bool,
    message: &str,
    now_ms: i64,
    counts: bool,
) -> Result<bool, String> {
    conn.execute(
        "INSERT INTO wb_checkin_logs (account_id, ok, message, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![account_id, i64::from(ok), message, now_ms],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM wb_checkin_logs
          WHERE id NOT IN (SELECT id FROM wb_checkin_logs ORDER BY created_at DESC, id DESC LIMIT ?1)",
        params![MAX_LOG_ROWS],
    )
    .map_err(|e| e.to_string())?;

    if ok {
        conn.execute(
            "UPDATE codebuddy_accounts
                SET last_checkin_at = ?2, last_checkin_status = 'ok',
                    checkin_fail_count = 0, updated_at = ?2
              WHERE id = ?1",
            params![account_id, now_ms],
        )
        .map_err(|e| e.to_string())?;
        return Ok(false);
    }

    conn.execute(
        "UPDATE codebuddy_accounts
            SET last_checkin_at = ?2, last_checkin_status = 'fail',
                checkin_fail_count = checkin_fail_count + ?3, updated_at = ?2
          WHERE id = ?1",
        params![account_id, now_ms, i64::from(counts)],
    )
    .map_err(|e| e.to_string())?;
    let fails: i64 = conn.query_row(
        "SELECT checkin_fail_count FROM codebuddy_accounts WHERE id = ?1",
        params![account_id],
        |r| r.get(0),
    ).map_err(|e| e.to_string())?;
    if fails < FAIL_LIMIT {
        return Ok(false);
    }
    // Report the disable exactly once — at the round that actually took the
    // account out of rotation. Otherwise every later failure would claim a
    // fresh auto-disable in the UI.
    let still_enabled: i64 = conn
        .query_row("SELECT enabled FROM codebuddy_accounts WHERE id = ?1", params![account_id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if still_enabled == 0 {
        return Ok(false);
    }
    conn.execute(
        "UPDATE codebuddy_accounts SET enabled = 0 WHERE id = ?1",
        params![account_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn list_checkin_log(conn: &Connection, limit: i64) -> Result<Vec<CheckinLogEntry>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT h.id, h.account_id, a.label, h.ok, h.message, h.created_at
               FROM wb_checkin_logs h
          LEFT JOIN codebuddy_accounts a ON a.id = h.account_id
              ORDER BY h.created_at DESC, h.id DESC
                 LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![limit.clamp(1, MAX_LOG_ROWS)], |row| {
            Ok(CheckinLogEntry {
                id: row.get(0)?,
                account_id: row.get(1)?,
                label: row.get(2)?,
                ok: row.get::<_, i64>(3)? != 0,
                message: row.get(4)?,
                created_at: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>();
    rows.map_err(|e| e.to_string())
}

pub fn clear_checkin_log(conn: &Connection) -> Result<usize, String> {
    conn.execute("DELETE FROM wb_checkin_logs", []).map_err(|e| e.to_string())
}

/// Accounts to check in this round: one named account when the user pressed the
/// row button, otherwise the whole enabled, unbanned, unexpired pool.
pub fn checkin_targets(
    conn: &Connection,
    account_id: Option<i64>,
) -> Result<Vec<(i64, String, ParsedCredential)>, String> {
    let sql = match account_id {
        Some(_) => "SELECT id, label, credential_type, credential FROM codebuddy_accounts
                      WHERE id = ?1",
        None => "SELECT id, label, credential_type, credential FROM codebuddy_accounts
                  WHERE enabled = 1 AND status <> 'banned' ORDER BY id",
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let collect = |row: &rusqlite::Row| -> rusqlite::Result<(i64, String, String, String)> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    };
    let mapped = match account_id {
        Some(id) => stmt.query_map(params![id], collect),
        None => stmt.query_map([], collect),
    };
    let out = mapped
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .map(|(id, label, kind, raw)| {
            (id, label, ParsedCredential::parse(CredentialKind::parse(&kind), &raw))
        })
        // A credential whose JWT already expired cannot check in; skip it so the
        // failure counter is not burned on a known-dead account.
        .filter(|(_, _, parsed)| !parsed.is_expired(credential::now_unix()))
        .collect::<Vec<_>>();
    Ok(out)
}

fn checkin_request(
    profile: Option<&UpstreamProfile>,
    parsed: &ParsedCredential,
) -> Result<Option<adapter::OutboundRequest>, String> {
    let profile = profile.ok_or_else(|| "未配置协议，无法签到".to_string())?;
    let Some(ep) = profile.checkin.clone() else {
        return Err("协议缺少 checkin 端点".to_string());
    };
    let cx = adapter::RenderContext {
        credential: &parsed.raw,
        token: &parsed.token,
        cookie: &parsed.cookie,
        model: "",
    };
    Ok(adapter::build_request(&ep, &cx))
}

/// Check in one account over HTTP. Never returns an Err: a failure is data the
/// caller records and moves on from.
async fn checkin_one(
    conn: Arc<Mutex<Connection>>,
    account_id: i64,
    label: String,
    parsed: ParsedCredential,
) -> CheckinResult {
    let profile = conn.lock().ok().and_then(|g| wb_commands::get_settings(&g).ok())
        .and_then(|s| s.adapter_json)
        .filter(|json| !json.trim().is_empty())
        .and_then(|json| UpstreamProfile::from_json(&json).ok());

    let (config_error, request) = match checkin_request(profile.as_ref(), &parsed) {
        Err(reason) => (Some(reason), None),
        Ok(None) => (Some("checkin 端点的 method 为空".to_string()), None),
        Ok(Some(request)) => (None, Some(request)),
    };
    // 配置缺失是站点问题不是账号问题：记日志但不累计连续失败。
    let counts = config_error.is_none();

    let outcome = match (config_error, request) {
        (Some(reason), _) => Err(reason),
        (None, Some(request)) => match wb_commands::execute_outbound(&request).await {
            Err(e) => Err(format!("请求失败: {e}")),
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    Ok(summarize_success(&body))
                } else if is_already_checked_in(status.as_u16(), &body) {
                    // 实测：重复签到回 400 {"code":10001,"msg":"今天已签到，请明天再来"}。
                    // 已签过=今日任务已完成，记成功而不是失败——否则一天点两次签到
                    // 就累计两次连续失败，5 次把有效账号自动停用。
                    Ok("今日已签到".to_string())
                } else {
                    let note = summarize_failure(&body);
                    if note.is_empty() {
                        Err(format!("上游 {status}"))
                    } else {
                        Err(format!("上游 {status}：{note}"))
                    }
                }
            }
        },
        (None, None) => unreachable!("checkin_request 只返回其一"),
    };

    let (ok, message) = match outcome {
        Ok(detail) => (true, detail),
        Err(reason) => (false, reason),
    };
    let at = credential::now_unix() * 1000;
    let disabled = conn
        .lock()
        .ok()
        .and_then(|guard| apply_result(&guard, account_id, ok, &message, at, counts).ok())
        .unwrap_or(false);

    CheckinResult {
        account_id,
        label,
        ok,
        message,
        at,
        disabled,
    }
}

/// 实测 daily-checkin 对重复签到的回包：HTTP 400 + `{"code":10001,"msg":"今天已签到，请明天再来"}`。
/// 用「400 + msg 含已签到」判，不依赖 code 数值（那是上游内部约定，可能变）。
fn is_already_checked_in(status: u16, body: &str) -> bool {
    status == 400 && body.contains("已签到")
}

/// 从失败回包里抠一句人能读的原因（`{"code":…,"msg":"…"}` 形状），抠不到就空。
fn summarize_failure(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body.trim())
        .ok()
        .and_then(|json| {
            ["msg", "message"]
                .iter()
                .find_map(|k| json.get(*k).and_then(|v| v.as_str()))
                .map(|s| s.chars().take(120).collect::<String>())
        })
        .unwrap_or_default()
}

/// Keep a short, human-quotable line out of a success body.
fn summarize_success(body: &str) -> String {
    const PATHS: [(&str, Option<&str>); 4] = [
        ("msg", None),
        ("message", None),
        ("data", Some("msg")),
        ("data", Some("message")),
    ];
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "已签到".to_string();
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
        // 实测 daily-checkin 回 {"code":0,"msg":"OK","data":{"credit":100,"streak_days":10}}
        // ——"OK" 没有信息量，积分与连登天数才是人要看的。
        if let Some(credit) = json
            .get("data")
            .and_then(|d| d.get("credit"))
            .and_then(|v| v.as_i64())
        {
            return match json
                .get("data")
                .and_then(|d| d.get("streak_days"))
                .and_then(|v| v.as_i64())
            {
                Some(days) => format!("领取 {credit} 积分，连登 {days} 天"),
                None => format!("领取 {credit} 积分"),
            };
        }
        for (first, second) in PATHS {
            let found = json
                .get(first)
                .and_then(|v| match second {
                    Some(key) => v.get(key),
                    None => Some(v),
                })
                .and_then(|v| v.as_str());
            if let Some(text) = found {
                return text.chars().take(120).collect();
            }
        }
    }
    trimmed.chars().take(120).collect()
}

// ---------------------------------------------------------------------------
// Check-in status query (read-only)
// ---------------------------------------------------------------------------

/// Fields the status endpoint reports. All optional: the caller decides how a
/// missing value is displayed, and a parsing miss never fails the whole call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedCheckinStatus {
    pub today_checked_in: Option<bool>,
    pub streak_days: Option<i64>,
    /// Rendered as a compact string ("4/7", "57%", raw text) because the
    /// upstream shape was only seen once and may vary.
    pub week_progress: Option<String>,
    pub total_credits: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinStatusDto {
    pub account_id: i64,
    pub label: String,
    /// False when the profile has no checkinStatus endpoint at all.
    pub configured: bool,
    pub ok: bool,
    pub message: Option<String>,
    pub today_checked_in: Option<bool>,
    pub streak_days: Option<i64>,
    pub week_progress: Option<String>,
    pub total_credits: Option<i64>,
    pub at: i64,
}

fn status_request(
    profile: Option<&UpstreamProfile>,
    parsed: &ParsedCredential,
) -> Result<Option<adapter::OutboundRequest>, String> {
    let profile = profile.ok_or_else(|| "未配置协议，无法查询签到状态".to_string())?;
    let Some(ep) = profile.checkin_status.clone() else {
        return Err("协议缺少 checkinStatus 端点".to_string());
    };
    let cx = adapter::RenderContext {
        credential: &parsed.raw,
        token: &parsed.token,
        cookie: &parsed.cookie,
        model: "",
    };
    Ok(adapter::build_request(&ep, &cx))
}

fn bool_field(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => match s.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// `week_progress` was only observed once, so render whatever looks progress-ish
/// into a short string instead of guessing a strict shape.
fn week_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(n) => {
            let f = n.as_f64()?;
            if (0.0..=1.0).contains(&f) && f.fract() != 0.0 {
                Some(format!("{:.0}%", f * 100.0))
            } else {
                Some(n.to_string())
            }
        }
        serde_json::Value::String(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        serde_json::Value::Object(o) => {
            let checked = ["checked", "days", "count"]
                .iter()
                .find_map(|k| o.get(*k).and_then(|v| v.as_i64()));
            let total = ["total", "days_total"]
                .iter()
                .find_map(|k| o.get(*k).and_then(|v| v.as_i64()));
            match (checked, total) {
                (Some(a), Some(b)) => Some(format!("{a}/{b}")),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parse a checkin-status body. Accepts the payload at the top level or under
/// `data` (the observed shape), snake_case or camelCase keys, string booleans.
pub fn parse_checkin_status(body: &str) -> ParsedCheckinStatus {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body.trim()) else {
        return ParsedCheckinStatus::default();
    };
    let empty = serde_json::Value::Null;
    let data = json.get("data").filter(|v| v.is_object()).unwrap_or(&empty);
    let get = |names: &[&str]| names.iter().find_map(|k| data.get(*k).or_else(|| json.get(*k)));
    ParsedCheckinStatus {
        today_checked_in: get(&["today_checked_in", "todayCheckedIn"]).and_then(bool_field),
        streak_days: get(&["streak_days", "streakDays"]).and_then(|v| v.as_i64()),
        week_progress: get(&["week_progress", "weekProgress"]).and_then(week_to_string),
        total_credits: get(&["total_credits", "totalCredits", "credit"]).and_then(|v| v.as_i64()),
    }
}

/// Query one account's check-in status. Read-only: nothing is written to the
/// account row, and a failure here never touches the fail counter.
async fn status_one(
    conn: Arc<Mutex<Connection>>,
    account_id: i64,
    label: String,
    parsed: ParsedCredential,
) -> CheckinStatusDto {
    let profile = conn
        .lock()
        .ok()
        .and_then(|g| wb_commands::get_settings(&g).ok())
        .and_then(|s| s.adapter_json)
        .filter(|json| !json.trim().is_empty())
        .and_then(|json| UpstreamProfile::from_json(&json).ok());

    let at = credential::now_unix() * 1000;
    let (configured, outcome) = match status_request(profile.as_ref(), &parsed) {
        Err(reason) => (false, Err(reason)),
        Ok(None) => (false, Err("checkinStatus 端点的 method 为空".to_string())),
        Ok(Some(request)) => match wb_commands::execute_outbound(&request).await {
            Err(e) => (true, Err(format!("请求失败: {e}"))),
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    (true, Ok(parse_checkin_status(&body)))
                } else {
                    let note = summarize_failure(&body);
                    let reason = if note.is_empty() {
                        format!("上游 {status}")
                    } else {
                        format!("上游 {status}：{note}")
                    };
                    (true, Err(reason))
                }
            }
        },
    };

    let mut dto = CheckinStatusDto {
        account_id,
        label,
        configured,
        ok: false,
        message: None,
        today_checked_in: None,
        streak_days: None,
        week_progress: None,
        total_credits: None,
        at,
    };
    match outcome {
        Ok(status) => {
            dto.ok = true;
            dto.today_checked_in = status.today_checked_in;
            dto.streak_days = status.streak_days;
            dto.week_progress = status.week_progress;
            dto.total_credits = status.total_credits;
        }
        Err(reason) => dto.message = Some(reason),
    }
    dto
}

/// Manual status query for one account, or the whole enabled pool. Serial with
/// the same 2s spacing as a check-in round — same endpoint family, same
/// risk profile. Emits `wb-checkin-status-done` per account.
#[tauri::command]
pub async fn wb_checkin_status(
    app: tauri::AppHandle,
    state: tauri::State<'_, DbState>,
    account_id: Option<i64>,
) -> Result<Vec<CheckinStatusDto>, String> {
    let conn = Arc::clone(&state.conn);
    let targets = {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        checkin_targets(&guard, account_id).map_err(|e| e.to_string())?
    };
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let mut results = Vec::with_capacity(targets.len());
    for (index, (id, label, parsed)) in targets.into_iter().enumerate() {
        if index > 0 {
            tokio::time::sleep(BETWEEN_ACCOUNTS).await;
        }
        let dto = status_one(Arc::clone(&conn), id, label, parsed).await;
        let _ = app.emit("wb-checkin-status-done", dto.clone());
        results.push(dto);
    }
    Ok(results)
}

/// Run a round over the requested scope and emit one event per account plus a
/// final one, so the panel can show progress on a big pool.
async fn run_round(
    app: &tauri::AppHandle,
    conn: Arc<Mutex<Connection>>,
    account_id: Option<i64>,
) -> Result<Vec<CheckinResult>, String> {
    let targets = {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        checkin_targets(&guard, account_id).map_err(|e| e.to_string())?
    };
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let mut results = Vec::with_capacity(targets.len());
    for (index, (id, label, parsed)) in targets.into_iter().enumerate() {
        if index > 0 {
            tokio::time::sleep(BETWEEN_ACCOUNTS).await;
        }
        let result = checkin_one(Arc::clone(&conn), id, label, parsed).await;
        let _ = app.emit("wb-checkin-done", result.clone());
        results.push(result);
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Schedule loop
// ---------------------------------------------------------------------------

/// Daily check-in loop, started from setup. Same shape as the tray status poll:
/// sleep, decide, act — no cron engine, because the only schedule the UI offers
/// is "every day at HH:MM".
pub fn spawn_loop(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(TICK_SECS)).await;
            if let Err(e) = tick(&app).await {
                eprintln!("[workbuddy] check-in tick failed: {e}");
            }
        }
    });
}

async fn tick(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let conn = Arc::clone(&app.state::<DbState>().conn);
    let due = {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        let settings = wb_commands::get_settings(&guard)?;
        if !settings.checkin_enabled {
            return Ok(());
        }
        match due_today(&settings.checkin_time, settings.last_auto_checkin_date.as_deref(), Local::now()) {
            Some(today) => today,
            None => return Ok(()),
        }
    };
    // Claim the day before running: a round that takes minutes must not be
    // started twice by two ticks.
    {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        wb_commands::update_settings(
            &guard,
            &WbSettingsPatch {
                last_auto_checkin_date: Some(due),
                ..Default::default()
            },
        )?;
    }
    let results = run_round(app, conn, None).await?;
    if !results.is_empty() {
        let _ = app.emit("wb-checkin-round", &results);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Manual "check in now" for one account, or the whole enabled pool.
#[tauri::command]
pub async fn wb_checkin_now(
    app: tauri::AppHandle,
    state: tauri::State<'_, DbState>,
    account_id: Option<i64>,
) -> Result<Vec<CheckinResult>, String> {
    run_round(&app, Arc::clone(&state.conn), account_id).await
}

#[tauri::command]
pub async fn wb_checkin_log_list(
    state: tauri::State<'_, DbState>,
    limit: Option<i64>,
) -> Result<Vec<CheckinLogEntry>, String> {
    let conn = Arc::clone(&state.conn);
    let limit = limit.unwrap_or(100);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        list_checkin_log(&guard, limit)
    })
    .await
    .map_err(|e| format!("读取失败: {}", e))?
}

#[tauri::command]
pub async fn wb_checkin_log_clear(state: tauri::State<'_, DbState>) -> Result<usize, String> {
    let conn = Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        clear_checkin_log(&guard)
    })
    .await
    .map_err(|e| format!("清空失败: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, min, 0).unwrap()
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::codebuddy::ensure_schema(&c).unwrap();
        c
    }

    fn account(c: &Connection, label: &str, raw: &str) -> i64 {
        c.execute(
            "INSERT INTO codebuddy_accounts (label, credential_type, credential, status, enabled, created_at)
             VALUES (?1, 'token', ?2, 'active', 1, 1)",
            params![label, raw],
        )
        .unwrap();
        c.last_insert_rowid()
    }

    // ---- schedule ----

    #[test]
    fn hhmm_parsing_is_strict() {
        assert_eq!(parse_hhmm("09:30"), Some(9 * 60 + 30));
        assert_eq!(parse_hhmm(" 00:00 "), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(23 * 60 + 59));
        for bad in ["9:30x", "24:00", "09:60", "abc", "", "09"] {
            assert_eq!(parse_hhmm(bad), None, "{bad} must not parse");
        }
    }

    #[test]
    fn schedule_fires_once_per_day_after_the_time() {
        let nine_thirty = at(2026, 9, 26, 9, 30);
        // Before the slot: not due.
        assert_eq!(due_today("09:30", None, nine_thirty - chrono::Duration::minutes(1)), None);
        // Exactly at it, and anything after, is due.
        assert_eq!(due_today("09:30", None, nine_thirty).as_deref(), Some("2026-09-26"));
        assert_eq!(due_today("09:30", None, nine_thirty + chrono::Duration::hours(5)).as_deref(), Some("2026-09-26"));
        // Already ran today -> nothing more, however late it gets.
        assert_eq!(due_today("09:30", Some("2026-09-26"), nine_thirty + chrono::Duration::hours(9)), None);
    }

    #[test]
    fn a_late_startup_catches_the_missed_round() {
        // The app was closed at 09:30 and opened at 21:00: the round is due.
        assert_eq!(due_today("09:30", Some("2026-09-25"), at(2026, 9, 26, 21, 0)).as_deref(), Some("2026-09-26"));
    }

    #[test]
    fn next_day_resumes_immediately() {
        assert_eq!(due_today("00:05", Some("2026-09-26"), at(2026, 9, 27, 0, 10)).as_deref(), Some("2026-09-27"));
    }

    #[test]
    fn a_broken_time_setting_disables_the_schedule_rather_than_panicking() {
        assert_eq!(due_today("25:99", None, at(2026, 9, 26, 12, 0)), None);
    }

    // ---- result bookkeeping ----

    #[test]
    fn a_repeat_checkin_is_ok_not_a_failure() {
        // 实测：重复签到回 400 {"code":10001,"msg":"今天已签到，请明天再来"}。
        assert!(is_already_checked_in(
            400,
            r#"{"code":10001,"msg":"今天已签到，请明天再来","requestId":"x"}"#
        ));
        assert!(!is_already_checked_in(200, r#"{"msg":"今天已签到"}"#));
        assert!(!is_already_checked_in(400, r#"{"msg":"参数不合法"}"#));
    }

    #[test]
    fn failure_bodies_surface_their_reason() {
        assert_eq!(
            summarize_failure(r#"{"code":10001,"msg":"今天已签到，请明天再来"}"#),
            "今天已签到，请明天再来"
        );
        assert_eq!(summarize_failure("not json"), "");
        assert_eq!(summarize_failure(""), "");
    }

    #[test]
    fn success_resets_the_fail_counter() {
        let c = conn();
        let id = account(&c, "A", "tok");
        apply_result(&c, id, false, "上游 500", 1_000, true).unwrap();
        apply_result(&c, id, false, "上游 500", 2_000, true).unwrap();
        apply_result(&c, id, true, "已签到", 3_000, true).unwrap();
        let (fails, status, last): (i64, String, i64) = c
            .query_row(
                "SELECT checkin_fail_count, last_checkin_status, last_checkin_at FROM codebuddy_accounts WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((fails, status.as_str(), last), (0, "ok", 3_000));
    }

    #[test]
    fn the_limit_disables_the_account_once() {
        let c = conn();
        let id = account(&c, "A", "tok");
        for i in 1..=FAIL_LIMIT {
            let disabled = apply_result(&c, id, false, "上游 401", i, true).unwrap();
            assert_eq!(disabled, i == FAIL_LIMIT, "only the crossing round reports it");
        }
        let enabled: i64 = c
            .query_row("SELECT enabled FROM codebuddy_accounts WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(enabled, 0);
        // A later failure must not re-report or resurrect it.
        assert!(!apply_result(&c, id, false, "again", 99, true).unwrap());
        let fails: i64 = c
            .query_row("SELECT checkin_fail_count FROM codebuddy_accounts WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(fails, FAIL_LIMIT + 1);
    }

    #[test]
    fn a_config_failure_does_not_count_toward_the_limit() {
        // 实测踩到的坑：没配 checkin 端点时点「全部签到」，每次都计连续失败，
        // 5 次就把有效账号自动停用。配置缺失不是账号的错，不累计。
        let c = conn();
        let id = account(&c, "A", "tok");
        for i in 1..=(FAIL_LIMIT + 2) {
            let disabled = apply_result(&c, id, false, "协议缺少 checkin 端点", i, false).unwrap();
            assert!(!disabled);
        }
        let (fails, enabled): (i64, i64) = c
            .query_row(
                "SELECT checkin_fail_count, enabled FROM codebuddy_accounts WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((fails, enabled), (0, 1));
        // 日志仍要记录，让人看得到为什么没签。
        assert_eq!(list_checkin_log(&c, 10).unwrap().len(), (FAIL_LIMIT + 2) as usize);
    }

    #[test]
    fn history_is_newest_first_and_fifo_bounded() {
        let c = conn();
        let id = account(&c, "A", "tok");
        for i in 0..(MAX_LOG_ROWS + 10) {
            apply_result(&c, id, i % 2 == 0, "msg", 1_000 + i, true).unwrap();
        }
        let rows = list_checkin_log(&c, MAX_LOG_ROWS + 50).unwrap();
        assert_eq!(rows.len() as i64, MAX_LOG_ROWS);
        assert_eq!(rows[0].created_at, 1_000 + MAX_LOG_ROWS + 9);
        assert!(rows.windows(2).all(|w| w[0].created_at >= w[1].created_at));
        assert_eq!(rows[0].label.as_deref(), Some("A"), "the account name is joined in");
    }

    #[test]
    fn clear_empties_history_only() {
        let c = conn();
        let id = account(&c, "A", "tok");
        apply_result(&c, id, true, "ok", 1, true).unwrap();
        assert_eq!(clear_checkin_log(&c).unwrap(), 1);
        assert!(list_checkin_log(&c, 10).unwrap().is_empty());
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM codebuddy_accounts", [], |r| r.get::<_, i64>(0)).unwrap(),
            1
        );
    }

    // ---- target selection ----

    #[test]
    fn targets_are_the_enabled_unbanned_accounts() {
        let c = conn();
        let a = account(&c, "A", "tok-a");
        let b = account(&c, "B", "tok-b");
        c.execute("UPDATE codebuddy_accounts SET enabled = 0 WHERE id = ?1", params![b]).unwrap();
        let ids: Vec<i64> = checkin_targets(&c, None).unwrap().into_iter().map(|(id, _, _)| id).collect();
        assert_eq!(ids, vec![a]);
    }

    #[test]
    fn an_expired_credential_is_skipped_without_touching_its_counter() {
        let c = conn();
        let live = account(&c, "live", "tok");
        // JWT-shaped token with exp in the past.
        let expired_raw = fake_jwt(1_000);
        let dead = account(&c, "dead", &expired_raw);
        c.execute("UPDATE codebuddy_accounts SET exp_unix = 1000 WHERE id = ?1", params![dead]).unwrap();
        let ids: Vec<i64> = checkin_targets(&c, None).unwrap().into_iter().map(|(id, _, _)| id).collect();
        assert_eq!(ids, vec![live]);

        // Naming it explicitly still cannot be scheduled.
        assert!(checkin_targets(&c, Some(dead)).unwrap().is_empty());
    }

    #[test]
    fn an_unknown_account_yields_no_targets() {
        let c = conn();
        assert!(checkin_targets(&c, Some(404)).unwrap().is_empty());
    }

    // ---- request construction ----

    #[test]
    fn missing_protocol_is_an_explained_failure() {
        let parsed = ParsedCredential::parse(CredentialKind::Token, "tok");
        assert!(checkin_request(None, &parsed).unwrap_err().contains("未配置协议"));
        let empty = UpstreamProfile {
            login_url: String::new(),
            credential_probe: None,
            checkin: None,
            chat: None,
            models: None,
            ..Default::default()
        };
        assert!(checkin_request(Some(&empty), &parsed).unwrap_err().contains("checkin"));
    }

    #[test]
    fn cookie_credentials_reach_the_checkin_header() {
        let parsed = ParsedCredential::parse(CredentialKind::Cookie, "token=TK; uid=9");
        let profile = UpstreamProfile {
            login_url: String::new(),
            credential_probe: None,
            checkin: Some(adapter::Endpoint {
                method: "POST".into(),
                url: "https://x/sign".into(),
                headers: vec![("cookie".into(), "{cookie}".into())],
                body: Some("{}".into()),
            }),
            chat: None,
            models: None,
            ..Default::default()
        };
        let request = checkin_request(Some(&profile), &parsed).unwrap().unwrap();
        assert_eq!(request.headers[0].1, "token=TK; uid=9");
        assert_eq!(request.body.as_deref(), Some("{}"));
    }

    #[test]
    fn success_summaries_stay_short_and_readable() {
        assert_eq!(summarize_success(""), "已签到");
        assert_eq!(summarize_success(r#"{"code":0,"msg":"签到成功，积分+10"}"#), "签到成功，积分+10");
        assert_eq!(summarize_success(r#"{"data":{"message":"ok"}}"#), "ok");
        assert_eq!(summarize_success(r#"{"code":0}"#), r#"{"code":0}"#);
        assert_eq!(summarize_success(&"x".repeat(400)).chars().count(), 120);
        // 实测 daily-checkin 的回包形状。
        assert_eq!(
            summarize_success(r#"{"code":0,"msg":"OK","data":{"credit":100,"streak_days":10}}"#),
            "领取 100 积分，连登 10 天"
        );
        assert_eq!(
            summarize_success(r#"{"code":0,"msg":"OK","data":{"credit":100}}"#),
            "领取 100 积分"
        );
    }

    // ---- status parsing ----

    #[test]
    fn status_parsing_tolerates_shapes_and_missing_fields() {
        // 实测形状（data 包裹 + snake_case）+ 一个宽松变体（顶层 + camelCase + 字符串布尔）。
        let measured = r#"{"code":0,"msg":"OK","data":{"today_checked_in":true,"streak_days":10,"week_progress":{"checked":4,"total":7},"total_credits":900}}"#;
        let s = parse_checkin_status(measured);
        assert_eq!(s.today_checked_in, Some(true));
        assert_eq!(s.streak_days, Some(10));
        assert_eq!(s.week_progress.as_deref(), Some("4/7"));
        assert_eq!(s.total_credits, Some(900));

        let flat = r#"{"today_checked_in":"false","streakDays":3,"week_progress":0.57,"credit":12}"#;
        let s = parse_checkin_status(flat);
        assert_eq!(s.today_checked_in, Some(false));
        assert_eq!(s.streak_days, Some(3));
        assert_eq!(s.week_progress.as_deref(), Some("57%"));
        assert_eq!(s.total_credits, Some(12));

        assert_eq!(parse_checkin_status("not json"), ParsedCheckinStatus::default());
        assert_eq!(parse_checkin_status("{}"), ParsedCheckinStatus::default());
    }

    #[test]
    fn status_request_needs_its_endpoint() {
        let parsed = ParsedCredential::parse(CredentialKind::Token, "tok");
        assert!(status_request(None, &parsed).unwrap_err().contains("未配置协议"));
        let empty = UpstreamProfile::default();
        assert!(status_request(Some(&empty), &parsed).unwrap_err().contains("checkinStatus"));
    }

    fn fake_jwt(exp: i64) -> String {
        use base64::Engine;
        let enc = |s: &str| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(s.as_bytes())
                .trim_end_matches('=')
                .to_string()
        };
        format!("{}.{}.sig", enc(r#"{"alg":"HS256"}"#), enc(&format!(r#"{{"exp":{exp}}}"#)))
    }
}
