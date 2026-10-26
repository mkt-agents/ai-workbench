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
        return Err("没有可用的工作区目录".into());
    }
    root.canonicalize().map_err(|e| format!("无法解析工作区路径: {e}"))
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where file and command tools may act this turn.
#[derive(Clone)]
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
        let root = self.root.as_ref().ok_or("没有选择工作区，不能读写文件或执行命令。")?;
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
            return Err("路径超出当前工作区".into());
        };
        if name == ".." || name == "." {
            return Err("路径超出当前工作区".into());
        }
        pending.push(name.to_os_string());
        match cursor.parent() {
            Some(parent) if parent != cursor => cursor = parent,
            _ => return Err("目录不存在".into()),
        }
    };
    let mut canon = existing.canonicalize().map_err(|e| e.to_string())?;
    if !canon.starts_with(root) {
        return Err("路径超出当前工作区".into());
    }
    for name in pending.iter().rev() {
        if name.is_empty() || name == ".." || name == "." {
            return Err("路径超出当前工作区".into());
        }
        canon.push(name);
    }
    Ok(canon)
}

/// Locations no permission level may write, delete or move.
pub(crate) struct Protected {
    /// Never written under: Windows, Program Files, this app's own data.
    pub trees: Vec<PathBuf>,
    /// 命令里出现这些目录就整条拒。**不含 Program Files**：`"C:\Program Files\nodejs\npm.cmd"`
    /// 是正常用法，写目标由 [`Protected::command_write_guard`] 单独判。
    pub command_trees: Vec<PathBuf>,
    /// Never deleted / moved themselves (but their contents are fine): home folder.
    pub exact: Vec<PathBuf>,
}

/// 归一化后的可比较键：剥掉 `\\?\` / `\\.\` 设备前缀、统一分隔符、去尾随点与空格、小写。
fn path_key(p: &Path) -> String {
    let mut s = p.display().to_string().replace('/', "\\");
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\\?\").or_else(|| s.strip_prefix(r"\\.\")) {
        s = rest.to_string();
    }
    // Win32 会忽略路径末尾的点与空格，字符串比较不会。
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    s.trim_end_matches('\\').to_lowercase()
}

/// 尽量解析成真实路径：8.3 短名、junction、符号链接都靠它展开。
/// 路径不存在时解析「存在的最长祖先」再拼回剩余部分（新文件也要能判）。
fn real_path(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path.to_path_buf();
    while let Some(parent) = cur.parent().map(Path::to_path_buf) {
        if let Some(name) = cur.file_name() {
            rest.push(name.to_os_string());
        }
        cur = parent;
        if let Ok(c) = std::fs::canonicalize(&cur) {
            let mut out = c;
            for name in rest.iter().rev() {
                out.push(name);
            }
            return out;
        }
    }
    path.to_path_buf()
}

/// 会往指定路径写的命令：它们的路径参数都当写目标看。
const WRITE_PROGRAMS: &[&str] = &[
    "copy",
    "xcopy",
    "robocopy",
    "move",
    "ren",
    "rename",
    "mkdir",
    "md",
    "mklink",
    "tee",
    "set-content",
    "add-content",
    "out-file",
    "new-item",
    "copy-item",
    "move-item",
    "expand-archive",
    "compress-archive",
];

impl Protected {
    pub(crate) fn from_env() -> Self {
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let mut trees: Vec<PathBuf> = Vec::new();
        let mut command_trees: Vec<PathBuf> = Vec::new();
        for key in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            let Some(path) = env(key) else { continue };
            // Program Files 里有 npm / git 这些正常工具，只能判「写目标」，不能一见就拒
            if key == "SystemRoot" {
                command_trees.push(path.clone());
            }
            trees.push(path);
        }
        if let Some(appdata) = env("APPDATA") {
            let dir = appdata.join("com.ai-workbench.app");
            trees.push(dir.clone());
            command_trees.push(dir);
        }
        // 用户目录下的凭证：写进去等于把密钥交出去
        let home = home_dir();
        for name in [".ssh", ".aws", ".npmrc", ".config", ".docker", ".gitconfig"] {
            let dir = home.join(name);
            trees.push(dir.clone());
            command_trees.push(dir);
        }
        Self { trees, command_trees, exact: vec![home] }
    }

    /// `Some(reason)` when `path` must not be touched. `destructive` = delete / move source.
    pub(crate) fn check(&self, path: &Path, destructive: bool) -> Option<String> {
        // 网络共享先按字面拒：`canonicalize` 一个不可达的 UNC 路径会卡住几秒。
        if path_key(path).starts_with(r"\\") {
            return Some("不能修改网络共享（UNC）路径上的文件。".into());
        }
        // 再解析真实路径：`\\?\C:\Windows\x`、8.3 短名 `C:\PROGRA~1\`、指向系统目录的
        // junction / 符号链接，字面比较都拦不住。
        let real = real_path(path);
        let target = path_key(&real);
        // Git's own state (index, MERGE_HEAD, refs, config) only changes through git.
        if real.components().any(|c| c.as_os_str().eq_ignore_ascii_case(".git")) {
            return Some("不能直接改 .git 目录里的文件，仓库状态只能通过 git 命令改变。".into());
        }
        // canonicalize 之后才暴露出来的共享路径
        if target.starts_with(r"\\") {
            return Some("不能修改网络共享（UNC）路径上的文件。".into());
        }
        for tree in &self.trees {
            let t = path_key(&real_path(tree));
            if !t.is_empty() && (target == t || target.starts_with(&format!("{t}\\"))) {
                return Some(format!("{} 是受保护的系统或应用目录，任何权限下都不能修改。", tree.display()));
            }
        }
        if destructive {
            // A drive root (`C:` / `C:\`) or the home folder itself.
            let is_root = real.parent().is_none() || target.len() <= 2;
            if is_root || self.exact.iter().any(|p| path_key(&real_path(p)) == target) {
                return Some("不能删除或移动磁盘根目录和用户主目录。".into());
            }
        }
        None
    }

    /// shell 的前置闸门：命令里提到受保护目录、或用环境变量指向系统目录、或写网络共享，
    /// 一律不执行。`copy a C:\Windows\x` 以前完全不受 `Protected` 约束。
    pub(crate) fn command_write_guard(&self, command: &str, cwd: &Path) -> Option<String> {
        let lower = command.replace('/', "\\").to_lowercase();
        for tree in &self.command_trees {
            let key = path_key(tree);
            if !key.is_empty() && lower.contains(&key) {
                return Some(format!(
                    "命令里出现了受保护目录「{}」，shell 不能直接改它。要读内容请用 read_file / list_dir。",
                    tree.display()
                ));
            }
        }
        // 环境变量间接指向系统目录：展开后就是上面那些树
        const ENV_ALIASES: &[&str] = &[
            "%systemroot%",
            "%windir%",
            "$env:systemroot",
            "$env:windir",
            "%appdata%",
            "$env:appdata",
        ];
        if ENV_ALIASES.iter().any(|alias| lower.contains(alias)) {
            return Some("命令通过环境变量指向系统目录，任何权限下都不执行。要读内容请用 read_file。".into());
        }
        // 网络共享：数据会离开本机
        if command
            .split_whitespace()
            .any(|w| w.trim_matches(['"', '\'']).starts_with(r"\\"))
        {
            return Some("命令里出现了网络共享（UNC）路径，任何权限下都不执行。".into());
        }
        // 写类命令的路径参数都是潜在写目标（含带空格、已加引号的路径）
        let words = crate::agent::split_words(command);
        let Some(first) = words.first() else { return None };
        if !WRITE_PROGRAMS.contains(&crate::agent::program_name(first).as_str()) {
            return None;
        }
        for word in &words[1..] {
            let token = word.trim_matches(['"', '\'']);
            // 开关（`/y`、`-Force`）不是路径
            if token.is_empty() || token.starts_with('/') || token.starts_with('-') {
                continue;
            }
            let path = if Path::new(token).is_absolute() { PathBuf::from(token) } else { cwd.join(token) };
            if let Some(reason) = self.check(&path, false) {
                return Some(reason);
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

/// Walk under `start`: `.gitignore` / `.git/info/exclude` / the user's global
/// ignore are honoured (a `target/` or `dist/` line in `.gitignore` used to be
/// ignored only because the name happened to be in `skip_dir`), plus the fixed
/// skip list for repos that do not ignore their own build output.
/// `visit` returning false stops the walk.
pub(crate) fn walk(
    start: &Path,
    request_id: &str,
    no_ignore: bool,
    mut visit: impl FnMut(&Path) -> bool,
) -> Result<(), String> {
    let mut builder = ignore::WalkBuilder::new(start);
    builder
        // 点文件照旧可以搜（.github / .env.example），旧实现同样不跳
        .hidden(false)
        .follow_links(false)
        // 不是 git 仓库的普通文件夹也读 .gitignore
        .require_git(false)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore)
        .filter_entry(|entry| entry.depth() == 0 || !skip_dir(&entry.file_name().to_string_lossy()));
    let mut seen = 0usize;
    for item in builder.build() {
        if is_cancelled(request_id) {
            return Err(crate::agent::cancelled_message());
        }
        let Ok(entry) = item else { continue };
        if entry.depth() == 0 || entry.path().is_dir() {
            continue;
        }
        seen += 1;
        if seen > WALK_CAP {
            return Ok(());
        }
        if !visit(entry.path()) {
            return Ok(());
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

/// 文件通配：gitignore 风格（`**` 可以跨目录；不带 `/` 的模式在任意层级按文件名匹配，
/// 旧实现只能按文件名逐段比对，`src/**/*.ts` 用不了）。
fn pattern_matcher(root: &Path, pattern: &str) -> Result<ignore::overrides::Override, String> {
    let mut builder = ignore::overrides::OverrideBuilder::new(root);
    builder.add(pattern).map_err(|e| format!("文件名模式无效: {e}"))?;
    builder.build().map_err(|e| format!("文件名模式无效: {e}"))
}

pub(crate) fn find_files(scope: &Scope, args: &Value, request_id: &str) -> Result<String, String> {
    let pattern = arg_str(args, "pattern");
    if pattern.is_empty() {
        return Err("缺少文件名模式".into());
    }
    let start = scope.resolve(&arg_str(args, "path"))?;
    let matcher = pattern_matcher(&start, &pattern)?;
    let no_ignore = arg_bool(args, "no_ignore");
    let mut hits = Vec::new();
    walk(&start, request_id, no_ignore, |path| {
        if matcher.matched(path, false).is_whitelist() {
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

/// How the hits are shaped: full lines (default), file names only, or per-file counts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrepMode {
    Content,
    Files,
    Count,
}

impl GrepMode {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "files" | "file" => Self::Files,
            "count" => Self::Count,
            _ => Self::Content,
        }
    }
}

struct GrepHit {
    lines: Vec<String>,
    /// 占用的全局额度：内容模式按命中行，files / count 按文件。
    used: usize,
}

/// One file's hits. `context` adds the surrounding lines (`path-line- text`),
/// `remaining` caps how many match lines this file may still report.
fn grep_file(
    shown: &str,
    text: &str,
    matcher: &Matcher,
    mode: GrepMode,
    context: usize,
    remaining: usize,
) -> GrepHit {
    let mut found: Vec<usize> = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if matcher.is_match(line) {
            found.push(idx);
            if mode == GrepMode::Content && found.len() >= remaining.max(1) {
                break;
            }
        }
    }
    if found.is_empty() {
        return GrepHit { lines: Vec::new(), used: 0 };
    }
    if mode == GrepMode::Files {
        return GrepHit { lines: vec![shown.to_string()], used: 1 };
    }
    if mode == GrepMode::Count {
        return GrepHit { lines: vec![format!("{shown}: {}", found.len())], used: 1 };
    }
    let all: Vec<&str> = text.lines().collect();
    // 合并匹配行 ±context 的区间，避免相邻命中把同一行印两遍
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &m in &found {
        let lo = m.saturating_sub(context);
        let hi = (m + context).min(all.len().saturating_sub(1));
        match ranges.last_mut() {
            Some(last) if lo <= last.1 + 1 => last.1 = last.1.max(hi),
            _ => ranges.push((lo, hi)),
        }
    }
    let mut lines = Vec::new();
    for (n, (lo, hi)) in ranges.iter().enumerate() {
        if n > 0 {
            lines.push("--".to_string());
        }
        for (i, line) in all.iter().enumerate().take(*hi + 1).skip(*lo) {
            let body: String = line.trim().chars().take(240).collect();
            let sep = if found.binary_search(&i).is_ok() { ':' } else { '-' };
            lines.push(format!("{shown}{sep}{}{sep} {body}", i + 1));
        }
    }
    GrepHit { lines, used: found.len() }
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
    let mode = GrepMode::parse(&arg_str(args, "mode"));
    let context = arg_usize(args, "context").unwrap_or(0).min(5);
    let max = arg_usize(args, "max").unwrap_or(GREP_CAP).clamp(1, 200);
    let no_ignore = arg_bool(args, "no_ignore");
    let start = scope.resolve(&arg_str(args, "path"))?;
    let mut hits: Vec<String> = Vec::new();
    let mut used = 0usize;
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
        let hit = grep_file(&scope.show(path), &text, &matcher, mode, context, max.saturating_sub(used));
        used += hit.used;
        hits.extend(hit.lines);
        used < max
    };
    if start.is_file() {
        scan(&start);
    } else {
        walk(&start, request_id, no_ignore, scan)?;
    }
    if hits.is_empty() {
        return Ok("没有匹配".into());
    }
    if used >= max {
        hits.push(format!("…(已到上限 {max} 条，缩小 pattern 或 path 再查)"));
    }
    Ok(hits.join("\n"))
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
    // Rewriting conflict markers is resolving the conflict, which is the user's call.
    if current.as_deref().is_some_and(crate::git_merge::has_conflict_markers) {
        return Err(format!(
            "{} 里有未解决的冲突标记（<<<<<<< / >>>>>>>），没有修改。请告诉用户在 Git 管理 → 提交页解决冲突。",
            scope.show(&path)
        ));
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
        return Err("不能删除整个工作区".into());
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
            command_trees: vec![PathBuf::from("C:\\Windows")],
            exact: vec![PathBuf::from("C:\\Users\\me")],
        };
        assert!(p.check(Path::new("C:\\Windows\\System32\\drivers\\etc\\x"), false).is_some());
        assert!(p.check(Path::new("c:/program files/app/a.dll"), false).is_some());
        assert!(p.check(Path::new("C:\\WindowsApps\\a"), false).is_none());
        assert!(p.check(Path::new("C:\\Users\\me"), true).is_some());
        assert!(p.check(Path::new("C:\\Users\\me"), false).is_none());
        assert!(p.check(Path::new("C:\\Users\\me\\Desktop\\a.txt"), true).is_none());
        assert!(p.check(Path::new("D:\\"), true).is_some());
        assert!(p.check(Path::new("D:\\repo\\.git\\MERGE_HEAD"), false).is_some());
        assert!(p.check(Path::new("D:\\repo\\.github\\ci.yml"), false).is_none());
    }

    /// 设备前缀 / UNC / 尾随点不能靠字面比较绕过；shell 走同一道闸门。
    #[test]
    fn protected_paths_resist_prefix_tricks() {
        let p = Protected {
            trees: vec![PathBuf::from("C:\\Windows")],
            command_trees: vec![PathBuf::from("C:\\Windows")],
            exact: vec![PathBuf::from("C:\\Users\\me")],
        };
        assert!(p.check(Path::new(r"\\?\C:\Windows\System32\a.dll"), false).is_some());
        assert!(p.check(Path::new(r"\\.\C:\Windows\a"), false).is_some());
        assert!(p.check(Path::new("C:\\Windows.\\a"), false).is_some());
        // UNC 不碰网络也能当场拒（canonicalize 会卡住）
        assert!(p.check(Path::new(r"\\srv\share\a"), false).is_some());

        let cwd = PathBuf::from("D:\\repo");
        assert!(p.command_write_guard("copy a.exe C:\\Windows\\x.exe", &cwd).is_some());
        assert!(p.command_write_guard("echo x > C:\\Windows\\x", &cwd).is_some());
        assert!(p.command_write_guard("copy a %SystemRoot%\\x", &cwd).is_some());
        assert!(p.command_write_guard(r"copy a \\srv\share\x", &cwd).is_some());
        // 正常命令不误伤：Program Files 里的工具照常跑
        assert!(p.command_write_guard("git status", &cwd).is_none());
        assert!(p.command_write_guard("npm run build", &cwd).is_none());
        assert!(p.command_write_guard("\"C:\\Program Files\\nodejs\\npm.cmd\" test", &cwd).is_none());
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
        let none = Protected { trees: vec![], command_trees: vec![], exact: vec![] };
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

    #[test]
    fn walk_honours_gitignore_and_skips_build_dirs() {
        let root = std::env::temp_dir().join(format!("aw-fs-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("generated")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
        std::fs::write(root.join("src").join("a.rs"), "a").unwrap();
        std::fs::write(root.join("generated").join("b.rs"), "b").unwrap();
        std::fs::write(root.join("node_modules").join("c.js"), "c").unwrap();
        let scope = Scope::new(Some(root.canonicalize().unwrap()), false);
        let files = find_files(&scope, &json!({ "pattern": "**/*.rs" }), "t").unwrap();
        assert!(files.contains("src/a.rs"), "{files}");
        // .gitignore 里的目录不再被搜到
        assert!(!files.contains("generated/b.rs"), "{files}");
        // 两种口径都能回来：no_ignore 关掉过滤
        let all = find_files(&scope, &json!({ "pattern": "**/*.rs", "no_ignore": true }), "t").unwrap();
        assert!(all.contains("generated/b.rs"), "{all}");
        // 硬编码跳过表仍然兜底（node_modules 不在 .gitignore 里）
        assert!(!all.contains("c.js"), "{all}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_modes_and_context_work() {
        let root = std::env::temp_dir().join(format!("aw-fs-grep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\ntwo\nneedle here\nfour\nfive\n").unwrap();
        std::fs::write(root.join("b.txt"), "nothing\n").unwrap();
        let scope = Scope::new(Some(root.canonicalize().unwrap()), false);
        let content = grep(&scope, &json!({ "pattern": "needle" }), "t").unwrap();
        assert!(content.contains("a.txt:3: needle here"), "{content}");
        let files = grep(&scope, &json!({ "pattern": "needle", "mode": "files" }), "t").unwrap();
        assert_eq!(files.trim(), "a.txt");
        let counts = grep(&scope, &json!({ "pattern": "needle", "mode": "count" }), "t").unwrap();
        assert_eq!(counts.trim(), "a.txt: 1");
        // context 带前后行，匹配行用 `:`、上下文行用 `-`
        let ctx = grep(&scope, &json!({ "pattern": "needle", "context": 1 }), "t").unwrap();
        assert!(ctx.contains("a.txt-2- two"), "{ctx}");
        assert!(ctx.contains("a.txt:3: needle here"), "{ctx}");
        assert!(ctx.contains("a.txt-4- four"), "{ctx}");
        assert!(!ctx.contains("a.txt-1- one"), "{ctx}");
        // max 生效并如实告知
        let capped = grep(&scope, &json!({ "pattern": "e", "regex": true, "max": 1 }), "t").unwrap();
        assert!(capped.contains("到上限"), "{capped}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
