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

/// 实测可用的协议（2026-10 抓包，与开发机 `wb_settings.adapter_json` 一致）。
/// 新装没有它时登录窗打不开、签到 / 积分全不可用；只含 `{token}` 占位符，不含任何凭证。
pub const DEFAULT_PROFILE_JSON: &str = include_str!("default_profile.json");

pub fn ensure_schema(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA_SQL).map_err(|e| e.to_string())?;
    // 只填空值：用户在「协议配置」里改过的保持不动；清空后下次启动会重新填回默认。
    conn.execute(
        "UPDATE wb_settings SET adapter_json = ?1
          WHERE id = 1 AND (adapter_json IS NULL OR trim(adapter_json) = '')",
        [DEFAULT_PROFILE_JSON],
    )
    .map_err(|e| e.to_string())?;
    let has_tag: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('codebuddy_accounts') WHERE name = 'tag'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .map_err(|e| e.to_string())?;
    if !has_tag {
        conn.execute("ALTER TABLE codebuddy_accounts ADD COLUMN tag TEXT", [])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
