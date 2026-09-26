//! Tauri commands for the WorkBuddy Manager account pool and settings.
//!
//! Credentials never leave this module in full: every DTO carries only a
//! preview. The pool table deliberately stays out of the db_load/db_save
//! whitelist so an accidental whole-table write from the frontend cannot
//! clobber stored credentials — all mutation goes through these commands.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::codebuddy::adapter::{self, RenderContext, UpstreamProfile};
use crate::codebuddy::credential::{self, CredentialKind, ParsedCredential, WbAccountDto};
use crate::DbState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WbSettings {
    pub port: i64,
    pub lan_enabled: bool,
    pub checkin_enabled: bool,
    pub checkin_time: String,
    pub last_auto_checkin_date: Option<String>,
    pub adapter_json: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WbSettingsPatch {
    pub port: Option<i64>,
    pub lan_enabled: Option<bool>,
    pub checkin_enabled: Option<bool>,
    pub checkin_time: Option<String>,
    pub last_auto_checkin_date: Option<String>,
    pub adapter_json: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddAccountInput {
    pub label: String,
    /// "token" | "cookie"
    pub credential_type: String,
    pub credential_raw: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPatch {
    pub label: Option<String>,
    pub notes: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub ok: bool,
    pub exp_unix: Option<i64>,
    pub status: String,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Storage layer (&Connection in, data out) — testable against in-memory sqlite.
// ---------------------------------------------------------------------------

fn row_to_dto(row: &rusqlite::Row) -> rusqlite::Result<WbAccountDto> {
    let kind = CredentialKind::parse(&row.get::<_, String>(2)?);
    let raw: String = row.get(3)?;
    let parsed = ParsedCredential::parse(kind, &raw);
    Ok(WbAccountDto {
        id: row.get(0)?,
        label: row.get(1)?,
        credential_type: kind.as_str().to_string(),
        credential_preview: parsed.preview(),
        email: row.get(4)?,
        exp_unix: row.get(5)?,
        status: row.get(6)?,
        enabled: row.get::<_, i64>(7)? != 0,
        last_checkin_at: row.get(8)?,
        last_checkin_status: row.get(9)?,
        checkin_fail_count: row.get(10)?,
        notes: row.get(11)?,
        created_at: row.get(12)?,
    })
}

const LIST_SQL: &str = r#"
    SELECT id, label, credential_type, credential, email, exp_unix, status, enabled,
           last_checkin_at, last_checkin_status, checkin_fail_count, notes, created_at
      FROM codebuddy_accounts
     ORDER BY id DESC"#;

pub fn list_accounts(conn: &Connection) -> Result<Vec<WbAccountDto>, String> {
    let mut stmt = conn.prepare(LIST_SQL).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], row_to_dto)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string());
    rows
}

pub fn insert_account(conn: &Connection, input: &AddAccountInput, now_ms: i64) -> Result<i64, String> {
    let kind = CredentialKind::parse(&input.credential_type);
    let raw = input.credential_raw.trim();
    if raw.is_empty() {
        return Err("凭证不能为空".to_string());
    }
    if input.label.trim().is_empty() {
        return Err("名称不能为空".to_string());
    }
    let parsed = ParsedCredential::parse(kind, raw);
    conn.execute(
        "INSERT INTO codebuddy_accounts
            (label, credential_type, credential, email, exp_unix, status, enabled, notes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'unverified', 1, ?6, ?7)",
        params![
            input.label.trim(),
            kind.as_str(),
            raw,
            input.email,
            parsed.exp_unix,
            input.notes,
            now_ms
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

pub fn update_account(conn: &Connection, id: i64, patch: &AccountPatch) -> Result<(), String> {
    let mut sets: Vec<String> = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(label) = &patch.label {
        sets.push(format!("label = ?{}", values.len() + 1));
        values.push(Box::new(label.clone()));
    }
    if let Some(notes) = &patch.notes {
        sets.push(format!("notes = ?{}", values.len() + 1));
        values.push(Box::new(notes.clone()));
    }
    if let Some(enabled) = patch.enabled {
        sets.push(format!("enabled = ?{}", values.len() + 1));
        values.push(Box::new(i64::from(enabled)));
    }
    if sets.is_empty() {
        return Ok(());
    }
    sets.push(format!("updated_at = ?{}", values.len() + 1));
    values.push(Box::new(crate::codebuddy::credential::now_unix() * 1000));
    values.push(Box::new(id));
    let sql = format!(
        "UPDATE codebuddy_accounts SET {} WHERE id = ?",
        sets.join(", ")
    );
    let refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
    conn.execute(&sql, refs.as_slice()).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_account(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM codebuddy_accounts WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Apply a probe outcome to the row: status + parsed exp + failure counter.
pub fn apply_probe_result(
    conn: &Connection,
    id: i64,
    status: &str,
    exp_unix: Option<i64>,
) -> Result<(), String> {
    conn.execute(
        "UPDATE codebuddy_accounts
            SET status = ?2, exp_unix = COALESCE(?3, exp_unix), updated_at = ?4
          WHERE id = ?1",
        params![id, status, exp_unix, credential::now_unix() * 1000],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_settings(conn: &Connection) -> Result<WbSettings, String> {
    conn.query_row(
        "SELECT port, lan_enabled, checkin_enabled, checkin_time, last_auto_checkin_date, adapter_json
           FROM wb_settings WHERE id = 1",
        [],
        |row| {
            Ok(WbSettings {
                port: row.get(0)?,
                lan_enabled: row.get::<_, i64>(1)? != 0,
                checkin_enabled: row.get::<_, i64>(2)? != 0,
                checkin_time: row.get(3)?,
                last_auto_checkin_date: row.get(4)?,
                adapter_json: row.get(5)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

pub fn update_settings(conn: &Connection, patch: &WbSettingsPatch) -> Result<WbSettings, String> {
    conn.execute(
        "UPDATE wb_settings SET
            port = COALESCE(?1, port),
            lan_enabled = COALESCE(?2, lan_enabled),
            checkin_enabled = COALESCE(?3, checkin_enabled),
            checkin_time = COALESCE(?4, checkin_time),
            last_auto_checkin_date = COALESCE(?5, last_auto_checkin_date),
            adapter_json = COALESCE(?6, adapter_json)
         WHERE id = 1",
        params![
            patch.port,
            patch.lan_enabled.map(i64::from),
            patch.checkin_enabled.map(i64::from),
            patch.checkin_time,
            patch.last_auto_checkin_date,
            patch.adapter_json
        ],
    )
    .map_err(|e| e.to_string())?;
    get_settings(conn)
}

/// The stored protocol, or None when it is absent/broken (callers surface the
/// parse error where the user can fix it, otherwise degrade to unverified).
pub fn load_profile(conn: &Connection) -> Result<Option<UpstreamProfile>, String> {
    match get_settings(conn)?.adapter_json {
        None => Ok(None),
        Some(json) if json.trim().is_empty() => Ok(None),
        Some(json) => UpstreamProfile::from_json(&json).map(Some),
    }
}

fn load_credential(conn: &Connection, id: i64) -> Result<ParsedCredential, String> {
    let (kind, raw): (String, String) = conn
        .query_row(
            "SELECT credential_type, credential FROM codebuddy_accounts WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| format!("账号 {id} 不存在"))?;
    Ok(ParsedCredential::parse(CredentialKind::parse(&kind), &raw))
}

/// Probe an account with the configured protocol. Pure part: decide what a
/// profile+credential implies before any HTTP happens.
pub fn probe_plan(
    profile: Option<&UpstreamProfile>,
    parsed: &ParsedCredential,
) -> Result<adapter::OutboundRequest, String> {
    let profile = profile.ok_or_else(|| "未配置协议（adapter_json 为空），无法验证".to_string())?;
    let ep = profile
        .credential_probe
        .as_ref()
        .ok_or_else(|| "协议缺少 credentialProbe 端点".to_string())?;
    let cx = RenderContext {
        credential: &parsed.raw,
        token: &parsed.token,
        cookie: &parsed.cookie,
        model: "",
    };
    if let Some(hint) = missing_bearer_hint(ep, parsed) {
        return Err(hint);
    }
    adapter::build_request(ep, &cx).ok_or_else(|| "credentialProbe 的 method 为空".to_string())
}

/// 端点模板要 `{token}`，可这条凭证是 Cookie 串 —— 探测必然 401，直接判给原因。
///
/// `token_in_cookie` 会从 Cookie 串里挑一个"名字像 token/session"的值当 Bearer，所以
/// 渲染结果非空并不代表真有令牌：CodeBuddy 的会话 Cookie 全是不可重放的那种。
fn missing_bearer_hint(ep: &adapter::Endpoint, parsed: &ParsedCredential) -> Option<String> {
    let wants_token = ep.url.contains("{token}")
        || ep
            .headers
            .iter()
            .any(|(_, v)| v.contains("{token}") || v.contains("{credential}"));
    if !wants_token || parsed.kind == CredentialKind::Token {
        return None;
    }
    Some(
        "credentialProbe 要 Bearer 令牌，但这条凭证是 Cookie 串（实测不可重放）：\
         请重新扫码——捕获脚本会从登录回包里抓到真正的登录 JWT（token 类型）"
            .to_string(),
    )
}

/// 探测响应 → (status, reason)。
///
/// 401/403 不再直接等于「已过期」：探测端点是我等用户手填的猜测值，未到期凭证被
/// 它拒了，更可能是端点或凭证类型不匹配——那种情况标 unverified 并把 HTTP 码留在
/// reason 里，别把账号写成"过期"误导排查。
fn probe_outcome(code: u16, self_expired: bool) -> (&'static str, Option<String>) {
    if (200..300).contains(&code) {
        return ("active", None);
    }
    if matches!(code, 401 | 403) {
        return if self_expired {
            ("expired", Some(format!("上游返回 HTTP {code}，且凭证已到期")))
        } else {
            (
                "unverified",
                Some(format!("上游返回 HTTP {code}：凭证本身未到期，更可能是探测端点或凭证类型不匹配")),
            )
        };
    }
    ("unverified", Some(format!("上游返回 HTTP {code}")))
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn wb_list_accounts(state: tauri::State<'_, DbState>) -> Result<Vec<WbAccountDto>, String> {
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        list_accounts(&guard)
    })
    .await
    .map_err(|e| format!("读取失败: {}", e))?
}

#[tauri::command]
pub async fn wb_add_account_manual(
    state: tauri::State<'_, DbState>,
    input: AddAccountInput,
) -> Result<i64, String> {
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        insert_account(&guard, &input, credential::now_unix() * 1000)
    })
    .await
    .map_err(|e| format!("添加失败: {}", e))?
}

#[tauri::command]
pub async fn wb_update_account(
    state: tauri::State<'_, DbState>,
    id: i64,
    patch: AccountPatch,
) -> Result<(), String> {
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        update_account(&guard, id, &patch)
    })
    .await
    .map_err(|e| format!("更新失败: {}", e))?
}

#[tauri::command]
pub async fn wb_delete_account(state: tauri::State<'_, DbState>, id: i64) -> Result<(), String> {
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        delete_account(&guard, id)
    })
    .await
    .map_err(|e| format!("删除失败: {}", e))?
}

/// Verify a stored credential against the configured probe endpoint, updating
/// the row status. HTTP runs on the async side so the connection lock is only
/// held for the two short reads/writes around it.
#[tauri::command]
pub async fn wb_probe_account(state: tauri::State<'_, DbState>, id: i64) -> Result<ProbeResult, String> {
    let conn = std::sync::Arc::clone(&state.conn);
    // Everything the async half needs is owned (cloned out of the guard): a
    // borrowed MutexGuard would drag the lock across the await and make the
    // command future non-Send.
    enum Plan {
        Ready(adapter::OutboundRequest, Option<i64>),
        NoProbe { reason: String, expired: bool },
    }
    let plan = {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        let parsed = load_credential(&guard, id)?;
        let profile = load_profile(&guard).unwrap_or(None);
        match probe_plan(profile.as_ref(), &parsed) {
            Ok(req) => Plan::Ready(req, parsed.exp_unix),
            Err(reason) => Plan::NoProbe { reason, expired: parsed.is_expired(credential::now_unix()) },
        }
    };
    let (request, exp_from_credential) = match plan {
        Plan::Ready(req, exp) => (req, exp),
        Plan::NoProbe { reason, expired } => {
            // Cannot probe (protocol not configured yet) — record why without
            // flipping an otherwise-plausible account to expired.
            let status = if expired { "expired" } else { "unverified" };
            return Ok(ProbeResult { ok: false, exp_unix: None, status: status.to_string(), reason: Some(reason) });
        }
    };

    let response = execute_outbound(&request).await;
    let (status, reason, exp_unix) = match response {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            let exp = body_exp_unix(&body).or(exp_from_credential);
            ("active".to_string(), None, exp)
        }
        Ok(resp) => {
            let code = resp.status().as_u16();
            let self_expired = exp_from_credential.is_some_and(|exp| exp <= credential::now_unix());
            let (status, reason) = probe_outcome(code, self_expired);
            (status.to_string(), reason, exp_from_credential)
        }
        Err(e) => ("unverified".to_string(), Some(format!("请求失败: {e}")), exp_from_credential),
    };
    let exp = exp_unix;

    let conn2 = std::sync::Arc::clone(&conn);
    let status_for_write = status.clone();
    tokio::task::spawn_blocking(move || {
        let guard = conn2.lock().map_err(|e| e.to_string())?;
        apply_probe_result(&guard, id, &status_for_write, exp)
    })
    .await
    .map_err(|e| format!("写入失败: {}", e))??;

    Ok(ProbeResult { ok: status == "active", exp_unix: exp, status, reason })
}

/// Best-effort: some probe endpoints echo the token expiry in their JSON body.
fn body_exp_unix(body: &str) -> Option<i64> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    const PATHS: [(&str, Option<&str>); 4] = [
        ("data", Some("expireTime")),
        ("data", Some("exp")),
        ("expireTime", None),
        ("exp", None),
    ];
    for (first, second) in PATHS {
        let found = json
            .get(first)
            .and_then(|v| match second {
                Some(key) => v.get(key),
                None => Some(v),
            })
            .and_then(|v| v.as_i64());
        if let Some(n) = found {
            // seconds vs millis
            return Some(if n > 1_000_000_000_000 { n / 1000 } else { n });
        }
    }
    None
}

pub(crate) async fn execute_outbound(
    request: &adapter::OutboundRequest,
) -> Result<reqwest::Response, reqwest::Error> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .danger_accept_invalid_certs(true)
        .build()?;
    let mut req = client
        .request(
            request
                .method
                .parse::<reqwest::Method>()
                .unwrap_or(reqwest::Method::GET),
            &request.url,
        )
        .headers(
            request
                .headers
                .iter()
                .filter_map(|(k, v)| {
                    Some((k.parse().ok()?, v.parse().ok()?))
                })
                .collect::<reqwest::header::HeaderMap>(),
        );
    if let Some(body) = &request.body {
        req = req.header("content-type", "application/json").body(body.clone());
    }
    req.send().await
}

#[tauri::command]
pub async fn wb_get_settings(state: tauri::State<'_, DbState>) -> Result<WbSettings, String> {
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        get_settings(&guard)
    })
    .await
    .map_err(|e| format!("读取失败: {}", e))?
}

#[tauri::command]
pub async fn wb_update_settings(
    state: tauri::State<'_, DbState>,
    patch: WbSettingsPatch,
) -> Result<WbSettings, String> {
    if let Some(json) = patch.adapter_json.as_deref() {
        if !json.trim().is_empty() {
            // Validate before persisting so the gateway never reads a broken profile.
            UpstreamProfile::from_json(json)?;
        }
    }
    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        update_settings(&guard, &patch)
    })
    .await
    .map_err(|e| format!("保存失败: {}", e))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codebuddy::adapter::Endpoint;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::codebuddy::ensure_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn probe_2xx_is_active() {
        assert_eq!(probe_outcome(200, false), ("active", None));
        assert_eq!(probe_outcome(204, true), ("active", None));
    }

    #[test]
    fn a_401_alone_is_not_proof_of_expiry() {
        let (status, reason) = probe_outcome(401, false);
        assert_eq!(status, "unverified");
        assert!(reason.unwrap().contains("凭证类型不匹配"));
        assert_eq!(probe_outcome(403, false).0, "unverified");
    }

    #[test]
    fn a_401_on_an_expired_credential_is_expired() {
        let (status, reason) = probe_outcome(401, true);
        assert_eq!(status, "expired");
        assert!(reason.unwrap().contains("已到期"));
    }

    #[test]
    fn other_codes_stay_unverified_with_the_code() {
        for code in [404u16, 405, 500, 503] {
            let (status, reason) = probe_outcome(code, false);
            assert_eq!(status, "unverified");
            assert_eq!(reason, Some(format!("上游返回 HTTP {code}")));
        }
    }

    fn input(credential: &str) -> AddAccountInput {
        AddAccountInput {
            label: "小号 A".into(),
            credential_type: "token".into(),
            credential_raw: credential.into(),
            email: None,
            notes: None,
        }
    }

    #[test]
    fn schema_seeds_settings_row() {
        let conn = mem_conn();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s.port, 8787);
        assert!(!s.lan_enabled);
        assert_eq!(s.checkin_time, "09:30");
        assert!(s.adapter_json.is_none());
    }

    #[test]
    fn settings_patch_only_touches_given_fields() {
        let conn = mem_conn();
        update_settings(
            &conn,
            &WbSettingsPatch { port: Some(9000), ..Default::default() },
        )
        .unwrap();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s.port, 9000);
        assert_eq!(s.checkin_time, "09:30");
    }

    #[test]
    fn insert_stores_credential_but_list_only_previews() {
        let conn = mem_conn();
        let id = insert_account(&conn, &input("supersecret-token-value-123456"), 1).unwrap();
        let rows = list_accounts(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].status, "unverified");
        assert!(!rows[0].credential_preview.contains("secret-token"));
    }

    #[test]
    fn insert_rejects_empty_label_or_credential() {
        let conn = mem_conn();
        let bad = AddAccountInput { label: "  ".into(), ..input("tok") };
        assert!(insert_account(&conn, &bad, 1).is_err());
        let bad2 = AddAccountInput { credential_raw: "".into(), ..input("tok") };
        assert!(insert_account(&conn, &bad2, 1).is_err());
    }

    #[test]
    fn patch_round_trip_and_delete() {
        let conn = mem_conn();
        let id = insert_account(&conn, &input("t"), 1).unwrap();
        update_account(
            &conn,
            id,
            &AccountPatch { label: Some("改名".into()), enabled: Some(false), ..Default::default() },
        )
        .unwrap();
        let row = &list_accounts(&conn).unwrap()[0];
        assert_eq!(row.label, "改名");
        assert!(!row.enabled);
        delete_account(&conn, id).unwrap();
        assert!(list_accounts(&conn).unwrap().is_empty());
    }

    #[test]
    fn probe_plan_requires_profile_and_endpoint() {
        let parsed = ParsedCredential::parse(CredentialKind::Token, "tok");
        assert!(probe_plan(None, &parsed).unwrap_err().contains("未配置协议"));
        let empty = UpstreamProfile {
            login_url: String::new(),
            credential_probe: None,
            checkin: None,
            chat: None,
            models: None,
            ..Default::default()
        };
        assert!(probe_plan(Some(&empty), &parsed).unwrap_err().contains("credentialProbe"));
    }

    #[test]
    fn probe_plan_injects_credential_into_headers() {
        let parsed = ParsedCredential::parse(CredentialKind::Token, "TOK");
        let profile = UpstreamProfile {
            login_url: String::new(),
            credential_probe: Some(Endpoint {
                method: "GET".into(),
                url: "https://x/user?c={credential}".into(),
                headers: vec![("authorization".into(), "Bearer {token}".into())],
                body: None,
            }),
            checkin: None,
            chat: None,
            models: None,
            ..Default::default()
        };
        let req = probe_plan(Some(&profile), &parsed).unwrap();
        assert_eq!(req.url, "https://x/user?c=TOK");
        assert_eq!(req.headers[0].1, "Bearer TOK");
        assert_eq!(req.method, "GET");
    }

    #[test]
    fn a_cookie_credential_against_a_bearer_probe_fails_with_the_real_reason() {
        // 用户实测踩到的坑：Cookie 串入库 + 探测端点要 Bearer ⇒ 上游只回一个没头没尾的
        // 401，排查方向全歪。发请求之前就该说"重新扫码抓 JWT"。
        let cookie = ParsedCredential::parse(
            CredentialKind::Cookie,
            "KEYCLOAK_SESSION=\"copilot/a/b\"; session=x|y|z",
        );
        let profile = UpstreamProfile {
            login_url: String::new(),
            credential_probe: Some(Endpoint {
                method: "GET".into(),
                url: "https://api.test/plan/v3/models".into(),
                headers: vec![("authorization".into(), "Bearer {token}".into())],
                body: None,
            }),
            checkin: None,
            chat: None,
            models: None,
            ..Default::default()
        };
        let err = probe_plan(Some(&profile), &cookie).unwrap_err();
        assert!(err.contains("重新扫码"), "{err}");

        // 同一份协议配 Cookie 端点、或凭证里真有令牌时，不该误报。
        let cookie_probe = UpstreamProfile {
            credential_probe: Some(Endpoint {
                method: "GET".into(),
                url: "https://api.test/me".into(),
                headers: vec![("cookie".into(), "{cookie}".into())],
                body: None,
            }),
            ..profile.clone()
        };
        assert!(probe_plan(Some(&cookie_probe), &cookie).is_ok());
        let bearer = ParsedCredential::parse(CredentialKind::Token, "sk-whatever");
        assert!(probe_plan(Some(&profile), &bearer).is_ok());
    }

    #[test]
    fn body_exp_reads_nested_and_flat_with_millis_normalisation() {
        assert_eq!(body_exp_unix(r#"{"data":{"expireTime":1800000000000}}"#), Some(1_800_000_000));
        assert_eq!(body_exp_unix(r#"{"exp":1800000000}"#), Some(1_800_000_000));
        assert_eq!(body_exp_unix("not json"), None);
    }
}
