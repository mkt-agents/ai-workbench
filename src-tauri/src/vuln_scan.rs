//! One vulnerability scan of one project: lock files → OSV, tracked files → secrets.
//!
//! Deliberately tauri-free: `lib.rs` wires progress events and the database, this
//! module only reads the filesystem, talks to OSV and returns what it found. That
//! keeps the whole pipeline testable and keeps the DB lock out of the network wait.
//!
//! Scope decisions that are *not* accidents: maven dependency auditing is refused
//! (parent/BOM inheritance cannot be resolved from text, and NVD-sized local data
//! is not something a desktop app ships), git-history secret scanning is out of v1,
//! and the dependency source of truth is always a lock file — an exact pin or
//! nothing, because a guessed version produces confidently wrong advisories.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::vuln_lock::{self, DepCoord};
use crate::vuln_osv::{self, OsvVuln};
use crate::vuln_secrets::{self, SecretHit};

pub const OSV_BATCH_URL: &str = "https://api.osv.dev/v1/querybatch";
const OSV_REQUEST_TIMEOUT_SECS: u64 = 45;
/// Larger than this and it is generated data, not source.
const MAX_SCAN_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VulnScanOutcome {
    /// `success | failed | cancelled | error` — the run vocabulary, no new words.
    pub status: String,
    pub error_kind: String,
    /// What to tell the tester when the run did not fully deliver.
    pub error: Option<String>,
    /// e.g. "依赖审计暂不支持 maven"; a note, not a failure.
    pub unsupported: Option<String>,
    pub deps_checked: usize,
    pub files_checked: usize,
    pub files_skipped: usize,
    pub critical: usize,
    pub high: usize,
    pub total: usize,
    pub vulns: Vec<OsvVuln>,
    pub secrets: Vec<SecretHit>,
}

impl Default for VulnScanOutcome {
    fn default() -> Self {
        VulnScanOutcome {
            status: "success".to_string(),
            error_kind: String::new(),
            error: None,
            unsupported: None,
            deps_checked: 0,
            files_checked: 0,
            files_skipped: 0,
            critical: 0,
            high: 0,
            total: 0,
            vulns: Vec::new(),
            secrets: Vec::new(),
        }
    }
}

/// Which lock files this project offers, and the parsed coordinates. `unsupported`
/// carries the "why not" when there is nothing we trust.
pub fn collect_dependencies(dir: &Path) -> Result<(Vec<DepCoord>, Option<String>), String> {
    let mut deps = BTreeSet::new();
    let mut sources = 0usize;
    let mut unsupported = None;

    if dir.join("package-lock.json").is_file() {
        let content = std::fs::read_to_string(dir.join("package-lock.json"))
            .map_err(|e| format!("读取 package-lock.json 失败: {}", e))?;
        deps.extend(vuln_lock::parse_package_lock(&content)?);
        sources += 1;
    }
    if dir.join("Cargo.lock").is_file() {
        let content = std::fs::read_to_string(dir.join("Cargo.lock"))
            .map_err(|e| format!("读取 Cargo.lock 失败: {}", e))?;
        deps.extend(vuln_lock::parse_cargo_lock(&content));
        sources += 1;
    }
    if dir.join("requirements.txt").is_file() {
        let content = std::fs::read_to_string(dir.join("requirements.txt"))
            .map_err(|e| format!("读取 requirements.txt 失败: {}", e))?;
        deps.extend(vuln_lock::parse_requirements_txt(&content));
        sources += 1;
    }
    if sources == 0 {
        if dir.join("pom.xml").is_file() || dir.join("build.gradle").is_file() || dir.join("build.gradle.kts").is_file() {
            unsupported = Some("依赖审计暂不支持 maven/gradle（parent/BOM 继承无法从文本可靠解析）；密钥扫描仍会照常进行".to_string());
        } else {
            unsupported = Some("未找到可解析的锁文件（package-lock.json / Cargo.lock / requirements.txt），跳过依赖审计".to_string());
        }
    }
    Ok((deps.into_iter().collect(), unsupported))
}

/// The file list the secret scan may read is produced by `collect_tracked_files`
/// in `vuln_commands` (it needs git, so it lives with the command layer); this
/// module only ever sees the resulting relative paths.

/// Generated trees, other lock files and binaries carry nothing but false positives.
pub fn is_scan_candidate(rel_path: &str) -> bool {
    let lower = rel_path.replace('\\', "/").to_lowercase();
    if lower.ends_with(".min.js") || lower.ends_with(".min.css") || lower.ends_with(".map") {
        return false;
    }
    const DENY_PARTS: &[&str] = &[
        "node_modules/", "dist/", "build/", "target/", "out/", "coverage/", "testdata/",
        "__snapshots__/", "vendor/", ".git/",
    ];
    if DENY_PARTS.iter().any(|p| lower.contains(p)) {
        return false;
    }
    const DENY_SUFFIXES: &[&str] = &[
        "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "cargo.lock", "go.sum", "cargo.toml.lock", ".lock",
        ".png", ".jpg", ".jpeg", ".gif", ".ico", ".webp", ".pdf", ".zip", ".gz", ".woff", ".woff2", ".ttf",
        ".eot", ".jar", ".war", ".class", ".exe", ".dll", ".so", ".dylib", ".mp4", ".mp3", ".png",
    ];
    if DENY_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
        return false;
    }
    true
}

/// Read + scan each candidate; `progress(done, total)` runs after every file.
pub fn scan_secrets(dir: &Path, files: &[String], mut progress: impl FnMut(usize, usize) + Send) -> (Vec<SecretHit>, usize, usize) {
    let mut hits = Vec::new();
    let mut checked = 0usize;
    let mut skipped = 0usize;
    let total = files.len();
    for (index, rel) in files.iter().enumerate() {
        if !is_scan_candidate(rel) {
            skipped += 1;
            progress(index + 1, total);
            continue;
        }
        let full = dir.join(rel);
        let over_size = std::fs::metadata(&full).map(|m| m.len() > MAX_SCAN_FILE_BYTES).unwrap_or(true);
        let content = if over_size { None } else { std::fs::read(&full).ok() };
        let Some(content) = content else {
            skipped += 1;
            progress(index + 1, total);
            continue;
        };
        // Binary sniff before any UTF-8 ceremony: a NUL in the head means generated.
        if content[..content.len().min(8192)].contains(&0) {
            skipped += 1;
            progress(index + 1, total);
            continue;
        }
        let text = String::from_utf8_lossy(&content);
        let mut file_hits = vuln_secrets::scan_text(rel, &text);
        hits.append(&mut file_hits);
        checked += 1;
        progress(index + 1, total);
    }
    (hits, checked, skipped)
}

/// How many advisory details one scan will fetch before disclosing truncation,
/// and how many in flight. 8 keeps a home line busy without courting a 429.
const DETAIL_CAP: usize = 300;
const DETAIL_CONCURRENCY: usize = 8;

/// Two-phase as the live API demands: batch POSTs for id stubs (the batch
/// answer carries *no* summary/severity), then `/v1/vulns/<id>` per advisory.
/// `cancelled` is polled between requests; progress covers both phases.
pub async fn query_osv(
    deps: &[DepCoord],
    cancelled: impl Fn() -> bool,
    mut progress: impl FnMut(usize, usize) + Send,
) -> Result<(Vec<OsvVuln>, bool), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(OSV_REQUEST_TIMEOUT_SECS))
        .user_agent("ai-workbench/vulnerability-audit")
        .build()
        .map_err(|e| format!("[E_NETWORK] 无法创建 HTTP 客户端：{}", e))?;

    // Phase 1: (dependency index, advisory id) pairs, aligned per chunk.
    let bodies = vuln_osv::build_batch_requests(deps);
    let mut wanted: Vec<(usize, String)> = Vec::new();
    for (chunk, body) in bodies.iter().enumerate() {
        if cancelled() {
            return Err("[E_CANCELLED] 扫描已取消".to_string());
        }
        let response = client
            .post(OSV_BATCH_URL)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("[E_NETWORK] OSV 请求失败：{}", e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("[E_NETWORK] OSV 返回 {}（稍后重试或检查网络）", status.as_u16()));
        }
        let text = response.text().await.map_err(|e| format!("[E_NETWORK] 读取 OSV 响应失败：{}", e))?;
        let start = chunk * vuln_osv::MAX_QUERIES_PER_REQUEST;
        for (offset, ids) in vuln_osv::parse_batch_ids(&text)?.into_iter().enumerate() {
            let dep_index = start + offset;
            if deps.get(dep_index).is_some() {
                wanted.extend(ids.into_iter().map(|id| (dep_index, id)));
            }
        }
        progress(chunk + 1, bodies.len());
    }

    // Phase 2: details, capped so a mega-lock project still finishes; the cap
    // is reported through `unsupported` by the caller (see vuln_commands).
    let mut out = Vec::new();
    let total = wanted.len().min(DETAIL_CAP);
    let truncated = wanted.len() > DETAIL_CAP;
    wanted.sort_by(|a, b| a.1.cmp(&b.1));
    wanted.truncate(DETAIL_CAP);
    let mut done = 0usize;
    for slot in wanted.chunks(DETAIL_CONCURRENCY) {
        if cancelled() {
            return Err("[E_CANCELLED] 扫描已取消".to_string());
        }
        let texts = futures_util::future::join_all(slot.iter().map(|(_index, id)| {
            let client = client.clone();
            let url = vuln_osv::detail_url(id);
            async move {
                let response = client.get(&url).send().await.map_err(|e| e.to_string())?;
                let response = response.error_for_status().map_err(|e| e.to_string())?;
                response.text().await.map_err(|e| e.to_string())
            }
        }))
        .await;
        for ((dep_index, id), text) in slot.iter().zip(texts) {
            done += 1;
            let Ok(body) = text else { continue }; // one vanished advisory must not kill the scan
            let Some(dep) = deps.get(*dep_index) else { continue };
            if let Ok(mut vuln) = vuln_osv::parse_vuln_record(&body, dep) {
                if vuln.id.is_empty() {
                    vuln.id = id.clone();
                }
                out.push(vuln);
            }
        }
        progress(done, total);
    }
    Ok((out, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aiwb-vuln-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn candidate_filter_drops_generated_trees_and_keeps_sources() {
        assert!(is_scan_candidate("src/main.rs"));
        assert!(is_scan_candidate("app/src/service.ts"));
        assert!(!is_scan_candidate("web/node_modules/acorn/index.js"));
        assert!(!is_scan_candidate("app/dist/bundle.js"));
        assert!(!is_scan_candidate("web/assets/app.min.js"));
        assert!(!is_scan_candidate("Cargo.lock"));
        assert!(!is_scan_candidate("web/package-lock.json"));
        assert!(!is_scan_candidate("src/test/testdata/big.json"));
        assert!(!is_scan_candidate("docs/图片.PNG"));
        assert!(!is_scan_candidate("public/logo.png"));
    }

    #[test]
    fn locks_are_discovered_or_refused_with_a_reason() {
        let dir = scratch("locks");
        fs::write(dir.join("package-lock.json"), r#"{"packages":{"node_modules/a":{"version":"1.0.0"}}}"#).unwrap();
        fs::write(dir.join("Cargo.lock"), "[[package]]\nname = \"x\"\nversion = \"0.1.0\"\n").unwrap();
        let (deps, unsupported) = collect_dependencies(&dir).unwrap();
        assert!(unsupported.is_none());
        assert_eq!(deps.len(), 2);

        let bare = scratch("locks-bare");
        let (deps, unsupported) = collect_dependencies(&bare).unwrap();
        assert!(deps.is_empty());
        assert!(unsupported.unwrap().contains("锁文件"));

        let java = scratch("locks-java");
        fs::write(java.join("pom.xml"), "<project/>").unwrap();
        let (_, unsupported) = collect_dependencies(&java).unwrap();
        assert!(unsupported.unwrap().contains("maven"));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&bare);
        let _ = fs::remove_dir_all(&java);
    }

    #[test]
    fn secrets_are_scanned_over_the_file_list_with_binary_skips() {
        let dir = scratch("secrets");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/db.ts"),
            "const apiKey = \"sk-9f3kQ2zLm8XbVt4YeWp\";\nconsole.log(1);\n",
        )
        .unwrap();
        fs::write(dir.join("src/logo.png"), [0u8, 1, 2, 0, 3]).unwrap();
        let mut seen = 0usize;
        let (hits, checked, skipped) = scan_secrets(&dir, &["src/db.ts".to_string(), "src/logo.png".to_string(), "package-lock.json".to_string()], |done, total| {
            seen = done;
            assert!(done <= total);
        });
        assert_eq!(checked, 1);
        assert_eq!(skipped, 2);
        assert_eq!(hits.len(), 1, "got {:?}", hits);
        assert_eq!(hits[0].file, "src/db.ts");
        assert_eq!(seen, 3);
        let _ = fs::remove_dir_all(&dir);
    }
}
