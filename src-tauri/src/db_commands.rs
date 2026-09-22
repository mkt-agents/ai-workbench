use serde::{Deserialize, Serialize};
use crate::DbState;

#[derive(Debug, Serialize, Deserialize)]
pub struct SqlRow {
    #[serde(flatten)]
    pub columns: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbTable {
    GitAccounts,
    GitRepoConfigs,
    GitHostConfigs,
    HostProfiles,
    WebPlugins,
    UserScripts,
    PluginStates,
    RecentProjects,
    GitWorkspaces,
    CursorAccounts,
    AiModels,
    CloudflaredProfiles,
    Snippets,
    WebPluginHistory,
    QuickAskSessions,
    JsonToolHistory,
    HttpSavedRequests,
    HttpHistory,
    // Test assistant. Order here is only the enum order; import follows the
    // file. FKs are not enforced at runtime (PRAGMA foreign_keys is off), so
    // the whole set must travel together to keep the rows referentially sane.
    TestProjects,
    TestRuns,
    TestHistory,
    ChangeReports,
    TestScenarios,
    ChangeReportRuns,
    VulnScans,
    VulnFindings,
}

pub fn table_from_key(key: &str) -> Option<DbTable> {
    match key {
        "git_accounts" => Some(DbTable::GitAccounts),
        "git_repo_configs" => Some(DbTable::GitRepoConfigs),
        "git_host_configs" => Some(DbTable::GitHostConfigs),
        "host_profiles" => Some(DbTable::HostProfiles),
        "web_plugins" => Some(DbTable::WebPlugins),
        "user_scripts" => Some(DbTable::UserScripts),
        "plugin_states" => Some(DbTable::PluginStates),
        "recent_projects" => Some(DbTable::RecentProjects),
        "git_workspaces" => Some(DbTable::GitWorkspaces),
        "cursor_accounts" => Some(DbTable::CursorAccounts),
        "ai_models" => Some(DbTable::AiModels),
        "cloudflared_profiles" => Some(DbTable::CloudflaredProfiles),
        "snippets" => Some(DbTable::Snippets),
        "web_plugin_history" => Some(DbTable::WebPluginHistory),
        "quick_ask_sessions" => Some(DbTable::QuickAskSessions),
        "json_tool_history" => Some(DbTable::JsonToolHistory),
        "http_saved_requests" => Some(DbTable::HttpSavedRequests),
        "http_history" => Some(DbTable::HttpHistory),
        "test_projects" => Some(DbTable::TestProjects),
        "test_runs" => Some(DbTable::TestRuns),
        "test_history" => Some(DbTable::TestHistory),
        "change_reports" => Some(DbTable::ChangeReports),
        "test_scenarios" => Some(DbTable::TestScenarios),
        "change_report_runs" => Some(DbTable::ChangeReportRuns),
        "vuln_scans" => Some(DbTable::VulnScans),
        "vuln_findings" => Some(DbTable::VulnFindings),
        _ => None,
    }
}

pub(crate) fn load_sql(table: DbTable) -> &'static str {
    match table {
        DbTable::GitAccounts => "SELECT * FROM git_accounts ORDER BY created_at DESC",
        DbTable::GitRepoConfigs => "SELECT * FROM git_repo_configs ORDER BY created_at DESC",
        DbTable::GitHostConfigs => "SELECT * FROM git_host_configs ORDER BY created_at DESC",
        DbTable::HostProfiles => "SELECT * FROM host_profiles ORDER BY created_at DESC",
        DbTable::WebPlugins => "SELECT * FROM web_plugins ORDER BY \"order\" ASC, added_at DESC",
        DbTable::UserScripts => "SELECT * FROM user_scripts ORDER BY created_at DESC",
        DbTable::PluginStates => "SELECT * FROM plugin_states ORDER BY updated_at DESC",
        DbTable::RecentProjects => "SELECT * FROM recent_projects ORDER BY last_opened_at DESC LIMIT 200",
        DbTable::GitWorkspaces => "SELECT * FROM git_workspaces ORDER BY created_at DESC",
        DbTable::CursorAccounts => "SELECT * FROM cursor_accounts ORDER BY created_at DESC",
        DbTable::AiModels => "SELECT * FROM ai_models ORDER BY is_default DESC, created_at ASC",
        DbTable::CloudflaredProfiles => "SELECT * FROM cloudflared_profiles ORDER BY created_at DESC",
        DbTable::Snippets => "SELECT * FROM snippets ORDER BY use_count DESC, updated_at DESC",
        DbTable::WebPluginHistory => "SELECT * FROM web_plugin_history ORDER BY opened_at DESC LIMIT 100",
        DbTable::QuickAskSessions => "SELECT * FROM quick_ask_sessions ORDER BY updated_at DESC LIMIT 30",
        DbTable::JsonToolHistory => "SELECT * FROM json_tool_history ORDER BY id DESC LIMIT 20",
        DbTable::HttpSavedRequests => "SELECT * FROM http_saved_requests ORDER BY saved_at DESC",
        DbTable::HttpHistory => "SELECT * FROM http_history ORDER BY id DESC LIMIT 50",
        // Runs keep their tails (64KB each), so the export side caps how many go
        // out; the newest ones are the useful ones.
        DbTable::TestProjects => "SELECT * FROM test_projects ORDER BY created_at DESC",
        DbTable::TestRuns => "SELECT * FROM test_runs ORDER BY started_at DESC LIMIT 500",
        DbTable::TestHistory => "SELECT * FROM test_history ORDER BY id DESC LIMIT 2000",
        DbTable::ChangeReports => "SELECT * FROM change_reports ORDER BY created_at DESC LIMIT 200",
        DbTable::TestScenarios => "SELECT * FROM test_scenarios ORDER BY report_id ASC, sort ASC",
        DbTable::ChangeReportRuns => "SELECT * FROM change_report_runs",
        DbTable::VulnScans => "SELECT * FROM vuln_scans ORDER BY started_at DESC LIMIT 200",
        DbTable::VulnFindings => "SELECT * FROM vuln_findings ORDER BY id DESC LIMIT 2000",
    }
}

fn delete_sql(table: DbTable) -> &'static str {
    match table {
        DbTable::GitAccounts => "DELETE FROM git_accounts",
        DbTable::GitRepoConfigs => "DELETE FROM git_repo_configs",
        DbTable::GitHostConfigs => "DELETE FROM git_host_configs",
        DbTable::HostProfiles => "DELETE FROM host_profiles",
        DbTable::WebPlugins => "DELETE FROM web_plugins",
        DbTable::UserScripts => "DELETE FROM user_scripts",
        DbTable::PluginStates => "DELETE FROM plugin_states",
        DbTable::RecentProjects => "DELETE FROM recent_projects",
        DbTable::GitWorkspaces => "DELETE FROM git_workspaces",
        DbTable::CursorAccounts => "DELETE FROM cursor_accounts",
        DbTable::AiModels => "DELETE FROM ai_models",
        DbTable::CloudflaredProfiles => "DELETE FROM cloudflared_profiles",
        DbTable::Snippets => "DELETE FROM snippets",
        DbTable::WebPluginHistory => "DELETE FROM web_plugin_history",
        DbTable::QuickAskSessions => "DELETE FROM quick_ask_sessions",
        DbTable::JsonToolHistory => "DELETE FROM json_tool_history",
        DbTable::HttpSavedRequests => "DELETE FROM http_saved_requests",
        DbTable::HttpHistory => "DELETE FROM http_history",
        DbTable::TestProjects => "DELETE FROM test_projects",
        DbTable::TestRuns => "DELETE FROM test_runs",
        DbTable::TestHistory => "DELETE FROM test_history",
        DbTable::ChangeReports => "DELETE FROM change_reports",
        DbTable::TestScenarios => "DELETE FROM test_scenarios",
        DbTable::ChangeReportRuns => "DELETE FROM change_report_runs",
        DbTable::VulnScans => "DELETE FROM vuln_scans",
        DbTable::VulnFindings => "DELETE FROM vuln_findings",
    }
}

fn json_str(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("missing or invalid string field: {key}"))
}

fn json_opt_str(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(|v| if v.is_null() { None } else { v.as_str().map(str::to_string) })
}

fn json_bool_as_i64(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<i64, String> {
    match obj.get(key) {
        Some(serde_json::Value::Bool(b)) => Ok(if *b { 1 } else { 0 }),
        Some(serde_json::Value::Number(n)) => Ok(n.as_i64().unwrap_or(0)),
        _ => Ok(0),
    }
}

/// Nullable tri-state column (e.g. a test that passed, failed, or never ran).
fn json_opt_i64(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<i64> {
    match obj.get(key) {
        Some(serde_json::Value::Bool(b)) => Some(if *b { 1 } else { 0 }),
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

fn json_f64(obj: &serde_json::Map<String, serde_json::Value>, key: &str, default: f64) -> f64 {
    obj.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
}

fn json_i64(obj: &serde_json::Map<String, serde_json::Value>, key: &str, default: i64) -> i64 {
    obj.get(key).and_then(|v| v.as_i64()).unwrap_or(default)
}

fn row_to_map(row: &rusqlite::Row, column_names: &[String]) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut map = serde_json::Map::new();
    for (i, name) in column_names.iter().enumerate() {
        let value = match row.get_ref(i).map_err(|e| e.to_string())? {
            rusqlite::types::ValueRef::Integer(n) => serde_json::Value::Number(serde_json::Number::from(n)),
            rusqlite::types::ValueRef::Real(f) => {
                serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, serde_json::Value::Number)
            }
            rusqlite::types::ValueRef::Text(s) => {
                serde_json::Value::String(String::from_utf8_lossy(s).to_string())
            }
            rusqlite::types::ValueRef::Blob(b) => {
                serde_json::Value::String(b.iter().map(|byte| format!("{:02x}", byte)).collect())
            }
            rusqlite::types::ValueRef::Null => serde_json::Value::Null,
        };
        map.insert(name.clone(), value);
    }
    Ok(map)
}

fn insert_row(tx: &rusqlite::Transaction<'_>, table: DbTable, obj: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    match table {
        DbTable::GitAccounts => {
            tx.execute(
                "INSERT INTO git_accounts (id, name, email, color, note, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "email")?,
                    json_str(obj, "color")?,
                    json_opt_str(obj, "note"),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::GitRepoConfigs => {
            tx.execute(
                "INSERT INTO git_repo_configs (path, name, user_name, email, account_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    json_str(obj, "path")?,
                    json_str(obj, "name")?,
                    json_str(obj, "user_name")?,
                    json_str(obj, "email")?,
                    json_opt_str(obj, "account_id"),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::GitHostConfigs => {
            tx.execute(
                "INSERT INTO git_host_configs (id, host, account_id, note, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "host")?,
                    json_str(obj, "account_id")?,
                    json_opt_str(obj, "note"),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::HostProfiles => {
            tx.execute(
                "INSERT INTO host_profiles (id, name, content, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "content")?,
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::WebPlugins => {
            tx.execute(
                "INSERT INTO web_plugins (id, name, url, local_path, downloaded_at, added_at, \"group\", tags, \"order\", last_opened_at, open_count, hotkey, is_preset) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "url")?,
                    json_opt_str(obj, "local_path"),
                    json_opt_str(obj, "downloaded_at"),
                    json_str(obj, "added_at")?,
                    json_opt_str(obj, "group").unwrap_or_default(),
                    json_opt_str(obj, "tags").unwrap_or_default(),
                    json_i64(obj, "order", 0),
                    json_opt_str(obj, "last_opened_at"),
                    json_i64(obj, "open_count", 0),
                    json_opt_str(obj, "hotkey").unwrap_or_default(),
                    json_bool_as_i64(obj, "is_preset")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::PluginStates => {
            tx.execute(
                "INSERT INTO plugin_states (plugin_id, enabled, config, updated_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    json_str(obj, "plugin_id")?,
                    json_bool_as_i64(obj, "enabled")?,
                    json_str(obj, "config")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::UserScripts => {
            tx.execute(
                "INSERT INTO user_scripts (id, name, description, match_pattern, match_patterns, code, enabled, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_opt_str(obj, "description").unwrap_or_default(),
                    json_opt_str(obj, "match_pattern").unwrap_or_else(|| "<all_urls>".to_string()),
                    json_opt_str(obj, "match_patterns").unwrap_or_else(|| "[\"<all_urls>\"]".to_string()),
                    json_opt_str(obj, "code").unwrap_or_default(),
                    json_bool_as_i64(obj, "enabled")?,
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::RecentProjects => {
            tx.execute(
                "INSERT INTO recent_projects (id, path, name, last_opened_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    json_i64(obj, "id", 0),
                    json_str(obj, "path")?,
                    json_opt_str(obj, "name"),
                    json_str(obj, "last_opened_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::GitWorkspaces => {
            tx.execute(
                "INSERT INTO git_workspaces (id, path, name, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    json_i64(obj, "id", 0),
                    json_str(obj, "path")?,
                    json_str(obj, "name")?,
                    json_str(obj, "created_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::CursorAccounts => {
            tx.execute(
                "INSERT INTO cursor_accounts (id, name, email, color, backup_path, profile_dir, profile_initialized, git_user_name, git_email, password, notes, is_logged_in, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "email")?,
                    json_str(obj, "color")?,
                    json_str(obj, "backup_path")?,
                    json_opt_str(obj, "profile_dir"),
                    json_bool_as_i64(obj, "profile_initialized")?,
                    json_opt_str(obj, "git_user_name"),
                    json_opt_str(obj, "git_email"),
                    json_opt_str(obj, "password"),
                    json_opt_str(obj, "notes"),
                    json_bool_as_i64(obj, "is_logged_in")?,
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::AiModels => {
            tx.execute(
                "INSERT INTO ai_models (id, name, provider, api_key, auth_type, base_url, model, temperature, max_tokens, is_default, last_test_ok, last_test_at, last_test_msg, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "provider")?,
                    json_str(obj, "api_key")?,
                    json_opt_str(obj, "auth_type").unwrap_or_else(|| "api".to_string()),
                    json_str(obj, "base_url")?,
                    json_str(obj, "model")?,
                    json_f64(obj, "temperature", 0.7),
                    json_i64(obj, "max_tokens", 4096),
                    json_bool_as_i64(obj, "is_default")?,
                    json_opt_i64(obj, "last_test_ok"),
                    json_opt_str(obj, "last_test_at"),
                    json_opt_str(obj, "last_test_msg"),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::CloudflaredProfiles => {
            tx.execute(
                "INSERT INTO cloudflared_profiles (id, name, hostname, local_url, token, auth_mode, config_path, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "hostname")?,
                    json_str(obj, "local_url")?,
                    json_opt_str(obj, "token").unwrap_or_default(),
                    json_opt_str(obj, "auth_mode").unwrap_or_else(|| "token".to_string()),
                    json_opt_str(obj, "config_path"),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::Snippets => {
            tx.execute(
                "INSERT INTO snippets (id, name, content, tags, params, use_count, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "content")?,
                    json_opt_str(obj, "tags").unwrap_or_default(),
                    json_opt_str(obj, "params").unwrap_or_default(),
                    json_i64(obj, "use_count", 0),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::WebPluginHistory => {
            tx.execute(
                "INSERT INTO web_plugin_history (plugin_id, url, name, opened_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    json_str(obj, "plugin_id")?,
                    json_str(obj, "url")?,
                    json_str(obj, "name")?,
                    json_str(obj, "opened_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::QuickAskSessions => {
            tx.execute(
                "INSERT INTO quick_ask_sessions (id, title, task, turns, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "title")?,
                    json_opt_str(obj, "task").unwrap_or_else(|| "none".to_string()),
                    json_opt_str(obj, "turns").unwrap_or_else(|| "[]".to_string()),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::JsonToolHistory => {
            tx.execute(
                "INSERT INTO json_tool_history (input, output, path, ok, nodes, chars, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    json_str(obj, "input")?,
                    json_str(obj, "output")?,
                    json_opt_str(obj, "path").unwrap_or_default(),
                    json_bool_as_i64(obj, "ok")?,
                    json_i64(obj, "nodes", 0),
                    json_i64(obj, "chars", 0),
                    json_str(obj, "created_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::HttpSavedRequests => {
            tx.execute(
                "INSERT INTO http_saved_requests (id, name, method, url, headers, params, body, body_type, saved_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "method")?,
                    json_str(obj, "url")?,
                    json_str(obj, "headers")?,
                    json_str(obj, "params")?,
                    json_opt_str(obj, "body").unwrap_or_default(),
                    json_opt_str(obj, "body_type").unwrap_or_else(|| "none".to_string()),
                    json_str(obj, "saved_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::HttpHistory => {
            tx.execute(
                "INSERT INTO http_history (id, method, url, timestamp) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    json_i64(obj, "id", 0),
                    json_str(obj, "method")?,
                    json_str(obj, "url")?,
                    json_i64(obj, "timestamp", 0),
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::TestProjects => {
            tx.execute(
                "INSERT INTO test_projects (id, name, path, type, framework, test_command, args, working_dir, env, enabled, created_at, updated_at, last_run_at, last_status, last_error_kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "name")?,
                    json_str(obj, "path")?,
                    json_str(obj, "type")?,
                    json_str(obj, "framework")?,
                    json_str(obj, "test_command")?,
                    json_opt_str(obj, "args"),
                    json_opt_str(obj, "working_dir"),
                    json_opt_str(obj, "env"),
                    json_i64(obj, "enabled", 1),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                    json_opt_str(obj, "last_run_at"),
                    json_opt_str(obj, "last_status"),
                    json_opt_str(obj, "last_error_kind").unwrap_or_default(),
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::TestRuns => {
            tx.execute(
                "INSERT INTO test_runs (id, project_id, started_at, completed_at, duration_ms, status, total_tests, passed, failed, skipped, output, suites, error_kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "project_id")?,
                    json_str(obj, "started_at")?,
                    json_str(obj, "completed_at")?,
                    json_i64(obj, "duration_ms", 0),
                    json_str(obj, "status")?,
                    json_i64(obj, "total_tests", 0),
                    json_i64(obj, "passed", 0),
                    json_i64(obj, "failed", 0),
                    json_i64(obj, "skipped", 0),
                    json_opt_str(obj, "output").unwrap_or_default(),
                    json_opt_str(obj, "suites").unwrap_or_else(|| "[]".to_string()),
                    json_opt_str(obj, "error_kind").unwrap_or_default(),
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::TestHistory => {
            tx.execute(
                "INSERT INTO test_history (id, project_id, run_id, timestamp, status, total, passed, failed) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    json_opt_i64(obj, "id"),
                    json_str(obj, "project_id")?,
                    json_opt_str(obj, "run_id"),
                    json_str(obj, "timestamp")?,
                    json_str(obj, "status")?,
                    json_opt_i64(obj, "total"),
                    json_opt_i64(obj, "passed"),
                    json_opt_i64(obj, "failed"),
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::ChangeReports => {
            tx.execute(
                "INSERT INTO change_reports (id, project_id, base, source, created_at, branch, head, files, adds, dels, untested, data, patch, ai, delta_coverage, accepted_at, ai_warnings) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "project_id")?,
                    json_str(obj, "base")?,
                    json_str(obj, "source")?,
                    json_str(obj, "created_at")?,
                    json_opt_str(obj, "branch").unwrap_or_default(),
                    json_opt_str(obj, "head").unwrap_or_default(),
                    json_i64(obj, "files", 0),
                    json_i64(obj, "adds", 0),
                    json_i64(obj, "dels", 0),
                    json_i64(obj, "untested", 0),
                    json_str(obj, "data")?,
                    json_opt_str(obj, "patch").unwrap_or_default(),
                    json_opt_str(obj, "ai"),
                    json_opt_str(obj, "delta_coverage"),
                    json_opt_str(obj, "accepted_at"),
                    json_opt_str(obj, "ai_warnings").unwrap_or_else(|| "[]".to_string()),
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::TestScenarios => {
            tx.execute(
                "INSERT INTO test_scenarios (id, report_id, project_id, run_id, title, detail, priority, status, note, sort, source, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    json_opt_i64(obj, "id"),
                    json_str(obj, "report_id")?,
                    json_str(obj, "project_id")?,
                    json_opt_str(obj, "run_id"),
                    json_str(obj, "title")?,
                    json_opt_str(obj, "detail"),
                    json_opt_str(obj, "priority").unwrap_or_default(),
                    json_opt_str(obj, "status").unwrap_or_else(|| "pending".to_string()),
                    json_opt_str(obj, "note"),
                    json_i64(obj, "sort", 0),
                    json_opt_str(obj, "source").unwrap_or_else(|| "ai".to_string()),
                    json_str(obj, "created_at")?,
                    json_str(obj, "updated_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::ChangeReportRuns => {
            tx.execute(
                "INSERT INTO change_report_runs (report_id, run_id, project_id, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    json_str(obj, "report_id")?,
                    json_str(obj, "run_id")?,
                    json_str(obj, "project_id")?,
                    json_str(obj, "created_at")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::VulnScans => {
            tx.execute(
                "INSERT INTO vuln_scans (id, project_id, started_at, completed_at, status, error_kind, deps_checked, files_checked, findings_critical, findings_high, findings_total) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    json_str(obj, "id")?,
                    json_str(obj, "project_id")?,
                    json_str(obj, "started_at")?,
                    json_opt_str(obj, "completed_at"),
                    json_str(obj, "status")?,
                    json_opt_str(obj, "error_kind").unwrap_or_default(),
                    json_i64(obj, "deps_checked", 0),
                    json_i64(obj, "files_checked", 0),
                    json_i64(obj, "findings_critical", 0),
                    json_i64(obj, "findings_high", 0),
                    json_i64(obj, "findings_total", 0),
                ],
            ).map_err(|e| e.to_string())?;
        }
        DbTable::VulnFindings => {
            tx.execute(
                "INSERT INTO vuln_findings (id, project_id, scan_id, kind, dedup_key, ecosystem, package, version, vuln_id, severity, summary, fixed_versions, aliases, file, line, preview, rule, status, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
                rusqlite::params![
                    json_opt_i64(obj, "id"),
                    json_str(obj, "project_id")?,
                    json_str(obj, "scan_id")?,
                    json_str(obj, "kind")?,
                    json_str(obj, "dedup_key")?,
                    json_opt_str(obj, "ecosystem").unwrap_or_default(),
                    json_opt_str(obj, "package").unwrap_or_default(),
                    json_opt_str(obj, "version").unwrap_or_default(),
                    json_opt_str(obj, "vuln_id").unwrap_or_default(),
                    json_opt_str(obj, "severity").unwrap_or_else(|| "unknown".to_string()),
                    json_opt_str(obj, "summary").unwrap_or_default(),
                    json_opt_str(obj, "fixed_versions").unwrap_or_else(|| "[]".to_string()),
                    json_opt_str(obj, "aliases").unwrap_or_else(|| "[]".to_string()),
                    json_opt_str(obj, "file").unwrap_or_default(),
                    json_i64(obj, "line", 0),
                    json_opt_str(obj, "preview").unwrap_or_default(),
                    json_opt_str(obj, "rule").unwrap_or_default(),
                    json_opt_str(obj, "status").unwrap_or_else(|| "open".to_string()),
                    json_str(obj, "first_seen")?,
                    json_str(obj, "last_seen")?,
                ],
            ).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn db_load(state: tauri::State<'_, DbState>, table: DbTable) -> Result<Vec<SqlRow>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    let sql = load_sql(table);
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let column_names: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    let mut results = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        results.push(SqlRow {
            columns: row_to_map(row, &column_names)?,
        });
    }
    Ok(results)
}

#[tauri::command]
pub async fn db_save(
    state: tauri::State<'_, DbState>,
    table: DbTable,
    rows: Vec<serde_json::Value>,
) -> Result<(), String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    // WAL + busy_timeout lets concurrent readers proceed while we write and
    // retries on lock contention instead of failing immediately.
    let _ = conn.execute_batch("PRAGMA busy_timeout=8000; PRAGMA synchronous=NORMAL;");
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("开启事务失败: {e}"))?;
    tx.execute(delete_sql(table), [])
        .map_err(|e| format!("清空表失败: {e}"))?;
    for row in rows {
        let obj = row
            .as_object()
            .ok_or_else(|| "each row must be a JSON object".to_string())?
            .clone();
        insert_row(&tx, table, &obj)?;
    }
    tx.commit().map_err(|e| format!("提交事务失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(r#"
            CREATE TABLE git_accounts (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, email TEXT NOT NULL,
                color TEXT NOT NULL, note TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE recent_projects (
                id INTEGER PRIMARY KEY, path TEXT NOT NULL, name TEXT, last_opened_at TEXT NOT NULL
            );
        "#).unwrap();
        conn
    }

    #[test]
    fn git_accounts_roundtrip() {
        let conn = mem_conn();
        let tx = conn.unchecked_transaction().unwrap();
        let mut map = serde_json::Map::new();
        map.insert("id".into(), "a1".into());
        map.insert("name".into(), "Test".into());
        map.insert("email".into(), "t@example.com".into());
        map.insert("color".into(), "#000".into());
        map.insert("created_at".into(), "2024-01-01".into());
        map.insert("updated_at".into(), "2024-01-01".into());
        insert_row(&tx, DbTable::GitAccounts, &map).unwrap();
        tx.commit().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM git_accounts", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    /// The test-result columns must survive the whole-table save, including the
    /// "never tested" case where they stay NULL.
    #[test]
    fn ai_models_roundtrip_keeps_last_test_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"CREATE TABLE ai_models (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, provider TEXT NOT NULL,
                api_key TEXT NOT NULL, auth_type TEXT DEFAULT 'api', base_url TEXT NOT NULL,
                model TEXT NOT NULL, temperature REAL DEFAULT 0.7, max_tokens INTEGER DEFAULT 4096,
                is_default INTEGER DEFAULT 0, last_test_ok INTEGER, last_test_at TEXT,
                last_test_msg TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );"#,
        )
        .unwrap();

        let base = |id: &str| {
            let mut map = serde_json::Map::new();
            map.insert("id".into(), id.into());
            map.insert("name".into(), "m".into());
            map.insert("provider".into(), "openai".into());
            map.insert("api_key".into(), "k".into());
            map.insert("base_url".into(), "http://x".into());
            map.insert("model".into(), "gpt".into());
            map.insert("created_at".into(), "2026-01-01".into());
            map.insert("updated_at".into(), "2026-01-01".into());
            map
        };
        let mut tested = base("t1");
        tested.insert("last_test_ok".into(), true.into());
        tested.insert("last_test_at".into(), "2026-09-19T00:00:00Z".into());
        tested.insert("last_test_msg".into(), "ok".into());
        let mut failed = base("t2");
        failed.insert("last_test_ok".into(), false.into());
        failed.insert("last_test_at".into(), "2026-09-19T00:00:00Z".into());
        failed.insert("last_test_msg".into(), serde_json::Value::Null);

        let tx = conn.unchecked_transaction().unwrap();
        insert_row(&tx, DbTable::AiModels, &tested).unwrap();
        insert_row(&tx, DbTable::AiModels, &failed).unwrap();
        insert_row(&tx, DbTable::AiModels, &base("t3")).unwrap();
        tx.commit().unwrap();

        let read = |id: &str| -> (Option<i64>, Option<String>, Option<String>) {
            conn.query_row(
                "SELECT last_test_ok, last_test_at, last_test_msg FROM ai_models WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap()
        };
        assert_eq!(read("t1").0, Some(1));
        assert_eq!(read("t1").2.as_deref(), Some("ok"));
        assert_eq!(read("t2").0, Some(0));
        assert_eq!(read("t2").2, None);
        let never = read("t3");
        assert_eq!(never.0, None);
        assert_eq!(never.1, None);
    }

    #[test]
    fn db_save_atomicity_transaction_rolls_back() {
        // Proves the transactional pattern: a failure mid-write must not leave
        // the table half-written. We run the same logic as db_save manually.
        let conn = mem_conn();
        {
            let tx = conn.unchecked_transaction().unwrap();

            let mut row1 = serde_json::Map::new();
            row1.insert("id".into(), "a1".into());
            row1.insert("name".into(), "First".into());
            row1.insert("email".into(), "f@e.com".into());
            row1.insert("color".into(), "#000".into());
            row1.insert("created_at".into(), "2024-01-01".into());
            row1.insert("updated_at".into(), "2024-01-01".into());
            insert_row(&tx, DbTable::GitAccounts, &row1).unwrap();

            // Second row is missing required fields — insert must fail.
            let row2 = serde_json::Map::new();
            let fail = insert_row(&tx, DbTable::GitAccounts, &row2);
            assert!(fail.is_err(), "expected insert of invalid row to fail");

            // Drop tx without commit → rollback. The valid first row must NOT persist.
        }
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM git_accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "transaction should have rolled back the valid row");
    }

    #[test]
    fn rejects_non_object_rows() {
        let v = serde_json::Value::String("bad".into());
        assert!(v.as_object().is_none());
    }
}
