//! WorkBuddy Manager domain: CodeBuddy account pool + OpenAI-compatible
//! reverse-proxy gateway.
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
        port INTEGER NOT NULL DEFAULT 8787,
        lan_enabled INTEGER NOT NULL DEFAULT 0,
        checkin_enabled INTEGER NOT NULL DEFAULT 0,
        checkin_time TEXT NOT NULL DEFAULT '09:30',
        last_auto_checkin_date TEXT,
        adapter_json TEXT
    );
    INSERT OR IGNORE INTO wb_settings (id) VALUES (1);
    CREATE TABLE IF NOT EXISTS wb_api_keys (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        key TEXT NOT NULL UNIQUE,
        label TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_at INTEGER NOT NULL,
        rotated_at INTEGER,
        last_used_at INTEGER,
        call_count INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS wb_ip_rules (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL CHECK(kind IN ('allow','deny')),
        ip_or_cidr TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 1,
        note TEXT,
        created_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS wb_request_logs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts INTEGER NOT NULL,
        key_id INTEGER,
        account_id INTEGER,
        model TEXT,
        stream INTEGER NOT NULL DEFAULT 0,
        status_code INTEGER,
        prompt_tokens INTEGER,
        completion_tokens INTEGER,
        latency_ms INTEGER,
        error TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_wb_logs_ts ON wb_request_logs(ts);
    CREATE INDEX IF NOT EXISTS idx_wb_logs_key ON wb_request_logs(key_id, ts);
    CREATE TABLE IF NOT EXISTS wb_checkin_logs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id INTEGER NOT NULL,
        ok INTEGER NOT NULL,
        message TEXT,
        created_at INTEGER NOT NULL
    );
"#;

pub fn ensure_schema(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA_SQL).map_err(|e| e.to_string())?;
    // CREATE TABLE IF NOT EXISTS won't add columns to an existing database;
    // later schema additions go through pragma checks here.
    add_column_if_missing(conn, "wb_api_keys", "rotated_at", "INTEGER")
}

fn add_column_if_missing(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<(), String> {
    let present: bool = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .any(|name| name == column);
    if !present {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotated_at_is_added_to_legacy_key_tables() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE wb_api_keys (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key TEXT NOT NULL UNIQUE,
                label TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                last_used_at INTEGER,
                call_count INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        ensure_schema(&c).unwrap();
        let has: Vec<String> = c
            .prepare("PRAGMA table_info(wb_api_keys)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(has.iter().any(|n| n == "rotated_at"), "legacy table must gain the column");
    }
}
