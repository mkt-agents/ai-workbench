use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;

use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const DOWNLOAD_URL: &str =
    "https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/";
const BIN_PATH_FILE: &str = "cloudflared_bin_path.txt";

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudflaredStatus {
    pub installed: bool,
    pub version: String,
    pub path: String,
    pub message: String,
    pub custom_path: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatus {
    pub id: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub local_url: Option<String>,
    pub public_url: Option<String>,
    pub mode: Option<String>,
    pub profile_id: Option<String>,
}

struct TunnelSession {
    id: String,
    pid: u32,
    mode: String,
    local_url: String,
    public_url: Option<String>,
    profile_id: Option<String>,
    child: Child,
}

pub struct CloudflaredState {
    sessions: Mutex<HashMap<String, TunnelSession>>,
}

impl Default for CloudflaredState {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

fn hidden_cmd(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn app_data_dir() -> Result<PathBuf, String> {
    let appdata =
        std::env::var("APPDATA").map_err(|_| "Failed to resolve APPDATA".to_string())?;
    let dir = Path::new(&appdata).join("com.ai-workbench.app");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create app data dir: {}", e))?;
    Ok(dir)
}

fn bin_path_file() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join(BIN_PATH_FILE))
}

fn read_custom_bin_path() -> Option<PathBuf> {
    let file = bin_path_file().ok()?;
    let content = fs::read_to_string(&file).ok()?;
    let path = PathBuf::from(content.trim());
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

fn write_custom_bin_path(path: &Path) -> Result<(), String> {
    let file = bin_path_file()?;
    fs::write(&file, path.to_string_lossy().as_bytes())
        .map_err(|e| format!("保存 cloudflared 路径失败: {}", e))
}

fn clear_custom_bin_path() -> Result<(), String> {
    let file = bin_path_file()?;
    if file.exists() {
        fs::remove_file(&file).map_err(|e| format!("清除路径失败: {}", e))?;
    }
    Ok(())
}

fn validate_bin_path(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err("所选路径不是文件或不存在".to_string());
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    #[cfg(target_os = "windows")]
    {
        if name != "cloudflared.exe" && name != "cloudflared" {
            return Err("请选择 cloudflared.exe".to_string());
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if name != "cloudflared" {
            return Err("请选择名为 cloudflared 的可执行文件".to_string());
        }
    }
    cloudflared_version_at(&path.to_string_lossy())?;
    Ok(())
}

fn resolve_cloudflared() -> Result<(String, String), String> {
    if let Some(custom) = read_custom_bin_path() {
        let path = custom.to_string_lossy().to_string();
        match cloudflared_version_at(&path) {
            Ok(ver) => return Ok((path, ver)),
            Err(e) => {
                // Stale custom path — fall through to PATH, but keep file for UI clear
                let _ = e;
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let output = hidden_cmd("cmd")
            .args(["/c", "where", "cloudflared"])
            .output()
            .map_err(|e| format!("查找 cloudflared 失败: {}", e))?;
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                let ver = cloudflared_version_at(&path).unwrap_or_default();
                return Ok((path, ver));
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let output = hidden_cmd("which")
            .arg("cloudflared")
            .output()
            .map_err(|e| format!("查找 cloudflared 失败: {}", e))?;
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                let ver = cloudflared_version_at(&path).unwrap_or_default();
                return Ok((path, ver));
            }
        }
    }

    // Fallback: try bare name
    let ver = cloudflared_version_at("cloudflared");
    if let Ok(v) = ver {
        return Ok(("cloudflared".to_string(), v));
    }

    Err("未找到 cloudflared。可用 winget 安装，或点击「选择程序」指定已下载的 exe。".to_string())
}

fn cloudflared_version_at(bin: &str) -> Result<String, String> {
    let output = hidden_cmd(bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("执行 cloudflared --version 失败: {}", e))?;
    if !output.status.success() {
        return Err("cloudflared --version 失败".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        let err = String::from_utf8_lossy(&output.stderr);
        let line = err.lines().next().unwrap_or("").trim();
        if line.is_empty() {
            return Err("无法解析版本".to_string());
        }
        return Ok(line.to_string());
    }
    Ok(line.to_string())
}

fn extract_trycloudflare_url(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let start = lower.find("https://")?;
    let rest_lower = &lower[start..];
    let end_rel = rest_lower.find(".trycloudflare.com")?;
    let end = start + end_rel + ".trycloudflare.com".len();
    let candidate = &line[start..end];
    let host = candidate.strip_prefix("https://")?;
    if host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn normalize_local_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("本地 URL 不能为空".to_string());
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.chars().all(|c| c.is_ascii_digit()) {
        format!("http://localhost:{}", trimmed)
    } else {
        format!("http://{}", trimmed)
    };
    let parsed = url::Url::parse(&with_scheme).map_err(|e| format!("无效 URL: {}", e))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("仅支持 http/https".to_string());
    }
    Ok(with_scheme)
}

fn kill_pid(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        let _ = hidden_cmd("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).output();
    }
}

fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        hidden_cmd("cmd")
            .args(["/c", "start", "", url])
            .spawn()
            .map_err(|e| format!("打开链接失败: {}", e))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("打开链接失败: {}", e))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("打开链接失败: {}", e))?;
        Ok(())
    }
}

/// Open a public tunnel URL in the default browser. http(s)-only so the IPC
/// entry point can never be used to launch arbitrary programs.
#[tauri::command]
pub fn open_in_browser(url: String) -> Result<(), String> {
    let trimmed = url.trim();
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err("仅支持打开 http(s) 链接".to_string());
    }
    open_url(trimmed)
}

#[tauri::command]
pub async fn cloudflared_status() -> Result<CloudflaredStatus, String> {
    tokio::task::spawn_blocking(cloudflared_status_sync)
        .await
        .map_err(|e| format!("Task failed: {e}"))
}

fn cloudflared_status_sync() -> CloudflaredStatus {
    let custom = read_custom_bin_path().is_some();
    match resolve_cloudflared() {
        Ok((path, version)) => {
            let using_custom = custom
                && read_custom_bin_path()
                    .map(|p| p.to_string_lossy() == path)
                    .unwrap_or(false);
            CloudflaredStatus {
                installed: true,
                version,
                path,
                message: if using_custom {
                    "已安装（自定义路径）".to_string()
                } else {
                    "已安装".to_string()
                },
                custom_path: using_custom,
            }
        }
        Err(message) => CloudflaredStatus {
            installed: false,
            version: String::new(),
            path: read_custom_bin_path()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            message,
            custom_path: custom,
        },
    }
}

#[tauri::command]
pub async fn cloudflared_pick_binary() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(cloudflared_pick_binary_sync)
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

fn cloudflared_pick_binary_sync() -> Result<Option<String>, String> {
    let mut dialog = rfd::FileDialog::new().set_title("选择 cloudflared 可执行文件");
    #[cfg(target_os = "windows")]
    {
        dialog = dialog.add_filter("cloudflared", &["exe"]);
    }
    let Some(path) = dialog.pick_file() else {
        return Ok(None);
    };
    validate_bin_path(&path)?;
    write_custom_bin_path(&path)?;
    Ok(Some(path.to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn cloudflared_set_binary_path(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || cloudflared_set_binary_path_sync(path))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

fn cloudflared_set_binary_path_sync(path: String) -> Result<String, String> {
    let p = PathBuf::from(path.trim());
    validate_bin_path(&p)?;
    write_custom_bin_path(&p)?;
    let ver = cloudflared_version_at(&p.to_string_lossy())?;
    Ok(format!("已设置: {} ({})", p.display(), ver))
}

#[tauri::command]
pub fn cloudflared_clear_binary_path() -> Result<String, String> {
    clear_custom_bin_path()?;
    Ok("已清除自定义路径，将使用 PATH 中的 cloudflared".to_string())
}

#[tauri::command]
pub async fn cloudflared_install() -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let result = tokio::task::spawn_blocking(|| {
            let output = hidden_cmd("cmd")
                .args([
                    "/c",
                    "winget",
                    "install",
                    "--id",
                    "Cloudflare.cloudflared",
                    "-e",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ])
                .output()
                .map_err(|e| format!("启动 winget 失败: {}", e))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success()
                || stdout.to_ascii_lowercase().contains("already installed")
                || stderr.to_ascii_lowercase().contains("already installed")
            {
                Ok("安装完成，请点击刷新检测版本".to_string())
            } else {
                Err(format!(
                    "winget 安装失败。stdout: {} stderr: {}",
                    stdout.trim(),
                    stderr.trim()
                ))
            }
        })
        .await
        .map_err(|e| format!("安装任务失败: {}", e))??;
        Ok(result)
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("当前系统请通过官方文档安装 cloudflared，然后点刷新".to_string())
    }
}

fn normalize_hostname(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let host = trimmed
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    if host.is_empty() || !host.contains('.') {
        return Err("请填写有效的二级域名，例如 dev.example.com".to_string());
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return Err("域名包含非法字符".to_string());
    }
    Ok(host)
}

fn public_url_from_hostname(hostname: &str) -> String {
    format!("https://{}", hostname)
}

fn quick_session_id(local_url: &str) -> String {
    format!("quick:{}", local_url)
}

/// Write a minimal cloudflared config file that embeds the tunnel token,
/// so the token never appears on the command line. The file is created in
/// the app data directory (user-only access) with a randomized name.
fn write_token_config(tok: &str) -> Result<PathBuf, String> {
    let dir = app_data_dir()?;
    let rnd: u64 = rand::random();
    let path = dir.join(format!("cloudflared-token-{:016x}.yml", rnd));
    let content = format!(
        "tunnel: token-authed\ntoken: {}\ncredentials-file: {}\n",
        tok,
        dir.join("cloudflared-creds.json").to_string_lossy()
    );
    fs::write(&path, content).map_err(|e| format!("写入 token 配置失败: {e}"))?;
    Ok(path)
}

fn session_to_status(session: &TunnelSession) -> TunnelStatus {
    TunnelStatus {
        id: session.id.clone(),
        running: true,
        pid: Some(session.pid),
        local_url: if session.local_url.is_empty() {
            None
        } else {
            Some(session.local_url.clone())
        },
        public_url: session.public_url.clone(),
        mode: Some(session.mode.clone()),
        profile_id: session.profile_id.clone(),
    }
}

/// Remove exited children from the map. Caller must hold the lock.
fn reap_exited(sessions: &mut HashMap<String, TunnelSession>) {
    let dead: Vec<String> = sessions
        .iter_mut()
        .filter_map(|(id, session)| match session.child.try_wait() {
            Ok(Some(_)) => Some(id.clone()),
            _ => None,
        })
        .collect();
    for id in dead {
        sessions.remove(&id);
    }
}

fn ensure_id_free(sessions: &HashMap<String, TunnelSession>, id: &str) -> Result<(), String> {
    if sessions.contains_key(id) {
        return Err("该隧道已在运行".to_string());
    }
    Ok(())
}

fn kill_session(mut session: TunnelSession) -> u32 {
    let pid = session.pid;
    let _ = session.child.kill();
    let _ = session.child.wait();
    kill_pid(pid);
    pid
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CloudflaredLogEvent {
    id: String,
    tag: String,
    line: String,
}

/// Known cloudflared startup failures worth a plain-Chinese fix hint, emitted as
/// an extra log line right after the offending one — the raw error alone sent a
/// user hunting through Cloudflare docs for what 1033 meant.
fn explain_cloudflared_line(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("the last ingress rule must match all urls") {
        return Some(
            "[cloudflared] config.yml 的 ingress 缺少兜底规则：最后一条不能带 hostname/path。在文件末尾补一行 `- service: http_status:404`（缩进与上一条对齐）后重新启动".into(),
        );
    }
    if lower.contains("invalid tunnel token") || lower.contains("token is not valid") {
        return Some(
            "[cloudflared] Tunnel Token 无效或已过期：到 Cloudflare Zero Trust 重新复制 token 并更新绑定".into(),
        );
    }
    if lower.contains("credentials file") && (lower.contains("not found") || lower.contains("no such file")) {
        return Some(
            "[cloudflared] 找不到隧道凭据文件：检查 config.yml 里 credentials-file 指向的 <隧道ID>.json 是否存在".into(),
        );
    }
    None
}

fn emit_log(app: &AppHandle, id: &str, tag: &str, line: &str) {
    let _ = app.emit(
        "cloudflared-log",
        CloudflaredLogEvent {
            id: id.to_string(),
            tag: tag.to_string(),
            line: line.to_string(),
        },
    );
}

fn spawn_log_readers(
    app: AppHandle,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    session_id: String,
    log_tag: String,
    parse_trycloudflare: bool,
) {
    fn spawn_one<R: std::io::Read + Send + 'static>(
        app: AppHandle,
        stream: Option<R>,
        session_id: String,
        log_tag: String,
        parse_trycloudflare: bool,
        announce_exit: bool,
    ) {
        if let Some(stream) = stream {
            thread::spawn(move || {
                let reader = BufReader::new(stream);
                for line in reader.lines().flatten() {
                    emit_log(&app, &session_id, &log_tag, &line);
                    if let Some(hint) = explain_cloudflared_line(&line) {
                        emit_log(&app, &session_id, &log_tag, &hint);
                    }
                    if parse_trycloudflare {
                        if let Some(url) = extract_trycloudflare_url(&line) {
                            if let Some(state) = app.try_state::<CloudflaredState>() {
                                if let Ok(mut guard) = state.sessions.lock() {
                                    if let Some(session) = guard.get_mut(&session_id) {
                                        session.public_url = Some(url.clone());
                                    }
                                }
                            }
                            let _ = app.emit(
                                "cloudflared-url",
                                serde_json::json!({ "id": session_id, "url": url }),
                            );
                        }
                    }
                }
                // Stream EOF means the cloudflared process is gone. Announce it once
                // (stderr reader only) so the log panel explains an incoming 1033.
                if announce_exit {
                    emit_log(
                        &app,
                        &session_id,
                        &log_tag,
                        "[cloudflared] 进程已退出，隧道不再在线（访问绑定域名会报 1033 Argo Tunnel error）",
                    );
                }
            });
        }
    }
    spawn_one(
        app.clone(),
        stdout,
        session_id.clone(),
        log_tag.clone(),
        parse_trycloudflare,
        false,
    );
    spawn_one(app, stderr, session_id, log_tag, parse_trycloudflare, true);
}

#[tauri::command]
pub fn cloudflared_open_download() -> Result<(), String> {
    open_url(DOWNLOAD_URL)
}

#[tauri::command]
pub fn cloudflared_tunnel_status(state: State<'_, CloudflaredState>) -> Vec<TunnelStatus> {
    let mut guard = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
    reap_exited(&mut guard);
    guard.values().map(session_to_status).collect()
}

/// Only forward values cloudflared accepts; empty / "auto" keeps the default
/// edge-protocol negotiation. HTTP2 is the escape hatch when QUIC (UDP 7844)
/// is throttled or blocked — a common cause of tunnels that never register.
fn push_protocol(cmd: &mut std::process::Command, protocol: Option<&str>) {
    let p = protocol
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if p == "http2" || p == "quic" {
        cmd.args(["--protocol", &p]);
    }
}

#[tauri::command]
pub async fn cloudflared_start_quick_tunnel(
    app: AppHandle,
    local_url: String,
    protocol: Option<String>,
) -> Result<TunnelStatus, String> {
    tokio::task::spawn_blocking(move || {
        cloudflared_start_quick_tunnel_sync(app, local_url, protocol)
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?
}

/// Blocking core: resolves the cloudflared binary (`where` + `--version`) and spawns
/// the tunnel with its log readers. A synchronous command runs all of that on the
/// main thread, which is what freezes the window.
fn cloudflared_start_quick_tunnel_sync(
    app: AppHandle,
    local_url: String,
    protocol: Option<String>,
) -> Result<TunnelStatus, String> {
    let state = app.state::<CloudflaredState>();
    let local = normalize_local_url(&local_url)?;
    let id = quick_session_id(&local);
    let log_tag = {
        let port = local
            .rsplit(':')
            .next()
            .unwrap_or("quick")
            .trim_matches(|c: char| !c.is_ascii_digit());
        format!(
            "quick:{}",
            if port.is_empty() { "url" } else { port }
        )
    };

    {
        let mut guard = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        reap_exited(&mut guard);
        ensure_id_free(&guard, &id)?;
    }

    let (bin, _) = resolve_cloudflared()?;
    // 临时隧道使用无 ingress 配置，避免加载默认 config.yml 中的 ingress 规则导致 404
    // cloudflared 会自动加载默认位置 (%USERPROFILE%\.cloudflared\config.yml) 的配置，
    // 如果其中包含 ingress 规则（特别是 catch-all http_status:404），临时隧道的 URL 会落入 catch-all 返回 404。
    // 使用 --config 指定一个不含 ingress 规则的配置文件即可阻止加载默认配置。
    let empty_cfg = std::env::temp_dir().join("cloudflared-quick-noiningress.yml");
    if !empty_cfg.exists() {
        let _ = std::fs::write(&empty_cfg, "tunnel: quick-tunnel-placeholder\n");
    }
    let mut cmd = hidden_cmd(&bin);
    cmd.args(["tunnel", "--no-autoupdate", "--config", &empty_cfg.to_string_lossy(), "--url", &local])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    push_protocol(&mut cmd, protocol.as_deref());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动 cloudflared 失败: {}（确认已安装并在 PATH 中）", e))?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let status = {
        let mut guard = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        reap_exited(&mut guard);
        if let Err(e) = ensure_id_free(&guard, &id) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        let session = TunnelSession {
            id: id.clone(),
            pid,
            mode: "quick".to_string(),
            local_url: local.clone(),
            public_url: None,
            profile_id: None,
            child,
        };
        let st = session_to_status(&session);
        guard.insert(id.clone(), session);
        st
    };

    spawn_log_readers(app, stdout, stderr, id, log_tag, true);
    Ok(status)
}

#[tauri::command]
pub async fn cloudflared_pick_config() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(cloudflared_pick_config_sync)
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

fn cloudflared_pick_config_sync() -> Result<Option<String>, String> {
    let mut dialog = rfd::FileDialog::new().set_title("选择 cloudflared config.yml");
    dialog = dialog.add_filter("YAML", &["yml", "yaml"]);
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let default_dir = PathBuf::from(home).join(".cloudflared");
        if default_dir.is_dir() {
            dialog = dialog.set_directory(default_dir);
        }
    }
    let Some(path) = dialog.pick_file() else {
        return Ok(None);
    };
    if !path.is_file() {
        return Err("所选配置文件不存在".to_string());
    }
    Ok(Some(path.to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn cloudflared_start_named_tunnel(
    app: AppHandle,
    profile_id: String,
    hostname: String,
    token: Option<String>,
    config_path: Option<String>,
    local_url: Option<String>,
    protocol: Option<String>,
) -> Result<TunnelStatus, String> {
    tokio::task::spawn_blocking(move || {
        cloudflared_start_named_tunnel_sync(
            app, profile_id, hostname, token, config_path, local_url, protocol,
        )
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?
}

/// Blocking core: same reasoning as `cloudflared_start_quick_tunnel_sync`.
fn cloudflared_start_named_tunnel_sync(
    app: AppHandle,
    profile_id: String,
    hostname: String,
    token: Option<String>,
    config_path: Option<String>,
    local_url: Option<String>,
    protocol: Option<String>,
) -> Result<TunnelStatus, String> {
    let state = app.state::<CloudflaredState>();
    let id = profile_id.trim().to_string();
    if id.is_empty() {
        return Err("缺少绑定 ID".to_string());
    }

    let host = normalize_hostname(&hostname)?;
    let public_url = public_url_from_hostname(&host);
    let local = match local_url {
        Some(u) if !u.trim().is_empty() => normalize_local_url(&u)?,
        _ => String::new(),
    };
    let log_tag = format!("named:{}", host);

    {
        let mut guard = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        reap_exited(&mut guard);
        ensure_id_free(&guard, &id)?;
    }

    let config = config_path
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let token = token
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let (bin, _) = resolve_cloudflared()?;
    let mut cmd = hidden_cmd(&bin);

    if let Some(cfg) = config {
        let cfg_path = PathBuf::from(&cfg);
        if !cfg_path.is_file() {
            return Err(format!("config.yml 不存在: {}", cfg));
        }
        cmd.args(["tunnel", "--no-autoupdate", "--config", &cfg, "run"]);
    } else if let Some(tok) = token {
        // SECURITY: Do NOT pass the token as a CLI argument (--token <tok>).
        // On Windows, command-line arguments of running processes are globally
        // readable via Task Manager, `wmic process get commandline`, or WMI.
        // Instead, write the token to a temp config file with a restrictive ACL
        // and point cloudflared at that via --config.
        let token_config_path = write_token_config(&tok)?;
        let cfg_path_str = token_config_path.to_string_lossy().to_string();
        let mut args = vec![
            "tunnel".to_string(),
            "--no-autoupdate".to_string(),
            "--config".to_string(),
            cfg_path_str,
            "run".to_string(),
        ];
        if !local.is_empty() {
            args.push("--url".to_string());
            args.push(local.clone());
        }
        cmd.args(&args);
    } else {
        return Err("请提供 Tunnel Token 或 config.yml 路径".to_string());
    }

    push_protocol(&mut cmd, protocol.as_deref());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动命名隧道失败: {}（确认已安装 cloudflared）", e))?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let status = {
        let mut guard = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        reap_exited(&mut guard);
        if let Err(e) = ensure_id_free(&guard, &id) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        let session = TunnelSession {
            id: id.clone(),
            pid,
            mode: "named".to_string(),
            local_url: local.clone(),
            public_url: Some(public_url.clone()),
            profile_id: Some(id.clone()),
            child,
        };
        let st = session_to_status(&session);
        guard.insert(id.clone(), session);
        st
    };

    spawn_log_readers(
        app.clone(),
        stdout,
        stderr,
        status.id.clone(),
        log_tag.clone(),
        false,
    );
    emit_log(
        &app,
        &status.id,
        &log_tag,
        &format!("Named tunnel started for {}", public_url),
    );
    let _ = app.emit(
        "cloudflared-url",
        serde_json::json!({ "id": status.id, "url": public_url }),
    );

    Ok(status)
}

#[tauri::command]
pub async fn cloudflared_stop_tunnel(
    state: State<'_, CloudflaredState>,
    id: String,
) -> Result<String, String> {
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err("缺少隧道 ID".to_string());
    }
    let session = drain_one_session(&state, &id);
    let Some(session) = session else {
        return Ok("该隧道未在运行".to_string());
    };
    let pid = tokio::task::spawn_blocking(move || kill_session(session))
        .await
        .map_err(|e| format!("Task failed: {e}"))?;
    Ok(format!("已停止隧道 (pid {})", pid))
}

/// Remove one session from the map (lock held only for the cheap map ops);
/// the blocking kill happens on the caller's blocking thread.
fn drain_one_session(state: &CloudflaredState, id: &str) -> Option<TunnelSession> {
    let mut guard = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
    reap_exited(&mut guard);
    guard.remove(id)
}

pub fn stop_all_sessions(state: &CloudflaredState) {
    let sessions = {
        let mut guard = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        reap_exited(&mut guard);
        guard.drain().map(|(_, session)| session).collect::<Vec<_>>()
    };
    for session in sessions {
        let _ = kill_session(session);
    }
}

#[tauri::command]
pub async fn cloudflared_stop_all_tunnels(
    state: State<'_, CloudflaredState>,
) -> Result<String, String> {
    let sessions = {
        let mut guard = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        reap_exited(&mut guard);
        guard.drain().map(|(_, session)| session).collect::<Vec<_>>()
    };
    let count = sessions.len();
    if count == 0 {
        return Ok("当前没有运行中的隧道".to_string());
    }
    // taskkill + child.wait per tunnel — off the async workers too.
    tokio::task::spawn_blocking(move || {
        for session in sessions {
            let _ = kill_session(session);
        }
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?;
    Ok(format!("已停止 {} 条隧道", count))
}

/// 一键配置新域名：修改 config.yml 添加 ingress 规则 + 执行 route dns 创建 CNAME
#[tauri::command]
pub async fn cloudflared_setup_new_domain(
    config_path: String,
    hostname: String,
    local_url: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        cloudflared_setup_new_domain_sync(config_path, hostname, local_url)
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?
}

/// Blocking core: rewrites config.yml and runs `cloudflared tunnel route dns`, which
/// talks to Cloudflare and regularly takes seconds.
fn cloudflared_setup_new_domain_sync(
    config_path: String,
    hostname: String,
    local_url: String,
) -> Result<String, String> {
    let cfg_path = PathBuf::from(&config_path);
    if !cfg_path.is_file() {
        return Err(format!("config.yml 不存在: {}", config_path));
    }

    // 1. 读取并解析 config.yml
    let content = fs::read_to_string(&cfg_path)
        .map_err(|e| format!("读取 config.yml 失败: {}", e))?;
    let mut config: serde_yaml::Value = serde_yaml::from_str(&content)
        .map_err(|e| format!("解析 config.yml 失败: {}", e))?;

    // 2. 获取隧道名称
    let tunnel_name = config
        .get("tunnel")
        .and_then(|v| v.as_str())
        .ok_or("config.yml 中缺少 tunnel 字段")?
        .to_string();

    // 3. 规范化 hostname 和 local_url
    let host = normalize_hostname(&hostname)?;
    let service = normalize_local_url(&local_url)?;

    // 4. 查找并更新 ingress 数组
    let ingress = config
        .get_mut("ingress")
        .and_then(|v| v.as_sequence_mut())
        .ok_or("config.yml 中缺少 ingress 字段")?;

    // 查找是否已有该 hostname 的规则
    let mut found = false;
    for item in ingress.iter_mut() {
        if let Some(item_hostname) = item.get("hostname").and_then(|v| v.as_str()) {
            if item_hostname == host {
                // 更新已有规则的 service
                if let Some(obj) = item.as_mapping_mut() {
                    obj.insert(
                        serde_yaml::Value::from("service"),
                        serde_yaml::Value::from(service.clone()),
                    );
                }
                found = true;
                break;
            }
        }
    }

    if !found {
        // 在 catch-all（http_status:404）之前插入新规则
        let new_rule = serde_yaml::from_str::<serde_yaml::Value>(&format!(
            "hostname: {}\nservice: {}",
            host, service
        ))
        .map_err(|e| format!("构造 ingress 规则失败: {}", e))?;

        // 找到 catch-all 的位置（最后一个只有 service 没有 hostname 的条目）
        let catch_all_idx = ingress.iter().rposition(|item| {
            item.get("hostname").is_none() && item.get("service").is_some()
        });

        match catch_all_idx {
            Some(idx) => ingress.insert(idx, new_rule),
            None => ingress.push(new_rule),
        }
    }

    // DNS does not depend on the local file. Run it first so a failed route
    // cannot leave config.yml rewritten or truncated.
    let (bin, _) = resolve_cloudflared()?;
    let output = hidden_cmd(&bin)
        .args(["tunnel", "route", "dns", &tunnel_name, &host])
        .output()
        .map_err(|e| format!("执行 route dns 失败: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let already = route_dns_target_exists(&stdout, &stderr);

    if !(output.status.success() || already) {
        return Err(format!(
            "route dns 失败，未修改 config.yml。stdout: {}\nstderr: {}",
            stdout.trim(),
            stderr.trim()
        ));
    }

    let updated = match patch_ingress_text(&content, &host, &service) {
        Ok(text) => text,
        Err(_) => serde_yaml::to_string(&config)
            .map_err(|e| format!("序列化 config.yml 失败: {}", e))?,
    };
    write_config_replacing(&cfg_path, updated.as_bytes())?;

    let msg = if already {
        format!("域名 {} 路由已存在（无需重复创建）", host)
    } else {
        format!("已为 {} 创建 CNAME 路由", host)
    };
    Ok(format!(
        "✅ {}\n✅ 已更新 config.yml 添加 ingress 规则\n{}",
        msg,
        stdout.trim()
    ))
}

fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// `cloudflared tunnel route dns` fails when the CNAME already exists, which is
/// the desired end state — treat it as success so re-running 一键配置 is
/// idempotent. cloudflared reports this on **stderr** with a non-zero exit.
fn route_dns_target_exists(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    combined.contains("already configured") || combined.contains("already exists")
}

fn yaml_unquote(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('\r');
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn yaml_field_in_item(item: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}:");
    for line in item.lines() {
        let trimmed = line.trim().trim_start_matches("- ").trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            return Some(yaml_unquote(rest));
        }
    }
    None
}

/// Insert or update an ingress rule without reserializing the whole document,
/// so comments and unrelated keys survive.
fn patch_ingress_text(content: &str, host: &str, service: &str) -> Result<String, String> {
    let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
    let had_trailing_nl = content.ends_with('\n');
    if lines.last().is_some_and(|l| l.is_empty()) && had_trailing_nl {
        lines.pop();
    }

    let ingress_idx = lines
        .iter()
        .position(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('#') && (trimmed == "ingress:" || trimmed.starts_with("ingress:"))
        })
        .ok_or("config.yml 中缺少 ingress 字段")?;
    let ingress_indent = leading_ws(&lines[ingress_idx]).to_string();

    let mut end = lines.len();
    for i in (ingress_idx + 1)..lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if leading_ws(&lines[i]).len() <= ingress_indent.len() {
            end = i;
            break;
        }
    }

    let mut catch_all: Option<usize> = None;
    let mut i = ingress_idx + 1;
    while i < end {
        let trimmed = lines[i].trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || !trimmed.starts_with('-') {
            i += 1;
            continue;
        }
        let item_start = i;
        let item_indent_len = leading_ws(&lines[i]).len();
        let mut item_end = i + 1;
        while item_end < end {
            let next = lines[item_end].trim();
            if next.is_empty() || next.starts_with('#') {
                item_end += 1;
                continue;
            }
            if leading_ws(&lines[item_end]).len() <= item_indent_len {
                break;
            }
            item_end += 1;
        }
        let item = lines[item_start..item_end].join("\n");
        let item_host = yaml_field_in_item(&item, "hostname");
        if item_host.as_deref() == Some(host) {
            if let Some(rel) = lines[item_start..item_end]
                .iter()
                .position(|line| line.trim().trim_start_matches("- ").trim().starts_with("service:"))
            {
                let abs = item_start + rel;
                let indent = leading_ws(&lines[abs]).to_string();
                lines[abs] = format!("{indent}service: {service}");
            } else {
                let indent = format!("{}  ", leading_ws(&lines[item_start]));
                lines.insert(item_end, format!("{indent}service: {service}"));
            }
            return Ok(join_yaml_lines(&lines, had_trailing_nl));
        }
        if item_host.is_none() && yaml_field_in_item(&item, "service").is_some() {
            catch_all = Some(item_start);
        }
        i = item_end;
    }

    let item_indent = format!("{ingress_indent}  ");
    let nested = format!("{item_indent}  ");
    let insert_at = catch_all.unwrap_or(end);
    lines.insert(insert_at, format!("{item_indent}- hostname: {host}"));
    lines.insert(insert_at + 1, format!("{nested}service: {service}"));
    Ok(join_yaml_lines(&lines, had_trailing_nl))
}

fn join_yaml_lines(lines: &[String], trailing_nl: bool) -> String {
    let mut text = lines.join("\n");
    if trailing_nl && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn write_config_replacing(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("yml.ai-workbench.tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("写入临时 config.yml 失败: {}", e))?;
    let replaced = replace_file(path, &tmp);
    if replaced.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    replaced
}

fn replace_file(dest: &Path, replacement: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            ReplaceFileW, REPLACEFILE_IGNORE_MERGE_ERRORS, REPLACEFILE_WRITE_THROUGH,
        };
        fn wide(path: &Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }
        let dest_w = wide(dest);
        let repl_w = wide(replacement);
        unsafe {
            ReplaceFileW(
                PCWSTR(dest_w.as_ptr()),
                PCWSTR(repl_w.as_ptr()),
                PCWSTR::null(),
                REPLACEFILE_WRITE_THROUGH | REPLACEFILE_IGNORE_MERGE_ERRORS,
                None,
                None,
            )
            .map_err(|e| format!("写入 config.yml 失败: {e}"))?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(replacement, dest).map_err(|e| format!("写入 config.yml 失败: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{explain_cloudflared_line, route_dns_target_exists};

    #[test]
    fn route_dns_existing_record_is_idempotent() {
        // cloudflared reports an existing CNAME on stderr with a non-zero exit.
        assert!(route_dns_target_exists(
            "",
            "failed to create CNAME: record already exists"
        ));
        assert!(route_dns_target_exists("CNAME record already configured", ""));
        assert!(!route_dns_target_exists(
            "",
            "failed to fetch credentials: authentication error"
        ));
    }

    #[test]
    fn explains_missing_ingress_catch_all() {
        let line = r#"ERR Couldn't start tunnel error="The last ingress rule must match all URLs (i.e. it should not have a hostname or path filter)""#;
        let hint = explain_cloudflared_line(line).expect("hint expected");
        assert!(hint.contains("http_status:404"), "got: {hint}");
    }

    #[test]
    fn explains_invalid_token() {
        let hint = explain_cloudflared_line("ERR Invalid Tunnel token provided");
        assert!(hint.is_some(), "hint expected");
        assert!(hint.unwrap().contains("Token 无效"));
    }

    #[test]
    fn regular_log_lines_get_no_hint() {
        let line = "INF Registered tunnel connection connIndex=0";
        assert!(explain_cloudflared_line(line).is_none());
    }
}
