//! Gateway API keys (`wk-...`) handed out to clients. The plaintext key is
//! returned exactly once, at creation/reset — every later read is masked, so a
//! reopened panel cannot screenshot live secrets.

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::DbState;

const KEY_PREFIX: &str = "wk-";
/// 32 hex chars — enough that guessing collides with nothing in practice.
const KEY_BODY_BYTES: usize = 16;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WbKeyDto {
    pub id: i64,
    pub key_masked: String,
    pub label: String,
    pub enabled: bool,
    pub created_at: i64,
    pub rotated_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub call_count: i64,
}

/// Newly created key — `key` is the only place the full secret is exposed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WbCreatedKey {
    pub id: i64,
    pub key: String,
    pub label: String,
}

pub fn generate_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; KEY_BODY_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    format!(
        "{KEY_PREFIX}{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// `wk-abcd…wxyz (label)` style mask: prefix + first 2 + last 4 of the body.
pub fn mask_key(key: &str) -> String {
    let body = key.strip_prefix(KEY_PREFIX).unwrap_or(key);
    if body.len() <= 8 {
        return format!("{KEY_PREFIX}****");
    }
    format!("{}{}…{}", KEY_PREFIX, &body[..2], &body[body.len() - 4..])
}

pub fn create_key(conn: &Connection, label: &str, now_ms: i64) -> Result<WbCreatedKey, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("密钥备注不能为空".to_string());
    }
    // Retry on the (astronomically unlikely) UNIQUE collision rather than
    // surfacing a database error to the user.
    for _ in 0..3 {
        let key = generate_key();
        let inserted = conn.execute(
            "INSERT INTO wb_api_keys (key, label, enabled, created_at) VALUES (?1, ?2, 1, ?3)",
            params![key, label, now_ms],
        );
        if inserted.is_ok() {
            return Ok(WbCreatedKey {
                id: conn.last_insert_rowid(),
                key,
                label: label.to_string(),
            });
        }
    }
    Err("密钥生成失败，请重试".to_string())
}

pub fn list_keys(conn: &Connection) -> Result<Vec<WbKeyDto>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, key, label, enabled, created_at, rotated_at, last_used_at, call_count
               FROM wb_api_keys ORDER BY id DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let key: String = row.get(1)?;
            Ok(WbKeyDto {
                id: row.get(0)?,
                key_masked: mask_key(&key),
                label: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                created_at: row.get(4)?,
                rotated_at: row.get(5)?,
                last_used_at: row.get(6)?,
                call_count: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn set_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<(), String> {
    conn.execute(
        "UPDATE wb_api_keys SET enabled = ?2 WHERE id = ?1",
        params![id, i64::from(enabled)],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_key(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM wb_api_keys WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Rotate in place: same id/label, brand new secret.
pub fn reset_key(conn: &Connection, id: i64, now_ms: i64) -> Result<String, String> {
    let exists: i64 = conn
        .query_row("SELECT COUNT(*) FROM wb_api_keys WHERE id = ?1", params![id], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if exists == 0 {
        return Err(format!("密钥 {id} 不存在"));
    }
    for _ in 0..3 {
        let key = generate_key();
        if conn
            .execute(
                "UPDATE wb_api_keys SET key = ?2, rotated_at = ?3 WHERE id = ?1",
                params![id, key, now_ms],
            )
            .is_ok()
        {
            return Ok(key);
        }
    }
    Err("密钥重置失败，请重试".to_string())
}

/// Resolve a presented bearer token to its key id (None = reject).
pub fn authenticate(conn: &Connection, key: &str, now_ms: i64) -> Option<i64> {
    let id: i64 = conn
        .query_row(
            "SELECT id FROM wb_api_keys WHERE key = ?1 AND enabled = 1",
            params![key],
            |r| r.get(0),
        )
        .ok()?;
    let _ = conn.execute(
        "UPDATE wb_api_keys SET last_used_at = ?2, call_count = call_count + 1 WHERE id = ?1",
        params![id, now_ms],
    );
    Some(id)
}

/// Enabled-key count, used to refuse LAN mode without any way to auth.
pub fn enabled_count(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM wb_api_keys WHERE enabled = 1", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn with_db<T, F>(state: &tauri::State<'_, DbState>, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T, String> + Send + 'static,
{
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        f(&guard)
    })
    .await
    .map_err(|e| format!("任务失败: {}", e))?
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[tauri::command]
pub async fn wb_key_create(
    state: tauri::State<'_, DbState>,
    label: String,
) -> Result<WbCreatedKey, String> {
    with_db(&state, move |conn| create_key(conn, &label, now_ms())).await
}

#[tauri::command]
pub async fn wb_key_list(state: tauri::State<'_, DbState>) -> Result<Vec<WbKeyDto>, String> {
    with_db(&state, list_keys).await
}

#[tauri::command]
pub async fn wb_key_set_enabled(
    state: tauri::State<'_, DbState>,
    id: i64,
    enabled: bool,
) -> Result<(), String> {
    with_db(&state, move |conn| set_enabled(conn, id, enabled)).await
}

#[tauri::command]
pub async fn wb_key_delete(state: tauri::State<'_, DbState>, id: i64) -> Result<(), String> {
    with_db(&state, move |conn| delete_key(conn, id)).await
}

#[tauri::command]
pub async fn wb_key_reset(state: tauri::State<'_, DbState>, id: i64) -> Result<String, String> {
    with_db(&state, move |conn| reset_key(conn, id, now_ms())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::codebuddy::ensure_schema(&c).unwrap();
        c
    }

    #[test]
    fn generated_keys_have_prefix_and_length() {
        let key = generate_key();
        assert!(key.starts_with(KEY_PREFIX));
        assert_eq!(key.len(), KEY_PREFIX.len() + KEY_BODY_BYTES * 2);
        assert_ne!(generate_key(), generate_key());
    }

    #[test]
    fn mask_hides_the_middle() {
        let key = generate_key();
        let masked = mask_key(&key);
        let body = &key[KEY_PREFIX.len()..];
        assert!(masked.starts_with("wk-"));
        assert!(masked.contains('…'));
        assert!(masked.ends_with(&body[body.len() - 4..]));
        assert!(!masked.contains(&body[4..body.len() - 4]), "middle must not leak");
    }

    #[test]
    fn mask_of_short_key_is_fully_redacted() {
        assert_eq!(mask_key("wk-short"), "wk-****");
    }

    #[test]
    fn create_list_toggle_delete_round_trip() {
        let c = conn();
        let created = create_key(&c, "CI 用", 100).unwrap();
        assert_eq!(created.label, "CI 用");
        let listed = list_keys(&c).unwrap();
        assert_eq!(listed.len(), 1);
        assert_ne!(listed[0].key_masked, created.key);
        assert!(listed[0].enabled);

        set_enabled(&c, created.id, false).unwrap();
        assert!(!list_keys(&c).unwrap()[0].enabled);
        assert_eq!(enabled_count(&c).unwrap(), 0);

        delete_key(&c, created.id).unwrap();
        assert!(list_keys(&c).unwrap().is_empty());
    }

    #[test]
    fn create_rejects_blank_label() {
        let c = conn();
        assert!(create_key(&c, "   ", 1).is_err());
    }

    #[test]
    fn authenticate_needs_the_exact_key_and_enabled() {
        let c = conn();
        let created = create_key(&c, "k", 10).unwrap();
        assert_eq!(authenticate(&c, &created.key, 20), Some(created.id));
        assert_eq!(authenticate(&c, "wk-wrong", 20), None);
        let row: (Option<i64>, i64) = c
            .query_row(
                "SELECT last_used_at, call_count FROM wb_api_keys WHERE id = ?1",
                params![created.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (Some(20), 1));

        set_enabled(&c, created.id, false).unwrap();
        assert_eq!(authenticate(&c, &created.key, 30), None);
    }

    #[test]
    fn reset_swaps_the_secret_keeps_identity() {
        let c = conn();
        let created = create_key(&c, "k", 10).unwrap();
        authenticate(&c, &created.key, 11);
        let fresh = reset_key(&c, created.id, 99).unwrap();
        assert_ne!(fresh, created.key);
        assert_eq!(authenticate(&c, &created.key, 100), None);
        assert_eq!(authenticate(&c, &fresh, 100), Some(created.id));
        let row: (i64, Option<i64>, i64) = c
            .query_row(
                "SELECT created_at, rotated_at, call_count FROM wb_api_keys WHERE id = ?1",
                params![created.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, 10, "created_at must survive a reset");
        assert_eq!(row.1, Some(99), "reset is stamped in rotated_at");
        assert_eq!(row.2, 2, "reset must not reset the usage counter");
    }

    #[test]
    fn reset_unknown_id_errors() {
        let c = conn();
        assert!(reset_key(&c, 404, 1).is_err());
    }
}
