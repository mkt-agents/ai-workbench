//! Merge / conflict state shared by the commit page and the agent: which
//! operation is in progress, which paths are unmerged, and the three versions of
//! a conflicted file. Conflicts are never resolved here on our own — every write
//! below is an explicit user choice coming from the resolver.

use crate::config::{err_code, ErrorCode};
use crate::git_commands::{ensure_git_repo, git_output, git_stdout, validate_repo_rel_path};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub(crate) fn code_err(code: ErrorCode, detail: impl Into<String>) -> String {
    err_code(code, detail).to_string()
}

/// The repository's git dir without spawning git: `.git` is a directory for a
/// normal clone and a `gitdir: <path>` file for worktrees and submodules.
pub(crate) fn resolve_git_dir(repo: &Path) -> Option<PathBuf> {
    let dot = repo.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = text.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
    let path = PathBuf::from(target);
    Some(if path.is_absolute() { path } else { repo.join(path) })
}

/// The operation git is in the middle of, read from its marker files.
pub(crate) fn repo_operation(git_dir: &Path) -> Option<&'static str> {
    if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir() {
        Some("rebase")
    } else if git_dir.join("MERGE_HEAD").is_file() {
        Some("merge")
    } else if git_dir.join("CHERRY_PICK_HEAD").is_file() {
        Some("cherry-pick")
    } else if git_dir.join("REVERT_HEAD").is_file() {
        Some("revert")
    } else {
        None
    }
}

pub(crate) fn operation_label(op: &str) -> &'static str {
    match op {
        "rebase" => "变基",
        "merge" => "合并",
        "cherry-pick" => "拣选提交",
        "revert" => "回退提交",
        _ => "操作",
    }
}

/// `git ls-files -u -z`: `<mode> <sha> <stage>\t<path>` per entry, one entry per
/// stage. Unlike conflict text in `git merge` output this does not depend on the
/// git language.
pub(crate) fn parse_unmerged_paths(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for record in raw.split('\0') {
        let Some((_, path)) = record.split_once('\t') else {
            continue;
        };
        if !path.is_empty() && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    out
}

pub(crate) fn unmerged_paths(repo_path: &str) -> Vec<String> {
    git_stdout(repo_path, &["ls-files", "-u", "-z"])
        .map(|raw| parse_unmerged_paths(&raw))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ConflictState {
    pub operation: Option<&'static str>,
    pub conflicted: Vec<String>,
}

pub(crate) fn conflict_state(repo_path: &str) -> ConflictState {
    let operation = resolve_git_dir(Path::new(repo_path))
        .as_deref()
        .and_then(repo_operation);
    ConflictState {
        operation,
        conflicted: unmerged_paths(repo_path),
    }
}

fn list_paths(paths: &[String]) -> String {
    let mut shown: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
    if paths.len() > 5 {
        shown.push("…");
    }
    shown.join("、")
}

/// One sentence for "you can't `action` right now", or None when the repo is clear.
pub(crate) fn blocked_reason(state: &ConflictState, action: &str) -> Option<String> {
    match state.operation {
        Some("merge") => Some(format!(
            "仓库正在合并中{}。请先在 Git 管理 → 提交页解决冲突并提交合并，或中止合并后再{action}。",
            if state.conflicted.is_empty() {
                String::new()
            } else {
                format!("，还有 {} 个冲突文件：{}", state.conflicted.len(), list_paths(&state.conflicted))
            }
        )),
        Some(op) => Some(format!(
            "仓库正在{}中，请先在终端完成（--continue）或中止（--abort）后再{action}。",
            operation_label(op)
        )),
        None if !state.conflicted.is_empty() => Some(format!(
            "还有 {} 个冲突文件没有解决：{}。请先在 Git 管理 → 提交页解决后再{action}。",
            state.conflicted.len(),
            list_paths(&state.conflicted)
        )),
        None => None,
    }
}

/// Refuse anything that would move history while a merge/rebase is open or
/// conflicts are unresolved (push, pull, stash, undo, amend).
pub(crate) fn ensure_repo_clear(repo_path: &str, action: &str) -> Result<(), String> {
    let state = conflict_state(repo_path);
    match blocked_reason(&state, action) {
        Some(reason) if state.operation.is_some() => Err(code_err(ErrorCode::GitOperationInProgress, reason)),
        Some(reason) => Err(code_err(ErrorCode::GitConflict, reason)),
        None => Ok(()),
    }
}

/// Conflict error with the file list, after an operation stopped on conflicts.
pub(crate) fn conflict_error(paths: &[String], what: &str) -> String {
    code_err(
        ErrorCode::GitConflict,
        format!(
            "{what}时有 {} 个文件冲突：{}。没有自动解决，合并状态已保留：请在 Git 管理 → 提交页逐个解决后提交合并，或中止合并。",
            paths.len(),
            list_paths(paths)
        ),
    )
}

/// MERGE_MSG minus git's `#` comment block (the conflict list).
pub(crate) fn strip_merge_msg(raw: &str) -> String {
    raw.lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// `<<<<<<<` / `>>>>>>>` left in the text means the file is not resolved yet.
/// A lone `=======` is not checked: it is a normal setext heading underline.
pub(crate) fn has_conflict_markers(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_end_matches('\r');
        l == "<<<<<<<" || l == ">>>>>>>" || l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ")
    })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MergeSegment {
    /// Text git merged by itself (identical on both sides, or changed on one side only).
    Same { text: String },
    Conflict { ours: String, base: String, theirs: String },
}

/// Split `git merge-file -p --diff3` output into merged text and conflict chunks.
/// Line endings are kept exactly, so joining the chosen pieces rebuilds the file.
pub(crate) fn parse_merge_segments(text: &str) -> Vec<MergeSegment> {
    #[derive(PartialEq)]
    enum At {
        Same,
        Ours,
        Base,
        Theirs,
    }
    let mut out = Vec::new();
    let (mut same, mut ours, mut base, mut theirs) = (String::new(), String::new(), String::new(), String::new());
    let mut at = At::Same;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        match at {
            At::Same if bare == "<<<<<<<" || bare.starts_with("<<<<<<< ") => {
                if !same.is_empty() {
                    out.push(MergeSegment::Same { text: std::mem::take(&mut same) });
                }
                at = At::Ours;
            }
            At::Same => same.push_str(line),
            At::Ours if bare == "|||||||" || bare.starts_with("||||||| ") => at = At::Base,
            At::Ours | At::Base if bare == "=======" => at = At::Theirs,
            At::Ours => ours.push_str(line),
            At::Base => base.push_str(line),
            At::Theirs if bare == ">>>>>>>" || bare.starts_with(">>>>>>> ") => {
                out.push(MergeSegment::Conflict {
                    ours: std::mem::take(&mut ours),
                    base: std::mem::take(&mut base),
                    theirs: std::mem::take(&mut theirs),
                });
                at = At::Same;
            }
            At::Theirs => theirs.push_str(line),
        }
    }
    if at != At::Same {
        // Truncated markers: keep what was read as an unresolved chunk rather than guess.
        out.push(MergeSegment::Conflict { ours, base, theirs });
    } else if !same.is_empty() {
        out.push(MergeSegment::Same { text: same });
    }
    out
}

/// Which index stages exist for `file` (1 = base, 2 = ours, 3 = theirs).
fn stages_of(repo_path: &str, file: &str) -> [bool; 3] {
    let raw = git_stdout(repo_path, &["ls-files", "-u", "-z", "--", file]).unwrap_or_default();
    let mut has = [false; 3];
    for record in raw.split('\0') {
        let Some((meta, _)) = record.split_once('\t') else {
            continue;
        };
        if let Some(stage) = meta.split_whitespace().nth(2).and_then(|s| s.parse::<usize>().ok()) {
            if (1..=3).contains(&stage) {
                has[stage - 1] = true;
            }
        }
    }
    has
}

fn read_stage(repo_path: &str, stage: u8, file: &str) -> Result<Vec<u8>, String> {
    let spec = format!(":{stage}:{file}");
    let out = git_output(repo_path, &["show", &spec])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
}

const TEXT_LIMIT: usize = 2 * 1024 * 1024;

fn as_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() > TEXT_LIMIT || bytes.iter().take(8000).any(|b| *b == 0) {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

fn current_branch(repo_path: &str) -> String {
    git_stdout(repo_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// "Merge branch 'x'" / "Merge remote-tracking branch 'origin/main'" → the quoted name.
pub(crate) fn merge_source_label(merge_msg: &str) -> String {
    let first = merge_msg.lines().next().unwrap_or("");
    let mut parts = first.split('\'');
    match (parts.next(), parts.next()) {
        (Some(_), Some(name)) if !name.is_empty() => name.to_string(),
        _ => String::new(),
    }
}

fn merge_msg(repo_path: &str) -> String {
    resolve_git_dir(Path::new(repo_path))
        .and_then(|d| std::fs::read_to_string(d.join("MERGE_MSG")).ok())
        .unwrap_or_default()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitMergeState {
    /// merge | rebase | cherry-pick | revert, or None.
    pub operation: Option<String>,
    pub conflicted: Vec<String>,
    /// MERGE_MSG without git's comment block: the default merge commit message.
    pub message: String,
    pub ours_label: String,
    pub theirs_label: String,
}

pub(crate) fn git_merge_state_sync(repo_path: String) -> Result<GitMergeState, String> {
    ensure_git_repo(&repo_path)?;
    let state = conflict_state(&repo_path);
    let raw_msg = if state.operation == Some("merge") { merge_msg(&repo_path) } else { String::new() };
    Ok(GitMergeState {
        operation: state.operation.map(str::to_string),
        conflicted: state.conflicted,
        message: strip_merge_msg(&raw_msg),
        ours_label: current_branch(&repo_path),
        theirs_label: merge_source_label(&raw_msg),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitConflictFile {
    /// content | deletedByThem | deletedByUs | deletedBoth
    pub kind: String,
    /// All present versions are UTF-8 text under 2MB; otherwise only whole-file choices.
    pub textual: bool,
    pub ours: Option<String>,
    pub base: Option<String>,
    pub theirs: Option<String>,
    pub segments: Vec<MergeSegment>,
    pub ours_label: String,
    pub theirs_label: String,
}

fn conflict_kind(has: [bool; 3]) -> &'static str {
    match (has[1], has[2]) {
        (true, true) => "content",
        (true, false) => "deletedByThem",
        (false, true) => "deletedByUs",
        (false, false) => "deletedBoth",
    }
}

/// Run `git merge-file -p --diff3` on temp copies of the three versions.
fn merge_file_segments(repo_path: &str, ours: &str, base: &str, theirs: &str) -> Result<Vec<MergeSegment>, String> {
    let dir = std::env::temp_dir().join(format!(
        "aw-merge-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let write = |name: &str, text: &str| -> Result<String, String> {
        let p = dir.join(name);
        std::fs::write(&p, text).map_err(|e| format!("写临时文件失败: {e}"))?;
        Ok(p.to_string_lossy().into_owned())
    };
    let result = (|| {
        let (o, b, t) = (write("ours", ours)?, write("base", base)?, write("theirs", theirs)?);
        let out = git_output(
            repo_path,
            &["merge-file", "-p", "--diff3", "-L", "ours", "-L", "base", "-L", "theirs", &o, &b, &t],
        )?;
        // Exit code is the number of conflicts; only a negative/abnormal code is an error.
        match out.status.code() {
            Some(c) if (0..127).contains(&c) => Ok(parse_merge_segments(&String::from_utf8_lossy(&out.stdout))),
            _ => Err(format!("git merge-file 失败: {}", String::from_utf8_lossy(&out.stderr).trim())),
        }
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

pub(crate) fn git_conflict_file_sync(repo_path: String, file: String) -> Result<GitConflictFile, String> {
    ensure_git_repo(&repo_path)?;
    validate_repo_rel_path(&file)?;
    let has = stages_of(&repo_path, &file);
    if !has.iter().any(|h| *h) {
        return Err("这个文件已经没有冲突了，请刷新列表。".into());
    }
    let read = |stage: u8, present: bool| -> Result<Option<Vec<u8>>, String> {
        if present { read_stage(&repo_path, stage, &file).map(Some) } else { Ok(None) }
    };
    let (base_b, ours_b, theirs_b) = (read(1, has[0])?, read(2, has[1])?, read(3, has[2])?);
    let texts: Vec<Option<Option<String>>> = [&base_b, &ours_b, &theirs_b]
        .iter()
        .map(|b| b.as_ref().map(|bytes| as_text(bytes)))
        .collect();
    let textual = texts.iter().all(|t| !matches!(t, Some(None)));
    let take = |i: usize| texts[i].clone().flatten();
    let (base, ours, theirs) = if textual { (take(0), take(1), take(2)) } else { (None, None, None) };
    let kind = conflict_kind(has);
    let segments = if textual && kind == "content" {
        merge_file_segments(
            &repo_path,
            ours.as_deref().unwrap_or(""),
            base.as_deref().unwrap_or(""),
            theirs.as_deref().unwrap_or(""),
        )?
    } else {
        Vec::new()
    };
    let raw_msg = merge_msg(&repo_path);
    Ok(GitConflictFile {
        kind: kind.to_string(),
        textual,
        ours,
        base,
        theirs,
        segments,
        ours_label: current_branch(&repo_path),
        theirs_label: merge_source_label(&raw_msg),
    })
}

fn run_git(repo_path: &str, args: &[&str], what: &str) -> Result<(), String> {
    let out = git_output(repo_path, args)?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if stderr.is_empty() { format!("{what}失败") } else { format!("{what}失败：{stderr}") })
}

/// Keep the working file's line endings: the stage blobs are LF, a Windows
/// checkout with autocrlf is CRLF.
fn match_line_endings(content: &str, existing: Option<&[u8]>) -> String {
    let crlf = existing.map(|b| b.windows(2).any(|w| w == b"\r\n")).unwrap_or(false);
    if crlf && !content.contains("\r\n") {
        content.replace('\n', "\r\n")
    } else {
        content.to_string()
    }
}

/// Record the user's resolution: write `content` (or take one side / delete) and
/// mark the path resolved. Refuses text that still has conflict markers.
pub(crate) fn git_resolve_conflict_sync(
    repo_path: String,
    file: String,
    content: Option<String>,
    take: Option<String>,
) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    validate_repo_rel_path(&file)?;
    let has = stages_of(&repo_path, &file);
    if !has.iter().any(|h| *h) {
        return Err("这个文件已经没有冲突了，请刷新列表。".into());
    }
    let full = Path::new(&repo_path).join(&file);
    match (content, take.as_deref()) {
        (Some(text), None) => {
            if has_conflict_markers(&text) {
                return Err("结果里还有冲突标记（<<<<<<< / >>>>>>>），请处理完再标记已解决。".into());
            }
            let existing = std::fs::read(&full).ok();
            let text = match_line_endings(&text, existing.as_deref());
            if let Some(parent) = full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&full, text).map_err(|e| format!("写入文件失败: {e}"))?;
            run_git(&repo_path, &["add", "--", &file], "标记已解决")?;
            Ok(format!("已解决：{file}"))
        }
        (None, Some(side @ ("ours" | "theirs"))) => {
            let present = if side == "ours" { has[1] } else { has[2] };
            if !present {
                run_git(&repo_path, &["rm", "-q", "--", &file], "删除文件")?;
                return Ok(format!("已解决（删除）：{file}"));
            }
            let flag = if side == "ours" { "--ours" } else { "--theirs" };
            run_git(&repo_path, &["checkout", flag, "--", &file], "取出版本")?;
            run_git(&repo_path, &["add", "--", &file], "标记已解决")?;
            Ok(format!("已解决：{file}"))
        }
        (None, Some("delete")) => {
            run_git(&repo_path, &["rm", "-q", "--", &file], "删除文件")?;
            Ok(format!("已解决（删除）：{file}"))
        }
        _ => Err("缺少解决方式".into()),
    }
}

pub(crate) fn git_merge_abort_sync(repo_path: String) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    if conflict_state(&repo_path).operation != Some("merge") {
        return Err("仓库当前没有进行中的合并。".into());
    }
    run_git(&repo_path, &["merge", "--abort"], "中止合并")?;
    Ok("已中止合并，仓库回到合并前的状态".into())
}

#[tauri::command]
pub async fn git_merge_state(path: String) -> Result<GitMergeState, String> {
    tokio::task::spawn_blocking(move || git_merge_state_sync(path))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

#[tauri::command]
pub async fn git_conflict_file(path: String, file: String) -> Result<GitConflictFile, String> {
    tokio::task::spawn_blocking(move || git_conflict_file_sync(path, file))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

#[tauri::command]
pub async fn git_resolve_conflict(
    path: String,
    file: String,
    content: Option<String>,
    take: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_resolve_conflict_sync(path, file, content, take))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

#[tauri::command]
pub async fn git_merge_abort(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_merge_abort_sync(path))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff3_output_splits_into_same_and_conflict() {
        let text = "a\n<<<<<<< ours\nmine\n||||||| base\nold\n=======\nyours\n>>>>>>> theirs\nz\r\n";
        assert_eq!(
            parse_merge_segments(text),
            vec![
                MergeSegment::Same { text: "a\n".into() },
                MergeSegment::Conflict { ours: "mine\n".into(), base: "old\n".into(), theirs: "yours\n".into() },
                MergeSegment::Same { text: "z\r\n".into() },
            ]
        );
        // A heading underline is not a marker outside a conflict.
        let doc = "Title\n=======\n";
        assert_eq!(parse_merge_segments(doc), vec![MergeSegment::Same { text: doc.into() }]);
    }

    #[test]
    fn unmerged_paths_dedupe_stages_and_keep_spaces() {
        let raw = "100644 aaa 1\ta.txt\0100644 bbb 2\ta.txt\0100644 ccc 3\ta.txt\0100644 ddd 2\tb c.txt\0";
        assert_eq!(parse_unmerged_paths(raw), vec!["a.txt".to_string(), "b c.txt".to_string()]);
        assert!(parse_unmerged_paths("").is_empty());
    }

    #[test]
    fn markers_and_merge_message() {
        assert!(has_conflict_markers("x\n<<<<<<< ours\ny\n"));
        assert!(has_conflict_markers(">>>>>>> theirs\r\n"));
        assert!(!has_conflict_markers("Title\n=======\n"));
        let msg = "Merge branch 'other'\n\n# Conflicts:\n#\ta.txt\n";
        assert_eq!(strip_merge_msg(msg), "Merge branch 'other'");
        assert_eq!(merge_source_label(msg), "other");
        assert_eq!(merge_source_label("Merge remote-tracking branch 'origin/main'"), "origin/main");
        assert_eq!(conflict_kind([true, true, false]), "deletedByThem");
        assert_eq!(conflict_kind([false, true, true]), "content");
    }

    #[test]
    fn operation_markers_and_reasons() {
        let dir = std::env::temp_dir().join(format!("aw-op-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(repo_operation(&dir), None);
        std::fs::write(dir.join("MERGE_HEAD"), "x").unwrap();
        assert_eq!(repo_operation(&dir), Some("merge"));
        std::fs::create_dir_all(dir.join("rebase-merge")).unwrap();
        assert_eq!(repo_operation(&dir), Some("rebase"));
        let _ = std::fs::remove_dir_all(&dir);

        let clear = ConflictState::default();
        assert!(blocked_reason(&clear, "推送").is_none());
        let merging = ConflictState { operation: Some("merge"), conflicted: vec!["a.txt".into()] };
        let reason = blocked_reason(&merging, "推送").unwrap();
        assert!(reason.contains("合并中") && reason.contains("a.txt") && reason.contains("推送"));
        let stash = ConflictState { operation: None, conflicted: vec!["b".into()] };
        assert!(blocked_reason(&stash, "提交").unwrap().contains("1 个冲突文件"));
    }

    #[test]
    fn line_endings_follow_the_working_file() {
        assert_eq!(match_line_endings("a\nb\n", Some(b"x\r\ny\r\n")), "a\r\nb\r\n");
        assert_eq!(match_line_endings("a\nb\n", Some(b"x\ny\n")), "a\nb\n");
        assert_eq!(match_line_endings("a\nb\n", None), "a\nb\n");
    }
}
