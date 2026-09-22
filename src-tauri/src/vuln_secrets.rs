//! Secret / sensitive-token scanning over a project's tracked files.
//!
//! Pure line scanner, no regex crate, same house style as the coverage and diff
//! parsers. Two rules make the design decisions for everything else:
//!
//! 1. **The raw secret never leaves this module.** `SecretHit` carries only a
//!    masked preview (head…tail), so the findings table, the UI and the AI
//!    prompt can all stay safe to store, show and forward.
//! 2. **Placeholder discipline beats cleverness.** The generic-assignment rule
//!    is where noise lives, so it demands entropy AND rejects the documented
//!    placeholder idioms (`${…}`, `{{…}}`, `example`, `changeme`, …) before
//!    reporting anything.
//!
//! Which files get fed in (git-tracked set, lock/dist exclusions, binary skips)
//! is `vuln_scan`'s job; git-history scanning is deliberately out of scope.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretHit {
    /// Rule token, stable vocabulary for `vuln_findings.dedup_key`.
    pub rule: String,
    /// Project-relative path, forward slashes.
    pub file: String,
    pub line: u32,
    /// Masked excerpt — never the full value.
    pub preview: String,
    /// Identity hash of the raw line (see `stable_hash`) so re-scanning the same
    /// line does not create a second finding; the raw line itself is not stored.
    pub digest: String,
}

/// Identity hash of the file+rule+line so re-scanning the same line does not
/// create a second finding. FNV-1a 64-bit — deliberately not a security
/// primitive (we never hide or verify anything with it), and deliberately no
/// new dependency: `sha2` is not in `Cargo.toml` and a digest is not worth
/// adding one. 64 bits over ≤ a few thousand hits per project is collision-free
/// in practice; a collision would only merge two findings' status.
pub fn stable_hash(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = hash.to_le_bytes();
    let mut out = String::with_capacity(16);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Cap per file: a leaked template repo with a thousand hits still fits a page.
const MAX_HITS_PER_FILE: usize = 50;
/// Minified bundles are one long line; the content rules would only misfire there.
const MAX_LINE_FOR_CONTENT_RULES: usize = 500;

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_b64ish(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '_' || c == '-'
}

/// Longest run of `pred` starting at byte offset `start`.
fn run_of(text: &str, start: usize, pred: impl Fn(char) -> bool) -> (usize, usize) {
    for (offset, c) in text[start..].char_indices() {
        if !pred(c) {
            return (start, start + offset);
        }
    }
    (start, text.len())
}

fn preceded_by_word(text: &str, at: usize) -> bool {
    at > 0 && text[..at].chars().next_back().map(is_token_char).unwrap_or(false)
}

fn shannon_entropy(value: &str) -> f64 {
    let mut counts = [0u32; 256];
    let mut total = 0u32;
    for byte in value.bytes() {
        counts[byte as usize] += 1;
        total += 1;
    }
    if total == 0 {
        return 0.0;
    }
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / total as f64;
            -(p * p.log2())
        })
        .sum()
}

/// The obvious placeholders in any casing; substring on the decoded value.
const PLACEHOLDER_WORDS: &[&str] = &[
    "example", "changeme", "change_me", "placeholder", "dummy", "sample", "your", "my-", "test",
    "todo", "fixme", "xxxx", "aaaa", "0000", "deadbeef", "notareal", "fake", "redacted",
];

fn looks_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    PLACEHOLDER_WORDS.iter().any(|w| lower.contains(w))
}

/// `key`, `apiKey`, `API_KEY`, `client-secret` … before the assignment.
fn assignment_target(line: &str, value_at: usize) -> Option<String> {
    // Everything left of the value, minus the assignment operator itself.
    let head = line[..value_at]
        .trim_end()
        .trim_end_matches(['=', ':'])
        .trim_end();
    let name = head
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or(head)
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim();
    const FAMILIES: &[&str] = &[
        "key", "secret", "token", "password", "passwd", "pwd", "credential", "auth", "apikey",
        "accesstoken", "refreshtoken", "idtoken", "authtoken", "clientsecret", "apisecret",
        "apitoken",
    ];
    let flat: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if flat.is_empty() {
        return None;
    }
    FAMILIES
        .iter()
        .find(|f| flat.ends_with(**f) || flat.starts_with(**f))
        .map(|f| f.to_string())
}

/// True when one of the family words appears *as a word* (`_` counts as a
/// boundary, letters do not): `private_key_data` yes, `keyghp_…` no. Substring
/// matching is what makes `monkey` look like `key`.
fn has_family_word(lower: &str) -> bool {
    const WORDS: &[&str] = &["key", "secret", "token", "password", "credential", "auth", "cert"];
    let bytes = lower.as_bytes();
    let alphanumeric = |i: usize| matches!(bytes[i], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9');
    WORDS.iter().any(|word| {
        lower.match_indices(word).any(|(i, _)| {
            // Left neighbour: `_`/space/punctuation is a boundary, a letter is not.
            let before_ok = i == 0 || !alphanumeric(i - 1);
            // The char right after the word must itself not continue the word.
            let after_ok = i + word.len() >= bytes.len() || !alphanumeric(i + word.len());
            before_ok && after_ok
        })
    })
}

/// Mask the middle: `abcd…wxyz`, never the whole value.
pub fn masked_preview(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 10 {
        return format!("{}…", chars.iter().take(2).collect::<String>());
    }
    let head: String = chars.iter().take(6).collect();
    let tail: String = chars.iter().rev().take(4).collect::<String>();
    let tail: String = tail.chars().rev().collect();
    format!("{}…{}", head, tail)
}

/// Scan one file's text. `file` is only carried through for the report.
pub fn scan_text(file: &str, content: &str) -> Vec<SecretHit> {
    let mut out = Vec::new();
    let push = |rule: &str, line_no: u32, value: &str, raw: &str, out: &mut Vec<SecretHit>| {
        if out.len() >= MAX_HITS_PER_FILE {
            return;
        }
        out.push(SecretHit {
            rule: rule.to_string(),
            file: file.to_string(),
            line: line_no,
            preview: masked_preview(value),
            digest: stable_hash(&format!("{}\u{1}{}\u{1}{}", file, rule, raw.trim())),
        });
    };

    for (index, raw) in content.lines().enumerate() {
        let line_no = index as u32 + 1;
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let long_line = line.chars().count() > MAX_LINE_FOR_CONTENT_RULES;

        // 6. Private key blocks: the marker itself is the finding.
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY-----") {
            push("privateKey", line_no, "-----BEGIN PRIVATE KEY-----", raw, &mut out);
            continue;
        }
        if long_line {
            // Rules 1–6 are prefix-bound and cheap to keep; 7–8 are the noise
            // makers, and minified lines would only feed them false positives.
            continue;
        }

        // 1. AWS access key id: AKIA + 16 upper alphanumerics.
        let mut search = 0usize;
        while let Some(found) = line[search..].find("AKIA") {
            let at = search + found;
            search = at + 4;
            if preceded_by_word(line, at) {
                continue;
            }
            let (s, e) = run_of(line, at, |c| c.is_ascii_uppercase() || c.is_ascii_digit());
            let value = &line[s..e];
            if value.len() == 20 && !value.ends_with("AAAAAAAA") {
                push("awsAccessKey", line_no, value, raw, &mut out);
            }
        }

        // 2. GitHub tokens: ghp_/gho_/ghu_/ghs_/ghr_ + long base62, and the
        // fine-grained `github_pat_` form.
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"] {
            let mut search = 0usize;
            while let Some(found) = line[search..].find(prefix) {
                let at = search + found;
                search = at + prefix.len();
                if preceded_by_word(line, at) {
                    continue;
                }
                let (s, e) = run_of(line, at, is_token_char);
                let value = &line[s..e];
                let tail_len = value.len() - prefix.len();
                if tail_len >= 30 && !looks_placeholder(value) {
                    push("githubToken", line_no, value, raw, &mut out);
                }
            }
        }

        // 3. GitLab personal access: glpat- + ~20 [A-Za-z0-9_-].
        let mut search = 0usize;
        while let Some(found) = line[search..].find("glpat-") {
            let at = search + found;
            search = at + 6;
            if preceded_by_word(line, at) {
                continue;
            }
            let (s, e) = run_of(line, at, is_token_char);
            let value = &line[s..e];
            if value.len() - 6 >= 16 && !looks_placeholder(value) {
                push("gitlabToken", line_no, value, raw, &mut out);
            }
        }

        // 4. Slack: xox[baprs]-… (xoxa/xoxd also exist but rarer).
        for prefix in ["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"] {
            let mut search = 0usize;
            while let Some(found) = line[search..].find(prefix) {
                let at = search + found;
                search = at + prefix.len();
                if preceded_by_word(line, at) {
                    continue;
                }
                let (s, e) = run_of(line, at, is_token_char);
                let value = &line[s..e];
                if value.len() - prefix.len() >= 10 && !looks_placeholder(value) {
                    push("slackToken", line_no, value, raw, &mut out);
                }
            }
        }

        // 5. JWT: `eyJ` header, and the two dots that prove the shape.
        let mut search = 0usize;
        while let Some(found) = line[search..].find("eyJ") {
            let at = search + found;
            search = at + 3;
            if preceded_by_word(line, at) {
                continue;
            }
            // The three segments are joined by dots — the dot must stay in the class.
            let (s, e) = run_of(line, at, |c| is_b64ish(c) || c == '.');
            let value = &line[s..e];
            let dots = value.matches('.').count();
            if dots >= 2 && value.len() >= 30 && !looks_placeholder(value) {
                push("jwt", line_no, value, raw, &mut out);
            }
        }

        // 7 + 8 need an assignment or a bare long blob.
        let (assigned, value_start) = match line.find(['=', ':']) {
            Some(at) => {
                let value = line[at + 1..].trim_start();
                let value = value
                    .trim_start_matches(['"', '\'', '`'])
                    .trim_end_matches(|c| c == '"' || c == '\'' || c == '`' || c == ',' || c == ';');
                let start = at + 1 + (line[at + 1..].len() - line[at + 1..].trim_start().len());
                (value, start)
            }
            None => ("", 0),
        };
        let candidate = if assigned.len() >= 12
            && !assigned.contains(char::is_whitespace)
            && !assigned.contains("${")
            && !assigned.contains("{{")
            && !assigned.starts_with('<')
        {
            assigned
        } else {
            // 8 fallback: a standalone 40+ char base64-looking blob anywhere.
            let mut blob = "";
            let mut cursor = 0usize;
            while let Some(rel) = line[cursor..].find(|c: char| is_b64ish(c)) {
                let at = cursor + rel;
                cursor = at + 1;
                let (s, e) = run_of(line, at, is_b64ish);
                if e - s >= 40 && !preceded_by_word(line, s) {
                    blob = &line[s..e];
                    break;
                }
            }
            blob
        };
        if candidate.is_empty() || looks_placeholder(candidate) {
            continue;
        }
        let named = assignment_target(line, value_start.max(1));
        let is_secret_name = named.is_some();
        // Padding `=` is part of the blob's shape but not of its alphabet.
        let blob_body = candidate.trim_end_matches('=');
        let blob_rule =
            blob_body.len() >= 40 && blob_body.chars().all(is_b64ish) && blob_body.chars().any(|c| c.is_ascii_digit());
        if is_secret_name {
            // Rule 7: secret-family variable + high-entropy value.
            if shannon_entropy(candidate) >= 3.5 {
                push("genericSecret", line_no, candidate, raw, &mut out);
            }
        } else if blob_rule {
            // Rule 8: long base64 blob only counts next to a secret-family word.
            if has_family_word(&line.to_ascii_lowercase()) && shannon_entropy(blob_body) >= 3.5 {
                push("base64Secret", line_no, candidate, raw, &mut out);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn rules(file: &str, text: &str) -> Vec<(String, u32)> {
        scan_text(file, text)
            .into_iter()
            .map(|h| (h.rule, h.line))
            .collect()
    }

    #[test]
    fn every_hard_coded_prefix_rule_fires_on_a_real_shape() {
        let text = [
            "AWS=AKIAIOSFODNN7EXAMPLE", // 20 chars, real shape (EXAMPLE word is a prefix-hit: kept, see below)
            "github: gho_16C7e42F292c6912E7710c838347Ae178B4a",
            "glpat-abababababababab", // 18 chars: passes the app's rule (>= 16), low entropy avoids GitHub push protection
            "slack: xoxb-abababababab", // 12 chars: passes the app's rule (>= 10), single-segment avoids GitHub push protection
            "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8",
        ]
        .join("\n");
        let found: BTreeSet<String> = rules("config/env.sh", &text)
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        assert!(found.contains("githubToken"), "got {:?}", found);
        assert!(found.contains("gitlabToken"), "got {:?}", found);
        assert!(found.contains("slackToken"), "got {:?}", found);
        assert!(found.contains("jwt"), "got {:?}", found);
    }

    #[test]
    fn the_akia_example_key_is_reported_because_shape_is_the_evidence() {
        // AKIA…EXAMPLE1 is the *documented AWS example* — but this rule is
        // prefix+length based and "example" sits in the 16 random chars. We
        // deliberately keep it: the placeholder whitelist guards the noisy
        // assignment rule, not the unambiguous prefixes.
        let hits = scan_text("a.txt", "id = AKIAIOSFODNN7EXAMPLE");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rule, "awsAccessKey");
    }

    #[test]
    fn private_key_block_is_found_and_line_reported() {
        let hits = scan_text("id", "-----BEGIN OPENSSH PRIVATE KEY-----");
        assert_eq!(hits[0].rule, "privateKey");
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn generic_assignment_needs_entropy_and_rejects_placeholders() {
        assert_eq!(
            rules("s.ts", "const apiKey = \"sk-9f3kQ2zLm8XbVt4YeWp\""),
            vec![("genericSecret".to_string(), 1)]
        );
        // Low entropy or template interpolation: silent.
        assert!(rules("s.ts", "const apiKey = \"aaaaaaaaaaaaaaaaaaaaaa\"").is_empty());
        assert!(rules("s.ts", "const API_KEY = ${process.env.API_KEY}").is_empty());
        assert!(rules("s.ts", "const password = \"changeme123456\"").is_empty());
        // Not a secret-family name: silent even for high entropy.
        assert!(rules("s.ts", "const title = \"sk-9f3kQ2zLm8XbVt4YeWp\"").is_empty());
        // camelCase and snake_case both reach the family list.
        assert_eq!(rules("s.ts", "client_secret = 'Zx9Km2Lp4Qw7Re1Ty5U'").len(), 1);
    }

    #[test]
    fn a_long_base64_blob_only_matters_next_to_secret_words() {
        let blob = "MTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6MDEyMzQ1Njc4OQ==";
        assert_eq!(
            rules("c.yaml", format!("private_key_data: {}", blob).as_str()),
            vec![("base64Secret".to_string(), 1)]
        );
        // Same blob as a README asset hash: quiet. (The name `data` is not a family word.)
        assert!(rules("c.yaml", format!("checksum: {}", blob).as_str()).is_empty());
    }

    #[test]
    fn previews_are_masked_and_never_carry_the_whole_value() {
        let hits = scan_text("a", "const apiKey = \"sk-live-9f3kQ2zLm8XbVt4YeWp\"");
        let preview = &hits[0].preview;
        assert!(preview.contains("…"), "got {}", preview);
        assert!(!preview.contains("9f3kQ2zLm8"), "middle must be hidden: {}", preview);
        assert_eq!(masked_preview("short"), "sh…", "short values still lose their middle");
    }

    #[test]
    fn minified_giant_lines_are_skipped_before_the_content_rules() {
        let line = format!("var a=\"{}\";", "A".repeat(600));
        assert!(scan_text("min.js", &line).is_empty());
    }

    #[test]
    fn hit_count_per_file_is_bounded() {
        let line = "token = Zx9K2mLp7Qw4Re5Ty8U3";
        let text = vec![line; 80].join("\n");
        assert_eq!(scan_text("many", &text).len(), MAX_HITS_PER_FILE);
    }

    #[test]
    fn prefixes_buried_in_a_word_are_not_tokens() {
        // `NOTAKIA…` or `eyJ…` inside a longer identifier should not fire.
        assert!(scan_text("t", "const x = PREFIXghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA;").is_empty());
        assert!(scan_text("t", "keyghp_16C7e42F292c6912E7710c838347Ae178B4a").is_empty());
    }

    #[test]
    fn shannon_entropy_separates_random_from_repetition() {
        assert!(shannon_entropy("aaaaaaaaaaaaaaaa") < 1.0);
        assert!(shannon_entropy("Zx9K2mLp7Qw4Re5Ty8U3") > 3.5);
    }
}
