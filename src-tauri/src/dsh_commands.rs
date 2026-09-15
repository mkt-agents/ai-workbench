use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::Command;
use std::io::{BufRead, BufReader, Read, Write};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tauri::Manager;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x08000000;

/// How long the server gets to start listening, and the port poll interval.
const DSH_START_TIMEOUT_SECS: u64 = 30;
const DSH_POLL_INTERVAL_MS: u64 = 250;
/// A freshly started server opens its port slightly before it can answer for `/`, so the
/// auth probe retries briefly instead of judging on the first attempt.
const DSH_HTTP_TIMEOUT_SECS: u64 = 6;
const DSH_HTTP_POLL_MS: u64 = 400;

/// Backup path for the DSH auth patch. The pre-rename suffix is still
/// recognised so a patch applied by an older build can still be restored.
fn dsh_backup_path(target: &std::path::Path) -> PathBuf {
    let current = target.with_extension("js.ai-workbench-backup");
    let legacy = target.with_extension("js.worktools-backup");
    if current.exists() || !legacy.exists() {
        current
    } else {
        legacy
    }
}

use crate::{config::{DSH_DEFAULT_PORT}, DshInstance, DshState};

/// Machine+User PATH exactly as the registry has it now. Every call costs a
/// PowerShell spawn, so multi-step operations read it once and hand the value to
/// `apply_path_value`.
fn current_path_value() -> String {
    crate::runtime_commands::load_env_snapshot().effective_path()
}

fn apply_path_value(cmd: &mut Command, path: &str) {
    if !path.trim().is_empty() {
        cmd.env("PATH", path);
    }
}

/// Hand a child process the *current* Machine+User PATH from the registry. Without
/// this, DSH/npm started after a runtime switch would keep using the Node.js that
/// was on PATH when this app launched.
fn apply_current_path(cmd: &mut Command) {
    apply_path_value(cmd, &current_path_value());
}

fn npm_global_root() -> Option<PathBuf> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "npm", "root", "-g"]);
    apply_current_path(&mut cmd);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Parse a package.json (used for the version and the bin entry).
fn read_package_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Directories on the (registry) PATH that look like a node/npm install. npm's builtin
/// prefix is exactly such a directory, so this both anchors the global-root candidates
/// and locates the node.exe used to launch DSH.
fn node_dirs_on_path(path_value: &str) -> Vec<PathBuf> {
    path_value
        .split(';')
        .map(|dir| PathBuf::from(dir.trim()))
        .filter(|dir| !dir.as_os_str().is_empty())
        .filter(|dir| dir.join("node.exe").is_file() || dir.join("npm.cmd").is_file())
        .collect()
}

/// Where the globally installed DSH package lives, without paying npm's ~2.3s startup.
///
/// npm on Windows installs globals into `%APPDATA%\npm\node_modules`, but the prefix can
/// be customised (and can follow a switched runtime), so `npm root -g` stays as the
/// fallback and only runs when the fast candidates miss.
fn dsh_package_dir(path_value: &str) -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(appdata).join("npm").join("node_modules"));
    }
    // Runtime-managed installs (nvm, fnm, a custom node dir) put globals under the node
    // directory that is currently on PATH.
    for dir in node_dirs_on_path(path_value) {
        roots.push(dir.join("node_modules"));
    }

    let installed = |root: PathBuf| {
        let pkg = root.join("@deepseek-ai").join("dsh");
        pkg.join("package.json").is_file().then_some(pkg)
    };
    for root in roots {
        if let Some(pkg) = installed(root) {
            return Some(pkg);
        }
    }

    installed(npm_global_root()?)
}

fn find_dsh_connection_index() -> Result<PathBuf, String> {
    // Fast path first: derived from disk, so a normal install never starts npm here.
    if let Some(pkg) = dsh_package_dir(&current_path_value()) {
        let path = pkg
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-client-connection")
            .join("lib")
            .join("index.js");
        if path.exists() {
            return Ok(path);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(root) = npm_global_root() {
        candidates.push(
            root.join("@deepseek-ai")
                .join("dsh")
                .join("node_modules")
                .join("@deepseek-ai")
                .join("dsh-client-connection")
                .join("lib")
                .join("index.js"),
        );
        candidates.push(
            root.join("@deepseek-ai")
                .join("dsh-client-connection")
                .join("lib")
                .join("index.js"),
        );
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        candidates.push(
            PathBuf::from(appdata)
                .join("npm")
                .join("node_modules")
                .join("@deepseek-ai")
                .join("dsh")
                .join("node_modules")
                .join("@deepseek-ai")
                .join("dsh-client-connection")
                .join("lib")
                .join("index.js"),
        );
    }

    for path in &candidates {
        if path.exists() {
            return Ok(path.clone());
        }
    }

    let tried = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" | ");
    Err(format!(
        "DSH connection file not found. Tried: {}",
        if tried.is_empty() {
            "(no candidates — npm root -g failed)".to_string()
        } else {
            tried
        }
    ))
}

/// Written into a patched file so "we already patched this" can be answered without
/// guessing from the code shape (see `patch_dsh_auth`).
const DSH_PATCH_SENTINEL: &str = "/* ai-workbench:dsh-auth-bypass */";

/// The two expressions the patch rewrites. Both must be present for it to apply.
const DSH_PATCH_AUTH_EXPR: &str =
    "return this.browserAuth.isAuthenticated(request) ? void 0 : 401;";
const DSH_PATCH_INDEX_EXPR: &str =
    "return this.browserAuth.authorizeIndex(request, response);";

/// Result of a DSH auth patch operation.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshPatchResult {
    pub patched: bool,
    pub message: String,
    /// Set when the patch could not be applied (e.g. package layout changed).
    pub warning: Option<String>,
}

/// Patch DSH connection package to bypass browser cookie authentication for iframe embedding.
/// Keeps the Host/Origin trust fence intact. Idempotent — safe to call multiple times.
///
/// Before patching, backs up the original file alongside the target so it can be
/// restored if the patch produces a broken DSH install (e.g. after a DSH update).
fn patch_dsh_auth() -> Result<DshPatchResult, String> {
    let target = find_dsh_connection_index()?;

    let content = fs::read_to_string(&target).map_err(|e| format!("Read failed: {e}"))?;

    // Detect *our* patch by the sentinel we write, never by code shape: DSH
    // 0.1.5-rc.1 ships stock code containing the very patterns the patch produces
    // (standalone `return void 0;` / `return true;` lines), so shape-based detection
    // declared a pristine file "already patched" — and, because it returned before the
    // backup step, left 还原 permanently failing with "未找到备份文件".
    if content.contains(DSH_PATCH_SENTINEL) {
        return Ok(DshPatchResult {
            patched: true,
            message: "DSH 认证补丁已存在".into(),
            warning: None,
        });
    }

    // Both targets must exist for the rewrite to mean anything. Newer DSH releases
    // authorize the embedded UI through `--trusted-host` plus a launch token instead,
    // so their absence is expected: skip quietly rather than warn on every start.
    if !content.contains(DSH_PATCH_AUTH_EXPR) || !content.contains(DSH_PATCH_INDEX_EXPR) {
        return Ok(DshPatchResult {
            patched: false,
            message: "当前 DSH 版本使用内置授权，无需认证补丁".into(),
            warning: None,
        });
    }

    // Back up the original before patching so we can restore on failure/update.
    let backup_path = dsh_backup_path(&target);
    if !backup_path.exists() {
        fs::write(&backup_path, &content)
            .map_err(|e| format!("备份原始文件失败: {e}"))?;
    }

    let patched = content
        .replace(DSH_PATCH_AUTH_EXPR, "return void 0;")
        .replace(DSH_PATCH_INDEX_EXPR, "return true;");

    if patched == content {
        return Ok(DshPatchResult {
            patched: false,
            message: "DSH 认证补丁模式未找到".into(),
            warning: Some(
                "DSH 包布局可能已变更，iframe 嵌入可能需手动适配".into(),
            ),
        });
    }

    // Sentinel on the first line: this is what makes "已打过补丁" answerable later.
    fs::write(&target, format!("{DSH_PATCH_SENTINEL}\n{patched}"))
        .map_err(|e| format!("写入补丁失败: {e}"))?;

    Ok(DshPatchResult {
        patched: true,
        message: "已应用 DSH 认证绕过补丁".into(),
        warning: None,
    })
}

/// Restore the original DSH connection file from the backup created by patch_dsh_auth.
/// Blocking core of `restore_dsh_auth`: resolving the connection file runs
/// `npm root -g` (plus a registry read), so this must not sit on the main thread.
fn restore_dsh_auth_sync() -> Result<String, String> {
    let target = find_dsh_connection_index()?;
    let backup_path = dsh_backup_path(&target);
    if !backup_path.exists() {
        // The patch path always writes the backup before touching the file, and a DSH
        // update replaces both — so a missing backup normally means the file is already
        // pristine. Only our sentinel proves a patch is still live.
        let content = fs::read_to_string(&target).unwrap_or_default();
        if content.contains(DSH_PATCH_SENTINEL) {
            return Err(
                "补丁仍在生效，但备份文件已丢失，无法还原；可重新安装 DSH 恢复原始文件：npm i -g @deepseek-ai/dsh"
                    .into(),
            );
        }
        return Ok("当前 DSH 文件未被本工具修改，无需还原".into());
    }
    let original = fs::read_to_string(&backup_path).map_err(|e| format!("读取备份失败: {e}"))?;
    fs::write(&target, original).map_err(|e| format!("恢复原始文件失败: {e}"))?;
    fs::remove_file(&backup_path).ok();
    Ok("已恢复 DSH 原始认证文件".into())
}

#[tauri::command]
pub async fn restore_dsh_auth() -> Result<String, String> {
    tokio::task::spawn_blocking(restore_dsh_auth_sync)
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

fn is_port_in_use(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// Marker baked into DSH's web UI HTML. Only a listener that actually answers with
/// this is treated as DSH, so an unrelated program on the same port is never mistaken
/// for a running service.
const DSH_HTML_MARKER: &str = "@deepseek-ai/dsh";

/// Plain (no token, no cookie) `GET /` against `port`, read capped at `max_bytes`.
/// These probes run inside `spawn_blocking`, and std keeps them dependency-free.
fn dsh_plain_get(port: u16, max_bytes: u64) -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(800)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1500)));
    let request = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/html\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let _ = stream.take(max_bytes).read_to_end(&mut buf);
    Some(String::from_utf8_lossy(&buf).to_string())
}

/// Does a real DSH web server answer on `port`? The marker check is what makes adopting
/// an untracked listener safe — an unrelated program on the same port never matches.
fn dsh_is_serving(port: u16) -> bool {
    dsh_plain_get(port, 32 * 1024)
        .map(|resp| resp.to_ascii_lowercase().contains(DSH_HTML_MARKER))
        .unwrap_or(false)
}

/// HTTP status of that plain `GET /`.
fn dsh_index_status(port: u16) -> Option<u16> {
    let head = dsh_plain_get(port, 2048)?;
    // "HTTP/1.1 200 OK\r\n…" → the second token.
    head.split_whitespace().nth(1)?.parse::<u16>().ok()
}

/// What a plain `GET /` — the iframe's first load, with no token and no cookie — says
/// about authorization.
#[derive(PartialEq)]
enum IndexAuth {
    /// The app is being served: the iframe can load it as-is.
    Serving,
    /// Explicitly refused: this build needs the auth patch.
    Refused,
    /// Not answering yet. DSH serves `404` for roughly the first 1.4s after the port
    /// opens, and that must not be read as a refusal — patching cannot fix a boot.
    NotReady,
}

fn probe_index_auth(port: u16) -> IndexAuth {
    match dsh_index_status(port) {
        Some(status) if (200..400).contains(&status) => IndexAuth::Serving,
        // DSH answers 401 when its authorization refuses the request.
        Some(401) | Some(403) => IndexAuth::Refused,
        _ => IndexAuth::NotReady,
    }
}

/// Wait for a verdict, or report `NotReady` when the window expires.
fn wait_for_index_auth(port: u16) -> IndexAuth {
    let deadline = std::time::Instant::now() + Duration::from_secs(DSH_HTTP_TIMEOUT_SECS);
    loop {
        match probe_index_auth(port) {
            IndexAuth::NotReady => {}
            verdict => return verdict,
        }
        if std::time::Instant::now() >= deadline {
            return IndexAuth::NotReady;
        }
        thread::sleep(Duration::from_millis(DSH_HTTP_POLL_MS));
    }
}

/// DSH web-server arguments, so both launch strategies stay in sync.
fn dsh_web_args(port: u16) -> Vec<String> {
    vec![
        "web".into(),
        "--no-open".into(),
        "--port".into(),
        port.to_string(),
        "--trusted-host".into(),
        "127.0.0.1".into(),
        "--trusted-host".into(),
        format!("127.0.0.1:{port}"),
    ]
}

/// Environment and flags shared by every launch attempt.
fn configure_dsh_command(cmd: &mut Command, path_value: &str) {
    cmd.env("DSH_SKIP_AUTH", "1");
    apply_path_value(cmd, path_value);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Spawn the DSH web server. `--no-open` keeps it from launching a browser, and
/// `--trusted-host` authorizes the loopback iframe this app embeds (that is `extra` in
/// DSH's `resolveLanTrust`, which does not include loopback on its own).
fn spawn_dsh_server(port: u16, path_value: &str) -> Result<std::process::Child, String> {
    let args = dsh_web_args(port);
    let mut previous: Option<String> = None;

    // Prefer the installed entry point: `node lib/bin.js` costs ~0.2s, where
    // `npx @deepseek-ai/dsh` spends ~2.5s resolving before node even starts.
    if let Some(pkg) = dsh_package_dir(path_value) {
        let entry = read_package_json(&pkg.join("package.json"))
            .and_then(|json| match json.get("bin") {
                Some(serde_json::Value::String(script)) => Some(script.clone()),
                Some(bin) => bin.get("dsh").and_then(|v| v.as_str()).map(str::to_string),
                None => None,
            })
            .map(|rel| pkg.join(rel))
            .filter(|path| path.is_file());
        if let Some(entry) = entry {
            // Absolute node.exe from the same PATH the package came from: the child gets a
            // different PATH of its own, so a bare "node" could resolve elsewhere.
            let program = node_dirs_on_path(path_value)
                .into_iter()
                .map(|dir| dir.join("node.exe"))
                .find(|exe| exe.is_file())
                .unwrap_or_else(|| PathBuf::from("node"));
            let mut cmd = Command::new(&program);
            cmd.arg(&entry).args(&args);
            configure_dsh_command(&mut cmd, path_value);
            match cmd.spawn() {
                Ok(child) => return Ok(child),
                Err(e) => previous = Some(format!("node 直接启动失败: {e}")),
            }
        }
    }

    // Fallback: npx, which also covers package layouts we do not recognise.
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "npx", "@deepseek-ai/dsh"]).args(&args);
    configure_dsh_command(&mut cmd, path_value);
    cmd.spawn().map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::NotFound {
            "未找到 Node.js，请先安装，或在「版本切换」里选择可用的 Node"
        } else {
            "启动 DSH 失败"
        };
        match &previous {
            Some(prev) => format!("{hint}: {e}（{prev}）"),
            None => format!("{hint}: {e}"),
        }
    })
}

/// Wait until the port listens: the authoritative "process is up" signal.
fn wait_for_port(port: u16) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(DSH_START_TIMEOUT_SECS);
    while std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(DSH_POLL_INTERVAL_MS));
        if is_port_in_use(port) {
            return true;
        }
    }
    false
}

/// Kill the server and the children `cmd /c npx` spawned.
fn stop_child(child: &mut std::process::Child) {
    kill_pid(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// Wait for a killed server to release its port, so its replacement can bind it.
/// Without this, `wait_for_port` can see the dying socket and start the successor too
/// early, which then fails to listen.
fn wait_for_port_release(port: u16) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while is_port_in_use(port) && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(200));
    }
}

/// Best-effort "who holds the port" suffix, so the failure text says which program to
/// close instead of leaving the user to guess.
fn port_owner_description(port: u16) -> String {
    let Some(pid) = find_pid_by_port(port) else {
        return String::new();
    };
    let mut cmd = Command::new("cmd");
    cmd.args([
        "/c",
        "tasklist",
        "/FI",
        &format!("PID eq {pid}"),
        "/FO",
        "CSV",
        "/NH",
    ]);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let name = cmd
        .output()
        .ok()
        .and_then(|o| {
            // CSV row: "node.exe","3656","Console","1","123,456 K"
            String::from_utf8_lossy(&o.stdout)
                .split(',')
                .next()
                .map(|first| first.trim_matches(|c| c == '"' || c == ' ').to_string())
        })
        .filter(|n| !n.is_empty() && !n.starts_with("INFO:"));
    match name {
        Some(n) => format!("（PID {pid} · {n}）"),
        None => format!("（PID {pid}）"),
    }
}

/// Register the default port as running when DSH answers on it but nothing is tracked.
/// Callers probe first (so no lock is held during the network round-trip).
fn adopt_untracked_dsh(instances: &mut Vec<DshInstance>) {
    if instances.iter().any(|i| i.port == DSH_DEFAULT_PORT) {
        return;
    }
    instances.push(DshInstance {
        pid: 0, // unknown: discovered by port, not spawned by us
        port: DSH_DEFAULT_PORT,
        auth_url: None,
        auth_patch_warning: None,
    });
}

/// Check whether a DSH instance is still healthy by probing the port.
///
/// We deliberately do NOT rely on the tracked PID: PIDs can be reused by the
/// OS, so a "live" PID may belong to a completely different process. The port
/// is the real source of truth for whether DSH is serving.
fn is_instance_alive(port: u16) -> bool {
    is_port_in_use(port)
}

/// Drop tracked instances whose port is no longer listening.
fn prune_dead_instances(instances: &mut Vec<DshInstance>) {
    instances.retain(|i| is_instance_alive(i.port))
}

fn local_addr_port(addr: &str) -> Option<u16> {
    let (_, raw) = addr.rsplit_once(':')?;
    raw.parse().ok()
}

fn line_listens_on_port(line: &str, port: u16) -> bool {
    let parts: Vec<&str> = line.split_whitespace().collect();
    // netstat: Proto  Local  Foreign  State  PID. Match the local port exactly
    // so 3080 does not kill a listener on 30800.
    if parts.len() < 4 || !parts.iter().any(|p| *p == "LISTENING") {
        return false;
    }
    local_addr_port(parts[1]) == Some(port)
}

/// Best-effort kill of a PID via the platform's taskkill/kill. Errors are
/// intentionally ignored — the process may have already exited.
fn kill_pid(pid: u32) {
    #[cfg(target_os = "windows")]
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    #[cfg(not(target_os = "windows"))]
    let _ = Command::new("kill").arg(pid.to_string()).output();
}

#[cfg(target_os = "windows")]
fn find_pid_by_port(port: u16) -> Option<u32> {
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "netstat", "-ano", "-p", "TCP"]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line_listens_on_port(line, port) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(pid_str) = parts.last() {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    return Some(pid);
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn find_pid_by_port(_port: u16) -> Option<u32> {
    None
}

fn sanitize_route_part(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    cleaned
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

/// Install dsh globally via npm and stream progress to frontend.
#[tauri::command]
pub async fn install_dsh(window: tauri::Window, version: Option<String>) -> Result<String, String> {
    use tauri::Emitter;

    let pkg = match &version {
        Some(v) => format!("@deepseek-ai/dsh@{}", v),
        None => "@deepseek-ai/dsh".to_string(),
    };

    window.emit("dsh:install_progress", serde_json::json!({
        "stage": "downloading",
        "percent": 10,
        "message": format!("正在下载 {}...", pkg)
    })).ok();

    let window_clone = window.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", "npm", "install", "-g", &pkg]);
        apply_current_path(&mut cmd);

        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        cmd.stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| format!("Failed to start npm: {}", e))?;

        // Stream stderr for progress feedback
        let stderr = child.stderr.take().unwrap();
        let reader = BufReader::new(stderr);
        let mut last_msg = String::new();

        for line in reader.lines() {
            if let Ok(line) = line {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() { continue; }

                // Parse npm progress output
                let (stage, percent, msg) = if trimmed.contains("added") || trimmed.contains("updated") {
                    ("installing", 70, format!("正在安装: {}", trimmed))
                } else if trimmed.contains("fetch") || trimmed.contains("http") {
                    ("downloading", 30, format!("下载中: {}", trimmed))
                } else if trimmed.contains("extract") || trimmed.contains("tar") {
                    ("extracting", 50, format!("解压中: {}", trimmed))
                } else if trimmed.contains("link") {
                    ("linking", 80, format!("链接中: {}", trimmed))
                } else {
                    ("progress", 40, trimmed.clone())
                };

                if trimmed != last_msg {
                    last_msg = trimmed;
                    let _ = window_clone.emit("dsh:install_progress", serde_json::json!({
                        "stage": stage,
                        "percent": percent,
                        "message": msg
                    }));
                }
            }
        }

        child.wait().map_err(|e| format!("Wait failed: {}", e))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
    .map_err(|e| format!("Install failed: {}", e))?;

    if result.success() {
        // Apply auth bypass patch after install
        let _ = patch_dsh_auth();
        window.emit("dsh:install_progress", serde_json::json!({
            "stage": "done",
            "percent": 100,
            "message": "安装完成"
        })).ok();
        Ok("安装成功".to_string())
    } else {
        Err("安装失败，请检查网络连接".to_string())
    }
}

/// Blocking core of `start_dsh`. Runs on a worker thread: it spawns `npx`, polls the
/// port for up to 30s and shells out to npm/PowerShell — none of which may run on the
/// main thread (that is what made the window report "not responding").
///
/// The instances lock is deliberately *not* held during that work, so `list_dsh` /
/// `stop_dsh` stay usable while a start is in flight.
fn start_dsh_sync(port: Option<u16>, window: tauri::Window) -> Result<DshInstance, String> {
    // Fixed port contract: never silently remap (FE embeds the requested port).
    let target_port = port.unwrap_or(DSH_DEFAULT_PORT);
    {
        let state = window.state::<DshState>();
        let mut instances = state.instances.lock().map_err(|e| e.to_string())?;
        prune_dead_instances(&mut instances);
        if let Some(existing) = instances.iter().find(|i| i.port == target_port).cloned() {
            return Ok(existing);
        }
    }
    if is_port_in_use(target_port) {
        // Occupied yet untracked: `DshState` is in-memory, so after an app restart an
        // already-running DSH looks "not started" while its port stays taken. Adopting
        // it satisfies the request (and re-enables Stop) instead of failing.
        if dsh_is_serving(target_port) {
            let instance = DshInstance {
                pid: 0, // discovered by port, not spawned here
                port: target_port,
                auth_url: None,
                auth_patch_warning: None,
            };
            {
                let state = window.state::<DshState>();
                let mut instances = state.instances.lock().map_err(|e| e.to_string())?;
                instances.retain(|i| i.port != target_port);
                instances.push(instance.clone());
            }
            return Ok(instance);
        }
        return Err(format!(
            "端口 {} 已被其他程序占用，且探测发现它不是 DeepSeek 服务{}。请关闭该程序后重试。",
            target_port,
            port_owner_description(target_port)
        ));
    }

    // One registry read for the whole start: every child below must see the PATH
    // written by the most recent runtime switch, not this process's stale copy.
    let path_value = current_path_value();

    // Install check from disk: `npm list -g` costs ~2.3s here, and a missing Node surfaces
    // from the spawn below with a clear message of its own.
    if dsh_package_dir(&path_value).is_none() {
        return Err(format!("[{}] DeepSeek Harness is not installed. Please install it first.", crate::config::ErrorCode::DshNotInstalled.as_str()));
    }

    // The auth patch rewrites DSH's own package, and the code it targets changes between
    // releases (it matched 0.1.5-rc.2 but not 0.1.5-rc.1). Newer builds also authorize the
    // loopback iframe through `--trusted-host`, which needs no file edit at all — so start
    // first and probe what the server actually does, instead of editing on faith.
    let mut child = spawn_dsh_server(target_port, &path_value)?;
    if !wait_for_port(target_port) {
        stop_child(&mut child);
        return Err(format!(
            "DSH 启动超时（{} 秒）: 端口 {} 未监听。请尝试手动运行 `npx @deepseek-ai/dsh web --port {}` 查看错误。",
            DSH_START_TIMEOUT_SECS, target_port, target_port
        ));
    }

    let mut auth_patch_warning: Option<String> = None;
    if wait_for_index_auth(target_port) == IndexAuth::Refused {
        // Explicitly refused, so this build does need the patch. DSH reads its files at
        // process start, hence the restart after patching.
        match patch_dsh_auth() {
            Ok(result) if result.patched => {
                auth_patch_warning = result.warning;
                stop_child(&mut child);
                wait_for_port_release(target_port);
                child = spawn_dsh_server(target_port, &path_value)?;
                if !wait_for_port(target_port) {
                    stop_child(&mut child);
                    return Err(format!(
                        "DSH 启动超时（{} 秒）: 应用认证补丁后端口 {} 仍未监听。",
                        DSH_START_TIMEOUT_SECS, target_port
                    ));
                }
                match wait_for_index_auth(target_port) {
                    IndexAuth::Serving => {}
                    IndexAuth::Refused => {
                        auth_patch_warning = Some(
                            "已应用认证补丁，但服务仍拒绝免认证访问，内嵌页面可能无法加载".into(),
                        );
                    }
                    // Still booting after the patch: say so, but do not claim it failed.
                    IndexAuth::NotReady => {
                        auth_patch_warning = Some(
                            "已应用认证补丁，但服务尚未就绪，页面可能空白，可稍后刷新".into(),
                        );
                    }
                }
            }
            // Targets gone (auth code changed again) or a hard error: a restart would not
            // help, and the server already listening stays usable.
            Ok(DshPatchResult {
                message, warning, ..
            }) => {
                auth_patch_warning = warning.or_else(|| {
                    Some(format!("服务未授权回环访问，且认证补丁不适用：{message}"))
                });
            }
            Err(e) => auth_patch_warning = Some(format!("DSH 认证补丁失败: {e}")),
        }
    }

    let instance = DshInstance {
        pid: 0, // PID no longer tracked meaningfully; port is the source of truth
        port: target_port,
        auth_url: None,
        auth_patch_warning,
    };

    // Remove old instance on same port if any
    {
        let state = window.state::<DshState>();
        let mut instances = state.instances.lock().map_err(|e| e.to_string())?;
        instances.retain(|i| i.port != target_port);
        instances.push(instance.clone());
    }

    Ok(instance)
}

#[tauri::command]
pub async fn start_dsh(port: Option<u16>, window: tauri::Window) -> Result<DshInstance, String> {
    tokio::task::spawn_blocking(move || start_dsh_sync(port, window))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

/// Blocking core of `stop_dsh`: `find_pid_by_port` shells out to netstat and
/// `kill_pid` runs taskkill.
fn stop_dsh_sync(port: u16, window: tauri::Window) -> Result<String, String> {
    let state = window.state::<DshState>();
    let mut instances = state.instances.lock().map_err(|e| e.to_string())?;

    // Try to find in tracked instances first
    let idx = instances.iter().position(|i| i.port == port);
    if let Some(i) = idx {
        instances.remove(i);
        // Best-effort: kill whatever is listening on the port via netstat.
        // The tracked PID is no longer reliable (OS reuse), so we resolve by port.
        if let Some(pid) = find_pid_by_port(port) {
            kill_pid(pid);
        }
        return Ok(format!("Stopped DSH on port {port}"));
    }

    // Fallback: find PID by port via netstat
    if let Some(pid) = find_pid_by_port(port) {
        kill_pid(pid);
        return Ok(format!("Stopped DSH on port {port} (PID: {pid})"));
    }

    Err(format!("No instance running on port {port}"))
}

#[tauri::command]
pub async fn stop_dsh(port: u16, window: tauri::Window) -> Result<String, String> {
    tokio::task::spawn_blocking(move || stop_dsh_sync(port, window))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

#[tauri::command]
pub async fn list_dsh(window: tauri::Window) -> Result<Vec<DshInstance>, String> {
    tokio::task::spawn_blocking(move || {
        let state = window.state::<DshState>();
        let tracked = {
            let mut instances = state.instances.lock().map_err(|e| e.to_string())?;
            prune_dead_instances(&mut instances);
            instances.clone()
        };
        // Nothing tracked yet: DSH may still be serving from before an app restart.
        // Probe outside the lock — it is a network round-trip.
        if !tracked.is_empty() || !dsh_is_serving(DSH_DEFAULT_PORT) {
            return Ok(tracked);
        }
        let mut instances = state.instances.lock().map_err(|e| e.to_string())?;
        adopt_untracked_dsh(&mut instances);
        Ok(instances.clone())
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?
}

/// Check if a port is currently in use (server is listening).
#[tauri::command]
pub async fn check_dsh_port(port: u16) -> bool {
    tokio::task::spawn_blocking(move || is_port_in_use(port))
        .await
        .unwrap_or(false)
}

/// Probe whether DSH HTTP is actually serving (not just TCP bind).
#[tauri::command]
pub async fn check_dsh_http(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(&url).send().await {
        Ok(resp) => resp.status().as_u16() < 500,
        Err(_) => false,
    }
}

/// Check whether Node.js / npx is available on the system.
#[tauri::command]
pub async fn check_nodejs_installed() -> bool {
    tokio::task::spawn_blocking(|| {
        // Use cmd /c where to find npx in PATH
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", "where", "npx"]);
        apply_current_path(&mut cmd);

        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        cmd.output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// Get installed dsh version.
/// Installed DSH version, or `None` when it is not installed. Blocking, so callers must
/// use `spawn_blocking`. Never uses `npx -y`, which would auto-install.
fn installed_dsh_version() -> Option<String> {
    // Fast path: the package's own package.json (~1ms) instead of `npm list -g` (~2.3s).
    let path_value = current_path_value();
    if let Some(pkg) = dsh_package_dir(&path_value) {
        if let Some(version) = read_package_json(&pkg.join("package.json"))
            .and_then(|json| json.get("version")?.as_str().map(str::to_string))
        {
            return Some(version);
        }
    }

    // Fallback for layouts the fast path cannot see.
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "npm", "list", "-g", "@deepseek-ai/dsh", "--depth=0"]);
    apply_path_value(&mut cmd, &path_value);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let output = cmd.output().ok()?;

    // Output format: "@deepseek-ai/dsh@1.2.3", empty when not installed.
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let version = stdout
        .split("@deepseek-ai/dsh@")
        .nth(1)?
        .split_whitespace()
        .next()?
        .trim()
        .to_string();
    (!version.is_empty()).then_some(version)
}

#[tauri::command]
pub async fn get_dsh_version() -> Result<String, String> {
    tokio::task::spawn_blocking(installed_dsh_version)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
        .ok_or_else(|| "Not installed".to_string())
}

/// Newest DeepSeek build the user should be offered: the higher of the `latest` and
/// `next` dist-tags.
///
/// Querying `version` alone was not enough — that returns `latest` only, and DSH
/// publishes some release candidates under `next` first (`latest` = 0.1.5-rc.1 while
/// `next` = 0.1.5-rc.2), so a genuinely newer build produced no update prompt. The
/// alpha channel stays excluded, matching `get_dsh_versions`.
#[tauri::command]
pub async fn get_dsh_latest_version() -> Result<String, String> {
    let output = tokio::task::spawn_blocking(|| {
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", "npm", "view", "@deepseek-ai/dsh", "dist-tags", "--json"]);

        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        cmd.output()
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
    .map_err(|e| format!("Failed to get latest version: {}", e))?;

    if !output.status.success() {
        return Err("Failed to query npm".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let tags: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse dist-tags: {}", e))?;

    let mut best: Option<String> = None;
    for tag in ["latest", "next"] {
        let Some(version) = tags.get(tag).and_then(|v| v.as_str()) else {
            continue;
        };
        if version.contains("alpha") {
            continue;
        }
        best = match best {
            Some(current)
                if !crate::runtime_commands::compare_versions(version, &current).is_gt() =>
            {
                Some(current)
            }
            _ => Some(version.to_string()),
        };
    }

    best.ok_or_else(|| "Could not determine latest version".to_string())
}

/// Get all available dsh versions from npm.
#[tauri::command]
pub async fn get_dsh_versions() -> Result<Vec<String>, String> {
    let output = tokio::task::spawn_blocking(|| {
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", "npm", "view", "@deepseek-ai/dsh", "versions", "--json"]);

        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        cmd.output()
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
    .map_err(|e| format!("Failed to get versions: {}", e))?;

    if !output.status.success() {
        return Err("Failed to query npm versions".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let all_versions: Vec<String> = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse versions: {}", e))?;

    // Filter out alpha versions
    let versions: Vec<String> = all_versions.into_iter()
        .filter(|v| !v.contains("alpha"))
        .collect();

    Ok(versions)
}

/// Update DSH to the version the UI offered, streaming progress.
#[tauri::command]
pub async fn update_dsh(
    version: Option<String>,
    window: tauri::Window,
) -> Result<String, String> {
    use tauri::Emitter;

    // Install the exact offered version instead of the `latest` tag: DSH publishes some
    // release candidates under `next` first (`latest` = 0.1.5-rc.1 while `next` =
    // 0.1.5-rc.2). With `@latest` npm resolved to the already-installed version, exited 0
    // with "up to date", and the workbench reported an update that never happened.
    let requested = version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let target = match &requested {
        Some(v) => format!("@deepseek-ai/dsh@{v}"),
        None => "@deepseek-ai/dsh@latest".to_string(),
    };

    window.emit("dsh:update_progress", serde_json::json!({
        "stage": "downloading",
        "percent": 10,
        "message": format!("正在下载 {target}...")
    })).ok();

    let window_clone = window.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new("cmd");
        cmd.args(["/c", "npm", "install", "-g", &target]);
        apply_current_path(&mut cmd);

        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        cmd.stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| format!("Failed to start npm: {}", e))?;

        let stderr = child.stderr.take().unwrap();
        let reader = BufReader::new(stderr);
        let mut last_msg = String::new();

        for line in reader.lines() {
            if let Ok(line) = line {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() { continue; }

                let (stage, percent, msg) = if trimmed.contains("added") || trimmed.contains("updated") {
                    ("installing", 70, format!("正在安装: {}", trimmed))
                } else if trimmed.contains("fetch") || trimmed.contains("http") {
                    ("downloading", 30, format!("下载中: {}", trimmed))
                } else if trimmed.contains("extract") || trimmed.contains("tar") {
                    ("extracting", 50, format!("解压中: {}", trimmed))
                } else if trimmed.contains("link") {
                    ("linking", 80, format!("链接中: {}", trimmed))
                } else {
                    ("progress", 40, trimmed.clone())
                };

                if trimmed != last_msg {
                    last_msg = trimmed;
                    let _ = window_clone.emit("dsh:update_progress", serde_json::json!({
                        "stage": stage,
                        "percent": percent,
                        "message": msg
                    }));
                }
            }
        }

        child.wait().map_err(|e| format!("Wait failed: {}", e))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
    .map_err(|e| format!("Update failed: {}", e))?;

    if result.success() {
        // Apply auth bypass patch after update
        let _ = patch_dsh_auth();
        // npm exits 0 even when it changed nothing, so report the version that is really
        // installed: a silent no-op must not read as a successful update.
        let installed = tokio::task::spawn_blocking(installed_dsh_version)
            .await
            .ok()
            .flatten();
        let message = match (installed, requested.as_deref()) {
            (Some(now), Some(want)) if now != want => {
                format!("安装命令已完成，但当前版本仍为 {now}（期望 {want}）")
            }
            (Some(now), _) => format!("已更新到 {now}"),
            (None, _) => "更新成功".to_string(),
        };
        window.emit("dsh:update_progress", serde_json::json!({
            "stage": "done",
            "percent": 100,
            "message": message.clone()
        })).ok();
        Ok(message)
    } else {
        Err("更新失败，请检查网络连接".to_string())
    }
}

/// Sync a model config from our app to DeepSeek Harness settings.
#[tauri::command]
pub fn sync_model_to_dsh(
    name: String,
    provider: String,
    api_key: String,
    base_url: String,
    model: String,
    max_tokens: u32,
) -> Result<String, String> {
    let dsh_home = std::env::var("USERPROFILE")
        .map(|p| PathBuf::from(p).join(".dsh"))
        .map_err(|e| format!("Cannot find DSH home: {}", e))?;

    fs::create_dir_all(&dsh_home)
        .map_err(|e| format!("Cannot create ~/.dsh: {}", e))?;

    let settings_path = dsh_home.join("settings.yaml");
    let creds_path = dsh_home.join(".credentials.yaml");

    // --- Read settings.yaml ---
    let settings_str = fs::read_to_string(&settings_path)
        .unwrap_or_else(|_| "llm-pi-ai:\n  providers: {}\n".to_string());
    let mut settings: serde_yaml::Value = serde_yaml::from_str(&settings_str)
        .map_err(|e| format!("Parse settings.yaml failed: {}", e))?;

    // Ensure llm-pi-ai.providers exists
    if settings["llm-pi-ai"].is_null() {
        settings["llm-pi-ai"] = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    if settings["llm-pi-ai"]["providers"].is_null() {
        settings["llm-pi-ai"]["providers"] = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }

    // Unique route per config name so syncing one model does not wipe siblings
    let name_part = sanitize_route_part(&name);
    let route = if name_part.is_empty() {
        provider.clone()
    } else {
        format!("{}_{}", sanitize_route_part(&provider), name_part)
    };
    let cred_ref = format!(
        "{}_API_KEY",
        route.to_uppercase().replace(|c: char| !c.is_alphanumeric(), "_")
    );

    // Map provider to DSH API protocol
    let api_protocol = match provider.as_str() {
        "anthropic" => "anthropic-messages",
        _ => "openai-completions",
    };

    // Build provider entry
    let mut provider_map = serde_yaml::Mapping::new();
    provider_map.insert(
        serde_yaml::Value::String("displayName".into()),
        serde_yaml::Value::String(name.clone()),
    );
    provider_map.insert(
        serde_yaml::Value::String("apiKeyEnv".into()),
        serde_yaml::Value::String(cred_ref.clone()),
    );
    provider_map.insert(
        serde_yaml::Value::String("api".into()),
        serde_yaml::Value::String(api_protocol.into()),
    );
    provider_map.insert(
        serde_yaml::Value::String("baseURL".into()),
        serde_yaml::Value::String(base_url),
    );

    // Build model entry
    let mut model_map = serde_yaml::Mapping::new();
    model_map.insert(
        serde_yaml::Value::String("id".into()),
        serde_yaml::Value::String(model.clone()),
    );
    if max_tokens > 0 {
        model_map.insert(
            serde_yaml::Value::String("maxTokens".into()),
            serde_yaml::Value::Number(max_tokens.into()),
        );
    }
    let models = serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(model_map)]);
    provider_map.insert(
        serde_yaml::Value::String("models".into()),
        models,
    );

    // Insert provider into settings
    settings["llm-pi-ai"]["providers"][&route] = serde_yaml::Value::Mapping(provider_map);

    // Write settings.yaml
    let new_settings = serde_yaml::to_string(&settings)
        .map_err(|e| format!("Serialize settings failed: {}", e))?;
    fs::write(&settings_path, new_settings)
        .map_err(|e| format!("Write settings.yaml failed: {}", e))?;

    // --- Read .credentials.yaml ---
    let creds_str = fs::read_to_string(&creds_path)
        .unwrap_or_else(|_| "version: 1\nrecords: {}\nrefs: {}\n".to_string());
    let mut creds: serde_yaml::Value = serde_yaml::from_str(&creds_str)
        .map_err(|e| format!("Parse .credentials.yaml failed: {}", e))?;

    // Ensure refs exists
    if creds["refs"].is_null() {
        creds["refs"] = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }

    // Set API key
    creds["refs"][&cred_ref] = serde_yaml::Value::String(api_key);

    // Write .credentials.yaml
    let new_creds = serde_yaml::to_string(&creds)
        .map_err(|e| format!("Serialize credentials failed: {}", e))?;
    fs::write(&creds_path, new_creds)
        .map_err(|e| format!("Write .credentials.yaml failed: {}", e))?;

    Ok(format!(
        "已同步 {} 到 DeepSeek（已写入 ~/.dsh settings 与 credentials）",
        model
    ))
}
