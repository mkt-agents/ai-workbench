//! 文档任务专用工作树。AI 只写隔离目录，不回退或删除用户工作区文件。

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::git_commands::{git_output, git_stdout};

static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

pub(super) struct DocgenWorkspace {
    pub path: String,
    repo: String,
    original_head: String,
    original_branch: String,
}

fn git(repo: &str, args: &[&str]) -> Result<String, String> {
    git_stdout(repo, args)
}

impl DocgenWorkspace {
    pub fn prepare(repo: &str, branch: &str) -> Result<Self, String> {
        git(repo, &["check-ref-format", "--branch", branch])?;
        git(repo, &["fetch", "origin", branch])?;
        let local = format!("refs/heads/{branch}");
        let remote = format!("refs/remotes/origin/{branch}");
        let base = git(repo, &["rev-parse", "--verify", &local])
            .or_else(|_| git(repo, &["rev-parse", "--verify", &remote]))?;
        let work = Self::create(repo, base.trim())?;
        if let Err(e) = git(&work.path, &["merge", "--ff-only", &remote]) {
            return Err(format!("文档分支无法快进：{e}；隔离目录保留在 {}", work.path));
        }
        Ok(work)
    }

    fn create(repo: &str, base: &str) -> Result<Self, String> {
        let original_head = git(repo, &["rev-parse", "HEAD"])?;
        let original_branch = git(repo, &["symbolic-ref", "--short", "-q", "HEAD"])
            .unwrap_or_default();
        let id = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ai-workbench-docgen-{}-{}-{id}",
            std::process::id(), chrono::Utc::now().timestamp_millis(),
        ));
        let path = dir.to_string_lossy().to_string();
        git(repo, &["worktree", "add", "--detach", &path, base])?;
        Ok(Self { path, repo: repo.into(), original_head, original_branch })
    }

    /// 发布前重新核对原工作区。不同分支、用户保存文件或切 HEAD 都不覆盖。
    pub fn publish(&self, branch: &str) -> Result<(), String> {
        let current = git(&self.repo, &["symbolic-ref", "--short", "-q", "HEAD"])?;
        if current != branch || current != self.original_branch {
            return Err("原工作区不在目标分支，未发布；请切换到目标分支后重跑".into());
        }
        if git(&self.repo, &["rev-parse", "HEAD"])? != self.original_head {
            return Err("原工作区 HEAD 已改变，未发布".into());
        }
        if !git(&self.repo, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
            return Err("原工作区出现用户改动，未发布；隔离产物已保留".into());
        }
        let head = git(&self.path, &["rev-parse", "HEAD"])?;
        git(&self.repo, &["merge", "--ff-only", head.trim()])?;
        Ok(())
    }

    pub fn remove(self) -> Result<(), String> {
        // 仅移除本任务自己创建的 worktree；不强制删除脏产物。
        git(&self.repo, &["worktree", "remove", &self.path])?;
        Ok(())
    }
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
    let root = Path::new(repo).canonicalize().map_err(|e| e.to_string())?;
    for path in changed_paths(repo)? {
        if super::is_outside_doc_dir(&path, doc_dir) {
            return Err(format!("AI 改动超出文档目录：{path}（隔离产物保留，不发布）"));
        }
        let full = root.join(&path);
        // 新文档不能借 symlink / junction 跳到用户目录。
        let resolved = crate::agent_fs::resolve_in_repo(&root, &path)?;
        if !resolved.starts_with(root.join(doc_dir)) {
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
    fn isolated_failure_never_reverts_user_files() {
        let f = Fixture::new();
        std::fs::write(f.0.join("code.rs"), "user edit").unwrap();
        std::fs::write(f.0.join("user.txt"), "user untracked").unwrap();
        let work = DocgenWorkspace::create(f.repo(), "HEAD").unwrap();
        std::fs::write(Path::new(&work.path).join("code.rs"), "AI outside doc").unwrap();
        assert!(validate_changes(&work.path, "doc").is_err());
        assert!(work.publish("release").is_err());
        assert_eq!(std::fs::read_to_string(f.0.join("code.rs")).unwrap(), "user edit");
        assert_eq!(std::fs::read_to_string(f.0.join("user.txt")).unwrap(), "user untracked");
        // 测试仅清理自己创建的隔离夹具。
        git(f.repo(), &["worktree", "remove", "--force", &work.path]).unwrap();
    }

    #[test]
    fn publish_only_fast_forwards_unchanged_clean_target_branch() {
        let f = Fixture::new();
        let work = DocgenWorkspace::create(f.repo(), "HEAD").unwrap();
        std::fs::write(Path::new(&work.path).join("doc/a.md"), "generated").unwrap();
        validate_changes(&work.path, "doc").unwrap();
        git(&work.path, &["add", "doc"]).unwrap();
        git(&work.path, &["commit", "-m", "docs"]).unwrap();
        work.publish("release").unwrap();
        assert_eq!(std::fs::read_to_string(f.0.join("doc/a.md")).unwrap(), "generated");
        work.remove().unwrap();
    }

    #[test]
    fn publish_refuses_concurrent_edits_and_branch_switch() {
        let f = Fixture::new();
        let work = DocgenWorkspace::create(f.repo(), "HEAD").unwrap();
        std::fs::write(f.0.join("doc/a.md"), "new user edit").unwrap();
        assert!(work.publish("release").is_err());
        git(f.repo(), &["switch", "-c", "other"]).unwrap();
        assert!(work.publish("release").is_err());
        work.remove().unwrap();
    }
}
