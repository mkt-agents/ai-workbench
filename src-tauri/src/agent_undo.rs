//! Undo for the agent's file changes.
//!
//! Before a change lands, the old bytes are copied to
//! `%APPDATA%\com.ai-workbench.app\agent-snapshots\<requestId>\`; one folder per turn.
//! 「撤销这次改动」 can revert the whole turn or one file at a time — every entry is
//! removed from the manifest once restored, so a per-file undo followed by 「全部还原」
//! only touches what is left.
//!
//! Covering all four ways the agent changes files:
//! `write` (overwrite), `create` (new file → undo deletes it), `delete` (copied aside
//! before it goes to the Recycle Bin) and `move` (undo renames it back). A *directory*
//! delete is not copied (the Recycle Bin is the undo for that) and is simply absent here.
//!
//! Snapshots are best effort: a file over [`MAX_FILE_BYTES`], or a failed copy, simply does
//! not get recorded, and the answer then offers no undo for it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Turns kept on disk; older folders are removed when a new turn starts.
const KEEP_TURNS: usize = 20;
/// Total budget for all snapshots of all turns.
const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// One file bigger than this is not copied (a 200MB log would blow the budget by itself).
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MANIFEST: &str = "manifest.json";

/// How the turn touched the file. Only the undo direction differs.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
enum Kind {
    /// Existing file overwritten: restore the copied bytes.
    #[default]
    Write,
    /// New file: undo deletes it.
    Create,
    /// File deleted (and copied aside): undo writes it back.
    Delete,
    /// File moved: undo renames it back.
    Move,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Entry {
    /// Absolute path of the file as it was before the turn.
    path: String,
    /// Snapshot file name inside the turn folder; `None` = the file did not exist yet
    /// (or a move, which needs no bytes).
    #[serde(default)]
    snapshot: Option<String>,
    #[serde(default)]
    bytes: u64,
    /// Missing in manifests written before per-file undo; then it is derived from `snapshot`.
    #[serde(default)]
    kind: Kind,
    /// Destination of a `move` entry.
    #[serde(default)]
    to: Option<String>,
}

impl Entry {
    /// Manifests from before this field existed only recorded writes and creates.
    fn kind(&self) -> Kind {
        if self.kind == Kind::Write && self.snapshot.is_none() {
            Kind::Create
        } else {
            self.kind
        }
    }
}

/// One row of the undo list the answer shows.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UndoFile {
    /// Absolute path (as recorded by the turn).
    pub path: String,
    /// `write` / `create` / `delete` / `move`.
    pub kind: String,
    /// Where a `move` put it.
    pub to: String,
    /// Recycle-bin deletes and moves have no bytes; the UI can still offer undo.
    pub bytes: u64,
}

#[tauri::command]
pub fn agent_undo(request_id: String, path: Option<String>) -> Result<String, String> {
    let root = root().ok_or("无法定位快照目录")?;
    undo_from(&root, &request_id, path.as_deref())
}

/// The files this turn can still undo (empty when the turn changed nothing, or the
/// snapshots were already pruned).
#[tauri::command]
pub fn agent_undo_files(request_id: String) -> Result<Vec<UndoFile>, String> {
    let root = root().ok_or("无法定位快照目录")?;
    let dir = turn_dir(&root, &request_id).ok_or("这一轮的请求号不合法")?;
    if !dir.join(MANIFEST).is_file() {
        return Ok(Vec::new());
    }
    Ok(list_files(&dir))
}

fn list_files(dir: &Path) -> Vec<UndoFile> {
    read_manifest(dir)
        .into_iter()
        .map(|e| UndoFile {
            kind: match e.kind() {
                Kind::Write => "write",
                Kind::Create => "create",
                Kind::Delete => "delete",
                Kind::Move => "move",
            }
            .to_string(),
            path: e.path,
            to: e.to.unwrap_or_default(),
            bytes: e.bytes,
        })
        .collect()
}

/// `%APPDATA%\com.ai-workbench.app\agent-snapshots`, created on demand.
fn root() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(appdata).join("com.ai-workbench.app").join("agent-snapshots");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Turn folder name. Request ids come from the front end (`qa-<ts>-<n>-<uuid8>`), so only
/// alphanumerics, `-` and `_` are accepted — anything else could climb out of the folder.
fn turn_dir(root: &Path, request_id: &str) -> Option<PathBuf> {
    if request_id.is_empty() || request_id.len() > 80 {
        return None;
    }
    if !request_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    Some(root.join(request_id))
}

fn read_manifest(dir: &Path) -> Vec<Entry> {
    std::fs::read_to_string(dir.join(MANIFEST))
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<Entry>>(&text).ok())
        .unwrap_or_default()
}

fn write_manifest(dir: &Path, entries: &[Entry]) -> std::io::Result<()> {
    let text = serde_json::to_string(entries).unwrap_or_else(|_| "[]".into());
    std::fs::write(dir.join(MANIFEST), text)
}

/// Copy `target` aside before the agent overwrites it. Returns `true` when this turn now
/// has something to undo (a real snapshot, or a record that the file is about to be created).
pub(crate) fn snapshot_before(request_id: &str, target: &Path) -> bool {
    let Some(root) = root() else {
        return false;
    };
    snapshot_into(&root, request_id, target, Kind::Write, None)
}

/// `delete_path` goes to the Recycle Bin, but a copy here means undo works without the user
/// digging it out. Directories are skipped: copying a tree is not worth the budget, and the
/// Recycle Bin restores them anyway.
pub(crate) fn snapshot_before_delete(request_id: &str, target: &Path) -> bool {
    if !target.is_file() {
        return false;
    }
    let Some(root) = root() else {
        return false;
    };
    snapshot_into(&root, request_id, target, Kind::Delete, None)
}

/// A move needs no bytes: undo is the reverse rename.
pub(crate) fn snapshot_before_move(request_id: &str, from: &Path, to: &Path) -> bool {
    let Some(root) = root() else {
        return false;
    };
    snapshot_into(&root, request_id, from, Kind::Move, Some(to.display().to_string()))
}

/// Whether this turn left anything to undo (cheap check used when finishing the answer).
pub(crate) fn has_snapshot(request_id: &str) -> bool {
    let Some(root) = root() else {
        return false;
    };
    turn_dir(&root, request_id)
        .map(|dir| dir.join(MANIFEST).is_file())
        .unwrap_or(false)
}

fn snapshot_into(root: &Path, request_id: &str, target: &Path, kind: Kind, to: Option<String>) -> bool {
    let Some(dir) = turn_dir(root, request_id) else {
        return false;
    };
    // 目录删除不记录：复制一整个目录不值这份预算，回收站就是它的撤销
    if kind == Kind::Delete && !target.is_file() {
        return false;
    }
    let mut entries = read_manifest(&dir);
    // 同一个文件在一轮里被改多次：最早那份才是「改之前」。
    let existing = entries.iter().position(|e| Path::new(&e.path) == target);
    if let Some(i) = existing {
        // 删掉又被重建：还是按「原来没有」退（这条记录已经够了）
        if kind != Kind::Move {
            return true;
        }
        // 移走又移回来：这条 move 记录作废，别让撤销去改一个本来就在原处的东西
        entries.remove(i);
        return write_manifest(&dir, &entries).is_ok();
    }
    // 这一轮的第一条记录可能就是新建文件：清单目录要在这里建出来
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let entry = if kind == Kind::Move {
        if !target.exists() {
            return false;
        }
        Entry { path: target.display().to_string(), snapshot: None, bytes: 0, kind, to }
    } else if target.is_file() {
        let Ok(meta) = target.metadata() else {
            return false;
        };
        if meta.len() > MAX_FILE_BYTES {
            return false;
        }
        let name = format!("{}.bak", entries.len());
        if std::fs::copy(target, dir.join(&name)).is_err() {
            return false;
        }
        Entry {
            path: target.display().to_string(),
            snapshot: Some(name),
            bytes: meta.len(),
            kind: if kind == Kind::Delete { Kind::Delete } else { Kind::Write },
            to: None,
        }
    } else {
        // 新建文件：撤销就是把它删掉。
        Entry { path: target.display().to_string(), snapshot: None, bytes: 0, kind: Kind::Create, to: None }
    };
    entries.push(entry);
    if write_manifest(&dir, &entries).is_err() {
        return false;
    }
    if entries.len() == 1 {
        // 这一轮的第一份快照：顺手清旧账，别让目录无限长大
        prune(root);
    }
    true
}

/// Undo one entry. `Ok(true)` = restored, `Ok(false)` = nothing to do (already back).
fn undo_entry(dir: &Path, entry: &Entry) -> Result<bool, String> {
    let target = Path::new(&entry.path);
    let ensure_parent = || {
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    };
    match entry.kind() {
        Kind::Write | Kind::Delete => {
            let Some(name) = &entry.snapshot else {
                return Err("这条记录没有备份文件，无法还原".into());
            };
            ensure_parent();
            std::fs::copy(dir.join(name), target)
                .map(|_| true)
                .map_err(|e| format!("写入失败（可能被占用或权限不足）：{e}"))
        }
        Kind::Create => {
            if !target.exists() {
                return Ok(false);
            }
            std::fs::remove_file(target).map(|_| true).map_err(|e| format!("删除失败：{e}"))
        }
        Kind::Move => {
            let Some(to) = &entry.to else {
                return Err("这条移动记录没有目标路径".into());
            };
            let to = Path::new(to);
            if !to.exists() {
                // 已经不在新位置了（用户自己挪回去，或又被移走）
                return Ok(target.exists());
            }
            if target.exists() {
                return Err("原位置已被占用，没有移回去".into());
            }
            ensure_parent();
            std::fs::rename(to, target)
                .map(|_| true)
                .map_err(|e| format!("移回失败（跨磁盘请手动处理）：{e}"))
        }
    }
}

fn undo_from(root: &Path, request_id: &str, only: Option<&str>) -> Result<String, String> {
    let dir = turn_dir(root, request_id).ok_or("这一轮的请求号不合法，无法撤销")?;
    if !dir.join(MANIFEST).is_file() {
        return Err("这一轮没有留下快照：可能没有改文件，或快照已被清理".into());
    }
    let entries = read_manifest(&dir);
    if entries.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("这一轮的快照是空的".into());
    }
    let picked: Vec<Entry> = match only {
        Some(path) => {
            let hit: Vec<Entry> = entries.iter().filter(|e| e.path == path).cloned().collect();
            if hit.is_empty() {
                return Err("这一轮没有改过这个文件（可能已经撤销过）".into());
            }
            hit
        }
        None => entries.clone(),
    };
    let mut restored: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    // 倒序：后写的先退，和写文件的反向一致
    for entry in picked.iter().rev() {
        match undo_entry(&dir, entry) {
            Ok(true) => {
                done.push(entry.path.clone());
                match entry.kind() {
                    Kind::Create => removed.push(entry.path.clone()),
                    _ => restored.push(entry.path.clone()),
                }
            }
            Ok(false) => done.push(entry.path.clone()),
            Err(_) => failed.push(entry.path.clone()),
        }
    }
    // 已还原的从清单里划掉：再点「全部还原」不该重做一遍
    let left: Vec<Entry> = entries.into_iter().filter(|e| !done.contains(&e.path)).collect();
    let left_count = left.len();
    if left.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
    } else if write_manifest(&dir, &left).is_err() {
        // 清单写不回去时宁可留着目录：重复撤销是幂等的，丢记录才是真的丢
        failed.push("（清单未能更新，下一次撤销可能重复执行）".into());
    }

    let mut lines: Vec<String> = Vec::new();
    if !restored.is_empty() {
        lines.push(format!("已还原 {} 个文件：{}", restored.len(), restored.join("、")));
    }
    if !removed.is_empty() {
        lines.push(format!("已删除 {} 个本轮新建的文件：{}", removed.len(), removed.join("、")));
    }
    if !failed.is_empty() {
        lines.push(format!(
            "{} 项没能还原（可能被占用或权限不足）：{}",
            failed.len(),
            failed.join("、")
        ));
    }
    if lines.is_empty() {
        lines.push("没有需要还原的改动".into());
    } else if only.is_some() && left_count > 0 {
        lines.push(format!("这一轮还有 {left_count} 个文件可以继续撤销"));
    }
    Ok(lines.join("\n"))
}

/// Keep the newest [`KEEP_TURNS`] folders, and the total under [`MAX_BYTES`].
fn prune(root: &Path) {
    let Ok(read) = std::fs::read_dir(root) else {
        return;
    };
    let mut turns: Vec<(std::time::SystemTime, u64, PathBuf)> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| {
            let size = dir_size(&e.path());
            (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), size, e.path())
        })
        .collect();
    // 同一毫秒内建出来的目录按名字排，免得「最老」变成随机
    turns.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    let mut total: u64 = turns.iter().map(|(_, size, _)| *size).sum();
    let mut keep = turns.len();
    for (_, size, path) in turns.iter() {
        if keep <= KEEP_TURNS && total <= MAX_BYTES {
            break;
        }
        if std::fs::remove_dir_all(path).is_ok() {
            total = total.saturating_sub(*size);
            keep -= 1;
        }
    }
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    read.flatten().map(|e| e.metadata().map(|m| m.len()).unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("aw-undo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn undo_restores_bytes_exactly() {
        let root = temp_root("exact");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("a.txt");
        std::fs::write(&target, b"original\nbytes\x00with\xffnul").unwrap();
        let fresh = work.join("b.txt");

        assert!(snapshot_into(&root, "req-1", &target, Kind::Write, None));
        assert!(snapshot_into(&root, "req-1", &fresh, Kind::Write, None));
        // 一轮里改两次：只留最早那份
        std::fs::write(&target, b"first edit").unwrap();
        assert!(snapshot_into(&root, "req-1", &target, Kind::Write, None));

        std::fs::write(&target, b"second edit").unwrap();
        std::fs::write(&fresh, b"created by the agent").unwrap();

        let summary = undo_from(&root, "req-1", None).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"original\nbytes\x00with\xffnul");
        assert!(!fresh.exists(), "新建的文件应被删掉");
        assert!(summary.contains("已还原 1 个文件"), "{summary}");
        assert!(summary.contains("已删除 1 个本轮新建的文件"), "{summary}");
        // 撤销后快照目录不再存在，第二次调用要有明确说法
        assert!(undo_from(&root, "req-1", None).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 一个文件一个文件地退：退完 a 之后目录里只剩 b，再点「全部还原」不会重做 a。
    #[test]
    fn single_file_undo_leaves_the_rest() {
        let root = temp_root("single");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let a = work.join("a.txt");
        let b = work.join("b.txt");
        std::fs::write(&a, b"a-old").unwrap();
        std::fs::write(&b, b"b-old").unwrap();
        assert!(snapshot_into(&root, "req-2", &a, Kind::Write, None));
        assert!(snapshot_into(&root, "req-2", &b, Kind::Write, None));
        std::fs::write(&a, b"a-new").unwrap();
        std::fs::write(&b, b"b-new").unwrap();

        let one = undo_from(&root, "req-2", Some(&a.display().to_string())).unwrap();
        assert!(one.contains("已还原 1 个文件"), "{one}");
        assert_eq!(std::fs::read(&a).unwrap(), b"a-old");
        assert_eq!(std::fs::read(&b).unwrap(), b"b-new", "只退指定的那个");

        let rest = undo_from(&root, "req-2", None).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"b-old");
        assert!(rest.contains("已还原 1 个文件"), "{rest}");
        // 再退一次同一个文件：清单里已经没有了
        assert!(undo_from(&root, "req-2", Some(&a.display().to_string())).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 删除与移动也在快照范围里：删除能写回来，移动能移回去。
    #[test]
    fn delete_and_move_are_undoable() {
        let root = temp_root("dvm");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let doomed = work.join("gone.txt");
        std::fs::write(&doomed, b"keep me").unwrap();
        assert!(snapshot_into(&root, "req-3", &doomed, Kind::Delete, None));
        std::fs::remove_file(&doomed).unwrap();

        let from = work.join("before.txt");
        let to = work.join("after.txt");
        std::fs::write(&from, b"moving").unwrap();
        assert!(snapshot_into(&root, "req-3", &from, Kind::Move, Some(to.display().to_string())));
        std::fs::rename(&from, &to).unwrap();

        let files = agent_undo_files_in(&root, "req-3");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].kind, "delete");
        assert_eq!(files[1].kind, "move");
        assert_eq!(files[1].to, to.display().to_string());

        undo_from(&root, "req-3", None).unwrap();
        assert_eq!(std::fs::read(&doomed).unwrap(), b"keep me");
        assert!(from.exists(), "移动应被移回原处");
        assert!(!to.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 一轮里第一次动作就是新建文件：清单目录得当场建出来，否则这条记录会丢。
    #[test]
    fn create_only_turn_is_undoable() {
        let root = temp_root("create");
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fresh = work.join("new.txt");
        assert!(snapshot_into(&root, "req-5", &fresh, Kind::Write, None));
        std::fs::write(&fresh, b"made by the agent").unwrap();
        assert_eq!(agent_undo_files_in(&root, "req-5")[0].kind, "create");
        undo_from(&root, "req-5", None).unwrap();
        assert!(!fresh.exists(), "新建的文件应被撤销掉");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 目录删除不复制：回收站就是它的撤销。
    #[test]
    fn directory_delete_is_not_snapshotted() {
        let root = temp_root("dir");
        let dir = root.join("folder");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!snapshot_into(&root, "req-4", &dir, Kind::Delete, None));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_request_ids_are_rejected() {
        let root = temp_root("ids");
        assert!(turn_dir(&root, "..").is_none());
        assert!(turn_dir(&root, "../../etc").is_none());
        assert!(turn_dir(&root, "").is_none());
        assert!(turn_dir(&root, "qa-1750-1-ab12cd34").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_keeps_the_newest_turns_only() {
        let root = temp_root("prune");
        for i in 0..(KEEP_TURNS + 3) {
            let dir = root.join(format!("req-{i:02}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(MANIFEST), "[]").unwrap();
        }
        prune(&root);
        let left = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(left, KEEP_TURNS);
        // 最老的被删、最新的还在
        assert!(!root.join("req-00").exists());
        assert!(root.join(format!("req-{:02}", KEEP_TURNS + 2)).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Test-only twin of `agent_undo_files` that takes an explicit root.
    fn agent_undo_files_in(root: &Path, request_id: &str) -> Vec<UndoFile> {
        list_files(&turn_dir(root, request_id).unwrap())
    }
}
