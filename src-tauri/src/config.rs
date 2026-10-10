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
///
/// 60 was sized for short answers and quietly sabotaged the report path: a
/// regression report asks for ~3000 tokens of output, which a local model needs
/// well over a minute to produce — so the request timed out, the retry policy
/// retried it, and one "slow" generation became three timed-out rounds. This
/// covers the whole request including the body read, so it has to exceed the
/// longest legitimate generation, not the typical one.
pub const GENERATE_TEXT_TIMEOUT_SECS: u64 = 300;

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
    // 应用内升级
    UpdateSourceUnreachable,
    UpdateAssetMissing,
    UpdateDownloadFailed,
    UpdateSizeMismatch,
    UpdateCancelled,
    UpdateLaunchFailed,
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
            ErrorCode::UpdateSourceUnreachable => "UPDATE_SOURCE_UNREACHABLE",
            ErrorCode::UpdateAssetMissing => "UPDATE_ASSET_MISSING",
            ErrorCode::UpdateDownloadFailed => "UPDATE_DOWNLOAD_FAILED",
            ErrorCode::UpdateSizeMismatch => "UPDATE_SIZE_MISMATCH",
            ErrorCode::UpdateCancelled => "UPDATE_CANCELLED",
            ErrorCode::UpdateLaunchFailed => "UPDATE_LAUNCH_FAILED",
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

/// 额度 / 签到类 HTTP 传输失败的统一文案。reqwest 原文常是
/// `error sending request for url (https://…)`，对用户无意义。
pub const HTTP_NETWORK_MESSAGE: &str = "网络不通，请检查网络或代理后重试";

/// 把 reqwest / 传输层错误收成短中文；网络类走 [`HTTP_NETWORK_MESSAGE`]，
/// 其它情况保留 `prefix: …`，并去掉 `for url (…)` 尾巴。
pub fn friendly_http_error(err: impl std::fmt::Display, prefix: &str) -> String {
    let raw = err.to_string();
    if is_http_transport_failure(&raw) {
        return HTTP_NETWORK_MESSAGE.to_string();
    }
    let short = strip_http_url_noise(&raw);
    if short.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {short}")
    }
}

fn is_http_transport_failure(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    lower.contains("error sending request")
        || lower.contains("connect error")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("connection timed out")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("dns error")
        || lower.contains("failed to lookup address")
        || lower.contains("name or service not known")
        || lower.contains("network unreachable")
        || lower.contains("no route to host")
        || lower.contains("tls handshake")
        || lower.contains("ssl")
        || lower.contains("certificate")
        || lower.contains("proxy")
}

/// Drop ` for url (https://…)` / similar tails so residual messages stay short.
fn strip_http_url_noise(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    for marker in [" for url (", " for url(", " url ("] {
        if let Some(i) = s.find(marker) {
            s.truncate(i);
            break;
        }
    }
    s.trim().trim_end_matches(':').trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{friendly_http_error, HTTP_NETWORK_MESSAGE};

    #[test]
    fn network_send_failure_is_humanized() {
        let raw = "error sending request for url (https://chatgpt.com/backend-api/wham/usage)";
        assert_eq!(friendly_http_error(raw, "请求失败"), HTTP_NETWORK_MESSAGE);
    }

    #[test]
    fn timeout_is_humanized() {
        assert_eq!(
            friendly_http_error("operation timed out", "续期请求失败"),
            HTTP_NETWORK_MESSAGE
        );
    }

    #[test]
    fn unrelated_error_keeps_prefix_without_url() {
        let raw = "unexpected eof for url (https://example.com/x)";
        assert_eq!(friendly_http_error(raw, "请求失败"), "请求失败: unexpected eof");
    }
}
