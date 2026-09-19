use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitConfig {
    pub scope: String,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoGitConfig {
    pub repo_path: String,
    pub name: String,
    pub email: String,
}

fn validate_email(email: &str) -> Result<(), String> {
    if email.is_empty() {
        return Err("邮箱不能为空".to_string());
    }
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err("邮箱格式无效".to_string());
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("名称不能为空".to_string());
    }
    Ok(())
}

fn git_command() -> Command {
    let mut cmd = Command::new("git");
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn map_git_spawn_err(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "未检测到 Git，请安装并加入 PATH".to_string()
    } else {
        format!("执行 git 失败: {}", e)
    }
}

fn is_git_work_tree(repo_path: &Path) -> Result<bool, String> {
    let output = git_command()
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(repo_path)
        .output()
        .map_err(map_git_spawn_err)?;
    if !output.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.trim() == "true")
}

fn ensure_git_repo(repo_path: &str) -> Result<(), String> {
    let path = Path::new(repo_path);
    if !path.exists() {
        return Err(format!("路径不存在: {}", repo_path));
    }
    if !path.is_dir() {
        return Err(format!("路径不是目录: {}", repo_path));
    }
    if !is_git_work_tree(path)? {
        return Err(format!("不是 Git 仓库: {}", repo_path));
    }
    Ok(())
}

fn git_output(repo_path: &str, args: &[&str]) -> Result<std::process::Output, String> {
    git_command()
        .args(args)
        .current_dir(repo_path)
        .output()
        .map_err(map_git_spawn_err)
}

fn git_stdout(repo_path: &str, args: &[&str]) -> Result<String, String> {
    let output = git_output(repo_path, args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    // Prefer trim_end: porcelain lines may start with a significant space (e.g. " M file").
    // Full trim() would corrupt status XY columns and make unstaged-only changes disappear.
    Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
}

/// How long a single git call may run inside the batch summary before it is
/// killed. Reads only, so killing is always safe.
const GIT_BATCH_TIMEOUT_SECS: u64 = 4;

/// Run git with a hard deadline and captured pipes.
///
/// The batch summary runs many repos in one IPC, so one wedged call — a stale
/// `index.lock`, an offline network share, a credential prompt — would hang
/// every other repo and tie up blocking-pool threads, and the cache-first UI
/// would surface that as "nothing ever refreshes".
fn git_output_timed(
    repo_path: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut child = git_command()
        .args(args)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(map_git_spawn_err)?;

    let (tx, rx) = std::sync::mpsc::channel::<(usize, Vec<u8>)>();
    // Drain both pipes on their own threads: a full 64KB stderr buffer would
    // otherwise deadlock git before it can exit.
    if let Some(mut pipe) = child.stdout.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send((0usize, buf));
        });
    }
    if let Some(mut pipe) = child.stderr.take() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send((1usize, buf));
        });
    }
    drop(tx);

    let verb = args.first().copied().unwrap_or("");
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "git {verb} 超时（>{}s），已跳过",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(format!("等待 git {verb} 失败: {}", e));
            }
        }
    };

    // Pipes hit EOF once the child is reaped, so this normally returns at once.
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Ok((slot, buf)) = rx.recv_timeout(Duration::from_millis(500)) {
        if slot == 0 {
            stdout = buf;
        } else {
            stderr = buf;
        }
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// `git_stdout` variant that reports a non-zero exit as an error carrying stderr.
fn git_stdout_timed(repo_path: &str, args: &[&str]) -> Result<String, String> {
    let output = git_output_timed(repo_path, args, Duration::from_secs(GIT_BATCH_TIMEOUT_SECS))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    while text.ends_with('\n') || text.ends_with('\r') {
        text.pop();
    }
    Ok(text)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRepoSummary {
    pub path: String,
    pub name: String,
    pub branch: String,
    pub dirty_count: u32,
    pub ahead: u32,
    pub behind: u32,
    pub has_upstream: bool,
    pub user_name: String,
    pub user_email: String,
    pub is_git: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusEntry {
    pub path: String,
    pub index_status: String,
    pub work_tree_status: String,
    pub group: String, // staged | unstaged | untracked
}

fn repo_display_name(repo_path: &str) -> String {
    Path::new(repo_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(repo_path)
        .to_string()
}

fn summarize_repo_sync(repo_path: String) -> GitRepoSummary {
    let name = repo_display_name(&repo_path);
    if let Err(e) = ensure_git_repo(&repo_path) {
        return GitRepoSummary {
            path: repo_path,
            name,
            branch: String::new(),
            dirty_count: 0,
            ahead: 0,
            behind: 0,
            has_upstream: false,
            user_name: String::new(),
            user_email: String::new(),
            is_git: false,
            error: Some(e),
        };
    }

    let branch = git_stdout(&repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| "HEAD".into());

    let dirty_count = git_porcelain_status(&repo_path)
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .count() as u32
        })
        .unwrap_or(0);

    let (ahead, behind, has_upstream) =
        match git_stdout(&repo_path, &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"]) {
            Ok(s) => {
                // left = commits on upstream not in HEAD (behind), right = commits on HEAD not in upstream (ahead)
                let mut parts = s.split_whitespace();
                let left: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                let right: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                (right, left, true)
            }
            Err(_) => (0, 0, false),
        };

    let user_name = git_stdout(&repo_path, &["config", "user.name"]).unwrap_or_default();
    let user_email = git_stdout(&repo_path, &["config", "user.email"]).unwrap_or_default();

    GitRepoSummary {
        path: repo_path,
        name,
        branch,
        dirty_count,
        ahead,
        behind,
        has_upstream,
        user_name,
        user_email,
        is_git: true,
        error: None,
    }
}

/// One repo's card state, gathered with two git calls instead of six.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBatchItem {
    pub path: String,
    pub name: String,
    pub branch: String,
    pub dirty_count: u32,
    pub ahead: u32,
    pub behind: u32,
    pub has_upstream: bool,
    pub has_commits: bool,
    pub user_name: String,
    pub user_email: String,
    pub origin_url: String,
    pub status: Vec<GitStatusEntry>,
    pub is_git: bool,
    pub error: Option<String>,
}

/// Parsed `git status --porcelain=v2 -z --branch -uall`.
#[derive(Debug, Default)]
struct V2Status {
    branch: String,
    upstream: Option<String>,
    ahead: u32,
    behind: u32,
    /// `# branch.ab` present at all — the pair (upstream, ab) is what v1's
    /// successful `rev-list` meant; upstream alone can show while the branch
    /// has no commits, and treating that as pushable invites a false push.
    ab_seen: bool,
    /// `# branch.oid` all-zero: a branch with no commits yet. Nothing can be
    /// undone or pushed there, so the UI must know before offering either.
    unborn: bool,
    entries: Vec<GitStatusEntry>,
}

/// Porcelain v2 writes `.` where v1 writes a space for "unchanged" (the fields
/// are space-separated, so a literal space would break parsing). Normalising
/// back keeps every downstream comparison identical to the v1 path.
fn normalize_xy(c: char) -> char {
    if c == '.' {
        ' '
    } else {
        c
    }
}

fn status_group(index: char, work: char) -> &'static str {
    let index = normalize_xy(index);
    let work = normalize_xy(work);
    if index == '?' && work == '?' {
        "untracked"
    } else if index != ' ' && index != '?' {
        "staged"
    } else {
        "unstaged"
    }
}

fn entry_from_xy(xy: &str, path: String) -> Option<GitStatusEntry> {
    let mut chars = xy.chars();
    let index = normalize_xy(chars.next()?);
    let work = normalize_xy(chars.next()?);
    if !is_porcelain_status_char(index) || !is_porcelain_status_char(work) || path.is_empty() {
        return None;
    }
    Some(GitStatusEntry {
        path,
        index_status: index.to_string(),
        work_tree_status: work.to_string(),
        group: status_group(index, work).to_string(),
    })
}

/// Split one porcelain record into the changelist rows the UI expects: a path
/// that is both staged and modified yields two rows (shared with `git_status`).
fn expand_porcelain_entry(e: GitStatusEntry) -> Vec<GitStatusEntry> {
    let mut out = Vec::new();
    if e.index_status != " " && e.index_status != "?" {
        out.push(GitStatusEntry {
            path: e.path.clone(),
            index_status: e.index_status.clone(),
            work_tree_status: " ".into(),
            group: "staged".into(),
        });
    }
    if e.work_tree_status != " " && e.work_tree_status != "?" && e.group != "untracked" {
        out.push(GitStatusEntry {
            path: e.path.clone(),
            index_status: " ".into(),
            work_tree_status: e.work_tree_status.clone(),
            group: "unstaged".into(),
        });
    }
    if e.group == "untracked" {
        out.push(e);
    }
    out
}

/// Parse NUL-separated porcelain v2 output. `-z` moves rename/unmerged path
/// fields into their own tokens, so the record type decides how many tokens to
/// consume — splitting on newlines instead would mis-align everything after a
/// renamed file.
fn parse_status_v2_z(raw: &str) -> V2Status {
    let mut out = V2Status::default();
    let mut tokens = raw.split('\0').filter(|t| !t.is_empty());

    while let Some(token) = tokens.next() {
        if let Some(header) = token.strip_prefix("# ") {
            let mut parts = header.splitn(2, ' ');
            let key = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("").trim();
            match key {
                "branch.head" => out.branch = value.to_string(),
                "branch.oid" => {
                    out.unborn = value.is_empty() || value.chars().all(|c| c == '0');
                }
                "branch.upstream" => {
                    if !value.is_empty() {
                        out.upstream = Some(value.to_string());
                    }
                }
                "branch.ab" => {
                    let mut seen = false;
                    for field in value.split_whitespace() {
                        if let Some(n) = field.strip_prefix('+') {
                            out.ahead = n.parse().unwrap_or(0);
                            seen = true;
                        } else if let Some(n) = field.strip_prefix('-') {
                            out.behind = n.parse().unwrap_or(0);
                            seen = true;
                        }
                    }
                    out.ab_seen = seen;
                }
                _ => {}
            }
            continue;
        }

        let kind = token.chars().next().unwrap_or(' ');
        // (space-separated field count including the trailing path, and how
        //  many further path tokens `-z` split onto their own lines).
        let (fields, trailing_paths) = match kind {
            '1' => (9, 0usize), // 1 XY sub mH mI mW hH hI <path>
            '2' => (10, 1),    // 2 XY sub mH mI mW hH hI X<score> <path> + <origPath>
            'u' => (10, 2),    // u XY sub mH mI mW iH iI iA <p1> + <p2> <p3>
            '?' => (2, 0),     // ? <path>  — no XY field at all
            _ => continue,     // '!' ignored submodule, or anything unknown
        };
        let pieces: Vec<&str> = token.splitn(fields, ' ').collect();
        if pieces.len() < fields {
            continue; // malformed record; never guess a path
        }
        // Untracked records carry no XY; v1 spelled them "??".
        let xy = if kind == '?' { "??" } else { pieces[1] };
        // `splitn` leaves the whole remainder in the final piece, so a path
        // containing spaces survives intact.
        let path = unquote_porcelain_path(pieces[fields - 1]);
        // The remaining paths of a rename/unmerged record must still be
        // consumed, or the next iteration would read them as fresh records.
        for _ in 0..trailing_paths {
            if tokens.next().is_none() {
                break;
            }
        }
        if let Some(entry) = entry_from_xy(xy, path) {
            out.entries.push(entry);
        }
    }
    out
}

/// Parse `git config --get-regexp` output ("key value" per line). Git lists
/// matches in increasing precedence, so the last value per key is the effective
/// one — the same answer `git config <key>` gives.
fn parse_config_pairs(raw: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in raw.lines() {
        let line = line.trim_end_matches(['\r']);
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(' ') {
            map.insert(key.to_string(), value.trim().to_string());
        } else {
            map.insert(line.to_string(), String::new());
        }
    }
    map
}

fn is_not_a_git_repo(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    // Only a genuine "not a repository" answers is_git=false: a permission or
    // offline-share error must not turn the card into a removable non-repo.
    lower.contains("not a git repository") || lower.contains("not inside a git")
}

fn is_unknown_option(stderr: &str) -> bool {
    stderr.to_ascii_lowercase().contains("unknown option")
}

/// Two git calls per repo: porcelain v2 (branch/upstream/ahead-behind/changes)
/// and one config regexp (identity + origin URL).
fn collect_repo_batch_item(
    repo_path: String,
    include_status: bool,
) -> Result<RepoBatchItem, String> {
    let name = repo_display_name(&repo_path);
    let status_args = ["status", "--porcelain=v2", "-z", "--branch", "-uall"];

    let v2 = match git_stdout_timed(&repo_path, &status_args) {
        Ok(raw) => parse_status_v2_z(&raw),
        Err(e) if is_not_a_git_repo(&e) => {
            return Ok(RepoBatchItem {
                path: repo_path,
                name,
                branch: String::new(),
                dirty_count: 0,
                ahead: 0,
                behind: 0,
                has_upstream: false,
                has_commits: false,
                user_name: String::new(),
                user_email: String::new(),
                origin_url: String::new(),
                status: Vec::new(),
                is_git: false,
                error: Some(e),
            });
        }
        // git < 2.11 has no porcelain v2: keep the old three-call path working.
        Err(e) if is_unknown_option(&e) => {
            let summary = summarize_repo_sync(repo_path.clone());
            let entries = if include_status {
                git_status_sync(repo_path.clone()).unwrap_or_default()
            } else {
                Vec::new()
            };
            return Ok(RepoBatchItem {
                path: summary.path,
                name: summary.name,
                branch: summary.branch,
                dirty_count: summary.dirty_count,
                ahead: summary.ahead,
                behind: summary.behind,
                has_upstream: summary.has_upstream,
                has_commits: true,
                user_name: summary.user_name,
                user_email: summary.user_email,
                origin_url: git_stdout(&repo_path, &["config", "--get", "remote.origin.url"])
                    .unwrap_or_default(),
                status: entries,
                is_git: summary.is_git,
                error: summary.error.or(Some(e)),
            });
        }
        Err(e) => return Err(e),
    };

    let config_raw = git_stdout_timed(
        &repo_path,
        &[
            "config",
            "--get-regexp",
            r"^(user\.name|user\.email|remote\.origin\.url)$",
        ],
    )
    .unwrap_or_default();
    let config = parse_config_pairs(&config_raw);

    let status: Vec<GitStatusEntry> = if include_status {
        v2.entries
            .clone()
            .into_iter()
            .flat_map(expand_porcelain_entry)
            .collect()
    } else {
        Vec::new()
    };

    Ok(RepoBatchItem {
        path: repo_path,
        name,
        branch: v2.branch,
        dirty_count: v2.entries.len() as u32,
        ahead: v2.ahead,
        behind: v2.behind,
        has_upstream: v2.upstream.is_some() && v2.ab_seen,
        has_commits: !v2.unborn,
        user_name: config.get("user.name").cloned().unwrap_or_default(),
        user_email: config.get("user.email").cloned().unwrap_or_default(),
        origin_url: config.get("remote.origin.url").cloned().unwrap_or_default(),
        status,
        is_git: true,
        error: None,
    })
}

/// How many repos are summarized in parallel inside one IPC.
const GIT_SUMMARIZE_WORKERS: usize = 8;

/// A failed repo must not sink the other 39 in the batch: report it per item.
fn batch_error_item(repo_path: String, error: String) -> RepoBatchItem {
    RepoBatchItem {
        name: repo_display_name(&repo_path),
        path: repo_path,
        branch: String::new(),
        dirty_count: 0,
        ahead: 0,
        behind: 0,
        has_upstream: false,
        has_commits: true,
        user_name: String::new(),
        user_email: String::new(),
        origin_url: String::new(),
        status: Vec::new(),
        // Unknown: never claim "not a git repo", that would offer destructive
        // UI affordances for a repo we merely could not read.
        is_git: true,
        error: Some(error),
    }
}

/// Batch card state for many repos in one IPC.
#[tauri::command]
pub async fn git_summarize_repos(
    paths: Vec<String>,
    include_status: bool,
) -> Result<Vec<RepoBatchItem>, String> {
    tokio::task::spawn_blocking(move || {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        // Contiguous chunks, joined in order: no shared mutable state, and the
        // result index still lines up with `paths`.
        let workers = GIT_SUMMARIZE_WORKERS.min(paths.len());
        let per = (paths.len() + workers - 1) / workers;
        let mut results: Vec<RepoBatchItem> = Vec::with_capacity(paths.len());
        std::thread::scope(|scope| {
            let handles: Vec<_> = paths
                .chunks(per)
                .map(|chunk| {
                    let chunk = chunk.to_vec();
                    scope.spawn(move || {
                        chunk
                            .into_iter()
                            .map(|path| {
                                collect_repo_batch_item(path.clone(), include_status)
                                    .unwrap_or_else(|error| batch_error_item(path, error))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            for handle in handles {
                if let Ok(items) = handle.join() {
                    results.extend(items);
                }
            }
        });
        Ok(results)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

fn is_porcelain_status_char(c: char) -> bool {
    // Porcelain v1 uses A/M/D/R/C/U/?/!/T/space; short-format also uses submodule
    // lowercase `m`. Accept the known set so we never silently drop lines.
    matches!(
        c,
        ' ' | 'M' | 'A' | 'D' | 'R' | 'C' | 'U' | '?' | '!' | 'T' | 'X' | 'm'
    )
}

/// Stable status listing for UI: always expand untracked files (like IDE changelists).
fn git_porcelain_status(repo_path: &str) -> Result<String, String> {
    git_stdout(repo_path, &["status", "--porcelain=v1", "-uall"])
}

/// Unquote git porcelain paths (`"a b"`, octal escapes like `\303\250`).
fn unquote_porcelain_path(raw: &str) -> String {
    let s = raw.trim();
    if !(s.starts_with('"') && s.ends_with('"') && s.len() >= 2) {
        return s.to_string();
    }
    let inner = &s[1..s.len() - 1];
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                c if c.is_ascii_digit() => {
                    let mut val: u8 = 0;
                    let mut n = 0;
                    while n < 3 && i + 1 + n < bytes.len() && bytes[i + 1 + n].is_ascii_digit() {
                        val = val
                            .wrapping_mul(8)
                            .wrapping_add(bytes[i + 1 + n] - b'0');
                        n += 1;
                    }
                    out.push(val);
                    i += 1 + n;
                }
                other => {
                    out.push(other);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_porcelain_line(line: &str) -> Option<GitStatusEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.len() < 3 || line.starts_with('#') {
        return None;
    }
    let mut chars = line.chars();
    let index_ch = chars.next()?;
    let work_ch = chars.next()?;
    if !is_porcelain_status_char(index_ch) || !is_porcelain_status_char(work_ch) {
        return None;
    }
    // XY + mandatory space, then path (byte index 3 is safe: status chars are ASCII)
    if line.as_bytes().get(2).copied() != Some(b' ') {
        return None;
    }
    let rest = line.get(3..)?.trim();
    if rest.is_empty() {
        return None;
    }
    // Handle rename "old -> new" (either side may be quoted)
    let path = if let Some(idx) = rest.find(" -> ") {
        unquote_porcelain_path(&rest[idx + 4..])
    } else {
        unquote_porcelain_path(rest)
    };

    let index_status = index_ch.to_string();
    let work_tree_status = work_ch.to_string();

    let group = if index_ch == '?' && work_ch == '?' {
        "untracked"
    } else if index_ch != ' ' && index_ch != '?' {
        "staged"
    } else {
        "unstaged"
    };

    Some(GitStatusEntry {
        path,
        index_status,
        work_tree_status,
        group: group.to_string(),
    })
}

fn has_staged_changes(repo_path: &str) -> Result<bool, String> {
    let output = git_output(repo_path, &["diff", "--cached", "--quiet"])?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(if stderr.trim().is_empty() {
                "检查暂存区失败".into()
            } else {
                stderr.trim().to_string()
            })
        }
    }
}

fn friendly_commit_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("no changes added to commit")
        || lower.contains("nothing to commit")
        || lower.contains("nothing added to commit")
    {
        return "没有已暂存的改动。请先暂存文件，或使用「暂存全部并提交」。".into();
    }
    let trimmed = raw.trim();
    if trimmed.chars().count() > 240 {
        let short: String = trimmed.chars().take(200).collect();
        format!("{short}…")
    } else {
        trimmed.to_string()
    }
}

fn git_status_sync(repo_path: String) -> Result<Vec<GitStatusEntry>, String> {
    ensure_git_repo(&repo_path)?;
    let out = git_porcelain_status(&repo_path)?;
    let mut entries = Vec::new();
    for line in out.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some(e) = parse_porcelain_line(line) else {
            // Do not silently drop — keep a best-effort unstaged entry when XY looks present.
            if line.as_bytes().get(2).copied() == Some(b' ') && line.len() > 3 {
                let path = unquote_porcelain_path(line[3..].trim());
                if !path.is_empty() {
                    entries.push(GitStatusEntry {
                        path,
                        index_status: " ".into(),
                        work_tree_status: "M".into(),
                        group: "unstaged".into(),
                    });
                }
            }
            continue;
        };
        // A file can appear as both staged and unstaged in one line (e.g. "MM")
        if e.index_status != " " && e.index_status != "?" {
            entries.push(GitStatusEntry {
                path: e.path.clone(),
                index_status: e.index_status.clone(),
                work_tree_status: " ".into(),
                group: "staged".into(),
            });
        }
        if e.work_tree_status != " " && e.work_tree_status != "?" && e.group != "untracked" {
            entries.push(GitStatusEntry {
                path: e.path.clone(),
                index_status: " ".into(),
                work_tree_status: e.work_tree_status.clone(),
                group: "unstaged".into(),
            });
        }
        if e.group == "untracked" {
            entries.push(e);
        }
    }
    Ok(entries)
}

fn git_stage_sync(repo_path: String, files: Vec<String>) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    if files.is_empty() {
        let output = git_output(&repo_path, &["add", "-A"])?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        return Ok("已暂存全部改动".into());
    }
    let mut args = vec!["add".to_string(), "--".to_string()];
    args.extend(files);
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = git_output(&repo_path, &str_args)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok("已暂存".into())
}

fn git_unstage_sync(repo_path: String, files: Vec<String>) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    if files.is_empty() {
        let output = git_output(&repo_path, &["restore", "--staged", "."])?;
        if !output.status.success() {
            // fallback for older git
            let output2 = git_output(&repo_path, &["reset", "HEAD", "--", "."])?;
            if !output2.status.success() {
                return Err(String::from_utf8_lossy(&output2.stderr).trim().to_string());
            }
        }
        return Ok("已取消全部暂存".into());
    }
    let mut args = vec!["restore".to_string(), "--staged".to_string(), "--".to_string()];
    args.extend(files.iter().cloned());
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = git_output(&repo_path, &str_args)?;
    if !output.status.success() {
        let mut args2 = vec!["reset".to_string(), "HEAD".to_string(), "--".to_string()];
        args2.extend(files);
        let str_args2: Vec<&str> = args2.iter().map(|s| s.as_str()).collect();
        let output2 = git_output(&repo_path, &str_args2)?;
        if !output2.status.success() {
            return Err(String::from_utf8_lossy(&output2.stderr).trim().to_string());
        }
    }
    Ok("已取消暂存".into())
}

fn git_commit_sync(repo_path: String, message: String) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    let msg = message.trim();
    if msg.is_empty() {
        return Err("提交说明不能为空".into());
    }
    if !has_staged_changes(&repo_path)? {
        return Err("没有已暂存的改动。请先暂存文件，或使用「暂存全部并提交」。".into());
    }
    let output = git_output(&repo_path, &["commit", "-m", msg])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{} {}", stderr.trim(), stdout.trim())
            .trim()
            .to_string();
        return Err(if combined.is_empty() {
            "提交失败".into()
        } else {
            friendly_commit_error(&combined)
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(if stdout.is_empty() {
        "提交成功".into()
    } else {
        stdout
    })
}

fn has_upstream(repo_path: &str) -> bool {
    git_stdout(repo_path, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).is_ok()
}

fn has_remote(repo_path: &str, name: &str) -> bool {
    git_stdout(repo_path, &["remote", "get-url", name]).is_ok()
}

fn friendly_push_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("could not read from remote")
        || lower.contains("permission denied")
        || lower.contains("authentication failed")
        || lower.contains("invalid credentials")
        || lower.contains("403")
        || lower.contains("401")
        || lower.contains("fatal: user cancelled dialog")
    {
        return "推送失败：远程认证失败，请检查凭据或 SSH 密钥".into();
    }
    if lower.contains("non-fast-forward")
        || lower.contains("fetch first")
        || lower.contains("updates were rejected")
        || lower.contains("failed to push some refs")
    {
        return "推送被拒绝：远程有更新的提交，请先拉取再推送".into();
    }
    if lower.contains("repository not found")
        || lower.contains("does not appear to be a git repository")
        || lower.contains("could not find remote")
    {
        return "推送失败：远程仓库不存在或无权访问".into();
    }
    if lower.contains("no upstream")
        || lower.contains("has no upstream branch")
        || (lower.contains("the current branch") && lower.contains("has no upstream"))
    {
        return "当前分支没有上游。请先配置远程（origin）后再推送。".into();
    }
    let trimmed = raw.trim();
    if trimmed.chars().count() > 240 {
        let short: String = trimmed.chars().take(200).collect();
        format!("{short}…")
    } else if trimmed.is_empty() {
        "推送失败".into()
    } else {
        trimmed.to_string()
    }
}

fn git_push_sync(repo_path: String) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;

    let remotes = git_stdout(&repo_path, &["remote"]).unwrap_or_default();
    if remotes.trim().is_empty() {
        return Err("没有配置远程仓库，请先添加 remote（例如 origin）".into());
    }

    let output = if has_upstream(&repo_path) {
        git_output(&repo_path, &["push"])?
    } else if has_remote(&repo_path, "origin") {
        git_output(&repo_path, &["push", "-u", "origin", "HEAD"])?
    } else {
        let first = remotes.lines().next().unwrap_or("").trim();
        if first.is_empty() {
            return Err("没有配置远程仓库，请先添加 remote".into());
        }
        return Err(format!(
            "当前分支没有上游，且未找到 origin。可用远程: {}。请先设置上游后再推送。",
            remotes.split_whitespace().collect::<Vec<_>>().join(", ")
        ));
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{} {}", stderr.trim(), stdout.trim())
            .trim()
            .to_string();
        return Err(friendly_push_error(&combined));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    // git push often prints progress to stderr even on success
    let msg = if !stdout.is_empty() {
        stdout
    } else if !stderr.is_empty() {
        stderr
    } else {
        "推送成功".into()
    };
    if msg.to_ascii_lowercase().contains("everything up-to-date") {
        Ok("已与远程同步，无需推送".into())
    } else {
        Ok(msg)
    }
}

fn validate_repo_rel_path(path: &str) -> Result<(), String> {
    let p = path.trim();
    if p.is_empty() {
        return Err("路径不能为空".into());
    }
    if p.contains('\0') || p.split(['/', '\\']).any(|seg| seg == "..") {
        return Err("非法路径".into());
    }
    let pb = Path::new(p);
    if pb.is_absolute() {
        return Err("请使用仓库内相对路径".into());
    }
    Ok(())
}

fn friendly_pull_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("not possible to fast-forward")
        || lower.contains("diverging branches")
        || lower.contains("need to specify how to reconcile")
        || lower.contains("cannot fast-forward")
    {
        return "拉取失败：本地与远程已分叉，无法快进合并。请在终端处理冲突或改用其它合并策略。".into();
    }
    if lower.contains("could not read from remote")
        || lower.contains("permission denied")
        || lower.contains("authentication failed")
        || lower.contains("403")
        || lower.contains("401")
    {
        return "拉取失败：远程认证失败，请检查凭据或 SSH 密钥".into();
    }
    if lower.contains("no upstream")
        || lower.contains("there is no tracking information")
        || (lower.contains("the current branch") && lower.contains("has no upstream"))
    {
        return "当前分支没有上游，无法拉取".into();
    }
    let trimmed = raw.trim();
    if trimmed.chars().count() > 240 {
        let short: String = trimmed.chars().take(200).collect();
        format!("{short}…")
    } else if trimmed.is_empty() {
        "拉取失败".into()
    } else {
        trimmed.to_string()
    }
}

fn truncate_diff_text(s: String) -> String {
    const MAX_BYTES: usize = 200_000;
    if s.len() <= MAX_BYTES {
        return s;
    }
    let mut end = MAX_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…\n\n[diff 过长，已截断]", &s[..end])
}

fn git_pull_sync(repo_path: String) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    if !has_upstream(&repo_path) {
        return Err("当前分支没有上游，无法拉取".into());
    }
    let output = git_output(&repo_path, &["pull", "--ff-only"])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{} {}", stderr.trim(), stdout.trim())
            .trim()
            .to_string();
        return Err(friendly_pull_error(&combined));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let msg = if !stdout.is_empty() {
        stdout
    } else if !stderr.is_empty() {
        stderr
    } else {
        "拉取成功".into()
    };
    if msg.to_ascii_lowercase().contains("already up to date") {
        Ok("已是最新，无需拉取".into())
    } else {
        Ok(msg)
    }
}

fn git_diff_sync(repo_path: String, file_path: String, staged: bool) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    validate_repo_rel_path(&file_path)?;

    if staged {
        let output = git_output(&repo_path, &["diff", "--cached", "--", &file_path])?;
        let code = output.status.code().unwrap_or(1);
        if code != 0 && code != 1 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(stderr.trim().to_string());
        }
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        return Ok(if text.trim().is_empty() {
            "（无差异）".into()
        } else {
            truncate_diff_text(text)
        });
    }

    let porcelain =
        git_stdout(&repo_path, &["status", "--porcelain=v1", "-uall", "--", &file_path])
            .unwrap_or_default();
    let is_untracked = porcelain.lines().any(|l| l.starts_with("??"));

    if is_untracked {
        #[cfg(windows)]
        let null_path = "NUL";
        #[cfg(not(windows))]
        let null_path = "/dev/null";
        let output = git_command()
            .args(["diff", "--no-index", "--", null_path, &file_path])
            .current_dir(&repo_path)
            .output()
            .map_err(map_git_spawn_err)?;
        let code = output.status.code().unwrap_or(1);
        // --no-index returns 1 when files differ; also may return 0
        if code > 1 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(if stderr.trim().is_empty() {
                "无法生成未跟踪文件的 diff".into()
            } else {
                stderr.trim().to_string()
            });
        }
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        return Ok(if text.trim().is_empty() {
            "（未跟踪文件，无文本 diff）".into()
        } else {
            truncate_diff_text(text)
        });
    }

    let output = git_output(&repo_path, &["diff", "--", &file_path])?;
    let code = output.status.code().unwrap_or(1);
    if code != 0 && code != 1 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(if text.trim().is_empty() {
        "（无差异）".into()
    } else {
        truncate_diff_text(text)
    })
}

fn git_discard_sync(
    repo_path: String,
    file_path: String,
    untracked: bool,
    staged: bool,
) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    validate_repo_rel_path(&file_path)?;

    if untracked {
        let output = git_output(&repo_path, &["clean", "-f", "--", &file_path])?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(if stderr.trim().is_empty() {
                "丢弃未跟踪文件失败".into()
            } else {
                stderr.trim().to_string()
            });
        }
        return Ok("已删除未跟踪文件".into());
    }

    if staged {
        // Reset both index and worktree to HEAD. `--worktree` alone leaves the
        // index untouched and, for a mixed file, deletes unstaged hunks instead.
        let output = git_output(
            &repo_path,
            &["restore", "--source=HEAD", "--staged", "--worktree", "--", &file_path],
        )?;
        if !output.status.success() {
            let output2 = git_output(&repo_path, &["checkout", "HEAD", "--", &file_path])?;
            if !output2.status.success() {
                let stderr = String::from_utf8_lossy(&output2.stderr);
                return Err(if stderr.trim().is_empty() {
                    "丢弃暂存改动失败".into()
                } else {
                    stderr.trim().to_string()
                });
            }
        }
        return Ok("已丢弃暂存改动".into());
    }

    let output = git_output(&repo_path, &["restore", "--worktree", "--", &file_path])?;
    if !output.status.success() {
        let output2 = git_output(&repo_path, &["checkout", "--", &file_path])?;
        if !output2.status.success() {
            let stderr = String::from_utf8_lossy(&output2.stderr);
            return Err(if stderr.trim().is_empty() {
                "丢弃改动失败".into()
            } else {
                stderr.trim().to_string()
            });
        }
    }
    Ok("已丢弃工作区改动".into())
}

/// Undo the last commit, IntelliJ-style: the strategy adapts to whether the tip is
/// already on the remote.
///
/// * not pushed (no upstream, or `ahead > 0`) -> `reset --soft HEAD~1`: the commit
///   disappears and its changes return to the index, ready to be amended.
/// * already on the remote -> `revert --no-edit HEAD`: a new commit that undoes the
///   tip is created. History is never rewritten, so the result can be pushed normally
///   instead of needing a force-push.
fn git_undo_last_commit_sync(repo_path: String) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;

    // "ahead == 0 with upstream" means the tip is already published.
    let mut published = false;
    if has_upstream(&repo_path) {
        if let Ok(s) = git_stdout(
            &repo_path,
            &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        ) {
            let mut parts = s.split_whitespace();
            let _behind: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let ahead: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            published = ahead == 0;
        }
    }

    if git_stdout(&repo_path, &["rev-parse", "--verify", "HEAD"]).is_err() {
        return Err("仓库还没有任何提交，无法回滚。".into());
    }

    if published {
        // Revert keeps the remote history intact. It refuses to run when the index or
        // worktree is dirty — that restriction is intentional (nothing gets clobbered).
        let output = git_output(&repo_path, &["revert", "--no-edit", "HEAD"])?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            // A conflicted revert leaves the repo mid-revert; roll it back so the user
            // can decide what to do instead of being stuck in REVERT_HEAD.
            let _ = git_output(&repo_path, &["revert", "--abort"]);
            let lower = stderr.to_lowercase();
            return Err(if lower.contains("local changes") || lower.contains("would be overwritten") {
                "工作区有未提交的改动，请先提交或暂存后再回滚已推送的提交。".into()
            } else if lower.contains("conflict") {
                "反向提交产生冲突，已自动取消；请手动处理该提交。".into()
            } else if stderr.is_empty() {
                "生成回滚提交失败".into()
            } else {
                stderr
            });
        }
        return Ok("已生成反向提交撤销该改动（历史未被改写，可直接推送）".into());
    }

    // Ensure HEAD~1 exists
    if git_stdout(&repo_path, &["rev-parse", "--verify", "HEAD~1"]).is_err() {
        return Err("没有可回滚的上次提交（仓库可能只有一个提交或为空）。".into());
    }

    let output = git_output(&repo_path, &["reset", "--soft", "HEAD~1"])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(if stderr.trim().is_empty() {
            "回滚上次提交失败".into()
        } else {
            stderr.trim().to_string()
        });
    }
    Ok("已回滚上次提交，改动保留在暂存区".into())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitContext {
    pub files: Vec<String>,
    pub diff: String,
    /// "staged" | "workdir"
    pub source: String,
}

fn truncate_context_text(s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…\n\n[已截断]", &s[..end])
}

fn git_commit_context_sync(repo_path: String) -> Result<GitCommitContext, String> {
    ensure_git_repo(&repo_path)?;
    const MAX: usize = 12 * 1024;

    let staged_names = git_stdout(&repo_path, &["diff", "--cached", "--name-only"]).unwrap_or_default();
    let staged_files: Vec<String> = staged_names
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if !staged_files.is_empty() {
        let output = git_output(&repo_path, &["diff", "--cached"])?;
        let code = output.status.code().unwrap_or(1);
        if code != 0 && code != 1 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(stderr.trim().to_string());
        }
        let diff = truncate_context_text(String::from_utf8_lossy(&output.stdout).to_string(), MAX);
        return Ok(GitCommitContext {
            files: staged_files,
            diff,
            source: "staged".into(),
        });
    }

    let work_names = git_stdout(&repo_path, &["diff", "--name-only"]).unwrap_or_default();
    let mut files: Vec<String> = work_names
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let porcelain = git_porcelain_status(&repo_path).unwrap_or_default();
    for line in porcelain.lines() {
        if line.starts_with("??") {
            let path = line.get(3..).unwrap_or("").trim();
            if !path.is_empty() && !files.iter().any(|f| f == path) {
                files.push(path.to_string());
            }
        }
    }

    if files.is_empty() {
        return Err("没有可分析的改动，请先修改或暂存文件".into());
    }

    let output = git_output(&repo_path, &["diff"])?;
    let code = output.status.code().unwrap_or(1);
    if code != 0 && code != 1 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    let mut diff = String::from_utf8_lossy(&output.stdout).to_string();
    let untracked: Vec<&str> = files
        .iter()
        .filter(|f| {
            porcelain.lines().any(|l| l.starts_with("??") && l.get(3..).map(|p| p.trim() == f.as_str()).unwrap_or(false))
        })
        .map(|s| s.as_str())
        .collect();
    if !untracked.is_empty() {
        diff.push_str("\n\n# Untracked files:\n");
        for u in untracked {
            diff.push_str("- ");
            diff.push_str(u);
            diff.push('\n');
        }
    }

    Ok(GitCommitContext {
        files,
        diff: truncate_context_text(diff, MAX),
        source: "workdir".into(),
    })
}

#[tauri::command]
pub async fn git_repo_summary(path: String) -> Result<GitRepoSummary, String> {
    tokio::task::spawn_blocking(move || Ok(summarize_repo_sync(path)))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_status(path: String) -> Result<Vec<GitStatusEntry>, String> {
    tokio::task::spawn_blocking(move || git_status_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_stage(path: String, files: Vec<String>) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_stage_sync(path, files))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_unstage(path: String, files: Vec<String>) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_unstage_sync(path, files))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_commit(path: String, message: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_commit_sync(path, message))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_push(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_push_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_pull(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_pull_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_diff(path: String, file_path: String, staged: bool) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_diff_sync(path, file_path, staged))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

/// Get recent commits (short format: hash + subject + relative date).
#[tauri::command]
pub async fn git_log(path: String, count: i64) -> Result<Vec<Vec<String>>, String> {
    tokio::task::spawn_blocking(move || {
        ensure_git_repo(&path)?;
        let count = count.clamp(1, 50);
        let output = git_output(
            &path,
            &[
                "log",
                &format!("-{}", count),
                "--pretty=format:%h|%s|%cr",
                "--no-merges",
            ],
        )?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(stderr.trim().to_string());
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let commits: Vec<Vec<String>> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| line.splitn(3, '|').map(String::from).collect())
            .collect();
        Ok(commits)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_discard(
    path: String,
    file_path: String,
    untracked: bool,
    staged: bool,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_discard_sync(path, file_path, untracked, staged))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_undo_last_commit(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_undo_last_commit_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_commit_context(path: String) -> Result<GitCommitContext, String> {
    tokio::task::spawn_blocking(move || git_commit_context_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_is_repo(path: String) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || {
        let p = Path::new(&path);
        if !p.is_dir() {
            return Ok(false);
        }
        is_git_work_tree(p)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

/// Extract the hostname from a git remote URL.
/// Handles HTTPS, SSH (`git@host:path`), and `ssh://` forms.
/// Returns None if the URL is empty or unparseable.
#[cfg(test)]
pub(crate) fn extract_host(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // SSH protocol form: ssh://[user@]host[:port]/path
    if url.contains("://") {
        let parsed = url::Url::parse(url).ok()?;
        let host = parsed.host_str()?;
        // url::Url preserves brackets for IPv6; strip them for matching.
        Some(host.trim_matches(['[', ']']).to_lowercase())
    } else if let Some(at) = url.find('@') {
        // SCP-like SSH: [user@]host:path
        let after = &url[at + 1..];
        let host_end = after.find(':').or_else(|| after.find('/'))?;
        let host = &after[..host_end];
        // Strip any port (host:port)
        let host = host.split(':').next().unwrap_or(host);
        if host.is_empty() {
            None
        } else {
            Some(host.to_lowercase())
        }
    } else {
        None
    }
}

/// Get the `origin` remote URL of a repo, or None if it has no origin.
#[tauri::command]
pub async fn git_remote_url(path: String) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || {
        let url = git_stdout(&path, &["remote", "get-url", "origin"])?;
        let trimmed = url.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitScannedRepo {
    pub path: String,
    pub name: String,
}

/// 扫描时不进入的目录（隐藏目录、依赖/构建产物等）
fn is_skip_dir(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "node_modules" | "target" | "dist" | "build" | "bin" | "obj" | "vendor" | "__pycache__"
    ) || name.starts_with('.')
}

fn is_git_dir_entry(path: &Path) -> bool {
    path.join(".git").exists()
}

/// 递归扫描目录下的 Git 仓库（找到含 .git 的目录即停，不继续深入）。
/// max_depth=1 表示只检查 root 的直接子目录；2 表示再往下看一层（如 packages/*/xxx）。
fn scan_git_repos_recursive(root: &Path, depth: u32, max_depth: u32, out: &mut Vec<GitScannedRepo>) -> Result<(), String> {
    if depth >= max_depth {
        return Ok(());
    }
    let entries = std::fs::read_dir(root).map_err(|e| format!("读取目录失败 {}: {}", root.display(), e))?;
    let mut child_dirs: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取目录项失败: {}", e))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // 跳过隐藏目录与垃圾目录（不进入）
        if is_skip_dir(&name) {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        if is_git_dir_entry(&path) {
            // 是仓库：收录且不再深入
            out.push(GitScannedRepo {
                path: path.to_string_lossy().to_string(),
                name,
            });
        } else {
            child_dirs.push(path);
        }
    }
    // 非仓库子目录继续向下扫（同层优先，避免深度震荡）
    for child in child_dirs {
        scan_git_repos_recursive(&child, depth + 1, max_depth, out)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn git_scan_repos(
    path: String,
    max_depth: Option<u32>,
) -> Result<Vec<GitScannedRepo>, String> {
    tokio::task::spawn_blocking(move || {
        let root = Path::new(&path);
        if !root.is_dir() {
            return Err(format!("路径不是目录: {}", path));
        }
        let max_depth = max_depth.unwrap_or(1);
        let mut out = Vec::new();
        scan_git_repos_recursive(root, 0, max_depth, &mut out)?;
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(out)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    fn mk_git(p: &Path) {
        std::fs::create_dir_all(p.join(".git")).unwrap();
    }
    fn mk_dir(p: &Path) {
        std::fs::create_dir_all(p).unwrap();
    }

    /// 深度 1：只找直接子目录中的仓库；跳过隐藏目录与 node_modules；仓库内部不再深入
    #[test]
    fn scan_depth1_finds_direct_children_only() {
        let tmp = std::env::temp_dir().join(format!("scan_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // 结构：root/{repoA(.git), plain/{repoB(.git)}, .hidden/{repoC(.git)}, node_modules/{repoD(.git)}, repoE/sub(.git 嵌套在 repoE 内不成立，repoE 本身无 .git)}
        mk_git(&tmp.join("repoA"));
        mk_git(&tmp.join("plain").join("repoB"));
        mk_git(&tmp.join(".hidden").join("repoC"));
        mk_git(&tmp.join("node_modules").join("repoD"));

        let mut out = Vec::new();
        scan_git_repos_recursive(&tmp, 0, 1, &mut out).unwrap();
        let names: Vec<&str> = out.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["repoA"], "depth1 只应找到直接子目录 repoA，实际: {:?}", names);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 深度 2：发现嵌套一层（plain/repoB）；垃圾目录与隐藏目录仍被跳过
    #[test]
    fn scan_depth2_finds_nested() {
        let tmp = std::env::temp_dir().join(format!("scan_test2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        mk_git(&tmp.join("repoA"));
        mk_git(&tmp.join("plain").join("repoB"));
        mk_git(&tmp.join(".hidden").join("repoC"));
        mk_git(&tmp.join("node_modules").join("repoD"));
        mk_git(&tmp.join("deep").join("sub").join("repoE")); // 3 层深

        let mut out = Vec::new();
        scan_git_repos_recursive(&tmp, 0, 2, &mut out).unwrap();
        let mut names: Vec<&str> = out.iter().map(|r| r.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["repoA", "repoB"], "depth2 应找到 repoA+repoB，实际: {:?}", names);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 仓库目录不再深入：repoX(.git)/inner(.git) 只收录 repoX
    #[test]
    fn scan_stops_at_repo() {
        let tmp = std::env::temp_dir().join(format!("scan_test3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        mk_git(&tmp.join("repoX").join("inner")); // repoX 含 .git 且其内还有 inner 仓库
        mk_git(&tmp.join("repoX"));

        let mut out = Vec::new();
        scan_git_repos_recursive(&tmp, 0, 2, &mut out).unwrap();
        let names: Vec<&str> = out.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["repoX"], "仓库内部不应再深入，实际: {:?}", names);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_host_https() {
        assert_eq!(extract_host("https://github.com/user/repo.git"), Some("github.com".to_string()));
        assert_eq!(extract_host("https://gitlab.company.com/group/project.git"), Some("gitlab.company.com".to_string()));
        assert_eq!(extract_host("http://gitea.local:3000/user/repo.git"), Some("gitea.local".to_string()));
    }

    #[test]
    fn extract_host_ssh() {
        assert_eq!(extract_host("git@github.com:user/repo.git"), Some("github.com".to_string()));
        assert_eq!(extract_host("git@gitlab.company.com:group/project.git"), Some("gitlab.company.com".to_string()));
        assert_eq!(extract_host("ssh://git@gitlab.company.com/user/repo.git"), Some("gitlab.company.com".to_string()));
        assert_eq!(extract_host("ssh://git@gitea.local:2222/user/repo.git"), Some("gitea.local".to_string()));
    }

    #[test]
    fn extract_host_invalid() {
        assert_eq!(extract_host(""), None);
        assert_eq!(extract_host("not-a-url"), None);
        assert_eq!(extract_host("/local/path"), None);
    }
}

fn set_git_config_sync(config: GitConfig) -> Result<String, String> {
    validate_name(&config.name)?;
    validate_email(&config.email)?;

    if config.scope == "local" {
        return Err(
            "local 作用域请使用「指定仓库」配置（set_repo_git_config），需提供仓库路径".to_string(),
        );
    }
    if config.scope != "global" {
        return Err("作用域必须是 global".to_string());
    }

    let output = git_command()
        .args(["config", "--global", "user.name", &config.name])
        .output()
        .map_err(map_git_spawn_err)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("设置 user.name 失败: {}", stderr.trim()));
    }

    let output = git_command()
        .args(["config", "--global", "user.email", &config.email])
        .output()
        .map_err(map_git_spawn_err)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("设置 user.email 失败: {}", stderr.trim()));
    }

    Ok(format!(
        "已设置全局 Git 配置: {} <{}>",
        config.name, config.email
    ))
}

fn set_repo_git_config_sync(config: RepoGitConfig) -> Result<String, String> {
    validate_name(&config.name)?;
    validate_email(&config.email)?;

    let repo_path = Path::new(&config.repo_path);

    if !repo_path.exists() {
        return Err(format!("路径不存在: {}", config.repo_path));
    }
    if !repo_path.is_dir() {
        return Err(format!("路径不是目录: {}", config.repo_path));
    }
    if !is_git_work_tree(repo_path)? {
        return Err(format!("不是 Git 仓库: {}", config.repo_path));
    }

    let output = git_command()
        .args(["config", "user.name", &config.name])
        .current_dir(&config.repo_path)
        .output()
        .map_err(map_git_spawn_err)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("设置 user.name 失败: {}", stderr.trim()));
    }

    let output = git_command()
        .args(["config", "user.email", &config.email])
        .current_dir(&config.repo_path)
        .output()
        .map_err(map_git_spawn_err)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("设置 user.email 失败: {}", stderr.trim()));
    }

    Ok(format!(
        "已设置仓库 Git 配置: {} <{}> @ {}",
        config.name, config.email, config.repo_path
    ))
}

fn get_repo_git_config_sync(repo_path: String) -> Result<(String, String), String> {
    let path = Path::new(&repo_path);
    if !path.exists() {
        return Err(format!("路径不存在: {}", repo_path));
    }
    if !path.is_dir() {
        return Err(format!("路径不是目录: {}", repo_path));
    }
    if !is_git_work_tree(path)? {
        return Err(format!("不是 Git 仓库: {}", repo_path));
    }

    let output = git_command()
        .args(["config", "user.name"])
        .current_dir(&repo_path)
        .output()
        .map_err(map_git_spawn_err)?;

    let name = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        String::new()
    };

    let output = git_command()
        .args(["config", "user.email"])
        .current_dir(&repo_path)
        .output()
        .map_err(map_git_spawn_err)?;

    let email = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        String::new()
    };

    Ok((name, email))
}

fn get_git_config_sync(scope: Option<String>) -> Result<(String, String), String> {
    let scope_flag = scope.unwrap_or_else(|| "global".to_string());
    if scope_flag != "global" {
        return Err("目前仅支持读取全局 Git 配置".to_string());
    }

    let output = git_command()
        .args(["config", "--global", "user.name"])
        .output()
        .map_err(map_git_spawn_err)?;

    let name = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        String::new()
    };

    let output = git_command()
        .args(["config", "--global", "user.email"])
        .output()
        .map_err(map_git_spawn_err)?;

    let email = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        String::new()
    };

    Ok((name, email))
}

#[tauri::command]
pub async fn set_git_config(config: GitConfig) -> Result<String, String> {
    tokio::task::spawn_blocking(move || set_git_config_sync(config))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn set_repo_git_config(config: RepoGitConfig) -> Result<String, String> {
    tokio::task::spawn_blocking(move || set_repo_git_config_sync(config))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn get_repo_git_config(repo_path: String) -> Result<(String, String), String> {
    tokio::task::spawn_blocking(move || get_repo_git_config_sync(repo_path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn get_git_config(scope: Option<String>) -> Result<(String, String), String> {
    tokio::task::spawn_blocking(move || get_git_config_sync(scope))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

/// Native folder picker. Returns None if the user cancels.
#[tauri::command]
pub async fn pick_directory() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(pick_directory_sync)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn pick_directory_sync() -> Result<Option<String>, String> {
    let folder = rfd::FileDialog::new()
        .set_title("选择 Git 仓库目录")
        .pick_folder();
    Ok(folder.map(|p| p.to_string_lossy().to_string()))
}

#[cfg(test)]
mod tests {
    use super::{parse_porcelain_line, validate_email, validate_name};

    #[test]
    fn validate_email_accepts_simple() {
        assert!(validate_email("a@b.com").is_ok());
    }

    #[test]
    fn validate_email_rejects_invalid() {
        assert!(validate_email("not-email").is_err());
        assert!(validate_email("").is_err());
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(validate_name("  ").is_err());
    }

    #[test]
    fn porcelain_unstaged_only_keeps_leading_space() {
        // Mimic git_stdout(trim_end): must NOT strip the leading status space.
        let raw = " M README.md\n";
        let out = raw.trim_end().to_string();
        let e = parse_porcelain_line(out.lines().next().unwrap()).expect("parse");
        assert_eq!(e.path, "README.md");
        assert_eq!(e.group, "unstaged");
        assert_eq!(e.work_tree_status, "M");
    }

    #[test]
    fn porcelain_full_trim_would_break_unstaged_only() {
        let broken = " M README.md".trim().to_string();
        assert!(
            parse_porcelain_line(&broken).is_none(),
            "full trim corrupts XY columns"
        );
    }

    #[test]
    fn porcelain_common_variants() {
        let cases = [
            ("M  staged.txt", "staged", "staged.txt"),
            ("MM both.txt", "staged", "both.txt"), // group before split is staged-first
            (" D deleted.txt", "unstaged", "deleted.txt"),
            ("?? new.txt", "untracked", "new.txt"),
            ("A  added.txt", "staged", "added.txt"),
            ("R  old.txt -> new.txt", "staged", "new.txt"),
            (" m submodule", "unstaged", "submodule"),
        ];
        for (line, group, path) in cases {
            let e = parse_porcelain_line(line).unwrap_or_else(|| panic!("parse failed: {line}"));
            assert_eq!(e.group, group, "group for {line}");
            assert_eq!(e.path, path, "path for {line}");
        }
    }

    #[test]
    fn porcelain_quoted_unicode_path() {
        // "你好.md" as octal-escaped C-string style quote
        let line = r#"?? "\344\275\240\345\245\275.md""#;
        let e = parse_porcelain_line(line).expect("parse quoted");
        assert_eq!(e.group, "untracked");
        assert_eq!(e.path, "你好.md");
    }
}

#[cfg(test)]
mod v2_tests {
    use super::{
        is_not_a_git_repo, is_unknown_option, parse_config_pairs, parse_status_v2_z,
        status_group,
    };

    const NUL: char = '\0';

    fn join(parts: &[&str]) -> String {
        parts.iter().map(|p| format!("{p}{NUL}")).collect()
    }

    #[test]
    fn reads_branch_upstream_and_ahead_behind() {
        let raw = join(&[
            "# branch.oid 6ae9b0c",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 3aa6 3aa6 src/app.ts",
            "? notes.md",
        ]);
        let s = parse_status_v2_z(&raw);
        assert_eq!(s.branch, "main");
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind, s.ab_seen), (2, 1, true));
        assert_eq!(s.entries.len(), 2, "one modified + one untracked");
        assert_eq!(s.entries[0].path, "src/app.ts");
        assert_eq!(s.entries[0].group, "unstaged");
        assert_eq!(s.entries[1].path, "notes.md");
        assert_eq!(s.entries[1].group, "untracked");
    }

    #[test]
    fn entry_groups_match_v1_rules() {
        assert_eq!(status_group('.', 'M'), "unstaged");
        assert_eq!(status_group('M', ' '), "staged");
        assert_eq!(status_group('?', '?'), "untracked");
    }

    #[test]
    fn rename_consumes_its_orig_path_token() {
        // `-z` puts the old path in its own record; if it were not consumed the
        // next iteration would read "old/name.ts" as a fresh entry.
        let raw = join(&[
            "# branch.oid abc",
            "# branch.head main",
            "2 R. N... 100644 100644 100644 aaa bbb R100 new/dir/file.ts",
            "old/dir/file.ts",
            "? tail.txt",
        ]);
        let s = parse_status_v2_z(&raw);
        assert_eq!(s.entries.len(), 2, "rename must not yield a phantom entry");
        assert_eq!(s.entries[0].path, "new/dir/file.ts");
        assert_eq!(s.entries[0].index_status, "R");
        assert_eq!(s.entries[1].path, "tail.txt");
    }

    #[test]
    fn unmerged_consumes_both_extra_path_tokens() {
        let raw = join(&[
            "# branch.oid abc",
            "# branch.head main",
            "u UU N... 100644 100644 100644 100644 100644 100644 src/merging.ts",
            ":1:src/merging.ts",
            ":2:src/merging.ts",
            "1 .M N... 100644 100644 100644 a b after.ts",
        ]);
        let s = parse_status_v2_z(&raw);
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries[0].path, "src/merging.ts");
        assert_eq!(s.entries[0].group, "staged");
        assert_eq!(s.entries[1].path, "after.ts");
    }

    #[test]
    fn keeps_spaces_and_non_ascii_in_paths() {
        let raw = join(&[
            "# branch.oid abc",
            "# branch.head main",
            "1 .M N... 100644 100644 100644 a b docs/my report.md",
            "? 中文文件.md",
        ]);
        let s = parse_status_v2_z(&raw);
        assert_eq!(s.entries[0].path, "docs/my report.md");
        assert_eq!(s.entries[1].path, "中文文件.md");
    }

    #[test]
    fn detached_and_unborn_branches() {
        let detached = parse_status_v2_z(&join(&[
            "# branch.oid 5f3e",
            "# branch.head (detached)",
            "? x.txt",
        ]));
        assert_eq!(detached.branch, "(detached)");
        assert!(!detached.ab_seen);
        assert!(!detached.unborn);

        let zeros = "0".repeat(40);
        let unborn = parse_status_v2_z(&join(&[
            &format!("# branch.oid {zeros}"),
            "# branch.head main",
            "? README.md",
        ]));
        assert!(unborn.unborn, "all-zero branch.oid means no commits yet");
    }

    #[test]
    fn ignored_submodule_records_are_skipped() {
        let s = parse_status_v2_z(&join(&[
            "# branch.oid abc",
            "# branch.head main",
            "! vendor/sub",
            "? real.txt",
        ]));
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].path, "real.txt");
    }

    #[test]
    fn config_takes_last_value_and_keeps_spaces() {
        let map = parse_config_pairs("user.name Old\nuser.name Zhang San\nuser.email a@b.com\n");
        assert_eq!(map.get("user.name").map(String::as_str), Some("Zhang San"));
        assert_eq!(map.get("user.email").map(String::as_str), Some("a@b.com"));
        assert!(parse_config_pairs("").is_empty());
    }

    #[test]
    fn only_a_real_non_repo_answers_not_a_repo() {
        assert!(is_not_a_git_repo(
            "fatal: not a git repository (or any of the parent directories): .git"
        ));
        // A permission or lock failure must NOT become is_git=false: the UI
        // would then offer "remove" for a perfectly good repository.
        assert!(!is_not_a_git_repo("error: could not open directory: Permission denied"));
        assert!(!is_not_a_git_repo(
            "fatal: Unable to create 'X/.git/index.lock': File exists."
        ));
        assert!(is_unknown_option("error: unknown option `porcelain=v2'"));
    }

    /// Real-git parity check: v2 records must match v1 lines path-for-path, or
    /// every card would show a different dirty badge than before this change.
    /// Excluded from the default run because it shells out to git; execute with
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore = "creates a throwaway git repository and shells out to git"]
    fn v1_and_v2_agree_on_changed_paths() {
        use super::{git_output_timed, parse_porcelain_line};
        use std::time::Duration;

        let root = std::env::temp_dir().join(format!("aiwb-v2-parity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");
        let dir = root.to_string_lossy().to_string();
        let run = |args: &[&str]| {
            git_output_timed(&dir, args, Duration::from_secs(20))
                .map(|o| o.status.success())
                .unwrap_or(false)
        };

        let outcome = (|| {
            if !run(&["init", "-q", "-b", "main"]) {
                return None;
            }
            run(&["config", "user.email", "parity@example.com"]);
            run(&["config", "user.name", "Parity Test"]);

            std::fs::write(root.join("a.txt"), "one\n").ok()?;
            std::fs::write(root.join("b.txt"), "two\n").ok()?;
            std::fs::create_dir_all(root.join("sub")).ok()?;
            std::fs::write(root.join("sub/e.txt"), "nested\n").ok()?;
            if !run(&["add", "-A"]) || !run(&["commit", "-q", "-m", "seed"]) {
                return None;
            }

            // One unstaged edit, one untracked file, an untracked nested dir,
            // and a staged rename — the shapes v1 and v2 encode differently.
            std::fs::write(root.join("a.txt"), "changed\n").ok()?;
            std::fs::write(root.join("c.txt"), "new\n").ok()?;
            std::fs::create_dir_all(root.join("fresh/deep")).ok()?;
            std::fs::write(root.join("fresh/deep/x.txt"), "deep\n").ok()?;
            if !run(&["mv", "b.txt", "renamed.txt"]) {
                return None;
            }

            let v1 = git_output_timed(&dir, &["status", "--porcelain=v1", "-uall"], Duration::from_secs(20)).ok()?;
            let v2 = git_output_timed(
                &dir,
                &["status", "--porcelain=v2", "-z", "--branch", "-uall"],
                Duration::from_secs(20),
            )
            .ok()?;

            let mut v1_paths: Vec<String> = String::from_utf8_lossy(&v1.stdout)
                .lines()
                .filter_map(parse_porcelain_line)
                .map(|e| e.path)
                .collect();
            let mut v2_paths: Vec<String> = parse_status_v2_z(&String::from_utf8_lossy(&v2.stdout))
                .entries
                .into_iter()
                .map(|e| e.path)
                .collect();
            v1_paths.sort();
            v2_paths.sort();
            Some((v1_paths, v2_paths))
        })();

        let _ = std::fs::remove_dir_all(&root);
        let (v1_paths, v2_paths) = outcome.expect("git setup failed in this environment");

        assert_eq!(
            v2_paths, v1_paths,
            "porcelain v2 must report exactly the paths v1 reported"
        );
        assert!(
            !v1_paths.is_empty(),
            "fixture produced no changes; the comparison would be vacuous"
        );
    }
}
