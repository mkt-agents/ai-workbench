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

/// Branch name rules per `git check-ref-format`. The frontend mirrors this for
/// instant feedback; this is the gatekeeper that runs before any git call.
fn validate_branch_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("分支名不能为空".to_string());
    }
    if name.starts_with('-') {
        return Err("分支名不能以 - 开头".to_string());
    }
    if name.starts_with('/') || name.ends_with('/') {
        return Err("分支名不能以 / 开头或结尾".to_string());
    }
    if name.ends_with(".lock") {
        return Err("分支名不能以 .lock 结尾".to_string());
    }
    if name.contains("..") {
        return Err("分支名不能包含 ..".to_string());
    }
    if name.contains("@{") {
        return Err("分支名不能包含 @{".to_string());
    }
    if name == "@" {
        return Err("分支名不能只是 @".to_string());
    }
    for ch in name.chars() {
        if matches!(ch, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\') {
            return Err(format!("分支名不能包含字符 “{}”", ch));
        }
    }
    // Each slash-separated component must be non-empty and start/end with a
    // non-dot (the trailing-slash cases were rejected above; this catches `//`,
    // `.feature/x` and `feature/x.`).
    for part in name.split('/') {
        if part.is_empty() {
            return Err("分支名不能包含连续的 /".to_string());
        }
        if part.starts_with('.') || part.ends_with('.') {
            return Err(format!("分支名的分段 “{}” 不能以 . 开头或结尾", part));
        }
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

pub(crate) fn ensure_git_repo(repo_path: &str) -> Result<(), String> {
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

pub(crate) fn git_output(repo_path: &str, args: &[&str]) -> Result<std::process::Output, String> {
    git_command()
        .args(args)
        .current_dir(repo_path)
        .output()
        .map_err(map_git_spawn_err)
}

pub(crate) fn git_stdout(repo_path: &str, args: &[&str]) -> Result<String, String> {
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

/// Safety net for a push or pull that never exits (dead socket, unanswered
/// dialog). Slow transfers are left alone; IntelliJ does not impose a
/// low-speed cutoff either.
const GIT_NETWORK_TIMEOUT: Duration = Duration::from_secs(600);

fn stop_git_child(child: &mut std::process::Child, tree: bool) {
    if tree {
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/PID", &child.id().to_string()])
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

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
    let child = git_command()
        .args(args)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(map_git_spawn_err)?;
    finish_git_child(child, args.first().copied().unwrap_or(""), timeout, false)
}

/// `host:port`, `http://host:port`, or `http=...;https=...;socks=...`.
fn parse_proxy_server(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if !raw.contains('=') {
        let url = proxy_url(raw, "http");
        return Some((url.clone(), url));
    }
    let mut http = None;
    let mut https = None;
    let mut socks = None;
    for part in raw.split([';', ' ']) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "http" => http = Some(proxy_url(value, "http")),
            "https" => https = Some(proxy_url(value, "http")),
            "socks" | "socks5" => socks = Some(proxy_url(value, "socks5h")),
            _ => {}
        }
    }
    let fallback = socks.clone().or(http.clone()).or(https.clone())?;
    Some((http.or(socks.clone()).unwrap_or_else(|| fallback.clone()), https.unwrap_or(fallback)))
}

fn proxy_url(value: &str, scheme: &str) -> String {
    let value = value.trim();
    if value.contains("://") {
        value.to_string()
    } else {
        format!("{scheme}://{value}")
    }
}

fn proxy_env_present() -> bool {
    ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "all_proxy"]
        .iter()
        .any(|key| std::env::var(key).map(|v| !v.trim().is_empty()).unwrap_or(false))
}

fn git_config_has_proxy(repo_path: &str) -> bool {
    ["http.proxy", "https.proxy"].iter().any(|key| {
        git_stdout(repo_path, &["config", "--get", key])
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    })
}

/// `(http_proxy, https_proxy)` from the Windows system proxy, when git itself
/// has neither a proxy env var nor `http.proxy` / `https.proxy`.
/// The commit-page push and the ai-hub agent share this so both reach GitHub.
pub(crate) fn git_proxy_env(repo_path: &str) -> Option<(String, String)> {
    system_proxy_for_git(repo_path)
}

/// IntelliJ applies the IDE/system proxy to git. A GUI process does not, so a
/// push fails with "unable to access" while IDEA succeeds.
fn system_proxy_for_git(repo_path: &str) -> Option<(String, String)> {
    if proxy_env_present() || git_config_has_proxy(repo_path) {
        return None;
    }
    windows_system_proxy()
}

#[cfg(target_os = "windows")]
fn windows_system_proxy() -> Option<(String, String)> {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    use windows::core::PCWSTR;

    let path: Vec<u16> = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let enable_name: Vec<u16> = "ProxyEnable".encode_utf16().chain(std::iter::once(0)).collect();
    let mut enabled: u32 = 0;
    let mut bytes: u32 = 4;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            PCWSTR(enable_name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut enabled as *mut u32).cast()),
            Some(&mut bytes),
        )
    };
    if status.is_err() || enabled == 0 {
        return None;
    }
    let server_name: Vec<u16> = "ProxyServer".encode_utf16().chain(std::iter::once(0)).collect();
    let mut size: u32 = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            PCWSTR(server_name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if status.is_err() || size < 2 {
        return None;
    }
    let mut buf = vec![0u16; size as usize / 2 + 1];
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            PCWSTR(server_name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    let text = String::from_utf16_lossy(&buf[..len]);
    parse_proxy_server(text.trim_end_matches('\u{0}').trim())
}

#[cfg(not(target_os = "windows"))]
fn windows_system_proxy() -> Option<(String, String)> {
    None
}

/// Push/pull. A Windows credential dialog is allowed, the same way IntelliJ
/// lets Git Credential Manager ask. A transfer that never finishes is stopped
/// at the hard deadline so the button cannot spin forever.
fn git_network_output(repo_path: &str, args: &[&str]) -> Result<std::process::Output, String> {
    let mut cmd = git_command();
    if let Some((http_proxy, https_proxy)) = system_proxy_for_git(repo_path) {
        cmd.env("http_proxy", &http_proxy);
        cmd.env("https_proxy", &https_proxy);
    }
    // The friendly_*_error translations match git's English wording; a localized
    // git would slip past them (rejected push, diverged pull, merge failures).
    cmd.env("LC_ALL", "C").env("LANGUAGE", "C");
    let child = cmd
        .args(args)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(map_git_spawn_err)?;
    finish_git_child(
        child,
        args.first().copied().unwrap_or(""),
        GIT_NETWORK_TIMEOUT,
        true,
    )
}

fn finish_git_child(
    mut child: std::process::Child,
    verb: &str,
    timeout: Duration,
    tree_kill: bool,
) -> Result<std::process::Output, String> {
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

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    stop_git_child(&mut child, tree_kill);
                    return Err(format!(
                        "git {verb} 超时（>{}s），已跳过",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                stop_git_child(&mut child, tree_kill);
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
    /// merge | rebase | cherry-pick | revert while git is mid-operation.
    pub operation: Option<String>,
    /// Unmerged paths (conflicts) right now.
    pub conflict_count: u32,
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

/// The XY pairs git uses for unmerged paths (both sides touched the file).
pub(crate) fn is_unmerged_xy(index: char, work: char) -> bool {
    matches!(
        (index, work),
        ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
    )
}

fn status_group(index: char, work: char) -> &'static str {
    let index = normalize_xy(index);
    let work = normalize_xy(work);
    if is_unmerged_xy(index, work) {
        // Its own group: staging one would mark it resolved with markers still inside.
        "conflicted"
    } else if index == '?' && work == '?' {
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
    if e.group == "conflicted" {
        return vec![e];
    }
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
            'u' => (11, 0),    // u XY sub m1 m2 m3 mW h1 h2 h3 <path>
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
                operation: None,
                conflict_count: 0,
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
                operation: None,
                conflict_count: 0,
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
    let operation = crate::git_merge::resolve_git_dir(Path::new(&repo_path))
        .as_deref()
        .and_then(crate::git_merge::repo_operation)
        .map(str::to_string);
    let conflict_count = v2.entries.iter().filter(|e| e.group == "conflicted").count() as u32;

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
        operation,
        conflict_count,
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
        operation: None,
        conflict_count: 0,
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
        let per = paths.len().div_ceil(workers);
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

    Some(GitStatusEntry {
        path,
        index_status,
        work_tree_status,
        group: status_group(index_ch, work_ch).to_string(),
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
    if trimmed.chars().count() > 800 {
        let short: String = trimmed.chars().take(700).collect();
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
        entries.extend(expand_porcelain_entry(e));
    }
    Ok(entries)
}

fn git_stage_sync(repo_path: String, files: Vec<String>) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    // `git add` on an unmerged path marks it resolved even with markers inside;
    // conflicts are only staged through git_resolve_conflict.
    let unmerged = crate::git_merge::unmerged_paths(&repo_path);
    let hit: Vec<String> = if files.is_empty() {
        unmerged
    } else {
        unmerged.into_iter().filter(|p| files.iter().any(|f| f == p)).collect()
    };
    if !hit.is_empty() {
        return Err(crate::git_merge::code_err(
            crate::config::ErrorCode::GitConflict,
            format!(
                "{} 个冲突文件不能直接暂存：{}。请在冲突面板里解决后标记已解决。",
                hit.len(),
                hit.iter().take(5).cloned().collect::<Vec<_>>().join("、")
            ),
        ));
    }
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

fn git_commit_sync(repo_path: String, message: String, amend: bool) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    let msg = message.trim();
    if msg.is_empty() {
        return Err("提交说明不能为空".into());
    }
    let state = crate::git_merge::conflict_state(&repo_path);
    // Concluding a merge is the one commit allowed mid-operation, and only once
    // every conflict is resolved; amend / rebase / cherry-pick stay blocked.
    let concluding_merge = state.operation == Some("merge") && state.conflicted.is_empty() && !amend;
    if !concluding_merge {
        if let Some(reason) = crate::git_merge::blocked_reason(&state, "提交") {
            let code = if state.operation.is_some() {
                crate::config::ErrorCode::GitOperationInProgress
            } else {
                crate::config::ErrorCode::GitConflict
            };
            return Err(crate::git_merge::code_err(code, reason));
        }
    }
    if amend {
        if git_stdout(&repo_path, &["rev-parse", "--verify", "HEAD"]).is_err() {
            return Err("仓库还没有任何提交，无法修改上一次提交。".into());
        }
    } else if !concluding_merge && !has_staged_changes(&repo_path)? {
        return Err("没有已暂存的改动。请先暂存文件，或使用「暂存全部并提交」。".into());
    }
    let output = if amend {
        git_output(&repo_path, &["commit", "--amend", "-m", msg])?
    } else {
        git_output(&repo_path, &["commit", "-m", msg])?
    };
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

/// A tip that already matches its upstream has been published. Amending it
/// rewrites history that other clones may already have.
pub(crate) fn counts_as_published(has_upstream: bool, ahead: u32) -> bool {
    has_upstream && ahead == 0
}

fn tip_is_published(repo_path: &str) -> bool {
    if !has_upstream(repo_path) {
        return false;
    }
    let Ok(s) = git_stdout(
        repo_path,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    ) else {
        return false;
    };
    let mut parts = s.split_whitespace();
    let _behind: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let ahead: u32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    counts_as_published(true, ahead)
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
        || lower.contains("user interactivity has been disabled")
    {
        return "推送失败：远程认证失败，请检查凭据或 SSH 密钥".into();
    }
    if raw.contains("git push 超时") || raw.contains("git pull 超时") {
        return "推送超时：超过 10 分钟还没完成，已停止。本地提交不受影响。".into();
    }
    // Network is the most common push failure and the one the user can actually
    // fix without re-entering credentials — name the symptom and the escape hatches.
    if lower.contains("unable to access")
        || lower.contains("failed to connect")
        || lower.contains("connection timed out")
        || lower.contains("could not resolve host")
        || lower.contains("network unreachable")
        || lower.contains("ssl_connect")
    {
        let detail = raw
            .lines()
            .map(str::trim)
            .find(|line| {
                let line = line.to_ascii_lowercase();
                line.contains("fatal:") || line.contains("unable to access") || line.contains("timed out")
            })
            .unwrap_or("")
            .chars()
            .take(160)
            .collect::<String>();
        return if detail.is_empty() {
            "推送失败：无法连接远程仓库（网络不通、超时或被墙）。请检查网络/代理；若仓库配置了其它远程，可改推那个远程，本地提交不受影响。".into()
        } else {
            format!(
                "推送失败：无法连接远程仓库。{detail} 请检查网络或系统代理，本地提交不受影响。"
            )
        };
    }
    if lower.contains("non-fast-forward")
        || lower.contains("fetch first")
        || lower.contains("updates were rejected")
        || lower.contains("failed to push some refs")
    {
        return crate::git_merge::code_err(
            crate::config::ErrorCode::GitPushRejected,
            "推送被拒绝：远程有更新的提交，请先拉取再推送",
        );
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
    crate::git_merge::ensure_repo_clear(&repo_path, "推送")?;

    let remotes = git_stdout(&repo_path, &["remote"]).unwrap_or_default();
    if remotes.trim().is_empty() {
        return Err("没有配置远程仓库，请先添加 remote（例如 origin）".into());
    }

    let output = match if has_upstream(&repo_path) {
        git_network_output(&repo_path, &["push"])
    } else if has_remote(&repo_path, "origin") {
        git_network_output(&repo_path, &["push", "-u", "origin", "HEAD"])
    } else {
        let first = remotes.lines().next().unwrap_or("").trim();
        if first.is_empty() {
            return Err("没有配置远程仓库，请先添加 remote".into());
        }
        return Err(format!(
            "当前分支没有上游，且未找到 origin。可用远程: {}。请先设置上游后再推送。",
            remotes.split_whitespace().collect::<Vec<_>>().join(", ")
        ));
    } {
        Ok(output) => output,
        Err(err) => return Err(friendly_push_error(&err)),
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

pub(crate) fn validate_repo_rel_path(path: &str) -> Result<(), String> {
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
    if lower.contains("unable to access")
        || lower.contains("failed to connect")
        || lower.contains("connection timed out")
        || lower.contains("could not resolve host")
        || lower.contains("network unreachable")
        || lower.contains("ssl_connect")
        || raw.contains("超时")
    {
        return "拉取失败：无法连接远程仓库（网络不通、超时或被墙）。请检查网络/代理。".into();
    }
    if lower.contains("not possible to fast-forward")
        || lower.contains("diverging branches")
        || lower.contains("need to specify how to reconcile")
        || lower.contains("cannot fast-forward")
    {
        return crate::git_merge::code_err(
            crate::config::ErrorCode::GitDiverged,
            "拉取失败：本地与远程已分叉，无法快进。可以选择「合并远程」把远程改动合进来，有冲突时会停下来交给你处理。",
        );
    }
    if lower.contains("would be overwritten by merge") || lower.contains("your local changes to the following files") {
        // git lists the files between its "...following files ...:" line and "Please commit".
        let files: Vec<&str> = raw
            .lines()
            .skip_while(|l| !l.to_ascii_lowercase().contains("following files"))
            .skip(1)
            .map(str::trim)
            .take_while(|l| !l.is_empty() && !l.to_ascii_lowercase().starts_with("please"))
            .take(5)
            .collect();
        return format!(
            "拉取失败：本地未提交的改动会被远程覆盖{}。请先提交或储藏这些改动再拉取，没有改动任何文件。",
            if files.is_empty() { String::new() } else { format!("（{}）", files.join("、")) }
        );
    }
    if lower.contains("could not read from remote")
        || lower.contains("permission denied")
        || lower.contains("authentication failed")
        || lower.contains("403")
        || lower.contains("401")
        || lower.contains("user interactivity has been disabled")
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

/// 截断标记。前端 `lib/diffRows.ts` 的 `DIFF_TRUNCATED_MARK` 靠它判定「这份 diff 不完整」，
/// 从而禁用按块暂存 / 还原——被截断的半行会被 `parseUnifiedDiff` 当成真的新增行打进补丁。
const DIFF_TRUNCATED_MARK: &str = "[diff 过长，已截断]";

fn truncate_diff_text(s: String) -> String {
    const MAX_BYTES: usize = 200_000;
    if s.len() <= MAX_BYTES {
        return s;
    }
    let mut end = MAX_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…\n\n{}", &s[..end], DIFF_TRUNCATED_MARK)
}

#[cfg(test)]
mod truncation_tests {
    use super::*;

    /// 前端与这里的字符串必须一字不差，否则 `diffTruncated()` 静默失效、半行又被打回补丁。
    #[test]
    fn truncation_marker_matches_the_frontend_guard() {
        let cut = truncate_diff_text("x".repeat(200_001));
        assert!(cut.ends_with(DIFF_TRUNCATED_MARK), "截断标记必须收尾");
        assert!(cut.len() < 200_100, "只该多出一小段标记，实际 {}", cut.len());
        let short = truncate_diff_text("keep me".to_string());
        assert_eq!(short, "keep me");
        assert!(!short.contains(DIFF_TRUNCATED_MARK));
    }
}

fn git_pull_sync(repo_path: String, merge: bool) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    crate::git_merge::ensure_repo_clear(&repo_path, "拉取")?;
    if !has_upstream(&repo_path) {
        return Err("当前分支没有上游，无法拉取".into());
    }
    // No --autostash: re-applying the stash can conflict while git still exits 0,
    // leaving markers behind. Git itself refuses a merge that would overwrite
    // uncommitted changes, which is the message the user should see instead.
    let args: &[&str] = if merge {
        &["pull", "--no-rebase", "--no-edit"]
    } else {
        &["pull", "--ff-only"]
    };
    let output = match git_network_output(&repo_path, args) {
        Ok(output) => output,
        Err(err) => return Err(friendly_pull_error(&err)),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{} {}", stderr.trim(), stdout.trim())
            .trim()
            .to_string();
        // Decided from the index, not git's wording: conflicts stay for the user.
        let conflicted = crate::git_merge::unmerged_paths(&repo_path);
        if !conflicted.is_empty() {
            return Err(crate::git_merge::conflict_error(&conflicted, "合并远程"));
        }
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchList {
    pub current: String,
    pub local: Vec<String>,
    pub remote: Vec<String>,
}

/// Where a new branch starts. `None` means the repo's own HEAD; a name is tried
/// as `origin/<name>` first and only then as a local `<name>` — "branch from
/// origin/master" must never silently fork off a stale local master.
/// Returns the ref git should use plus its short sha for the result row.
pub(crate) fn resolve_branch_base(
    repo_path: &str,
    base: Option<&str>,
) -> Result<(String, String), String> {
    let short = |sha: &str| -> String { sha.trim().chars().take(7).collect() };
    let raw = base.map(str::trim).unwrap_or_default();
    if raw.is_empty() {
        // An unborn HEAD has nothing to branch from — "first commit" is the fix,
        // and saying so beats git's generic fatal output.
        let sha = git_stdout(repo_path, &["rev-parse", "--verify", "HEAD"])
            .map_err(|_| "当前分支还没有任何提交，无法作为基点".to_string())?;
        return Ok(("HEAD".to_string(), short(&sha)));
    }
    validate_branch_name(raw)?;
    for cand in [format!("refs/remotes/origin/{raw}"), format!("refs/heads/{raw}")] {
        if let Ok(sha) = git_stdout(repo_path, &["rev-parse", "--verify", &cand]) {
            return Ok((cand, short(&sha)));
        }
    }
    Err(format!(
        "找不到基点 {raw}：本地没有这个分支，也没有 origin/{raw} 的跟踪引用。勾上「先从远端取基点」再试，或确认分支名。"
    ))
}

/// `refs/remotes/origin/x` reads as `origin/x`, `refs/heads/x` as `x`.
fn base_ref_label(r: &str) -> String {
    ["refs/remotes/", "refs/heads/"]
        .iter()
        .find_map(|p| r.strip_prefix(p).map(str::to_string))
        .unwrap_or_else(|| r.to_string())
}

fn clip_git_tail(raw: &str, fallback: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else if trimmed.chars().count() > 200 {
        format!("{}…", trimmed.chars().take(160).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

fn friendly_fetch_error(raw: &str, name: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("couldn't find remote ref") || lower.contains("no matching ref") {
        return format!("远端没有分支 {name}");
    }
    if lower.contains("authentication failed")
        || lower.contains("permission denied")
        || lower.contains("403")
    {
        return format!("没有权限读取远端分支 {name}（检查凭据或访问令牌）");
    }
    if lower.contains("could not resolve host") || lower.contains("timed out") {
        return "连不上远端：检查网络或代理后重试".into();
    }
    clip_git_tail(raw, "拉取远端分支失败")
}

fn friendly_publish_error(raw: &str, name: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("non-fast-forward") || (lower.contains("! [rejected]") && lower.contains("fetch first")) {
        return format!("origin 上已有一条不同的 {name}，不会覆盖：先确认远端那条分支怎么处理");
    }
    if lower.contains("permission denied")
        || lower.contains("read-only")
        || lower.contains("authentication failed")
        || lower.contains("403")
    {
        return "远端拒绝写入：当前凭据没有建分支的权限".into();
    }
    if lower.contains("could not resolve host")
        || lower.contains("timed out")
        || lower.contains("the remote end hung up")
    {
        return "连不上远端或传输中断：检查网络后重试".into();
    }
    clip_git_tail(raw, &format!("在 origin 创建 {name} 失败"))
}

/// Create a local branch at `base` (HEAD when absent) and switch to it. Pushing
/// to the remote is a separate step: the frontend reuses `git_push`, whose
/// no-upstream path is exactly `push -u origin HEAD`.
fn git_create_branch_sync(
    repo_path: String,
    name: String,
    base: Option<String>,
) -> Result<String, String> {
    let name = name.trim().to_string();
    validate_branch_name(&name)?;
    ensure_git_repo(&repo_path)?;
    let (base_ref, sha) = resolve_branch_base(&repo_path, base.as_deref())?;

    if git_stdout(&repo_path, &["rev-parse", "--verify", &format!("refs/heads/{}", name)]).is_ok() {
        return Err(format!("本地已存在分支 {}，如需切换请使用「批量切换分支」", name));
    }

    let output = git_output(&repo_path, &["checkout", "-b", &name, &base_ref])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    Ok(format!(
        "已创建并切换到分支 {}（基于 {}@{}）",
        name,
        base_ref_label(&base_ref),
        sha
    ))
}

/// `git fetch origin +refs/heads/<name>:refs/remotes/origin/<name>`: refresh one
/// tracking ref, never the working tree. `+` because a tracking ref may need to
/// move backwards after someone else force-pushed.
fn git_fetch_branch_sync(repo_path: String, name: String) -> Result<String, String> {
    let name = name.trim().to_string();
    validate_branch_name(&name)?;
    ensure_git_repo(&repo_path)?;
    if !has_remote(&repo_path, "origin") {
        return Err("这个仓库没有配置 origin，取不到远端分支".into());
    }
    let spec = format!("+refs/heads/{name}:refs/remotes/origin/{name}");
    let output = match git_network_output(&repo_path, &["fetch", "origin", &spec]) {
        Ok(output) => output,
        Err(err) => return Err(friendly_fetch_error(&err, &name)),
    };
    if !output.status.success() {
        let combined = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        return Err(friendly_fetch_error(&combined, &name));
    }
    let sha = git_stdout(&repo_path, &["rev-parse", "--verify", &format!("refs/remotes/origin/{name}")])?;
    Ok(format!(
        "已取到 origin/{name}（{}）",
        sha.trim().chars().take(7).collect::<String>()
    ))
}

/// Create the branch **on origin** from a base ref without touching the working
/// tree or HEAD. Deliberately not behind `ensure_repo_clear`: nothing in the
/// worktree moves, so a repo that is mid-merge can still publish a branch.
/// Landing it locally is the existing switch flow (git DWIM builds the tracking
/// branch for a remote-only name).
fn git_publish_branch_sync(
    repo_path: String,
    name: String,
    base: Option<String>,
) -> Result<String, String> {
    let name = name.trim().to_string();
    validate_branch_name(&name)?;
    ensure_git_repo(&repo_path)?;
    if !has_remote(&repo_path, "origin") {
        return Err("没有配置 origin，只能先在本地建分支".into());
    }
    let (base_ref, sha) = resolve_branch_base(&repo_path, base.as_deref())?;
    let spec = format!("{base_ref}:refs/heads/{name}");
    let output = match git_network_output(&repo_path, &["push", "origin", &spec]) {
        Ok(output) => output,
        Err(err) => return Err(friendly_publish_error(&err, &name)),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(friendly_publish_error(
            &format!("{stderr} {stdout}"),
            &name,
        ));
    }
    if format!("{stderr} {stdout}").to_ascii_lowercase().contains("everything up-to-date") {
        return Ok(format!("origin 上已有同一个 {name}，未重复创建"));
    }
    Ok(format!(
        "已在 origin 创建 {}（基于 {}@{}）",
        name,
        base_ref_label(&base_ref),
        sha
    ))
}

/// `git checkout <name>`. Git's DWIM covers the remote-only case: when only
/// `origin/<name>` exists locally, checkout creates a local tracking branch.
fn git_switch_branch_sync(repo_path: String, name: String) -> Result<String, String> {
    let name = name.trim().to_string();
    validate_branch_name(&name)?;
    ensure_git_repo(&repo_path)?;
    // 合并中 / 有未解决冲突时 git 自己也会拒绝 checkout，但吐的是原始报错；
    // 走与其它写操作同一条门，用户看到的是「去提交页解决冲突」。
    crate::git_merge::ensure_repo_clear(&repo_path, "切换分支")?;

    // 英文输出才能稳定识别失败原因（本机 git 可能是中文语言包），再由 describe_checkout_error 翻译
    let output = git_command()
        .args(["checkout", &name])
        .env("LC_ALL", "C")
        .env("LANGUAGE", "C")
        .current_dir(&repo_path)
        .output()
        .map_err(map_git_spawn_err)?;
    if !output.status.success() {
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        );
        return Err(describe_checkout_error(&name, combined.trim()));
    }
    Ok(format!("已切换到分支 {}", name))
}

/// 缩进的文件列表行（git 用 tab 缩进列出受影响的文件），最多列 6 个。
fn indented_paths(raw: &str, after: &str) -> String {
    let Some(start) = raw.find(after) else {
        return String::new();
    };
    let files: Vec<&str> = raw[start + after.len()..]
        .lines()
        .skip_while(|l| l.trim().is_empty() || !l.starts_with([' ', '\t']))
        .take_while(|l| l.starts_with([' ', '\t']))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if files.is_empty() {
        return String::new();
    }
    let mut out: String = files.iter().take(6).map(|f| format!("\n  {f}")).collect();
    if files.len() > 6 {
        out.push_str(&format!("\n  …等 {} 个文件", files.len()));
    }
    out
}

/// git checkout 失败的英文原文 → 中文原因 + 下一步。认不出的原样附在「切换分支失败」后面。
fn describe_checkout_error(name: &str, raw: &str) -> String {
    const LOCAL: &str = "Your local changes to the following files would be overwritten by checkout:";
    const UNTRACKED: &str =
        "The following untracked working tree files would be overwritten by checkout:";
    if raw.contains(LOCAL) {
        return format!(
            "无法切换到 {name}：以下文件有未提交的改动，切过去会被目标分支的版本覆盖{}\n请先提交或「储藏改动」后再切换。",
            indented_paths(raw, LOCAL)
        );
    }
    if raw.contains(UNTRACKED) {
        return format!(
            "无法切换到 {name}：目标分支里有同名文件，会覆盖以下未跟踪文件{}\n请先移走、提交或删除它们后再切换。",
            indented_paths(raw, UNTRACKED)
        );
    }
    if raw.contains("did not match") {
        return format!("分支 {name} 不存在（本地与远端跟踪引用中均未找到，可先批量拉取）");
    }
    if raw.contains("is already checked out at") || raw.contains("is already used by worktree") {
        return format!("无法切换到 {name}：该分支已在另一个工作树中检出，不能同时检出两次。");
    }
    if raw.contains("resolve your current index first") || raw.contains("you need to resolve") {
        return format!("无法切换到 {name}：当前有未解决的合并冲突，请先解决冲突并提交。");
    }
    if raw.contains("index.lock") {
        return format!(
            "无法切换到 {name}：仓库被另一个 git 进程占用（index.lock）。等它结束，或确认没有 git 在运行后删除 .git/index.lock。"
        );
    }
    format!("切换到 {name} 失败：{raw}")
}

/// Local and remote-tracking refs only — no network. Feeds the switch modal's
/// branch picker. `remote` carries plain branch names with the remote segment
/// stripped (`refs/remotes/origin/feature/x` → `feature/x`, any remote), so the
/// frontend can merge "local master" and "origin master" into one switchable
/// entry; a bare remote ref like `refs/remotes/origin` lstrips to empty and a
/// symbolic `…/HEAD` are both dropped.
fn git_list_branches_sync(repo_path: String) -> Result<GitBranchList, String> {
    ensure_git_repo(&repo_path)?;
    let current =
        git_stdout(&repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let mut local: Vec<String> = git_stdout(
        &repo_path,
        &["for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads"],
    )
    .unwrap_or_default()
    .lines()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .collect();
    let mut remote: Vec<String> = git_stdout(
        &repo_path,
        &["for-each-ref", "--format=%(refname:lstrip=3)", "refs/remotes"],
    )
    .unwrap_or_default()
    .lines()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty() && s != "HEAD")
    .collect();
    local.sort_by_key(|a| a.to_lowercase());
    remote.sort_by_key(|a| a.to_lowercase());
    // The same branch on several remotes (origin/x + upstream/x) collapses to one name.
    local.dedup();
    remote.dedup();
    Ok(GitBranchList { current, local, remote })
}

/// 一次预览最多列几条提交（`total` 仍是真实数，前端据此说「只列前 N」）
const PENDING_COMMITS_CAP: u32 = 50;

/// 「会被推出去的提交」按哪个范围算。三档，范围字符串全部在这里拼死，
/// 调用方只能传仓库路径 —— 没有任何用户输入能变成 git 参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingScope {
    /// `upstream` / `remoteTracking` / `unpushed`，前端按它选措辞，Rust 侧不写 UI 文案
    pub kind: &'static str,
    /// 拿来对比的引用名（`origin/main`）；`unpushed` 档为空
    pub basis: String,
    /// 拼进 `git log` / `git rev-list` 的范围参数
    pub log_args: Vec<String>,
}

pub(crate) fn pending_commit_scope(
    upstream: Option<&str>,
    branch: &str,
    remote_refs: &[String],
) -> PendingScope {
    if let Some(up) = upstream.map(str::trim).filter(|s| !s.is_empty()) {
        return PendingScope {
            kind: "upstream",
            basis: up.to_string(),
            log_args: vec!["@{upstream}..HEAD".to_string()],
        };
    }
    // 没配 upstream 不等于没有参照：本地若已有同名远端跟踪引用，它才是「推上去会和什么比」的东西。
    // origin 优先，同远端多个引用取第一个（remote_refs 来自 for-each-ref，已按名字排序）。
    let suffix = format!("/{}", branch);
    if branch != "HEAD" {
        let hit = remote_refs
            .iter()
            .find(|r| r.starts_with("refs/remotes/origin/") && r.ends_with(&suffix))
            .or_else(|| remote_refs.iter().find(|r| r.ends_with(&suffix)));
        if let Some(r) = hit {
            let basis = r.strip_prefix("refs/remotes/").unwrap_or(r).to_string();
            return PendingScope {
                kind: "remoteTracking",
                basis: basis.clone(),
                log_args: vec![format!("{}..HEAD", basis)],
            };
        }
    }
    // 最后兜底：HEAD 上还没进过任何远端引用的提交。新分支 / 全新仓库都走这条。
    PendingScope {
        kind: "unpushed",
        basis: String::new(),
        log_args: vec!["HEAD".to_string(), "--not".to_string(), "--remotes".to_string()],
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitPendingCommit {
    pub hash: String,
    pub subject: String,
    pub when: String,
    pub files: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitPendingCommits {
    pub branch: String,
    /// `PendingScope::kind`，或 `noCommits`（一次提交都还没有）
    pub scope: String,
    pub basis: String,
    pub commits: Vec<GitPendingCommit>,
    /// 范围内全部提交数（含被折叠掉的 merge 之外的部分）
    pub total: u32,
    pub truncated: bool,
}

/// `\x1e` 起头、`\x1f` 分隔 hash/标题/相对时间，随后每行一条改动路径。
fn parse_pending_log(text: &str) -> Vec<GitPendingCommit> {
    let mut out = Vec::new();
    for chunk in text.split('\u{1e}') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let mut lines = chunk.lines();
        let Some(head) = lines.next() else { continue };
        let mut parts = head.splitn(3, '\u{1f}');
        let hash = parts.next().unwrap_or("").trim().to_string();
        let subject = parts.next().unwrap_or("").trim().to_string();
        let when = parts.next().unwrap_or("").trim().to_string();
        // 标题为空也保留这条：预览少报一条就是谎报「这次会推走什么」，条数与 rev-list 也得对得上。
        if hash.is_empty() {
            continue;
        }
        let files = lines
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        out.push(GitPendingCommit {
            hash,
            subject,
            when,
            files,
        });
    }
    out
}

fn git_pending_commits_sync(repo_path: String) -> Result<GitPendingCommits, String> {
    ensure_git_repo(&repo_path)?;
    let branch = git_stdout(&repo_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_default()
        .trim()
        .to_string();
    if git_stdout(&repo_path, &["rev-parse", "--verify", "-q", "HEAD"]).is_err() {
        // 没提交就没得推：给空表，而不是让 git 的 "does not have any commits yet" 冒成失败
        return Ok(GitPendingCommits {
            branch,
            scope: "noCommits".to_string(),
            basis: String::new(),
            commits: vec![],
            total: 0,
            truncated: false,
        });
    }
    let upstream = git_stdout(
        &repo_path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
    let remote_refs: Vec<String> = git_stdout(
        &repo_path,
        &["for-each-ref", "--format=%(refname)", "refs/remotes"],
    )
    .unwrap_or_default()
    .lines()
    .map(|l| l.trim().to_string())
    .filter(|l| !l.is_empty() && !l.ends_with("/HEAD"))
    .collect();
    let scope = pending_commit_scope(upstream.as_deref(), &branch, &remote_refs);
    // 中文路径默认会被转义成 `"...\346..."`，预览里没法读；这条命令只为展示，就地关掉引用。
    let mut log_args: Vec<String> = vec![
        "-c".to_string(),
        "core.quotePath=false".to_string(),
        "log".to_string(),
        format!("--max-count={}", PENDING_COMMITS_CAP + 1),
        "--pretty=format:%x1e%h%x1f%s%x1f%cr".to_string(),
        "--name-only".to_string(),
        "--no-merges".to_string(),
    ];
    log_args.extend(scope.log_args.iter().cloned());
    let commits = parse_pending_log(
        &git_stdout(&repo_path, &log_args.iter().map(String::as_str).collect::<Vec<&str>>())?,
    );
    let mut count_args = vec![
        "rev-list".to_string(),
        "--count".to_string(),
        "--no-merges".to_string(),
    ];
    count_args.extend(scope.log_args.iter().cloned());
    let total = git_stdout(
        &repo_path,
        &count_args.iter().map(String::as_str).collect::<Vec<&str>>(),
    )
    .unwrap_or_default()
    .trim()
    .parse::<u32>()
    .unwrap_or(0);
    Ok(GitPendingCommits {
        branch,
        scope: scope.kind.to_string(),
        basis: scope.basis,
        truncated: total as usize > commits.len(),
        commits,
        total,
    })
}

#[cfg(test)]
mod pending_commits_tests {
    use super::*;

    fn refs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn upstream_is_the_first_choice() {
        let s = pending_commit_scope(Some("origin/main"), "main", &refs(&["refs/remotes/origin/main"]));
        assert_eq!(s.kind, "upstream");
        assert_eq!(s.basis, "origin/main");
        assert_eq!(s.log_args, vec!["@{upstream}..HEAD".to_string()]);
    }

    #[test]
    fn blank_upstream_is_not_an_upstream() {
        let s = pending_commit_scope(Some("   "), "main", &refs(&[]));
        assert_eq!(s.kind, "unpushed");
        assert_eq!(s.log_args, vec!["HEAD", "--not", "--remotes"]);
    }

    #[test]
    fn remote_tracking_ref_steps_in_without_upstream_and_prefers_origin() {
        let s = pending_commit_scope(
            None,
            "dev",
            &refs(&["refs/remotes/fork/dev", "refs/remotes/origin/dev"]),
        );
        assert_eq!(s.kind, "remoteTracking");
        assert_eq!(s.basis, "origin/dev");
        assert_eq!(s.log_args, vec!["origin/dev..HEAD".to_string()]);

        let s2 = pending_commit_scope(None, "dev", &refs(&["refs/remotes/fork/dev"]));
        assert_eq!(s2.basis, "fork/dev");
    }

    #[test]
    fn detached_head_never_matches_a_remote_ref_by_suffix() {
        // `refs/remotes/origin/HEAD` 会被 `!/HEAD` 过滤掉，但 detached HEAD 的分支名正好是 "HEAD"，
        // 后缀匹配会把任何 origin/* 认错，所以这一档必须靠分支名守卫。
        let s = pending_commit_scope(None, "HEAD", &refs(&["refs/remotes/origin/HEAD", "refs/remotes/origin/main"]));
        assert_eq!(s.kind, "unpushed");
        assert!(s.basis.is_empty());
    }

    #[test]
    fn a_branch_only_local_has_nothing_to_match() {
        let s = pending_commit_scope(None, "feature-x", &refs(&["refs/remotes/origin/main"]));
        assert_eq!(s.kind, "unpushed");
    }

    #[test]
    fn log_records_keep_files_and_order() {
        let text = "\u{1e}a1b2c3d\u{1f}feat: 加推送预览\u{1f}3 hours ago\nsrc/a.ts\nsrc/b.ts\n\n\u{1e}9f8e7d6\u{1f}fix: 修一个\u{1f}2 days ago\n";
        let got = parse_pending_log(text);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].hash, "a1b2c3d");
        assert_eq!(got[0].subject, "feat: 加推送预览");
        assert_eq!(got[0].when, "3 hours ago");
        assert_eq!(got[0].files, vec!["src/a.ts".to_string(), "src/b.ts".to_string()]);
        assert!(got[1].files.is_empty());
        assert!(parse_pending_log("").is_empty());
    }

    #[test]
    fn empty_subject_still_counts_as_a_commit() {
        // 少报一条 = 谎报「这次会推走什么」，而且条数要和 rev-list --count 对得上。
        let got = parse_pending_log("\u{1e}dead001\u{1f}\u{1f}5 minutes ago\nx.txt");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].subject, "");
        assert_eq!(got[0].files, vec!["x.txt".to_string()]);
    }

    #[test]
    fn junk_lines_are_dropped() {
        assert!(parse_pending_log("\n\n  \n").is_empty());
        assert!(parse_pending_log("\u{1e}\u{1f}\u{1f}no hash").is_empty());
    }

    /// 真实仓库跑通三档范围：无上游 → unpushed，配了上游 → upstream，中文路径不转义。
    #[test]
    fn pending_commits_against_a_real_repo() {
        if git_command().arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("aw-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let repo = root.join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let sh = |args: &[&str]| {
            let out = git_command().args(args).current_dir(&repo).output().expect("git");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        sh(&["init", "-q", "-b", "main"]);
        sh(&["config", "user.email", "t@t"]);
        sh(&["config", "user.name", "t"]);
        sh(&["config", "core.autocrlf", "false"]);

        // 一次提交都没有：空表，不是报错
        let none = git_pending_commits_sync(repo.to_string_lossy().into_owned()).unwrap();
        assert_eq!(none.scope, "noCommits");
        assert!(none.commits.is_empty());

        std::fs::write(repo.join("说明.md"), "x").unwrap();
        sh(&["add", "-A"]);
        sh(&["commit", "-qm", "docs: 中文路径"]);
        std::fs::write(repo.join("a.txt"), "y").unwrap();
        sh(&["add", "-A"]);
        sh(&["commit", "-qm", "feat: 第二个"]);

        // 还没有任何远端引用：走 HEAD --not --remotes
        let local = git_pending_commits_sync(repo.to_string_lossy().into_owned()).unwrap();
        assert_eq!(local.branch, "main");
        assert_eq!(local.scope, "unpushed");
        assert_eq!(local.total, 2);
        assert_eq!(local.commits[0].subject, "feat: 第二个");
        assert_eq!(local.commits[1].files, vec!["说明.md".to_string()]);

        // 推到本地 bare 远端并设上游后，只剩未推送的那条
        let remote = root.join("g.git");
        sh(&["init", "-q", "--bare", &remote.to_string_lossy()]);
        sh(&["remote", "add", "origin", &remote.to_string_lossy()]);
        sh(&["push", "-q", "-u", "origin", "main"]);
        std::fs::write(repo.join("b.txt"), "z").unwrap();
        sh(&["add", "-A"]);
        sh(&["commit", "-qm", "fix: 第三个"]);

        let up = git_pending_commits_sync(repo.to_string_lossy().into_owned()).unwrap();
        assert_eq!(up.scope, "upstream");
        assert_eq!(up.basis, "origin/main");
        assert_eq!(up.total, 1);
        assert_eq!(up.commits[0].files, vec!["b.txt".to_string()]);
        assert!(!up.truncated);

        let _ = std::fs::remove_dir_all(&root);
    }
}


/// Most context lines a diff view may ask for (`-U`): enough for a whole file.
const DIFF_CONTEXT_MAX: u32 = 100_000;

/// View-only diff flags: `-w` and `-U<n>`. A `-w` diff no longer matches the file, so its hunks
/// cannot be applied back and the UI turns staging off for it. `-U<n>` (完整文件视图) still
/// applies — `git apply --recount` recomputes the hunk counts — so block actions stay available
/// there; only a truncated diff (`truncate_diff_text`) disables them.
fn diff_view_args(ignore_whitespace: bool, context_lines: Option<u32>) -> Vec<String> {
    let mut out = Vec::new();
    if ignore_whitespace {
        out.push("-w".to_string());
    }
    if let Some(n) = context_lines {
        out.push(format!("-U{}", n.min(DIFF_CONTEXT_MAX)));
    }
    out
}

fn git_diff_sync(
    repo_path: String,
    file_path: String,
    staged: bool,
    ignore_whitespace: bool,
    context_lines: Option<u32>,
) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    validate_repo_rel_path(&file_path)?;
    let view = diff_view_args(ignore_whitespace, context_lines);
    let ws: Vec<&str> = view.iter().map(String::as_str).collect();

    if staged {
        let mut args = vec!["diff", "--cached"];
        args.extend_from_slice(&ws);
        args.extend_from_slice(&["--", &file_path]);
        let output = git_output(&repo_path, &args)?;
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
        let mut args = vec!["diff", "--no-index"];
        args.extend_from_slice(&ws);
        args.extend_from_slice(&["--", null_path, &file_path]);
        let output = git_command()
            .args(&args)
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

    let mut args = vec!["diff"];
    args.extend_from_slice(&ws);
    args.extend_from_slice(&["--", &file_path]);
    let output = git_output(&repo_path, &args)?;
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
    // Restoring an unmerged path from HEAD would silently resolve it as "ours".
    if crate::git_merge::unmerged_paths(&repo_path).iter().any(|p| p == &file_path) {
        return Err(crate::git_merge::code_err(
            crate::config::ErrorCode::GitConflict,
            format!("{file_path} 有冲突，不能直接回滚。请在冲突面板里选择保留哪一边。"),
        ));
    }

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
    crate::git_merge::ensure_repo_clear(&repo_path, "撤销上次提交")?;

    // "ahead == 0 with upstream" means the tip is already published.
    let published = tip_is_published(&repo_path);

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

/// Reject patches that would leave the repository (`..` or an absolute path).
pub(crate) fn patch_stays_in_repo(patch: &str) -> bool {
    for line in patch.lines() {
        let Some(rest) = line.strip_prefix("+++ ").or_else(|| line.strip_prefix("--- ")) else {
            continue;
        };
        let token = rest.split_whitespace().next().unwrap_or("");
        if token == "/dev/null" || token.ends_with("/dev/null") || token == "NUL" {
            continue;
        }
        let path = token
            .strip_prefix("a/")
            .or_else(|| token.strip_prefix("b/"))
            .unwrap_or(token);
        if path.is_empty() {
            continue;
        }
        if path.contains('\0')
            || path.starts_with('/')
            || path.starts_with('\\')
            || Path::new(path).is_absolute()
        {
            return false;
        }
        if path.split(['/', '\\']).any(|seg| seg == "..") {
            return false;
        }
    }
    true
}

/// Files a patch touches (`+++ b/<path>`), for the conflict check below.
fn patch_files(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| l.strip_prefix("+++ "))
        .map(|rest| rest.trim())
        .filter(|p| *p != "/dev/null")
        .map(|p| p.strip_prefix("b/").unwrap_or(p).to_string())
        .collect()
}

/// Apply a hunk / block patch to the index (`--cached`) or to the working
/// file (`worktree`: reverting a block in the diff view, or undoing that).
fn git_apply_cached_sync(repo_path: String, patch: String, reverse: bool, worktree: bool) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    if patch.trim().is_empty() {
        return Err("没有可暂存的代码块".into());
    }
    if !patch_stays_in_repo(&patch) {
        return Err("补丁路径超出当前仓库".into());
    }
    // An unmerged file is the resolver's: a patch would quietly pick its content.
    let unmerged = crate::git_merge::unmerged_paths(&repo_path);
    if let Some(hit) = patch_files(&patch).into_iter().find(|f| unmerged.contains(f)) {
        return Err(crate::git_merge::code_err(
            crate::config::ErrorCode::GitConflict,
            format!("{hit} 有冲突，请在冲突面板里解决。"),
        ));
    }
    let mut cmd = git_command();
    cmd.current_dir(&repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut args = vec!["apply", "--whitespace=nowarn", "--recount"];
    if !worktree {
        args.push("--cached");
    }
    if reverse {
        args.push("--reverse");
    }
    args.push("-");
    cmd.args(args);
    let mut child = cmd.spawn().map_err(map_git_spawn_err)?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(patch.as_bytes()).map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            if worktree { "还原失败".into() } else { "按块暂存失败".into() }
        } else if worktree {
            format!("还原失败：{stderr}（文件可能在打开差异后又被改过，请刷新后再试）")
        } else {
            stderr
        });
    }
    Ok(match (worktree, reverse) {
        (true, true) => "已还原所选改动".into(),
        (true, false) => "已恢复刚才还原的改动".into(),
        (false, true) => "已取消暂存所选代码块".into(),
        (false, false) => "已暂存所选代码块".into(),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStashEntry {
    pub index: u32,
    pub message: String,
}

fn git_stash_push_sync(repo_path: String, message: String) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    crate::git_merge::ensure_repo_clear(&repo_path, "储藏")?;
    let message = {
        let trimmed = message.trim();
        if trimmed.is_empty() {
            "WIP".to_string()
        } else {
            trimmed.to_string()
        }
    };
    let output = git_output(&repo_path, &["stash", "push", "-u", "-m", &message])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "收起改动失败".into()
        } else {
            stderr
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.to_ascii_lowercase().contains("no local changes") {
        return Err("没有可收起的改动".into());
    }
    Ok(if stdout.is_empty() {
        "已收起改动".into()
    } else {
        stdout
    })
}

fn git_stash_list_sync(repo_path: String) -> Result<Vec<GitStashEntry>, String> {
    ensure_git_repo(&repo_path)?;
    let output = git_output(
        &repo_path,
        &["stash", "list", "--pretty=format:%gd%x1f%s"],
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "读取暂存列表失败".into()
        } else {
            stderr
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (slot, message) = line.split_once('\u{1f}').unwrap_or((line, ""));
        let index = slot
            .trim_start_matches("stash@{")
            .trim_end_matches('}')
            .parse()
            .unwrap_or(0);
        out.push(GitStashEntry {
            index,
            message: message.to_string(),
        });
    }
    Ok(out)
}

fn git_stash_apply_sync(repo_path: String, index: u32, drop: bool) -> Result<String, String> {
    ensure_git_repo(&repo_path)?;
    if !drop {
        crate::git_merge::ensure_repo_clear(&repo_path, "恢复储藏")?;
    }
    let spec = format!("stash@{{{index}}}");
    let verb = if drop { "drop" } else { "pop" };
    let output = git_output(&repo_path, &["stash", verb, &spec])?;
    if !output.status.success() {
        if !drop {
            // git keeps the stash entry when pop conflicts; the files wait for the resolver.
            let conflicted = crate::git_merge::unmerged_paths(&repo_path);
            if !conflicted.is_empty() {
                return Err(crate::git_merge::conflict_error(&conflicted, "恢复储藏"));
            }
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let combined = format!("{stderr} {stdout}").trim().to_string();
        return Err(if combined.is_empty() {
            if drop {
                "删除暂存失败".into()
            } else {
                "恢复暂存失败".into()
            }
        } else {
            combined
        });
    }
    Ok(if drop {
        "已删除这条暂存".into()
    } else {
        "已恢复这条暂存".into()
    })
}

#[tauri::command]
pub async fn git_commit(path: String, message: String, amend: Option<bool>) -> Result<String, String> {
    let amend = amend.unwrap_or(false);
    tokio::task::spawn_blocking(move || git_commit_sync(path, message, amend))
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
pub async fn git_pull(path: String, merge: Option<bool>) -> Result<String, String> {
    let merge = merge.unwrap_or(false);
    tokio::task::spawn_blocking(move || git_pull_sync(path, merge))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_create_branch(
    path: String,
    name: String,
    base: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_create_branch_sync(path, name, base))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_fetch_branch(path: String, name: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_fetch_branch_sync(path, name))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_publish_branch(
    path: String,
    name: String,
    base: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_publish_branch_sync(path, name, base))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_switch_branch(path: String, name: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_switch_branch_sync(path, name))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_list_branches(path: String) -> Result<GitBranchList, String> {
    tokio::task::spawn_blocking(move || git_list_branches_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn git_current_branch_sync(path: &str) -> Result<String, String> {
    // 用 `symbolic-ref` 而不是 `rev-parse --abbrev-ref HEAD`：后者在还没提交过的仓库里
    // 会直接 fatal（HEAD 解析不了），前者照样给出分支名；detached HEAD 时它失败，
    // 正好当作「没有分支」，调用点按仓库粒度记忆。
    git_stdout(path, &["symbolic-ref", "--short", "-q", "HEAD"])
}

/// 当前分支名。天玑的「以后都允许」要按「仓库 + 分支」记。
/// detached HEAD / 非 git 目录返回 Err，调用点当成「没有分支」。
#[tauri::command]
pub async fn git_current_branch(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_current_branch_sync(&path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

/// 只读：这次推送会带走哪些提交、每条改了哪些文件。不联网。
#[tauri::command]
pub async fn git_pending_commits(path: String) -> Result<GitPendingCommits, String> {
    tokio::task::spawn_blocking(move || git_pending_commits_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_diff(
    path: String,
    file_path: String,
    staged: bool,
    ignore_whitespace: Option<bool>,
    context_lines: Option<u32>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        git_diff_sync(path, file_path, staged, ignore_whitespace.unwrap_or(false), context_lines)
    })
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

/// `git log --since` only accepts a calendar stamp we generate (`YYYY-MM-DD` or with time).
fn git_since_arg(since: &str) -> Result<String, String> {
    let s = since.trim();
    let ok = s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-'
        || (s.len() >= 19 && s.len() <= 19 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' && (s.as_bytes()[10] == b' ' || s.as_bytes()[10] == b'T'));
    if !ok || !s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'-' | b':' | b' ' | b'T')) {
        return Err("invalid since".to_string());
    }
    Ok(format!("--since={s}"))
}

#[cfg(test)]
mod checkout_error_tests {
    use super::describe_checkout_error;

    #[test]
    fn local_changes_are_translated_with_the_file_list() {
        let raw = "error: Your local changes to the following files would be overwritten by checkout:\n\tsrc/components/GitCommitPanel.tsx\n\tsrc/styles/changelog.css\nPlease commit your changes or stash them before you switch branches.\nAborting";
        let msg = describe_checkout_error("dev", raw);
        assert!(msg.starts_with("无法切换到 dev"), "{msg}");
        assert!(msg.contains("\n  src/components/GitCommitPanel.tsx\n  src/styles/changelog.css\n"), "{msg}");
        assert!(!msg.contains("Please commit"), "{msg}");
        assert!(msg.contains("储藏改动"), "{msg}");
    }

    #[test]
    fn untracked_and_missing_and_unknown() {
        let raw = "error: The following untracked working tree files would be overwritten by checkout:\n\ta.txt\nPlease move or remove them before you switch branches.\nAborting";
        let msg = describe_checkout_error("dev", raw);
        assert!(msg.contains("未跟踪文件") && msg.contains("\n  a.txt\n"), "{msg}");
        assert!(describe_checkout_error("x", "error: pathspec 'x' did not match any file(s) known to git")
            .contains("不存在"));
        assert_eq!(describe_checkout_error("x", "fatal: boom"), "切换到 x 失败：fatal: boom");
    }
}

#[cfg(test)]
mod since_tests {
    use super::git_since_arg;

    #[test]
    fn since_accepts_a_local_day_start() {
        assert_eq!(
            git_since_arg("2026-10-17 00:00:00").unwrap(),
            "--since=2026-10-17 00:00:00"
        );
        assert_eq!(git_since_arg("2026-10-17").unwrap(), "--since=2026-10-17");
        assert!(git_since_arg("--all").is_err());
        assert!(git_since_arg("yesterday").is_err());
    }

    #[test]
    fn log_files_parse_subject_and_paths() {
        let raw = "\u{1e}fix ask\u{1f}2 hours ago\nsrc/a.ts\nsrc/b.rs\n\n\u{1e}wip\u{1f}3 hours ago\n";
        let rows = super::parse_log_files(raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].subject, "fix ask");
        assert_eq!(rows[0].files, vec!["src/a.ts".to_string(), "src/b.rs".to_string()]);
        assert_eq!(rows[1].subject, "wip");
        assert!(rows[1].files.is_empty());
    }
}

/// Get recent commits (short format: hash + subject + relative date).
/// `since` limits the walk to a local calendar start, e.g. `2026-10-17 00:00:00`.
#[tauri::command]
pub async fn git_log(path: String, count: i64, since: Option<String>) -> Result<Vec<Vec<String>>, String> {
    tokio::task::spawn_blocking(move || {
        ensure_git_repo(&path)?;
        let count = count.clamp(1, 50);
        let since_arg = match since.as_deref() {
            Some(s) if !s.trim().is_empty() => Some(git_since_arg(s)?),
            _ => None,
        };
        let count_arg = format!("-{count}");
        let mut args = vec![
            "log".to_string(),
            count_arg,
            "--pretty=format:%h|%s|%cr".to_string(),
            "--no-merges".to_string(),
        ];
        if let Some(s) = since_arg {
            args.push(s);
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = git_output(&path, &arg_refs)?;
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

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitFiles {
    pub subject: String,
    pub when: String,
    pub files: Vec<String>,
}

/// `git log --pretty=format:%x1e%s%x1f%cr --name-only` records, split on `\x1e`.
fn parse_log_files(text: &str) -> Vec<GitCommitFiles> {
    let mut out = Vec::new();
    for chunk in text.split('\u{1e}') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let mut lines = chunk.lines();
        let Some(head) = lines.next() else { continue };
        let mut parts = head.splitn(2, '\u{1f}');
        let subject = parts.next().unwrap_or("").trim().to_string();
        let when = parts.next().unwrap_or("").trim().to_string();
        if subject.is_empty() {
            continue;
        }
        let files = lines
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        out.push(GitCommitFiles { subject, when, files });
    }
    out
}

/// Recent commits with the paths they touched. No patches.
#[tauri::command]
pub async fn git_log_files(
    path: String,
    count: i64,
    since: Option<String>,
) -> Result<Vec<GitCommitFiles>, String> {
    tokio::task::spawn_blocking(move || {
        ensure_git_repo(&path)?;
        let count = count.clamp(1, 50);
        let since_arg = match since.as_deref() {
            Some(s) if !s.trim().is_empty() => Some(git_since_arg(s)?),
            _ => None,
        };
        let count_arg = format!("-{count}");
        let mut args = vec![
            "log".to_string(),
            count_arg,
            "--pretty=format:%x1e%s%x1f%cr".to_string(),
            "--name-only".to_string(),
            "--no-merges".to_string(),
        ];
        if let Some(s) = since_arg {
            args.push(s);
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = git_output(&path, &arg_refs)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(stderr.trim().to_string());
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(parse_log_files(&text))
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
pub async fn git_apply_cached(
    path: String,
    patch: String,
    reverse: Option<bool>,
    worktree: Option<bool>,
) -> Result<String, String> {
    let reverse = reverse.unwrap_or(false);
    let worktree = worktree.unwrap_or(false);
    tokio::task::spawn_blocking(move || git_apply_cached_sync(path, patch, reverse, worktree))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_stash_push(path: String, message: Option<String>) -> Result<String, String> {
    let message = message.unwrap_or_default();
    tokio::task::spawn_blocking(move || git_stash_push_sync(path, message))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_stash_list(path: String) -> Result<Vec<GitStashEntry>, String> {
    tokio::task::spawn_blocking(move || git_stash_list_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_stash_pop(path: String, index: u32) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_stash_apply_sync(path, index, false))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn git_stash_drop(path: String, index: u32) -> Result<String, String> {
    tokio::task::spawn_blocking(move || git_stash_apply_sync(path, index, true))
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
        out.sort_by_key(|a| a.name.to_lowercase());
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
pub async fn pick_directory(window: tauri::WebviewWindow) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || pick_directory_sync(&window))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

/// Owned by the calling window so the dialog stays above it even when that window is
/// always-on-top (the pinned ai-hub window would otherwise cover it).
fn pick_directory_sync(window: &tauri::WebviewWindow) -> Result<Option<String>, String> {
    let folder = rfd::FileDialog::new()
        .set_title("选择 Git 仓库目录")
        .set_parent(window)
        .pick_folder();
    Ok(folder.map(|p| p.to_string_lossy().to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        counts_as_published, friendly_push_error, parse_porcelain_line, parse_proxy_server,
        patch_stays_in_repo, validate_email, validate_name,
    };

    #[test]
    fn amend_of_a_published_tip_is_the_same_rule_as_undo() {
        assert!(counts_as_published(true, 0));
        assert!(!counts_as_published(true, 1));
        assert!(!counts_as_published(false, 0));
    }

    #[test]
    fn patch_rejects_paths_that_leave_the_repo() {
        let inside = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n";
        assert!(patch_stays_in_repo(inside));
        assert!(patch_stays_in_repo("--- /dev/null\n+++ b/src/new.rs\n"));
        assert!(!patch_stays_in_repo("--- a/../outside.rs\n+++ b/../outside.rs\n"));
        assert!(!patch_stays_in_repo("--- a/C:/Windows/x.rs\n+++ b/C:/Windows/x.rs\n"));
        assert!(!patch_stays_in_repo("--- /etc/passwd\n+++ /etc/passwd\n"));
    }

    #[test]
    fn push_error_translates_network_failure() {
        // The exact error from the field: github.com unreachable behind the wall.
        let raw = "fatal: unable to access 'https://github.com/mkt-agents/ai-workbench.git/': Failed to connect to github.com:443";
        let msg = friendly_push_error(raw);
        assert!(msg.contains("无法连接远程仓库"), "got: {msg}");
        assert!(msg.contains("本地提交不受影响"), "got: {msg}");
    }

    #[test]
    fn push_error_translates_timeout() {
        let msg = friendly_push_error("git push 超时（>600s），已跳过");
        assert!(msg.contains("推送超时"), "got: {msg}");
        assert!(msg.contains("本地提交不受影响"), "got: {msg}");
    }

    #[test]
    fn push_error_keeps_403_inside_unable_to_access() {
        let msg = friendly_push_error(
            "fatal: unable to access 'https://github.com/x.git/': The requested URL returned error: 403",
        );
        assert!(msg.contains("认证失败"), "got: {msg}");
    }

    #[test]
    fn proxy_server_plain_host_applies_to_http_and_https() {
        let (http, https) = parse_proxy_server("127.0.0.1:7890").unwrap();
        assert_eq!(http, "http://127.0.0.1:7890");
        assert_eq!(https, "http://127.0.0.1:7890");
    }

    #[test]
    fn proxy_server_splits_protocols() {
        let (http, https) = parse_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7891").unwrap();
        assert_eq!(http, "http://127.0.0.1:7890");
        assert_eq!(https, "http://127.0.0.1:7891");
    }

    #[test]
    fn push_error_translates_disabled_prompt() {
        let msg = friendly_push_error(
            "fatal: Cannot prompt because user interactivity has been disabled.",
        );
        assert!(msg.contains("认证失败"), "got: {msg}");
    }

    #[test]
    fn push_error_keeps_authentication_branch_before_generic_text() {
        let msg = friendly_push_error("fatal: Authentication failed for 'https://github.com/x.git/'");
        assert!(msg.contains("认证失败"), "got: {msg}");
    }

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
        diff_view_args, expand_porcelain_entry, is_not_a_git_repo, is_unknown_option, parse_config_pairs,
        parse_porcelain_line, parse_status_v2_z, status_group,
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
        for xy in ["UU", "AA", "DD", "AU", "UA", "DU", "UD"] {
            let mut c = xy.chars();
            assert_eq!(status_group(c.next().unwrap(), c.next().unwrap()), "conflicted", "{xy}");
        }
        assert_eq!(parse_porcelain_line("UU a.txt").unwrap().group, "conflicted");
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
    fn diff_view_flags_cap_context() {
        assert!(diff_view_args(false, None).is_empty());
        assert_eq!(diff_view_args(true, Some(3)), vec!["-w".to_string(), "-U3".to_string()]);
        assert_eq!(diff_view_args(false, Some(u32::MAX)), vec!["-U100000".to_string()]);
    }

    #[test]
    fn unmerged_records_are_conflicts_with_one_path() {
        // Captured from a real `git merge` conflict (content + modify/delete).
        let raw = join(&[
            "# branch.oid 4ca70b1cf68cce234cfc23ff4a9e17667f03c262",
            "# branch.head main",
            "u UU N... 100644 100644 100644 100644 18a965e1f6e48e7b23df34f2178d9e2039bd99e3 4dae846e6187b275d4bef06c06978ee7eb6b3ac6 00d20ecb9e50c8ffda9bf7fa4b703726be33e4f6 src/merging.ts",
            "u UD N... 100644 100644 000000 100644 587be6b4c3f93f93c489c0111bba5596147a26cb 351be5bf6e17c59ea560546d69654115ecb2fd8d 0000000000000000000000000000000000000000 b c.txt",
            "1 .M N... 100644 100644 100644 a b after.ts",
        ]);
        let s = parse_status_v2_z(&raw);
        assert_eq!(s.entries.len(), 3);
        assert_eq!(s.entries[0].path, "src/merging.ts");
        assert_eq!(s.entries[0].group, "conflicted");
        assert_eq!(s.entries[1].path, "b c.txt");
        assert_eq!(s.entries[1].group, "conflicted");
        assert_eq!(s.entries[2].path, "after.ts");
        // A conflict stays one row instead of splitting into staged + unstaged.
        assert_eq!(expand_porcelain_entry(s.entries[0].clone()).len(), 1);
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

#[cfg(test)]
mod branch_tests {
    use super::{
        git_command, git_create_branch_sync, git_fetch_branch_sync, git_list_branches_sync,
        git_publish_branch_sync, git_stdout, git_switch_branch_sync, resolve_branch_base,
        validate_branch_name,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn branch_name_validation_rules() {
        for ok in ["main", "feature/x", "release-1.2", "v2.0.1", "user@host", "a/b/c"] {
            assert!(validate_branch_name(ok).is_ok(), "{ok} 应当合法");
        }
        for bad in [
            "", "  ", "-lead", "lead/", "/root", "a//b", "hot.fix.lock", "a..b", "a@{x", "@",
            "a b", "a~b", "a^b", "a:b", "a?b", "a*b", "a[b", "a\\b", ".dot", "dot.", "a/.b",
        ] {
            assert!(validate_branch_name(bad).is_err(), "{bad} 应当被拒绝");
        }
    }

    fn git_run(dir: &Path, args: &[&str]) -> Result<(), String> {
        let out = git_command()
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    /// origin (bare) + work1 clone with `master` committed and pushed.
    /// Returns (temp root, origin dir, work1 dir).
    fn seed_remote(tag: &str) -> Result<(PathBuf, PathBuf, PathBuf), String> {
        let root = std::env::temp_dir().join(format!("aiwb-branch-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let origin = root.join("origin.git");
        let work1 = root.join("work1");
        std::fs::create_dir_all(&origin).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&work1).map_err(|e| e.to_string())?;
        git_run(&origin, &["init", "-q", "--bare", "-b", "master"])?;
        git_run(&work1, &["init", "-q", "-b", "master"])?;
        git_run(&work1, &["config", "user.email", "branch-test@example.com"])?;
        git_run(&work1, &["config", "user.name", "Branch Test"])?;
        std::fs::write(work1.join("file.txt"), "one\n").map_err(|e| e.to_string())?;
        git_run(&work1, &["add", "-A"])?;
        git_run(&work1, &["commit", "-q", "-m", "seed"])?;
        git_run(
            &work1,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )?;
        git_run(&work1, &["push", "-q", "-u", "origin", "master"])?;
        Ok((root, origin, work1))
    }

    #[test]
    #[ignore = "creates throwaway git repositories and shells out to git"]
    fn create_branch_switches_and_rejects_duplicates() {
        let (root, _origin, work1) = seed_remote("create").expect("seed remote");
        let dir = work1.to_string_lossy().to_string();

        git_create_branch_sync(dir.clone(), "feature/x".into(), None).expect("create branch");
        assert_eq!(
            git_stdout(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            "feature/x",
            "create must land on the new branch"
        );

        let dup = git_create_branch_sync(dir.clone(), "feature/x".into(), None).unwrap_err();
        assert!(dup.contains("已存在"), "重复创建应报已存在，实际: {dup}");
        assert!(git_create_branch_sync(dir.clone(), "-lead".into(), None).is_err());

        // An unborn HEAD (no commits) cannot branch.
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        git_run(&empty, &["init", "-q", "-b", "master"]).unwrap();
        git_run(&empty, &["config", "user.email", "branch-test@example.com"]).unwrap();
        git_run(&empty, &["config", "user.name", "Branch Test"]).unwrap();
        let err = git_create_branch_sync(empty.to_string_lossy().to_string(), "x".into(), None)
            .unwrap_err();
        assert!(err.contains("没有任何提交"), "unborn HEAD 应报无提交，实际: {err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 只建远端：工作区、HEAD、本地分支三者都不许动。
    #[test]
    #[ignore = "creates throwaway git repositories and shells out to git"]
    fn publish_branch_creates_on_origin_without_touching_worktree() {
        let (root, origin, work1) = seed_remote("publish").expect("seed remote");
        let dir = work1.to_string_lossy().to_string();
        std::fs::write(work1.join("dirty.txt"), "local only\n").unwrap();

        // release/1.0 exists on origin and as a tracking ref, but not locally.
        git_run(&work1, &["branch", "-q", "release/1.0"]).unwrap();
        git_run(&work1, &["push", "-q", "origin", "release/1.0"]).unwrap();
        git_run(&work1, &["branch", "-q", "-D", "release/1.0"]).unwrap();

        let msg = git_publish_branch_sync(
            dir.clone(),
            "feature/from-remote".into(),
            Some("release/1.0".into()),
        )
        .expect("publish from remote base");
        assert!(msg.contains("origin/release/1.0"), "应写明基点，实际: {msg}");

        let on_origin = git_stdout(
            &origin.to_string_lossy(),
            &["rev-parse", "--verify", "refs/heads/feature/from-remote"],
        )
        .expect("branch must exist on origin");
        let base =
            git_stdout(&dir, &["rev-parse", "--verify", "refs/remotes/origin/release/1.0"]).unwrap();
        assert_eq!(
            on_origin.trim(),
            base.trim(),
            "远端新分支必须指向基点同一个提交"
        );

        assert_eq!(
            git_stdout(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap().trim(),
            "master",
            "HEAD 不许动"
        );
        assert!(work1.join("dirty.txt").exists(), "工作区文件不该被动过");
        assert!(
            git_stdout(&dir, &["for-each-ref", "refs/heads/feature/from-remote"])
                .unwrap()
                .is_empty(),
            "本地不该出现这个分支"
        );

        let again =
            git_publish_branch_sync(dir.clone(), "feature/from-remote".into(), Some("release/1.0".into()))
                .unwrap();
        assert!(again.contains("未重复创建"), "重复发布应报无需重复，实际: {again}");

        let missing =
            git_publish_branch_sync(dir.clone(), "feature/nope".into(), Some("no/such/base".into()))
                .unwrap_err();
        assert!(missing.contains("找不到基点"), "缺基点应报找不到，实际: {missing}");

        // No origin → plain refusal before any network call.
        let lone = root.join("lone");
        std::fs::create_dir_all(&lone).unwrap();
        git_run(&lone, &["init", "-q", "-b", "master"]).unwrap();
        std::fs::write(lone.join("a.txt"), "a\n").unwrap();
        git_run(&lone, &["config", "user.email", "t@example.com"]).unwrap();
        git_run(&lone, &["config", "user.name", "T"]).unwrap();
        git_run(&lone, &["add", "-A"]).unwrap();
        git_run(&lone, &["commit", "-q", "-m", "seed"]).unwrap();
        let no_remote =
            git_publish_branch_sync(lone.to_string_lossy().to_string(), "x".into(), None).unwrap_err();
        assert!(no_remote.contains("origin"), "实际: {no_remote}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 「先从远端取基点」：fetch 只推进跟踪引用，且基点一律优先走远端引用。
    #[test]
    #[ignore = "creates throwaway git repositories and shells out to git"]
    fn fetch_branch_advances_tracking_ref_and_base_prefers_remote() {
        let (root, origin, work1) = seed_remote("fetch").expect("seed remote");
        git_run(&root, &["clone", "-q", &origin.to_string_lossy(), "work2"]).unwrap();
        let work2 = root.join("work2");
        let dir2 = work2.to_string_lossy().to_string();

        std::fs::write(work1.join("file.txt"), "one\ntwo\n").unwrap();
        git_run(&work1, &["add", "-A"]).unwrap();
        git_run(&work1, &["commit", "-q", "-m", "second"]).unwrap();
        git_run(&work1, &["push", "-q", "origin", "master"]).unwrap();

        let stale =
            git_stdout(&dir2, &["rev-parse", "--verify", "refs/remotes/origin/master"]).unwrap();
        // work2's local master is behind: a base that preferred local would fork
        // off the wrong commit.
        let (r, sha) = resolve_branch_base(&dir2, Some("master")).expect("base resolves");
        assert_eq!(r, "refs/remotes/origin/master", "基点必须走远端引用，实际: {r}");
        assert_eq!(sha, stale.trim().chars().take(7).collect::<String>());

        let msg = git_fetch_branch_sync(dir2.clone(), "master".into()).expect("fetch");
        assert!(msg.contains("已取到 origin/master"), "实际: {msg}");
        let fresh =
            git_stdout(&dir2, &["rev-parse", "--verify", "refs/remotes/origin/master"]).unwrap();
        assert_ne!(fresh, stale, "跟踪引用应被推进");
        let (_, sha2) = resolve_branch_base(&dir2, Some("master")).unwrap();
        assert_eq!(sha2, fresh.trim().chars().take(7).collect::<String>());

        let none = git_fetch_branch_sync(dir2.clone(), "no/such".into()).unwrap_err();
        assert!(none.contains("远端没有分支"), "实际: {none}");

        // The fresh commit is what lands on origin — no checkout involved.
        git_publish_branch_sync(dir2.clone(), "feature/off-fresh".into(), Some("master".into()))
            .expect("publish fresh");
        let on_origin = git_stdout(
            &origin.to_string_lossy(),
            &["rev-parse", "--verify", "refs/heads/feature/off-fresh"],
        )
        .unwrap();
        assert_eq!(on_origin.trim(), fresh.trim(), "远端新分支应指向刚取到的提交");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "creates throwaway git repositories and shells out to git"]
    fn switch_branch_dwims_to_remote_only_branch() {
        let (root, origin, work1) = seed_remote("dwim").expect("seed remote");
        // Publish a branch work2 will only have as a remote-tracking ref.
        git_run(&work1, &["branch", "feature/x"]).unwrap();
        git_run(&work1, &["push", "-q", "origin", "feature/x"]).unwrap();

        git_run(&root, &["clone", "-q", &origin.to_string_lossy(), "work2"]).unwrap();
        let work2 = root.join("work2");
        let dir = work2.to_string_lossy().to_string();

        let list = git_list_branches_sync(dir.clone()).expect("list branches");
        assert_eq!(list.current, "master");
        assert!(list.local.contains(&"master".to_string()));
        assert!(!list.local.iter().any(|b| b == "feature/x"));
        // Remote refs carry plain branch names — `origin/feature/x` lstrips to
        // `feature/x`, so it can merge with the local list in the UI.
        assert!(list.remote.contains(&"feature/x".to_string()));
        assert!(list.remote.contains(&"master".to_string()));
        assert!(!list.remote.iter().any(|b| b == "HEAD" || b.is_empty()));

        git_switch_branch_sync(dir.clone(), "feature/x".into()).expect("DWIM switch");
        let after = git_list_branches_sync(dir).unwrap();
        assert_eq!(after.current, "feature/x");
        assert!(after.local.contains(&"feature/x".to_string()));

        // A branch nobody has anywhere must say so plainly.
        let missing = git_switch_branch_sync(root.join("work2").to_string_lossy().to_string(), "nope".into())
            .unwrap_err();
        assert!(missing.contains("不存在"), "缺失分支应报不存在，实际: {missing}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "creates throwaway git repositories and shells out to git"]
    fn switch_branch_refuses_conflicting_dirty_tree() {
        let (root, _origin, work1) = seed_remote("conflict").expect("seed remote");
        let dir = work1.to_string_lossy().to_string();

        // dev carries a different content of file.txt.
        git_run(&work1, &["checkout", "-q", "-b", "dev"]).unwrap();
        std::fs::write(work1.join("file.txt"), "two\n").unwrap();
        git_run(&work1, &["commit", "-q", "-am", "dev change"]).unwrap();
        git_run(&work1, &["checkout", "-q", "master"]).unwrap();

        // Uncommitted edit to the same file: git must refuse, we must not force.
        std::fs::write(work1.join("file.txt"), "three\n").unwrap();
        assert!(
            git_switch_branch_sync(dir.clone(), "dev".into()).is_err(),
            "冲突脏树切换必须失败"
        );

        // Restore via git (not a manual write): autocrlf means a hand-written
        // "one\n" can still count as modified on Windows.
        git_run(&work1, &["checkout", "--", "file.txt"]).unwrap();
        git_switch_branch_sync(dir.clone(), "dev".into()).expect("clean switch");
        assert_eq!(
            git_stdout(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            "dev"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod conflict_flow_tests {
    use super::*;
    use std::path::PathBuf;

    fn sh(dir: &Path, args: &[&str]) {
        let out = git_command().args(args).current_dir(dir).output().expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn clone_into(root: &Path, remote: &Path, name: &str) -> PathBuf {
        sh(root, &["clone", "-q", &remote.to_string_lossy(), name]);
        let dir = root.join(name);
        sh(&dir, &["config", "user.email", "t@t"]);
        sh(&dir, &["config", "user.name", "t"]);
        sh(&dir, &["config", "core.autocrlf", "false"]);
        dir
    }

    /// Reverting one block of a hunk in the working file leaves the other block
    /// alone, and applying the same patch forward brings it back (撤销还原).
    #[test]
    fn worktree_block_revert_and_undo() {
        if git_command().arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("aw-block-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        sh(&root, &["init", "-q", "-b", "main"]);
        sh(&root, &["config", "user.email", "t@t"]);
        sh(&root, &["config", "user.name", "t"]);
        sh(&root, &["config", "core.autocrlf", "false"]);
        let original: String = (1..=10).map(|i| format!("l{i}\n")).collect();
        std::fs::write(root.join("f.txt"), &original).unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-qm", "base"]);
        let changed = original.replace("l2\n", "L2\n").replace("l8\n", "L8\n");
        std::fs::write(root.join("f.txt"), &changed).unwrap();
        // One hunk, two blocks; only the first block (l2) is kept, the other
        // block's new line stays as context: what diffRows.blockPatch("new") builds.
        let patch = "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,10 +1,10 @@\n l1\n-l2\n+L2\n l3\n l4\n l5\n l6\n l7\n L8\n l9\n l10\n";
        let rp = root.to_string_lossy().into_owned();
        assert_eq!(git_apply_cached_sync(rp.clone(), patch.into(), true, true).unwrap(), "已还原所选改动");
        assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), original.replace("l8\n", "L8\n"));
        git_apply_cached_sync(rp.clone(), patch.into(), false, true).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("f.txt")).unwrap(), changed);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two clones edit the same line; the second one's merge pull must stop and
    /// hand the conflict over instead of resolving or aborting it.
    #[test]
    fn merge_pull_keeps_conflicts_for_the_user() {
        if git_command().arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("aw-flow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let remote = root.join("remote.git");
        sh(&root, &["init", "-q", "--bare", "-b", "main", &remote.to_string_lossy()]);
        let a = clone_into(&root, &remote, "a");
        std::fs::write(a.join("f.txt"), "one\nbase\nthree\n").unwrap();
        sh(&a, &["add", "-A"]);
        sh(&a, &["commit", "-qm", "base"]);
        sh(&a, &["push", "-q", "-u", "origin", "HEAD"]);
        let b = clone_into(&root, &remote, "b");

        std::fs::write(a.join("f.txt"), "one\nfrom a\nthree\n").unwrap();
        sh(&a, &["commit", "-qam", "a"]);
        sh(&a, &["push", "-q"]);
        std::fs::write(b.join("f.txt"), "one\nfrom b\nthree\n").unwrap();
        sh(&b, &["commit", "-qam", "b"]);
        let bp = b.to_string_lossy().into_owned();

        let rejected = git_push_sync(bp.clone()).unwrap_err();
        assert!(rejected.starts_with("[GIT_PUSH_REJECTED]"), "{rejected}");
        let diverged = git_pull_sync(bp.clone(), false).unwrap_err();
        assert!(diverged.starts_with("[GIT_DIVERGED]"), "{diverged}");

        let err = git_pull_sync(bp.clone(), true).unwrap_err();
        assert!(err.starts_with("[GIT_CONFLICT]") && err.contains("f.txt"), "{err}");
        let state = crate::git_merge::conflict_state(&bp);
        assert_eq!(state.operation, Some("merge"), "merge state must be kept");
        assert_eq!(state.conflicted, vec!["f.txt".to_string()]);

        assert!(git_commit_sync(bp.clone(), "x".into(), false).unwrap_err().contains("冲突"));
        assert!(git_push_sync(bp.clone()).unwrap_err().starts_with("[GIT_OPERATION_IN_PROGRESS]"));
        assert!(git_stage_sync(bp.clone(), vec![]).unwrap_err().starts_with("[GIT_CONFLICT]"));
        assert!(git_discard_sync(bp.clone(), "f.txt".into(), false, false).is_err());
        // 批量切换分支走同一条门：不给原始 checkout 报错，而是指回提交页。
        let switched = git_switch_branch_sync(bp.clone(), "main".into()).unwrap_err();
        assert!(switched.starts_with("[GIT_OPERATION_IN_PROGRESS]"), "{switched}");

        let file = crate::git_merge::git_conflict_file_sync(bp.clone(), "f.txt".into()).unwrap();
        assert_eq!(file.kind, "content");
        assert!(file.segments.iter().any(|s| matches!(s, crate::git_merge::MergeSegment::Conflict { .. })));
        let marked = crate::git_merge::git_resolve_conflict_sync(
            bp.clone(),
            "f.txt".into(),
            Some("<<<<<<< ours\nx\n>>>>>>> theirs\n".into()),
            None,
        );
        assert!(marked.is_err(), "markers must be refused");
        crate::git_merge::git_resolve_conflict_sync(bp.clone(), "f.txt".into(), Some("one\nfrom a and b\nthree\n".into()), None)
            .unwrap();

        let merge = crate::git_merge::git_merge_state_sync(bp.clone()).unwrap();
        assert!(merge.conflicted.is_empty());
        assert!(merge.message.starts_with("Merge"), "{}", merge.message);
        git_commit_sync(bp.clone(), merge.message, false).unwrap();
        let after = crate::git_merge::conflict_state(&bp);
        assert!(after.operation.is_none() && after.conflicted.is_empty());
        git_push_sync(bp.clone()).unwrap();
        // 门只挡冲突：合并提交完、工作区干净之后，切换必须照常成功。
        sh(&b, &["checkout", "-q", "-b", "side"]);
        sh(&b, &["checkout", "-q", "main"]);
        git_switch_branch_sync(bp.clone(), "side".into()).expect("clean repo must switch");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod current_branch_tests {
    use super::{git_command, git_current_branch_sync};
    use std::path::Path;

    fn git_ok(dir: &Path, args: &[&str]) -> bool {
        git_command()
            .args(args)
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn empty_repo_keeps_its_branch_and_detached_head_has_none() {
        let root = std::env::temp_dir().join(format!("aw-current-branch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        if !git_ok(&root, &["init", "-q", "-b", "main"]) {
            let _ = std::fs::remove_dir_all(&root);
            return; // 本机没有 git：当跳过
        }
        let dir = root.to_string_lossy().into_owned();

        // 一次提交都没有也要给出分支名：`rev-parse --abbrev-ref HEAD` 在这里会 fatal。
        assert_eq!(git_current_branch_sync(&dir).unwrap(), "main");

        assert!(git_ok(&root, &["config", "user.email", "branch@test"]));
        assert!(git_ok(&root, &["config", "user.name", "Branch Test"]));
        std::fs::write(root.join("a.txt"), "x").unwrap();
        assert!(git_ok(&root, &["add", "-A"]));
        assert!(git_ok(&root, &["commit", "-qm", "seed"]));

        // 换了分支就得跟着变——分支级「以后都允许」全靠这个值。
        assert!(git_ok(&root, &["checkout", "-q", "-b", "feature/x"]));
        assert_eq!(git_current_branch_sync(&dir).unwrap(), "feature/x");

        // detached HEAD：没有分支可记，报错让调用点回落到仓库粒度。
        assert!(git_ok(&root, &["checkout", "-q", "--detach"]));
        assert!(git_current_branch_sync(&dir).is_err());

        // 非 git 目录同样报错，而不是返回空串。
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(git_current_branch_sync(&plain.to_string_lossy()).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }
}