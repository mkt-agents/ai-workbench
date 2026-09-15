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

pub const QUICK_ASK_LABEL: &str = "quick-ask";
pub(crate) const QUICK_ASK_BUBBLE_LABEL: &str = "quick-ask-bubble";
pub(crate) const MAIN_LABEL: &str = "main";
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
    // Re-read size after Windows may have clamped the tiny undecorated window.
    apply_circular_region(win);
    let _ = win.set_always_on_top(true);
    let _ = win.show();
    // Enable hit-testing only after the window is shown at a safe position.
    let _ = win.set_ignore_cursor_events(false);
    Ok(())
}

/// Clip the HWND to an ellipse so WebView2 white corners cannot show.
#[cfg(windows)]
pub(crate) fn apply_circular_region<R: Runtime>(win: &WebviewWindow<R>) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{CreateEllipticRgn, SetWindowRgn};

    let Ok(hwnd) = win.hwnd() else {
        return;
    };
    let Ok(size) = win.outer_size() else {
        return;
    };
    let w = size.width as i32;
    let h = size.height as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    unsafe {
        let hrgn = CreateEllipticRgn(0, 0, w, h);
        // SetWindowRgn takes ownership of hrgn when successful.
        let _ = SetWindowRgn(HWND(hwnd.0 as *mut _), Some(hrgn), true);
    }
}

#[cfg(not(windows))]
fn apply_circular_region<R: Runtime>(_win: &WebviewWindow<R>) {}

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
        .background_color(tauri::window::Color(0, 0, 0, 0))
        .visible(false)
        .focused(false)
        .build()
        .map_err(|e| e.to_string())?;
    // Start ignoring cursor until ready+show — prevents a pre-ready ghost from eating clicks.
    let _ = win.set_ignore_cursor_events(true);
    // Re-assert size after create (Windows may clamp tiny undecorated windows).
    let _ = win.set_size(tauri::LogicalSize::new(BUBBLE_SIZE, BUBBLE_SIZE));
    let _ = win.set_position(tauri::PhysicalPosition::new(pos_x, pos_y));
    apply_circular_region(&win);
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
        apply_circular_region(&win);
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
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

pub fn toggle_quick_ask<R: Runtime>(app: &AppHandle<R>) {
    if let Err(e) = ensure_quick_ask_window(app) {
        eprintln!("[tray] ensure quick-ask failed: {e}");
        return;
    }
    if let Some(win) = app.get_webview_window(QUICK_ASK_LABEL) {
        match win.is_visible() {
            Ok(true) => {
                let _ = win.hide();
            }
            _ => {
                let _ = win.show();
                let _ = win.set_focus();
                let _ = app.emit("quick-ask-shown", ());
            }
        }
    }
}

pub fn show_quick_ask<R: Runtime>(app: &AppHandle<R>) {
    if let Err(e) = ensure_quick_ask_window(app) {
        eprintln!("[tray] ensure quick-ask failed: {e}");
        return;
    }
    if let Some(win) = app.get_webview_window(QUICK_ASK_LABEL) {
        let _ = win.show();
        let _ = win.set_focus();
        let _ = app.emit("quick-ask-shown", ());
    }
}
