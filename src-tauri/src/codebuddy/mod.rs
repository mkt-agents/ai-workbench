//! WorkBuddy Manager domain: CodeBuddy account pool + daily check-in.
//!
//! The upstream protocol (login page, check-in endpoint, chat endpoint) is not
//! hardcoded anywhere in this module — everything flows through the configurable
//! `UpstreamProfile` stored in `wb_settings.adapter_json`, so the real endpoints
//! can be pinned down by packet capture without a rebuild.

pub mod adapter;
pub mod credential;

pub use adapter::{Endpoint, OutboundRequest, UpstreamProfile};
pub use credential::{CredentialKind, ParsedCredential};

/// Tables owned by the WorkBuddy domain. Idempotent; called from setup next to
/// the other schemas (same pattern as `report_history::ensure_schema`).
pub const SCHEMA_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS codebuddy_accounts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        label TEXT NOT NULL,
        email TEXT,
        credential_type TEXT NOT NULL CHECK(credential_type IN ('token','cookie')),
        credential TEXT NOT NULL,
        extra_json TEXT,
        exp_unix INTEGER,
        status TEXT NOT NULL DEFAULT 'active',
        enabled INTEGER NOT NULL DEFAULT 1,
        last_checkin_at INTEGER,
        last_checkin_status TEXT,
        checkin_fail_count INTEGER NOT NULL DEFAULT 0,
        notes TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_cb_accounts_enabled
        ON codebuddy_accounts(enabled, status);
    CREATE TABLE IF NOT EXISTS wb_settings (
        id INTEGER PRIMARY KEY CHECK(id = 1),
        checkin_enabled INTEGER NOT NULL DEFAULT 0,
        checkin_time TEXT NOT NULL DEFAULT '09:30',
        last_auto_checkin_date TEXT,
        adapter_json TEXT
    );
    INSERT OR IGNORE INTO wb_settings (id) VALUES (1);
    CREATE TABLE IF NOT EXISTS wb_checkin_logs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id INTEGER NOT NULL,
        ok INTEGER NOT NULL,
        message TEXT,
        created_at INTEGER NOT NULL
    );
"#;

pub fn ensure_schema(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA_SQL).map_err(|e| e.to_string())
}
