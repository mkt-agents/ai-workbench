//! Host another application's own window inside the workbench window.
//!
//! Desktop apps such as DeepSeek Harness desktop and Cursor draw through their
//! own Chromium surfaces behind private URL schemes, so a webview cannot show
//! them. The only way to put them *inside* the app is to adopt their top-level
//! window with `SetParent` and keep it positioned over the page slot:
//!
//! * `find` locates the window by image name (visible top-level windows only).
//! * `attach` remembers the original styles/parent, strips the standalone frame,
//!   marks the window `WS_CHILD` of the main window and places it.
//! * `relayout` follows the slot while the page resizes; `show` toggles it.
//! * `detach` — and `detach_all`, run on exit — hands the window back untouched.
//!
//! Adopted windows are children of ours, which means Windows destroys them along
//! with our window. Every exit path therefore detaches first; `release` restores
//! the original frame, owner and screen position.

/// One embeddable desktop window, as reported to the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DesktopWindowInfo {
    /// The image name we matched on (echo of the request).
    pub process_name: String,
    pub pid: u32,
    pub title: String,
    /// Win32 class of the adopted window — what the page shows on hover, so a
    /// wrong pick (splash, helper window) is visible instead of silent.
    pub class: String,
    /// True when the window is currently parented into the workbench.
    pub attached: bool,
    /// Diagnostic code for an adopted window that will not show anything
    /// (`""` = fine): `gone` | `not-child` | `minimized` | `invisible` | `size`.
    /// Empty is the normal case; the page turns the code into a warning chip,
    /// because a blank area with "embedded" next to it is a lie.
    pub warning: String,
}

/// Outcome of a relayout: whether the window is still ours.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DesktopRelayoutResult {
    /// `ok` (moved) | `reparented` (the app un-parented itself, taken back) |
    /// `replaced` (the frame was destroyed, a fresh one was adopted) | `lost`.
    pub state: String,
    pub title: String,
    pub class: String,
    /// Same diagnostic code as `DesktopWindowInfo::warning`.
    pub warning: String,
}

/// One top-level window seen for a process, for troubleshooting. Every window
/// matching the image-name needle is reported, hidden and owned ones included,
/// so "nothing was adopted" can be read off the page instead of guessed at.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DesktopWindowCandidate {
    pub process_name: String,
    pub pid: u32,
    /// Window handle as hex (`0x00010123`) — passed back to adopt that window.
    pub hwnd: String,
    pub class: String,
    pub title: String,
    pub visible: bool,
    pub owned: bool,
    pub area: i64,
}

#[cfg(windows)]
mod imp {
    use super::{DesktopRelayoutResult, DesktopWindowCandidate, DesktopWindowInfo};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tauri::Manager;
    use windows::core::{BOOL, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetParent, GetWindow, GetWindowLongPtrW, GetWindowRect,
        GetWindowThreadProcessId, GetWindowTextW, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
        SetParent, SetWindowLongPtrW,
        SetWindowPos, ShowWindow, GWL_EXSTYLE, GWL_STYLE, GWLP_HWNDPARENT, GW_OWNER, HWND_TOP,
        SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SWP_SHOWWINDOW, SW_RESTORE, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_APPWINDOW,
        WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_MAXIMIZE, WS_MAXIMIZEBOX, WS_MINIMIZE, WS_MINIMIZEBOX,
        WS_POPUP, WS_SYSMENU, WS_THICKFRAME, WS_VISIBLE,
    };

    /// Below this (physical px²) a window is a helper, not an application frame.
    const MIN_FRAME_AREA: i64 = 30_000;

    /// Raw handles are `*mut c_void`, which is not `Send`, so the registry keeps
    /// them as `isize` and converts at the call site.
    fn as_hwnd(raw: isize) -> HWND {
        HWND(raw as *mut _)
    }

    /// A window we adopted, with everything needed to hand it back as found.
    struct Embedded {
        hwnd: isize,
        pid: u32,
        title: String,
        class: String,
        /// The host window it was parented to (compared on every relayout).
        parent: isize,
        /// Frame/owner state captured at attach time.
        style: isize,
        exstyle: isize,
        owner: isize,
    }

    /// Append one line to `%TEMP%\ai-workbench-desktop-embed.log` (debug builds
    /// only). The adoption decision is otherwise invisible once the window is
    /// inside the app: this is the only way to see *which* window we took.
    fn debug_log(line: &str) {
        #[cfg(debug_assertions)]
        {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(std::env::temp_dir().join("ai-workbench-desktop-embed.log"))
            {
                let _ = writeln!(file, "{line}");
            }
        }
        #[cfg(not(debug_assertions))]
        let _ = line;
    }

    static EMBEDDED: Mutex<Option<HashMap<String, Embedded>>> = Mutex::new(None);

    fn map_key(process: &str) -> String {
        process.trim().to_ascii_lowercase()
    }

    fn with_map<R>(f: impl FnOnce(&mut HashMap<String, Embedded>) -> R) -> R {
        let mut guard = EMBEDDED.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(HashMap::new());
        }
        f(guard.as_mut().expect("initialised just above"))
    }

    fn scale_and_hwnd(app: &tauri::AppHandle) -> Result<(HWND, f64), String> {
        let window = app.get_window("main").ok_or("主窗口不存在")?;
        let scale = window.scale_factor().unwrap_or(1.0);
        let raw = window.hwnd().map_err(|e| format!("无法获取主窗口句柄: {e}"))?;
        Ok((HWND(raw.0 as *mut _), scale))
    }

    /// Image name (without extension) of a running process, or `None` if gone.
    fn image_stem(pid: u32) -> Option<String> {
        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        };
        let _ = unsafe { CloseHandle(handle) };
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
        Some(file.strip_suffix(".exe").unwrap_or(file).to_string())
    }

    fn window_area(hwnd: HWND) -> i64 {
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
            return 0;
        }
        (rect.right - rect.left).max(0) as i64 * (rect.bottom - rect.top).max(0) as i64
    }

    fn window_text(hwnd: HWND) -> String {
        let mut buf = [0u16; 512];
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..len.clamp(0, buf.len() as i32) as usize])
    }

    fn class_name(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..len.clamp(0, buf.len() as i32) as usize])
    }

    /// Our link to the window is gone and nothing took its place.
    fn lost() -> DesktopRelayoutResult {
        DesktopRelayoutResult {
            state: "lost".into(),
            title: String::new(),
            class: String::new(),
            warning: String::new(),
        }
    }

    /// Why the adopted window shows nothing, as a code the page can translate.
    ///
    /// Checked right after adopting (and on every relayout): a window that is
    /// parented but iconic/invisible/zero-sized leaves the slot blank, and the
    /// page would otherwise still claim it is embedded.
    fn verify(hwnd: HWND, parent: HWND) -> &'static str {
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() {
                return "gone";
            }
            if GetParent(hwnd).unwrap_or_default() != parent {
                return "not-child";
            }
            if IsIconic(hwnd).as_bool() {
                return "minimized";
            }
            if !IsWindowVisible(hwnd).as_bool() {
                return "invisible";
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok()
                && (rect.right - rect.left < 2 || rect.bottom - rect.top < 2)
            {
                return "size";
            }
        }
        ""
    }

    /// Every top-level window whose owning process image name contains the needle.
    #[derive(Clone)]
    pub(super) struct Candidate {
        hwnd: HWND,
        pid: u32,
        stem: String,
        class: String,
        title: String,
        visible: bool,
        owned: bool,
        toolwindow: bool,
        area: i64,
    }

    struct Found {
        hwnd: HWND,
        pid: u32,
        title: String,
        class: String,
    }

    struct Ctx {
        /// Lowercased image-name needle.
        needle: String,
        out: Vec<Candidate>,
    }

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return BOOL(1);
        }
        let Some(stem) = image_stem(pid) else {
            return BOOL(1);
        };
        // Substring, not equality: the launcher may be `idea64.exe` while the
        // user (or a shortcut) calls it "idea".
        if !stem.to_ascii_lowercase().contains(&ctx.needle) {
            return BOOL(1);
        }
        let mut buf = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut buf);
        let exstyle = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        ctx.out.push(Candidate {
            hwnd,
            pid,
            stem,
            class: String::from_utf16_lossy(&buf[..len.clamp(0, buf.len() as i32) as usize]),
            title: window_text(hwnd),
            visible: IsWindowVisible(hwnd).as_bool(),
            owned: GetWindow(hwnd, GW_OWNER).is_ok(),
            toolwindow: exstyle & WS_EX_TOOLWINDOW.0 as isize != 0,
            area: window_area(hwnd),
        });
        BOOL(1)
    }

    /// Helper windows Windows keeps around for every process; never the frame.
    fn is_helper_class(class: &str) -> bool {
        matches!(
            class,
            "Default IME"
                | "MSCTFIME UI"
                | "IME"
                | "GDI+ Hook Window Class"
                | "SysShadow"
                | "tooltips_class32"
                | "Windows.UI.Core.CoreWindow"
        )
    }

    /// Ranked best-first, for the diagnostics list only: exact image name,
    /// unowned, not a tool window, titled, visible, then largest.
    fn candidates(process: &str) -> Vec<Candidate> {
        let needle = process.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut ctx = Ctx { needle, out: Vec::new() };
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut ctx as *mut Ctx as isize));
        }
        let exact = process.trim().to_string();
        ctx.out.sort_by(|a, b| {
            let rank = |c: &Candidate| {
                (
                    c.stem.eq_ignore_ascii_case(&exact) as u8,
                    (!c.owned) as u8,
                    (!c.toolwindow) as u8,
                    (!c.title.is_empty()) as u8,
                    c.visible as u8,
                    c.area,
                )
            };
            rank(b).cmp(&rank(a))
        });
        ctx.out
    }

    /// Why no window could be adopted — spelled out for the page to show.
    fn no_window_message(process: &str) -> String {
        let all = candidates(process);
        if all.is_empty() {
            return format!(
                "没有找到进程名匹配「{process}」的窗口。确认它已经启动，且进程名（任务管理器 → 详细信息里的映像名称）与这里一致。"
            );
        }
        let hidden = all.iter().filter(|c| !c.visible).count();
        let sample = all
            .iter()
            .take(3)
            .map(|c| {
                format!(
                    "{}「{}」{}",
                    c.class,
                    c.title,
                    if c.visible { "可见" } else { "隐藏" }
                )
            })
            .collect::<Vec<_>>()
            .join("、");
        format!(
            "有 {} 个匹配窗口，但没有可用的可见窗口（隐藏 {hidden} 个）：{sample}",
            all.len()
        )
    }

    /// The window worth adopting.
    ///
    /// Deliberately conservative, most specific rule first, so the pick is the
    /// app's real frame and not a splash/helper of the same process:
    ///
    /// 1. exact image name + main-frame shape (visible, unowned, not a tool
    ///    window, titled, not an IME helper, big enough)
    /// 2. same shape, any image name
    /// 3. exact image name, visible and unowned
    /// 4. any visible unowned window
    /// 5. any visible window (last resort for odd shells)
    ///
    /// Each rule picks the largest window it allows.
    fn find_window(process: &str) -> Option<Found> {
        let all = candidates(process);
        let exact = process.trim().to_string();
        let is_exact = |c: &Candidate| c.stem.eq_ignore_ascii_case(&exact);
        let is_frame = |c: &Candidate| {
            c.visible
                && !c.owned
                && !c.toolwindow
                && !c.title.is_empty()
                && !is_helper_class(&c.class)
                && c.area >= MIN_FRAME_AREA
        };
        let pick = |test: &dyn Fn(&Candidate) -> bool| {
            all.iter().filter(|c| test(c)).max_by_key(|c| c.area).cloned()
        };
        pick(&|c| is_exact(c) && is_frame(c))
            .or_else(|| pick(&|c| is_frame(c)))
            .or_else(|| pick(&|c| is_exact(c) && c.visible && !c.owned))
            .or_else(|| pick(&|c| c.visible && !c.owned))
            .or_else(|| pick(&|c| c.visible))
            .map(|best| Found {
                hwnd: best.hwnd,
                pid: best.pid,
                title: best.title,
                class: best.class,
            })
    }

    pub fn describe_candidates(process: &str) -> Vec<DesktopWindowCandidate> {
        candidates(process)
            .into_iter()
            .take(12)
            .map(|c| DesktopWindowCandidate {
                process_name: c.stem,
                pid: c.pid,
                hwnd: format!("0x{:X}", c.hwnd.0 as usize),
                class: c.class,
                title: c.title,
                visible: c.visible,
                owned: c.owned,
                area: c.area,
            })
            .collect()
    }

    fn place(hwnd: HWND, x: f64, y: f64, width: f64, height: f64, scale: f64) {
        let px = (x * scale).round() as i32;
        let py = (y * scale).round() as i32;
        let pw = ((width * scale).round() as i32).max(1);
        let ph = ((height * scale).round() as i32).max(1);
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), px, py, pw, ph, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    /// Hand the window back: original parent, styles and screen position.
    fn release(entry: Embedded) {
        let hwnd = as_hwnd(entry.hwnd);
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return;
        }
        // GetWindowRect is in screen coordinates even for children, so the
        // window reappears where the user last saw it.
        let mut rect = RECT::default();
        let placed = unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok();
        unsafe {
            let _ = SetParent(hwnd, None);
            SetWindowLongPtrW(hwnd, GWL_STYLE, entry.style);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, entry.exstyle);
            if entry.owner != 0 {
                SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, entry.owner);
            }
            let mut flags = SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOZORDER;
            if !placed {
                flags |= SWP_NOMOVE | SWP_NOSIZE;
            }
            let _ = SetWindowPos(
                hwnd,
                None,
                if placed { rect.left } else { 0 },
                if placed { rect.top } else { 0 },
                if placed { rect.right - rect.left } else { 0 },
                if placed { rect.bottom - rect.top } else { 0 },
                flags,
            );
        }
    }

    pub fn find_info(process: &str) -> Option<DesktopWindowInfo> {
        let key = map_key(process);
        let attached = with_map(|m| {
            m.get(&key)
                .map(|e| (e.hwnd, e.pid, e.title.clone(), e.class.clone(), e.parent))
        });
        if let Some((hwnd, pid, title, class, parent)) = attached {
            if unsafe { IsWindow(Some(as_hwnd(hwnd))) }.as_bool() {
                let warning = verify(as_hwnd(hwnd), as_hwnd(parent));
                return Some(DesktopWindowInfo {
                    process_name: process.to_string(),
                    pid,
                    title,
                    class,
                    attached: true,
                    warning: warning.to_string(),
                });
            }
            with_map(|m| {
                m.remove(&key);
            });
        }
        let found = find_window(process)?;
        Some(DesktopWindowInfo {
            process_name: process.to_string(),
            pid: found.pid,
            title: found.title,
            class: found.class,
            attached: false,
            warning: String::new(),
        })
    }

    pub fn list() -> Vec<DesktopWindowInfo> {
        let entries: Vec<(String, isize, u32, String, String, isize)> = with_map(|m| {
            m.iter()
                .map(|(k, e)| {
                    (
                        k.clone(),
                        e.hwnd,
                        e.pid,
                        e.title.clone(),
                        e.class.clone(),
                        e.parent,
                    )
                })
                .collect()
        });
        let mut alive = Vec::new();
        for (key, hwnd, pid, title, class, parent) in entries {
            if unsafe { IsWindow(Some(as_hwnd(hwnd))) }.as_bool() {
                let warning = verify(as_hwnd(hwnd), as_hwnd(parent));
                alive.push(DesktopWindowInfo {
                    process_name: key,
                    pid,
                    title,
                    class,
                    attached: true,
                    warning: warning.to_string(),
                });
            } else {
                with_map(|m| {
                    m.remove(&key);
                });
            }
        }
        alive
    }

    /// Strip the standalone frame off `hwnd` and parent it into `parent`.
    ///
    /// Kept separate from `place` because the app can undo this behind our back:
    /// Chromium re-asserts its own top-level frame on some window events, and
    /// `relayout` puts the window back with this same call.
    fn take_over(hwnd: HWND, parent: HWND) -> Result<(), String> {
        unsafe {
            // A minimized window stays minimized once it is a child — it would
            // occupy the slot and paint nothing. `IsWindowVisible` is true for
            // iconic windows, so the picker happily hands them over.
            if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            // Frameless child: drop the standalone frame the app draws for its
            // own top-level window, keep it visible.
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let exstyle = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let child_style = (style
                & !(WS_POPUP.0 as isize
                    | WS_CAPTION.0 as isize
                    | WS_THICKFRAME.0 as isize
                    | WS_SYSMENU.0 as isize
                    | WS_MINIMIZE.0 as isize
                    | WS_MINIMIZEBOX.0 as isize
                    | WS_MAXIMIZEBOX.0 as isize
                    | WS_MAXIMIZE.0 as isize))
                | WS_CHILD.0 as isize
                | WS_VISIBLE.0 as isize;
            SetWindowLongPtrW(hwnd, GWL_STYLE, child_style);
            // WS_EX_LAYERED has to go: a layered window only composites through
            // UpdateLayeredWindow against a layered parent, so as a child of an
            // ordinary window it draws nothing at all (blank slot).
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                exstyle & !(WS_EX_APPWINDOW.0 as isize | WS_EX_LAYERED.0 as isize),
            );
            // The host must clip children, or Chromium paints over the
            // sibling webview that draws the surrounding page.
            let host_style = GetWindowLongPtrW(parent, GWL_STYLE);
            if host_style & WS_CLIPCHILDREN.0 as isize == 0 {
                SetWindowLongPtrW(parent, GWL_STYLE, host_style | WS_CLIPCHILDREN.0 as isize);
            }
            SetParent(hwnd, Some(parent)).map_err(|e| format!("SetParent: {e}"))?;
        }
        Ok(())
    }

    /// Adopt one specific window (the page's window picker).
    pub fn attach_window(
        app: &tauri::AppHandle,
        process: &str,
        hwnd_raw: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> Result<DesktopWindowInfo, String> {
        let key = map_key(process);
        if key.is_empty() {
            return Err("进程名不能为空".into());
        }
        let text = hwnd_raw.trim();
        let hwnd = as_hwnd(
            usize::from_str_radix(text.trim_start_matches("0x"), 16)
                .map_err(|_| format!("无效的窗口句柄: {text}"))? as isize,
        );
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err("这个窗口已经不存在了".into());
        }
        let (parent, scale) = scale_and_hwnd(app)?;
        // Whatever we had for this process steps aside first.
        if let Some(previous) = with_map(|m| m.remove(&key)) {
            release(previous);
        }
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
        let exstyle = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        let owner = unsafe { GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        take_over(hwnd, parent).map_err(|e| format!("无法接管 {process} 的窗口（{text}）: {e}"))?;
        place(hwnd, x, y, width, height, scale);
        let class = class_name(hwnd);
        let title = window_text(hwnd);
        let warning = verify(hwnd, parent);
        debug_log(&format!(
            "attach_window {key} hwnd=0x{:X} class={class} title={title} warning={warning:?}",
            hwnd.0 as usize
        ));
        with_map(|m| {
            m.insert(
                key.clone(),
                Embedded {
                    hwnd: hwnd.0 as isize,
                    pid,
                    title: title.clone(),
                    class: class.clone(),
                    parent: parent.0 as isize,
                    style,
                    exstyle,
                    owner,
                },
            );
        });
        Ok(DesktopWindowInfo {
            process_name: process.to_string(),
            pid,
            title,
            class,
            attached: true,
            warning: warning.to_string(),
        })
    }

    pub fn attach(
        app: &tauri::AppHandle,
        process: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> Result<DesktopWindowInfo, String> {
        let key = map_key(process);
        if key.is_empty() {
            return Err("进程名不能为空".into());
        }
        let (parent, scale) = scale_and_hwnd(app)?;

        // Already adopted: just follow the slot.
        let existing =
            with_map(|m| m.get(&key).map(|e| (e.hwnd, e.pid, e.title.clone(), e.class.clone())));
        if let Some((hwnd, pid, title, class)) = existing {
            if unsafe { IsWindow(Some(as_hwnd(hwnd))) }.as_bool() {
                place(as_hwnd(hwnd), x, y, width, height, scale);
                let warning = verify(as_hwnd(hwnd), parent);
                return Ok(DesktopWindowInfo {
                    process_name: process.to_string(),
                    pid,
                    title,
                    class,
                    attached: true,
                    warning: warning.to_string(),
                });
            }
            with_map(|m| {
                m.remove(&key);
            });
        }

        let found = find_window(process).ok_or_else(|| no_window_message(process))?;
        let style = unsafe { GetWindowLongPtrW(found.hwnd, GWL_STYLE) };
        let exstyle = unsafe { GetWindowLongPtrW(found.hwnd, GWL_EXSTYLE) };
        let owner = unsafe { GetWindowLongPtrW(found.hwnd, GWLP_HWNDPARENT) };
        debug_log(&format!(
            "attach {key} pick hwnd=0x{:X} class={} title={} style=0x{style:X} exstyle=0x{exstyle:X} owner=0x{owner:X} iconic={} zoomed={}",
            found.hwnd.0 as usize,
            found.class,
            found.title,
            unsafe { IsIconic(found.hwnd) }.as_bool(),
            unsafe { IsZoomed(found.hwnd) }.as_bool()
        ));
        take_over(found.hwnd, parent).map_err(|e| {
            debug_log(&format!(
                "attach {key} hwnd=0x{:X} class={} FAILED {e}",
                found.hwnd.0 as usize, found.class
            ));
            format!("无法接管 {process} 的窗口（{}）: {e}", found.class)
        })?;
        place(found.hwnd, x, y, width, height, scale);
        let warning = verify(found.hwnd, parent);
        debug_log(&format!(
            "attach {key} hwnd=0x{:X} class={} title={} area={} warning={:?}",
            found.hwnd.0 as usize,
            found.class,
            found.title,
            window_area(found.hwnd),
            warning
        ));
        with_map(|m| {
            m.insert(
                key,
                Embedded {
                    hwnd: found.hwnd.0 as isize,
                    pid: found.pid,
                    title: found.title.clone(),
                    class: found.class.clone(),
                    parent: parent.0 as isize,
                    style,
                    exstyle,
                    owner,
                },
            );
        });
        Ok(DesktopWindowInfo {
            process_name: process.to_string(),
            pid: found.pid,
            title: found.title,
            class: found.class,
            attached: true,
            warning: warning.to_string(),
        })
    }

    /// Move an already-adopted window, and repair it if the app undid our work.
    ///
    /// Electron shells re-assert their own top-level frame on some window
    /// events, which silently slides the window back onto the desktop while the
    /// page still believes it is embedded — so every relayout checks the parent
    /// link, and re-adopts when the frame is gone (IDE splash → main window).
    pub fn relayout(
        app: &tauri::AppHandle,
        process: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) -> Result<DesktopRelayoutResult, String> {
        let key = map_key(process);
        let current =
            with_map(|m| m.get(&key).map(|e| (e.hwnd, e.parent, e.title.clone(), e.class.clone())));
        let Some((hwnd, entry_parent, title, class)) = current else {
            return Ok(lost());
        };
        let window = as_hwnd(hwnd);
        let (parent, scale) = scale_and_hwnd(app)?;

        if !unsafe { IsWindow(Some(window)) }.as_bool() {
            // The frame is gone for good (splash → main window, AWT rebuild).
            with_map(|m| {
                m.remove(&key);
            });
            debug_log(&format!("relayout {key} frame gone, re-adopting"));
            return match attach(app, process, x, y, width, height) {
                Ok(info) => Ok(DesktopRelayoutResult {
                    state: "replaced".into(),
                    title: info.title,
                    class: info.class,
                    warning: info.warning,
                }),
                Err(error) => {
                    debug_log(&format!("relayout {key} re-adopt failed: {error}"));
                    Ok(lost())
                }
            };
        }

        if entry_parent != 0 && unsafe { GetParent(window) }.unwrap_or_default() != parent {
            // The app put its frame back: take it over again.
            if let Err(error) = take_over(window, parent) {
                debug_log(&format!("relayout {key} re-parent failed: {error}"));
                return Ok(lost());
            }
            place(window, x, y, width, height, scale);
            let warning = verify(window, parent);
            debug_log(&format!(
                "relayout {key} re-parented (class={class} warning={warning:?})"
            ));
            return Ok(DesktopRelayoutResult {
                state: "reparented".into(),
                title,
                class,
                warning: warning.to_string(),
            });
        }

        place(window, x, y, width, height, scale);
        let warning = verify(window, parent);
        Ok(DesktopRelayoutResult {
            state: "ok".into(),
            title,
            class,
            warning: warning.to_string(),
        })
    }

    pub fn set_visible(process: &str, visible: bool) -> Result<(), String> {
        let key = map_key(process);
        let hwnd = with_map(|m| m.get(&key).map(|e| e.hwnd));
        let Some(hwnd) = hwnd else {
            return Ok(());
        };
        if !unsafe { IsWindow(Some(as_hwnd(hwnd))) }.as_bool() {
            with_map(|m| {
                m.remove(&key);
            });
            return Ok(());
        }
        let flags = SWP_NOACTIVATE
            | SWP_NOMOVE
            | SWP_NOSIZE
            | SWP_NOZORDER
            | if visible { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
        unsafe {
            let _ = SetWindowPos(as_hwnd(hwnd), None, 0, 0, 0, 0, flags);
        }
        Ok(())
    }

    pub fn detach(process: &str) {
        let key = map_key(process);
        if let Some(entry) = with_map(|m| m.remove(&key)) {
            release(entry);
        }
    }

    pub fn detach_all() {
        let entries: Vec<Embedded> = with_map(|m| m.drain().map(|(_, e)| e).collect());
        for entry in entries {
            release(entry);
        }
    }
}

/// Hand every adopted window back to the desktop. Called on exit: adopted
/// windows are our children, so Windows would destroy them with us.
pub fn release_all() {
    #[cfg(windows)]
    imp::detach_all();
}

/// Locate the desktop window for `process_name`. `attached` tells the two apart.
#[tauri::command]
pub async fn desktop_embed_find(process_name: String) -> Result<Option<DesktopWindowInfo>, String> {
    #[cfg(windows)]
    {
        Ok(imp::find_info(&process_name))
    }
    #[cfg(not(windows))]
    {
        let _ = process_name;
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Every top-level window the process owns — the troubleshooting view shown
/// when adoption finds nothing usable.
#[tauri::command]
pub async fn desktop_embed_candidates(
    process_name: String,
) -> Result<Vec<DesktopWindowCandidate>, String> {
    #[cfg(windows)]
    {
        Ok(imp::describe_candidates(&process_name))
    }
    #[cfg(not(windows))]
    {
        let _ = process_name;
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Take the window over and place it at the slot (logical coordinates).
#[tauri::command]
pub async fn desktop_embed_attach(
    app: tauri::AppHandle,
    process_name: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<DesktopWindowInfo, String> {
    #[cfg(windows)]
    {
        imp::attach(&app, &process_name, x, y, width, height)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, x, y, width, height);
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Adopt the window the user picked from the candidate list.
#[tauri::command]
pub async fn desktop_embed_attach_window(
    app: tauri::AppHandle,
    process_name: String,
    hwnd: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<DesktopWindowInfo, String> {
    #[cfg(windows)]
    {
        imp::attach_window(&app, &process_name, &hwnd, x, y, width, height)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, hwnd, x, y, width, height);
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Follow the slot while the page resizes; repairs the link when needed.
#[tauri::command]
pub async fn desktop_embed_relayout(
    app: tauri::AppHandle,
    process_name: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<DesktopRelayoutResult, String> {
    #[cfg(windows)]
    {
        imp::relayout(&app, &process_name, x, y, width, height)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, x, y, width, height);
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Show or hide the adopted window (page switched away / app minimised).
#[tauri::command]
pub async fn desktop_embed_show(process_name: String, visible: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        imp::set_visible(&process_name, visible)
    }
    #[cfg(not(windows))]
    {
        let _ = (process_name, visible);
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Give the window back to the desktop, untouched.
#[tauri::command]
pub async fn desktop_embed_detach(process_name: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        imp::detach(&process_name);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = process_name;
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Release every adopted window (used on exit so they are not destroyed).
#[tauri::command]
pub async fn desktop_embed_detach_all() -> Result<(), String> {
    #[cfg(windows)]
    {
        imp::detach_all();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Windows currently adopted into the workbench, for resyncing the page.
#[tauri::command]
pub async fn desktop_embed_list() -> Result<Vec<DesktopWindowInfo>, String> {
    #[cfg(windows)]
    {
        Ok(imp::list())
    }
    #[cfg(not(windows))]
    {
        Err("当前系统不支持嵌入桌面应用".into())
    }
}
