//! WeCom 出站与诊断文本的脱敏工具。
//!
//! - `redact_credentials`：**用户可见回复**的凭证遮盖（最后一道出站防线）；
//! - `redact_diagnostic`：写日志/诊断前的投影（含 URL query、路径）；
//! - `is_sensitive_file_path`：禁止作为附件外发的凭证类文件/目录。
//!
//! 背景：DSH/天玑 的模型可以读本机文件（只读沙箱只限制写，不限制读），
//! 被诱导时会把 `~/.dsh/.credentials.yaml` 之类的明文密钥复述到回复里。
//! 提示词只能降低概率，出站遮盖才是兜底。

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

/// 用户可见回复的凭证遮盖（**不动**路径与普通文本）。
///
/// 覆盖模型最常"手滑"复述的形态：
/// - `XXX_API_KEY = sk-…` / `token: …` / `credential = …` 这类键值；
/// - `sk-` / `ak-` / `pt-` / `ghp_` / JWT / AWS `AKIA…` 前缀密钥；
/// - `Authorization:` 头、`Bearer …`、PEM 私钥块。
///
/// 故意不遮盖裸的 32 位十六进制：排障回复里的 traceId/md5 与密钥形状相同，
/// 误伤会让答案失去可用性。
pub fn redact_credentials(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            // PEM 私钥整块
            (
                Regex::new(r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----")
                    .expect("pem re"),
                "[REDACTED:private-key]",
            ),
            // 鉴权头
            (
                Regex::new(r"(?im)^\s*(authorization|proxy-authorization|x-api-key)\s*[:=]\s*\S.*$")
                    .expect("auth header re"),
                "$1: [REDACTED]",
            ),
            // Bearer
            (
                Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}").expect("bearer re"),
                "Bearer [REDACTED]",
            ),
            // 键值型密钥（保留键名，只遮值）
            (
                Regex::new(
                    r#"(?i)([A-Za-z0-9_.-]*(?:api[_-]?key|apikey|secret|token|password|passwd|credential|private[_-]?key|access[_-]?key)[A-Za-z0-9_.-]*)\s*["']?\s*[:=]\s*(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s,;&}\]\[]+)"#,
                )
                .expect("credential field re"),
                "$1=[REDACTED]",
            ),
            // 常见前缀密钥（sk-or-v1-… / ak-… / pt-… / GitHub / HuggingFace / slack）
            (
                Regex::new(r"(?i)\b(?:sk|ak|pt|gh[pousr]|hf|glpat|npm|xox[abprs])[-_][A-Za-z0-9_-]{8,}")
                    .expect("prefixed key re"),
                "[REDACTED]",
            ),
            // AWS access key id
            (
                Regex::new(r"\bAKIA[0-9A-Z]{12,}\b").expect("aws key re"),
                "[REDACTED]",
            ),
            // JWT
            (
                Regex::new(r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}")
                    .expect("jwt re"),
                "[REDACTED]",
            ),
        ]
    });
    let mut out = text.to_string();
    for (pattern, replacement) in patterns {
        out = pattern.replace_all(&out, *replacement).into_owned();
    }
    out
}

/// 禁止作为附件外发的凭证类文件/目录（`WECOM_ATTACH` 与 `wecom_send_file` 共用）。
///
/// 附件链路会把文本文件**原样贴进对话**，绕过终稿脱敏，所以必须在入口拦截。
pub fn is_sensitive_file_path(path: &str) -> bool {
    let lower = path.trim().to_ascii_lowercase().replace('/', "\\");
    if lower.is_empty() {
        return false;
    }
    let name = lower.rsplit('\\').next().unwrap_or("");
    if name.is_empty() {
        return false;
    }
    const SENSITIVE_NAMES: &[&str] = &[
        ".env",
        ".netrc",
        ".pgpass",
        ".npmrc",
        ".pypirc",
        ".git-credentials",
        ".credentials",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
        "credentials",
    ];
    if SENSITIVE_NAMES
        .iter()
        .any(|blocked| name == *blocked || name.starts_with(&format!("{blocked}.")))
    {
        return true;
    }
    if name.starts_with("id_rsa") || name.starts_with("id_ed25519") {
        return true;
    }
    const SENSITIVE_EXTS: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".kdbx", ".ppk"];
    if SENSITIVE_EXTS.iter().any(|ext| name.ends_with(ext)) {
        return true;
    }
    // 常见凭证目录：DSH 自身凭据、SSH、云厂商、我们的应用数据（含密钥库/会话旁证）。
    const SENSITIVE_DIRS: &[&str] = &[
        "\\.dsh\\",
        "\\.ssh\\",
        "\\.aws\\",
        "\\.azure\\",
        "\\.kube\\",
        "\\.gnupg\\",
        "\\com.ai-workbench.app\\",
    ];
    SENSITIVE_DIRS.iter().any(|dir| lower.contains(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_reply_credentials_are_redacted() {
        let text = "OPENROUTER_API_KEY = sk-or-v1-abcdef1234567890\n\
SUB2API_API_KEY = sk-8cb8cc94aa11bb22\n\
LONGCAT_API_KEY = ak-2v375bb88cc33dd44\n\
QODER_MANAGED_CREDENTIAL = pt-9f8e7d6c5b4a\n\
token = 25sh95hu8AZsImI7htVqoUhaPUZ78e9xb5MzPo5k7AaNpKVdux\n\
Authorization: Bearer header-secret-value\n\
eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U\n\
-----BEGIN OPENSSH PRIVATE KEY-----\nMIIEsecret\n-----END OPENSSH PRIVATE KEY-----";
        let out = redact_credentials(text);
        for secret in [
            "sk-or-v1-abcdef1234567890",
            "sk-8cb8cc94aa11bb22",
            "ak-2v375bb88cc33dd44",
            "pt-9f8e7d6c5b4a",
            "25sh95hu8AZs",
            "header-secret-value",
            "eyJhbGciOiJIUzI1NiJ9",
            "MIIEsecret",
        ] {
            assert!(!out.contains(secret), "retained {secret} in {out}");
        }
        // 键名保留，便于模型/用户知道哪里被遮。
        assert!(out.contains("OPENROUTER_API_KEY"), "{out}");
        assert!(out.contains("[REDACTED"), "{out}");
    }

    #[test]
    fn visible_reply_keeps_troubleshooting_identifiers() {
        let text = "结论：requestId=wecom-123 matchId=1454805448406827008 traceId=4bf92f3577b34da6a3ce929d0e0e4736 [L1] 服务正常";
        assert_eq!(redact_credentials(text), text);
    }

    #[test]
    fn sensitive_attachment_paths_are_blocked() {
        for path in [
            r"C:\Users\u\.dsh\.credentials.yaml",
            r"C:\Users\u\.ssh\id_rsa",
            r"D:\proj\.env.local",
            r"D:\proj\server.pem",
            r"D:\proj\client.key",
            r"C:\Users\u\AppData\Roaming\com.ai-workbench.app\ai-workbench.db",
        ] {
            assert!(is_sensitive_file_path(path), "{path}");
        }
        for path in [
            r"D:\proj\doc\report.md",
            r"D:\proj\src\main.rs",
            r"D:\proj\build\setup.exe",
            r"D:\proj\docs\token-usage.md",
        ] {
            assert!(!is_sensitive_file_path(path), "{path}");
        }
    }

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
