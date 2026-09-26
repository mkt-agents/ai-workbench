//! Credential parsing for pooled CodeBuddy accounts. A credential is either a
//! bare token (possibly a JWT whose `exp` we can read) or a raw Cookie header
//! string. Pure functions only — storage and probing live in `wb_commands`.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Token,
    Cookie,
}

impl CredentialKind {
    /// Accepts the DB / IPC spelling; anything unknown falls back to Token so
    /// a malformed row still renders instead of vanishing from the pool.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "cookie" => Self::Cookie,
            _ => Self::Token,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Token => "token",
            Self::Cookie => "cookie",
        }
    }
}

/// What a stored credential string decodes to. `token` for a Cookie blob is
/// the best bearer-looking candidate found inside it (see `token_in_cookie`).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCredential {
    pub kind: CredentialKind,
    pub raw: String,
    pub token: String,
    pub cookie: String,
    pub exp_unix: Option<i64>,
}

impl ParsedCredential {
    pub fn parse(kind: CredentialKind, raw: &str) -> Self {
        let raw = raw.trim().to_string();
        let (token, cookie, exp_unix) = match kind {
            CredentialKind::Token => {
                let exp = jwt_exp_unix(&raw);
                (raw.clone(), String::new(), exp)
            }
            CredentialKind::Cookie => {
                let token = token_in_cookie(&raw).unwrap_or_default();
                let exp = if token.is_empty() { None } else { jwt_exp_unix(&token) };
                (token, raw.clone(), exp)
            }
        };
        Self { kind, raw, token, cookie, exp_unix }
    }

    pub fn is_expired(&self, now_unix: i64) -> bool {
        self.exp_unix.is_some_and(|exp| exp <= now_unix)
    }

    /// Shown in the account table — the pool stores credentials in plaintext
    /// (same convention as cursor_accounts / ai_models), but the UI never
    /// needs to echo them back in full.
    pub fn preview(&self) -> String {
        let compact: String = self.raw.chars().filter(|c| !c.is_whitespace()).collect();
        let chars: Vec<char> = compact.chars().collect();
        if chars.len() <= 14 {
            return chars.iter().map(|_| "•").collect();
        }
        let head: String = chars[..6].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    }
}

/// JWT `exp` claim (unix seconds) from a `header.payload.sig` token.
/// Same decoding path as cursor's `jwt_exp_unix`, generalised to any
/// base64url payload with a numeric exp.
pub fn jwt_exp_unix(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let padded = match payload.len() % 4 {
        0 => payload.to_string(),
        n => format!("{}{}", payload, "=".repeat(4 - n)),
    };
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(padded.as_bytes())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.as_bytes()))
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("exp").and_then(|v| v.as_i64())
}

/// Pick the most likely bearer token out of a Cookie header string: prefer a
/// cookie whose name smells like a token/session and whose value parses as a
/// JWT; otherwise any JWT-shaped value; otherwise any token-ish value.
pub fn token_in_cookie(cookie: &str) -> Option<String> {
    let mut jwt_fallback: Option<String> = None;
    for part in cookie.split(';') {
        // Skip malformed segments instead of giving up on the whole cookie.
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let name_l = name.trim().to_ascii_lowercase();
        let looks_named = name_l.contains("token") || name_l.contains("jwt") || name_l.contains("session");
        if looks_named {
            if jwt_exp_unix(value).is_some() {
                return Some(value.to_string());
            }
            jwt_fallback.get_or_insert_with(|| value.to_string());
            continue;
        }
        if jwt_fallback.is_none() && jwt_exp_unix(value).is_some() {
            jwt_fallback = Some(value.to_string());
        }
    }
    jwt_fallback
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Row shape sent to the frontend; carries no full credential.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WbAccountDto {
    pub id: i64,
    pub label: String,
    pub email: Option<String>,
    pub credential_type: String,
    pub credential_preview: String,
    pub exp_unix: Option<i64>,
    pub status: String,
    pub enabled: bool,
    pub last_checkin_at: Option<i64>,
    pub last_checkin_status: Option<String>,
    pub checkin_fail_count: i64,
    pub notes: Option<String>,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jwt(exp: i64) -> String {
        use base64::Engine;
        let enc = |s: &str| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(s.as_bytes())
                .trim_end_matches('=')
                .to_string()
        };
        format!(
            "{}.{}.sig",
            enc(r#"{"alg":"HS256"}"#),
            enc(&format!(r#"{{"exp":{exp}}}"#))
        )
    }

    #[test]
    fn token_credential_exposes_jwt_exp() {
        let pc = ParsedCredential::parse(CredentialKind::Token, &fake_jwt(1_800_000_000));
        assert_eq!(pc.exp_unix, Some(1_800_000_000));
        assert!(!pc.is_expired(1_700_000_000));
        assert!(pc.is_expired(1_900_000_000));
    }

    #[test]
    fn opaque_token_has_no_exp() {
        let pc = ParsedCredential::parse(CredentialKind::Token, "sk-abc123");
        assert_eq!(pc.exp_unix, None);
        assert!(!pc.is_expired(i64::MAX));
    }

    #[test]
    fn cookie_prefers_named_jwt_then_any_jwt_shaped_value() {
        let cookie = format!("theme=dark; {} ; access_token={}", fake_jwt(42), fake_jwt(7));
        let token = token_in_cookie(&cookie).unwrap();
        assert_eq!(jwt_exp_unix(&token), Some(7));

        let only_value_jwt = format!("sid= {}; x=1", fake_jwt(99));
        assert_eq!(jwt_exp_unix(&token_in_cookie(&only_value_jwt).unwrap()), Some(99));

        let no_jwt = "a=1; refreshToken=opaque-xyz";
        assert_eq!(token_in_cookie(no_jwt).as_deref(), Some("opaque-xyz"));
        assert_eq!(token_in_cookie("a=1; b=2"), None);
    }

    #[test]
    fn cookie_credential_fills_both_views() {
        let raw = format!("t=1; token={}", fake_jwt(123));
        let pc = ParsedCredential::parse(CredentialKind::Cookie, &raw);
        assert_eq!(pc.cookie, raw);
        assert_eq!(pc.exp_unix, Some(123));
        assert!(!pc.token.is_empty());
    }

    #[test]
    fn preview_never_leaks_middle() {
        let pc = ParsedCredential::parse(CredentialKind::Token, "abcdefghij1234567890");
        let p = pc.preview();
        assert!(p.starts_with("abcdef"));
        assert!(p.ends_with("7890"));
        assert!(!p.contains("ghij1234"));
        let short = ParsedCredential::parse(CredentialKind::Token, "short");
        assert!(!short.preview().contains('s') || short.preview().chars().all(|c| c == '•'));
    }
}
