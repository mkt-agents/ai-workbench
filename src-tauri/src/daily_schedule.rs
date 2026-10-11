//! 额度看板「每日定点」调度：WorkBuddy 签到 / Qoder 领积分等共用同一 tick。
//!
//! 厂商只提供：读开关与时间、占日期、跑一轮。HTTP 与 settings 表仍留在各自模块。

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, Timelike};
use rusqlite::Connection;
use tauri::Manager;

use crate::DbState;

const TICK_SECS: u64 = 60;

/// 账号间间隔（签到 / 领积分共用，降低风控压力）。
pub const BETWEEN_ACCOUNTS: Duration = Duration::from_secs(2);

/// Minutes since midnight for "HH:MM"; None when malformed.
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
/// last-run date is not today.
pub fn due_today(time: &str, last_run_date: Option<&str>, now: DateTime<Local>) -> Option<String> {
    let scheduled = parse_hhmm(time)?;
    let today = date_key(now);
    if last_run_date == Some(today.as_str()) {
        return None;
    }
    let minutes_now = now.hour() * 60 + now.minute();
    (minutes_now >= scheduled).then_some(today)
}

#[derive(Debug, Clone)]
pub struct ScheduleState {
    pub enabled: bool,
    pub time: String,
    pub last_run_date: Option<String>,
}

/// 若今日到点且尚未跑过：先占日期，再执行 `run`。未到点返回 `Ok(false)`。
pub async fn run_if_due<F, Fut>(
    conn: Arc<Mutex<Connection>>,
    job_id: &str,
    load: impl FnOnce(&Connection) -> Result<ScheduleState, String>,
    mark_run: impl FnOnce(&Connection, &str) -> Result<(), String>,
    run: F,
) -> Result<bool, String>
where
    F: FnOnce(Arc<Mutex<Connection>>) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let due = {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        let state = load(&guard)?;
        if !state.enabled {
            return Ok(false);
        }
        match due_today(&state.time, state.last_run_date.as_deref(), Local::now()) {
            Some(today) => today,
            None => return Ok(false),
        }
    };
    {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        mark_run(&guard, &due)?;
    }
    if let Err(e) = run(Arc::clone(&conn)).await {
        eprintln!("[{job_id}] daily job failed after claiming {due}: {e}");
        return Err(e);
    }
    Ok(true)
}

/// 单循环驱动全部每日 Job（在 setup 里只 spawn 一次）。
pub fn spawn(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(TICK_SECS)).await;
            if let Err(e) = crate::wb_checkin::auto_tick(&app).await {
                eprintln!("[workbuddy] check-in tick failed: {e}");
            }
            if let Err(e) = crate::qoder_quota::auto_tick(&app).await {
                eprintln!("[qoder] auto-claim tick failed: {e}");
            }
        }
    });
}

/// 供 Job 从 AppHandle 取连接。
pub fn conn_of(app: &tauri::AppHandle) -> Arc<Mutex<Connection>> {
    Arc::clone(&app.state::<DbState>().conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).single().unwrap()
    }

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
        assert_eq!(due_today("09:30", None, nine_thirty - chrono::Duration::minutes(1)), None);
        assert_eq!(due_today("09:30", None, nine_thirty).as_deref(), Some("2026-09-26"));
        assert_eq!(
            due_today("09:30", None, nine_thirty + chrono::Duration::hours(5)).as_deref(),
            Some("2026-09-26")
        );
        assert_eq!(
            due_today("09:30", Some("2026-09-26"), nine_thirty + chrono::Duration::hours(9)),
            None
        );
    }

    #[test]
    fn a_late_startup_catches_the_missed_round() {
        assert_eq!(
            due_today("09:30", Some("2026-09-25"), at(2026, 9, 26, 21, 0)).as_deref(),
            Some("2026-09-26")
        );
    }

    #[test]
    fn next_day_resumes_immediately() {
        assert_eq!(
            due_today("00:05", Some("2026-09-26"), at(2026, 9, 27, 0, 10)).as_deref(),
            Some("2026-09-27")
        );
    }

    #[test]
    fn a_broken_time_setting_disables_the_schedule_rather_than_panicking() {
        assert_eq!(due_today("25:99", None, at(2026, 9, 26, 12, 0)), None);
    }
}
