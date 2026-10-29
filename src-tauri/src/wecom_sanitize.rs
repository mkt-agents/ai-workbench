//! Diagnostic-only sanitizer. User-visible WeCom replies are not redacted.

use regex::Regex;
use std::sync::OnceLock;

const REDACT: &str = "[\u{672c}\u{5730}\u{8def}\u{5f84}]"; // ben-di lu-jing

/// Replace known workspace root and Windows/UNC absolute-path heuristics.
pub fn redact_local_paths(text: &str, repo_path: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = text.to_string();
    let root = repo_path.trim().trim_end_matches(['\\', '/']);
    if !root.is_empty() {
        let variants = [
            root.to_string(),
            root.replace('\\', "/"),
            root.replace('/', "\\"),
        ];
        for v in variants {
            if v.len() >= 2 {
                out = replace_ignore_case(&out, &v, REDACT);
            }
        }
    }
    out = win_abs_path_re().replace_all(&out, REDACT).into_owned();
    out = unc_path_re().replace_all(&out, REDACT).into_owned();
    out
}

fn replace_ignore_case(hay: &str, needle: &str, rep: &str) -> String {
    if needle.is_empty() {
        return hay.to_string();
    }
    // Match on the original string: Unicode case folding can change byte lengths.
    regex::RegexBuilder::new(&regex::escape(needle))
        .case_insensitive(true)
        .build()
        .expect("escaped literal regex")
        .replace_all(hay, regex::NoExpand(rep))
        .into_owned()
}

/// Diagnostic-only projection. Call before truncating, persisting or copying text.
/// URL query/fragment values are omitted wholesale, including unknown credential names.
/// This is not a general private-message anonymizer; callers must avoid logging bodies.
pub fn redact_diagnostic(text: &str) -> String {
    static URL: OnceLock<Regex> = OnceLock::new();
    static HEADERS: OnceLock<Regex> = OnceLock::new();
    static FIELDS: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    let out = URL
        .get_or_init(|| Regex::new(r#"(?i)https?://[^\s<>\"']+"#).unwrap())
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let raw = &caps[0];
            let scheme_end = raw.find("://").unwrap() + 3;
            let end = raw.find(['?', '#']).unwrap_or(raw.len());
            let base = &raw[..end];
            let authority_end = base[scheme_end..]
                .find('/')
                .map_or(base.len(), |i| scheme_end + i);
            let mut clean = if let Some(at) = base[scheme_end..authority_end].rfind('@') {
                format!(
                    "{}[REDACTED]@{}",
                    &base[..scheme_end],
                    &base[scheme_end + at + 1..]
                )
            } else {
                base.to_string()
            };
            if end < raw.len() {
                clean.push_str("?[REDACTED]");
            }
            clean
        })
        .into_owned();
    let out = HEADERS.get_or_init(|| Regex::new(r#"(?im)\b(authorization|proxy-authorization|cookie|set-cookie|x-api-key)\s*[\"']?\s*[:=]\s*[^\r\n]+"#).unwrap())
        .replace_all(&out, "$1: [REDACTED]").into_owned();
    let out = FIELDS.get_or_init(|| Regex::new(r#"(?i)\b((?:access[_-]?|refresh[_-]?|bot[_-]?)?token|(?:api[_-]?|bot[_-]?|corp[_-]?)?secret|api[_-]?key|password|passwd|credential|key)\s*[\"']?\s*[:=]\s*(?:\"[^\"]*\"|'[^']*'|[^\s,;&}\]]+)"#).unwrap())
        .replace_all(&out, "$1=[REDACTED]").into_owned();
    let out = BEARER
        .get_or_init(|| Regex::new(r#"(?i)\bBearer\s+[^\s,;\"']+"#).unwrap())
        .replace_all(&out, "Bearer [REDACTED]")
        .into_owned();
    let out = TOKEN
        .get_or_init(|| Regex::new(r"\bsk-[A-Za-z0-9_-]+").unwrap())
        .replace_all(&out, "[REDACTED]")
        .into_owned();
    redact_local_paths(&out, "")
}

fn win_abs_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b[a-z]:[\\/]\S+").expect("win path re"))
}

fn unc_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\\\\\S+").expect("unc path re"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_case_expansion_does_not_change_original_offsets() {
        assert_eq!(
            redact_local_paths("İ D:\\work", r"D:\work"),
            format!("İ {REDACT}")
        );
        assert_eq!(
            redact_local_paths("中文 D:/WORK/src", r"d:\work"),
            format!("中文 {REDACT}/src")
        );
    }

    #[test]
    fn diagnostic_credentials_removed_before_truncation() {
        let text = "https://alice:password@example.test/path?token=fixture-secret#fragment\nAuthorization: Bearer header-secret\n{\"api_key\":\"json-secret\"}\nsecret=field-secret\nBearer bearer-secret\nsk-fixture-secret";
        let result = redact_diagnostic(text);
        for secret in [
            "alice",
            "password",
            "fixture-secret",
            "header-secret",
            "json-secret",
            "field-secret",
            "bearer-secret",
            "fragment",
        ] {
            assert!(!result.contains(secret), "retained {secret}");
        }
        assert!(result.contains("example.test/path"));
        assert_eq!(
            redact_diagnostic("requestId=wecom-123 phase=total elapsedMs=12"),
            "requestId=wecom-123 phase=total elapsedMs=12"
        );
    }

    #[test]
    fn redacts_known_root() {
        let root = r"D:\ai_project\ai-workbench";
        let s = redact_local_paths(r"open D:\ai_project\ai-workbench\src\a.ts", root);
        assert!(!s.contains("ai_project"));
        assert!(s.contains(REDACT));
        assert!(s.contains("src"));
    }

    #[test]
    fn redacts_forward_slash_variant() {
        let root = r"D:\work\repo";
        let s = redact_local_paths("path is D:/work/repo/readme.md", root);
        assert!(s.contains(REDACT));
        assert!(!s.contains("D:/work"));
    }

    #[test]
    fn redacts_other_drive_paths() {
        let s = redact_local_paths(r"see C:\Users\foo\secret.txt", "");
        assert_eq!(s, format!("see {REDACT}"));
    }
}
