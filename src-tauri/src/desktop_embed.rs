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
    /// Window size in physical px, so the picker can show "W × H" instead of an
    /// opaque area figure.
    pub width: i64,
    pub height: i64,
    /// This handle is the window the auto-pick would adopt right now — the
    /// picker badges it as the recommended choice.
    pub recommended: bool,
    /// This handle is the window embedded right now.
    pub current: bool,
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
        EnumWindows, GetClassNameW, GetForegroundWindow, GetParent, GetWindow, GetWindowLongPtrW,
        GetWindowRect, GetWindowThreadProcessId, GetWindowTextW, IsHungAppWindow, IsIconic,
        IsWindow, IsWindowVisible, IsZoomed,
        SetParent, SetWindowLongPtrW,
        SetWindowPos, ShowWindow, GWL_EXSTYLE, GWL_STYLE, GWLP_HWNDPARENT, GW_OWNER, HWND_TOP,
        SW_HIDE, SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SWP_SHOWWINDOW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_APPWINDOW,
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

    /// True when the handle is still the live window of the recorded pid.
    ///
    /// `IsWindow` alone lies in one nasty way: after a process dies, Windows
    /// recycles handle values, so a stale map entry can pass the existence
    /// check while pointing at an innocent NEW window — in practice our own
    /// WebView child. Re-parenting or resizing that would lay waste to the
    /// whole workbench, so every consumer pins the check to the recorded pid.
    fn entry_live(hwnd: isize, pid: u32) -> bool {
        let window = as_hwnd(hwnd);
        if !unsafe { IsWindow(Some(window)) }.as_bool() {
            return false;
        }
        let mut live = 0u32;
        unsafe { GetWindowThreadProcessId(window, Some(&mut live)) };
        live == pid
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
        /// Last visibility WE decided for the window (1 = shown). The picker
        /// dialog uses set_visible(false) while itself on screen; place()
        /// keeps the window hidden until it is flipped back.
        shown: isize,
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
        let (w, h) = window_size(hwnd);
        w * h
    }

    fn window_size(hwnd: HWND) -> (i64, i64) {
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
            return (0, 0);
        }
        (
            (rect.right - rect.left).max(0) as i64,
            (rect.bottom - rect.top).max(0) as i64,
        )
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
        /// WS_VISIBLE was observed on a minimized window: `IsWindowVisible`
        /// is true there, which is why the console window slid through the
        /// old main-frame rules.
        iconic: bool,
        /// Raw window style, used to require a real frame border.
        style: isize,
        area: i64,
        /// The window the user is looking at. A multi-window app (VS Code:
        /// welcome + workspace) brings its real frame forward, so the
        /// foreground window is the best guess at "the one worth adopting".
        foreground: bool,
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
            iconic: IsIconic(hwnd).as_bool(),
            style: unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) },
            area: window_area(hwnd),
            foreground: unsafe { GetForegroundWindow() } == hwnd,
        });
        BOOL(1)
    }

    /// Helper windows Windows keeps around for every process; never the frame.
    /// A real app frame: visible, unowned, not a tool window, titled, not an
    /// IME/helper class, has a movable frame border, and comfortably larger
    /// than splash- or popup-sized.
    ///
    /// The border requirement is what keeps the adopt churn honest: the
    /// IntelliJ splash (WS_POPUP, no caption), Chromium notification popups
    /// and AWT console windows are all caption-less popups that used to pass
    /// as "main frames" and get re-adopted into the slot in an endless cycle.
    fn looks_like_main_frame(c: &Candidate) -> bool {
        c.visible
            && !c.iconic
            && !c.owned
            && !c.toolwindow
            && !c.title.is_empty()
            && !is_helper_class(&c.class)
            && c.area >= MIN_FRAME_AREA
            && (c.style & (WS_CAPTION.0 as isize | WS_THICKFRAME.0 as isize)) != 0
    }

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
        ) ||
        // IME candidate/branding windows (e.g. Sogou) never render app content.
        class.contains("Sogou")
            || class.contains("SoPY")
            || class.contains("IME")
            || class.ends_with("HintWnd")
    }

    /// A title that names a *document* rather than just the app: VS Code and
    /// other Electron shells title their welcome window with the bare app name
    /// ("Cursor") but a real workspace "‹folder› - Cursor". Preferring the
    /// latter stops the welcome window from being adopted while the workspace
    /// frame — the one the user is actually working in — is left on the desktop.
    fn title_is_workspace(title: &str, process: &str) -> bool {
        let title = title.trim();
        !title.is_empty()
            && !title.eq_ignore_ascii_case(process.trim())
            && (title.contains(" - ") || title.contains(" — ") || title.contains(" – "))
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
        let pick = |test: &dyn Fn(&Candidate) -> bool| {
            all.iter()
                .filter(|c| test(c))
                // Within a rule prefer the workspace frame over the welcome
                // window, then the window the user is looking at, then size.
                .max_by_key(|c| {
                    (
                        title_is_workspace(&c.title, &exact) as u8,
                        c.foreground as u8,
                        c.area,
                    )
                })
                .cloned()
        };
        pick(&|c| is_exact(c) && looks_like_main_frame(c))
            .or_else(|| pick(&|c| looks_like_main_frame(c)))
            .or_else(|| pick(&|c| is_exact(c) && c.visible && !c.owned && !c.iconic))
            .or_else(|| pick(&|c| c.visible && !c.owned && !c.iconic))
            .or_else(|| pick(&|c| c.visible && !c.iconic))
            .map(|best| Found {
                hwnd: best.hwnd,
                pid: best.pid,
                title: best.title,
                class: best.class,
            })
    }

    /// Like find_window but only the "main-frame shape" rules. Used when
    /// swapping away a window the app hid itself: it must be replaced by a
    /// genuine frame, never by a last-resort stray.
    fn find_frame(process: &str) -> Option<Found> {
        let all = candidates(process);
        let exact = process.trim().to_string();
        let is_exact = |c: &Candidate| c.stem.eq_ignore_ascii_case(&exact);
        let pick = |test: &dyn Fn(&Candidate) -> bool| {
            all.iter()
                .filter(|c| test(c))
                .max_by_key(|c| {
                    (
                        title_is_workspace(&c.title, &exact) as u8,
                        c.foreground as u8,
                        c.area,
                    )
                })
                .cloned()
        };
        pick(&|c| is_exact(c) && looks_like_main_frame(c))
            .or_else(|| pick(&|c| looks_like_main_frame(c)))
            .map(|best| Found {
                hwnd: best.hwnd,
                pid: best.pid,
                title: best.title,
                class: best.class,
            })
    }

    pub fn describe_candidates(process: &str) -> Vec<DesktopWindowCandidate> {
        let key = map_key(process);
        let current_hwnd = with_map(|m| m.get(&key).map(|e| e.hwnd));
        // What the auto-pick would adopt right now, so the picker can badge it.
        let recommended = find_window(process).map(|f| f.hwnd.0 as isize);
        candidates(process)
            .into_iter()
            // IME / tooltip / shadow helpers can never be meaningfully
            // adopted — hide them so the picker stays readable. Same for
            // invisible zero-area stubs: embedding one can only yield a
            // blank slot.
            .filter(|c| !is_helper_class(&c.class) && (c.visible || c.area > 0))
            .take(12)
            .map(|c| {
                let (width, height) = window_size(c.hwnd);
                DesktopWindowCandidate {
                    process_name: c.stem,
                    pid: c.pid,
                    hwnd: format!("0x{:X}", c.hwnd.0 as usize),
                    class: c.class,
                    title: c.title,
                    visible: c.visible,
                    owned: c.owned,
                    area: c.area,
                    width,
                    height,
                    recommended: recommended == Some(c.hwnd.0 as isize),
                    current: current_hwnd == Some(c.hwnd.0 as isize),
                }
            })
            .collect()
    }

    /// `show` mirrors the entry's last known visibility: the picker dialog
    /// deliberately hides the hosted window (it paints above the webview and
    /// would cover the dialog), and a relayout must not flip it back on.
    fn place(hwnd: HWND, x: f64, y: f64, width: f64, height: f64, scale: f64, show: bool) {
        let px = (x * scale).round() as i32;
        let py = (y * scale).round() as i32;
        let pw = ((width * scale).round() as i32).max(1);
        let ph = ((height * scale).round() as i32).max(1);
        let visibility = if show { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
        unsafe {
            // A hung (not-responding) foreign window would stall the whole
            // backend: SetWindowPos synchronously sends it messages that will
            // never be processed while it is busy shutting down. Skip this
            // tick instead of freezing the workbench alongside it.
            if IsHungAppWindow(hwnd).as_bool() {
                return;
            }
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), px, py, pw, ph, SWP_NOACTIVATE | visibility);
        }
    }

    /// Hand the window back: original parent, styles and screen position.
    fn release(entry: Embedded) {
        let hwnd = as_hwnd(entry.hwnd);
        if !entry_live(entry.hwnd, entry.pid) {
            return;
        }
        // GetWindowRect is in screen coordinates even for children, so the
        // window reappears where the user last saw it.
        let mut rect = RECT::default();
        let placed = unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok();
        unsafe {
            let _ = SetParent(hwnd, None);
            // Keep the CURRENT visibility, not the one from attach time: the
            // app may have hidden its own window while it was embedded (a
            // splash being disposed), and handing it back must not flip that
            // hidden window back to visible on the desktop.
            let current_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            if current_style & WS_VISIBLE.0 as isize != 0 {
                SetWindowLongPtrW(hwnd, GWL_STYLE, entry.style | WS_VISIBLE.0 as isize);
            } else {
                SetWindowLongPtrW(hwnd, GWL_STYLE, entry.style & !(WS_VISIBLE.0 as isize));
            }
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, entry.exstyle);
            if entry.owner != 0 {
                SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, entry.owner);
            }
            // Same hang guard as place(): a window that is mid-shutdown can
            // never answer the synchronous messages of SetWindowPos, which
            // would block backend threads until TerminateProcess gets there.
            if !IsHungAppWindow(hwnd).as_bool() {
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
    }

    pub fn find_info(process: &str) -> Option<DesktopWindowInfo> {
        let key = map_key(process);
        let attached = with_map(|m| {
            m.get(&key)
                .map(|e| (e.hwnd, e.pid, e.title.clone(), e.class.clone(), e.parent))
        });
        if let Some((hwnd, pid, title, class, parent)) = attached {
            if entry_live(hwnd, pid) {
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
            if entry_live(hwnd, pid) {
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
            // Hide before touching anything: until SetParent runs this is still
            // a visible top-level frame, and every restyle below makes it
            // repaint on the desktop — that is the launch flash. It stays
            // hidden until the caller's place(show=true) reveals it as our
            // child, so nothing is ever painted outside the slot.
            let _ = ShowWindow(hwnd, SW_HIDE);
            // A minimized/maximized window stays in that display state once it
            // is a child and would paint nothing. Clear the state bits while
            // hidden instead of `SW_RESTORE`: a restore paints the frame on the
            // desktop for a frame before the re-parent lands.
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
                | WS_CHILD.0 as isize;
            // Deliberately no WS_VISIBLE: `place` decides visibility, so the
            // window cannot flash onto the desktop between here and SetParent.
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
        place(hwnd, x, y, width, height, scale, true);
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
                    shown: 1,
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

    /// Why a single adoption attempt did not take.
    enum AttachFail {
        /// No candidate window existed at scan time — safe to retry soon.
        Empty,
        /// A window was found but could not be taken over — propagate.
        Hard(String),
    }

    /// One adoption attempt; no waiting, one scan.
    ///
    /// `strict` restricts the pick to genuine main frames (used by the
    /// relayout self-heal): launching a splash first is fine, but RE-adopting
    /// after a frame death must never drag in a splash / console window,
    /// which then dies in turn and produces an endless adopt cycle.
    fn attach_once(
        app: &tauri::AppHandle,
        process: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        strict: bool,
    ) -> Result<DesktopWindowInfo, AttachFail> {
        let key = map_key(process);
        if key.is_empty() {
            return Err(AttachFail::Hard("进程名不能为空".into()));
        }
        let (parent, scale) =
            scale_and_hwnd(app).map_err(|e| AttachFail::Hard(e.to_string()))?;

        // Already adopted: just follow the slot.
        let existing = with_map(|m| {
            m.get(&key)
                .map(|e| (e.hwnd, e.pid, e.title.clone(), e.class.clone(), e.shown))
        });
        if let Some((hwnd, pid, title, class, shown)) = existing {
            if entry_live(hwnd, pid) {
                place(as_hwnd(hwnd), x, y, width, height, scale, shown != 0);
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

        let found = if strict {
            find_frame(process)
        } else {
            find_window(process)
        }
        .ok_or(AttachFail::Empty)?;
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
            AttachFail::Hard(format!("无法接管 {process} 的窗口（{}）: {e}", found.class))
        })?;
        place(found.hwnd, x, y, width, height, scale, true);
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
                    shown: 1,
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

    /// Single attempt, project-shaped error for the existing callers.
    pub fn attach(
        app: &tauri::AppHandle,
        process: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        strict: bool,
    ) -> Result<DesktopWindowInfo, String> {
        attach_once(app, process, x, y, width, height, strict).map_err(|fail| match fail {
            AttachFail::Empty => no_window_message(process),
            AttachFail::Hard(message) => message,
        })
    }

    /// Adoption with the scan kept tight so the app's window is taken over
    /// within milliseconds of appearing. A JS-side poll loop would flash the
    /// window on the desktop for a whole timer tick before it lands in the
    /// slot; scanning in-process right after each failed attempt cuts that to
    /// roughly one frame.
    ///
    /// After the first ~5s the scan relaxes (10ms → 120ms): a cold IDE start
    /// can take a minute, and tight scanning for that long would just burn
    /// CPU while nothing changes.
    pub fn attach_wait(
        app: &tauri::AppHandle,
        process: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        wait_ms: u64,
    ) -> Result<DesktopWindowInfo, String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        let tight_until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match attach_once(app, process, x, y, width, height, false) {
                Ok(info) => return Ok(info),
                Err(AttachFail::Hard(message)) => return Err(message),
                Err(AttachFail::Empty) => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err(no_window_message(process));
            }
            let gap = if std::time::Instant::now() < tight_until {
                10
            } else {
                120
            };
            std::thread::sleep(std::time::Duration::from_millis(gap));
        }
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
        let current = with_map(|m| {
            m.get(&key).map(|e| {
                (
                    e.hwnd,
                    e.parent,
                    e.pid,
                    e.title.clone(),
                    e.class.clone(),
                    e.style,
                    e.exstyle,
                    e.owner,
                    e.shown,
                )
            })
        });
        let Some((
            old_hwnd,
            entry_parent,
            old_pid,
            title,
            class,
            old_style,
            old_exstyle,
            old_owner,
            entry_shown,
        )) = current
        else {
            return Ok(lost());
        };
        let window = as_hwnd(old_hwnd);
        let (parent, scale) = scale_and_hwnd(app)?;

        if !entry_live(old_hwnd, old_pid) {
            // The frame is gone for good (splash → main window, AWT rebuild).
            with_map(|m| {
                m.remove(&key);
            });
            debug_log(&format!("relayout {key} frame gone, re-adopting"));
            return match attach(app, process, x, y, width, height, true) {
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
            place(window, x, y, width, height, scale, entry_shown != 0);
            let warning = if entry_shown != 0 { verify(window, parent) } else { "" };
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

        place(window, x, y, width, height, scale, entry_shown != 0);
        let warning = if entry_shown != 0 { verify(window, parent) } else { "" };

        // IDE-style startup ( IntelliJ IDEA, some Electron shells ): the
        // window we adopted during launch was the splash, which the app then
        // HIDES while the real frame appears later as a fresh top-level
        // window. The hidden child still exists and is still parented, so
        // neither the gone nor the not-child repair fires. If a genuine main
        // frame of the same process is up now, swap to it. Never fires while
        // we hid the window ourselves (picker dialog): that is not an
        // app-side hide and swapping would drag in some other window.
        if entry_shown != 0 {
            if let Some(found) = find_frame(process) {
                // Swap when the adopted window is unusable (invisible /
                // zero-sized — the splash the app then hid) or when the app has
                // since raised a *better* frame: its workspace window while we
                // still hold the bare-name welcome window.
                let better_workspace = title_is_workspace(&found.title, process)
                    && !title_is_workspace(&title, process);
                if found.hwnd != window
                    && (warning == "invisible" || warning == "size" || better_workspace)
                {
                    let style = unsafe { GetWindowLongPtrW(found.hwnd, GWL_STYLE) };
                    let exstyle = unsafe { GetWindowLongPtrW(found.hwnd, GWL_EXSTYLE) };
                    let owner = unsafe { GetWindowLongPtrW(found.hwnd, GWLP_HWNDPARENT) };
                    match take_over(found.hwnd, parent) {
                        Ok(()) => {
                            debug_log(&format!(
                                "relayout {key} swap {warning} (class={class}) -> hwnd=0x{:X} class={}",
                                found.hwnd.0 as usize,
                                found.class
                            ));
                            place(found.hwnd, x, y, width, height, scale, true);
                            let warning = verify(found.hwnd, parent);
                            release(Embedded {
                                hwnd: old_hwnd,
                                pid: old_pid,
                                title: title.clone(),
                                class: class.clone(),
                                parent: entry_parent,
                                style: old_style,
                                exstyle: old_exstyle,
                                owner: old_owner,
                                shown: entry_shown,
                            });
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
                                        shown: 1,
                                    },
                                );
                            });
                            return Ok(DesktopRelayoutResult {
                                state: "replaced".into(),
                                title: found.title,
                                class: found.class,
                                warning: warning.to_string(),
                            });
                        }
                        Err(error) => {
                            debug_log(&format!(
                                "relayout {key} swap to hwnd=0x{:X} failed: {error}",
                                found.hwnd.0 as usize
                            ));
                        }
                    }
                }
            }
        }

        Ok(DesktopRelayoutResult {
            state: "ok".into(),
            title,
            class,
            warning: warning.to_string(),
        })
    }

    pub fn set_visible(process: &str, visible: bool) -> Result<(), String> {
        let key = map_key(process);
        let live = with_map(|m| m.get(&key).map(|e| (e.hwnd, e.pid)));
        let Some((hwnd, pid)) = live else {
            return Ok(());
        };
        if !entry_live(hwnd, pid) {
            with_map(|m| {
                m.remove(&key);
            });
            return Ok(());
        }
        // Remember the decision: place() must not flip the window back on
        // while the picker dialog relies on it staying hidden.
        with_map(|m| {
            if let Some(entry) = m.get_mut(&key) {
                entry.shown = if visible { 1 } else { 0 };
            }
        });
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
        imp::attach(&app, &process_name, x, y, width, height, false)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, x, y, width, height);
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Adopt with an in-process scan loop, so a window appearing on the desktop is
/// taken over within one frame of showing up (no desktop flash).
#[tauri::command]
pub async fn desktop_embed_attach_wait(
    app: tauri::AppHandle,
    process_name: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    wait_ms: u64,
) -> Result<DesktopWindowInfo, String> {
    #[cfg(windows)]
    {
        let task = tokio::task::spawn_blocking(move || {
            imp::attach_wait(&app, &process_name, x, y, width, height, wait_ms)
        });
        task.await
            .map_err(|e| format!("嵌入失败: {e}"))?
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, x, y, width, height, wait_ms);
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
