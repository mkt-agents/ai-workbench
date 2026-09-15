//! Tauri command to cancel a long-running request by id.
use crate::cancellation;

#[tauri::command]
pub fn cancel_request(id: String) -> Result<(), String> {
    cancellation::cancel_request(&id);
    Ok(())
}
