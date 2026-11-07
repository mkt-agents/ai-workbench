//! 文档任务工作区辅助：解析文档仓、来源仓只读快照、变更越界校验。
//! 文档生成直接写入原仓 doc 目录；此处不再创建「写隔离」worktree。

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::git_commands::{git_output, git_stdout};

static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

fn git(repo: &str, args: &[&str]) -> Result<String, String> {
    git_stdout(repo, args)
}

/// 父目录可能是多仓容器：优先按文档目录定位真实仓库，再退回父目录本身。
/// 不把“父目录有多个子仓”误判成文档仓库。
pub(super) fn resolve_doc_repo(workspace: &str, doc_dir: &str) -> Result<String, String> {
    use crate::git_commands::{canonicalize_for_process, strip_verbatim_prefix};
    if crate::git_commands::ensure_git_repo(workspace).is_ok() {
        return Ok(strip_verbatim_prefix(Path::new(workspace))
            .to_string_lossy()
            .into_owned());
    }
    let base = if doc_dir.contains(':') && std::path::Path::new(doc_dir).is_absolute() {
        std::path::Path::new(doc_dir).to_path_buf()
    } else {
        std::path::Path::new(workspace).join(doc_dir)
    };
    let base = canonicalize_for_process(&base).unwrap_or_else(|_| strip_verbatim_prefix(&base));
    let workspace = Path::new(workspace);
    let workspace = canonicalize_for_process(workspace)
        .unwrap_or_else(|_| strip_verbatim_prefix(workspace));
    let mut current = if base.is_dir() { base } else {
        let Some(parent) = base.parent() else { return Err(format!("文档目录无效：{doc_dir}")); };
        parent.to_path_buf()
    };
    loop {
        // `rev-parse` 会向上找到父仓，所以必须从文档目录逐级回退，不能直接
        // 对 doc 路径调用 ensure_git_repo——那会把“文档目录本身”当成仓库根。
        if current.join(".git").exists()
            || crate::git_commands::git_stdout(current.to_string_lossy().as_ref(), &["rev-parse", "--show-toplevel"])
                .map(|root| Path::new(&root) == current.as_path())
                .unwrap_or(false)
        {
            let current_canon = canonicalize_for_process(&current)
                .unwrap_or_else(|_| current.clone());
            if current_canon == workspace || current_canon.starts_with(&workspace) {
                return Ok(strip_verbatim_prefix(&current)
                    .to_string_lossy()
                    .into_owned());
            }
        }
        if current == workspace { break; }
        if !current.pop() { break; }
    }
    Err(format!(
        "工作区 {} 下未找到包含文档目录 {} 的 Git 仓库；父目录本身不是仓库时也会跳过未勾选子仓",
        workspace.display(), doc_dir
    ))
}

pub(super) struct SourceSnapshot { pub(super) path: String, repo: String }

impl Drop for SourceSnapshot {
    fn drop(&mut self) {
        let _ = git(&self.repo, &["worktree", "remove", "--force", &self.path]);
    }
}

impl std::ops::Deref for SourceSnapshot {
    type Target = str;
    fn deref(&self) -> &str { &self.path }
}

pub(super) fn snapshot_source(repo: &str, head: &str) -> Result<SourceSnapshot, String> {
    let id = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "ai-workbench-docgen-source-{}-{}-{id}",
        std::process::id(), chrono::Utc::now().timestamp_millis(),
    ));
    let path = dir.to_string_lossy().to_string();
    git(repo, &["worktree", "add", "--detach", &path, head])?;
    Ok(SourceSnapshot { path, repo: repo.into() })
}

/// 不解析 porcelain rename 的多字段形式；用 -z 的逐路径 diff + 未跟踪清单。
pub(super) fn changed_paths(repo: &str) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for args in [
        vec!["diff", "HEAD", "--name-only", "-z", "--"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        let out = git_output(repo, &args)?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        for name in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let name = String::from_utf8(name.to_vec()).map_err(|_| "文件路径不是 UTF-8")?;
            if !paths.contains(&name) { paths.push(name); }
        }
    }
    Ok(paths)
}

pub(super) fn validate_changes(repo: &str, doc_dir: &str) -> Result<(), String> {
    use crate::git_commands::strip_verbatim_prefix;
    // root 保持 canonicalize 原样喂给 resolve_in_repo（两边同带/同不带 `\\?\`）；
    // 越界比对再剥前缀，避免和手填 doc_dir 混用时 starts_with 误伤。
    let root = Path::new(repo).canonicalize().map_err(|e| e.to_string())?;
    let root_plain = strip_verbatim_prefix(&root);
    for path in changed_paths(repo)? {
        if super::is_outside_doc_dir(&path, doc_dir) {
            return Err(format!("AI 改动超出文档目录：{path}（未提交，请人工核对工作区）"));
        }
        let full = root.join(&path);
        // 新文档不能借 symlink / junction 跳到用户目录。
        let resolved = crate::agent_fs::resolve_in_repo(&root, &path)?;
        if !strip_verbatim_prefix(&resolved).starts_with(root_plain.join(doc_dir)) {
            return Err(format!("文档路径越界：{path}"));
        }
        if full.exists() && !full.is_file() {
            return Err(format!("文档产物不是普通文件：{path}"));
        }
        if let Ok(meta) = std::fs::symlink_metadata(&full) {
            if meta.file_type().is_symlink() {
                return Err(format!("文档产物不能是符号链接：{path}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let id = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("docgen-test-{}-{id}", std::process::id()));
            std::fs::create_dir(&p).unwrap();
            let repo = p.to_str().unwrap();
            git(repo, &["init", "-b", "release"]).unwrap();
            git(repo, &["config", "user.name", "Docgen Test"]).unwrap();
            git(repo, &["config", "user.email", "docgen@example.invalid"]).unwrap();
            std::fs::create_dir(p.join("doc")).unwrap();
            std::fs::write(p.join("doc/a.md"), "old").unwrap();
            std::fs::write(p.join("code.rs"), "original").unwrap();
            git(repo, &["add", "."]).unwrap();
            git(repo, &["commit", "-m", "initial"]).unwrap();
            Self(p)
        }
        fn repo(&self) -> &str { self.0.to_str().unwrap() }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn collection_handles_root_commit_and_keeps_all_files() {
        let f = Fixture::new();
        let root = super::super::collect_delta(f.repo(), None, 0).unwrap();
        assert_eq!(root.files.len(), 2);
        let prev = git(f.repo(), &["rev-parse", "HEAD"]).unwrap();
        for n in 0..161 { std::fs::write(f.0.join(format!("doc/{n}.md")), "new").unwrap(); }
        git(f.repo(), &["add", "doc"]).unwrap();
        git(f.repo(), &["commit", "-m", "many docs"]).unwrap();
        let delta = super::super::collect_delta(f.repo(), Some(&prev), 0).unwrap();
        assert_eq!(delta.files.len(), 161);
        assert!(super::super::collect_delta(f.repo(), Some("invalid-sha"), 0).is_err());
    }

    #[test]
    fn commit_failure_is_not_success_and_push_retry_reuses_exact_sha() {
        let f = Fixture::new();
        std::fs::write(f.0.join("doc/a.md"), "draft").unwrap();
        git(f.repo(), &["config", "user.name", ""]).unwrap();
        assert!(super::super::commit_docs(f.repo(), "doc", "release", 1).is_err());
        git(f.repo(), &["config", "user.name", "Docgen Test"]).unwrap();
        super::super::commit_docs(f.repo(), "doc", "release", 1).unwrap();
        let sha = git(f.repo(), &["rev-parse", "HEAD"]).unwrap();
        let pending = super::super::PendingPublication {
            workspace: f.repo().into(), branch: "release".into(), head: sha.clone(),
            sources: Vec::new(), changed_files: Vec::new(), summary: "done".into(),
        };
        assert!(super::super::retry_publication(&pending).is_err()); // 无 remote
        let remote = f.0.join("remote.git");
        git(f.repo(), &["init", "--bare", remote.to_str().unwrap()]).unwrap();
        git(f.repo(), &["remote", "add", "origin", remote.to_str().unwrap()]).unwrap();
        super::super::retry_publication(&pending).unwrap();
        assert_eq!(git(remote.to_str().unwrap(), &["rev-parse", "refs/heads/release"]).unwrap(), sha);
        std::fs::write(f.0.join("doc/a.md"), "user next change").unwrap();
        git(f.repo(), &["add", "doc"]).unwrap();
        git(f.repo(), &["commit", "-m", "user next commit"]).unwrap();
        assert!(super::super::retry_publication(&pending).is_err());
    }

    #[test]
    fn parent_directory_resolves_to_repo_containing_doc_dir() {
        let root = std::env::temp_dir().join(format!("docgen-parent-{}-{}", std::process::id(), NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed)));
        let repo = root.join("pmys.saas.parent.pom");
        let doc = repo.join("doc");
        // 父目录不是仓库；文档目录位于子仓中。
        std::fs::create_dir_all(&doc).unwrap();
        git(&repo.to_str().unwrap(), &["init", "-b", "release"]).unwrap();
        std::fs::write(repo.join("README.md"), "x").unwrap();
        git(&repo.to_str().unwrap(), &["add", "."]).unwrap();
        git(&repo.to_str().unwrap(), &["commit", "-m", "init"]).unwrap();
        let resolved = super::resolve_doc_repo(root.to_str().unwrap(), "pmys.saas.parent.pom/doc").unwrap();
        // 返回值必须可直接当 CreateProcess cwd：不能带 `\\?\`
        assert!(
            !resolved.contains(r"\\?\"),
            "resolve_doc_repo returned verbatim path: {resolved}"
        );
        let resolved_canon = crate::git_commands::canonicalize_for_process(&resolved).unwrap_or_default();
        let repo_canon = crate::git_commands::canonicalize_for_process(&repo).unwrap_or_default();
        assert_eq!(resolved_canon, repo_canon);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn source_snapshot_uses_exact_sha_and_is_removed_on_drop() {
        let f = Fixture::new();
        let head = git(f.repo(), &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(f.0.join("doc/later.md"), "user later").unwrap();
        git(f.repo(), &["add", "doc"]).unwrap();
        git(f.repo(), &["commit", "-m", "later"]).unwrap();
        let snap = super::snapshot_source(f.repo(), &head).unwrap();
        assert_eq!(git(&snap.path, &["rev-parse", "HEAD"]).unwrap(), head);
        assert!(!std::path::Path::new(&snap.path).join("doc/later.md").exists());
        let path = snap.path.clone();
        drop(snap);
        assert!(!std::path::Path::new(&path).exists());
        assert!(!git(f.repo(), &["worktree", "list"]).unwrap().contains(&path));
    }

    #[test]
    fn source_snapshot_must_stay_alive_for_collect() {
        let f = Fixture::new();
        let head = git(f.repo(), &["rev-parse", "HEAD"]).unwrap();
        // 复现旧 bug：只 clone path 就丢临时值 → Drop 删掉 worktree → 采集 cwd 失效
        let dangling = {
            let snap = super::snapshot_source(f.repo(), &head).unwrap();
            snap.path.clone()
        };
        assert!(
            super::super::collect_delta(&dangling, None, 0).is_err(),
            "dangling snapshot path should not be usable after Drop"
        );
        // 正确用法：持有快照直到采集结束
        let snap = super::snapshot_source(f.repo(), &head).unwrap();
        let delta = super::super::collect_delta(&snap.path, None, 0).unwrap();
        assert!(!delta.head.is_empty());
    }

    #[test]
    fn validate_changes_rejects_writes_outside_doc_dir() {
        let f = Fixture::new();
        std::fs::write(f.0.join("code.rs"), "AI outside doc").unwrap();
        assert!(validate_changes(f.repo(), "doc").is_err());
        std::fs::write(f.0.join("doc/a.md"), "generated").unwrap();
        // 同时有越界改动时仍失败
        assert!(validate_changes(f.repo(), "doc").is_err());
        git(f.repo(), &["checkout", "--", "code.rs"]).unwrap();
        assert!(validate_changes(f.repo(), "doc").is_ok());
    }
}
