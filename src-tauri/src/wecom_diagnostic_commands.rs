//! Read-only diagnostic queries; exports return redacted JSON for the frontend save dialog.

#[tauri::command]
pub async fn wecom_session_recent_page(key: Option<String>, limit: Option<usize>) -> Result<crate::wecom_session::RecentEntriesPage, String> {
    tokio::task::spawn_blocking(move || crate::wecom_session::recent_entries_page(key.as_deref(), limit.unwrap_or(30).clamp(1, 200)))
        .await.map_err(|_| "DIAGNOSTICS_TASK_FAILED".to_string())?
}

use crate::wecom_diagnostics::{self, QueryFilter, QueryPage};

#[tauri::command]
pub async fn wecom_diagnostics_query(
    filter: Option<QueryFilter>, cursor: Option<String>, limit: Option<usize>,
) -> Result<QueryPage, String> {
    tokio::task::spawn_blocking(move || wecom_diagnostics::query(filter.unwrap_or_default(), cursor, limit))
        .await.map_err(|_| "DIAGNOSTICS_TASK_FAILED".to_string())?
}

#[tauri::command]
pub async fn wecom_diagnostic_request(request_id: String) -> Result<QueryPage, String> {
    tokio::task::spawn_blocking(move || wecom_diagnostics::request(&request_id))
        .await.map_err(|_| "DIAGNOSTICS_TASK_FAILED".to_string())?
}

#[tauri::command]
pub async fn wecom_diagnostic_export(request_id: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || wecom_diagnostics::export(&request_id))
        .await.map_err(|_| "DIAGNOSTICS_TASK_FAILED".to_string())?
}
