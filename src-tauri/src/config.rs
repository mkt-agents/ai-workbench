//! Centralized constants and error codes for the Rust backend.
//!
//! Error codes are returned to the frontend as stable identifiers so the UI
//! can translate them; human-readable Chinese strings belong in the i18n JSON.

use serde::Serialize;

/// Default DeepSeek Harness port.
pub const DSH_DEFAULT_PORT: u16 = 3080;

/// Milliseconds to wait for DSH to begin listening after spawn.
pub const DSH_START_WAIT_MS: u64 = 800;

/// Max attempts when retrying a file copy (e.g. locked by another process).
pub const FILE_COPY_MAX_RETRIES: u32 = 8;

/// Sleep between copy retry attempts.
pub const FILE_COPY_RETRY_INTERVAL_MS: u64 = 200;

/// Max attempts when retrying a directory removal.
pub const DIR_REMOVE_MAX_RETRIES: u32 = 5;

/// Sleep between directory removal attempts.
pub const DIR_REMOVE_RETRY_INTERVAL_MS: u64 = 300;

/// Size cap for copying the live Cursor state.vscdb (64 MB). Larger copies are
/// refused to avoid freezing the app when Cursor's DB bloats.
pub const MAX_LIVE_DB_COPY_BYTES: u64 = 64 * 1024 * 1024;

/// Budget for directory size scans: max time before returning partial result.
pub const DIR_SIZE_SCAN_TIMEOUT_MS: u64 = 1500;

/// Budget for directory size scans: max entries to visit.
pub const DIR_SIZE_SCAN_MAX_ENTRIES: u32 = 12_000;

/// Tray status refresh interval (seconds).
pub const TRAY_POLL_INTERVAL_SECS: u64 = 5;

/// Soft-quit deadline before force-killing Cursor (ms).
pub const CURSOR_SOFT_QUIT_TIMEOUT_MS: u64 = 8_000;

/// Grace period after Cursor exits before its files are copied (ms). Long
/// enough for cookies / SQLite WAL to flush, far shorter than the previous
/// fixed 2.5 s stall on every quit.
pub const CURSOR_STOP_SETTLE_MS: u64 = 800;

/// Wait timeout after confirming Cursor stopped (ms).
pub const CURSOR_STOP_WAIT_TIMEOUT_MS: u64 = 15_000;

/// HTTP timeout for model connection tests (seconds).
pub const MODEL_TEST_TIMEOUT_SECS: u64 = 15;

/// HTTP timeout for listing provider models (seconds).
pub const MODEL_LIST_TIMEOUT_SECS: u64 = 20;

/// HTTP timeout for single-shot text generation (seconds).
pub const GENERATE_TEXT_TIMEOUT_SECS: u64 = 60;

/// HTTP timeout for streaming text generation (seconds).
pub const GENERATE_STREAM_TIMEOUT_SECS: u64 = 120;

/// Stable error codes returned to the frontend.
#[derive(Debug, Clone, Copy)]
pub enum ErrorCode {
    InvalidAccountId,
    CursorNotFound,
    CursorDbTooLarge,
    BackupIncomplete,
    AuthMissing,
    // AI / DSH
    AiModelNotConfigured,
    AiEmptyPrompt,
    AiEmptyResponse,
    AiRequestCancelled,
    DshNotInstalled,
    DshStartFailed,
    DshInstallFailed,
    // Generic
    InvalidInput,
    InternalError,
}

impl ErrorCode {
    /// Wire identifier sent to the frontend.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidAccountId => "INVALID_ACCOUNT_ID",
            ErrorCode::CursorNotFound => "CURSOR_NOT_FOUND",
            ErrorCode::CursorDbTooLarge => "CURSOR_DB_TOO_LARGE",
            ErrorCode::BackupIncomplete => "BACKUP_INCOMPLETE",
            ErrorCode::AuthMissing => "AUTH_MISSING",
            ErrorCode::AiModelNotConfigured => "AI_MODEL_NOT_CONFIGURED",
            ErrorCode::AiEmptyPrompt => "AI_EMPTY_PROMPT",
            ErrorCode::AiEmptyResponse => "AI_EMPTY_RESPONSE",
            ErrorCode::AiRequestCancelled => "AI_REQUEST_CANCELLED",
            ErrorCode::DshNotInstalled => "DSH_NOT_INSTALLED",
            ErrorCode::DshStartFailed => "DSH_START_FAILED",
            ErrorCode::DshInstallFailed => "DSH_INSTALL_FAILED",
            ErrorCode::InvalidInput => "INVALID_INPUT",
            ErrorCode::InternalError => "INTERNAL_ERROR",
        }
    }
}

/// Helper to create a structured error from a code + detail message.
pub fn err_code(code: ErrorCode, detail: impl Into<String>) -> WorkbenchError {
    WorkbenchError::new(code, detail)
}

/// Error payload returned to the frontend. The `code` is a stable identifier
/// that the UI translates via i18n; `detail` carries the original message for
/// logging / debugging.
#[derive(Debug, Clone, Serialize)]
pub struct WorkbenchError {
    pub code: &'static str,
    pub detail: String,
}

impl WorkbenchError {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code: code.as_str(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for WorkbenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.detail)
    }
}

impl std::error::Error for WorkbenchError {}
