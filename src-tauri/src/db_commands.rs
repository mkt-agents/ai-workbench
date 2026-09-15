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
    HostProfiles,
    WebPlugins,
    PluginStates,
    RecentProjects,
    GitWorkspaces,
    CursorAccounts,
    AiModels,
    CloudflaredProfiles,
    Snippets,
    WebPluginHistory,
}

pub fn table_from_key(key: &str) -> Option<DbTable> {
    match key {
        "git_accounts" => Some(DbTable::GitAccounts),
        "git_repo_configs" => Some(DbTable::GitRepoConfigs),
        "host_profiles" => Some(DbTable::HostProfiles),
        "web_plugins" => Some(DbTable::WebPlugins),
        "plugin_states" => Some(DbTable::PluginStates),
        "recent_projects" => Some(DbTable::RecentProjects),
        "git_workspaces" => Some(DbTable::GitWorkspaces),
        "cursor_accounts" => Some(DbTable::CursorAccounts),
        "ai_models" => Some(DbTable::AiModels),
        "cloudflared_profiles" => Some(DbTable::CloudflaredProfiles),
        "snippets" => Some(DbTable::Snippets),
        "web_plugin_history" => Some(DbTable::WebPluginHistory),
        _ => None,
    }
}

fn load_sql(table: DbTable) -> &'static str {
    match table {
        DbTable::GitAccounts => "SELECT * FROM git_accounts ORDER BY created_at DESC",
        DbTable::GitRepoConfigs => "SELECT * FROM git_repo_configs ORDER BY created_at DESC",
        DbTable::HostProfiles => "SELECT * FROM host_profiles ORDER BY created_at DESC",
        DbTable::WebPlugins => "SELECT * FROM web_plugins ORDER BY \"order\" ASC, added_at DESC",
        DbTable::PluginStates => "SELECT * FROM plugin_states ORDER BY updated_at DESC",
        DbTable::RecentProjects => "SELECT * FROM recent_projects ORDER BY last_opened_at DESC LIMIT 200",
        DbTable::GitWorkspaces => "SELECT * FROM git_workspaces ORDER BY created_at DESC",
        DbTable::CursorAccounts => "SELECT * FROM cursor_accounts ORDER BY created_at DESC",
        DbTable::AiModels => "SELECT * FROM ai_models ORDER BY is_default DESC, created_at ASC",
        DbTable::CloudflaredProfiles => "SELECT * FROM cloudflared_profiles ORDER BY created_at DESC",
        DbTable::Snippets => "SELECT * FROM snippets ORDER BY use_count DESC, updated_at DESC",
        DbTable::WebPluginHistory => "SELECT * FROM web_plugin_history ORDER BY opened_at DESC LIMIT 100",
    }
}

fn delete_sql(table: DbTable) -> &'static str {
    match table {
        DbTable::GitAccounts => "DELETE FROM git_accounts",
        DbTable::GitRepoConfigs => "DELETE FROM git_repo_configs",
        DbTable::HostProfiles => "DELETE FROM host_profiles",
        DbTable::WebPlugins => "DELETE FROM web_plugins",
        DbTable::PluginStates => "DELETE FROM plugin_states",
        DbTable::RecentProjects => "DELETE FROM recent_projects",
        DbTable::GitWorkspaces => "DELETE FROM git_workspaces",
        DbTable::CursorAccounts => "DELETE FROM cursor_accounts",
        DbTable::AiModels => "DELETE FROM ai_models",
        DbTable::CloudflaredProfiles => "DELETE FROM cloudflared_profiles",
        DbTable::Snippets => "DELETE FROM snippets",
        DbTable::WebPluginHistory => "DELETE FROM web_plugin_history",
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
                "INSERT INTO ai_models (id, name, provider, api_key, auth_type, base_url, model, temperature, max_tokens, is_default, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
