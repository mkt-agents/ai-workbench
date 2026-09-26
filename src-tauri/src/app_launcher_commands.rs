//! App launcher commands: launch / kill / detect external applications.
//!
//! Windows-only (matches the rest of the app). Uses `taskkill` for soft+force
//! kill and `tasklist` for running detection.

use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Soft-kill timeout: wait up to 3 seconds after the first `taskkill /IM`
/// before escalating to `/F`.
const SOFT_KILL_TIMEOUT_MS: u64 = 3000;

/// Poll interval while waiting for a process to exit.
const KILL_POLL_INTERVAL_MS: u64 = 300;

/// Normalise a process image name: strip `.exe` if present (case-insensitive),
/// trim whitespace. Detection compares bare stems on both sides.
fn normalize_image_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.len() > 4 && trimmed[trimmed.len() - 4..].eq_ignore_ascii_case(".exe") {
        trimmed[..trimmed.len() - 4].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Decode bytes from Windows console output (GBK on Chinese Windows).
/// Falls back to UTF-8 if GBK decoding fails.
#[cfg(windows)]
fn decode_gbk(bytes: &[u8]) -> String {
    let (cow, _, had_errors) = encoding_rs::GBK.decode(bytes);
    if had_errors {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        cow.into_owned()
    }
}

#[cfg(windows)]
fn hidden_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Launch an external application by path with optional arguments.
///
/// First checks if the file exists, then uses `ShellExecute` for UAC elevation support.
/// Returns detailed error messages for common failure cases.
#[tauri::command]
pub async fn launch_app(path: String, args: Option<String>) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let task = tokio::task::spawn_blocking(move || launch_app_inner(path, args));
        task.await.map_err(|e| format!("启动任务失败: {}", e))?
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (path, args);
        Err("当前系统不支持启动应用".into())
    }
}

#[cfg(target_os = "windows")]
fn launch_app_inner(path: String, args: Option<String>) -> Result<String, String> {
    {
        use windows::core::{PCWSTR, w};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use std::path::Path;

        let path = path.trim();
        if path.is_empty() {
            return Err("可执行文件路径不能为空".into());
        }

        // Check if file exists first to avoid "Open with" dialog
        let path_obj = Path::new(path);
        if !path_obj.exists() {
            return Err(format!("文件不存在: {}", path));
        }
        if !path_obj.is_file() {
            return Err(format!("路径不是文件: {}", path));
        }

        // Convert to wide strings for ShellExecute (UTF-16 + null terminator)
        let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let args_str = args.as_deref().unwrap_or("");
        let args_wide: Vec<u16> = args_str.encode_utf16().chain(std::iter::once(0)).collect();

        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(path_wide.as_ptr()),
                PCWSTR(args_wide.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };

        // ShellExecute returns instance handle > 32 on success
        if result.0 as usize > 32 {
            Ok(format!("已启动 ({})", path))
        } else {
            Err(format!("启动应用失败: ShellExecute 错误代码 {}", result.0 as usize))
        }
    }
}

/// Kill a process by image name. First tries a soft kill (`taskkill /IM`),
/// waits up to 3 s, then escalates to `taskkill /F /IM` if still running.
#[tauri::command]
pub async fn kill_app(process_name: String) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let task = tokio::task::spawn_blocking(move || kill_app_inner(process_name));
        task.await.map_err(|e| format!("关闭任务失败: {}", e))?
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = process_name;
        Err("当前系统不支持结束进程".into())
    }
}

#[cfg(target_os = "windows")]
fn kill_app_inner(process_name: String) -> Result<String, String> {
    {
        let image = normalize_image_name(&process_name);
        if image.is_empty() {
            return Err("进程名不能为空".into());
        }
        // taskkill /IM matches the exact image name — the ".exe" suffix is
        // mandatory (a bare stem yields "没有找到进程" and kills nothing).
        let image_arg = format!("{}.exe", image);

        if !is_app_running_inner(&image) {
            return Ok(format!("{} 未在运行", image));
        }

        // Soft kill first so the app can flush state on exit.
        let _ = hidden_command("taskkill")
            .args(["/IM", &image_arg])
            .output();

        let deadline = Instant::now() + Duration::from_millis(SOFT_KILL_TIMEOUT_MS);
        while is_app_running_inner(&image) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(KILL_POLL_INTERVAL_MS));
        }

        if is_app_running_inner(&image) {
            // Force kill as last resort.
            let output = hidden_command("taskkill")
                .args(["/F", "/IM", &image_arg])
                .output()
                .map_err(|e| format!("执行 taskkill 失败: {}", e))?;

            if !output.status.success() {
                let stderr = decode_gbk(&output.stderr);
                let stdout = decode_gbk(&output.stdout);
                let combined = format!("{} {}", stdout, stderr).to_ascii_lowercase();
                if combined.contains("not found")
                    || combined.contains("找不到")
                    || combined.contains("没有")
                {
                    invalidate_tasklist_cache();
                    return Ok(format!("{} 已关闭", image));
                }
                return Err(format!("强制结束 {} 失败: {}", image, stderr.trim()));
            }
        }

        invalidate_tasklist_cache();
        Ok(format!("已关闭 {}", image))
    }
}

/// Check whether a process with the given image name is currently running.
#[tauri::command]
pub async fn is_app_running(process_name: String) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        let task = tokio::task::spawn_blocking(move || {
            let image = normalize_image_name(&process_name);
            if image.is_empty() {
                return Err("进程名不能为空".into());
            }
            Ok(is_app_running_inner(&image))
        });
        task.await.map_err(|e| format!("检测任务失败: {}", e))?
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = process_name;
        Err("当前系统不支持进程检测".into())
    }
}

/// Internal: run `tasklist /FI "IMAGENAME eq <name>.exe"` and check for a match.
/// The filter needs the `.exe` suffix — querying a bare stem always misses.
#[cfg(windows)]
fn is_app_running_inner(image: &str) -> bool {
    let output = match hidden_command("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {}.exe", image), "/NH"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };

    if !output.status.success() {
        return false;
    }

    let stdout = decode_gbk(&output.stdout);

    // Check for "no tasks" messages in different languages
    let no_tasks_patterns = [
        "No tasks are running",
        "没有正在运行的任务",
        "No tasks",
        "没有",
        "INFO:",
    ];

    for pattern in &no_tasks_patterns {
        if stdout.contains(pattern) {
            return false;
        }
    }

    // If we got here and output is not empty, the process is running.
    // tasklist output for a running process looks like:
    // chrome.exe           12345 Console      1     123,456 K
    let trimmed = stdout.trim();
    !trimmed.is_empty()
}

/// Internal: check multiple process names with a single `tasklist` call.
/// Results are cached for ~800ms so launch-polling and concurrent cards
/// don't spawn a `tasklist` process per check.
#[cfg(windows)]
static TASKLIST_CACHE: std::sync::Mutex<Option<(Instant, Vec<String>)>> = std::sync::Mutex::new(None);

/// Drop the cached tasklist snapshot (call after kills).
#[cfg(windows)]
fn invalidate_tasklist_cache() {
    if let Ok(mut guard) = TASKLIST_CACHE.lock() {
        *guard = None;
    }
}

#[cfg(windows)]
fn is_apps_running_batch(images: &[String]) -> Vec<bool> {
    let snapshot: Vec<String> = match TASKLIST_CACHE.lock() {
        Ok(mut guard) => {
            if let Some((at, names)) = guard.as_ref() {
                if at.elapsed() < Duration::from_millis(800) {
                    return match_names_from_snapshot(images, names);
                }
            }
            match collect_tasklist_names() {
                Some(names) => { *guard = Some((Instant::now(), names.clone())); names }
                None => return vec![false; images.len()],
            }
        }
        Err(_) => match collect_tasklist_names() {
            Some(names) => names,
            None => return vec![false; images.len()],
        },
    };
    match_names_from_snapshot(images, &snapshot)
}

#[cfg(windows)]
fn collect_tasklist_names() -> Option<Vec<String>> {
    let output = hidden_command("tasklist").args(["/NH"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = decode_gbk(&output.stdout);
    Some(parse_tasklist_lines(&stdout))
}

/// Parse `tasklist /NH` output into lowercase image-name stems (`.exe` stripped)
/// so they compare equal to `normalize_image_name` outputs. Names containing
/// spaces are quoted in the table format — take the quoted part whole.
fn parse_tasklist_lines(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            if let Some(rest) = line.strip_prefix('"') {
                rest.split('"').next()
            } else {
                line.split_whitespace().next()
            }
        })
        .map(|s| normalize_image_name(s).to_lowercase())
        .collect()
}

#[cfg(windows)]
fn match_names_from_snapshot(images: &[String], snapshot: &[String]) -> Vec<bool> {
    images
        .iter()
        .map(|img| snapshot.iter().any(|p| p == &img.to_lowercase()))
        .collect()
}

/// Check whether multiple processes are running in a single call.
/// `force` bypasses the ~800ms snapshot cache (use after kills).
#[tauri::command]
pub async fn check_apps_running(process_names: Vec<String>, force: Option<bool>) -> Result<Vec<bool>, String> {
    #[cfg(windows)]
    {
        if process_names.is_empty() {
            return Ok(vec![]);
        }
        if force.unwrap_or(false) {
            invalidate_tasklist_cache();
        }
        let task = tokio::task::spawn_blocking(move || {
            let normalized: Vec<String> = process_names
                .iter()
                .map(|s| normalize_image_name(s))
                .collect();
            is_apps_running_batch(&normalized)
        });
        Ok(task.await.map_err(|e| format!("检测任务失败: {}", e))?)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (process_names, force);
        Err("当前系统不支持进程检测".into())
    }
}

/// Extract the icon from an executable file and return it as a base64-encoded PNG.
///
/// Uses `ExtractIconExW` to get the HICON, then converts it to a bitmap
/// using GDI, and finally encodes it as a PNG image.
#[tauri::command]
pub async fn extract_app_icon(path: String) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let task = tokio::task::spawn_blocking(move || extract_app_icon_sync(path));
        task.await.map_err(|e| format!("图标任务失败: {}", e))?
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        Err("当前系统不支持图标提取".into())
    }
}

/// Read the file version of an executable from its Win32 version resource.
/// Runs on the blocking pool — reading the version resource touches disk.
#[tauri::command]
pub async fn get_app_version(path: String) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        let task = tokio::task::spawn_blocking(move || get_app_version_sync(path));
        task.await.map_err(|e| format!("版本任务失败: {}", e))?
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        Err("当前系统不支持读取版本".into())
    }
}

/// Format a VS_FIXEDFILEINFO ms/ls pair into a dotted version, trimming trailing
/// zero groups but keeping at least two components (e.g. 1.2.3.0 -> "1.2.3").
fn format_version_parts(ms: u32, ls: u32) -> String {
    let parts = [
        (ms >> 16) & 0xffff,
        ms & 0xffff,
        (ls >> 16) & 0xffff,
        ls & 0xffff,
    ];
    let mut n = 4;
    while n > 2 && parts[n - 1] == 0 {
        n -= 1;
    }
    parts[..n].iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".")
}

/// Sync version read, safe to call from `spawn_blocking` (e.g. batch import).
#[cfg(target_os = "windows")]
fn get_app_version_sync(path: String) -> Result<String, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };

    let path = path.trim();
    if path.is_empty() {
        return Err("路径不能为空".into());
    }
    if !std::path::Path::new(path).is_file() {
        return Ok(String::new());
    }
    let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path_wide.as_ptr()), None) } as usize;
    if size == 0 {
        return Ok(String::new());
    }
    let mut buf: Vec<u8> = vec![0; size];
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(path_wide.as_ptr()),
            None,
            size as u32,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
        )
    }
    .map_err(|e| format!("读取版本失败: {}", e))?;

    // Root sub-block "\\" -> VS_FIXEDFILEINFO.
    let root_wide: Vec<u16> = "\\".encode_utf16().chain(std::iter::once(0)).collect();
    let mut info: *mut VS_FIXEDFILEINFO = std::ptr::null_mut();
    let mut len: u32 = 0;
    let ok = unsafe {
        VerQueryValueW(
            buf.as_ptr() as *const core::ffi::c_void,
            PCWSTR(root_wide.as_ptr()),
            &mut info as *mut _ as *mut *mut core::ffi::c_void,
            &mut len,
        )
    };
    if !ok.as_bool() || info.is_null() {
        return Ok(String::new());
    }
    let ffi = unsafe { &*info };
    Ok(format_version_parts(ffi.dwFileVersionMS, ffi.dwFileVersionLS))
}

/// Sync icon extraction, safe to call from `spawn_blocking` contexts (e.g. the scan).
///
/// Prefers the exe's own PE icon resources: `ExtractIconExW` only ever hands
/// back the system size (32x32) and takes group 0 blindly, which is a *blank*
/// placeholder group for e.g. git-bash.exe. Reading RT_GROUP_ICON lets us skip
/// blank groups and take the largest real artwork at up to 64px.
#[cfg(target_os = "windows")]
fn extract_app_icon_sync(path: String) -> Result<String, String> {
    use windows::core::PCWSTR;
    use std::path::Path;

    let path = path.trim();
    if path.is_empty() {
        return Err("路径不能为空".into());
    }
    let path_obj = Path::new(path);
    if !path_obj.exists() || !path_obj.is_file() {
        return Err("文件不存在或不是文件".into());
    }
    let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    match unsafe { extract_icon_pe_png(PCWSTR(path_wide.as_ptr())) } {
        Ok(Some(png)) => return Ok(base64_encode(&png)),
        // The file carries no icon resource at all (OpenAI's codex.exe, rustup.exe):
        // stop here. The shell fallback would hand back the generic
        // "unknown application" bitmap, which reads as a broken icon — the
        // frontend's letter tile is the honest answer.
        Ok(None) => return Err("文件不含图标资源".into()),
        Err(_) => {}
    }

    // Legacy path: ExtractIconExW group 0, owned handle, destroyed by renderer.
    let legacy = (|| -> Result<Vec<u8>, String> {
        unsafe {
            let mut large = windows::Win32::UI::WindowsAndMessaging::HICON::default();
            let mut small = windows::Win32::UI::WindowsAndMessaging::HICON::default();
            let count = windows::Win32::UI::Shell::ExtractIconExW(
                PCWSTR(path_wide.as_ptr()),
                0,
                Some(&mut large),
                Some(&mut small),
                1,
            );
            if !small.0.is_null() {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(small);
            }
            if count == 0 || large.0.is_null() {
                return Err("无法提取图标".into());
            }
            render_icon_to_png(large, true)
        }
    })();
    if let Ok(png) = legacy {
        return Ok(base64_encode(&png));
    }

    // Last resort: ask the shell for the icon Explorer itself would display.
    if let Ok(png) = unsafe { extract_icon_via_shell(PCWSTR(path_wide.as_ptr())) } {
        return Ok(base64_encode(&png));
    }

    Err("无法提取图标".into())
}

/// The icon the shell/Explorer shows for this file (32x32). Resolves
/// PNG-compressed and otherwise GDI-undecodable icon members that both
/// `LoadImageW` and `ExtractIconExW` render as blank (7zFM.exe).
#[cfg(target_os = "windows")]
unsafe fn extract_icon_via_shell(path_wide: windows::core::PCWSTR) -> Result<Vec<u8>, String> {
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::HICON;

    let mut info = SHFILEINFOW::default();
    // Caller owns the returned hIcon and must destroy it.
    if SHGetFileInfoW(
        path_wide,
        Default::default(),
        Some(&mut info),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_LARGEICON,
    ) == 0
        || info.hIcon.0.is_null()
    {
        return Err("shell 未取得图标".into());
    }
    // render_icon_to_png(owns = true) destroys the icon on every path.
    render_icon_to_png(HICON(info.hIcon.0), true)
}

/// Enumerate the file's RT_GROUP_ICON resources (loaded as datafile, no code
/// runs) and rasterise the biggest group that GDI can actually draw — all
/// while the module is still mapped.
///
/// Groups are tried largest-first and the first non-blank raster wins: the
/// largest entry is frequently a PNG-compressed 256px image that `LoadImageW`
/// silently renders as nothing (EXCEL.EXE, 7zFM.exe), so ranking alone is not
/// enough. Two aliasing traps this ordering avoids: `LR_SHARED` hands back a
/// system-cached handle that collides across modules (several unrelated exes
/// rendered as one identical icon), and freeing the module before drawing
/// leaves the image data unmapped. So: load owned, render, then free.
///
/// `Ok(Some(_))` = rasterised; `Ok(None)` = the file has no icon groups at all
/// (callers must not fall back to the shell's generic bitmap); `Err` = it has
/// groups but GDI could not draw them, so a shell retry is worthwhile.
#[cfg(target_os = "windows")]
#[derive(Default)]
struct EnumState {
    /// Every RT_GROUP_ICON name seen, integer or string.
    seen: usize,
    /// Groups we could rank by integer ID: (largest member area, id).
    groups: Vec<(u32, u16)>,
}

#[cfg(target_os = "windows")]
unsafe fn extract_icon_pe_png(
    path_wide: windows::core::PCWSTR,
) -> Result<Option<Vec<u8>>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{FreeLibrary, HINSTANCE, HMODULE};
    use windows::Win32::System::LibraryLoader::{
        EnumResourceNamesW, LoadLibraryExW, DONT_RESOLVE_DLL_REFERENCES,
        LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{HICON, IMAGE_ICON, LoadImageW, RT_GROUP_ICON};

    extern "system" fn collect_groups(
        hModule: HMODULE,
        _resType: PCWSTR,
        resName: PCWSTR,
        out_state: isize,
    ) -> windows::core::BOOL {
        unsafe {
            let state = &mut *(out_state as *mut EnumState);
            // Counted before the integer-ID filter: string-named groups
            // (MAINICON etc.) are still proof the file HAS icon artwork, even
            // though this enumerator can't rank them by ID.
            state.seen += 1;
            if (resName.0 as usize) > 0xffff {
                return true.into();
            }
            if let Some(candidate) = rank_icon_group(hModule, resName.0 as u16) {
                if !state.groups.iter().any(|(_, id)| *id == candidate.1) {
                    state.groups.push(candidate);
                }
            }
            true.into()
        }
    }

    let module = LoadLibraryExW(
        path_wide,
        None,
        DONT_RESOLVE_DLL_REFERENCES | LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
    )
    .map_err(|_| "无法加载可执行文件资源".to_string())?;

    let mut state = EnumState::default();
    let ok = EnumResourceNamesW(
        Some(module),
        RT_GROUP_ICON,
        Some(collect_groups),
        &mut state as *mut _ as isize,
    );
    if !ok.as_bool() || state.seen == 0 || state.groups.is_empty() {
        let _ = FreeLibrary(module);
        return if state.seen == 0 { Ok(None) } else { Err("没有可排序的图标组".into()) };
    }
    let mut groups = state.groups;
    groups.sort_by(|a, b| b.0.cmp(&a.0));

    let mut last_err = "加载图标失败".to_string();
    let mut rendered = None;
    for &(_, group_id) in &groups {
        // No LR_SHARED → we own the handle and the renderer destroys it.
        match LoadImageW(
            Some(HINSTANCE(module.0)),
            PCWSTR::from_raw(group_id as *const u16),
            IMAGE_ICON,
            64,
            64,
            Default::default(),
        ) {
            Ok(h) if !h.0.is_null() => match render_icon_to_png(HICON(h.0), true) {
                Ok(png) => {
                    rendered = Some(png);
                    break;
                }
                Err(e) => last_err = e,
            },
            _ => {}
        }
    }
    let _ = FreeLibrary(module);
    match rendered {
        Some(png) => Ok(Some(png)),
        None => Err(last_err),
    }
}

/// Read one RT_GROUP_ICON resource and report (largest member area, group id).
/// Groups whose members are all 0x0 placeholders rank as 0 and lose against
/// any real artwork.
#[cfg(target_os = "windows")]
unsafe fn rank_icon_group(
    module: windows::Win32::Foundation::HMODULE,
    group_id: u16,
) -> Option<(u32, u16)> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HRSRC;
    use windows::Win32::System::LibraryLoader::{FindResourceW, LoadResource, LockResource, SizeofResource};
    use windows::Win32::UI::WindowsAndMessaging::RT_GROUP_ICON;

    let hsrc = FindResourceW(Some(module), PCWSTR::from_raw(group_id as *const u16), RT_GROUP_ICON);
    if hsrc == HRSRC::default() {
        return None;
    }
    let hres = LoadResource(Some(module), hsrc).ok()?;
    let size = SizeofResource(Some(module), hsrc) as usize;
    let ptr = LockResource(hres) as *const u8;
    if ptr.is_null() || size < 6 {
        return None;
    }
    let data = std::slice::from_raw_parts(ptr, size);
    let count = u16::from_le_bytes([data[4], data[5]]) as usize;
    let mut area: u32 = 0;
    for e in data[6..].chunks_exact(14).take(count) {
        let w = e[0] as u32 % 257; // 0 means 256
        let h = e[1] as u32 % 257;
        area = area.max(w * h);
    }
    if area == 0 {
        None
    } else {
        Some((area, group_id))
    }
}

/// Rasterise an HICON to PNG bytes via a 32-bit DIB section (keeps alpha on
/// every display depth, unlike CreateCompatibleBitmap). `owns` controls
/// whether the icon handle is destroyed afterwards (LR_SHARED: never).
#[cfg(target_os = "windows")]
unsafe fn render_icon_to_png(
    icon: windows::Win32::UI::WindowsAndMessaging::HICON,
    owns: bool,
) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDIBits,
        GetObjectW, HGDIOBJ, ReleaseDC, SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, DrawIconEx, GetIconInfo, ICONINFO, DI_NORMAL,
    };

    let mut icon_info = ICONINFO::default();
    if GetIconInfo(icon, &mut icon_info).is_err() {
        if owns {
            let _ = DestroyIcon(icon);
        }
        return Err("获取图标信息失败".into());
    }
    // GetIconInfo hands back two separate bitmaps: the colour one is the icon
    // as-is; a mask-only icon (monochrome) is double-stacked, halve that one.
    let probe = if icon_info.hbmColor.0.is_null() { icon_info.hbmMask } else { icon_info.hbmColor };
    if probe.0.is_null() {
        if owns {
            let _ = DestroyIcon(icon);
        }
        return Err("图标无位图数据".into());
    }
    let mut bmp = BITMAP::default();
    let ok = GetObjectW(
        probe.into(),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bmp as *mut _ as *mut std::ffi::c_void),
    );
    let (width, height) = if ok == 0 {
        (0, 0)
    } else if icon_info.hbmColor.0.is_null() {
        (bmp.bmWidth, bmp.bmHeight / 2)
    } else {
        (bmp.bmWidth, bmp.bmHeight)
    };
    for bmp in [icon_info.hbmColor, icon_info.hbmMask] {
        if !bmp.0.is_null() {
            let _ = DeleteObject(bmp.into());
        }
    }
    if width <= 0 || height <= 0 {
        if owns {
            let _ = DestroyIcon(icon);
        }
        return Err("获取图标尺寸失败".into());
    }

    let hdc_screen = GetDC(Some(HWND::default()));
    if hdc_screen.0.is_null() {
        if owns {
            let _ = DestroyIcon(icon);
        }
        return Err("获取屏幕DC失败".into());
    }
    let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
    if hdc_mem.0.is_null() {
        let _ = ReleaseDC(Some(HWND::default()), hdc_screen);
        if owns {
            let _ = DestroyIcon(icon);
        }
        return Err("创建内存DC失败".into());
    }

    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = width;
    bmi.bmiHeader.biHeight = -height; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB.0;

    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let hbitmap = match CreateDIBSection(Some(hdc_mem), &raw const bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) {
        Ok(hb) if !hb.0.is_null() && !bits_ptr.is_null() => hb,
        _ => {
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(Some(HWND::default()), hdc_screen);
            if owns {
                let _ = DestroyIcon(icon);
            }
            return Err("创建32位位图失败".into());
        }
    };
    let old_bmp = SelectObject(hdc_mem, hbitmap.into());
    if old_bmp == HGDIOBJ::default() {
        let _ = DeleteObject(hbitmap.into());
        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(Some(HWND::default()), hdc_screen);
        if owns {
            let _ = DestroyIcon(icon);
        }
        return Err("选择位图失败".into());
    }

    let draw_result = DrawIconEx(hdc_mem, 0, 0, icon, width, height, 0, None, DI_NORMAL);
    let mut pixels: Vec<u8> = vec![0u8; (((width * 32 + 31) / 32) * 4 * height) as usize];
    let get_result = if draw_result.is_ok() {
        GetDIBits(
            hdc_mem,
            hbitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    } else {
        0
    };
    let _ = SelectObject(hdc_mem, old_bmp);
    let _ = DeleteObject(hbitmap.into());
    let _ = DeleteDC(hdc_mem);
    let _ = ReleaseDC(Some(HWND::default()), hdc_screen);
    if owns {
        let _ = DestroyIcon(icon);
    }
    if get_result == 0 {
        return Err("读取图标像素失败".into());
    }

    // BGRA (top-down) → RGBA.
    let row_size = ((width * 32 + 31) / 32) * 4;
    let mut rgba: Vec<u8> = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let src_row = (y * row_size) as usize;
        for x in 0..width {
            let src = src_row + (x * 4) as usize;
            let (b, g, r, a) = (pixels[src], pixels[src + 1], pixels[src + 2], pixels[src + 3]);
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }

    // Reject a blank raster. GDI cannot decode PNG-compressed icon members (the
    // 256px entry most Office/7-Zip groups rank as largest), and the failed
    // decode comes back as a fully transparent bitmap — a valid PNG that renders
    // as nothing. Callers use the Err to fall through to the next candidate.
    let opaque = rgba.chunks_exact(4).filter(|p| p[3] > 24).count();
    if opaque * 200 < rgba.len() / 4 {
        return Err("图标为空白".into());
    }
    encode_png(&rgba, width, height)
}

/// Encode RGBA pixel data as a minimal PNG file.
///
/// This is a simple PNG encoder that produces a valid PNG without external crates.
/// It uses zlib compression via the `flate2` crate (already a dependency of tauri).
#[cfg(windows)]
fn encode_png(rgba: &[u8], width: i32, height: i32) -> Result<Vec<u8>, String> {
    use std::io::Write;

    let mut out = Vec::new();

    // PNG signature.
    out.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

    // IHDR chunk.
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // Bit depth
    ihdr.push(6); // Color type: RGBA
    ihdr.push(0); // Compression
    ihdr.push(0); // Filter
    ihdr.push(0); // Interlace
    write_chunk(&mut out, b"IHDR", &ihdr)?;

    // IDAT chunk — compress the raw scanlines with filter byte 0 (None).
    let mut raw = Vec::with_capacity(((width * 4 + 1) * height) as usize);
    for y in 0..height {
        raw.push(0); // Filter: None
        let row_start = (y * width * 4) as usize;
        raw.extend_from_slice(&rgba[row_start..row_start + (width * 4) as usize]);
    }

    let mut compressed = Vec::new();
    {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        let mut encoder = ZlibEncoder::new(&mut compressed, Compression::default());
        encoder.write_all(&raw).map_err(|e| format!("PNG压缩失败: {}", e))?;
        encoder.finish().map_err(|e| format!("PNG压缩失败: {}", e))?;
    }
    write_chunk(&mut out, b"IDAT", &compressed)?;

    // IEND chunk.
    write_chunk(&mut out, b"IEND", &[])?;

    Ok(out)
}

/// Write a PNG chunk with length, type, data, and CRC.
#[cfg(windows)]
fn write_chunk(out: &mut Vec<u8>, chunk_type: &[u8], data: &[u8]) -> Result<(), String> {
    // Length (big-endian).
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    // Type + data.
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    // CRC32 of type + data.
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(chunk_type);
    hasher.update(data);
    let crc = hasher.finalize();
    out.extend_from_slice(&crc.to_be_bytes());
    Ok(())
}

/// Base64-encode bytes to a string.
#[cfg(windows)]
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Open a native file picker to browse for an executable file.
///
/// Returns the selected path as a string, or an error if the user cancels.
#[tauri::command]
pub async fn browse_app_file() -> Result<String, String> {
    #[cfg(windows)]
    {
        let task = tokio::task::spawn_blocking(|| {
            use rfd::FileDialog;

            let dialog = FileDialog::new()
                .set_title("选择应用")
                .add_filter("可执行文件", &["exe"]);

            match dialog.pick_file() {
                Some(path) => Ok(path.to_string_lossy().to_string()),
                None => Err("未选择文件".into()),
            }
        });
        task.await.map_err(|e| format!("文件对话框任务失败: {}", e))?
    }

    #[cfg(not(windows))]
    {
        Err("当前系统不支持文件浏览".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_image_name_strips_exe() {
        assert_eq!(normalize_image_name("Cursor.exe"), "Cursor");
        assert_eq!(normalize_image_name("notepad.exe"), "notepad");
        assert_eq!(normalize_image_name("my app"), "my app");
        assert_eq!(normalize_image_name("  spaced.exe  "), "spaced");
        assert_eq!(normalize_image_name("WXWork.EXE"), "WXWork");
    }

    #[test]
    fn tasklist_lines_parse_to_bare_lowercase_stems() {
        let stdout = "WXWork.exe                    8424 Console                    1    320,716 K\n\"Microsoft Edge\"                  12345 Console                    1    100,000 K\nexplorer.exe                   9644 Console                    1    155,644 K\n";
        let names = parse_tasklist_lines(stdout);
        assert_eq!(names, vec!["wxwork", "microsoft edge", "explorer"]);
        // The batch query side normalizes the same way — both sides must agree.
        let query = normalize_image_name("WXWork.exe").to_lowercase();
        assert!(names.contains(&query));
    }

    #[test]
    fn pick_main_exe_matches_across_separator_differences() {
        // "CC Switch" (registry DisplayName) vs cc-switch.exe on disk.
        let exes = vec![cand("uninstall"), cand("cc-switch")];
        assert_eq!(
            pick_main_exe(&exes, Some("CC Switch")).as_deref(),
            Some("C:\\app\\cc-switch.exe")
        );
        let underscore = vec![cand("my_tool")];
        assert_eq!(
            pick_main_exe(&underscore, Some("My Tool")).as_deref(),
            Some("C:\\app\\my_tool.exe")
        );
    }

    #[test]
    fn uninstall_string_yields_install_dir() {
        assert_eq!(
            dir_from_uninstall_string("\"D:\\w_app\\Tencent\\QQNT\\Uninstall.exe\"").as_deref(),
            Some("D:\\w_app\\Tencent\\QQNT")
        );
        assert_eq!(
            dir_from_uninstall_string("C:\\Program Files\\App\\unins000.exe /SILENT").as_deref(),
            Some("C:\\Program Files\\App")
        );
        // MsiExec-based uninstallers have no install dir to recover.
        assert_eq!(dir_from_uninstall_string("MsiExec.exe /X{1F1C2DFD-2BB2-4243-9AB5-10174A7C6998}"), None);
    }

    #[test]
    fn batch_match_is_extension_and_case_insensitive() {
        let snapshot = parse_tasklist_lines("CBbot.exe                    26548 Console                    1    173,776 K\n");
        let results = match_names_from_snapshot(&["CBbot".to_string(), "nosuch".to_string()], &snapshot);
        assert_eq!(results, vec![true, false]);
    }

    fn cand(stem: &str) -> (String, String) {
        (stem.to_string(), format!("C:\\app\\{stem}.exe"))
    }

    #[test]
    fn pick_main_exe_exact_match_wins() {
        let exes = vec![cand("chrome"), cand("chrome_proxy")];
        assert_eq!(
            pick_main_exe(&exes, Some("Chrome")).as_deref(),
            Some("C:\\app\\chrome.exe")
        );
    }

    #[test]
    fn pick_main_exe_prefix_match_with_display_name() {
        let exes = vec![cand("chrome"), cand("setup")];
        assert_eq!(
            pick_main_exe(&exes, Some("Google Chrome")).as_deref(),
            Some("C:\\app\\chrome.exe")
        );
    }

    #[test]
    fn pick_main_exe_skips_auxiliary_exes() {
        let exes = vec![cand("uninstall"), cand("update"), cand("MyAppElevated"), cand("myapp")];
        assert_eq!(
            pick_main_exe(&exes, Some("MyApp")).as_deref(),
            Some("C:\\app\\myapp.exe")
        );
    }

    #[test]
    fn pick_main_exe_never_guesses_when_nothing_matches() {
        let exes = vec![cand("thing1"), cand("thing2")];
        assert_eq!(pick_main_exe(&exes, Some("Unrelated Product")), None);
    }

    #[test]
    fn pick_main_exe_single_candidate_without_name_is_safe() {
        let exes = vec![cand("onlyone")];
        assert_eq!(
            pick_main_exe(&exes, None).as_deref(),
            Some("C:\\app\\onlyone.exe")
        );
        let many = vec![cand("a"), cand("b")];
        assert_eq!(pick_main_exe(&many, None), None);
    }

    #[test]
    fn auxiliary_filter_rejects_real_world_noise() {
        for stem in [
            "mdnsresponder", "msmpeng", "nissrv", "wmplayer", "wslhost", "wslservice",
            "maintenanceservice", "crashdump32", "crashrpt", "plugin-container",
            "pingsender", "nmhproxy", "yunshu dpengine", "ccfilewatermark",
            "onedrivestandaloneupdater", "expediteupdater", "uhssvc",
            "pdfaccountsvr", "kdumprepn", "fastpdf", "picpreview", "env_detect",
            "extexport", "ieinstal", "ielowutil", "wabmig", "setup_wm",
            "msal.wsl.proxy", "microsoft.applicationidentity", "qtwebengineprocess",
            "yunshudiagnosis", "yunshurepair", "i4service",
        ] {
            assert!(is_auxiliary_exe_stem(stem), "should be noise: {}", stem);
        }
    }

    #[test]
    fn auxiliary_filter_keeps_real_apps() {
        for stem in [
            "chrome", "firefox", "code", "cursor", "wechat", "wxwork", "qq", "dingtalk",
            "telegram", "discord", "notepad++", "dism++", "potplayer", "idaviewer0703",
            "yunshu", "aone", "i4tools", "wpspdf", "wpsoffice", "msedge", "mstsc",
            "mspaint", "iexplore", "deepl", "es-plus", "everything",
        ] {
            assert!(!is_auxiliary_exe_stem(stem), "should be app: {}", stem);
        }
    }

    #[test]
    fn blocked_dirs_cover_windows_builtins_and_components() {
        for dir in [
            "windows defender", "internet explorer", "wsl", "common files",
            "mozilla maintenance service", "microsoft update health tools",
            "windows mail", "windows media player", "windows photo viewer",
            "bonjour", "yunshu plugin", ".pnpm-store",
        ] {
            assert!(is_blocked_program_dir(dir), "should be blocked: {}", dir);
        }
        assert!(!is_blocked_program_dir("mozilla firefox"));
        assert!(!is_blocked_program_dir("tencent"));
        assert!(!is_blocked_program_dir("microsoft onedrive"));
    }

    #[test]
    fn start_menu_filter_accepts_apps_rejects_housekeeping() {
        assert!(is_valid_start_menu_target("d:\\w_app\\7-zip\\7zfm.exe"));
        assert!(is_valid_start_menu_target("c:\\program files\\google\\chrome\\application\\chrome.exe"));
        assert!(is_valid_start_menu_target("d:\\w_app\\grok bot\\grok bot.exe"));
        assert!(!is_valid_start_menu_target("c:\\windows\\system32\\cleanmgr.exe"));
        assert!(!is_valid_start_menu_target("c:\\programdata\\package cache\\{guid}\\vcredist_x64.exe"));
        assert!(!is_valid_start_menu_target("d:\\app\\unins000.exe"));
        assert!(!is_valid_start_menu_target("d:\\app\\help.exe"));
        assert!(!is_valid_start_menu_target("d:\\app\\readme.txt"));
        // Confirmed Start Menu noise on this machine.
        assert!(!is_valid_start_menu_target("c:\\program files\\mozilla firefox\\private_browsing.exe"));
        assert!(!is_valid_start_menu_target("c:\\program files\\google\\chrome\\application\\chrome_proxy.exe"));
        assert!(!is_valid_start_menu_target("c:\\program files\\microsoft office\\root\\office16\\setlang.exe"));
        assert!(!is_valid_start_menu_target("c:\\program files\\java\\jre1.8.0_45\\bin\\javacpl.exe"));
        assert!(!is_valid_start_menu_target("c:\\program files (x86)\\windows kits\\10\\app certification kit\\appcertui.exe"));
        assert!(!is_valid_start_menu_target("d:\\app\\autoupdate.exe"));
    }

    #[test]
    fn start_menu_rejects_housekeeping_shortcut_names() {
        assert!(is_start_menu_noise_display("Uninstall"));
        assert!(is_start_menu_noise_display("卸载 腾讯电脑管家"));
        assert!(is_start_menu_noise_display("关于 Java"));
        assert!(!is_start_menu_noise_display("Google Chrome"));
        assert!(!is_start_menu_noise_display("CC Switch"));
    }

    /// Build a minimal MS-SHLLINK blob: 76-byte header + optional ItemIDList +
    /// LinkInfo whose LocalBasePath (ANSI/OEM, GBK-encoded) sits at offset 20.
    fn lnk_blob(with_idlist: bool, target: &str) -> Vec<u8> {
        let mut b = vec![0u8; 76];
        b[0..4].copy_from_slice(&0x0000_004Cu32.to_le_bytes()); // signature
        let mut flags = 0x02u32; // HAS_LINK_INFO + IS_UNICODE (parser ignores unicode path)
        if with_idlist {
            flags |= 0x01; // HAS_IDLIST
        }
        flags |= 0x80;
        b[20..24].copy_from_slice(&flags.to_le_bytes());
        if with_idlist {
            // ItemIDSize counts its own 2-byte header: total 8 (2 hdr + 6 data), then terminator.
            b.extend_from_slice(&8u16.to_le_bytes());
            b.extend_from_slice(&[0u8; 6]);
            b.extend_from_slice(&0u16.to_le_bytes());
        }
        // LinkInfo base header (20 bytes): LocalBasePathOffset(ANSI) at [0x0C].
        let li = b.len();
        let base_off = 20u32;
        let mut li_hdr = vec![0u8; 20];
        li_hdr[0..4].copy_from_slice(&21u32.to_le_bytes()); // BlockSize
        li_hdr[12..16].copy_from_slice(&base_off.to_le_bytes());
        b.extend_from_slice(&li_hdr);
        let _ = li;
        // Path string, GBK-encoded (matches the parser's OEM decode).
        let (gbk, _, _) = encoding_rs::GBK.encode(target);
        b.extend_from_slice(&gbk);
        b.push(0);
        b
    }

    #[test]
    fn parse_lnk_reads_ansi_gbk_and_rejects_garbage() {
        assert_eq!(
            parse_lnk_local_path(&lnk_blob(false, r"C:\Program Files\App\app.exe")),
            Some(r"C:\Program Files\App\app.exe".to_string())
        );
        // With a LinkTargetIDList present (real .lnk files always carry one):
        // walking past each ItemID (size counts its own header) must land on LinkInfo.
        assert_eq!(
            parse_lnk_local_path(&lnk_blob(true, r"C:\Apps\Chrome\chrome.exe")),
            Some(r"C:\Apps\Chrome\chrome.exe".to_string())
        );
        // Chinese path round-trips through the GBK decode.
        assert_eq!(
            parse_lnk_local_path(&lnk_blob(true, r"D:\软件\网易云音乐\cloudmusic.exe")),
            Some(r"D:\软件\网易云音乐\cloudmusic.exe".to_string())
        );
        // Too short, bad signature.
        assert_eq!(parse_lnk_local_path(&[0u8; 10]), None);
        let mut bad = lnk_blob(false, "x");
        bad[0] = 0x99;
        assert_eq!(parse_lnk_local_path(&bad), None);
    }

    #[test]
    fn version_parts_trim_trailing_zeros() {
        assert_eq!(format_version_parts(0x0001_0002, 0x0003_0000), "1.2.3");
        assert_eq!(format_version_parts(0x0001_0002, 0x0000_0000), "1.2");
        assert_eq!(format_version_parts(0x0001_0000, 0x0000_0000), "1.0");
        assert_eq!(format_version_parts(0x0012_0034, 0x0056_0078), "18.52.86.120");
    }

    #[cfg(windows)]
    #[test]
    fn newest_version_exe_picks_highest_version_dir() {
        let base = std::env::temp_dir().join(format!("al_ver_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for v in ["2.1.280", "2.1.9", "2.1.281", "notaversion"] {
            let d = base.join(v);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("claude.exe"), b"x").unwrap();
        }
        // Numeric compare, not lexical: 2.1.281 must beat 2.1.9.
        let got = newest_version_exe(&base, "claude.exe").unwrap();
        assert!(got.ends_with(r"2.1.281\claude.exe"), "got {}", got);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn path_key_normalizes_case_separators_and_trailing_slash() {
        assert_eq!(normalize_path_key("C:/App/exe"), normalize_path_key("c:\\app\\exe"));
        assert_eq!(normalize_path_key("C:\\App\\"), normalize_path_key("c:\\app"));
    }

    /// Audit helper: run the real scan on this machine and dump results next to
    /// the Start Menu ground truth for offline diffing. `cargo test -- --ignored`.
    #[test]
    #[ignore = "audit-only: touches the real registry and disk"]
    fn dump_scan_results_for_audit() {
        let apps = scan_installed_apps_sync().unwrap();
        let lines: Vec<String> = apps.iter().map(|a| format!("{}\t{}", a.display_name, a.exe_path)).collect();
        std::fs::write("target/scan_dump.tsv", lines.join("\n")).unwrap();
        println!("scanned {} apps", apps.len());
    }

    /// Audit helper: run real icon extraction against known exes and write the
    /// PNGs next to the test binary so failures can be told apart (backend
    /// error vs. frontend render). `cargo test -- --ignored`.
    #[test]
    #[ignore = "audit-only: touches the real filesystem"]
    fn dump_icon_extraction_for_audit() {
        fn base64_decode(s: &str) -> Option<Vec<u8>> {
            const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut acc: u32 = 0;
            let mut bits = 0;
            let mut out = Vec::new();
            for c in s.bytes() {
                if c == b'=' {
                    break;
                }
                let v = CHARS.iter().position(|&x| x == c)? as u32;
                acc = (acc << 6) | v;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    out.push((acc >> bits) as u8);
                }
            }
            Some(out)
        }

        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let mut candidates: Vec<String> = vec![
            format!("{windir}\\System32\\notepad.exe"),
            format!("{windir}\\explorer.exe"),
            format!("{local}\\OpenAI\\Codex\\bin\\codex.exe"),
        ];
        // Every real scanned app the user's launcher list can contain.
        if let Ok(dump) = std::fs::read_to_string("target/scan_dump.tsv") {
            for line in dump.lines() {
                if let Some((_name, path)) = line.split_once('\t') {
                    candidates.push(path.to_string());
                }
            }
        }
        let mut lines = Vec::new();
        for (i, path) in candidates.iter().enumerate() {
            let verdict = match extract_app_icon_sync(path.clone()) {
                Ok(b64) => {
                    match base64_decode(&b64) {
                        Some(bytes) => {
                            let w = bytes.get(16..20).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
                            let h = bytes.get(20..24).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
                            std::fs::write(format!("target/icon_audit_{i}.png"), &bytes).ok();
                            format!("OK b64={} png={}x{} file={}B", b64.len(), w, h, bytes.len())
                        }
                        None => format!("OK-but-undecodable b64={}", b64.len()),
                    }
                }
                Err(e) => format!("ERR {e}"),
            };
            lines.push(format!("{path}\t{verdict}"));
        }
        std::fs::write("target/icon_audit.txt", lines.join("\n")).unwrap();
        println!("{}", lines.join("\n"));
    }
}

/// One discovered installed application.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstalledAppInfo {
    pub display_name: String,
    pub exe_path: String,
    pub publisher: String,
    pub version: String,
    pub icon: Option<String>,
}

/// Scan the registry for installed applications.
///
/// Reads both HKLM and HKCU uninstall keys (the 64-bit and 32-bit views)
/// and returns a deduplicated list of applications with their executable paths.
#[tauri::command]
pub async fn scan_installed_apps() -> Result<Vec<InstalledAppInfo>, String> {
    #[cfg(target_os = "windows")]
    {
        let results = tokio::task::spawn_blocking(scan_installed_apps_sync)
            .await
            .map_err(|e| format!("扫描任务失败: {}", e))??;
        Ok(results)
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("当前系统不支持扫描已安装应用".into())
    }
}

/// Synchronous body of the scan — split out so tests can run it directly.
#[cfg(windows)]
pub(crate) fn scan_installed_apps_sync() -> Result<Vec<InstalledAppInfo>, String> {
    let mut apps: Vec<InstalledAppInfo> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let hives = [
        (windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
        (windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
        (windows::Win32::System::Registry::HKEY_CURRENT_USER, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
    ];

    for (hive, path) in hives {
        if let Ok(subkeys) = enum_subkeys(hive, path) {
            for subkey in subkeys {
                let full_path = format!(r"{}\{}", path, subkey);
                if let Some(app) = read_app_info(hive, &full_path, &subkey) {
                    let key = normalize_path_key(&app.exe_path);
                    if !key.is_empty() && seen.insert(key) {
                        apps.push(app);
                    }
                }
            }
        }
    }

    // Also scan common install locations for portable / non-registry apps.
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        scan_program_dir(&std::path::PathBuf::from(program_files), &mut apps, &mut seen);
    }
    if let Some(program_files_x86) = std::env::var_os("ProgramFiles(x86)") {
        scan_program_dir(&std::path::PathBuf::from(program_files_x86), &mut apps, &mut seen);
    }
    // Per-user installs live under %LOCALAPPDATA%\Programs (Electron convention);
    // the rest of LOCALAPPDATA is caches and vendor components, not apps.
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        scan_program_dir(
            &std::path::PathBuf::from(local_app_data).join("Programs"),
            &mut apps,
            &mut seen,
        );
    }

    // Shortcuts (Start Menu + Desktop) last: they are the ground truth for what
    // the user can launch, covering other drives and deep layouts the dir scan can't see.
    scan_shortcut_apps(&mut apps, &mut seen);

    // A curated set of per-user CLI/dev tools that register no uninstall key and
    // ship no shortcut, so no standard "installed apps" source sees them.
    scan_cli_tool_apps(&mut apps, &mut seen);

    // Sort by display name for a stable list.
    apps.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    Ok(apps)
}

/// Recursively collect `*.lnk` files below `dir` (bounded depth).
#[cfg(windows)]
fn collect_lnk_files(dir: &std::path::Path, depth: u32, out: &mut Vec<std::path::PathBuf>) {
    if depth == 0 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_lnk_files(&p, depth - 1, out);
            } else if p.extension().map(|x| x.eq_ignore_ascii_case("lnk")).unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

/// Minimal MS-SHLLINK parser: extract the LocalBasePath from a .lnk file.
/// COM (CLSID_ShellLink) is unregistered on some locked-down enterprise
/// machines, so we read the documented binary layout directly.
fn parse_lnk_local_path(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 76 {
        return None;
    }
    if u32::from_le_bytes(bytes[0..4].try_into().ok()?) != 0x0000_004C {
        return None;
    }
    let flags = u32::from_le_bytes(bytes[20..24].try_into().ok()?) as usize;
    const HAS_IDLIST: usize = 0x01;
    const HAS_LINK_INFO: usize = 0x02;

    let mut pos = 76usize;
    if flags & HAS_IDLIST != 0 {
        // ItemIDList: a run of ItemIDs, each ItemIDSize (2 bytes) + data, where
        // ItemIDSize INCLUDES its own 2-byte header; terminated by ItemIDSize==0.
        loop {
            let mut b2 = [0u8; 2];
            b2.copy_from_slice(bytes.get(pos..pos + 2)?);
            let sz = u16::from_le_bytes(b2) as usize;
            if sz == 0 {
                pos += 2;
                break;
            }
            if sz < 2 {
                return None;
            }
            pos += sz;
            if pos + 2 > bytes.len() {
                return None;
            }
        }
    }
    if flags & HAS_LINK_INFO == 0 {
        return None;
    }
    let li = pos;
    // LinkInfo base header: [0x00]BlockSize [0x04]LinkInfoSize [0x08]VolumeIDOffset
    //                      [0x0C]LocalBasePathOffset [0x10]CommonPathSuffixOffset
    // Per spec LocalBasePath holds the full ANSI path, but non-standard installers
    // (Chinese vendors, Electron builders) routinely leave it a stray byte and put
    // the whole path at CommonPathSuffix — we've seen that on every file here. So
    // read both OEM/GBK strings and keep whichever actually resolves to a drive-rooted
    // .exe; fall back to their concatenation for the compliant prefix+suffix layout.
    let mut b4 = [0u8; 4];
    b4.copy_from_slice(bytes.get(li + 12..li + 16)?);
    let base_off = u32::from_le_bytes(b4) as usize;
    b4.copy_from_slice(bytes.get(li + 16..li + 20)?);
    let cps_off = u32::from_le_bytes(b4) as usize;

    let read_oem = |off: usize| -> String {
        let start = li + off;
        if off == 0 || start >= bytes.len() {
            return String::new();
        }
        let mut end = start;
        while end < bytes.len() && bytes[end] != 0 {
            end += 1;
        }
        let (cow, _, _) = encoding_rs::GBK.decode(&bytes[start..end]);
        cow.into_owned()
    };
    let full_exe = |s: &str| s.contains(':') && s.to_lowercase().ends_with(".exe");

    let base = read_oem(base_off);
    let cps = read_oem(cps_off);
    if full_exe(&base) {
        Some(base)
    } else if full_exe(&cps) {
        Some(cps)
    } else {
        let joined = format!("{}\\{}", base.trim_end_matches('\\'), cps.trim_start_matches('\\'));
        if full_exe(&joined) {
            Some(joined)
        } else {
            None
        }
    }
}

/// Shortcut-based apps: Start Menu **and** Desktop (user + public). Both are
/// where Windows records "things the user launches"; the Desktop especially
/// catches apps that never registered a Start Menu entry or uninstall key.
/// Resolves each .lnk via `parse_lnk_local_path` — no COM dependency.
#[cfg(windows)]
fn scan_shortcut_apps(apps: &mut Vec<InstalledAppInfo>, seen: &mut std::collections::HashSet<String>) {
    let mut roots: Vec<std::path::PathBuf> = [
        std::env::var_os("APPDATA"),
        std::env::var_os("ProgramData"),
    ]
    .into_iter()
    .flatten()
    .map(|base| std::path::PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs"))
    .filter(|p| p.is_dir())
    .collect();
    // Desktop folders (shallow — shortcuts may sit in a subfolder).
    for desktop in [desktop_dir(), public_desktop_dir()].into_iter().flatten() {
        if desktop.is_dir() {
            roots.push(desktop);
        }
    }

    let mut links: Vec<std::path::PathBuf> = Vec::new();
    for r in &roots {
        collect_lnk_files(r, 4, &mut links);
    }
    for lnk in links {
        let name = lnk.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        if is_start_menu_noise_display(&name) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&lnk) else { continue };
        let Some(target) = parse_lnk_local_path(&bytes) else { continue };
        if !is_valid_start_menu_target(&target.to_lowercase()) || !std::path::Path::new(&target).is_file() {
            continue;
        }
        if seen.insert(normalize_path_key(&target)) {
            let version = get_app_version_sync(target.clone()).unwrap_or_default();
            apps.push(InstalledAppInfo {
                display_name: name,
                exe_path: target,
                publisher: String::new(),
                version,
                icon: None,
            });
        }
    }
}

/// Resolve the current user's Desktop via the shell folder (handles OneDrive
/// redirection), falling back to %USERPROFILE%\Desktop.
#[cfg(windows)]
fn desktop_dir() -> Option<std::path::PathBuf> {
    let profile = std::env::var_os("USERPROFILE")?;
    let candidate = std::path::PathBuf::from(profile).join("Desktop");
    if candidate.is_dir() {
        return Some(candidate);
    }
    let one_drive = std::env::var_os("OneDrive").map(|b| std::path::PathBuf::from(b).join("Desktop"));
    one_drive.filter(|p| p.is_dir()).or(Some(candidate))
}

#[cfg(windows)]
fn public_desktop_dir() -> Option<std::path::PathBuf> {
    let pf = std::env::var_os("PUBLIC")?;
    Some(std::path::PathBuf::from(pf).join("Desktop"))
}

/// Per-user CLI / dev tools that install into `%LOCALAPPDATA%\<Vendor>` with no
/// uninstall key and no shortcut, so no standard "installed apps" source sees
/// them. This is a deliberately small, curated allowlist (extend as needed) — a
/// broad LOCALAPPDATA sweep would drown the list in caches and updater dirs.
#[cfg(windows)]
fn scan_cli_tool_apps(apps: &mut Vec<InstalledAppInfo>, seen: &mut std::collections::HashSet<String>) {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else { return };
    let local = std::path::PathBuf::from(local);
    // (display name, path under LOCALAPPDATA, exe file, exe sits under a versioned subdir)
    const SPECS: &[(&str, &str, &str, bool)] = &[
        ("Codex", r"OpenAI\Codex\bin", "codex.exe", false),
        ("Claude Code", r"Claude-3p\claude-code", "claude.exe", true),
    ];
    for (name, rel, exe, versioned) in SPECS {
        let root = local.join(rel);
        let exe_path = if *versioned {
            newest_version_exe(&root, exe)
        } else {
            let p = root.join(exe);
            p.is_file().then(|| p.to_string_lossy().to_string())
        };
        let Some(exe_path) = exe_path else { continue };
        if seen.insert(normalize_path_key(&exe_path)) {
            let version = get_app_version_sync(exe_path.clone()).unwrap_or_default();
            apps.push(InstalledAppInfo {
                display_name: name.to_string(),
                exe_path,
                publisher: String::new(),
                version,
                icon: None,
            });
        }
    }
}

/// Pick `exe` from the highest version-numbered subdirectory of `root`
/// (e.g. `claude-code\2.1.281\claude.exe` wins over `2.1.280`).
#[cfg(windows)]
fn newest_version_exe(root: &std::path::Path, exe: &str) -> Option<String> {
    let mut best: Option<(Vec<u32>, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir.file_name()?.to_string_lossy().to_string();
        let ver: Vec<u32> = name.split('.').filter_map(|s| s.parse::<u32>().ok()).collect();
        if ver.is_empty() {
            continue;
        }
        let cand = dir.join(exe);
        if !cand.is_file() {
            continue;
        }
        if best.as_ref().map(|(bv, _)| ver > *bv).unwrap_or(true) {
            best = Some((ver, cand));
        }
    }
    best.map(|(_, p)| p.to_string_lossy().to_string())
}

/// Acceptance filter for a Start Menu shortcut's resolved exe target.
fn is_valid_start_menu_target(path_lower: &str) -> bool {
    if !path_lower.ends_with(".exe") {
        return false;
    }
    const NOISE_DIRS: &[&str] = &[
        "\\windows\\", "\\system32\\", "\\syswow64\\", "\\winsxs\\", "\\package cache\\",
        "\\servicing\\", "\\temp\\", "\\windowsapps\\", "\\$recycle.bin\\", "\\windows kits\\",
    ];
    if NOISE_DIRS.iter().any(|n| path_lower.contains(n)) {
        return false;
    }
    let stem = path_lower.rsplit(['\\', '/']).next().unwrap_or("").trim_end_matches(".exe");
    !is_start_menu_noise_stem(stem)
}

/// Stems that are uninstallers/setup/updaters or per-feature helper launchers,
/// not the app itself. Start Menu targets are already resolved files, so this is
/// a lighter filter than the install-dir sweep (`is_auxiliary_exe_stem` would clip
/// real apps here). The confirmed entries came from this machine's Start Menu.
fn is_start_menu_noise_stem(stem_lower: &str) -> bool {
    stem_lower.starts_with("unins")
        || stem_lower.starts_with("uninstall")
        || matches!(
            stem_lower,
            "setup" | "install" | "installer" | "update" | "updater" | "autoupdate" | "autoupdater"
                | "repair" | "modify" | "help" | "readme" | "license" | "changelog"
                | "private_browsing" | "chrome_proxy" | "setlang" | "javacpl" | "jmc"
                | "appcertui" | "about"
        )
}

/// Housekeeping shortcuts (name literally "Uninstall"/"卸载"/"关于"…) are noise even
/// when their target exe looks fine — the shortcut, not the exe, is the tell.
fn is_start_menu_noise_display(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    matches!(
        n.as_str(),
        "uninstall" | "卸载" | "卸载程序" | "update" | "更新" | "about" | "关于" | "help" | "帮助" | "readme" | "license"
    ) || n.starts_with("卸载")
        || n.starts_with("uninstall")
        || n.starts_with("关于")
        || n.starts_with("about ")
}

#[cfg(windows)]
fn enum_subkeys(
    hive: windows::Win32::System::Registry::HKEY,
    path: &str,
) -> Result<Vec<String>, String> {
    use windows::Win32::System::Registry::{
        RegOpenKeyExW, RegEnumKeyExW, RegCloseKey, HKEY, KEY_READ, KEY_WOW64_64KEY,
    };
    use windows::core::{PCWSTR, PWSTR};

    let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey: HKEY = HKEY::default();

    let access = KEY_READ | KEY_WOW64_64KEY;
    let result = unsafe {
        RegOpenKeyExW(hive, PCWSTR(path_wide.as_ptr()), Some(0), access, &mut hkey)
    };

    if result.is_err() {
        return Err(format!("无法打开注册表键: {}", path));
    }

    let mut subkeys = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name_buf = [0u16; 256];
        let mut name_len = 256u32;
        let result = unsafe {
            RegEnumKeyExW(hkey, index, Some(PWSTR(name_buf.as_mut_ptr())), &mut name_len, None, None, None, None)
        };
        if result.is_err() {
            break;
        }
        let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        if !name.is_empty() {
            subkeys.push(name);
        }
        index += 1;
    }

    unsafe { let _ = RegCloseKey(hkey); }
    Ok(subkeys)
}

#[cfg(windows)]
fn read_app_info(
    hive: windows::Win32::System::Registry::HKEY,
    path: &str,
    _subkey: &str,
) -> Option<InstalledAppInfo> {
    use windows::Win32::System::Registry::{
        RegOpenKeyExW, RegCloseKey, HKEY, KEY_READ, KEY_WOW64_64KEY,
    };
    use windows::core::PCWSTR;

    let path_wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey: HKEY = HKEY::default();

    let access = KEY_READ | KEY_WOW64_64KEY;
    let result = unsafe {
        RegOpenKeyExW(hive, PCWSTR(path_wide.as_ptr()), Some(0), access, &mut hkey)
    };
    if result.is_err() {
        return None;
    }

    let display_name = query_string_value(hkey, "DisplayName");
    // Hidden system components (Java control panel, Windows Kits tools, redistributable
    // helpers) set SystemComponent=1 — Windows' own "Apps & Features" hides them, and so
    // should we, or the list fills with noise no launcher wants.
    if query_dword(hkey, "SystemComponent") == Some(1) || display_name.is_none() {
        unsafe { let _ = RegCloseKey(hkey); }
        return None;
    }
    let publisher = query_string_value(hkey, "Publisher").unwrap_or_default();
    let version = query_string_value(hkey, "DisplayVersion").unwrap_or_default();

    // Prefer InstallLocation (installation directory) to find the actual exe.
    // UninstallString points to the uninstaller, not the main application.
    let install_location = query_string_value(hkey, "InstallLocation");

    // Try to find the main exe in the install directory.
    let mut exe_path = install_location
        .as_deref()
        .map(|loc| find_exe_in_dir(loc, display_name.as_deref()))
        .flatten()
        .unwrap_or_default();

    // Many vendors (QQ, old Win32 apps) leave InstallLocation empty; fall back
    // to the uninstaller's own directory, which is the install dir in practice.
    if exe_path.is_empty() {
        if let Some(uninstall) = query_string_value(hkey, "UninstallString") {
            if let Some(dir) = dir_from_uninstall_string(&uninstall) {
                exe_path = find_exe_in_dir(&dir, display_name.as_deref()).unwrap_or_default();
            }
        }
    }

    unsafe { let _ = RegCloseKey(hkey); }

    let display_name = display_name.filter(|s| !s.is_empty())?;
    // Validate: exe_path must be non-empty and point to an actual file.
    if exe_path.is_empty() || !std::path::Path::new(&exe_path).is_file() {
        return None;
    }

    // Try to extract icon with a timeout to avoid blocking the scan.
    let icon = extract_app_icon_sync(exe_path.clone()).ok();

    Some(InstalledAppInfo {
        display_name,
        exe_path,
        publisher,
        version,
        icon,
    })
}

#[cfg(windows)]
fn query_string_value(hkey: windows::Win32::System::Registry::HKEY, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{RegQueryValueExW, REG_VALUE_TYPE};
    use windows::core::PCWSTR;

    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut value_type: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let mut data_len: u32 = 0;

    let result = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut value_type),
            None,
            Some(&mut data_len),
        )
    };
    if result.is_err() {
        return None;
    }

    if data_len == 0 {
        return None;
    }

    let mut buf = vec![0u8; data_len as usize];
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut value_type),
            Some(buf.as_mut_ptr()),
            Some(&mut data_len),
        )
    };
    if result.is_err() {
        return None;
    }

    // REG_SZ = 1, REG_EXPAND_SZ = 2 (contains unexpanded env vars like %ProgramFiles%)
    if value_type.0 == 1 || value_type.0 == 2 {
        let wide_len = (data_len as usize) / 2;
        let wide_slice: Vec<u16> = buf.chunks_exact(2)
            .take(wide_len)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&wide_slice);
        let s = s.trim_end_matches('\0').to_string();
        // Expand environment variables for REG_EXPAND_SZ values.
        if value_type.0 == 2 {
            expand_env_vars(&s)
        } else {
            Some(s)
        }
    } else {
        None
    }
}

/// Read a REG_DWORD value from an already-open key handle.
#[cfg(windows)]
fn query_dword(hkey: windows::Win32::System::Registry::HKEY, name: &str) -> Option<u32> {
    use windows::Win32::System::Registry::{RegQueryValueExW, REG_DWORD, REG_VALUE_TYPE};
    use windows::core::PCWSTR;

    let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut value_type: REG_VALUE_TYPE = REG_VALUE_TYPE(0);
    let mut data = 0u32;
    let mut data_len = 4u32;
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut value_type),
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut data_len),
        )
    };
    if result.is_err() || value_type != REG_DWORD {
        return None;
    }
    Some(data)
}

/// Expand Windows environment variables in a path via the OS (`ExpandEnvironmentStringsW`).
/// Returns the input unchanged if expansion fails or leaves unexpanded variables.
#[cfg(windows)]
fn expand_env_vars(input: &str) -> Option<String> {
    if !input.contains('%') {
        return Some(input.to_string());
    }
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows::core::PCWSTR;

    let input_wide: Vec<u16> = input.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; input_wide.len() * 2 + 16];
    let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(input_wide.as_ptr()), Some(&mut buf)) };
    if needed == 0 || needed as usize > buf.len() {
        return Some(input.to_string());
    }
    let expanded = String::from_utf16_lossy(&buf[..needed as usize - 1]); // drop null
    // If the OS left variables unexpanded, pass through as-is; launching will fail loudly.
    Some(expanded)
}

/// Noise exes that are never the "main application" of an install dir.
/// Expects an already-lowercased file stem. Edge-word rules (prefix/suffix)
/// plus a list of confirmed system/vendor component names.
fn is_auxiliary_exe_stem(stem_lower: &str) -> bool {
    // Spaces or dots in a stem = vendor component, never a launchable app name
    // (dots: "msal.wsl.proxy", "Microsoft.ApplicationId").
    if stem_lower.contains(' ') || stem_lower.contains('.') {
        return true;
    }
    // Stems with embedded spaces or version-ish junk are components too.
    const AUX_PREFIXES: &[&str] = &[
        "unins", "msmpeng", "msedgewebview", "mipdlp", "mp", "wm", "wsl", "crash",
        "update", "updater", "setup", "elevator", "bgsvc", "plugin-", "maintain",
        "default-browser", "pingsender", "kdump", "picpreview",
    ];
    const AUX_SUFFIXES: &[&str] = &[
        "update", "updater", "installer", "install", "setup", "service", "services",
        "svc", "svr", "helper", "elevator", "launcher", "proxy", "report", "reporter",
        "migrate", "agent", "daemon", "container", "diag", "diagnosis", "repair",
        "preview", "detect", "webengineprocess", "watermark", "config",
    ];
    const AUX_EXACT: &[&str] = &[
        "install", "setup", "update", "updater", "elevator", "stublauncher", "patch",
        "env_detect", "nissrv", "mdnsresponder", "wab", "wabmig", "extexport",
        "ieinstal", "iediagcmd", "ielowutil", "vstoolsfeedback", "fastpdf",
    ];
    // Confirmed real apps that a rule above would otherwise clip (msedge: "ms"/"edge").
    const ALLOWED_EXCEPTIONS: &[&str] = &["msedge", "mspaint", "mstsc", "notepad", "wordpad"];
    if ALLOWED_EXCEPTIONS.contains(&stem_lower) {
        return false;
    }
    AUX_EXACT.contains(&stem_lower)
        || AUX_PREFIXES.iter().any(|p| stem_lower.starts_with(p))
        || AUX_SUFFIXES.iter().any(|s| stem_lower.ends_with(s))
}

/// Collapse name separators so "CC Switch" matches stem "cc-switch".
fn squash_name(s: &str) -> String {
    s.chars().filter(|c| *c != ' ' && *c != '-' && *c != '_').collect()
}

/// Pick the main executable from a list of (stem, path) candidates.
/// Exact name match wins, then prefix match; never falls back blindly.
fn pick_main_exe(candidates: &[(String, String)], display_name: Option<&str>) -> Option<String> {
    let candidates: Vec<&(String, String)> = candidates
        .iter()
        .filter(|(stem, _)| !is_auxiliary_exe_stem(stem))
        .collect();
    let name_lower = display_name.map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty());
    if let Some(name) = name_lower {
        let sq_name = squash_name(&name);
        // Exact match first (separator-insensitive).
        if let Some((_, path)) = candidates.iter().find(|(n, _)| *n == name || squash_name(n) == sq_name) {
            return Some(path.clone());
        }
        // A meaningful (3+ chars) word of the display name equals or prefixes the
        // exe stem — covers "Google Chrome" -> chrome, "Visual Studio Code" -> code.
        let tokens: Vec<&str> = name.split_whitespace().filter(|t| t.len() >= 3).collect();
        if let Some((_, path)) = candidates.iter().find(|(n, _)| {
            let sq = squash_name(n);
            n.len() >= 3 && tokens.iter().any(|tk| {
                let sqtk = squash_name(tk);
                *n == *tk || sq == sqtk || sqtk.starts_with(&sq)
            })
        }) {
            return Some(path.clone());
        }
        // Exe stem starts with the full display name (e.g. "Cursor" -> cursor-updater is
        // already filtered; "DeepSeek" -> deepseek_harness).
        if let Some((_, path)) = candidates
            .iter()
            .find(|(n, _)| sq_name.len() >= 4 && squash_name(n).starts_with(sq_name.as_str()))
        {
            return Some(path.clone());
        }
        return None;
    }
    // No display name: only safe when exactly one candidate exists.
    if candidates.len() == 1 {
        return Some(candidates[0].1.clone());
    }
    None
}

/// Parent directory of an UninstallString like `"C:\app\unins000.exe" /S`
/// (quotes optional). Returns None if no path separator is present.
fn dir_from_uninstall_string(raw: &str) -> Option<String> {
    let s = raw.trim();
    let path_part = if let Some(rest) = s.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        // Unquoted: drop a trailing " /args" section before locating the dir.
        s.split(" /").next().unwrap_or(s)
    };
    let p = std::path::Path::new(path_part);
    p.parent().filter(|par| !par.as_os_str().is_empty()).map(|par| par.to_string_lossy().to_string())
}

/// Find an executable file in a directory.
/// Tries to match the display name against .exe file stems.
#[cfg(windows)]
fn find_exe_in_dir(dir: &str, display_name: Option<&str>) -> Option<String> {
    let dir_path = std::path::Path::new(dir);
    if !dir_path.is_dir() {
        return None;
    }

    let entries = std::fs::read_dir(dir_path).ok()?;
    let mut exes: Vec<(String, String)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().map(|e| e.eq_ignore_ascii_case("exe")).unwrap_or(false) {
            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                exes.push((name.to_lowercase(), path.to_string_lossy().to_string()));
            }
        }
    }

    pick_main_exe(&exes, display_name)
}

/// Program Files subdirectories that are Windows built-ins or shared vendor
/// components — never user-launchable "applications".
#[cfg(windows)]
fn is_blocked_program_dir(dir_lower: &str) -> bool {
    const BLOCKED: &[&str] = &[
        "windows defender", "windows mail", "windows media player", "windows photo viewer",
        "internet explorer", "wsl", "bonjour", "common files", "windowsapps",
        "mozilla maintenance service", "microsoft update health tools",
        "yunshu plugin", "installshield installation information",
    ];
    BLOCKED.contains(&dir_lower) || dir_lower.starts_with('.')
}

/// .exe files at the root of a directory, as (lowercase stem, full path).
#[cfg(windows)]
fn collect_root_exes(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() && p.extension().map(|x| x.eq_ignore_ascii_case("exe")).unwrap_or(false) {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    out.push((stem.to_lowercase(), p.to_string_lossy().to_string()));
                }
            }
        }
    }
    out
}

#[cfg(windows)]
fn push_scanned_app(
    apps: &mut Vec<InstalledAppInfo>,
    seen: &mut std::collections::HashSet<String>,
    display_name: String,
    exe_path: String,
) {
    if seen.insert(normalize_path_key(&exe_path)) {
        apps.push(InstalledAppInfo {
            display_name,
            exe_path,
            publisher: String::new(),
            version: String::new(),
            icon: None,
        });
    }
}

#[cfg(windows)]
fn scan_program_dir(
    dir: &std::path::Path,
    apps: &mut Vec<InstalledAppInfo>,
    seen: &mut std::collections::HashSet<String>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name_os = path.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let dir_name = dir_name_os.to_lowercase();
        if is_blocked_program_dir(&dir_name) {
            continue;
        }

        let candidates = collect_root_exes(&path);
        if !candidates.is_empty() {
            // Use the directory name as the identity: "i4Tools9" -> i4Tools,
            // "YunShu" -> YunShu.exe; junk-only directories match nothing.
            if let Some(exe_path) = pick_main_exe(&candidates, Some(&dir_name)) {
                push_scanned_app(apps, seen, dir_name_os, exe_path);
            }
            continue;
        }

        // No exes at the root: a vendor container like "Tencent" or "DAUM" whose
        // products live one level down (Tencent\Weixin\Weixin.exe). Descend once.
        if let Ok(subs) = std::fs::read_dir(&path) {
            for sub in subs.flatten() {
                let sub_path = sub.path();
                if !sub_path.is_dir() {
                    continue;
                }
                let sub_name_os = sub_path.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
                let sub_name = sub_name_os.to_lowercase();
                if is_blocked_program_dir(&sub_name) {
                    continue;
                }
                let sub_candidates = collect_root_exes(&sub_path);
                if sub_candidates.is_empty() {
                    continue;
                }
                if let Some(exe_path) = pick_main_exe(&sub_candidates, Some(&sub_name)) {
                    push_scanned_app(apps, seen, sub_name_os, exe_path);
                }
            }
        }
    }
}

/// Normalised dedup key: case + separator insensitive, trailing slash trimmed.
fn normalize_path_key(path: &str) -> String {
    path.trim().to_lowercase().replace('/', "\\").trim_end_matches('\\').to_string()
}
