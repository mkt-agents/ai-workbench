//! Quick-ask bubble and quick-ask window management.
//!
//! The bubble is a small always-on-top transparent orb that lives on the
//! desktop; clicking it toggles the quick-ask window. This module owns the
//! window lifecycle, position clamping (to avoid covering the main titlebar),
//! and the Windows-specific circular HWND region that clips WebView2 corners.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

/// Real (Win32) window visibility helpers.
///
/// tao drives visibility through a *diff of its cached window flags* and ultimately
/// calls `ShowWindow` (see `tao::platform_impl::windows::window_state::apply_diff`).
/// When the OS ignores that call the cached flag desynchronises from the real window,
/// so every later `show()`/`hide()` short-circuits on an empty diff and the window can
/// never be shown again — this is exactly what parked the on-demand quick-ask window
/// in a permanently hidden state while the bubble (created at startup) kept working.
/// So read and drive the real window state instead of trusting tao's cache.
#[cfg(windows)]
pub(crate) mod vis {
    use tauri::{Runtime, WebviewWindow};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsWindowVisible, SetWindowPos, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SWP_SHOWWINDOW,
    };

    fn hwnd_of<R: Runtime>(win: &WebviewWindow<R>) -> Option<HWND> {
        win.hwnd().ok().map(|h| HWND(h.0 as *mut _))
    }

    /// True on-screen visibility, bypassing tao's cached flag.
    pub fn really_visible<R: Runtime>(win: &WebviewWindow<R>) -> Option<bool> {
        let hwnd = hwnd_of(win)?;
        Some(unsafe { IsWindowVisible(hwnd).as_bool() })
    }

    /// Force real visibility with `SetWindowPos`, which still works where `ShowWindow`
    /// is silently ignored. Returns `false` when the window could not be updated.
    pub fn force_visible<R: Runtime>(win: &WebviewWindow<R>, visible: bool) -> bool {
        let Some(hwnd) = hwnd_of(win) else {
            eprintln!("[tray] force_visible: window handle unavailable");
            return false;
        };
        let mut flags = SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOZORDER;
        flags |= if visible {
            SWP_SHOWWINDOW
        } else {
            SWP_HIDEWINDOW
        };
        match unsafe { SetWindowPos(hwnd, None, 0, 0, 0, 0, flags) } {
            Ok(()) => true,
            Err(e) => {
                eprintln!(
                    "[tray] force_visible(visible={visible}, hwnd=0x{:X}) failed: {e}",
                    hwnd.0 as isize
                );
                false
            }
        }
    }
}

#[cfg(not(windows))]
pub(crate) mod vis {
    use tauri::{Runtime, WebviewWindow};

    pub fn really_visible<R: Runtime>(_win: &WebviewWindow<R>) -> Option<bool> {
        None
    }

    pub fn force_visible<R: Runtime>(_win: &WebviewWindow<R>, _visible: bool) -> bool {
        false
    }
}

pub const QUICK_ASK_LABEL: &str = "quick-ask";
pub(crate) const QUICK_ASK_BUBBLE_LABEL: &str = "quick-ask-bubble";
pub(crate) const MAIN_LABEL: &str = "main";
/// Bounding window size = 44px visible orb (`.qa-bubble-orb` in styles.css).
/// The window stays a plain (uncut) rectangle: the DWM blur-behind
/// transparency makes the corners around the CSS-rounded orb see-through,
/// and the orb fills the window so no opaque backing can leak through.
/// Keep the orb size in styles.css in sync with this.
const BUBBLE_SIZE: f64 = 44.0;
const BUBBLE_MARGIN: f64 = 24.0;
/// Keep the bubble out of the top chrome band so it cannot cover main titlebar buttons.
const BUBBLE_TOP_SAFE_LOGICAL: f64 = 96.0;
/// Park hidden bubble far off-screen so a failed hide cannot keep eating clicks.
const BUBBLE_PARK_POS: (i32, i32) = (-10_000, -10_000);

/// When true, `quick_ask_bubble_ready` may show the window (avoids white flash).
static BUBBLE_SHOULD_SHOW: AtomicBool = AtomicBool::new(false);
/// Frontend has painted transparent orb and called ready.
static BUBBLE_CONTENT_READY: AtomicBool = AtomicBool::new(false);

pub fn ensure_quick_ask_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if app.get_webview_window(QUICK_ASK_LABEL).is_some() {
        return Ok(());
    }
    let url = WebviewUrl::App("index.html#/quick-ask".into());
    WebviewWindowBuilder::new(app, QUICK_ASK_LABEL, url)
        .title("Quick Ask")
        .inner_size(560.0, 520.0)
        .resizable(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Prepare the quick-ask window shortly after startup, on the main thread.
///
/// `setup` is the one place where building a window is known-good (that is how the
/// desktop bubble is created), so the window is materialised here instead of from the
/// toggle path. It runs in the background after a short delay so the cold start is not
/// blocked by a second WebView2.
pub fn prebuild_quick_ask_window<R: Runtime>(app: &AppHandle<R>) {
    if app.get_webview_window(QUICK_ASK_LABEL).is_some() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let inner = app.clone();
        if let Err(e) = app.run_on_main_thread(move || {
            if let Err(e) = ensure_quick_ask_window(&inner) {
                eprintln!("[tray] prebuild quick-ask failed: {e}");
            }
        }) {
            eprintln!("[tray] prebuild dispatch failed: {e}");
        }
    });
}

pub(crate) fn default_bubble_position<R: Runtime>(app: &AppHandle<R>) -> (i32, i32) {
    let Ok(Some(monitor)) = app.primary_monitor() else {
        return (100, 100);
    };
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let size = BUBBLE_SIZE * scale;
    let margin = BUBBLE_MARGIN * scale;
    let x = work.position.x as f64 + work.size.width as f64 - size - margin;
    let y = work.position.y as f64 + work.size.height as f64 - size - margin;
    clamp_bubble_position(app, x.round() as i32, y.round() as i32)
}

/// Keep bubble in the primary work area (physical pixels), below the titlebar-safe band.
pub(crate) fn clamp_bubble_position<R: Runtime>(app: &AppHandle<R>, x: i32, y: i32) -> (i32, i32) {
    let Ok(Some(monitor)) = app.primary_monitor() else {
        return (x, y);
    };
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let size = (BUBBLE_SIZE * scale).round() as i32;
    let margin = (BUBBLE_MARGIN * scale).round() as i32;
    let top_safe = (BUBBLE_TOP_SAFE_LOGICAL * scale).round() as i32;
    let min_x = work.position.x + margin;
    // Never allow Y into the top band — that is exactly where main titlebar controls sit.
    let min_y = work.position.y + margin.max(top_safe);
    let max_x = work.position.x + work.size.width as i32 - size - margin;
    let max_y = work.position.y + work.size.height as i32 - size - margin;
    (
        x.clamp(min_x, max_x.max(min_x)),
        y.clamp(min_y, max_y.max(min_y)),
    )
}

/// True when a persisted point sits in the top chrome band (legacy HiDPI clamp damage).
pub(crate) fn is_titlebar_danger_zone<R: Runtime>(app: &AppHandle<R>, x: i32, y: i32) -> bool {
    let Ok(Some(monitor)) = app.primary_monitor() else {
        return false;
    };
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let size = (BUBBLE_SIZE * scale).round() as i32;
    let margin = (BUBBLE_MARGIN * scale).round() as i32;
    let top_safe = (BUBBLE_TOP_SAFE_LOGICAL * scale).round() as i32;
    let max_x = work.position.x + work.size.width as i32 - size - margin;
    let in_top = y < work.position.y + top_safe;
    // Old bug parked the window at the top-right corner of the work area.
    let near_right = x >= max_x - margin;
    let right_half =
        (work.size.width as i32) > 0 && x > work.position.x + work.size.width as i32 / 2;
    (in_top && near_right) || (in_top && right_half)
}

fn resolve_bubble_position<R: Runtime>(
    app: &AppHandle<R>,
    x: Option<i32>,
    y: Option<i32>,
) -> (i32, i32) {
    match (x, y) {
        (Some(px), Some(py)) if is_titlebar_danger_zone(app, px, py) => default_bubble_position(app),
        (Some(px), Some(py)) => clamp_bubble_position(app, px, py),
        _ => default_bubble_position(app),
    }
}

fn park_bubble_window<R: Runtime>(win: &WebviewWindow<R>) {
    let _ = win.set_ignore_cursor_events(true);
    let _ = win.set_always_on_top(false);
    let _ = win.hide();
    // `hide()` can be a silent no-op (see `vis`); park for real.
    let _ = vis::force_visible(win, false);
    let _ = win.set_position(tauri::PhysicalPosition::new(BUBBLE_PARK_POS.0, BUBBLE_PARK_POS.1));
}

fn reveal_bubble_window<R: Runtime>(
    app: &AppHandle<R>,
    win: &WebviewWindow<R>,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    let (px, py) = resolve_bubble_position(app, x, y);
    let _ = win.set_size(tauri::LogicalSize::new(BUBBLE_SIZE, BUBBLE_SIZE));
    let _ = win.set_position(tauri::PhysicalPosition::new(px, py));
    let _ = win.set_always_on_top(true);
    let _ = win.show();
    // `show()` can be a silent no-op (see `vis`); show for real.
    let _ = vis::force_visible(win, true);
    // Enable hit-testing only after the window is shown at a safe position.
    let _ = win.set_ignore_cursor_events(false);
    Ok(())
}

/// The orb's circular shape comes from CSS `border-radius` on the webview
/// content. Do NOT use `SetWindowRgn` here: a window region forces the GDI
/// repaint path and defeats the DWM blur-behind transparency tao sets up for
/// `transparent(true)` — the uncovered window backing then shows through as
/// a white ring, and the region edge itself is aliased (no anti-aliasing).
pub fn ensure_quick_ask_bubble<R: Runtime>(
    app: &AppHandle<R>,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    // Never destroy an existing window here. CONTENT_READY stays false until the
    // webview mounts — treating that as "stale" caused close+sleep and froze IPC.
    if app.get_webview_window(QUICK_ASK_BUBBLE_LABEL).is_some() {
        return Ok(());
    }
    let (pos_x, pos_y) = resolve_bubble_position(app, x, y);
    let url = WebviewUrl::App("index.html#/quick-ask-bubble".into());
    // Builder.position() is logical pixels — do NOT pass physical coords there.
    // Set PhysicalPosition immediately after build instead.
    let win = WebviewWindowBuilder::new(app, QUICK_ASK_BUBBLE_LABEL, url)
        .title("Quick Ask")
        .inner_size(BUBBLE_SIZE, BUBBLE_SIZE)
        .min_inner_size(BUBBLE_SIZE, BUBBLE_SIZE)
        .max_inner_size(BUBBLE_SIZE, BUBBLE_SIZE)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .transparent(true)
        .shadow(false)
        .visible(false)
        .focused(false)
        .build()
        .map_err(|e| e.to_string())?;
    // Start ignoring cursor until ready+show — prevents a pre-ready ghost from eating clicks.
    let _ = win.set_ignore_cursor_events(true);
    // Re-assert size after create (Windows may clamp tiny undecorated windows).
    let _ = win.set_size(tauri::LogicalSize::new(BUBBLE_SIZE, BUBBLE_SIZE));
    let _ = win.set_position(tauri::PhysicalPosition::new(pos_x, pos_y));
    Ok(())
}

pub fn set_quick_ask_bubble_visible_inner<R: Runtime>(
    app: &AppHandle<R>,
    visible: bool,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    if !visible {
        BUBBLE_SHOULD_SHOW.store(false, Ordering::SeqCst);
        // Keep CONTENT_READY — reuse the same webview on next show.
        if let Some(win) = app.get_webview_window(QUICK_ASK_BUBBLE_LABEL) {
            park_bubble_window(&win);
        }
        return Ok(());
    }

    BUBBLE_SHOULD_SHOW.store(true, Ordering::SeqCst);
    ensure_quick_ask_bubble(app, x, y)?;
    let Some(win) = app.get_webview_window(QUICK_ASK_BUBBLE_LABEL) else {
        return Err("bubble window missing".into());
    };
    // Show only after frontend is ready (or immediately on subsequent toggles).
    if BUBBLE_CONTENT_READY.load(Ordering::SeqCst) {
        reveal_bubble_window(app, &win, x, y)?;
    } else {
        // Keep parked / click-through until ready callback reveals it.
        let _ = win.set_ignore_cursor_events(true);
        let (px, py) = resolve_bubble_position(app, x, y);
        let _ = win.set_size(tauri::LogicalSize::new(BUBBLE_SIZE, BUBBLE_SIZE));
        let _ = win.set_position(tauri::PhysicalPosition::new(px, py));
    }
    Ok(())
}

/// Called by the bubble webview after CSS/transparent bg are applied.
#[tauri::command]
pub fn quick_ask_bubble_ready(app: AppHandle) -> Result<(), String> {
    BUBBLE_CONTENT_READY.store(true, Ordering::SeqCst);
    if !BUBBLE_SHOULD_SHOW.load(Ordering::SeqCst) {
        if let Some(win) = app.get_webview_window(QUICK_ASK_BUBBLE_LABEL) {
            park_bubble_window(&win);
        }
        return Ok(());
    }
    let Some(win) = app.get_webview_window(QUICK_ASK_BUBBLE_LABEL) else {
        return Ok(());
    };
    // Prefer the position already applied during ensure/show; fall back to default if unsafe.
    let (x, y) = match win.outer_position() {
        Ok(pos) if !is_titlebar_danger_zone(&app, pos.x, pos.y) => (Some(pos.x), Some(pos.y)),
        _ => (None, None),
    };
    reveal_bubble_window(&app, &win, x, y)
}

pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window(MAIN_LABEL) {
        if let Err(e) = win.show() {
            eprintln!("[tray] show main failed: {e}");
        }
        if let Err(e) = win.unminimize() {
            eprintln!("[tray] unminimize main failed: {e}");
        }
        if let Err(e) = win.set_focus() {
            eprintln!("[tray] focus main failed: {e}");
        }
        // Tray click / second launch must always bring the window back: apply the real
        // visibility last so nothing above can undo it.
        if !vis::force_visible(&win, true) {
            eprintln!("[tray] main window still hidden after force_visible");
        }
    }
}

pub fn toggle_quick_ask<R: Runtime>(app: &AppHandle<R>) {
    // Never build the window from this path: `WebviewWindowBuilder::build()` does NOT
    // return when it runs on a command thread — it creates the HWND and then blocks
    // forever, so the window stayed permanently hidden and every entry point (bubble
    // click, tray item, Ctrl+Alt+K) looked dead. The window is prepared on the main
    // thread at startup by `prebuild_quick_ask_window`; this path only toggles it.
    let Some(win) = app.get_webview_window(QUICK_ASK_LABEL) else {
        // Not materialised yet — schedule the prebuild and let the next click toggle it
        // instead of blocking here (see the note above).
        prebuild_quick_ask_window(app);
        return;
    };
    // Judge from the real window, not from `is_visible()` — tao's cached flag desyncs
    // when `ShowWindow` is ignored, which made this toggle a permanent no-op.
    let visible = vis::really_visible(&win).unwrap_or_else(|| win.is_visible().unwrap_or(false));
    if visible {
        if let Err(e) = win.hide() {
            eprintln!("[tray] hide quick-ask failed: {e}");
        }
        let _ = vis::force_visible(&win, false);
    } else {
        if let Err(e) = win.show() {
            eprintln!("[tray] show quick-ask failed: {e}");
        }
        if let Err(e) = win.unminimize() {
            eprintln!("[tray] unminimize quick-ask failed: {e}");
        }
        if let Err(e) = win.set_focus() {
            eprintln!("[tray] focus quick-ask failed: {e}");
        }
        // tao's own calls can be dropped silently, so force the real visibility last.
        if !vis::force_visible(&win, true) {
            eprintln!("[tray] quick-ask still hidden after force_visible");
        }
        let _ = app.emit("quick-ask-shown", ());
    }
}

pub fn show_quick_ask<R: Runtime>(app: &AppHandle<R>) {
    // Same rule as `toggle_quick_ask`: never build from here (see the comment there).
    let Some(win) = app.get_webview_window(QUICK_ASK_LABEL) else {
        prebuild_quick_ask_window(app);
        return;
    };
    if let Err(e) = win.show() {
        eprintln!("[tray] show quick-ask failed: {e}");
    }
    if let Err(e) = win.unminimize() {
        eprintln!("[tray] unminimize quick-ask failed: {e}");
    }
    if let Err(e) = win.set_focus() {
        eprintln!("[tray] focus quick-ask failed: {e}");
    }
    if !vis::force_visible(&win, true) {
        eprintln!("[tray] quick-ask still hidden after force_visible");
    }
    let _ = app.emit("quick-ask-shown", ());
}

/// Hide the quick-ask window for real. The frontend used `getCurrentWindow().hide()`,
/// which tao resolves through its cached flag diff — once that cache desyncs (because
/// the real window was shown with `SetWindowPos`) the call becomes a no-op, so Esc /
/// the close button could not dismiss the window. Route it through Rust instead.
pub fn hide_quick_ask<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window(QUICK_ASK_LABEL) {
        if let Err(e) = win.hide() {
            eprintln!("[tray] hide quick-ask failed: {e}");
        }
        vis::force_visible(&win, false);
    }
}
