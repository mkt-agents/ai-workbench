//! Outbound WeCom text sanitizer: strip local absolute paths from replies.

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
    let lower_hay = hay.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let mut out = String::with_capacity(hay.len());
    let mut i = 0;
    while let Some(pos) = lower_hay[i..].find(&lower_needle) {
        let abs = i + pos;
        out.push_str(&hay[i..abs]);
        out.push_str(rep);
        i = abs + needle.len();
    }
    out.push_str(&hay[i..]);
    out
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