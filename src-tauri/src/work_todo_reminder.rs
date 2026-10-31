//! Daily work-todo reminder.

use std::sync::Arc;

use chrono::Local;
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::daily_schedule::{self, ScheduleState, date_key};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkTodoRemindPayload {
    pub open_count: u64,
    pub today_count: u64,
    pub backlog_count: u64,
}

fn read_setting(conn: &Connection, key: &str) -> Option<serde_json::Value> {
    let raw: String = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .ok()?;
    serde_json::from_str(&raw).ok()
}

fn setting_bool(conn: &Connection, key: &str) -> bool {
    match read_setting(conn, key) {
        Some(serde_json::Value::Bool(b)) => b,
        Some(serde_json::Value::String(s)) => s == "true" || s == "1",
        _ => false,
    }
}

fn setting_str(conn: &Connection, key: &str) -> String {
    match read_setting(conn, key) {
        Some(serde_json::Value::String(s)) => s,
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn patch_setting(conn: &Connection, key: &str, value: serde_json::Value) -> Result<(), String> {
    conn.execute(
        "INSERT INTO app_settings (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value.to_string()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn count_open_tasks(conn: &Connection, today: &str) -> Result<(u64, u64, u64), String> {
    let open: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM work_tasks WHERE status = 'open' AND (work_date IS NULL OR work_date <= ?1)",
            params![today],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let today_count: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM work_tasks WHERE status = 'open' AND work_date = ?1",
            params![today],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let backlog: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM work_tasks WHERE status = 'open' AND work_date IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok((open, today_count, backlog))
}

pub async fn auto_tick(app: &tauri::AppHandle) -> Result<(), String> {
    let conn = daily_schedule::conn_of(app);
    let app = app.clone();
    let _ran = daily_schedule::run_if_due(
        Arc::clone(&conn),
        "work-todo-remind",
        |c| {
            Ok(ScheduleState {
                enabled: setting_bool(c, "workTodoRemindEnabled"),
                time: {
                    let t = setting_str(c, "workTodoRemindTime");
                    if t.trim().is_empty() {
                        "09:30".to_string()
                    } else {
                        t
                    }
                },
                last_run_date: {
                    let d = setting_str(c, "workTodoRemindLastDate");
                    if d.trim().is_empty() {
                        None
                    } else {
                        Some(d)
                    }
                },
            })
        },
        |c, due| {
            patch_setting(
                c,
                "workTodoRemindLastDate",
                serde_json::Value::String(due.to_string()),
            )
        },
        move |conn| {
            let app = app.clone();
            async move {
                let today = date_key(Local::now());
                let guard = conn.lock().map_err(|e| e.to_string())?;
                let (open, today_count, backlog) = count_open_tasks(&guard, &today)?;
                drop(guard);
                if open == 0 {
                    return Ok(());
                }
                let payload = WorkTodoRemindPayload {
                    open_count: open,
                    today_count,
                    backlog_count: backlog,
                };
                let _ = app.emit("work-todo-remind", &payload);
                let visible = app
                    .get_webview_window("main")
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false);
                if !visible {
                    let _ = crate::tray::open_main_tab(app.clone(), "drafts".to_string());
                } else if let Some(win) = app.get_webview_window("main") {
                    let _ = win.request_user_attention(Some(tauri::UserAttentionType::Informational));
                }
                Ok(())
            }
        },
    )
    .await?;
    Ok(())
}
