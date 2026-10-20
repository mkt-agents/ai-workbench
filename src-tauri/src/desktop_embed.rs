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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_SUCCESS, HWND, LPARAM, POINT, RECT,
    };
    use windows::Win32::Graphics::Gdi::{
        ClientToScreen, RedrawWindow, RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW,
    };
    use windows::Win32::System::Threading::{
        AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetFocus, keybd_event, SetActiveWindow, SetFocus, KEYEVENTF_KEYUP, VK_MENU,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetClientRect, GetForegroundWindow, GetParent, GetWindow,
        GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, GetWindowTextW,
        GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindow, IsWindowVisible, IsZoomed,
        SetForegroundWindow, SetParent, SetWindowLongPtrW, SetWindowPlacement, SetWindowPos,
        ShowWindow, GWLP_HWNDPARENT, GWL_EXSTYLE, GWL_STYLE, GW_OWNER, HWND_TOP, SWP_FRAMECHANGED,
        SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
        SW_HIDE, SW_RESTORE, SW_SHOWNA, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOWPLACEMENT,
        WINDOW_LONG_PTR_INDEX, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_APPWINDOW,
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
        /// Input-queue thread of the hosted window, attached to the host
        /// thread's queue at take_over time (0 = same thread / not attached).
        /// release() must detach the pair or the two threads keep sharing
        /// focus state after the window is back on the desktop.
        attached_thread: u32,
        /// The window's original placement, captured before take_over touched
        /// it: a maximized-adopted window must be handed back maximized.
        placement: WINDOWPLACEMENT,
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

    /// Last `(hwnd, x, y, w, h)` written to the trace, so the 400ms heartbeat
    /// does not flood the log with identical lines. Diagnostic only.
    static LAST_PLACE: Mutex<Option<(isize, i32, i32, i32, i32)>> = Mutex::new(None);

    /// The rect whose move a settle-nudge is owed. A rapid slot-move run
    /// (sidebar toggle animation) can leave Chromium's compositor anchored to
    /// the OLD window position — its content stays shifted by the move delta
    /// (blank strip, clicks land beside the visible UI) even though the
    /// window rect itself converged. The first tick whose rect repeats
    /// (animation settled) gets a 1px out-and-back: two fresh real
    /// WM_WINDOWPOSCHANGED/WM_MOVE pairs on an idle pump re-anchor it.
    static NUDGE_PENDING: Mutex<Option<(isize, i32, i32, i32, i32)>> = Mutex::new(None);

    /// Last `(hwnd, verify-code)` written to the trace, so a persisting warning
    /// is logged once instead of every 400ms heartbeat. Diagnostic only.
    static LAST_WARN: Mutex<Option<(isize, &'static str)>> = Mutex::new(None);

    /// Last full `EnumWindows` scan per process. relayout's heartbeat asks
    /// every 400ms, but the frame-swap check does not need that rate: a routine
    /// look for a better workspace window every few seconds is enough, while a
    /// broken window (the app hid its splash) is rechecked quickly.
    static FRAME_SCAN_AT: Mutex<Option<HashMap<String, std::time::Instant>>> = Mutex::new(None);

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

    /// True when a full process window scan is due for `key`. Urgent scans
    /// (blank slot) run at most every 500ms; routine ones every 5s.
    fn frame_scan_due(key: &str, urgent: bool) -> bool {
        let now = std::time::Instant::now();
        let gap = if urgent {
            std::time::Duration::from_millis(500)
        } else {
            std::time::Duration::from_secs(5)
        };
        let mut guard = FRAME_SCAN_AT
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let scans = guard.get_or_insert_with(HashMap::new);
        match scans.get(key) {
            Some(last) if now.duration_since(*last) < gap => false,
            _ => {
                scans.insert(key.to_string(), now);
                true
            }
        }
    }

    fn scale_and_hwnd(app: &tauri::AppHandle) -> Result<(HWND, f64), String> {
        let window = app.get_window("main").ok_or("主窗口不存在")?;
        let scale = window.scale_factor().unwrap_or(1.0);
        let raw = window
            .hwnd()
            .map_err(|e| format!("无法获取主窗口句柄: {e}"))?;
        Ok((HWND(raw.0 as *mut _), scale))
    }

    /// Image name (without extension) of a running process, or `None` if gone.
    fn image_stem(pid: u32) -> Option<String> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
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
            if IsZoomed(hwnd).as_bool() {
                return "maximized";
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
        /// The window the user is looking at right now. relayout's
        /// better-workspace swap only follows a frame the app actually raised:
        /// a background helper titled "… — AppName" must never steal the slot.
        foreground: bool,
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

    /// Same shape as `looks_like_main_frame` but the window is hidden right
    /// now. Only a *workspace-titled* frame qualifies: adopting reveals what we
    /// take, so a hidden splash or bare-name window must never be resurrected
    /// onto the desktop — only the app's real document frame is worth it.
    fn looks_like_main_frame_hidden(c: &Candidate, process: &str) -> bool {
        !c.visible
            && !c.iconic
            && !c.owned
            && !c.toolwindow
            && !is_helper_class(&c.class)
            && c.area >= MIN_FRAME_AREA
            && (c.style & (WS_CAPTION.0 as isize | WS_THICKFRAME.0 as isize)) != 0
            && title_is_workspace(&c.title, process)
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

    /// A window that can never render the app's UI, so it must not be offered
    /// for adoption: Chromium/Electron's background, tray-host and power-message
    /// windows, crash handlers and the DDE server window, plus anything untitled
    /// that the app itself keeps hidden. A real frame is either visible or
    /// titled, so nothing the user could mean is caught here.
    fn is_non_ui_window(c: &Candidate) -> bool {
        is_helper_class(&c.class)
            || matches!(
                c.class.as_str(),
                "Chrome_WidgetWin_0"
                    | "Electron_NotifyIconHostWindow"
                    | "Base_PowerMessageWindow"
                    | "OleDdeWndClass"
            )
            || c.class.starts_with("crashpad_")
            || (c.title.trim().is_empty() && !c.visible)
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
        let mut ctx = Ctx {
            needle,
            out: Vec::new(),
        };
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
            // Last resort: the app hid its own workspace frame (closed to tray,
            // or the picker hid it). Adopting reveals it again.
            .or_else(|| pick(&|c| is_exact(c) && looks_like_main_frame_hidden(c, &exact)))
            .map(|best| Found {
                hwnd: best.hwnd,
                pid: best.pid,
                title: best.title,
                class: best.class,
                foreground: best.foreground,
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
            // A hidden workspace frame counts as a frame too: the app itself hid
            // its main window (tray) and the re-adopt must still find it. relayout
            // only swaps while WE keep the window shown, so this cannot drag the
            // picker's own hidden window back in front of the dialog.
            .or_else(|| pick(&|c| is_exact(c) && looks_like_main_frame_hidden(c, &exact)))
            .map(|best| Found {
                hwnd: best.hwnd,
                pid: best.pid,
                title: best.title,
                class: best.class,
                foreground: best.foreground,
            })
    }

    pub fn describe_candidates(process: &str) -> Vec<DesktopWindowCandidate> {
        let key = map_key(process);
        let current_entry = with_map(|m| {
            m.get(&key)
                .filter(|e| entry_live(e.hwnd, e.pid))
                .map(|e| e.hwnd)
        });
        let current_hwnd = current_entry;
        // What the auto-pick would adopt right now, so the picker can badge it.
        let recommended = find_window(process).map(|f| f.hwnd.0 as isize);
        let mut out: Vec<DesktopWindowCandidate> = candidates(process)
            .into_iter()
            // Helper classes (IME/tooltip/shadow) and non-UI windows (Chromium
            // background, tray host, crash handler…) can never be adopted
            // meaningfully — drop them so the picker stays readable. Same for
            // invisible zero-area stubs: embedding one only yields a blank slot.
            .filter(|c| {
                (current_hwnd == Some(c.hwnd.0 as isize) || !is_non_ui_window(c))
                    && (c.visible || c.area > 0)
            })
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
            .collect();

        // The hosted window is a child of our own window now, so EnumWindows no
        // longer enumerates it — put it back so the picker can mark it as the
        // current window instead of showing only the leftover helper windows.
        if let Some(hwnd) = current_entry {
            let raw = as_hwnd(hwnd);
            let hex = format!("0x{:X}", hwnd as usize);
            if !out.iter().any(|c| c.hwnd == hex) {
                let (width, height) = window_size(raw);
                let mut pid = 0u32;
                unsafe { GetWindowThreadProcessId(raw, Some(&mut pid)) };
                out.insert(
                    0,
                    DesktopWindowCandidate {
                        process_name: process.trim().to_string(),
                        pid,
                        hwnd: hex,
                        class: class_name(raw),
                        title: window_text(raw),
                        visible: unsafe { IsWindowVisible(raw) }.as_bool(),
                        owned: false,
                        area: width * height,
                        width,
                        height,
                        recommended: false,
                        current: true,
                    },
                );
            }
        }
        out
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
            let key = (hwnd.0 as isize, px, py, pw, ph);
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), px, py, pw, ph, SWP_NOACTIVATE | visibility);
            // A window adopted while maximized (Chromium re-asserts its own
            // maximized placement from inside the WM_WINDOWPOSCHANGING sync of
            // our SetWindowPos above) keeps fighting every subsequent place:
            // the rect we just wrote is replaced by the maximized screen rect
            // on the next sync, so the slot drifts and will not come back even
            // through re-attach. Force the normal placement, re-place, and
            // repaint the invalidated slot. The window is a hidden/child window
            // here, so no restore flash can reach the desktop.
            if IsZoomed(hwnd).as_bool() {
                let mut wp = WINDOWPLACEMENT {
                    length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                    ..Default::default()
                };
                if GetWindowPlacement(hwnd, &mut wp).is_ok() {
                    wp.showCmd = SW_SHOWNORMAL.0 as u32;
                    let _ = SetWindowPlacement(hwnd, &wp);
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOP),
                        px,
                        py,
                        pw,
                        ph,
                        SWP_NOACTIVATE | visibility,
                    );
                }
                let _ = RedrawWindow(
                    Some(hwnd),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
                );
            }
            // Settle nudge (see NUDGE_PENDING): arm on every changed rect, fire
            // once on the first unchanged tick while the window is shown.
            let settled = NUDGE_PENDING
                .lock()
                .map(|mut g| {
                    if *g == Some(key) {
                        *g = None;
                        true
                    } else {
                        *g = Some(key);
                        false
                    }
                })
                .unwrap_or(false);
            if settled && show {
                let _ = SetWindowPos(hwnd, None, px - 1, py, pw, ph, SWP_NOACTIVATE | SWP_NOZORDER);
                let _ = SetWindowPos(hwnd, None, px, py, pw, ph, SWP_NOACTIVATE | SWP_NOZORDER);
                debug_log(&format!("place 0x{:X} settle nudge", key.0));
            }
            // Diagnostic (temp): target vs actual rect, plus the live display
            // state. Throttled on unchanged target so the 400ms heartbeat does
            // not flood the log; a persistent mismatch (drift) re-logs.
            let key = (hwnd.0 as isize, px, py, pw, ph);
            let mut actual = RECT::default();
            let measured = GetWindowRect(hwnd, &mut actual).is_ok();
            let mut origin = POINT::default();
            let host = GetParent(hwnd).unwrap_or_default();
            let in_client = !host.is_invalid() && ClientToScreen(host, &mut origin).as_bool();
            let mut client = RECT::default();
            let client_size =
                if !host.is_invalid() && GetClientRect(host, &mut client).is_ok() {
                    format!("client=({},{})", client.right, client.bottom)
                } else {
                    "client=(?,?)".into()
                };
            let (ax, ay, aw, ah) = (
                actual.left - origin.x,
                actual.top - origin.y,
                actual.right - actual.left,
                actual.bottom - actual.top,
            );
            let drifted = measured && in_client && (ax, ay, aw, ah) != (px, py, pw, ph);
            let changed = LAST_PLACE
                .lock()
                .map(|mut g| {
                    let changed = *g != Some(key);
                    if changed {
                        *g = Some(key);
                    }
                    changed
                })
                .unwrap_or(true);
            if changed || drifted {
                debug_log(&format!(
                    "place 0x{:X} target=({px},{py},{pw},{ph}) dpr={scale:.2} actual=({ax},{ay},{aw},{ah}) {client_size} zoomed={} iconic={} show={show}",
                    key.0,
                    IsZoomed(hwnd).as_bool(),
                    IsIconic(hwnd).as_bool(),
                ));
            }
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
            // Detach the input queues first, while the window is still ours:
            // the two threads must stop sharing focus/activation state before
            // the window is a top-level frame again.
            if entry.attached_thread != 0 {
                let host_thread = GetWindowThreadProcessId(as_hwnd(entry.parent), None);
                if host_thread != 0 && host_thread != entry.attached_thread {
                    let _ = AttachThreadInput(entry.attached_thread, host_thread, false);
                }
            }
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
            // Restore the ORIGINAL placement, not just style bits: a window
            // adopted maximized had its placement forced to normal for the
            // embed; handing it back with a restored WS_MAXIMIZE style bit but
            // a normal placement is the exact mismatch that used to cause the
            // slot drift. A hidden-now window is never resurrected: the app
            // may have hidden it while embedded (a splash being disposed).
            let mut placement = entry.placement;
            placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            if current_style & WS_VISIBLE.0 as isize == 0 {
                placement.showCmd = SW_HIDE.0 as u32;
            }
            let _ = SetWindowPlacement(hwnd, &placement);
            let placed_normal = placement.showCmd == SW_SHOWNORMAL.0 as u32;
            // Same hang guard as place(): a window that is mid-shutdown can
            // never answer the synchronous messages of SetWindowPos, which
            // would block backend threads until TerminateProcess gets there.
            if !IsHungAppWindow(hwnd).as_bool() {
                let mut flags = SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOZORDER;
                if !placed || !placed_normal {
                    // Maximized/minimized windows ignore move/size anyway, and
                    // the placement above already carries the original rect.
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

    /// Re-send `WM_SETFOCUS` to the hosted window.
    ///
    /// `SetFocus` alone can be a no-op from the app's point of view: the
    /// message only re-registers the app's input when the window cycles
    /// hide → focus → show (an already-visible focused window shown with
    /// `SW_SHOWNOACTIVATE` receives nothing — the same recipe as the show
    /// path in `set_visible`, which is the one that empirically works).
    /// Never sleeps: callers pace retries from background threads.
    /// Returns whether the window ended up holding the (shared) queue focus.
    fn sync_hosted_focus(hwnd: HWND, attached: bool) -> bool {
        unsafe {
            // Never resurrect a window the page hid on purpose (picker dialog,
            // tab switch): the show below would paint it over the webview.
            if !IsWindowVisible(hwnd).as_bool() || IsHungAppWindow(hwnd).as_bool() {
                return false;
            }
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_HIDEWINDOW,
            );
            let _ = SetFocus(Some(hwnd));
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            !attached || GetFocus() == hwnd
        }
    }

    /// Idempotent queue attach (see take_over): safe to call every refocus
    /// round — an already-attached pair just succeeds again. A busy launch
    /// window refuses `AttachThreadInput` for a while ("unread input"), which
    /// is exactly when the app is launched and adopted; retrying here until
    /// it lands is what keeps switching between embedded apps typeable.
    /// Returns whether the hosted window's queue is shared with the host's.
    fn ensure_queue_shared(parent: HWND, hwnd: HWND, child_thread: u32) -> bool {
        if child_thread == 0 {
            return true; // same thread as the host
        }
        let host_thread = unsafe { GetWindowThreadProcessId(parent, None) };
        if host_thread == 0 || host_thread == child_thread {
            return true;
        }
        let ok = unsafe { AttachThreadInput(child_thread, host_thread, true) }.as_bool();
        if !ok {
            debug_log(&format!(
                "queue attach retry failed hwnd=0x{:X}",
                hwnd.0 as usize
            ));
        }
        ok
    }

    /// Make the workbench the foreground/active window, working around the
    /// Windows foreground lock that a freshly launched app leaves behind.
    ///
    /// When we adopt a just-launched app, its window held the foreground and
    /// consumed our foreground rights: a plain `SetForegroundWindow` from the
    /// now-background workbench process fails, activation dangles on a window
    /// that is no longer top-level, and no queue can establish focus — the
    /// embedded app is typing-dead until the user happens to re-activate the
    /// workbench (which is why detach → re-embed "fixes" it). Two documented
    /// unlocks, tried in order: attach to the current foreground queue, then
    /// the ALT-key trick when the foreground is dangling (null/stale).
    fn raise_host(parent: HWND) -> bool {
        unsafe {
            if GetForegroundWindow() == parent {
                return true;
            }
            let this_thread = GetCurrentThreadId();
            let fg = GetForegroundWindow();
            if !fg.is_invalid() {
                let fg_thread = GetWindowThreadProcessId(fg, None);
                let mut attached = false;
                if fg_thread != 0 && fg_thread != this_thread {
                    attached = AttachThreadInput(this_thread, fg_thread, true).as_bool();
                }
                let ok = SetForegroundWindow(parent).as_bool();
                if attached {
                    let _ = AttachThreadInput(this_thread, fg_thread, false);
                }
                if ok {
                    let _ = SetActiveWindow(parent);
                    return true;
                }
            }
            // Foreground dangling (null or a stale child pointer): briefly tap
            // ALT so the system accepts a background process's activation.
            keybd_event(VK_MENU.0 as u8, 0, Default::default(), 0);
            let ok = SetForegroundWindow(parent).as_bool();
            keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            if ok {
                let _ = SetActiveWindow(parent);
            }
            ok
        }
    }

    /// Give the hosted window keyboard focus when the workbench itself is the
    /// active window.
    ///
    /// Cross-process parenting keeps the hosted window in its own input queue
    /// until the queues are attached (see take_over); even attached, focus
    /// never moves on its own — a freshly embedded app sits there unable to
    /// receive typing until the user clicks it. Hand focus over at the moments
    /// the user's intent is obvious: right after adopting, re-parenting or a
    /// frame swap. Runs on the main thread (focus belongs to the calling
    /// thread's attached queue — the hosted window is attached to the
    /// host/main queue), and guarded on the host being foreground so the 400ms
    /// heartbeat can never yank focus out of the workbench's own inputs.
    ///
    /// Guarded on the host being foreground so the 400ms heartbeat can never
    /// yank focus out of the workbench's own inputs; when the host is NOT in
    /// front (a freshly launched app consumed the foreground), `raise_host`
    /// takes it back with one of the documented unlocks instead of silently
    /// giving up — that bail-out was why launch-embed left the app deaf while
    /// detach → re-embed (user click re-activated the host) worked.
    fn focus_hosted(app: &tauri::AppHandle, parent: HWND, hwnd: HWND, child_thread: u32) {
        // Raw HWND is not Send: carry the handles as isize across the closure.
        let parent_raw = parent.0 as isize;
        let hwnd_raw = hwnd.0 as isize;
        let _ = app.run_on_main_thread(move || unsafe {
            let parent = as_hwnd(parent_raw);
            let hwnd = as_hwnd(hwnd_raw);
            if IsIconic(hwnd).as_bool() {
                return;
            }
            let fg_before = GetForegroundWindow();
            let raised = raise_host(parent);
            let attached = ensure_queue_shared(parent, hwnd, child_thread);
            let _ = SetFocus(Some(hwnd));
            let nudged = sync_hosted_focus(hwnd, attached);
            debug_log(&format!(
                "focus 0x{:X} fg_before=0x{:X} raised={raised} attached={attached} focused={} nudged={nudged}",
                hwnd.0 as usize,
                fg_before.0 as usize,
                GetFocus() == hwnd
            ));
        });
    }

    /// Follow-up focus campaign for adoptions that land while the app is still
    /// starting (launch-embed): the first `WM_SETFOCUS` can arrive before the
    /// app's input pipeline is ready, the foreground/activation state can
    /// still be dangling from the takeover, and `AttachThreadInput` can keep
    /// failing while the busy app has unread input. Every round re-tries the
    /// queue attach FIRST (even with the host not in front — the user will
    /// come back and click), then re-nudges, paced from a background thread.
    fn refocus(app: &tauri::AppHandle, parent: HWND, hwnd: HWND, child_thread: u32) {
        let app = app.clone();
        let parent_raw = parent.0 as isize;
        let hwnd_raw = hwnd.0 as isize;
        std::thread::spawn(move || {
            let mut elapsed = 0u64;
            for at in [300u64, 800, 1600] {
                std::thread::sleep(std::time::Duration::from_millis(at - elapsed));
                elapsed = at;
                // Queue attach retry runs on this background thread: it does
                // not need the UI thread, and it must happen even while the
                // user is elsewhere (the guards below would skip the round).
                let attached =
                    ensure_queue_shared(as_hwnd(parent_raw), as_hwnd(hwnd_raw), child_thread);
                let _ = app.run_on_main_thread(move || unsafe {
                    let parent = as_hwnd(parent_raw);
                    let hwnd = as_hwnd(hwnd_raw);
                    if IsIconic(hwnd).as_bool()
                        || IsHungAppWindow(hwnd).as_bool()
                        // The page may have hidden the window meanwhile
                        // (picker open): nudging would show it again.
                        || !IsWindowVisible(hwnd).as_bool()
                    {
                        return;
                    }
                    if GetForegroundWindow() != parent {
                        // The user is elsewhere: nothing to fix here, and the
                        // desktop_embed_focus command covers the come-back.
                        return;
                    }
                    let nudged = sync_hosted_focus(hwnd, attached);
                    debug_log(&format!(
                        "refocus 0x{:X} focused={} nudged={nudged}",
                        hwnd.0 as usize,
                        GetFocus() == hwnd
                    ));
                });
            }
        });
    }

    /// Write one style word, treating "returned 0" as the failure it may be.
    ///
    /// `SetWindowLongPtrW` returns the previous value, so a legitimately-zero
    /// old style is indistinguishable from failure without the last-error
    /// round trip. Used on the way into a takeover, where a silent failure
    /// would leave the target window frameless but still top-level.
    fn set_style_checked(
        hwnd: HWND,
        index: WINDOW_LONG_PTR_INDEX,
        value: isize,
    ) -> Result<(), String> {
        unsafe {
            SetLastError(ERROR_SUCCESS);
            SetWindowLongPtrW(hwnd, index, value);
            let error = GetLastError();
            if error != ERROR_SUCCESS {
                return Err(format!("SetWindowLongPtrW({index:?}) 失败: {error:?}"));
            }
        }
        Ok(())
    }

    /// Put a window back exactly as `take_over` found it, after a failed
    /// takeover. Restoring the placement also restores the show state, but
    /// the style was captured after the window was hidden, so explicitly show
    /// it again when it was visible before.
    fn restore_taken_state(
        hwnd: HWND,
        host: HWND,
        style: isize,
        exstyle: isize,
        host_style: isize,
        host_style_changed: bool,
        placement: &WINDOWPLACEMENT,
        was_visible: bool,
    ) {
        unsafe {
            let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, style);
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, exstyle);
            if host_style_changed {
                let _ = SetWindowLongPtrW(host, GWL_STYLE, host_style);
            }
            let _ = SetWindowPlacement(hwnd, placement);
            if was_visible {
                let _ = ShowWindow(hwnd, SW_SHOWNA);
            }
        }
    }

    /// Strip the standalone frame off `hwnd` and parent it into `parent`.
    ///
    /// Kept separate from `place` because the app can undo this behind our back:
    /// Chromium re-asserts its own top-level frame on some window events, and
    /// `relayout` puts the window back with this same call.
    ///
    /// Returns the hosted window's input-queue thread id (0 when it matches the
    /// host's). The queue attach is attempted here with a short retry; a busy
    /// launch window can still refuse it, in which case the id is returned
    /// anyway and the refocus campaign keeps retrying (see ensure_queue_shared)
    /// — without a shared queue, clicks and SetFocus cannot reach the app.
    /// `release` must detach the pair.
    ///
    /// Also returns the window's ORIGINAL `WINDOWPLACEMENT`, captured before
    /// anything touches it: `release` must hand a maximized-adopted window
    /// back as maximized — restoring only the WS_MAXIMIZE style bit leaves the
    /// style inconsistent with the (restored-to-normal) placement, the exact
    /// mismatch that caused the slot drift.
    fn take_over(hwnd: HWND, parent: HWND) -> Result<(u32, WINDOWPLACEMENT), String> {
        unsafe {
            // Original placement, captured before anything touches the window.
            let mut original = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            if GetWindowPlacement(hwnd, &mut original).is_err() {
                original.showCmd = SW_SHOWNORMAL.0 as u32;
            }
            let was_visible = IsWindowVisible(hwnd).as_bool();
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
            //
            // Clearing the WS_MAXIMIZE style bit alone is not enough: the app
            // (Chromium) still believes it is maximized and re-applies the
            // maximized screen rect on its own WM_WINDOWPOSCHANGING sync, which
            // is the slot drift that place() then has to fight. Really leave
            // the zoomed/minimized placement via SetWindowPlacement; it may
            // show the still top-level window, so hide it again immediately —
            // both calls run in the same pump iteration and no WM_PAINT lands
            // in between, so nothing is ever painted on the desktop.
            if IsZoomed(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                if original.showCmd != SW_SHOWNORMAL.0 as u32 {
                    let mut wp = original;
                    wp.showCmd = SW_SHOWNORMAL.0 as u32;
                    let _ = SetWindowPlacement(hwnd, &wp);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
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
            let host_style = GetWindowLongPtrW(parent, GWL_STYLE);
            let host_style_changed = host_style & WS_CLIPCHILDREN.0 as isize == 0;
            // Deliberately no WS_VISIBLE: `place` decides visibility, so the
            // window cannot flash onto the desktop between here and SetParent.
            //
            // Every write is checked and rolled back: a target running at a
            // higher integrity level refuses these calls (UIPI), and stopping
            // half-way would leave the user's app hidden and frameless with no
            // way back except restarting it.
            if let Err(error) = set_style_checked(hwnd, GWL_STYLE, child_style) {
                restore_taken_state(
                    hwnd,
                    parent,
                    style,
                    exstyle,
                    host_style,
                    false,
                    &original,
                    was_visible,
                );
                return Err(format!(
                    "无法修改目标窗口样式，它可能以管理员权限运行（{error}）"
                ));
            }
            // WS_EX_LAYERED has to go: a layered window only composites through
            // UpdateLayeredWindow against a layered parent, so as a child of an
            // ordinary window it draws nothing at all (blank slot).
            if let Err(error) = set_style_checked(
                hwnd,
                GWL_EXSTYLE,
                exstyle & !(WS_EX_APPWINDOW.0 as isize | WS_EX_LAYERED.0 as isize),
            ) {
                restore_taken_state(
                    hwnd,
                    parent,
                    style,
                    exstyle,
                    host_style,
                    false,
                    &original,
                    was_visible,
                );
                return Err(format!(
                    "无法修改目标窗口样式，它可能以管理员权限运行（{error}）"
                ));
            }
            // The host must clip children, or Chromium paints over the
            // sibling webview that draws the surrounding page.
            if host_style_changed {
                if let Err(error) = set_style_checked(
                    parent,
                    GWL_STYLE,
                    host_style | WS_CLIPCHILDREN.0 as isize,
                ) {
                    restore_taken_state(
                        hwnd,
                        parent,
                        style,
                        exstyle,
                        host_style,
                        true,
                        &original,
                        was_visible,
                    );
                    return Err(format!("无法更新工作台窗口样式（{error}）"));
                }
            }
            if let Err(error) = SetParent(hwnd, Some(parent)) {
                restore_taken_state(
                    hwnd,
                    parent,
                    style,
                    exstyle,
                    host_style,
                    host_style_changed,
                    &original,
                    was_visible,
                );
                // ERROR_ACCESS_DENIED: an elevated target rejects the
                // reparent from this (lower-integrity) process.
                let denied = (error.code().0 as u32) & 0xFFFF == 5;
                return Err(if denied {
                    "接管被系统拒绝：目标应用可能以管理员权限运行，请以管理员身份重启工作台后再试"
                        .into()
                } else {
                    format!("SetParent: {error}")
                });
            }
            // Share the input queues: the hosted window's thread keeps its own
            // queue, and focus/activation state is per-queue — without the
            // attach, clicks on the hosted app cannot take keyboard focus and
            // every keystroke stays in the webview.
            let host_thread = GetWindowThreadProcessId(parent, None);
            let child_thread = GetWindowThreadProcessId(hwnd, None);
            if host_thread != 0 && child_thread != 0 && host_thread != child_thread {
                // `AttachThreadInput` fails while either queue has unread input
                // (the just-launched app gets input right away), and the
                // return value is easy to miss: one attempt left the queues
                // unshared, and focus then never travelled into the app. Retry
                // briefly here; the refocus campaign keeps retrying after — a
                // busy launch window refuses for far longer than this loop.
                let mut joined = false;
                for attempt in 0..10 {
                    if attempt > 0 {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    if AttachThreadInput(child_thread, host_thread, true).as_bool() {
                        joined = true;
                        break;
                    }
                }
                if !joined {
                    debug_log(&format!(
                        "take_over hwnd=0x{:X} queue attach failed, will retry on refocus",
                        hwnd.0 as usize
                    ));
                }
                // Report the foreign thread id either way: `release`'s detach
                // is a harmless no-op on a never-attached pair, and the focus
                // campaign needs the id to keep retrying the attach.
                return Ok((child_thread, original));
            }
            Ok((0, original))
        }
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
        dpr: Option<f64>,
    ) -> Result<DesktopWindowInfo, String> {
        let key = map_key(process);
        if key.is_empty() {
            return Err("进程名不能为空".into());
        }
        let text = hwnd_raw.trim();
        let hwnd = as_hwnd(
            usize::from_str_radix(text.trim_start_matches("0x").trim_start_matches("0X"), 16)
                .map_err(|_| format!("无效的窗口句柄: {text}"))? as isize,
        );
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err("这个窗口已经不存在了".into());
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return Err("无法读取这个窗口所属的进程".into());
        }
        if pid == std::process::id() {
            return Err("不能嵌入工作台自己的窗口".into());
        }
        // A handle from another app's candidate list must never be adopted
        // under this app's key: that is how a stale picker used to put A's
        // window into B's slot. Same matching rule as the candidate scan.
        let stem = image_stem(pid).ok_or("目标窗口的进程已经退出")?;
        if !stem.to_ascii_lowercase().contains(&key) {
            return Err(format!(
                "这个窗口属于「{stem}」，不是「{process}」，未接管"
            ));
        }
        let (parent, scale) = scale_and_hwnd(app)?;
        let scale = dpr.filter(|d| *d > 0.0).unwrap_or(scale);

        // Re-picking the window already in the slot is a no-op (and must be
        // checked before the WS_CHILD reject: our own children are WS_CHILD).
        let existing = with_map(|m| {
            m.get(&key)
                .map(|e| (e.hwnd, e.pid, e.title.clone(), e.class.clone(), e.shown))
        });
        if let Some((current, current_pid, title, class, shown)) = existing {
            if current == hwnd.0 as isize && entry_live(current, current_pid) {
                place(as_hwnd(current), x, y, width, height, scale, shown != 0);
                let warning = verify(as_hwnd(current), parent);
                return Ok(DesktopWindowInfo {
                    process_name: process.to_string(),
                    pid: current_pid,
                    title,
                    class,
                    attached: true,
                    warning: warning.to_string(),
                });
            }
        }

        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
        if style & WS_CHILD.0 as isize != 0 {
            return Err("这个窗口已经是子窗口，不能再次嵌入".into());
        }
        let exstyle = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        let owner = unsafe { GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT) };
        let (attached_thread, placement) = take_over(hwnd, parent)
            .map_err(|e| format!("无法接管 {process} 的窗口（{text}）: {e}"))?;
        place(hwnd, x, y, width, height, scale, true);
        focus_hosted(app, parent, hwnd, attached_thread);
        refocus(app, parent, hwnd, attached_thread);
        let class = class_name(hwnd);
        let title = window_text(hwnd);
        let warning = verify(hwnd, parent);
        debug_log(&format!(
            "attach_window {key} hwnd=0x{:X} class={class} title={title} warning={warning:?}",
            hwnd.0 as usize
        ));
        // The previous window for this app steps aside only after the new one
        // is actually ours: a failed takeover must leave that embedding (and
        // the user's window) untouched rather than dropping it on the floor.
        let previous = with_map(|m| {
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
                    attached_thread,
                    placement,
                },
            )
        });
        if let Some(previous) = previous {
            if previous.hwnd != hwnd.0 as isize {
                release(previous);
            }
        }
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
        dpr: Option<f64>,
    ) -> Result<DesktopWindowInfo, AttachFail> {
        let key = map_key(process);
        if key.is_empty() {
            return Err(AttachFail::Hard("进程名不能为空".into()));
        }
        let (parent, scale) = scale_and_hwnd(app).map_err(|e| AttachFail::Hard(e.to_string()))?;
        let scale = dpr.filter(|d| *d > 0.0).unwrap_or(scale);

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
        let (attached_thread, placement) = take_over(found.hwnd, parent).map_err(|e| {
            debug_log(&format!(
                "attach {key} hwnd=0x{:X} class={} FAILED {e}",
                found.hwnd.0 as usize, found.class
            ));
            AttachFail::Hard(format!("无法接管 {process} 的窗口（{}）: {e}", found.class))
        })?;
        place(found.hwnd, x, y, width, height, scale, true);
        focus_hosted(app, parent, found.hwnd, attached_thread);
        refocus(app, parent, found.hwnd, attached_thread);
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
                    attached_thread,
                    placement,
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
    ///
    /// `dpr` is the webview's `devicePixelRatio` (DPI scale × page zoom); when
    /// present it replaces the bare `window.scale_factor()` for the CSS px →
    /// physical px mapping, so a zoomed page or a per-monitor DPI change keeps
    /// the hosted window glued to the slot.
    pub fn attach(
        app: &tauri::AppHandle,
        process: &str,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        strict: bool,
        dpr: Option<f64>,
    ) -> Result<DesktopWindowInfo, String> {
        attach_once(app, process, x, y, width, height, strict, dpr).map_err(|fail| match fail {
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
        dpr: Option<f64>,
    ) -> Result<DesktopWindowInfo, String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        let tight_until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match attach_once(app, process, x, y, width, height, false, dpr) {
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
        dpr: Option<f64>,
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
                    e.attached_thread,
                    e.placement,
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
            old_attached_thread,
            old_placement,
        )) = current
        else {
            return Ok(lost());
        };
        let window = as_hwnd(old_hwnd);
        let (parent, scale) = scale_and_hwnd(app)?;
        let scale = dpr.filter(|d| *d > 0.0).unwrap_or(scale);

        if !entry_live(old_hwnd, old_pid) {
            // The frame is gone for good (splash → main window, AWT rebuild).
            with_map(|m| {
                m.remove(&key);
            });
            debug_log(&format!("relayout {key} frame gone, re-adopting"));
            return match attach(app, process, x, y, width, height, true, dpr) {
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
            // The app put its frame back: take it over again. The entry keeps
            // its ORIGINAL placement — the re-takeover's snapshot is already
            // of a child window, not the pre-embed state.
            if take_over(window, parent).is_err() {
                debug_log(&format!("relayout {key} re-parent failed"));
                return Ok(lost());
            }
            place(window, x, y, width, height, scale, entry_shown != 0);
            if entry_shown != 0 {
                focus_hosted(app, parent, window, old_attached_thread);
                refocus(app, parent, window, old_attached_thread);
            }
            let warning = if entry_shown != 0 {
                verify(window, parent)
            } else {
                ""
            };
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
        let warning = if entry_shown != 0 {
            verify(window, parent)
        } else {
            ""
        };
        // Diagnostic (temp): log a verify() conclusion only when it flips, so
        // the 400ms heartbeat stays quiet but any drift into a broken display
        // state (minimized/maximized/invisible/…) hits the trace once.
        let warned = LAST_WARN
            .lock()
            .map(|mut g| {
                let changed = *g != Some((old_hwnd, warning));
                if changed {
                    *g = Some((old_hwnd, warning));
                }
                changed
            })
            .unwrap_or(true);
        if !warning.is_empty() && warned {
            debug_log(&format!(
                "relayout {key} warning={warning:?} target=({x:.0},{y:.0},{width:.0},{height:.0})"
            ));
        }

        // IDE-style startup ( IntelliJ IDEA, some Electron shells ): the
        // window we adopted during launch was the splash, which the app then
        // HIDES while the real frame appears later as a fresh top-level
        // window. The hidden child still exists and is still parented, so
        // neither the gone nor the not-child repair fires. If a genuine main
        // frame of the same process is up now, swap to it. Never fires while
        // we hid the window ourselves (picker dialog): that is not an
        // app-side hide and swapping would drag in some other window.
        if entry_shown != 0
            && frame_scan_due(&key, warning == "invisible" || warning == "size")
        {
            if let Some(found) = find_frame(process) {
                // Swap when the adopted window is unusable (invisible /
                // zero-sized — the splash the app then hid) or when the app has
                // since raised a *better* frame: its workspace window while we
                // still hold the bare-name welcome window.
                let better_workspace = found.foreground
                    && title_is_workspace(&found.title, process)
                    && !title_is_workspace(&title, process);
                if found.hwnd != window
                    && (warning == "invisible" || warning == "size" || better_workspace)
                {
                    let style = unsafe { GetWindowLongPtrW(found.hwnd, GWL_STYLE) };
                    let exstyle = unsafe { GetWindowLongPtrW(found.hwnd, GWL_EXSTYLE) };
                    let owner = unsafe { GetWindowLongPtrW(found.hwnd, GWLP_HWNDPARENT) };
                    match take_over(found.hwnd, parent) {
                        Ok((attached_thread, placement)) => {
                            debug_log(&format!(
                                "relayout {key} swap {warning} (class={class}) -> hwnd=0x{:X} class={}",
                                found.hwnd.0 as usize,
                                found.class
                            ));
                            place(found.hwnd, x, y, width, height, scale, true);
                            focus_hosted(app, parent, found.hwnd, attached_thread);
                            refocus(app, parent, found.hwnd, attached_thread);
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
                                attached_thread: old_attached_thread,
                                placement: old_placement,
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
                                        attached_thread,
                                        placement,
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
            | if visible {
                SWP_SHOWWINDOW
            } else {
                SWP_HIDEWINDOW
            };
        let child_thread =
            with_map(|m| m.get(&key).map(|e| e.attached_thread)).unwrap_or(0);
        unsafe {
            let hosted = as_hwnd(hwnd);
            let _ = SetWindowPos(hosted, None, 0, 0, 0, 0, flags);
            // A window cannot keep its focus once it is hidden: hiding it moves
            // focus to the parent, so showing it again leaves the app with no
            // focused element and it drops every keystroke until a click. The
            // hide → focus → show cycle below makes the window receive
            // WM_SETFOCUS again, which is what makes the app register its
            // input. Done only while the host is in front (tab switch / picker
            // close / confirm dialog — the user is back here), never while we
            // sink into the background.
            if visible && GetForegroundWindow() == GetParent(hosted).unwrap_or_default() {
                let attached = ensure_queue_shared(
                    GetParent(hosted).unwrap_or_default(),
                    hosted,
                    child_thread,
                );
                sync_hosted_focus(hosted, attached);
            }
        }
        Ok(())
    }

    /// Undo an app-side minimize/maximize on the hosted window.
    ///
    /// relayout cannot repair these: it re-asserts the slot rect, but an iconic
    /// or zoomed child keeps painting wrong. `SW_RESTORE` clears both states;
    /// the next relayout re-places the window into the slot.
    pub fn restore(process: &str) -> Result<(), String> {
        let key = map_key(process);
        let live = with_map(|m| m.get(&key).map(|e| (e.hwnd, e.pid, e.shown)));
        let Some((hwnd, pid, shown)) = live else {
            return Err("窗口已经不存在了".into());
        };
        if !entry_live(hwnd, pid) {
            with_map(|m| {
                m.remove(&key);
            });
            return Err("窗口已经不存在了".into());
        }
        let window = as_hwnd(hwnd);
        unsafe {
            let _ = ShowWindow(window, SW_RESTORE);
            // SW_RESTORE also reveals the window. Keep a deliberately hidden
            // window (picker / confirm dialog) hidden: restore the state, not
            // the visibility.
            let _ = SetWindowPos(
                window,
                None,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE
                    | SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | if shown != 0 {
                        SWP_SHOWWINDOW
                    } else {
                        SWP_HIDEWINDOW
                    },
            );
        }
        debug_log(&format!("restore {key} hwnd=0x{:X}", hwnd as usize));
        Ok(())
    }

    pub fn detach(process: &str) {
        let key = map_key(process);
        if let Some(entry) = with_map(|m| m.remove(&key)) {
            release(entry);
        }
    }

    /// Keyboard focus for the hosted window of `process`, when the workbench
    /// itself is the active window. Used when the page brings a retained
    /// window back (tab re-entry, picker close): show(true) cannot move focus.
    pub fn focus(app: &tauri::AppHandle, process: &str) {
        let key = map_key(process);
        let live = with_map(|m| {
            m.get(&key)
                .map(|e| (e.hwnd, e.parent, e.pid, e.attached_thread))
        });
        let Some((hwnd, parent, pid, child_thread)) = live else {
            return;
        };
        if !entry_live(hwnd, pid) {
            with_map(|m| {
                m.remove(&key);
            });
            return;
        }
        focus_hosted(app, as_hwnd(parent), as_hwnd(hwnd), child_thread);
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
    dpr: Option<f64>,
) -> Result<DesktopWindowInfo, String> {
    #[cfg(windows)]
    {
        imp::attach(&app, &process_name, x, y, width, height, false, dpr)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, x, y, width, height, dpr);
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
    dpr: Option<f64>,
) -> Result<DesktopWindowInfo, String> {
    #[cfg(windows)]
    {
        let task = tokio::task::spawn_blocking(move || {
            imp::attach_wait(&app, &process_name, x, y, width, height, wait_ms, dpr)
        });
        task.await.map_err(|e| format!("嵌入失败: {e}"))?
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, x, y, width, height, wait_ms, dpr);
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
    dpr: Option<f64>,
) -> Result<DesktopWindowInfo, String> {
    #[cfg(windows)]
    {
        imp::attach_window(&app, &process_name, &hwnd, x, y, width, height, dpr)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, hwnd, x, y, width, height, dpr);
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
    dpr: Option<f64>,
) -> Result<DesktopRelayoutResult, String> {
    #[cfg(windows)]
    {
        imp::relayout(&app, &process_name, x, y, width, height, dpr)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name, x, y, width, height, dpr);
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

/// Clear an app-side minimize/maximize on the hosted window.
#[tauri::command]
pub async fn desktop_embed_restore(process_name: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        imp::restore(&process_name)
    }
    #[cfg(not(windows))]
    {
        let _ = process_name;
        Err("当前系统不支持嵌入桌面应用".into())
    }
}

/// Keyboard focus for the hosted window (page bring-back / picker close).
#[tauri::command]
pub async fn desktop_embed_focus(
    app: tauri::AppHandle,
    process_name: String,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        imp::focus(&app, &process_name);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (app, process_name);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_dtos_use_camel_case_process_name() {
        let info = DesktopWindowInfo {
            process_name: "Cursor".into(),
            pid: 42,
            title: "workspace - Cursor".into(),
            class: "Chrome_WidgetWin_1".into(),
            attached: true,
            warning: String::new(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["processName"], "Cursor");
        assert!(json.get("process_name").is_none());

        let relayout = DesktopRelayoutResult {
            state: "ok".into(),
            title: "workspace - Cursor".into(),
            class: "Chrome_WidgetWin_1".into(),
            warning: String::new(),
        };
        let json = serde_json::to_value(&relayout).unwrap();
        assert_eq!(json["state"], "ok");

        let candidate = DesktopWindowCandidate {
            process_name: "Cursor".into(),
            pid: 42,
            hwnd: "0x00010123".into(),
            class: "Chrome_WidgetWin_1".into(),
            title: "workspace - Cursor".into(),
            visible: true,
            owned: false,
            area: 100,
            width: 10,
            height: 10,
            recommended: true,
            current: false,
        };
        let json = serde_json::to_value(&candidate).unwrap();
        assert_eq!(json["processName"], "Cursor");
        assert!(json.get("process_name").is_none());
    }
}
