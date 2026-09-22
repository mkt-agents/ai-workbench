//! Change analysis for the regression report: git's machine-readable diff output in,
//! structured "what changed, what it touches, what must be re-tested" data out.
//!
//! Pure by design (no tauri, no sqlite, no process spawning) so it compiles and runs
//! from a scratch crate, and so the git plumbing stays in one obvious place.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// One changed file, as classified for the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    /// `None` unless git reported a rename/copy.
    pub old_path: Option<String>,
    /// A M D R C T U — straight from `--name-status`.
    pub status: String,
    pub adds: u32,
    pub dels: u32,
    pub module: String,
    pub layer: String,
    pub risks: Vec<String>,
    /// Paired test file we looked for, `None` when no convention applies.
    pub test_path: Option<String>,
    pub has_test: bool,
    /// Untracked (new, never committed) files carry no diff and no line counts.
    pub untracked: bool,
    /// Short shas of the commits in `commit_details` that touched this file.
    #[serde(default)]
    pub commits: Vec<String>,
    /// Weak signals (substring-only security/money matches): surfaced grey, never
    /// counted as high risk, never fed into `suggested_scope`.
    #[serde(default)]
    pub hints: Vec<String>,
}

/// One commit in the analysed range — enough to answer "which change did this".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitBrief {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub body: String,
}

/// `git log --format=%x02…%x01 --name-only -z` output: record bodies separated by
/// \x02, each `fields\x01` followed by NUL-separated paths. Control bytes cannot
/// appear in commit messages (git sanitises them), so the markers are unambiguous.
/// Returns (briefs, path -> short shas) with paths left as git printed them.
pub fn parse_log_z(raw: &str) -> (Vec<CommitBrief>, BTreeMap<String, Vec<String>>) {
    let mut briefs = Vec::new();
    let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for record in raw.split('\u{2}') {
        let Some((fields, names)) = record.split_once('\u{1}') else { continue };
        let cols: Vec<&str> = fields.splitn(6, '\0').map(|c| c.trim_matches('\n')).collect();
        if cols.len() < 6 || cols[0].is_empty() {
            continue;
        }
        let body = cols[5].trim();
        briefs.push(CommitBrief {
            sha: cols[0].to_string(),
            short_sha: cols[1].to_string(),
            author: cols[2].to_string(),
            date: cols[3].to_string(),
            subject: cols[4].trim().to_string(),
            body: body.to_string(),
        });
        for path in names.split('\0') {
            let path = path.trim_matches(|c| c == '\n' || c == '\r').trim();
            if path.is_empty() {
                continue;
            }
            let entry = by_file.entry(path.to_string()).or_default();
            if entry.last().map(String::as_str) != Some(cols[1]) {
                entry.push(cols[1].to_string());
            }
        }
    }
    (briefs, by_file)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleGroup {
    pub name: String,
    pub files: u32,
    pub adds: u32,
    pub dels: u32,
    pub layers: Vec<String>,
    pub risks: Vec<String>,
    pub untested: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiChange {
    /// Added | Removed | Changed (the same signature appeared on both sides).
    pub kind: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeStats {
    pub files: u32,
    pub adds: u32,
    pub dels: u32,
    pub modules: u32,
    pub code_files: u32,
    pub untested: u32,
    pub test_files: u32,
    /// IDE / build output dropped from the report.
    pub ignored: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeReport {
    /// "" for uncommitted changes, otherwise the ref the diff ran against.
    pub base: String,
    /// "uncommitted" | "base" | "commits"
    pub source: String,
    pub commits: Vec<String>,
    /// Full commit metadata, newest first; `commits` stays as the subject list the
    /// old UI text was built from.
    #[serde(default)]
    pub commit_details: Vec<CommitBrief>,
    pub files: Vec<FileChange>,
    pub groups: Vec<ModuleGroup>,
    pub stats: ChangeStats,
    pub api_changes: Vec<ApiChange>,
    /// Tokens the UI maps to "建议回归范围" rows.
    pub scope: Vec<String>,
    pub truncated: bool,
}

/// `--name-status -z` fields: `[<status>, <path>]` normally,
/// `[R100, <old>, <new>]` for renames and copies — old first, verified against git 2.x.
pub fn parse_name_status_z(raw: &str) -> Vec<(String, Option<String>, String)> {
    let fields: Vec<&str> = raw.split('\0').filter(|f| !f.is_empty()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let status = fields[i].trim_start_matches(['\n', '\r', ' ']).to_string();
        let code: String = status.chars().take(1).collect();
        if matches!(code.as_str(), "R" | "C") && i + 2 < fields.len() {
            out.push((code, Some(fields[i + 1].to_string()), fields[i + 2].to_string()));
            i += 3;
        } else if i + 1 < fields.len() {
            out.push((code, None, fields[i + 1].to_string()));
            i += 2;
        } else {
            break;
        }
    }
    out
}

/// `--numstat -z`: `<adds>\t<dels>\t<path>\0`, where renames leave the path slot empty
/// and follow with `<old>\0<new>\0`.
pub fn parse_numstat_z(raw: &str) -> Vec<(u32, u32, Option<String>, String)> {
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let head = fields[i].trim_end_matches(['\n', '\r']);
        if head.is_empty() {
            i += 1;
            continue;
        }
        let mut parts = head.splitn(3, '\t');
        let (adds_raw, dels_raw, path_raw) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let adds = adds_raw.trim().parse().unwrap_or(0);
        let dels = dels_raw.trim().parse().unwrap_or(0);
        if !path_raw.trim().is_empty() {
            out.push((adds, dels, None, path_raw.trim().to_string()));
            i += 1;
            continue;
        }
        // Rename: the two paths are the next fields.
        let old = fields.get(i + 1).copied().unwrap_or("").trim().to_string();
        let new = fields.get(i + 2).copied().unwrap_or("").trim().to_string();
        if old.is_empty() || new.is_empty() {
            break;
        }
        out.push((adds, dels, Some(old), new));
        i += 3;
    }
    out
}

/// Binary blobs report `-`; callers map that to 0/0 before we see it.
fn is_code(path: &str) -> bool {
    !matches!(
        extension(path).as_str(),
        "md" | "txt" | "adoc" | "png" | "jpg" | "jpeg" | "gif" | "ico" | "svg" | "woff" | "woff2" | "ttf"
            | "jar" | "zip" | "gz" | "pdf" | "xls" | "xlsx"
    )
}

fn extension(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path)
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_lowercase())
        .unwrap_or_default()
}

fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// IDE droppings and build output are not a change set — but repos that forgot to
/// ignore them would otherwise bury the real files, so they are counted separately
/// and surfaced as "已忽略 N 个". Exact directory segments only: `src/bin` in Rust and
/// a module named `xx-target` stay visible.
pub fn is_noise(path: &str) -> bool {
    const NOISE_DIRS: &[&str] = &[
        ".idea",
        ".vs",
        ".vscode",
        ".gradle",
        "node_modules",
        "__pycache__",
        ".pytest_cache",
        ".mypy_cache",
        "target",
        "build",
        "dist",
        "out",
        "coverage",
        "logs",
        ".next",
        ".nuxt",
        "generated-sources",
    ];
    const NOISE_SUFFIX: &[&str] = &[
        ".iml", ".user", ".suo", ".ds_store", ".orig", ".rej", ".log", ".patch", ".class", ".jar",
    ];
    let lower = path.to_lowercase();
    if NOISE_SUFFIX.iter().any(|suffix| lower.ends_with(suffix)) {
        return true;
    }
    let segments: Vec<&str> = lower.split('/').collect();
    let dirs = if segments.is_empty() { 0 } else { segments.len() - 1 };
    segments[..dirs].iter().any(|segment| NOISE_DIRS.contains(segment))
}

/// Everything before the first `src` segment is the module: maven multi-module repos
/// and npm workspaces both group cleanly that way, a single-module repo lands on
/// `(root)`.
pub fn module_of(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(index) = segments.iter().position(|s| *s == "src") {
        if index > 0 {
            return segments[..index].join("/");
        }
        return "(root)".to_string();
    }
    match segments.len() {
        0 | 1 => "(root)".to_string(),
        _ => segments[0].to_string(),
    }
}

/// Coarse "which layer did we touch" label — this is what drives the suggested
/// regression scope, so ambiguous paths resolve toward the more testable label.
pub fn layer_of(path: &str) -> &'static str {
    let name = file_name(path);
    let lower = path.to_lowercase();
    let ext = extension(path);

    let is_test = lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
        || lower.contains("/src/test/")
        || name.ends_with("Test.java")
        || name.ends_with("Tests.java")
        || name.ends_with("IT.java")
        || name.ends_with("Spec.java")
        || name.ends_with("_test.go")
        || name.ends_with("_test.rs")
        || matches!(ext.as_str(), "ts" | "tsx" | "js" | "jsx" | "vue" | "svelte")
            && (name.contains(".test.") || name.contains(".spec."));
    if is_test && !lower.contains("/src/main/") {
        return "test";
    }

    if lower.contains("/db/migration/")
        || lower.contains("/migration/")
        || lower.contains("/migrations/")
        || (ext == "sql" && !lower.contains("/target/"))
    {
        return "migration";
    }
    if ext == "sql" {
        return "sql";
    }
    if matches!(
        name.as_str(),
        "pom.xml" | "package.json" | "package-lock.json" | "Cargo.toml" | "go.mod" | "build.gradle" | "yarn.lock" | "pnpm-lock.yaml"
    ) || name.ends_with(".gradle")
    {
        return "build";
    }
    if lower.ends_with(".mapper.xml") || (lower.contains("/mapper/") && ext == "xml") {
        return "sql";
    }
    if lower.contains("/resources/") && matches!(ext.as_str(), "yml" | "yaml" | "properties") {
        return "config";
    }
    if matches!(ext.as_str(), "yml" | "yaml" | "properties" | "toml" | "ini" | "env")
        || lower.contains("/config/")
        || name.starts_with('.') && ext.is_empty()
    {
        return "config";
    }
    if ext == "md" || ext == "txt" || ext == "adoc" || lower.starts_with("docs/") {
        return "docs";
    }
    if matches!(ext.as_str(), "css" | "scss" | "less" | "sass") {
        return "style";
    }
    if matches!(ext.as_str(), "java" | "kt" | "kts" | "scala") {
        if lower.contains("/controller/") || lower.contains("/web/") || lower.contains("/rest/") || lower.contains("/api/")
            || name.ends_with("Controller.java")
            || name.ends_with("Resource.java")
            || name.ends_with("Controller.kt")
        {
            return "api";
        }
        if lower.contains("/service/") || name.ends_with("Service.java") || name.ends_with("ServiceImpl.java") || name.ends_with("Manager.java") {
            return "service";
        }
        if lower.contains("/mapper/") || lower.contains("/repository/") || lower.contains("/dao/") || name.ends_with("Mapper.java") {
            return "persistence";
        }
        if lower.contains("/dto/") || lower.contains("/vo/") || lower.contains("/entity/") || lower.contains("/model/") || lower.contains("/domain/") {
            return "model";
        }
        return "logic";
    }
    if matches!(ext.as_str(), "vue" | "jsx" | "tsx" | "svelte") {
        if lower.contains("/pages/") || lower.contains("/views/") || lower.contains("/router/") {
            return "page";
        }
        if lower.contains("/components/") {
            return "component";
        }
        return "view";
    }
    if matches!(ext.as_str(), "ts" | "js") {
        if lower.contains("/api/") || lower.contains("/http/") || lower.contains("/request") || name.contains("api") {
            return "api";
        }
        if lower.contains("/store") || lower.contains("/stores/") || lower.contains("/hooks/") {
            return "state";
        }
        if lower.contains("/utils/") || lower.contains("/lib/") {
            return "logic";
        }
        return "frontend";
    }
    if ext == "rs" {
        if name.ends_with("_commands.rs") || lower.contains("/commands/") {
            return "api";
        }
        return "logic";
    }
    if ext == "go" {
        if lower.contains("/handler") || lower.contains("/api") || lower.contains("/router") {
            return "api";
        }
        if lower.contains("/store") || lower.contains("/repo") || lower.contains("/dao") {
            return "persistence";
        }
        return "logic";
    }
    if matches!(ext.as_str(), "xml" | "json") {
        return "config";
    }
    "other"
}

/// Strong-evidence words: a camel/snake word equal to one of these is a real
/// security/money surface. Substring-only matches (a file named `tokenizer.ts`)
/// are demoted to hints — the old substring rule produced so many false
/// positives that testers stopped trusting the badge.
const SEC_WORDS: &[&str] = &["security", "auth", "login", "password", "passwd", "credential", "credentials", "token", "secret", "apikey"];
const MONEY_WORDS: &[&str] = &["pay", "payment", "payments", "price", "amount", "billing", "money", "cash", "fund", "order", "invoice", "checkout"];
/// Weak, substring-level — demoted to hints when nothing stronger fires.
const SEC_SUBSTR: &[&str] = &["security", "auth", "login", "password", "token"];
const MONEY_SUBSTR: &[&str] = &["pay", "price", "amount", "order", "bill"];

/// Split a path into lower-case words on separators, camelCase and snake_case
/// boundaries. "OAuth2Filter.kt" → ["o", "auth2filter", "kt"] roughly, and
/// crucially "tokenizer.ts" yields "tokenizer", never "token".
pub fn path_words(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    for segment in path.split(|c: char| !(c.is_alphanumeric())) {
        if segment.is_empty() {
            continue;
        }
        let chars: Vec<char> = segment.chars().collect();
        let mut word = String::new();
        for (i, ch) in chars.iter().enumerate() {
            if ch.is_uppercase() && !word.is_empty() {
                let prev = chars[i - 1];
                let next_lower = chars.get(i + 1).map(|c| c.is_lowercase()).unwrap_or(false);
                if prev.is_lowercase() || prev.is_numeric() || (next_lower && word.chars().count() > 1) {
                    out.push(word.to_lowercase());
                    word = String::new();
                }
            }
            word.push(*ch);
        }
        if !word.is_empty() {
            out.push(word.to_lowercase());
        }
    }
    out
}

/// Content evidence per file from the patch's added lines: an `+` line whose
/// words include password/token/… counts as strong, even if the file name says
/// nothing. Keyed by the normalized new-side path.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileSignals {
    pub security: bool,
    pub money: bool,
}

pub fn patch_content_signals(patch: &str) -> BTreeMap<String, FileSignals> {
    let mut out: BTreeMap<String, FileSignals> = BTreeMap::new();
    let mut current = String::new();
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current = path.split('\t').next().unwrap_or(path).trim().to_lowercase();
            continue;
        }
        if current.is_empty() || !line.starts_with('+') || line.starts_with("+++") {
            continue;
        }
        let words = path_words(&line[1..]);
        let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
        let entry = out.entry(current.clone()).or_default();
        if has(&SEC_WORDS) || (words.iter().any(|w| w == "api") && words.iter().any(|w| w == "key")) {
            entry.security = true;
        }
        if has(&MONEY_WORDS)
            || has(&["balance", "payable", "currency", "decimal", "refund", "settle", "settlement"])
        {
            entry.money = true;
        }
    }
    out
}

/// Risk tokens per file. Rendered as badges, and read by `suggested_scope`.
/// Returns (risks, hints); hints never reach the scope mapping.
pub fn risk_tokens_with(
    path: &str,
    status: &str,
    layer: &str,
    signals: FileSignals,
) -> (Vec<String>, Vec<String>) {
    let mut risks = Vec::new();
    let mut hints = Vec::new();
    let lower = path.to_lowercase();
    if layer == "sql" || lower.contains("mapper") && extension(path) == "xml" {
        risks.push("sql".to_string());
    }
    if layer == "migration" {
        risks.push("migration".to_string());
    }
    if layer == "build" {
        risks.push("dependency".to_string());
    }
    if layer == "config" {
        risks.push("config".to_string());
    }
    if status == "D" {
        risks.push("deleted".to_string());
    }
    if status == "A" {
        risks.push("newFile".to_string());
    }
    let words = path_words(path);
    let word_hit = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
    if word_hit(&SEC_WORDS) || signals.security {
        risks.push("security".to_string());
    } else if SEC_SUBSTR.iter().any(|s| lower.contains(s)) {
        hints.push("security".to_string());
    }
    if word_hit(&MONEY_WORDS) || signals.money {
        risks.push("money".to_string());
    } else if MONEY_SUBSTR.iter().any(|s| lower.contains(s)) {
        hints.push("money".to_string());
    }
    (risks, hints)
}

/// Conventional test location for a source file, used only as a hint of what to look
/// for; `None` when no convention applies (docs, config, …).
pub fn test_candidates(path: &str) -> Vec<String> {
    let name = file_name(path);
    let ext = extension(path);
    let mut out = Vec::new();

    if matches!(ext.as_str(), "java" | "kt" | "kts" | "scala") {
        let Some(index) = path.find("/src/main/") else { return out };
        let prefix = &path[..index];
        // "java/com/x/web/AccountController.java" after `/src/main/`.
        let rest = &path[index + "/src/main/".len()..];
        let stem = name.strip_suffix(&format!(".{}", ext)).unwrap_or(&name);
        let dir = rest.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
        let Some((source_root, package_dir)) = dir.split_once('/').map(|(root, pkg)| (root, pkg)).or_else(|| Some((dir, ""))) else { return out };
        if source_root.is_empty() {
            return out;
        }
        let base = format!(
            "{}src/test/{}/{}/",
            if prefix.is_empty() { String::new() } else { format!("{}/", prefix) },
            source_root,
            package_dir
        );
        for suffix in ["Test", "Tests", "IT"] {
            out.push(format!("{}{}{}.{}", base, stem, suffix, ext));
        }
        return out;
    }

    if matches!(ext.as_str(), "ts" | "tsx" | "js" | "jsx" | "vue" | "svelte") {
        let without_ext = name.strip_suffix(&format!(".{}", ext)).unwrap_or(&name);
        let stem = without_ext.rsplit('.').next().unwrap_or(without_ext);
        let dir = path.strip_suffix(&name).unwrap_or("");
        for suffix in ["test", "spec"] {
            out.push(format!("{}{}.{}.{}", dir, without_ext, suffix, ext));
        }
        out.push(format!("{}__tests__/{}.test.{}", dir, stem, ext));
        out.push(format!("{}{}.test.ts", dir, stem));
        return out;
    }

    if ext == "py" {
        // pytest layouts vary more than the others, so offer the common shapes:
        // mirrored `tests/` tree, sibling test file, and a flat `tests/` directory.
        let stem = name.strip_suffix(".py").unwrap_or(&name);
        let dir = path.strip_suffix(&name).unwrap_or("");
        let leaf = format!("test_{}.py", stem);
        if stem == "__init__" || stem == "conftest" {
            return out;
        }
        out.push(leaf.clone());
        out.push(format!("tests/{}", leaf));
        if !dir.is_empty() {
            out.push(format!("{}{}", dir, leaf));
            out.push(format!("tests/{}{}", dir, leaf));
            let mirror = dir
                .trim_start_matches("./")
                .rsplit_once('/')
                .map(|(head, _)| format!("tests/{}/", head))
                .unwrap_or_else(|| "tests/".to_string());
            let mirrored = format!("{}{}", mirror, leaf);
            if !out.contains(&mirrored) {
                out.push(mirrored);
            }
        }
        return out;
    }

    if ext == "rs" {
        // Inline `#[cfg(test)]` is the common Rust convention and is reported through
        // `inline_test_files`; the other shapes are integration test directories —
        // including a crate that lives in a subdirectory (`src-tauri/src/x.rs` →
        // `src-tauri/tests/x.rs`), not just at the repo root.
        let stem = name.strip_suffix(".rs").unwrap_or(&name);
        out.push(format!("tests/{}.rs", stem));
        if let Some(at) = path.find("/src/") {
            out.push(format!("{}/tests/{}.rs", &path[..at], stem));
        }
        out.push(format!("src/tests/{}.rs", stem));
        return out;
    }

    if ext == "go" {
        let dir = path.strip_suffix(&name).unwrap_or("");
        let stem = name.strip_suffix(".go").unwrap_or(&name);
        out.push(format!("{}{}_test.go", dir, stem));
        return out;
    }

    out
}

fn matched_test(candidates: &[String], known: &HashSet<String>) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| known.contains(&normalize_key(candidate)))
        .cloned()
}

fn normalize_key(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_lowercase()
}

/// Signature-level changes pulled out of the patch: only what a caller can act on
/// (an interface that moved is a regression trigger, the body is noise). One row per
/// public name, so a report lists "what a tester must re-check", not every hunk.
pub fn api_surface_changes(patch: &str) -> Vec<ApiChange> {
    // (full shape, display params, return type) per name, plus the file it came from.
    let mut removed: BTreeMap<String, Vec<(String, String, String, String)>> = BTreeMap::new();
    let mut added: BTreeMap<String, Vec<(String, String, String, String)>> = BTreeMap::new();
    let mut file = String::new();
    let mut pre_image = String::new();

    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            file = String::new();
            pre_image = String::new();
            continue;
        }
        if let Some(path) = line.strip_prefix("--- a/") {
            pre_image = path.trim().to_string();
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ b/") {
            file = path.trim().to_string();
            continue;
        }
        if line.starts_with("+++ /dev/null") {
            // Deletion: attribute the removed signatures to the file that still exists.
            file = pre_image.clone();
            continue;
        }
        if !line.starts_with('+') && !line.starts_with('-') {
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let Some((name, full, display, ret)) = signature_of(&file, &line[1..]) else { continue };
        if name.is_empty() || file.is_empty() {
            continue;
        }
        if line.starts_with('+') {
            added.entry(name).or_default().push((full, display, ret, file.clone()));
        } else {
            removed.entry(name).or_default().push((full, display, ret, file.clone()));
        }
    }

    let names: BTreeSet<String> = added.keys().chain(removed.keys()).cloned().collect();
    let mut out = Vec::new();
    for name in names {
        let adds = added.get(&name).cloned().unwrap_or_default();
        let dels = removed.get(&name).cloned().unwrap_or_default();
        // The same signature on both sides is a moved line, not a changed contract.
        let fresh_adds: Vec<(String, String, String, String)> = adds.iter().filter(|entry| !dels.contains(entry)).cloned().collect();
        let fresh_dels: Vec<(String, String, String, String)> = dels.iter().filter(|entry| !adds.contains(entry)).cloned().collect();
        let (kind, label) = match (fresh_dels.first(), fresh_adds.first()) {
            (Some(old), Some(new)) => (
                "changed",
                if old.1 != new.1 {
                    format!("{}({} ⇒ {})", name, old.1, new.1)
                } else if old.2 != new.2 {
                    // same params, moved return type — the call site breaks silently.
                    format!(
                        "{}({}) 返回 {} ⇒ {}",
                        name,
                        new.1,
                        if old.2.is_empty() { "-" } else { &old.2 },
                        if new.2.is_empty() { "-" } else { &new.2 },
                    )
                } else {
                    format!("{}({}) 可见性/泛型变化", name, new.1)
                },
            ),
            (Some(old), None) => ("removed", format!("{}({})", name, old.1)),
            (None, Some(new)) => ("added", format!("{}({})", name, new.1)),
            (None, None) => continue,
        };
        let path = match (fresh_dels.first(), fresh_adds.first()) {
            (_, Some(new)) => new.3.clone(),
            (Some(old), None) => old.3.clone(),
            (None, None) => continue,
        };
        out.push(ApiChange { kind: kind.to_string(), name: label, path });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.name.cmp(&b.name)));
    out
}

/// Keep only public entry points, normalised to a five-part shape
/// (visibility, generics, name, parameter types, return type) so a pure
/// reformat does not read as a signature change — but a changed *type* does,
/// which the old "N args" shape missed entirely. Returns
/// (name, full shape, display params, return type).
fn signature_of(path: &str, body: &str) -> Option<(String, String, String, String)> {
    let ext = extension(path);
    let trimmed = body.trim();
    let (visibility, rest) = match ext.as_str() {
        "java" | "kt" | "kts" | "scala" => {
            let mut rest = trimmed;
            let mut visibility = String::new();
            for modifier in ["public", "protected", "private", "static", "final", "synchronized", "abstract", "default", "open", "override"] {
                if let Some(after) = rest.strip_prefix(&format!("{} ", modifier)) {
                    if matches!(modifier, "public" | "protected" | "private") {
                        visibility = modifier.to_string();
                    }
                    rest = after.trim_start();
                }
            }
            if !rest.contains('(') || rest.starts_with("class ") || rest.starts_with("interface ") {
                return None;
            }
            (visibility, rest.to_string())
        }
        "rs" => {
            let is_pub = trimmed.starts_with("pub ");
            let rest = trimmed
                .strip_prefix("pub(crate) fn ")
                .or_else(|| trimmed.strip_prefix("pub fn "))
                .or_else(|| trimmed.strip_prefix("fn "))?;
            (if is_pub { "pub".to_string() } else { String::new() }, format!("fn {}", rest))
        }
        "go" => {
            let after = trimmed.strip_prefix("func ")?;
            // `func (s *Svc) Handle(...)` — skip the receiver to reach the name.
            let after = match after.strip_prefix('(') {
                Some(stripped) => stripped.split_once(')')?.1.trim_start(),
                None => after,
            };
            let name = after.split('(').next()?.trim();
            if name.is_empty() || !name.chars().next()?.is_uppercase() {
                return None; // unexported
            }
            (String::new(), after.to_string())
        }
        "ts" | "tsx" | "js" | "jsx" => {
            let rest = trimmed
                .strip_prefix("export async function ")
                .or_else(|| trimmed.strip_prefix("export function "))
                .or_else(|| trimmed.strip_prefix("export const "))?;
            ("export".to_string(), format!("fn {}", rest))
        }
        "vue" | "svelte" => {
            let rest = trimmed.strip_prefix("export function ")?;
            ("export".to_string(), format!("fn {}", rest))
        }
        _ => return None,
    };
    // name up to the first '(' of the parameter list.
    let head = rest.split('(').next()?;
    let name = {
        let without_generics = if ext == "rs" {
            // `fn foo<T: Display>(...)` — the name sits before the generic list.
            head.split('<').next().unwrap_or(head)
        } else {
            head
        };
        without_generics.split_whitespace().last()?.to_string()
    };
    if name.is_empty()
        || !name.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)
    {
        return None; // `const foo = (…)` yields a stray `=` as "name"
    }
    let generics = if ext == "rs" && head.contains('<') {
        balanced(&head[head.find('<')?..], '<', '>').unwrap_or_default()
    } else {
        String::new()
    };
    let params_start = rest.find('(')?;
    let params_inner = balanced(&rest[params_start..], '(', ')')?;
    let params: Vec<String> = split_top(&params_inner, &[','])
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| param_type(p, &ext))
        .collect();
    // Return type: what follows the closing paren, before the body / throws.
    let after = rest[params_start + params_inner.len() + 2..].trim_start();
    let ret = match ext.as_str() {
        "rs" => after
            .strip_prefix("->")
            .map(|r| split_top(r, &['{'])[0].trim().to_string())
            .unwrap_or_default(),
        "go" => {
            // `) error {`, `) (int, error) {`, `) {`
            let piece = split_top(after, &['{']);
            let upto = piece[0].trim();
            upto.trim_start_matches('(').trim_end_matches(')').trim().to_string()
        }
        "java" => {
            // java declares the return type *before* the name: `String f(...)`,
            // `Map<String, String> f(...)`. Scan back from the name over
            // `>`(`<` nesting so a space inside the generics is not a separator.
            let without_name = {
                let name_len = head.split_whitespace().next_back().map(str::len).unwrap_or(0);
                head[..head.len().saturating_sub(name_len)].trim_end().to_string()
            };
            let chars: Vec<char> = without_name.chars().collect();
            let mut depth = 0i32;
            let mut start = 0usize;
            let mut i = chars.len();
            while i > 0 {
                i -= 1;
                match chars[i] {
                    '>' => depth += 1,
                    '<' => depth -= 1,
                    c if depth == 0 && c.is_whitespace() => {
                        start = i + 1;
                        break;
                    }
                    _ => {}
                }
            }
            let ty: String = chars[start..].iter().collect();
            let ty = ty.trim();
            // A bare constructor (`Foo(...)`) or a dangling generic prefix has no return type.
            if ty.is_empty() || ty.contains('<') && !ty.ends_with('>') {
                String::new()
            } else {
                ty.to_string()
            }
        }
        "kt" | "kts" | "scala" => {
            let piece = split_top(after, &['{']);
            let upto = piece[0].trim();
            let bare = upto.strip_prefix(':').unwrap_or(upto).trim();
            bare.split_whitespace()
                .take_while(|t| *t != "throws")
                .next()
                .unwrap_or("")
                .to_string()
        }
        _ => String::new(),
    };
    // `full` decides equivalence (a visibility, generics or return flip is a
    // contract change); the tester-facing label stays readable: parameter types,
    // and the return type only when *only* the return changed.
    let full = format!("{}<{}>|({})|{}", visibility, generics, params.join(","), ret);
    let display = params.join(", ");
    Some((name, full, display, ret))
}

/// One parameter text -> its type token: java puts the type before the name
/// (everything but the last word, modifiers dropped), go puts it last,
/// rust/ts take what follows `:`. Defaults and names collapse to "?".
fn param_type(raw: &str, ext: &str) -> String {
    let p = raw.trim();
    match ext {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "vue" | "svelte" => {
            let without_default = p.split('=').next().unwrap_or(p).trim();
            match without_default.split_once(':') {
                Some((_name, ty)) => ty.trim().to_string(),
                // `const foo = (a, b) =>` — bare names carry no type info.
                None => "?".to_string(),
            }
        }
        "go" => {
            let without_default = p.split('=').next().unwrap_or(p).trim();
            without_default
                .split_whitespace()
                .last()
                .map(str::to_string)
                .unwrap_or_else(|| "?".to_string())
        }
        _ => {
            // java/kotlin: "final Long id" → "Long"; "String... args" → "String...".
            let tokens: Vec<&str> = p
                .split_whitespace()
                .filter(|t| !matches!(*t, "final" | "const" | "val" | "var"))
                .collect();
            match tokens.len() {
                0 => "?".to_string(),
                1 => {
                    // `(Long)` / `(id)` — cannot tell apart; single token is the type.
                    tokens[0].to_string()
                }
                n => tokens[..n - 1].join(" "),
            }
        }
    }
}

/// Take the balanced content of the first group opened at the string start.
fn balanced(s: &str, open: char, close: char) -> Option<String> {
    let mut depth = 0usize;
    let bytes: Vec<char> = s.chars().collect();
    if bytes.first() != Some(&open) {
        return None;
    }
    let mut out = String::new();
    for (i, ch) in bytes.iter().enumerate() {
        if i == 0 {
            depth = 1;
            continue;
        }
        match ch {
            c if *c == open => depth += 1,
            c if *c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(out);
                }
            }
            _ => {}
        }
        out.push(*ch);
    }
    None
}

/// Split at the top nesting level on any of `stops` (paren/angle depth aware,
/// so `Map<String, String>` and `(a, (b))` survive).
fn split_top(s: &str, stops: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth -= 1,
            c if stops.contains(&c) && depth <= 0 => {
                out.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(ch);
    }
    out.push(current);
    out
}

/// The static answer to "这次改动要回归到哪一层".
pub fn suggested_scope(layers: &BTreeSet<String>, risks: &BTreeSet<String>, api_changes: &[ApiChange]) -> Vec<String> {
    let mut scope = Vec::new();
    let has = |name: &str| layers.iter().any(|l| l == name);
    let has_risk = |name: &str| risks.iter().any(|r| r == name);

    if has_risk("migration") {
        scope.push("dataMigration".to_string());
    }
    if has_risk("sql") || has("persistence") {
        scope.push("dataAccess".to_string());
    }
    if has("api") || api_changes.iter().any(|c| c.kind != "added") {
        scope.push("apiContract".to_string());
    }
    if has("service") || has("logic") {
        scope.push("businessFlow".to_string());
    }
    if has("page") || has("component") || has("view") || has("state") {
        scope.push("uiFlow".to_string());
    }
    if has_risk("security") {
        scope.push("security".to_string());
    }
    if has_risk("money") {
        scope.push("money".to_string());
    }
    if has_risk("dependency") || has_risk("config") {
        scope.push("smoke".to_string());
    }
    if scope.is_empty() {
        scope.push(if has("docs") || has("style") { "none".to_string() } else { "smoke".to_string() });
    }
    scope
}

/// One map-reduce unit for the AI step: a module's worth of files plus the
/// interface changes that live in them. Chunked so a 300-file report does not
/// get one model call with a truncated prompt — each chunk fits its own budget.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChunk {
    pub module: String,
    pub files: Vec<FileChange>,
    pub api: Vec<ApiChange>,
}

/// Pack files into ≤`max_files`-per-chunk, splitting a large module across
/// chunks; api changes attach to a chunk of their own module (capped at
/// `max_api`, overflow starts a files-empty chunk so nothing is dropped).
pub fn plan_ai_chunks(report: &ChangeReport, max_files: usize, max_api: usize) -> Vec<AiChunk> {
    let max_files = max_files.max(1);
    let mut chunks: Vec<AiChunk> = Vec::new();
    for file in &report.files {
        match chunks.iter_mut().find(|c| c.module == file.module && c.files.len() < max_files) {
            Some(chunk) => chunk.files.push(file.clone()),
            None => {
                chunks.push(AiChunk { module: file.module.clone(), files: vec![file.clone()], api: Vec::new() });
            }
        }
    }
    for change in &report.api_changes {
        let module = module_of(&normalize_key(&change.path));
        match chunks.iter_mut().find(|c| c.module == module && c.api.len() < max_api) {
            Some(chunk) => chunk.api.push(change.clone()),
            None => {
                chunks.push(AiChunk { module, files: Vec::new(), api: vec![change.clone()] });
            }
        }
    }
    chunks
}

/// Cross-check the AI markdown against the static report: back-ticked
/// identifiers that appear nowhere in it are hallucination suspects. Shown as a
/// warning strip — never silently "fixes" the model, never blocks the report.
pub fn verify_ai_against_report(ai: &str, report: &ChangeReport) -> Vec<String> {
    let mut hay = String::new();
    for file in &report.files {
        hay.push_str(&file.path.to_lowercase());
        hay.push(' ');
        hay.push_str(&file.module.to_lowercase());
        hay.push(' ');
    }
    for change in &report.api_changes {
        hay.push_str(&change.name.to_lowercase());
        hay.push(' ');
        hay.push_str(&change.path.to_lowercase());
        hay.push(' ');
    }
    for commit in &report.commit_details {
        hay.push_str(&commit.subject.to_lowercase());
        hay.push(' ');
    }
    for group in &report.groups {
        for name in group.untested.iter().chain(group.layers.iter()) {
            hay.push_str(&name.to_lowercase());
            hay.push(' ');
        }
    }

    let mut suspects = Vec::new();
    let mut rest = ai;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else { break };
        let token = rest[..end].trim();
        rest = &rest[end + 1..];
        for word in token.split_whitespace() {
            let w = word.trim_matches(|c: char| !c.is_alphanumeric() && !"._/-".contains(c));
            if w.chars().count() < 4 {
                continue;
            }
            let ident_like = "._/-".chars().any(|sep| w.contains(sep))
                || w.chars().skip(1).any(|c| c.is_uppercase());
            if !ident_like {
                continue;
            }
            let lw = w.to_lowercase();
            let leaf = lw.rsplit('/').next().unwrap_or(&lw);
            let stem = leaf.split('.').next().unwrap_or(leaf);
            if hay.contains(&lw) || (stem.chars().count() >= 4 && hay.contains(stem)) {
                continue;
            }
            if !suspects.contains(&lw) && suspects.len() < 15 {
                suspects.push(lw);
            }
        }
    }
    suspects
}

/// Layers a unit test could plausibly exist for. Config/build/SQL have their own risk
/// tokens, so calling them a "test gap" would just be noise.
fn is_testable_layer(layer: &str) -> bool {
    matches!(
        layer,
        "api" | "service" | "persistence" | "logic" | "model" | "page" | "component" | "view" | "state" | "frontend"
    )
}

#[allow(clippy::too_many_arguments)]
fn classify(
    path: &str,
    status: &str,
    adds: u32,
    dels: u32,
    old_path: Option<String>,
    untracked: bool,
    known_test_paths: &HashSet<String>,
    inline_test_files: &HashSet<String>,
    signals: &BTreeMap<String, FileSignals>,
    file_commits: &BTreeMap<String, Vec<String>>,
) -> FileChange {
    let key = normalize_key(path);
    let layer = layer_of(path).to_string();
    let candidates = test_candidates(path);
    let test_path = matched_test(&candidates, known_test_paths);
    let is_test = layer == "test";
    let has_test = is_test || test_path.is_some() || inline_test_files.contains(&key);
    let file_signal = signals.get(&key).copied().unwrap_or_default();
    let (mut risks, hints) = risk_tokens_with(path, status, &layer, file_signal);
    if !is_test && is_code(path) && !has_test && is_testable_layer(&layer) {
        risks.push("untested".to_string());
    }
    // Renames keep their history under the *old* path in the log output, but the
    // report is filed under the new one; check both.
    let mut commits = file_commits.get(&key).cloned().unwrap_or_default();
    if let Some(old) = &old_path {
        for sha in file_commits.get(&normalize_key(old)).cloned().unwrap_or_default() {
            if !commits.contains(&sha) {
                commits.push(sha);
            }
        }
    }
    FileChange {
        path: path.to_string(),
        old_path,
        status: status.to_string(),
        adds,
        dels,
        module: module_of(&key),
        layer,
        risks,
        test_path,
        has_test,
        untracked,
        commits,
        hints,
    }
}

/// Everything `build_report` needs, so the call sites do not drown in positionals.
#[derive(Default)]
pub struct ReportInputs<'a> {
    pub base: &'a str,
    pub source: &'a str,
    /// Commit subjects, newest first (kept for the AI prompt and markdown export).
    pub subjects: Vec<String>,
    pub commit_details: Vec<CommitBrief>,
    /// normalize_key(path) -> short shas.
    pub file_commits: BTreeMap<String, Vec<String>>,
    pub name_status_z: &'a str,
    pub numstat_z: &'a str,
    pub untracked: &'a str,
    pub known_test_paths: HashSet<String>,
    pub inline_test_files: HashSet<String>,
    pub patch: &'a str,
    pub truncated: bool,
}

/// Assemble everything the UI shows. `untracked` is NUL-separated (`ls-files -z`),
/// because a quoted path would lose CJK file names.
pub fn build_report(inputs: &ReportInputs) -> ChangeReport {
    let ReportInputs {
        base,
        source,
        subjects,
        commit_details,
        file_commits,
        name_status_z,
        numstat_z,
        untracked,
        known_test_paths,
        inline_test_files,
        patch,
        truncated,
    } = inputs;
    let signals = patch_content_signals(patch);
    let mut ignored = 0u32;
    let mut counts: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for (adds, dels, old, new) in parse_numstat_z(numstat_z) {
        counts.insert(normalize_key(&new), (adds, dels));
        if let Some(old) = old {
            counts.insert(normalize_key(&old), (adds, dels));
        }
    }

    let mut files: Vec<FileChange> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (status, old, path) in parse_name_status_z(name_status_z) {
        let key = normalize_key(&path);
        if is_noise(&key) {
            ignored += 1;
            continue;
        }
        if !seen.insert(key.clone()) {
            continue;
        }
        let (adds, dels) = counts.get(&key).copied().unwrap_or((0, 0));
        files.push(classify(&path, &status, adds, dels, old, false, known_test_paths, inline_test_files, &signals, file_commits));
    }

    for path in untracked.split('\0').map(str::trim).filter(|l| !l.is_empty()) {
        let key = normalize_key(path);
        if is_noise(&key) {
            ignored += 1;
            continue;
        }
        if !seen.insert(key.clone()) {
            continue;
        }
        files.push(classify(path, "A", 0, 0, None, true, known_test_paths, inline_test_files, &signals, file_commits));
    }


    files.sort_by(|a, b| a.module.cmp(&b.module).then(a.path.cmp(&b.path)));

    let mut grouped: BTreeMap<String, ModuleGroup> = BTreeMap::new();
    for file in &files {
        let group = grouped.entry(file.module.clone()).or_insert_with(|| ModuleGroup {
            name: file.module.clone(),
            files: 0,
            adds: 0,
            dels: 0,
            layers: Vec::new(),
            risks: Vec::new(),
            untested: Vec::new(),
        });
        group.files += 1;
        group.adds += file.adds;
        group.dels += file.dels;
        if !group.layers.contains(&file.layer) {
            group.layers.push(file.layer.clone());
        }
        for risk in &file.risks {
            if !group.risks.contains(risk) {
                group.risks.push(risk.clone());
            }
        }
        if file.risks.iter().any(|r| r == "untested") {
            group.untested.push(file.path.clone());
        }
    }

    let layers: BTreeSet<String> = files.iter().map(|f| f.layer.clone()).collect();
    let risks: BTreeSet<String> = files.iter().flat_map(|f| f.risks.clone()).collect();
    let api_changes = api_surface_changes(patch);
    let code_files = files.iter().filter(|f| f.layer != "test" && f.layer != "docs" && is_code(&f.path)).count() as u32;
    let untested = files.iter().filter(|f| f.risks.iter().any(|r| r == "untested")).count() as u32;

    let scope = suggested_scope(&layers, &risks, &api_changes);
    ChangeReport {
        base: base.to_string(),
        source: source.to_string(),
        commits: subjects.clone(),
        commit_details: commit_details.clone(),
        stats: ChangeStats {
            files: files.len() as u32,
            adds: files.iter().map(|f| f.adds).sum(),
            dels: files.iter().map(|f| f.dels).sum(),
            modules: grouped.len() as u32,
            code_files,
            untested,
            test_files: files.iter().filter(|f| f.layer == "test").count() as u32,
            ignored,
        },
        groups: grouped.into_values().collect(),
        api_changes,
        scope,
        files,
        truncated: *truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| normalize_key(p)).collect()
    }

    #[test]
    fn java_module_layer_and_test_pairing() {
        let path = "pmys.saas.account.service/src/main/java/com/x/web/AccountController.java";
        assert_eq!(module_of(path), "pmys.saas.account.service");
        assert_eq!(layer_of(path), "api");
        assert_eq!(
            test_candidates(path),
            vec![
                "pmys.saas.account.service/src/test/java/com/x/web/AccountControllerTest.java",
                "pmys.saas.account.service/src/test/java/com/x/web/AccountControllerTests.java",
                "pmys.saas.account.service/src/test/java/com/x/web/AccountControllerIT.java",
            ]
        );
        assert_eq!(module_of("src/main/java/X.java"), "(root)");
        assert_eq!(module_of("README.md"), "(root)");
    }

    #[test]
    fn layers_separate_sql_config_build_and_tests() {
        assert_eq!(layer_of("app/src/main/resources/mapper/UserMapper.xml"), "sql");
        assert_eq!(layer_of("app/src/main/resources/application.yml"), "config");
        assert_eq!(layer_of("app/pom.xml"), "build");
        assert_eq!(layer_of("db/migration/V2__add_column.sql"), "migration");
        assert_eq!(layer_of("app/src/test/java/com/x/UserServiceTest.java"), "test");
        assert_eq!(layer_of("src/pages/user/List.vue"), "page");
        assert_eq!(layer_of("src/components/Filter.vue"), "component");
        assert_eq!(layer_of("src/api/user.ts"), "api");
        assert_eq!(layer_of("src/core/store.ts"), "state");
        assert_eq!(layer_of("src-tauri/src/test_commands.rs"), "api");
        assert_eq!(layer_of("internal/handler/order.go"), "api");
        assert_eq!(layer_of("docs/guide.md"), "docs");
        assert_eq!(layer_of("src/styles.css"), "style");
    }

    #[test]
    fn numstat_and_name_status_renames_parse_field_by_field() {
        // Real `git diff --name-status -z` / `--numstat -z` bytes for a rename.
        let name_status = "R100\0src/main/java/com/x/Big.java\0src/main/java/com/x/Renamed.java\0";
        let numstat = "0\t0\t\0src/main/java/com/x/Big.java\0src/main/java/com/x/Renamed.java\0";
        assert_eq!(
            parse_name_status_z(name_status),
            vec![("R".to_string(), Some("src/main/java/com/x/Big.java".to_string()), "src/main/java/com/x/Renamed.java".to_string())]
        );
        assert_eq!(
            parse_numstat_z(numstat),
            vec![(0, 0, Some("src/main/java/com/x/Big.java".to_string()), "src/main/java/com/x/Renamed.java".to_string())]
        );
        // A path containing a space and CJK characters stays one field.
        assert_eq!(
            parse_numstat_z("1\t1\tpages/用户 管理.vue\0"),
            vec![(1, 1, None, "pages/用户 管理.vue".to_string())]
        );
    }

    #[test]
    fn report_flags_missing_tests_and_groups_by_module() {
        let name_status = "M\0a-service/src/main/java/com/x/web/AController.java\0A\0a-service/src/main/java/com/x/web/B.java\0M\0b-service/src/test/java/com/x/BTest.java\0";
        let numstat = "1\t1\ta-service/src/main/java/com/x/web/AController.java\05\t0\tb-service/src/test/java/com/x/BTest.java\0";
        let known = set(&["a-service/src/test/java/com/x/web/AControllerTest.java"]);

        let report = build_report(&ReportInputs {
            name_status_z: name_status,
            numstat_z: numstat,
            untracked: "docs/note.md\0",
            known_test_paths: known.clone(),
            ..Default::default()
        });
        assert_eq!(report.stats.files, 4, "three tracked files plus one untracked");
        assert_eq!(report.stats.modules, 3, "two modules plus (root) for the doc");
        assert_eq!(report.stats.test_files, 1);

        let a = report.files.iter().find(|f| f.path.ends_with("AController.java")).unwrap();
        assert!(a.has_test && a.risks.is_empty(), "the paired test exists: {:?}", a.risks);
        assert_eq!(a.adds, 1);

        let b = report.files.iter().find(|f| f.path.ends_with("B.java")).unwrap();
        assert!(b.risks.contains(&"untested".to_string()));
        assert!(b.risks.contains(&"newFile".to_string()));

        let doc = report.files.iter().find(|f| f.path == "docs/note.md").unwrap();
        assert!(doc.untracked);
        assert!(
            !doc.risks.iter().any(|r| r == "untested"),
            "docs are not a test gap: {:?}",
            doc.risks
        );

        let group = report.groups.iter().find(|g| g.name == "b-service").unwrap();
        assert_eq!(group.untested, Vec::<String>::new(), "the changed test file is not a gap");
    }

    #[test]
    fn scope_follows_the_layers_and_risks_touched() {
        let mut layers = BTreeSet::new();
        layers.insert("api".to_string());
        layers.insert("persistence".to_string());
        let risks = ["migration".to_string()].into_iter().collect();
        let scope = suggested_scope(&layers, &risks, &[]);
        assert_eq!(scope, vec!["dataMigration", "dataAccess", "apiContract"]);

        let only_docs: BTreeSet<String> = ["docs".to_string()].into_iter().collect();
        assert_eq!(suggested_scope(&only_docs, &BTreeSet::new(), &[]), vec!["none"]);

        let removed = vec![ApiChange { kind: "removed".to_string(), name: "get(Long)".to_string(), path: "a/Api.java".to_string() }];
        assert!(suggested_scope(&BTreeSet::new(), &BTreeSet::new(), &removed).contains(&"apiContract".to_string()));
    }

    #[test]
    fn ide_and_build_noise_is_dropped_and_counted() {
        let name_status = "M\0target/classes/A.class\0A\0.idea/workspace.xml\0M\0app/src/main/java/com/x/App.java\0M\0src/styles.css\0";
        let report = build_report(&ReportInputs {
            name_status_z: name_status,
            ..Default::default()
        });
        assert_eq!(report.stats.files, 2, "only the source file and the stylesheet survive");
        assert_eq!(report.stats.ignored, 2);
        let css = report.files.iter().find(|f| f.path == "src/styles.css").unwrap();
        assert!(!css.risks.iter().any(|r| r == "untested"), "styles are not a test gap: {:?}", css.risks);
        // `src/bin` in Rust and a module named `target-service` are real code.
        assert!(!is_noise("src-tauri/src/bin/ai_workbench.rs"));
        assert!(!is_noise("target-service/src/main/java/X.java"));
        assert!(is_noise("app/target/classes/X.class"));
    }

    #[test]
    fn inline_rust_tests_count_as_coverage_without_self_pairing() {
        let name_status = "M\0src-tauri/src/lib.rs\0M\0src-tauri/src/tool_commands.rs\0";
        let inline = set(&["src-tauri/src/lib.rs"]);
        let report = build_report(&ReportInputs {
            name_status_z: name_status,
            inline_test_files: inline.clone(),
            ..Default::default()
        });
        let paired = report.files.iter().find(|f| f.path == "src-tauri/src/lib.rs").unwrap();
        assert!(paired.has_test && paired.test_path.is_none(), "inline module tests, no sibling file: {:?}", paired);
        let missing = report.files.iter().find(|f| f.path.ends_with("tool_commands.rs")).unwrap();
        assert!(missing.risks.contains(&"untested".to_string()));
    }

    #[test]
    fn api_surface_distinguishes_added_removed_and_mere_moves() {
        let patch = "--- a/src/main/java/com/x/web/AController.java\n+++ b/src/main/java/com/x/web/AController.java\n@@\n-public String findUser(Long id) {\n+public String findUser(Long id) {\n+public void deleteUser(Long id) {\n@@\n";
        let changes = api_surface_changes(patch);
        assert_eq!(changes.len(), 1, "a line that moved is not an API change: {:?}", changes);
        assert_eq!(changes[0].kind, "added");
        assert_eq!(changes[0].name, "deleteUser(Long)");

        let go = api_surface_changes("--- a/x.go\n+++ b/x.go\n@@\n-func LoadDB(ctx context.Context, dsn string) error {\n");
        assert_eq!(go[0].kind, "removed");
        assert_eq!(go[0].name, "LoadDB(context.Context, string)");
        assert_eq!(go[0].path, "x.go");

        let unexported = api_surface_changes("--- a/x.go\n+++ b/x.go\n@@\n-func loadDB(ctx context.Context) error {\n");
        assert!(unexported.is_empty(), "unexported go helpers are not a public surface");

        let receiver = api_surface_changes("--- a/svc.go\n+++ b/svc.go\n@@\n-func (s *Svc) Exported(ctx context.Context) error {\n+func (s *Svc) Exported(ctx context.Context, extra string) error {\n");
        assert_eq!(receiver.len(), 1);
        assert_eq!(receiver[0].kind, "changed");
        assert_eq!(receiver[0].name, "Exported(context.Context ⇒ context.Context, string)");
    }

    #[test]
    fn a_type_change_with_same_arity_is_a_signature_change() {
        // The old "N args" shape saw both sides as "1 args" and stayed silent.
        let changes = api_surface_changes(
            "--- a/S.java\n+++ b/S.java\n@@\n-public String f(Long a) {\n+public String f(String a) {\n",
        );
        assert_eq!(changes.len(), 1, "type flip must surface: {:?}", changes);
        assert_eq!(changes[0].kind, "changed");
        assert_eq!(changes[0].name, "f(Long ⇒ String)");
    }

    #[test]
    fn a_return_only_change_shows_the_return_types_not_a_blank_move() {
        let changes = api_surface_changes(
            "--- a/S.java\n+++ b/S.java\n@@\n-public String f(Long a) {\n+public int f(Long a) {\n",
        );
        assert_eq!(changes.len(), 1, "same params, moved return type: {:?}", changes);
        assert_eq!(changes[0].name, "f(Long) 返回 String ⇒ int");

        let to_unit = api_surface_changes(
            "--- a/l.rs\n+++ b/l.rs\n@@\n-pub fn load(cfg: Config) -> Result<()> {\n+pub fn load(cfg: Config) {\n",
        );
        assert_eq!(to_unit[0].name, "load(Config) 返回 Result<()> ⇒ -");
    }

    #[test]
    fn generic_parameters_do_not_split_on_inner_commas() {
        let changes = api_surface_changes(
            "--- a/S.java\n+++ b/S.java\n@@\n+public void f(Map<String, String> m, int n) {\n",
        );
        assert_eq!(changes[0].name, "f(Map<String, String>, int)");
    }

    #[test]
    fn security_and_money_tokens_need_word_or_content_evidence() {
        // Word match: strong, no matter what the lines say.
        let (risks, hints) = risk_tokens_with("src/auth/session.ts", "M", "logic", FileSignals::default());
        assert!(risks.iter().any(|r| r == "security"));
        assert!(hints.is_empty());
        // Substring-only: demoted to a hint — "tokenizer" is not an auth surface.
        let (risks, hints) = risk_tokens_with("src/utils/tokenizer.ts", "M", "logic", FileSignals::default());
        assert!(!risks.iter().any(|r| r == "security"), "got {:?}", risks);
        assert_eq!(hints, vec!["security".to_string()]);
        // Added-line content promotes even a neutrally named file.
        let (risks, _) = risk_tokens_with(
            "src/core/settings.ts",
            "M",
            "logic",
            FileSignals { security: true, money: false },
        );
        assert!(risks.iter().any(|r| r == "security"));
        // "ordered-list.css": substring "order" only ever earns a hint.
        let (risks, hints) = risk_tokens_with("src/styles/ordered-list.css", "M", "style", FileSignals::default());
        assert!(!risks.iter().any(|r| r == "money"), "got {:?}", risks);
        assert_eq!(hints, vec!["money".to_string()]);
    }

    #[test]
    fn patch_lines_detect_secret_and_money_content() {
        let patch = "--- a/x.ts\n+++ b/x.ts\n@@\n+const apiKey = \"sk-live-9f3k\";\n+let balance = account.balance;\n";
        let signals = patch_content_signals(patch);
        let s = signals.get("x.ts").copied().expect("keyed by the b/ path, lowercased");
        assert!(s.security, "api key line: {:?}", signals.keys().collect::<Vec<_>>());
        assert!(s.money, "balance line");
    }

    #[test]
    fn log_records_carry_briefs_and_file_attribution() {
        let raw = "\u{2}abc123def\u{0}abc123d\u{0}张三\u{0}2026-09-20T10:00:00+08:00\u{0}fix: 支付回调超时\u{0}body 第二行\u{1}\napp/src/Pay.ts\u{0}app/src/Order.ts\u{0}\u{2}999aaa\u{0}999aaa\u{0}li\u{0}2026-09-21T09:00:00+08:00\u{0}chore: deps\u{0}\u{1}\napp/pom.xml\u{0}";
        let (briefs, by_file) = parse_log_z(raw);
        assert_eq!(briefs.len(), 2);
        assert_eq!(briefs[0].author, "张三");
        assert_eq!(briefs[0].body, "body 第二行");
        assert_eq!(briefs[1].subject, "chore: deps");
        assert_eq!(by_file["app/src/Pay.ts"], vec!["abc123d".to_string()]);
        assert_eq!(by_file["app/src/Order.ts"], vec!["abc123d".to_string()]);
        assert_eq!(by_file["app/pom.xml"], vec!["999aaa".to_string()]);
    }

    #[test]
    fn commit_attribution_lands_on_report_files() {
        let name_status = "M\0app/src/Pay.ts\0";
        let file_commits = [(
            "app/src/pay.ts".to_string(),
            vec!["abc123d".to_string()],
        )]
        .into_iter()
        .collect();
        let report = build_report(&ReportInputs {
            name_status_z: name_status,
            file_commits,
            ..Default::default()
        });
        assert_eq!(report.files[0].commits, vec!["abc123d"]);
    }

    #[test]
    fn verify_ai_flags_invented_identifiers_but_not_report_terms() {
        let report = build_report(&ReportInputs {
            name_status_z: "M\0app/src/web/PayController.java\0",
            patch: "--- a/app/src/web/PayController.java\n+++ b/app/src/web/PayController.java\n@@\n+public void refundOrder(Long id) {\n",
            ..Default::default()
        });
        let ai = "改动了 `PayController.java`，新增 `refundOrder`；但 `GhostService.kt` 与 `totallyMadeUpFn` 并不存在。";
        let suspects = verify_ai_against_report(ai, &report);
        assert!(suspects.contains(&"ghostservice.kt".to_string()), "got {:?}", suspects);
        assert!(suspects.contains(&"totallymadeupfn".to_string()), "got {:?}", suspects);
        assert!(!suspects.iter().any(|s| s.contains("paycontroller") || s.contains("refundorder")), "real names must not be flagged: {:?}", suspects);
    }

    #[test]
    fn plan_ai_chunks_packs_modules_and_keeps_every_api_change() {
        let mut name_status = String::new();
        for i in 0..25 {
            name_status.push_str(&format!("M\0svc/src/main/java/A{}.java\0", i));
        }
        name_status.push_str("M\0web/src/App.tsx\0");
        let mut report = build_report(&ReportInputs {
            name_status_z: &name_status,
            ..Default::default()
        });
        report.api_changes = vec![ApiChange { kind: "added".to_string(), name: "f()".to_string(), path: "svc/src/main/java/A1.java".to_string() }];

        let chunks = plan_ai_chunks(&report, 20, 3);
        assert_eq!(chunks.len(), 3, "25 svc files split 20+5, web is its own chunk");
        assert_eq!((chunks[0].module.as_str(), chunks[0].files.len()), ("svc", 20));
        assert_eq!(chunks[1].files.len(), 5);
        // The api change lands in the first chunk of its own module.
        assert_eq!(chunks[0].api.len(), 1);
        let packed: usize = chunks.iter().map(|c| c.files.len()).sum();
        assert_eq!(packed, report.files.len(), "no file is dropped by packing");
        let api: usize = chunks.iter().map(|c| c.api.len()).sum();
        assert_eq!(api, report.api_changes.len());
    }

    #[test]
    fn deleted_files_keep_their_signatures_attributed() {
        let changes = api_surface_changes("diff --git a/src/main/java/com/x/Gone.java b/src/main/java/com/x/Gone.java\n--- a/src/main/java/com/x/Gone.java\n+++ /dev/null\n@@\n-public void cancelOrder(Long id) {\n");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "removed");
        assert_eq!(changes[0].path, "src/main/java/com/x/Gone.java");
    }
}
