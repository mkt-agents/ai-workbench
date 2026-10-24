use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::io::{BufRead, BufReader, Read, Write};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::Manager;

/// `about:blank` and the local DSH origin stay inside a WebView2 popup so
/// sign-in can script the window it just opened. Anything else is an
/// account-platform link and belongs in the system browser.
fn dsh_popup_stays_in_webview(url: &url::Url) -> bool {
    match url.scheme() {
        "http" | "https" => matches!(
            url.host_str(),
            Some("127.0.0.1") | Some("localhost") | Some("::1")
        ),
        _ => true,
    }
}

fn open_dsh_external(url: &str) -> Result<(), String> {
    crate::cloudflared_commands::open_url(url)
}

/// The shell plugin listens for `target=_blank` clicks and calls `shell.open`,
/// which on Windows does not follow the browser chosen in Settings. Stopping
/// the event before that listener lets this webview's new-window handler launch
/// the registered browser instead.
const DSH_EXTERNAL_LINK_SCRIPT: &str = r#"(function () {
  document.addEventListener("click", function (event) {
    var el = event.target;
    while (el && el.tagName !== "A") el = el.parentElement;
    if (!el) return;
    if (String(el.getAttribute("target") || "").toLowerCase() !== "_blank") return;
    var href = el.href || "";
    if (href.indexOf("http://") !== 0 && href.indexOf("https://") !== 0) return;
    event.stopPropagation();
  }, true);
})();
"#;

/// Child webview that loads the launch-token URL as its own top-level page.
/// An iframe cannot: DSH sets a SameSite=Strict cookie, which a cross-site
/// parent (the workbench origin) is not allowed to send back.
const DSH_EMBED_LABEL: &str = "dsh-embed";
static DSH_EMBED_URL: Mutex<Option<String>> = Mutex::new(None);

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
use crate::port_commands::{find_pid_by_port, is_port_in_use, kill_pid, wait_for_port_release};

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

/// Kept so an old install can still be recognised. Startup no longer calls this:
/// DSH 0.2 exchanges a launch token for a cookie instead of this file rewrite.
#[allow(dead_code)]
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

/// Drop a patch left by an older build of this app. Current startups never write one.
/// Missing install or a pristine file is success.
fn restore_leftover_patch() -> Result<(), String> {
    let target = match find_dsh_connection_index() {
        Ok(path) => path,
        Err(_) => return Ok(()),
    };
    let content = fs::read_to_string(&target).unwrap_or_default();
    if !content.contains(DSH_PATCH_SENTINEL) && !dsh_backup_path(&target).exists() {
        return Ok(());
    }
    restore_dsh_auth_sync().map(|_| ())
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
    /// Trusted, but this process still wants its launch token (DSH 0.2+).
    LoginRequired,
    /// Host/Origin fence rejected the request. A token cannot fix this.
    Forbidden,
    /// Not answering yet. DSH serves `404` for roughly the first 1.4s after the port
    /// opens, and that must not be read as a refusal.
    NotReady,
}

/// `dsh web:` lines carry the process launch token. Other `dsh web:` lines are status text.
fn parse_dsh_web_line(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("dsh web:")?.trim();
    if rest.starts_with("http://") || rest.starts_with("https://") {
        Some(rest.to_string())
    } else {
        None
    }
}

/// Plain `GET /` is DSH's login wall (no cookie, no token). The HTML marker is absent
/// on that response, so a port check alone cannot tell this apart from a foreign program.
fn dsh_needs_login(port: u16) -> bool {
    dsh_plain_get(port, 4096)
        .map(|body| {
            let lower = body.to_ascii_lowercase();
            lower.contains("dsh web authentication required")
                || (lower.contains(" 401 ") && lower.contains("@deepseek-ai/dsh"))
        })
        .unwrap_or(false)
}

fn probe_index_auth(port: u16) -> IndexAuth {
    match dsh_index_status(port) {
        Some(status) if (200..400).contains(&status) => IndexAuth::Serving,
        // DSH answers 401 when its authorization refuses the request.
        Some(401) => IndexAuth::LoginRequired,
        Some(403) => IndexAuth::Forbidden,
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
    apply_path_value(cmd, path_value);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
}

struct SpawnedDsh {
    child: std::process::Child,
    /// First `dsh web: http…` line. The reader keeps draining stdout afterwards.
    auth_url: mpsc::Receiver<String>,
}

fn watch_launch_url(child: &mut std::process::Child) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let Some(stdout) = child.stdout.take() else {
        return rx;
    };
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if let Some(url) = parse_dsh_web_line(&line) {
                let _ = tx.send(url);
            }
        }
    });
    rx
}

/// Spawn the DSH web server. `--no-open` keeps it from launching a browser.
/// `--trusted-host` passes the Host/Origin fence; identity is the launch token
/// printed on stdout (`dsh web: http://127.0.0.1:<port>/?token=…`).
fn spawn_dsh_server(port: u16, path_value: &str) -> Result<SpawnedDsh, String> {
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
                Ok(mut child) => {
                    return Ok(SpawnedDsh {
                        auth_url: watch_launch_url(&mut child),
                        child,
                    });
                }
                Err(e) => previous = Some(format!("node 直接启动失败: {e}")),
            }
        }
    }

    // Fallback: npx, which also covers package layouts we do not recognise.
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "npx", "@deepseek-ai/dsh"]).args(&args);
    configure_dsh_command(&mut cmd, path_value);
    let mut child = cmd.spawn().map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::NotFound {
            "未找到 Node.js，请先安装，或在「版本切换」里选择可用的 Node"
        } else {
            "启动 DSH 失败"
        };
        match &previous {
            Some(prev) => format!("{hint}: {e}（{prev}）"),
            None => format!("{hint}: {e}"),
        }
    })?;
    Ok(SpawnedDsh {
        auth_url: watch_launch_url(&mut child),
        child,
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
    let _ = kill_pid(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// True when neither the TCP port nor a DSH HTTP response remains.
fn dsh_fully_stopped(port: u16) -> bool {
    !is_port_in_use(port) && !dsh_is_serving(port)
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

/// Drop tracked instances whose port is no longer listening, and reap any
/// owned Child handles for those ports so we do not leak process handles.
fn prune_dead_instances(state: &DshState) -> Result<(), String> {
    let mut instances = state.instances.lock().map_err(|e| e.to_string())?;
    instances.retain(|i| is_instance_alive(i.port));
    let alive: std::collections::HashSet<u16> =
        instances.iter().map(|i| i.port).collect();
    drop(instances);

    let mut children = state.children.lock().map_err(|e| e.to_string())?;
    let stale: Vec<u16> = children
        .keys()
        .copied()
        .filter(|p| !alive.contains(p))
        .collect();
    for port in stale {
        if let Some(mut child) = children.remove(&port) {
            let _ = child.try_wait();
        }
    }
    Ok(())
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

        // `map_while`：读出错就收摊，别让 `flatten` 在坏管道上无限转
        for line in reader.lines().map_while(Result::ok) {
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

        child.wait().map_err(|e| format!("Wait failed: {}", e))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
    .map_err(|e| format!("Install failed: {}", e))?;

    if result.success() {
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
    let existing = {
        let state = window.state::<DshState>();
        prune_dead_instances(&state)?;
        let instances = state.instances.lock().map_err(|e| e.to_string())?;
        instances.iter().find(|i| i.port == target_port).cloned()
    };
    // A tracked server that already handed us a launch URL can be reused. One that is
    // only sitting on the login wall has to be replaced so we can read a fresh token.
    if let Some(existing) = existing {
        if existing.auth_url.is_some() || !dsh_needs_login(target_port) {
            return Ok(existing);
        }
    }
    if is_port_in_use(target_port) {
        // Occupied yet untracked: `DshState` is in-memory, so after an app restart an
        // already-running DSH looks "not started" while its port stays taken.
        if dsh_is_serving(target_port) && !dsh_needs_login(target_port) {
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
        if dsh_needs_login(target_port) {
            // The process is up but this app never saw its launch token (stdout was
            // discarded, or it was started outside the app). Restart it so the new
            // process prints a token we can log in with. The signing secret is durable,
            // so a browser that already completed login keeps its cookie.
            match stop_dsh_sync(target_port, window.clone()) {
                Ok(_) => {}
                Err(_) if !is_port_in_use(target_port) => {}
                Err(e) => return Err(e),
            }
            if is_port_in_use(target_port) {
                return Err(format!(
                    "端口 {target_port} 上的 DeepSeek 仍在运行，无法重新启动以获取登录地址{}",
                    port_owner_description(target_port)
                ));
            }
        } else {
            return Err(format!(
                "端口 {} 已被其他程序占用，且探测发现它不是 DeepSeek 服务{}。请关闭该程序后重试。",
                target_port,
                port_owner_description(target_port)
            ));
        }
    }

    // One registry read for the whole start: every child below must see the PATH
    // written by the most recent runtime switch, not this process's stale copy.
    let path_value = current_path_value();

    // Install check from disk: `npm list -g` costs ~2.3s here, and a missing Node surfaces
    // from the spawn below with a clear message of its own.
    if dsh_package_dir(&path_value).is_none() {
        return Err(format!("[{}] DeepSeek Harness is not installed. Please install it first.", crate::config::ErrorCode::DshNotInstalled.as_str()));
    }

    // Do not rewrite DSH's install. 0.2+ mints a launch token and exchanges it for a
    // signed cookie. If an older build left its auth patch on disk, put the original
    // file back before this process reads it.
    restore_leftover_patch()?;
    let mut launch = spawn_dsh_server(target_port, &path_value)?;
    if !wait_for_port(target_port) {
        stop_child(&mut launch.child);
        return Err(format!(
            "DSH 启动超时（{} 秒）: 端口 {} 未监听。请尝试手动运行 `npx @deepseek-ai/dsh web --port {}` 查看错误。",
            DSH_START_TIMEOUT_SECS, target_port, target_port
        ));
    }

    let auth_url = launch
        .auth_url
        .recv_timeout(Duration::from_secs(8))
        .ok();
    let auth_patch_warning = match (wait_for_index_auth(target_port), auth_url.is_some()) {
        (IndexAuth::Serving, _) => None,
        (IndexAuth::LoginRequired | IndexAuth::NotReady, true) => None,
        (IndexAuth::LoginRequired, false) => Some(
            "服务要求授权登录，但没有读到启动时打印的登录地址。可点「外开浏览器」手动打开 dsh 打印的地址。"
                .into(),
        ),
        (IndexAuth::Forbidden, _) => Some(
            "服务拒绝了回环地址（403）。请确认本机 DSH 接受 --trusted-host 127.0.0.1。".into(),
        ),
        (IndexAuth::NotReady, false) => {
            Some("服务尚未就绪，页面可能空白，可稍后刷新。".into())
        }
    };

    let child = launch.child;
    let spawned_pid = child.id();
    let instance = DshInstance {
        pid: spawned_pid,
        port: target_port,
        auth_url,
        auth_patch_warning,
    };

    // Remove old instance on same port if any; retain the Child so stop can kill
    // the process tree without relying solely on netstat.
    {
        let state = window.state::<DshState>();
        let mut instances = state.instances.lock().map_err(|e| e.to_string())?;
        instances.retain(|i| i.port != target_port);
        instances.push(instance.clone());
        let mut children = state.children.lock().map_err(|e| e.to_string())?;
        if let Some(mut old) = children.insert(target_port, child) {
            let _ = old.try_wait();
        }
    }

    Ok(instance)
}

#[tauri::command]
pub async fn start_dsh(port: Option<u16>, window: tauri::Window) -> Result<DshInstance, String> {
    tokio::task::spawn_blocking(move || start_dsh_sync(port, window))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

/// Blocking core of `stop_dsh`: kill the owned Child (if any), then fall back to
/// netstat → taskkill by port. Success requires the port to be free and DSH HTTP
/// to stop answering — otherwise refresh would re-adopt a surviving server.
fn close_dsh_embed(app: &tauri::AppHandle) {
    if let Some(webview) = app.get_webview(DSH_EMBED_LABEL) {
        let _ = webview.close();
    }
    if let Ok(mut slot) = DSH_EMBED_URL.lock() {
        *slot = None;
    }
}

fn stop_dsh_sync(port: u16, window: tauri::Window) -> Result<String, String> {
    close_dsh_embed(window.app_handle());
    let state = window.state::<DshState>();

    let mut owned_child = {
        let mut children = state.children.lock().map_err(|e| e.to_string())?;
        children.remove(&port)
    };

    let was_tracked = {
        let mut instances = state.instances.lock().map_err(|e| e.to_string())?;
        if let Some(idx) = instances.iter().position(|i| i.port == port) {
            instances.remove(idx);
            true
        } else {
            false
        }
    };

    let port_pid = find_pid_by_port(port);
    if owned_child.is_none() && !was_tracked && port_pid.is_none() && dsh_fully_stopped(port) {
        return Err(format!("No instance running on port {port}"));
    }
    // Listener or HTTP still up but not tracked — keep going and kill by port.

    let mut kill_errors: Vec<String> = Vec::new();
    let owned_pid = owned_child.as_ref().map(|c| c.id());

    if let Some(ref mut child) = owned_child {
        if let Err(e) = kill_pid(child.id()) {
            kill_errors.push(e);
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    // Port may still be held by a grandchild (e.g. `cmd /c npx` → node) if the
    // tree kill missed, or by an orphan adopted after app restart.
    if let Some(pid) = find_pid_by_port(port) {
        if owned_pid != Some(pid) {
            if let Err(e) = kill_pid(pid) {
                kill_errors.push(e);
            }
        }
    }

    if !wait_for_port_release(port) || dsh_is_serving(port) {
        let detail = if kill_errors.is_empty() {
            port_owner_description(port)
        } else {
            format!("（{}）{}", kill_errors.join("; "), port_owner_description(port))
        };
        return Err(format!(
            "停止失败：端口 {port} 上的 DeepSeek 服务仍在运行{detail}"
        ));
    }

    Ok(format!("Stopped DSH on port {port}"))
}

/// Show, move, or hide the authorized Harness page. The URL is the launch-token
/// address from `start_dsh`; resizing sends the same URL again and must not reload.
#[tauri::command]
pub async fn dsh_set_embed(
    app: tauri::AppHandle,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    visible: bool,
) -> Result<(), String> {
    let show = visible && width > 1.0 && height > 1.0 && !url.is_empty();
    if !show {
        if let Some(webview) = app.get_webview(DSH_EMBED_LABEL) {
            webview.hide().map_err(|e| e.to_string())?;
        }
        return Ok(());
    }

    let parsed = url
        .parse::<url::Url>()
        .map_err(|e| format!("登录地址无效: {e}"))?;
    if let Some(webview) = app.get_webview(DSH_EMBED_LABEL) {
        webview
            .set_position(tauri::LogicalPosition::new(x, y))
            .map_err(|e| e.to_string())?;
        webview
            .set_size(tauri::LogicalSize::new(width, height))
            .map_err(|e| e.to_string())?;
        webview.show().map_err(|e| e.to_string())?;
        let changed = DSH_EMBED_URL.lock().ok().and_then(|slot| slot.clone()) != Some(url.clone());
        if changed {
            webview.navigate(parsed).map_err(|e| e.to_string())?;
            if let Ok(mut slot) = DSH_EMBED_URL.lock() {
                *slot = Some(url);
            }
        }
        return Ok(());
    }

    let window = app.get_window("main").ok_or("主窗口不存在")?;
    let builder = tauri::webview::WebviewBuilder::new(
        DSH_EMBED_LABEL,
        tauri::WebviewUrl::External(parsed),
    )
    .focused(false)
    .background_color(tauri::webview::Color(15, 17, 19, 255))
    .initialization_script(DSH_EXTERNAL_LINK_SCRIPT)
    // Sign-in does window.open("about:blank") and then location.replace.
    // WebView2 drops that open unless a handler allows it, so the login UI
    // only shows the copy-link dialog. Account links (usage, top-up, the
    // platform home) are target=_blank to an external host. The default
    // popup from this child webview never appears, so those clicks do
    // nothing — send them to the system browser instead.
    .on_new_window(|url, _features| {
        if dsh_popup_stays_in_webview(&url) {
            tauri::webview::NewWindowResponse::Allow
        } else {
            let _ = open_dsh_external(url.as_str());
            tauri::webview::NewWindowResponse::Deny
        }
    });
    window
        .add_child(
            builder,
            tauri::LogicalPosition::new(x, y),
            tauri::LogicalSize::new(width, height),
        )
        .map_err(|e| format!("无法嵌入登录页: {e}"))?;
    if let Ok(mut slot) = DSH_EMBED_URL.lock() {
        *slot = Some(url);
    }
    Ok(())
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
        prune_dead_instances(&state)?;
        let tracked = {
            let instances = state.instances.lock().map_err(|e| e.to_string())?;
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

/// Newest DeepSeek build the user should be offered: the `latest` dist-tag only.
///
/// `next` is deliberately excluded: it can point at a release candidate whose
/// dependency packages are not yet published (e.g. 0.2.1-rc.3 shipped without
/// dsh-deepseek-llm-api-extensions@0.2.1-rc.3), so an upgrade to it always fails
/// with ETARGET. The alpha channel stays excluded, matching `get_dsh_versions`.
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
    if let Some(version) = tags.get("latest").and_then(|v| v.as_str()) {
        if !version.contains("alpha") {
            best = Some(version.to_string());
        }
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

    // Install the exact offered version instead of the `latest` tag: with `@latest` npm
    // resolved to the already-installed version, exited 0 with "up to date", and the
    // workbench reported an update that never happened.
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
        // Keep npm's own error lines so a non-zero exit can report the real cause
        // (e.g. ETARGET when upstream published an incomplete release) instead of a
        // generic network hint.
        let mut npm_errors: Vec<String> = Vec::new();

        for line in reader.lines().map_while(Result::ok) {
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() { continue; }

            if trimmed.starts_with("npm error") && npm_errors.len() < 5 {
                npm_errors.push(trimmed.clone());
            }

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

        child
            .wait()
            .map(|status| (status, npm_errors))
            .map_err(|e| format!("Wait failed: {}", e))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
    .map_err(|e| format!("Update failed: {}", e))?;
    let (result, npm_errors) = result;

    if result.success() {
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
        let code = result.code().unwrap_or(-1);
        if npm_errors.is_empty() {
            Err(format!("更新失败（npm 退出码 {code}），请检查网络连接"))
        } else {
            Err(format!(
                "更新失败（npm 退出码 {code}）：{}",
                npm_errors.join(" | ")
            ))
        }
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

#[cfg(test)]
mod tests {
    use super::{dsh_popup_stays_in_webview, parse_dsh_web_line};

    #[test]
    fn login_popup_stays_in_the_embed() {
        let blank = url::Url::parse("about:blank").unwrap();
        let local = url::Url::parse("http://127.0.0.1:3080/").unwrap();
        assert!(dsh_popup_stays_in_webview(&blank));
        assert!(dsh_popup_stays_in_webview(&local));
    }

    #[test]
    fn account_links_leave_the_embed() {
        for raw in [
            "https://platform.deepseek.com/usage",
            "https://platform.deepseek.com/",
            "https://platform.deepseek.com/top_up",
        ] {
            let parsed = url::Url::parse(raw).unwrap();
            assert!(!dsh_popup_stays_in_webview(&parsed), "{raw}");
        }
    }

    #[test]
    fn reads_the_launch_token_url() {
        assert_eq!(
            parse_dsh_web_line("dsh web: http://127.0.0.1:3080/?token=abc"),
            Some("http://127.0.0.1:3080/?token=abc".into())
        );
    }

    #[test]
    fn ignores_status_lines() {
        assert_eq!(
            parse_dsh_web_line("dsh web: opening the default browser; pass --no-open to disable"),
            None
        );
        assert_eq!(parse_dsh_web_line("npm warn deprecated"), None);
    }
}
