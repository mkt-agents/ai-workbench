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

/// Risk tokens per file. Rendered as badges, and read by `suggested_scope`.
pub fn risk_tokens(path: &str, status: &str, layer: &str) -> Vec<String> {
    let mut risks = Vec::new();
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
    if lower.contains("/security/") || lower.contains("auth") || lower.contains("login") || lower.contains("password") || lower.contains("token") {
        risks.push("security".to_string());
    }
    if lower.contains("/pay") || lower.contains("price") || lower.contains("amount") || lower.contains("order") {
        risks.push("money".to_string());
    }
    risks
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
    let mut removed: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut added: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
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
        let Some((name, shape)) = signature_of(&file, &line[1..]) else { continue };
        if name.is_empty() || file.is_empty() {
            continue;
        }
        if line.starts_with('+') {
            added.entry(name).or_default().push((shape, file.clone()));
        } else {
            removed.entry(name).or_default().push((shape, file.clone()));
        }
    }

    let names: BTreeSet<String> = added.keys().chain(removed.keys()).cloned().collect();
    let mut out = Vec::new();
    for name in names {
        let adds = added.get(&name).cloned().unwrap_or_default();
        let dels = removed.get(&name).cloned().unwrap_or_default();
        // The same signature on both sides is a moved line, not a changed contract.
        let fresh_adds: Vec<(String, String)> = adds.iter().filter(|entry| !dels.contains(entry)).cloned().collect();
        let fresh_dels: Vec<(String, String)> = dels.iter().filter(|entry| !adds.contains(entry)).cloned().collect();
        let (kind, label) = match (fresh_dels.first(), fresh_adds.first()) {
            (Some(old), Some(new)) => ("changed", format!("{}({} → {})", name, old.0, new.0)),
            (Some(old), None) => ("removed", format!("{}({})", name, old.0)),
            (None, Some(new)) => ("added", format!("{}({})", name, new.0)),
            (None, None) => continue,
        };
        let path = match (fresh_dels.first(), fresh_adds.first()) {
            (_, Some(new)) => new.1.clone(),
            (Some(old), None) => old.1.clone(),
            (None, None) => continue,
        };
        out.push(ApiChange { kind: kind.to_string(), name: label, path });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.name.cmp(&b.name)));
    out
}

/// Keep only public entry points, normalised to (name, parameter shape) so a pure
/// reformat does not read as a signature change.
fn signature_of(path: &str, body: &str) -> Option<(String, String)> {
    let ext = extension(path);
    let trimmed = body.trim();
    let declaration = match ext.as_str() {
        "java" | "kt" | "kts" | "scala" => {
            let rest = trimmed.strip_prefix("public ")?.trim_start_matches("static ");
            if !rest.contains('(') || rest.starts_with("class ") || rest.starts_with("interface ") {
                return None;
            }
            let head = rest.split('(').next()?;
            let name = head.split_whitespace().last()?;
            (name.to_string(), rest)
        }
        "rs" => {
            let rest = trimmed.strip_prefix("pub fn ")?;
            (rest.split('(').next()?.trim().to_string(), rest)
        }
        "go" => {
            let rest = trimmed.strip_prefix("func ")?;
            // `func (s *Svc) Handle(...)` — skip the receiver to reach the name.
            let after = match rest.strip_prefix('(') {
                Some(stripped) => stripped.split_once(')')?.1.trim_start(),
                None => rest,
            };
            let name = after.split('(').next()?.trim();
            if name.is_empty() || !name.chars().next()?.is_uppercase() {
                return None; // unexported
            }
            (name.to_string(), after)
        }
        "ts" | "tsx" | "js" | "jsx" => {
            let rest = trimmed
                .strip_prefix("export async function ")
                .or_else(|| trimmed.strip_prefix("export function "))
                .or_else(|| trimmed.strip_prefix("export const "))?;
            (rest.split(['(', ' ', '=']).next()?.to_string(), rest)
        }
        "vue" | "svelte" => {
            let rest = trimmed.strip_prefix("export function ")?;
            (rest.split('(').next()?.to_string(), rest)
        }
        _ => return None,
    };
    let (name, params) = declaration;
    if name.is_empty() {
        return None;
    }
    Some((name, param_shape(params)?))
}

/// Arity plus rough parameter types, without depending on a real parser.
fn param_shape(rest: &str) -> Option<String> {
    let inner = rest.split_once('(')?.1.rsplit_once(')')?.0.trim();
    if inner.is_empty() {
        return Some(String::new());
    }
    let count = inner.split(',').count();
    Some(format!("{} args", count))
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
) -> FileChange {
    let key = normalize_key(path);
    let layer = layer_of(path).to_string();
    let candidates = test_candidates(path);
    let test_path = matched_test(&candidates, known_test_paths);
    let is_test = layer == "test";
    let has_test = is_test || test_path.is_some() || inline_test_files.contains(&key);
    let mut risks = risk_tokens(path, status, &layer);
    if !is_test && is_code(path) && !has_test && is_testable_layer(&layer) {
        risks.push("untested".to_string());
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
    }
}

/// Assemble everything the UI shows. `untracked` is NUL-separated (`ls-files -z`),
/// because a quoted path would lose CJK file names.
pub fn build_report(
    base: &str,
    source: &str,
    commits: Vec<String>,
    name_status_z: &str,
    numstat_z: &str,
    untracked: &str,
    known_test_paths: &HashSet<String>,
    inline_test_files: &HashSet<String>,
    patch: &str,
    truncated: bool,
) -> ChangeReport {
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
        files.push(classify(&path, &status, adds, dels, old, false, known_test_paths, inline_test_files));
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
        files.push(classify(path, "A", 0, 0, None, true, known_test_paths, inline_test_files));
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
        commits,
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
        truncated,
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

        let report = build_report("", "uncommitted", vec![], name_status, numstat, "docs/note.md\0", &known, &set(&[]), "", false);
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
        let report = build_report("", "uncommitted", vec![], name_status, "", "", &set(&[]), &set(&[]), "", false);
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
        let report = build_report("", "uncommitted", vec![], name_status, "", "", &set(&[]), &inline, "", false);
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
        assert_eq!(changes[0].name, "deleteUser(1 args)");

        let go = api_surface_changes("--- a/x.go\n+++ b/x.go\n@@\n-func LoadDB(ctx context.Context, dsn string) error {\n");
        assert_eq!(go[0].kind, "removed");
        assert_eq!(go[0].name, "LoadDB(2 args)");
        assert_eq!(go[0].path, "x.go");

        let unexported = api_surface_changes("--- a/x.go\n+++ b/x.go\n@@\n-func loadDB(ctx context.Context) error {\n");
        assert!(unexported.is_empty(), "unexported go helpers are not a public surface");

        let receiver = api_surface_changes("--- a/svc.go\n+++ b/svc.go\n@@\n-func (s *Svc) Exported(ctx context.Context) error {\n+func (s *Svc) Exported(ctx context.Context, extra string) error {\n");
        assert_eq!(receiver.len(), 1);
        assert_eq!(receiver[0].kind, "changed");
        assert_eq!(receiver[0].name, "Exported(1 args → 2 args)");
    }

    #[test]
    fn deleted_files_keep_their_signatures_attributed() {
        let changes = api_surface_changes("diff --git a/src/main/java/com/x/Gone.java b/src/main/java/com/x/Gone.java\n--- a/src/main/java/com/x/Gone.java\n+++ /dev/null\n@@\n-public void cancelOrder(Long id) {\n");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, "removed");
        assert_eq!(changes[0].path, "src/main/java/com/x/Gone.java");
    }
}
