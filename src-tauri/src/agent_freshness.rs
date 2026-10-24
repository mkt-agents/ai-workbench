//! Workspace freshness fingerprint: did the files change since the workbench context
//! was collected? The AI hub caches that context for two minutes, so an edit made in
//! the editor (or a branch switch) must not be answered from the stale copy.
//!
//! Cheap on purpose. In a git repository it is one `rev-parse --is-inside-work-tree`,
//! one `status --porcelain` and the mtimes of the dirty paths; anywhere else a bounded
//! directory walk of `(path, mtime, size)`. Nothing reads file contents.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Files visited by the non-git walk before giving up (dependencies folders are skipped).
const MAX_SCAN_FILES: usize = 3_000;
/// Directories that never tell us anything about the project's own source.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".next",
    ".cache",
    ".turbo",
    ".venv",
    "venv",
    "__pycache__",
    "node_modules",
    "target",
    "dist",
    "build",
];
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFingerprint {
    /// Hex digest; empty when the path is not a readable directory.
    pub fingerprint: String,
    /// Files that went into the digest (0 on the git path, where git decides).
    pub files: usize,
    /// The path is inside a git work tree, so the digest covers HEAD + status.
    pub git: bool,
}

#[tauri::command]
pub async fn workspace_fingerprint(path: String) -> Result<WorkspaceFingerprint, String> {
    tokio::task::spawn_blocking(move || Ok(fingerprint(&path)))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

pub(crate) fn fingerprint(path: &str) -> WorkspaceFingerprint {
    let root = Path::new(path);
    if !root.is_dir() {
        return WorkspaceFingerprint { fingerprint: String::new(), files: 0, git: false };
    }
    if let Some(digest) = git_fingerprint(path) {
        return WorkspaceFingerprint { fingerprint: digest, files: 0, git: true };
    }
    let (digest, files) = walk_fingerprint(root);
    WorkspaceFingerprint { fingerprint: digest, files, git: false }
}

fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// `mtime:size` of one file — the whole point is to notice a save without reading it.
fn stamp(meta: &std::fs::Metadata) -> Vec<u8> {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format!("{mtime}:{}", meta.len()).into_bytes()
}

fn git_fingerprint(path: &str) -> Option<String> {
    let inside = crate::git_commands::git_stdout(path, &["rev-parse", "--is-inside-work-tree"]).ok()?;
    if inside.trim() != "true" {
        return None;
    }
    // An empty repository has no HEAD yet; the status output still describes it.
    let head = crate::git_commands::git_stdout(path, &["rev-parse", "HEAD"]).unwrap_or_default();
    let status =
        crate::git_commands::git_stdout(path, &["status", "--porcelain=v1"]).unwrap_or_default();
    let mut hash = fnv1a(head.trim().as_bytes(), FNV_OFFSET);
    hash = fnv1a(status.as_bytes(), hash);
    // `status` alone cannot tell "still the same dirty file" from "dirty file edited again":
    // the mtime of every path it names does.
    for line in status.lines() {
        let raw = line.get(3..).unwrap_or("").trim().trim_matches('"');
        let target = raw.rsplit(" -> ").next().unwrap_or(raw);
        let Ok(meta) = std::fs::metadata(Path::new(path).join(target)) else {
            continue;
        };
        hash = fnv1a(target.as_bytes(), hash);
        hash = fnv1a(&stamp(&meta), hash);
    }
    Some(format!("{hash:016x}"))
}

fn walk_fingerprint(root: &Path) -> (String, usize) {
    let mut hash = FNV_OFFSET;
    let mut files = 0usize;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        children.sort_by_key(|e| e.file_name());
        for entry in children {
            if files >= MAX_SCAN_FILES {
                return (format!("{hash:016x}"), files);
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(entry.path());
                }
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string().replace('\\', "/");
            hash = fnv1a(rel.as_bytes(), hash);
            hash = fnv1a(&stamp(&meta), hash);
            files += 1;
        }
    }
    (format!("{hash:016x}"), files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aw-fp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_plain_folder_digest_changes_when_a_file_changes() {
        let root = temp_dir("plain");
        std::fs::write(root.join("a.txt"), "one").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("b.rs"), "fn b() {}").unwrap();

        let path = root.display().to_string();
        let first = fingerprint(&path);
        assert!(!first.git);
        assert_eq!(first.files, 2);
        assert_eq!(first.fingerprint.len(), 16);
        // Unchanged tree → identical digest.
        assert_eq!(fingerprint(&path).fingerprint, first.fingerprint);

        // Content change with a new size is always visible; touch alone may not be
        // (mtime granularity), so the assertion uses a real write.
        std::fs::write(root.join("src").join("b.rs"), "fn b() { /* edited */ }").unwrap();
        assert_ne!(fingerprint(&path).fingerprint, first.fingerprint);

        // Build folders are skipped: a rebuilt dependency tree is not a context change.
        let before_deps = fingerprint(&path).fingerprint;
        std::fs::create_dir_all(root.join("node_modules").join("pkg")).unwrap();
        std::fs::write(root.join("node_modules").join("pkg").join("index.js"), "x").unwrap();
        assert_eq!(fingerprint(&path).fingerprint, before_deps);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unreadable_paths_get_an_empty_digest() {
        let out = fingerprint("Z:\\definitely-not-here-aw-fp");
        assert_eq!(out.fingerprint, "");
        assert!(!out.git);
    }

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(fnv1a(b"", FNV_OFFSET), FNV_OFFSET);
        assert_ne!(fnv1a(b"a", FNV_OFFSET), fnv1a(b"b", FNV_OFFSET));
    }
}
