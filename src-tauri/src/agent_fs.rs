//! AI hub agent file tools: path scope (repo-only vs whole machine), protected
//! locations, read / list / find / grep / write / edit / delete / move, and the
//! diff shown on the approval card.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::cancellation::is_cancelled;

const READ_CAP: usize = 20_000;
const READ_LINES_DEFAULT: usize = 400;
const WRITE_CAP: usize = 200_000;
const LIST_CAP: usize = 200;
const GREP_CAP: usize = 50;
const FIND_CAP: usize = 200;
/// Stop walking after this many entries so a machine-wide search cannot run forever.
const WALK_CAP: usize = 50_000;
const DIFF_CAP: usize = 4_000;

pub(crate) fn arg_str(args: &Value, key: &str) -> String {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}

fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).map(|n| n as usize)
}

fn arg_bool(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.trim(), "true" | "1" | "yes"),
        _ => false,
    }
}

pub(crate) fn repo_root(path: &str) -> Result<PathBuf, String> {
    let root = PathBuf::from(path);
    if !root.is_dir() {
        return Err("没有可用的仓库目录".into());
    }
    root.canonicalize().map_err(|e| format!("无法解析仓库路径: {e}"))
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where file and command tools may act this turn.
pub(crate) struct Scope {
    pub root: Option<PathBuf>,
    /// 完全权限: absolute paths anywhere; relative paths against `base`.
    pub anywhere: bool,
    /// Base for relative paths: the repo, else the home folder.
    pub base: PathBuf,
}

impl Scope {
    pub(crate) fn new(root: Option<PathBuf>, anywhere: bool) -> Self {
        let base = root.clone().unwrap_or_else(home_dir);
        Self { root, anywhere, base }
    }

    pub(crate) fn has_files(&self) -> bool {
        self.root.is_some() || self.anywhere
    }

    pub(crate) fn resolve(&self, user_path: &str) -> Result<PathBuf, String> {
        if self.anywhere {
            return Ok(resolve_free(&self.base, user_path));
        }
        let root = self.root.as_ref().ok_or("没有选择仓库，不能读写文件或执行命令。")?;
        resolve_in_repo(root, user_path)
    }

    /// Path as shown to the model: repo-relative when inside the repo.
    pub(crate) fn show(&self, path: &Path) -> String {
        if let Some(root) = &self.root {
            if let Ok(rel) = path.strip_prefix(root) {
                let s = rel.display().to_string();
                return if s.is_empty() { ".".into() } else { s.replace('\\', "/") };
            }
        }
        path.display().to_string()
    }
}

/// Lexical normalisation (no filesystem access): joins relative paths to `base`
/// and folds `.` / `..` so `D:\a\..\b` becomes `D:\b`.
pub(crate) fn resolve_free(base: &Path, user_path: &str) -> PathBuf {
    let raw = user_path.trim();
    let joined = if raw.is_empty() {
        base.to_path_buf()
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        base.join(raw)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::ParentDir => {
                // Never climb above a drive / root.
                if out.components().count() > 1 {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve `user_path` and reject anything outside `root`.
/// Missing parents are allowed so a write can create nested files.
pub(crate) fn resolve_in_repo(root: &Path, user_path: &str) -> Result<PathBuf, String> {
    let raw = user_path.trim();
    if raw.is_empty() {
        return Ok(root.to_path_buf());
    }
    let p = Path::new(raw);
    let joined = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    let mut pending = Vec::new();
    let mut cursor = joined.as_path();
    let existing = loop {
        if cursor.exists() {
            break cursor;
        }
        let Some(name) = cursor.file_name() else {
            return Err("路径超出当前仓库".into());
        };
        if name == ".." || name == "." {
            return Err("路径超出当前仓库".into());
        }
        pending.push(name.to_os_string());
        match cursor.parent() {
            Some(parent) if parent != cursor => cursor = parent,
            _ => return Err("目录不存在".into()),
        }
    };
    let mut canon = existing.canonicalize().map_err(|e| e.to_string())?;
    if !canon.starts_with(root) {
        return Err("路径超出当前仓库".into());
    }
    for name in pending.iter().rev() {
        if name.is_empty() || name == ".." || name == "." {
            return Err("路径超出当前仓库".into());
        }
        canon.push(name);
    }
    Ok(canon)
}

/// Locations no permission level may write, delete or move.
pub(crate) struct Protected {
    /// Never written under: Windows, Program Files, this app's own data.
    pub trees: Vec<PathBuf>,
    /// Never deleted / moved themselves (but their contents are fine): home folder.
    pub exact: Vec<PathBuf>,
}

impl Protected {
    pub(crate) fn from_env() -> Self {
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let mut trees: Vec<PathBuf> = ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
            .iter()
            .filter_map(|k| env(k))
            .collect();
        if let Some(appdata) = env("APPDATA") {
            trees.push(appdata.join("com.ai-workbench.app"));
        }
        Self { trees, exact: vec![home_dir()] }
    }

    /// `Some(reason)` when `path` must not be touched. `destructive` = delete / move source.
    pub(crate) fn check(&self, path: &Path, destructive: bool) -> Option<String> {
        let key = |p: &Path| p.display().to_string().replace('/', "\\").trim_end_matches('\\').to_lowercase();
        let target = key(path);
        for tree in &self.trees {
            let t = key(tree);
            if !t.is_empty() && (target == t || target.starts_with(&format!("{t}\\"))) {
                return Some(format!("{} 是受保护的系统或应用目录，任何权限下都不能修改。", tree.display()));
            }
        }
        if destructive {
            // A drive root (`C:` / `C:\`) or the home folder itself.
            let is_root = path.parent().is_none() || target.len() <= 2;
            if is_root || self.exact.iter().any(|p| key(p) == target) {
                return Some("不能删除或移动磁盘根目录和用户主目录。".into());
            }
        }
        None
    }
}

fn read_text(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.contains(&0) {
        return Err("这是二进制文件，未读取内容".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Numbered lines `offset..offset+limit` (1-based) with a header giving the total.
pub(crate) fn page_lines(text: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = offset.max(1).min(total.max(1));
    let mut out = String::new();
    let mut shown = 0usize;
    for (i, line) in lines.iter().enumerate().skip(start - 1).take(limit) {
        let row = format!("{:>6}|{line}\n", i + 1);
        if out.len() + row.len() > READ_CAP {
            break;
        }
        out.push_str(&row);
        shown += 1;
    }
    let end = start + shown.saturating_sub(1);
    let more = if end < total {
        format!("，还有 {} 行，用 offset={} 继续读", total - end, end + 1)
    } else {
        String::new()
    };
    format!("[第 {start}-{end} 行，共 {total} 行{more}]\n{out}")
}

pub(crate) fn read_file(scope: &Scope, args: &Value) -> Result<String, String> {
    let path = scope.resolve(&arg_str(args, "path"))?;
    if !path.is_file() {
        return Err("不是文件".into());
    }
    let text = read_text(&path)?;
    let offset = arg_usize(args, "offset").unwrap_or(1);
    let limit = arg_usize(args, "limit").unwrap_or(READ_LINES_DEFAULT).clamp(1, 2_000);
    Ok(page_lines(&text, offset, limit))
}

pub(crate) fn list_dir(scope: &Scope, args: &Value) -> Result<String, String> {
    let path = scope.resolve(&arg_str(args, "path"))?;
    if !path.is_dir() {
        return Err("不是目录".into());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let mark = if entry.path().is_dir() { "/" } else { "" };
        names.push(format!("{name}{mark}"));
        if names.len() >= LIST_CAP {
            break;
        }
    }
    names.sort();
    Ok(format!("{}:\n{}", scope.show(&path), names.join("\n")))
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | ".next" | "coverage" | "$Recycle.Bin" | "System Volume Information"
    )
}

/// Depth-first walk under `start`, skipping dependency / build folders.
fn walk(start: &Path, request_id: &str, mut visit: impl FnMut(&Path) -> bool) -> Result<(), String> {
    let mut stack = vec![start.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        if is_cancelled(request_id) {
            return Err(crate::agent::cancelled_message());
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            seen += 1;
            if seen > WALK_CAP {
                return Ok(());
            }
            let path = entry.path();
            if path.is_dir() {
                if !skip_dir(&entry.file_name().to_string_lossy()) {
                    stack.push(path);
                }
            } else if !visit(&path) {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Case-insensitive `*` / `?` wildcard match.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub(crate) fn find_files(scope: &Scope, args: &Value, request_id: &str) -> Result<String, String> {
    let pattern = arg_str(args, "pattern");
    if pattern.is_empty() {
        return Err("缺少文件名模式".into());
    }
    let start = scope.resolve(&arg_str(args, "path"))?;
    // A pattern with a slash matches the relative path, otherwise just the file name.
    let by_path = pattern.contains('/') || pattern.contains('\\');
    let pattern = pattern.replace('\\', "/");
    let mut hits = Vec::new();
    walk(&start, request_id, |path| {
        let rel = path.strip_prefix(&start).unwrap_or(path).display().to_string().replace('\\', "/");
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if glob_match(&pattern, if by_path { &rel } else { &name }) {
            hits.push(scope.show(path));
        }
        hits.len() < FIND_CAP
    })?;
    if hits.is_empty() {
        return Ok("没有找到匹配的文件".into());
    }
    let more = if hits.len() >= FIND_CAP { format!("\n（只列出前 {FIND_CAP} 个）") } else { String::new() };
    Ok(format!("{}{more}", hits.join("\n")))
}

enum Matcher {
    Literal(String),
    Regex(regex::Regex),
}

impl Matcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal(needle) => line.to_lowercase().contains(needle),
            Self::Regex(re) => re.is_match(line),
        }
    }
}

pub(crate) fn grep(scope: &Scope, args: &Value, request_id: &str) -> Result<String, String> {
    let pattern = arg_str(args, "pattern");
    if pattern.is_empty() {
        return Err("缺少搜索词".into());
    }
    let matcher = if arg_bool(args, "regex") {
        let re = regex::RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .size_limit(1 << 20)
            .build()
            .map_err(|e| format!("正则表达式无效: {e}"))?;
        Matcher::Regex(re)
    } else {
        Matcher::Literal(pattern.to_lowercase())
    };
    let glob = arg_str(args, "glob");
    let start = scope.resolve(&arg_str(args, "path"))?;
    let mut hits = Vec::new();
    let mut scan = |path: &Path| {
        if !glob.is_empty() {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if !glob_match(&glob, &name) {
                return true;
            }
        }
        let Ok(meta) = path.metadata() else { return true };
        if meta.len() > 1_000_000 {
            return true;
        }
        let Ok(text) = read_text(path) else { return true };
        for (idx, line) in text.lines().enumerate() {
            if matcher.is_match(line) {
                let shown: String = line.trim().chars().take(240).collect();
                hits.push(format!("{}:{}: {shown}", scope.show(path), idx + 1));
                if hits.len() >= GREP_CAP {
                    return false;
                }
            }
        }
        true
    };
    if start.is_file() {
        scan(&start);
    } else {
        walk(&start, request_id, scan)?;
    }
    if hits.is_empty() {
        Ok("没有匹配".into())
    } else {
        Ok(hits.join("\n"))
    }
}

/// Replace `old` with `new`. `old` must occur exactly once unless `replace_all`.
pub(crate) fn apply_edit(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String, String> {
    if old.is_empty() {
        return Err("old 不能为空；新建文件请用 write_file".into());
    }
    let count = content.matches(old).count();
    match count {
        0 => Err("文件里没找到 old 指定的文本（需要逐字一致，包括缩进和空白）。先用 read_file 看准原文。".into()),
        1 => Ok(content.replacen(old, new, 1)),
        n if replace_all => {
            let _ = n;
            Ok(content.replace(old, new))
        }
        n => Err(format!("old 在文件里出现了 {n} 处。多带几行上下文让它唯一，或设 replace_all=true 全部替换。")),
    }
}

/// Unified diff with 3 lines of context, clipped for the approval card.
pub(crate) fn unified_diff(label: &str, old: &str, new: &str) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    let text = diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{label}"), &format!("b/{label}"))
        .to_string();
    if text.chars().count() > DIFF_CAP {
        let mut clipped: String = text.chars().take(DIFF_CAP).collect();
        clipped.push_str("\n…（改动较多，只显示开头）");
        clipped
    } else {
        text
    }
}

/// `(+added, -removed)` line counts.
pub(crate) fn diff_stats(old: &str, new: &str) -> (usize, usize) {
    let diff = similar::TextDiff::from_lines(old, new);
    let (mut add, mut del) = (0, 0);
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => add += 1,
            similar::ChangeTag::Delete => del += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (add, del)
}

/// New content a write / edit would produce, plus the current one (`None` = new file).
fn planned_change(scope: &Scope, tool: &str, args: &Value) -> Result<(PathBuf, Option<String>, String), String> {
    let path = scope.resolve(&arg_str(args, "path"))?;
    let current = if path.is_file() { Some(read_text(&path)?) } else { None };
    let next = match tool {
        "write_file" => {
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.chars().count() > WRITE_CAP {
                return Err("内容过长".into());
            }
            content.to_string()
        }
        _ => {
            let Some(cur) = current.as_deref() else {
                return Err("文件不存在；新建文件请用 write_file".into());
            };
            let old = args.get("old").and_then(|v| v.as_str()).unwrap_or("");
            let new = args.get("new").and_then(|v| v.as_str()).unwrap_or("");
            apply_edit(cur, old, new, arg_bool(args, "replace_all"))?
        }
    };
    Ok((path, current, next))
}

/// What the approval card shows for a file change: a unified diff, or the new file.
pub(crate) fn change_preview(scope: &Scope, tool: &str, args: &Value) -> Option<String> {
    let (path, current, next) = planned_change(scope, tool, args).ok()?;
    let label = scope.show(&path);
    Some(match current {
        Some(cur) => unified_diff(&label, &cur, &next),
        None => unified_diff(&label, "", &next),
    })
}

pub(crate) fn write_or_edit(scope: &Scope, tool: &str, args: &Value, protected: &Protected) -> Result<String, String> {
    let (path, current, next) = planned_change(scope, tool, args)?;
    if let Some(reason) = protected.check(&path, false) {
        return Err(reason);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &next).map_err(|e| e.to_string())?;
    let (add, del) = diff_stats(current.as_deref().unwrap_or(""), &next);
    let verb = if current.is_some() { "已修改" } else { "已新建" };
    Ok(format!("{verb} {}（+{add} −{del} 行）", scope.show(&path)))
}

pub(crate) fn delete_path(scope: &Scope, args: &Value, protected: &Protected) -> Result<String, String> {
    let path = scope.resolve(&arg_str(args, "path"))?;
    if !path.exists() {
        return Err("路径不存在".into());
    }
    if scope.root.as_deref() == Some(path.as_path()) {
        return Err("不能删除整个仓库".into());
    }
    if let Some(reason) = protected.check(&path, true) {
        return Err(reason);
    }
    to_recycle_bin(&path)?;
    Ok(format!("已移到回收站：{}", scope.show(&path)))
}

pub(crate) fn move_path(scope: &Scope, args: &Value, protected: &Protected) -> Result<String, String> {
    let from = scope.resolve(&arg_str(args, "from"))?;
    let to = scope.resolve(&arg_str(args, "to"))?;
    if !from.exists() {
        return Err("源路径不存在".into());
    }
    if to.exists() {
        return Err("目标已存在；先删除或换个名字".into());
    }
    for (p, destructive) in [(&from, true), (&to, false)] {
        if let Some(reason) = protected.check(p, destructive) {
            return Err(reason);
        }
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&from, &to).map_err(|e| format!("移动失败（跨磁盘移动请先复制再删除）: {e}"))?;
    Ok(format!("已移动 {} → {}", scope.show(&from), scope.show(&to)))
}

#[cfg(windows)]
fn to_recycle_bin(path: &Path) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW,
    };
    // pFrom is a list of paths terminated by an extra NUL.
    let mut from: Vec<u16> = path.display().to_string().encode_utf16().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 || op.fAnyOperationsAborted.as_bool() {
        return Err(format!("移到回收站失败（代码 {code}）"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn to_recycle_bin(path: &Path) -> Result<(), String> {
    let _ = path;
    Err("当前系统不支持回收站".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_free_folds_dots_and_keeps_roots() {
        let base = PathBuf::from("C:\\work\\repo");
        assert_eq!(resolve_free(&base, "src\\a.ts"), PathBuf::from("C:\\work\\repo\\src\\a.ts"));
        assert_eq!(resolve_free(&base, "..\\other"), PathBuf::from("C:\\work\\other"));
        assert_eq!(resolve_free(&base, "D:\\x\\..\\y"), PathBuf::from("D:\\y"));
        assert_eq!(resolve_free(&base, "C:\\..\\..\\Windows"), PathBuf::from("C:\\Windows"));
        assert_eq!(resolve_free(&base, ""), base);
    }

    #[test]
    fn repo_scope_rejects_escapes() {
        let root = std::env::temp_dir().join(format!("aw-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let canon = root.canonicalize().unwrap();
        let scope = Scope::new(Some(canon.clone()), false);
        assert!(scope.resolve("src/new/a.txt").is_ok());
        assert!(scope.resolve("../outside.txt").is_err());
        assert!(scope.resolve("C:\\Windows\\win.ini").is_err());
        assert!(Scope::new(None, false).resolve("a.txt").is_err());
        let anywhere = Scope::new(Some(canon.clone()), true);
        assert_eq!(anywhere.resolve("C:\\Windows\\win.ini").unwrap(), PathBuf::from("C:\\Windows\\win.ini"));
        assert_eq!(anywhere.show(&canon.join("src").join("a.ts")), "src/a.ts");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn protected_paths_block_system_and_roots() {
        let p = Protected {
            trees: vec![PathBuf::from("C:\\Windows"), PathBuf::from("C:\\Program Files")],
            exact: vec![PathBuf::from("C:\\Users\\me")],
        };
        assert!(p.check(Path::new("C:\\Windows\\System32\\drivers\\etc\\x"), false).is_some());
        assert!(p.check(Path::new("c:/program files/app/a.dll"), false).is_some());
        assert!(p.check(Path::new("C:\\WindowsApps\\a"), false).is_none());
        assert!(p.check(Path::new("C:\\Users\\me"), true).is_some());
        assert!(p.check(Path::new("C:\\Users\\me"), false).is_none());
        assert!(p.check(Path::new("C:\\Users\\me\\Desktop\\a.txt"), true).is_none());
        assert!(p.check(Path::new("D:\\"), true).is_some());
    }

    #[test]
    fn edit_requires_a_unique_match() {
        assert_eq!(apply_edit("a b a", "b", "c", false).unwrap(), "a c a");
        assert!(apply_edit("a b a", "a", "c", false).unwrap_err().contains("2 处"));
        assert_eq!(apply_edit("a b a", "a", "c", true).unwrap(), "c b c");
        assert!(apply_edit("abc", "x", "y", false).is_err());
        assert!(apply_edit("abc", "", "y", false).is_err());
    }

    #[test]
    fn diff_shows_changed_lines() {
        let d = unified_diff("a.txt", "one\ntwo\nthree\n", "one\n2\nthree\n");
        assert!(d.contains("--- a/a.txt") && d.contains("-two") && d.contains("+2"));
        assert_eq!(diff_stats("one\ntwo\n", "one\n2\nthree\n"), (2, 1));
    }

    #[test]
    fn paging_numbers_lines_and_says_whats_left() {
        let text = "a\nb\nc\nd\n";
        let page = page_lines(text, 2, 2);
        assert!(page.starts_with("[第 2-3 行，共 4 行，还有 1 行，用 offset=4 继续读]"));
        assert!(page.contains("     2|b") && page.contains("     3|c") && !page.contains("|d"));
        assert!(page_lines(text, 1, 10).starts_with("[第 1-4 行，共 4 行]"));
    }

    #[test]
    fn glob_matches_names_case_insensitively() {
        assert!(glob_match("*.TS", "app.ts"));
        assert!(glob_match("use*Stream.ts", "useAskStream.ts"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("*.rs", "main.rs.bak"));
        assert!(glob_match("src/*/a.rs", "src/x/a.rs"));
    }

    #[test]
    fn file_tools_round_trip_in_a_temp_repo() {
        let root = std::env::temp_dir().join(format!("aw-fs-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let scope = Scope::new(Some(root.canonicalize().unwrap()), false);
        let none = Protected { trees: vec![], exact: vec![] };
        let msg = write_or_edit(&scope, "write_file", &json!({ "path": "n/a.txt", "content": "x\ny\n" }), &none).unwrap();
        assert!(msg.contains("已新建") && msg.contains("+2"));
        let preview = change_preview(&scope, "edit_file", &json!({ "path": "n/a.txt", "old": "y", "new": "z" })).unwrap();
        assert!(preview.contains("-y") && preview.contains("+z"));
        write_or_edit(&scope, "edit_file", &json!({ "path": "n/a.txt", "old": "y", "new": "z" }), &none).unwrap();
        assert!(read_file(&scope, &json!({ "path": "n/a.txt" })).unwrap().contains("     2|z"));
        assert!(grep(&scope, &json!({ "pattern": "^z$", "regex": true }), "t").unwrap().contains("n/a.txt:2"));
        assert!(find_files(&scope, &json!({ "pattern": "*.txt" }), "t").unwrap().contains("n/a.txt"));
        move_path(&scope, &json!({ "from": "n/a.txt", "to": "n/b.txt" }), &none).unwrap();
        assert!(root.join("n").join("b.txt").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
}
