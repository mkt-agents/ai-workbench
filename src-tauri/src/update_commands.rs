//! 应用内升级：读 GitHub Release 详情（更新日志 + 安装包资产）→ 流式下载安装包 →
//! 交给一个**分离的**看门狗进程，等本进程退出后再静默运行 NSIS 安装器。
//!
//! 为什么必须有看门狗：NSIS 无法覆盖正在运行的 exe，所以「先起安装器、后退本进程」
//! 这个顺序一定失败，只能是「先排好安装器 → 退本进程 → 安装器才动手」。
//!
//! 更新日志与安装包**只走 GitHub**：Gitee 上这个仓库目前是 404，jsDelivr 只有 tag、
//! 没有正文也没有资产（三源版本探测仍在前端 `src/lib/version.ts`，那是另一件事）。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::{Emitter, Manager, Runtime};
use tokio::io::AsyncWriteExt;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::cancellation::{self, CancelGuard};
use crate::config::{err_code, ErrorCode, WorkbenchError};

/// 与 `src/lib/version.ts` 的 `APP_GITHUB_REPO` 保持一致，改一处必须改两处。
const GITHUB_REPO: &str = "mkt-agents/ai-workbench";
const GITHUB_API: &str = "https://api.github.com";
const GITHUB_WEB: &str = "https://github.com";

/// 允许下载的入口 host。`browser_download_url` 之后会 302 到
/// `objects.githubusercontent.com`，那一跳由 reqwest 自己跟随，这里不校验。
const ALLOWED_HOSTS: [&str; 2] = ["github.com", "objects.githubusercontent.com"];

/// 更新日志在弹窗里只做预览，过长即截断（弹窗内仍给「查看完整发布页」）。
pub const NOTES_MAX_CHARS: usize = 4000;

/// 看门狗最长等待主进程消失的时间：120 × 250ms = 30s，超时照样开安装器。
const WATCHDOG_MAX_POLLS: u32 = 120;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInfo {
    /// 实际取到的是哪个 release（tag 命名与请求不一致时以此为准）。
    pub version: String,
    pub notes: Option<String>,
    pub setup_url: Option<String>,
    pub setup_size: u64,
    pub msi_url: Option<String>,
    pub html_url: String,
    /// true = GitHub API 没答上来（多半是匿名配额 403），日志与大小是靠
    /// **资产直链探测**兜底拿到的：能升级，但没有更新说明。
    pub degraded: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadedInstaller {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    stage: String,
    percent: u8,
    /// 已下载字节数与总字节数（total=0 表示服务端没给 Content-Length）。
    /// 刻意不带文案字段：中文字串留在后端会让 en-US 界面漏出中文，
    /// 进度文字一律由前端按 stage + 字节数用 i18n 拼。
    downloaded: u64,
    total: u64,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    #[serde(default)]
    name: String,
    size: Option<u64>,
    browser_download_url: Option<String>,
}

impl GhAsset {
    fn url(&self) -> &str {
        self.browser_download_url.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: Option<String>,
    html_url: Option<String>,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

// ---------------------------------------------------------------------------
// 纯函数层（可单测、不含 IO）
// ---------------------------------------------------------------------------

/// `https://host/...` 里的 host；不是 https 直接 None（安装包绝不允许走 http）。
fn url_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(&rest[..end])
}

fn asset_url_allowed(url: &str) -> bool {
    match url_host(url) {
        Some(host) => ALLOWED_HOSTS.iter().any(|h| host.eq_ignore_ascii_case(h)),
        None => false,
    }
}

/// NSIS setup.exe 优先，MSI 作回落。线上资产名形如
/// `AI.Workbench_0.2.8_x64-setup.exe` / `AI.Workbench_0.2.8_x64_en-US.msi`。
fn pick_setup_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase().ends_with("-setup.exe") && asset_url_allowed(a.url()))
}

fn pick_msi_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    assets.iter().find(|a| {
        let name = a.name.to_ascii_lowercase();
        name.ends_with(".msi") && asset_url_allowed(a.url())
    })
}

fn clean_release_notes(body: &str) -> String {
    let stripped = strip_markdown_images(&strip_html_tags(body));
    let text = stripped.replace("- [ ]", "- □").replace("- [x]", "- ☑");
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= NOTES_MAX_CHARS {
            out.push('\n');
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

/// 丢掉 markdown 图片 `![alt](url)`——弹窗里的 MarkdownView 不渲染图片，留着就是一行噪音。
/// 普通链接 `[文字](url)` 保留（渲染器支持）。未闭合的 `![` 当普通文本写回。
fn strip_markdown_images(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(pos) = rest.find("![") {
        out.push_str(&rest[..pos]);
        let after_alt = rest[pos + 2..].find("](").map(|p| pos + 2 + p + 2);
        match after_alt.and_then(|a| rest[a..].find(')').map(|p| a + p + 1)) {
            Some(end) => rest = &rest[end..],
            // 形如 `![未闭合` —— 把 "![" 原样写回，继续处理后面
            None => {
                out.push_str("![");
                rest = &rest[pos + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 摘掉 `<img …>`（弹窗里不渲染图，留着只是噪音）与 `<br>`（转换行）。
/// 其余尖括号内容原样保留——release 正文里 `<v0.2.9>` 这类占位符不该被当成标签吞掉。
fn strip_html_tags(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(end) = find_tag_end(bytes, i) {
                let name: String = src[i + 1..end]
                    .chars()
                    .map(|c| c.to_ascii_lowercase())
                    .take_while(|c| c.is_ascii_alphabetic())
                    .collect();
                match name.as_str() {
                    "img" => {}
                    "br" => out.push('\n'),
                    _ => out.push_str(&src[i..=end]),
                }
                i = end + 1;
                continue;
            }
        }
        let start = i;
        while i < bytes.len() && bytes[i] != b'<' {
            i += 1;
        }
        if i == start {
            // 落单的 '<' 且后面没有 '>'：当普通字符写回去，否则会原地死循环。
            out.push('<');
            i += 1;
        } else {
            out.push_str(&src[start..i]);
        }
    }
    out
}

fn find_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|b| *b == b'>')
        .map(|offset| start + offset)
}

/// 从下载 URL 尾段取文件名，只保留安全字符——它既参与路径拼接，也进 PowerShell 字面量。
/// setup.exe 与 msi 都原样保留后缀：`update_install` 靠后缀决定用 `/S` 还是 `msiexec`，
/// 把 MSI 伪装成 `.exe` 会装不上。
fn sanitize_asset_name(url: &str) -> String {
    let last = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .split('/')
        .last()
        .unwrap_or("");
    let safe: String = last
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    let lower = safe.to_ascii_lowercase();
    if safe.contains("..") {
        return String::from("ai-workbench-setup.exe");
    }
    if lower.ends_with("-setup.exe") || lower.ends_with(".msi") {
        safe
    } else {
        String::from("ai-workbench-setup.exe")
    }
}

/// PowerShell 单引号字面量：内部的 `'` 写成 `''`。
fn ps_quote(src: &str) -> String {
    format!("'{}'", src.replace('\'', "''"))
}

/// 静默安装成功后从注册表找安装目录并拉起新版主程序——升级的最后一环是
/// 「应用自己重新打开」，否则用户面对一个已退出的应用无从得知装没装上。
/// 找不到注册项就什么都不做：用户手动打开也能靠前端的 pending 版本确认结果。
/// （单行片段：作为 format! 的**值**插进去，里面的花括号不用转义。）
const RELAUNCH_SNIPPET: &str = "$e = Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*','HKLM:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*' -EA SilentlyContinue | Where-Object DisplayName -like 'AI Workbench*' | Select-Object -First 1; if ($e -and $e.InstallLocation) { Start-Process -FilePath (Join-Path $e.InstallLocation.Trim('\\') 'ai_workbench.exe') }";

/// 看门狗脚本：等 PID 消失（最长 30s）→ 静默装 → 成功则拉起新版、失败则改弹
/// 交互安装界面，让用户至少能看到失败原因（杀软拦截 / 文件占用）。
///
/// NSIS 用 `/S`；MSI（release 里的回落资产）必须走 `msiexec /i … /qb`，
/// 直接把 `.msi` 交给 `Start-Process` 只会打开资源管理器语义的默认动作。
///
/// 变量名不能叫 `$pid`：那是 PowerShell 的只读自动变量，赋值会直接报错。
fn watchdog_script(installer: &Path, app_pid: u32, max_polls: u32) -> String {
    let path = installer.to_string_lossy();
    let is_msi = path.to_ascii_lowercase().ends_with(".msi");
    let (silent_file, silent_args, interactive_file) = if is_msi {
        (
            ps_quote("msiexec.exe"),
            format!("'/i', {}, '/qb'", ps_quote(&path)),
            ps_quote(&path),
        )
    } else {
        (ps_quote(&path), "'/S'".to_string(), ps_quote(&path))
    };
    format!(
        "$ErrorActionPreference='SilentlyContinue'; $watchId={pid}; $polls=0; \
         while ((Get-Process -Id $watchId -ErrorAction SilentlyContinue) -and ($polls -lt {max})) {{ Start-Sleep -Milliseconds 250; $polls++ }}; \
         Start-Sleep -Milliseconds 500; \
         $p = Start-Process -FilePath {silent_file} -ArgumentList {silent_args} -Wait -PassThru; \
         if (-not $p -or $p.ExitCode -ne 0) {{ Start-Process -FilePath {interactive_file} }} else {{ {relaunch} }}",
        pid = app_pid,
        max = max_polls,
        silent_file = silent_file,
        silent_args = silent_args,
        interactive_file = interactive_file,
        relaunch = RELAUNCH_SNIPPET,
    )
}

fn to_info(rel: GhRelease) -> ReleaseInfo {
    let setup = pick_setup_asset(&rel.assets);
    ReleaseInfo {
        version: rel
            .tag_name
            .unwrap_or_default()
            .trim_start_matches('v')
            .to_string(),
        notes: rel
            .body
            .as_deref()
            .map(clean_release_notes)
            .filter(|s| !s.is_empty()),
        setup_url: setup.map(|a| a.url().to_string()),
        setup_size: setup.and_then(|a| a.size).unwrap_or(0),
        msi_url: pick_msi_asset(&rel.assets).map(|a| a.url().to_string()),
        html_url: rel
            .html_url
            .unwrap_or_else(|| format!("{}/{}/releases", GITHUB_WEB, GITHUB_REPO)),
        degraded: false,
    }
}

// ---------------------------------------------------------------------------
// IO 辅助
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 起一个不带控制台的子进程（与 `runtime_commands.rs` / `tool_commands.rs` 同款）。
fn hidden_cmd(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, WorkbenchError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("ai-workbench")
        .build()
        .map_err(|e| err_code(ErrorCode::InternalError, format!("初始化网络客户端失败: {}", e)))
}

/// 取一个 release 详情。`Err(原因)` 同时覆盖"网络废了"与"答了但没这个 release"，
/// 原因里一定带状态码：403（未登录 60 次/小时·IP 的匿名配额）与 404（没这个 tag）
/// 对用户的处置完全不同，不能都糊成"没取到"。
async fn fetch_release(url: &str) -> Result<GhRelease, String> {
    let resp = http_client(Duration::from_secs(15))
        .map_err(|e| e.detail)?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("请求失败: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let text = resp.text().await.map_err(|e| format!("读取响应失败: {}", e))?;
    serde_json::from_str::<GhRelease>(&text).map_err(|e| format!("解析 release 失败: {}", e))
}

/// GitHub 匿名 API 配额只有 60 次/小时·IP（共享出口很容易打满），但 **release 资产直链
/// 不占配额**。API 全失败时按下载路径的固定命名拼候选 URL，探到就用：顶多丢更新日志，
/// 升级本身不该被限流卡死（版本探测那三路里 jsDelivr 就是为这个才存在的）。
fn fallback_setup_urls(version: &str) -> [String; 2] {
    let base = format!(
        "{}/{}/releases/download/v{}",
        GITHUB_WEB, GITHUB_REPO, version
    );
    [
        format!("{base}/AI.Workbench_{version}_x64-setup.exe"),
        format!("{base}/ai_workbench_{version}_x64-setup.exe"),
    ]
}

/// HEAD 探测候选直链，命中返回 (url, size)。
///
/// 实测：GitHub 的下载直链会 302 跳到 `objects.githubusercontent.com`，HEAD 回包
/// **常没有 `Content-Length`**，所以这里的 size 可能是 0（=未知）。前端据此不显示
/// 体积文案；真正的完整性校验在 `download_installer_to` 里用 GET 的 content_length 做。
async fn probe_setup_url(version: &str) -> Option<(String, u64)> {
    let client = http_client(Duration::from_secs(20)).ok()?;
    for url in fallback_setup_urls(version) {
        if let Ok(resp) = client.head(&url).send().await {
            if resp.status().is_success() {
                return Some((url, resp.content_length().unwrap_or(0)));
            }
        }
    }
    None
}

fn app_data_dir() -> Result<PathBuf, WorkbenchError> {
    let appdata = std::env::var("APPDATA")
        .map_err(|_| err_code(ErrorCode::InternalError, "无法定位 APPDATA"))?;
    let dir = Path::new(&appdata).join("com.ai-workbench.app");
    fs::create_dir_all(&dir)
        .map_err(|e| err_code(ErrorCode::InternalError, format!("创建数据目录失败: {}", e)))?;
    Ok(dir)
}

fn emit_progress<R: Runtime, E: Emitter<R>>(
    emitter: &E,
    stage: &str,
    percent: u8,
    downloaded: u64,
    total: u64,
) {
    let _ = emitter.emit(
        "app:update_progress",
        UpdateProgress {
            stage: stage.to_string(),
            percent,
            downloaded,
            total,
        },
    );
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let value = bytes as f64;
    if value >= MB {
        format!("{:.1} MB", value / MB)
    } else {
        format!("{:.0} KB", value / KB)
    }
}

// ---------------------------------------------------------------------------
// Tauri 命令
// ---------------------------------------------------------------------------

/// 取某版本的更新日志与安装包地址。依次尝试 `v{version}` → `{version}` → latest；
/// API 全废（多半是匿名配额 403）时改用**不占配额**的资产直链探测兜底，
/// 只在连直链也探不到时才报错——此时前端回落到「在浏览器中打开」。
#[tauri::command]
pub async fn update_fetch_release_info(version: String) -> Result<ReleaseInfo, WorkbenchError> {
    let v = version.trim().trim_start_matches('v');
    if v.is_empty() {
        return Err(err_code(ErrorCode::InvalidInput, "版本号不能为空"));
    }
    let base = format!("{}/repos/{}", GITHUB_API, GITHUB_REPO);
    let candidates = [
        (format!("{}/releases/tags/v{}", base, v), "tag"),
        (format!("{}/releases/tags/{}", base, v), "tag"),
        (format!("{}/releases/latest", base), "latest"),
    ];
    let mut reasons: Vec<String> = Vec::new();
    for (url, label) in candidates.iter() {
        match fetch_release(url).await {
            Ok(rel) => return Ok(to_info(rel)),
            Err(reason) => reasons.push(format!("{} {}", label, reason)),
        }
    }
    // 配额打满时没有日志，但升级仍可完成：直链探测不经过 api.github.com。
    if let Some((url, size)) = probe_setup_url(v).await {
        return Ok(ReleaseInfo {
            version: v.to_string(),
            notes: None,
            setup_url: Some(url),
            setup_size: size,
            msi_url: None,
            html_url: format!("{}/{}/releases/tag/v{}", GITHUB_WEB, GITHUB_REPO, v),
            degraded: true,
        });
    }
    Err(err_code(
        ErrorCode::UpdateSourceUnreachable,
        format!("{}；直链也没探到 v{} 的安装包", reasons.join("；"), v),
    ))
}

/// 流式下载安装包到 `%APPDATA%\com.ai-workbench.app\updates\`。
/// 先写 `.part` 再改名，中途失败不会留下一个"看起来是完整安装包"的文件。
#[tauri::command]
pub async fn update_download(
    request_id: String,
    url: String,
    window: tauri::Window,
) -> Result<DownloadedInstaller, WorkbenchError> {
    download_installer(&request_id, &url, &mut |stage, percent, downloaded, total| {
        emit_progress(&window, stage, percent, downloaded, total)
    })
    .await
}

/// `update_download` 的实现体，进度通过回调外发——这样不依赖 `tauri::Window`
/// 也能被 `#[ignore]` 的真实网络审计测试直接驱动（复刻真实调用链，不造假响应）。
/// `target_dir` 为测试用的落盘目录覆盖；None 时落 app_data。
async fn download_installer(
    request_id: &str,
    url: &str,
    on_progress: &mut (dyn FnMut(&str, u8, u64, u64) + Send),
) -> Result<DownloadedInstaller, WorkbenchError> {
    download_installer_to(request_id, url, None, on_progress).await
}

async fn download_installer_to(
    request_id: &str,
    url: &str,
    target_dir: Option<PathBuf>,
    on_progress: &mut (dyn FnMut(&str, u8, u64, u64) + Send),
) -> Result<DownloadedInstaller, WorkbenchError> {
    let _guard = CancelGuard::new(request_id);
    if !asset_url_allowed(url) {
        return Err(err_code(
            ErrorCode::UpdateAssetMissing,
            "下载地址不在允许的来源内",
        ));
    }

    let dir = match target_dir {
        Some(dir) => dir,
        None => app_data_dir()?.join("updates"),
    };
    fs::create_dir_all(&dir)
        .map_err(|e| err_code(ErrorCode::InternalError, format!("创建下载目录失败: {}", e)))?;
    // 只清上一次的安装产物，别碰目录里其它文件。
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_artifact = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e.to_ascii_lowercase().as_str(), "exe" | "msi" | "part"))
                .unwrap_or(false);
            if is_artifact {
                let _ = fs::remove_file(&path);
            }
        }
    }

    let name = sanitize_asset_name(url);
    let final_path = dir.join(&name);
    let tmp_path = dir.join(format!("{}.part", name));

    on_progress("download", 0, 0, 0);
    let mut resp = http_client(Duration::from_secs(1800))?
        .get(url)
        .send()
        .await
        .map_err(|e| {
            err_code(
                ErrorCode::UpdateDownloadFailed,
                format!("连接失败: {}", e),
            )
        })?;
    if !resp.status().is_success() {
        return Err(err_code(
            ErrorCode::UpdateDownloadFailed,
            format!("HTTP {}", resp.status()),
        ));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(|e| err_code(ErrorCode::InternalError, format!("创建下载文件失败: {}", e)))?;

    let mut downloaded: u64 = 0;
    let mut reported: u64 = 0;
    loop {
        let failed = if cancellation::is_cancelled(request_id) {
            Some(err_code(ErrorCode::UpdateCancelled, "下载已取消"))
        } else {
            match tokio::time::timeout(Duration::from_secs(30), resp.chunk()).await {
                Err(_) => Some(err_code(
                    ErrorCode::UpdateDownloadFailed,
                    "下载超时（30 秒无数据）",
                )),
                Ok(Ok(Some(chunk))) => {
                    if let Err(e) = file.write_all(&chunk).await {
                        Some(err_code(
                            ErrorCode::InternalError,
                            format!("写入失败: {}", e),
                        ))
                    } else {
                        downloaded += chunk.len() as u64;
                        if downloaded - reported >= 1_048_576 {
                            reported = downloaded;
                            let percent = if total > 0 {
                                ((downloaded * 100) / total).min(99) as u8
                            } else {
                                0
                            };
                            on_progress("download", percent, downloaded, total);
                        }
                        None
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => Some(err_code(
                    ErrorCode::UpdateDownloadFailed,
                    format!("下载中断: {}", e),
                )),
            }
        };
        // 任何一条失败路径都要扔掉半成品，不能留一个假的"安装包"。
        if let Some(err) = failed {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            on_progress("download", 0, downloaded, total);
            return Err(err);
        }
    }
    if let Err(e) = file.flush().await {
        drop(file);
        let _ = fs::remove_file(&tmp_path);
        return Err(err_code(ErrorCode::InternalError, format!("写入失败: {}", e)));
    }
    drop(file);

    // 只校验大小（不引入摘要依赖）：对不上就删掉，让用户改走浏览器下载。
    if total > 0 && downloaded != total {
        let _ = fs::remove_file(&tmp_path);
        return Err(err_code(
            ErrorCode::UpdateSizeMismatch,
            format!(
                "下载不完整（{} ≠ 预期 {}）",
                format_size(downloaded),
                format_size(total)
            ),
        ));
    }
    on_progress("verify", 99, downloaded, downloaded);
    fs::rename(&tmp_path, &final_path).map_err(|e| {
        err_code(ErrorCode::InternalError, format!("保存安装包失败: {}", e))
    })?;
    let size = fs::metadata(&final_path).map(|m| m.len()).unwrap_or(downloaded);
    on_progress("verify", 100, size, size);
    Ok(DownloadedInstaller {
        path: final_path.to_string_lossy().to_string(),
        size,
    })
}

/// 排好看门狗 → 停隧道 → 退出本进程。安装器在 PID 消失后才动手。
#[tauri::command]
pub fn update_install(path: String, app: tauri::AppHandle) -> Result<(), WorkbenchError> {
    let installer = Path::new(&path);
    if !installer.is_file() {
        return Err(err_code(
            ErrorCode::UpdateAssetMissing,
            "安装包不存在，请重新下载",
        ));
    }
    let kind_ok = installer
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "exe" || e == "msi"
        })
        .unwrap_or(false);
    if !kind_ok {
        return Err(err_code(ErrorCode::InvalidInput, "只支持 .exe / .msi 安装包"));
    }

    let script = watchdog_script(installer, std::process::id(), WATCHDOG_MAX_POLLS);
    hidden_cmd("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            err_code(
                ErrorCode::UpdateLaunchFailed,
                format!("启动安装程序失败: {}", e),
            )
        })?;

    emit_progress(&app, "launch", 100, 0, 0);
    // 与托盘「退出」同一条路径：不先停隧道会留下孤儿 cloudflared 进程。
    if let Some(state) = app.try_state::<crate::cloudflared_commands::CloudflaredState>() {
        crate::cloudflared_commands::stop_all_sessions(&state);
    }
    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SETUP_URL: &str = "https://github.com/mkt-agents/ai-workbench/releases/download/v0.2.8/AI.Workbench_0.2.8_x64-setup.exe";
    const MSI_URL: &str = "https://github.com/mkt-agents/ai-workbench/releases/download/v0.2.8/AI.Workbench_0.2.8_x64_en-US.msi";

    fn asset(name: &str, url: &str) -> GhAsset {
        GhAsset {
            name: name.to_string(),
            size: Some(123),
            browser_download_url: Some(url.to_string()),
        }
    }

    #[test]
    fn setup_asset_wins_over_msi_and_is_case_insensitive() {
        let assets = vec![
            asset("AI.Workbench_0.2.8_x64_en-US.msi", MSI_URL),
            asset("ai.workbench_0.2.8_X64-SETUP.EXE", SETUP_URL),
        ];
        let picked = pick_setup_asset(&assets).expect("应该挑到 setup.exe");
        assert_eq!(picked.url(), SETUP_URL);
        assert!(pick_msi_asset(&assets).is_some());
    }

    #[test]
    fn source_archives_are_not_picked_as_installers() {
        let assets = vec![
            asset("source-code.tar.gz", "https://codeload.github.com/x/y/tar.gz/v1"),
            asset("AI.Workbench_x64_en-US.msi", MSI_URL),
        ];
        assert!(pick_setup_asset(&assets).is_none());
    }

    #[test]
    fn only_https_github_hosts_are_allowed() {
        assert!(asset_url_allowed(SETUP_URL));
        assert!(asset_url_allowed(
            "https://objects.githubusercontent.com/x/AI.Workbench_x64-setup.exe"
        ));
        assert!(!asset_url_allowed(&SETUP_URL.replace("https://", "http://")));
        assert!(!asset_url_allowed("https://evil.com/github.com/AI-x64-setup.exe"));
        assert!(!asset_url_allowed("https://github.com.evil.com/a-setup.exe"));
        assert!(!asset_url_allowed("not-a-url"));
    }

    #[test]
    fn asset_name_keeps_real_installer_suffixes() {
        assert_eq!(
            sanitize_asset_name(SETUP_URL),
            "AI.Workbench_0.2.8_x64-setup.exe"
        );
        assert_eq!(
            sanitize_asset_name(MSI_URL),
            "AI.Workbench_0.2.8_x64_en-US.msi"
        );
        // 未知后缀 / 目录穿越 / 空格：一律退回安全默认名
        assert_eq!(
            sanitize_asset_name("https://x.github.com/dir/tool.zip"),
            "ai-workbench-setup.exe"
        );
        assert_eq!(
            sanitize_asset_name("https://x.github.com/dir/AI..-setup.exe?tok=1"),
            "ai-workbench-setup.exe"
        );
        assert_eq!(
            sanitize_asset_name("https://x.github.com/dir/My App_x64-setup.exe"),
            "MyApp_x64-setup.exe"
        );
    }

    #[test]
    fn notes_drop_images_keep_inline_tags_and_truncate() {
        let body = "## v0.2.9\n![badge](https://x/y.png)\n- 完成<br>- 待办 <br >\n- [ ] 未做\n- [x] 已做\n<b>保留</b> <v0.2.9>\n详见[发布页](https://github.com/x)";
        let cleaned = clean_release_notes(body);
        assert!(!cleaned.contains("![badge"));
        assert!(!cleaned.contains("https://x/y.png"));
        assert!(!cleaned.contains("<br"));
        assert!(cleaned.contains("<b>保留</b>"));
        assert!(cleaned.contains("<v0.2.9>"));
        assert!(cleaned.contains("- □ 未做"));
        assert!(cleaned.contains("- ☑ 已做"));
        // 普通链接要留着（渲染器能把「发布页」显示成可点链接）
        assert!(cleaned.contains("详见[发布页](https://github.com/x)"));

        let long = "字".repeat(NOTES_MAX_CHARS + 50);
        let truncated = clean_release_notes(&long);
        assert!(truncated.chars().count() <= NOTES_MAX_CHARS + 2);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn unclosed_markdown_image_is_kept_as_text() {
        assert_eq!(strip_markdown_images("a ![未闭合"), "a ![未闭合");
        assert_eq!(
            strip_markdown_images("x ![alt](u.png) y"),
            "x  y"
        );
    }

    #[test]
    fn lone_angle_bracket_does_not_hang_the_stripper() {
        assert_eq!(strip_html_tags("a < b"), "a < b");
        assert_eq!(strip_html_tags("a < b > c"), "a < b > c");
    }

    #[test]
    fn watchdog_waits_for_pid_then_falls_back_to_ui() {
        let script = watchdog_script(
            Path::new(r"C:\Users\me\Local Settings\AI Workbench_0.2.9_x64-setup.exe"),
            4321,
            WATCHDOG_MAX_POLLS,
        );
        // 产品名带空格必须整体进单引号字面量
        assert!(script.contains(r#"'C:\Users\me\Local Settings\AI Workbench_0.2.9_x64-setup.exe'"#));
        assert!(script.contains("$watchId=4321"));
        assert!(script.contains("-ArgumentList '/S'"));
        assert!(script.contains(&format!("-lt {}", WATCHDOG_MAX_POLLS)));
        // PowerShell 的 $PID 是只读自动变量，覆盖它脚本会当场报错
        assert!(!script.contains("$pid="));
        // 静默成功才拉起新版，失败走交互安装界面
        assert!(script.contains("} else {"));
        assert!(script.contains("'ai_workbench.exe'"));
        assert!(script.contains("InstallLocation"));
    }

    #[test]
    fn ps_quote_doubles_inner_single_quote() {
        assert_eq!(ps_quote(r"C:\it's\a.exe"), r"'C:\it''s\a.exe'");
    }

    /// 从注册表卸载项读本机 AI Workbench 的 (安装目录, 登记版本)。
    /// 只给下面的真实安装审计用——不想把 `D:\...` 之类写死在测试里。
    fn installed_registration() -> Option<(PathBuf, String)> {
        let probe = "$e = Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*',\
             'HKLM:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*' -EA SilentlyContinue \
             | Where-Object DisplayName -like 'AI Workbench*' | Select-Object -First 1; \
             if ($e) { '{0}|{1}' -f $e.InstallLocation.Trim('\\\"'), $e.DisplayVersion }";
        let out = hidden_cmd("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", probe])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .find(|l| l.contains('|'))?
            .to_string();
        let (dir, version) = line.split_once('|')?;
        Some((PathBuf::from(dir), version.to_string()))
    }

    /// 真装一遍：下载线上 setup.exe → 用**生成的看门狗脚本**静默安装 → 核对确实装了。
    /// 平时不跑：`cargo test -- --ignored audit_real_silent_install_end_to_end`
    /// 默认装"当前构建号"对应的 release（同版本重装，不会把本机装旧）；
    /// 要验"升级到更新版本"就显式指定：`AIWB_AUDIT_VERSION=0.2.8 cargo test -- --ignored ...`
    /// 想复刻"先退应用再装"的真实时序：`AIWB_AUDIT_WAIT_PID=<已安装实例的 PID>`（预算 10 分钟）。
    ///
    /// 成败证据用 `uninstall.exe` 的时间戳 + 注册表版本号，**不用主程序时间戳**：
    /// 实测 NSIS 对同版本的主 exe 是跳过覆盖的（mtime 保持原值），只有卸载器每次都被重写。
    #[tokio::test]
    #[ignore = "会对本机已安装的 AI Workbench 执行一次真实静默重装"]
    async fn audit_real_silent_install_end_to_end() {
        let wanted = std::env::var("AIWB_AUDIT_VERSION")
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
        let info = update_fetch_release_info(wanted.clone())
            .await
            .expect("线上 release 应可取（API 或资产直链兜底）");
        assert_eq!(info.version, wanted, "应取到请求的版本: {}", info.version);
        let url = info.setup_url.clone().expect("应带 setup.exe 资产");
        let dir = std::env::temp_dir().join("aiwb-update-audit-install");
        let file = download_installer_to("audit-install", &url, Some(dir.clone()), &mut |_, _, _, _| {})
            .await
            .expect("真实下载应成功");
        assert!(file.path.to_ascii_lowercase().ends_with("-setup.exe"));

        let (dir_installed, version_before) =
            installed_registration().expect("注册表里应能查到本机安装位置");
        // NSIS 不带 /D 时会沿用注册表里的安装目录，所以这一步不会搬动本机安装。
        let uninstaller = dir_installed.join("uninstall.exe");
        let before = fs::metadata(&uninstaller).and_then(|m| m.modified()).ok();

        // 等哪个 PID 消失才开装：默认用一个必然不存在的值（立刻装）；
        // 想复刻"先退应用再装"的真实时序就传本机已安装实例的 PID：
        //   AIWB_AUDIT_WAIT_PID=36744 cargo test -- --ignored audit_real_silent_install
        // 预算给 2400 × 250ms = 10 分钟，够人手工从托盘退出。
        let wait_pid = std::env::var("AIWB_AUDIT_WAIT_PID")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(u32::MAX);
        let script = watchdog_script(Path::new(&file.path), wait_pid, 2400);
        let status = hidden_cmd("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .status()
            .expect("powershell 应能启动");
        assert!(status.success(), "看门狗应正常退出");

        let after = fs::metadata(&uninstaller)
            .and_then(|m| m.modified())
            .expect("安装后卸载器应存在");
        assert!(
            before.map(|b| after > b).unwrap_or(true),
            "静默安装应重写卸载器: {}",
            uninstaller.display()
        );
        let (_, version_after) = installed_registration().expect("安装后仍应查得到注册项");
        assert_eq!(version_after, wanted, "登记版本应为 {}", wanted);
        assert!(
            dir_installed.join("ai_workbench.exe").is_file(),
            "主程序应在 {} 原位可用",
            dir_installed.display()
        );
        // 同版本重装时 NSIS 会跳过主 exe 的覆盖（实测 mtime 不变），这是正常形态，
        // 所以这里只在版本变了时才要求主 exe 被重写。
        if version_before != version_after {
            let exe = dir_installed.join("ai_workbench.exe");
            assert!(
                fs::metadata(&exe).and_then(|m| m.modified()).ok() > before,
                "跨版本升级应重写主程序: {}",
                exe.display()
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fallback_urls_use_the_real_asset_naming_and_skip_the_api() {
        let urls = fallback_setup_urls("0.2.8");
        assert!(urls[0].ends_with("/download/v0.2.8/AI.Workbench_0.2.8_x64-setup.exe"));
        assert!(urls[1].ends_with("/download/v0.2.8/ai_workbench_0.2.8_x64-setup.exe"));
        assert!(urls.iter().all(|u| asset_url_allowed(u)));
        // 关键：兜底不能再走 api.github.com —— 限流的正是它
        assert!(urls.iter().all(|u| !u.contains("api.github.com")));
    }

    #[test]
    fn msi_fallback_goes_through_msiexec_not_the_file_itself() {
        let script = watchdog_script(Path::new(r"C:\updates\AI Workbench_0.2.9_x64_en-US.msi"), 77, WATCHDOG_MAX_POLLS);
        assert!(script.contains("-FilePath 'msiexec.exe'"));
        assert!(script.contains(r#"-ArgumentList '/i', 'C:\updates\AI Workbench_0.2.9_x64_en-US.msi', '/qb'"#));
        assert!(!script.contains("'/S'"));
        // 回落仍然是那个 .msi（双击走安装向导）
        assert!(script.contains("Start-Process -FilePath 'C:\\updates\\AI Workbench_0.2.9_x64_en-US.msi' }"));
        // MSI 静默成功后同样拉起新版
        assert!(script.ends_with("else { $e = Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*','HKLM:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*' -EA SilentlyContinue | Where-Object DisplayName -like 'AI Workbench*' | Select-Object -First 1; if ($e -and $e.InstallLocation) { Start-Process -FilePath (Join-Path $e.InstallLocation.Trim('\\') 'ai_workbench.exe') } }"));
    }

    #[test]
    fn release_info_maps_assets_notes_and_tag() {
        let info = to_info(GhRelease {
            tag_name: Some("v0.2.8".to_string()),
            html_url: Some("https://github.com/x/y/releases/tag/v0.2.8".to_string()),
            body: Some("### 新功能\n- 应用内升级".to_string()),
            assets: vec![
                asset("AI.Workbench_0.2.8_x64_en-US.msi", MSI_URL),
                asset("AI.Workbench_0.2.8_x64-setup.exe", SETUP_URL),
            ],
        });
        assert_eq!(info.version, "0.2.8");
        assert_eq!(info.setup_size, 123);
        assert_eq!(info.setup_url.as_deref(), Some(SETUP_URL));
        assert_eq!(info.msi_url.as_deref(), Some(MSI_URL));
        assert!(info.notes.unwrap().contains("应用内升级"));
    }

    #[test]
    fn release_without_allowed_asset_degrades_to_no_urls() {
        let info = to_info(GhRelease {
            tag_name: Some("v9".to_string()),
            html_url: None,
            body: None,
            assets: vec![asset("bad-setup.exe", "http://github.com/x/y/bad-setup.exe")],
        });
        assert!(info.setup_url.is_none());
        assert!(info.notes.is_none());
        assert_eq!(
            info.html_url,
            "https://github.com/mkt-agents/ai-workbench/releases"
        );
    }

    /// 真实链路审计：打线上 GitHub，走完整的「取详情 → 流式下载 → 落盘 → 校验大小」，
    /// 不伪造响应。平时不跑，改动下载/解析逻辑后手动执行：
    /// `cargo test -- --ignored audit_real_release_fetch_and_download`
    #[tokio::test]
    #[ignore = "需要联网并下载约 8 MB 真实安装包"]
    async fn audit_real_release_fetch_and_download() {
        // 请求"自己正在构建的这个版本"——它就是线上已发布的 tag，
        // 与真实调用链一致（应用里传给这层的版本，正是前端三源探测拿到的 remote）。
        let wanted = env!("CARGO_PKG_VERSION");
        let info = update_fetch_release_info(wanted.to_string())
            .await
            .expect("线上 release 应可取（API 或资产直链兜底）");
        assert!(!info.version.is_empty(), "版本号不能为空");
        if info.degraded {
            // 匿名配额 403 时的合法形态：没有日志，但升级链路仍然完整
            assert!(info.notes.is_none(), "兜底路径不该带日志");
            assert!(
                info.html_url.contains("/releases/tag/"),
                "兜底应指向 tag 发布页: {}",
                info.html_url
            );
        } else {
            let notes = info.notes.clone().expect("线上 release 写了更新说明");
            assert!(notes.contains('v') || notes.contains('-'), "正文应有内容");
            assert_eq!(info.version, wanted, "应取到请求的那个版本");
            assert!(info.setup_size > 1_000_000, "setup 应有正常体积: {}", info.setup_size);
        }
        let url = info.setup_url.clone().expect("应拿到 setup.exe 直链");
        assert!(asset_url_allowed(&url));
        assert!(url.ends_with("-setup.exe"), "应挑到 NSIS setup: {}", url);

        let dir = std::env::temp_dir().join("aiwb-update-audit");
        let mut stages: Vec<(String, u8, u64, u64)> = Vec::new();
        let file = download_installer_to(
            "audit-req",
            &url,
            Some(dir.clone()),
            &mut |stage, percent, downloaded, total| {
                stages.push((stage.to_string(), percent, downloaded, total))
            },
        )
        .await
        .expect("真实下载应成功");

        // 落盘文件大小必须等于下载时看到的 content_length（我们唯一的完整性校验口径）；
        // API 兜底路径拿不到声明体积（HEAD 302 无 Content-Length），此时只看下盘大小。
        let on_disk = fs::metadata(&file.path).expect("文件应存在").len();
        if info.setup_size > 0 {
            assert_eq!(on_disk, info.setup_size, "落盘大小应等于 API 声明");
        }
        assert_eq!(file.size, on_disk);
        assert!(on_disk > 1_000_000, "安装包应有正常体积: {}", on_disk);
        assert!(file.path.ends_with("-setup.exe"), "命名应保留资产名: {}", file.path);
        assert!(
            stages.iter().any(|(s, ..)| s == "verify"),
            "应发出 verify 阶段事件"
        );
        // 下载阶段百分比单调不减，且上报的字节数不超过声明体积
        let percents: Vec<u8> = stages
            .iter()
            .filter(|(s, ..)| s == "download")
            .map(|(_, p, ..)| *p)
            .collect();
        assert!(percents.windows(2).all(|w| w[0] <= w[1]), "百分比倒退: {:?}", percents);
        assert!(
            stages
                .iter()
                .all(|(s, _, downloaded, total)| s != "download" || *downloaded <= *total.max(&file.size)),
            "上报字节数不应超过总大小: {:?}",
            stages
        );
        // 没有留下 .part 半成品
        assert!(fs::read_dir(&dir)
            .map(|rd| rd.flatten().filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("part")).count() == 0)
            .unwrap_or(true));

        let _ = fs::remove_dir_all(&dir);
    }
}
