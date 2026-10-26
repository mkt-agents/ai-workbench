//! AI Hub bubble and ai-hub window management.
//!
//! The bubble is a small always-on-top transparent orb that lives on the
//! desktop; clicking it toggles the ai-hub window. This module owns the
//! window lifecycle, position clamping (to avoid covering the main titlebar),
//! and the Windows-specific circular HWND region that clips WebView2 corners.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use tauri::{
    AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

/// Real (Win32) window visibility helpers.
///
/// tao drives visibility through a *diff of its cached window flags* and ultimately
/// calls `ShowWindow` (see `tao::platform_impl::windows::window_state::apply_diff`).
/// When the OS ignores that call the cached flag desynchronises from the real window,
/// so every later `show()`/`hide()` short-circuits on an empty diff and the window can
/// never be shown again — this is exactly what parked the on-demand ai-hub window
/// in a permanently hidden state while the bubble (created at startup) kept working.
/// So read and drive the real window state instead of trusting tao's cache.
#[cfg(windows)]
pub(crate) mod vis {
    use tauri::{Runtime, WebviewWindow};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_MENU};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, IsWindowVisible, SetForegroundWindow, SetWindowPos, HWND_NOTOPMOST,
        HWND_TOPMOST, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SWP_SHOWWINDOW,
    };

    fn hwnd_of<R: Runtime>(win: &WebviewWindow<R>) -> Option<HWND> {
        win.hwnd().ok().map(|h| HWND(h.0 as *mut _))
    }

    /// Whether the window currently owns the foreground (i.e. is not covered/inactive).
    pub fn is_foreground<R: Runtime>(win: &WebviewWindow<R>) -> Option<bool> {
        let hwnd = hwnd_of(win)?;
        Some(unsafe { GetForegroundWindow() } == hwnd)
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

    /// Another app owns the foreground, so a plain `set_focus` is rejected and the
    /// window stays buried. Lift it above that app, then give it the keyboard.
    /// `SWP_NOACTIVATE` must not be used here: that flag is what left the first
    /// bubble click after an app switch looking like a no-op.
    pub fn bring_to_front<R: Runtime>(win: &WebviewWindow<R>) -> bool {
        let Some(hwnd) = hwnd_of(win) else {
            eprintln!("[tray] bring_to_front: window handle unavailable");
            return false;
        };
        let stay_top = win.is_always_on_top().unwrap_or(false);
        let show = SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW;
        unsafe {
            if let Err(e) = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, show) {
                eprintln!("[tray] bring_to_front topmost failed: {e}");
                return false;
            }
            if !SetForegroundWindow(hwnd).as_bool() {
                // An Alt tap is the documented way past the foreground lock once
                // the originating click has already been dispatched.
                keybd_event(VK_MENU.0 as u8, 0, Default::default(), 0);
                keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
                let _ = SetForegroundWindow(hwnd);
            }
            if !stay_top {
                if let Err(e) = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, show) {
                    eprintln!("[tray] bring_to_front drop topmost failed: {e}");
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
        true
    }
}

#[cfg(not(windows))]
pub(crate) mod vis {
    use tauri::{Runtime, WebviewWindow};

    pub fn really_visible<R: Runtime>(_win: &WebviewWindow<R>) -> Option<bool> {
        None
    }

    pub fn is_foreground<R: Runtime>(_win: &WebviewWindow<R>) -> Option<bool> {
        None
    }

    pub fn force_visible<R: Runtime>(_win: &WebviewWindow<R>, _visible: bool) -> bool {
        false
    }

    pub fn bring_to_front<R: Runtime>(_win: &WebviewWindow<R>) -> bool {
        false
    }
}

pub const AI_HUB_LABEL: &str = "ai-hub";
pub(crate) const AI_HUB_BUBBLE_LABEL: &str = "ai-hub-bubble";
pub(crate) const MAIN_LABEL: &str = "main";
/// Bounding window size (logical px). The orb disc is 40px (`.qa-bubble-orb`);
/// pet skins anchor to the bottom; front-end keeps a permanent speech headroom
/// above (`PET_SPEECH_HEADROOM`) so talk bubbles fade in/out without resizing
/// (resizing made the pet jump). Menu still temporarily grows the box.
/// The window stays a plain (uncut) rectangle: DWM blur-behind makes the
/// uncovered corners see-through.
const BUBBLE_SIZE_DEFAULT: u32 = 112;
/// builder 的尺寸区间：窗本身 non-resizable，只有 `set_ai_hub_bubble_box` 能改。
/// 上界给「就地菜单」临时撑高用（菜单开=窗变高，桌宠底边不动）。
const BUBBLE_BOX_MIN: u32 = 88;
const BUBBLE_BOX_MAX_W: u32 = 224;
const BUBBLE_BOX_MAX_H: u32 = 320;
const BUBBLE_MARGIN: f64 = 24.0;
/// Keep the bubble out of the top chrome band so it cannot cover main titlebar buttons.
const BUBBLE_TOP_SAFE_LOGICAL: f64 = 96.0;
/// Park hidden bubble far off-screen so a failed hide cannot keep eating clicks.
const BUBBLE_PARK_POS: (i32, i32) = (-10_000, -10_000);

/// 桌宠窗当前逻辑尺寸：前端按设置档位在挂载时下发，之后所有几何数学读这里。
static BUBBLE_W: AtomicU32 = AtomicU32::new(BUBBLE_SIZE_DEFAULT);
static BUBBLE_H: AtomicU32 = AtomicU32::new(BUBBLE_SIZE_DEFAULT);

/// 当前桌宠窗逻辑尺寸（宽, 高）。
pub(crate) fn bubble_box() -> (u32, u32) {
    (
        BUBBLE_W.load(Ordering::SeqCst),
        BUBBLE_H.load(Ordering::SeqCst),
    )
}

/// 尺寸先夹进 builder 区间，越界不报错：桌宠宁可小一点也不能建不出窗。
fn clamp_bubble_box(width: u32, height: u32) -> (u32, u32) {
    (
        width.clamp(BUBBLE_BOX_MIN, BUBBLE_BOX_MAX_W),
        height.clamp(BUBBLE_BOX_MIN, BUBBLE_BOX_MAX_H),
    )
}

fn store_bubble_box(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = clamp_bubble_box(width, height);
    BUBBLE_W.store(w, Ordering::SeqCst);
    BUBBLE_H.store(h, Ordering::SeqCst);
    (w, h)
}

/// 物理像素尺寸（按 DPR 取整）；位置/clamp/危险带全部用它。
fn bubble_box_physical(scale: f64) -> (i32, i32) {
    let (w, h) = bubble_box();
    (
        (w as f64 * scale).round() as i32,
        (h as f64 * scale).round() as i32,
    )
}

/// 窗内「真正能点」的矩形（逻辑像素，相对窗左上角）。
///
/// 窗比角色大一圈（头顶常驻说话留白 + 左右留白），但 Windows 整窗都吃鼠标，透明留白
/// 会把下面窗口的点击拦下来。前端每次 `syncBox()` 把可交互区发下来，`gaze.rs` 按它切
/// `set_ignore_cursor_events`；`w == 0` 表示整窗可点（就地菜单打开时）。
static HIT_X: AtomicU32 = AtomicU32::new(0);
static HIT_Y: AtomicU32 = AtomicU32::new(0);
static HIT_W: AtomicU32 = AtomicU32::new(0);
static HIT_H: AtomicU32 = AtomicU32::new(0);

/// 当前可交互矩形（逻辑像素）；`w == 0` = 整窗。
pub(crate) fn bubble_hit_rect() -> (u32, u32, u32, u32) {
    (
        HIT_X.load(Ordering::SeqCst),
        HIT_Y.load(Ordering::SeqCst),
        HIT_W.load(Ordering::SeqCst),
        HIT_H.load(Ordering::SeqCst),
    )
}

fn store_hit_rect(hit: Option<[u32; 4]>) {
    let [x, y, w, h] = hit.unwrap_or([0, 0, 0, 0]);
    HIT_X.store(x, Ordering::SeqCst);
    HIT_Y.store(y, Ordering::SeqCst);
    HIT_W.store(w, Ordering::SeqCst);
    HIT_H.store(h, Ordering::SeqCst);
}

/// When true, `ai_hub_bubble_ready` may show the window (avoids white flash).
static BUBBLE_SHOULD_SHOW: AtomicBool = AtomicBool::new(false);
/// Frontend has painted transparent orb and called ready.
static BUBBLE_CONTENT_READY: AtomicBool = AtomicBool::new(false);

pub fn ensure_ai_hub_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if app.get_webview_window(AI_HUB_LABEL).is_some() {
        return Ok(());
    }
    let url = WebviewUrl::App("index.html#/ai-hub".into());
    let win = WebviewWindowBuilder::new(app, AI_HUB_LABEL, url)
        .title("天玑")
        .inner_size(720.0, 640.0)
        .min_inner_size(640.0, 600.0)
        // 放大 is capped (LARGE_MAX in aiHub/useAiHubWindow.ts): a full-screen hub leaves the
        // reading column floating in empty space, so neither drag-resize nor
        // Win+Up / snap-to-top may make it larger.
        .max_inner_size(1120.0, 960.0)
        .maximizable(false)
        .resizable(true)
        .decorations(false)
        .always_on_top(false)
        .skip_taskbar(true)
        // The hub layout relies on the logical 640×600 minimum; a Ctrl+wheel
        // zoom shrinks the CSS viewport and squeezes the header/composer.
        .zoom_hotkeys_enabled(false)
        .visible(false)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    // WebView2 may have persisted a zoom level from an earlier session.
    let _ = win.set_zoom(1.0);
    Ok(())
}

/// Prepare the ai-hub window shortly after startup, on the main thread.
///
/// `setup` is the one place where building a window is known-good (that is how the
/// desktop bubble is created), so the window is materialised here instead of from the
/// toggle path. It runs in the background after a short delay so the cold start is not
/// blocked by a second WebView2.
pub fn prebuild_ai_hub_window<R: Runtime>(app: &AppHandle<R>) {
    if app.get_webview_window(AI_HUB_LABEL).is_some() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let inner = app.clone();
        if let Err(e) = app.run_on_main_thread(move || {
            if let Err(e) = ensure_ai_hub_window(&inner) {
                eprintln!("[tray] prebuild ai-hub failed: {e}");
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
    let (bw, bh) = bubble_box_physical(scale);
    let margin = (BUBBLE_MARGIN * scale).round() as i32;
    let x = work.position.x + work.size.width as i32 - bw - margin;
    let y = work.position.y + work.size.height as i32 - bh - margin;
    clamp_bubble_position(app, x, y)
}

/// 一块屏的可用工作区（物理像素：左 / 上 / 宽 / 高）。
pub(crate) type WorkArea = (i32, i32, i32, i32);

pub(crate) fn work_area_of(monitor: &tauri::Monitor) -> WorkArea {
    let work = monitor.work_area();
    (
        work.position.x,
        work.position.y,
        work.size.width as i32,
        work.size.height as i32,
    )
}

/// 纯函数：把桌宠窗左上角夹进一块工作区（全物理像素）。
///
/// 左 / 下留 `BUBBLE_MARGIN`，顶部还额外让出主标题栏安全带 —— 与旧的
/// `clamp_bubble_position` 语义完全一致，只是把「哪块屏」变成参数，好写跨屏单测。
pub(crate) fn clamp_to_work_area(
    work: WorkArea,
    box_size: (i32, i32),
    point: (i32, i32),
    scale: f64,
) -> (i32, i32) {
    let margin = (BUBBLE_MARGIN * scale).round() as i32;
    let top_safe = (BUBBLE_TOP_SAFE_LOGICAL * scale).round() as i32;
    let min_x = work.0 + margin;
    // 顶部安全带就是主标题栏按钮所在的那条，Y 永远不许进去。
    let min_y = work.1 + margin.max(top_safe);
    let max_x = work.0 + work.2 - box_size.0 - margin;
    let max_y = work.1 + work.3 - box_size.1 - margin;
    (
        point.0.clamp(min_x, max_x.max(min_x)),
        point.1.clamp(min_y, max_y.max(min_y)),
    )
}

/// 桌宠窗该算在哪块屏：以**窗口中心**为准。
///
/// 只看左上角会在窗口跨屏时判错（左上角还在副屏、身体已经压到主屏），那样每次松手
/// 都被拉回主屏。这里先用左上角找一块屏估出中心，再用中心定屏；最多两轮，
/// 因为跨屏两端 DPI 不同时窗口的物理尺寸本身会变，需要再认一次。
pub(crate) fn monitor_for_box<R: Runtime>(
    app: &AppHandle<R>,
    top_left: (i32, i32),
) -> Option<(WorkArea, f64)> {
    let mut probe = top_left;
    let mut found = None;
    for _ in 0..2 {
        let monitor = app
            .monitor_from_point(probe.0 as f64, probe.1 as f64)
            .ok()
            .flatten()
            .or_else(|| app.primary_monitor().ok().flatten())?;
        let scale = monitor.scale_factor();
        let (bw, bh) = bubble_box_physical(scale);
        found = Some((work_area_of(&monitor), scale));
        let center = (top_left.0 + bw / 2, top_left.1 + bh / 2);
        if center == probe {
            break;
        }
        probe = center;
    }
    found
}

/// Keep the bubble on the work area of the monitor it mostly sits on (physical pixels),
/// below the titlebar-safe band. 主屏只是「默认落点」，不是唯一允许的屏幕。
pub(crate) fn clamp_bubble_position<R: Runtime>(app: &AppHandle<R>, x: i32, y: i32) -> (i32, i32) {
    match monitor_for_box(app, (x, y)) {
        Some((work, scale)) => clamp_to_work_area(work, bubble_box_physical(scale), (x, y), scale),
        None => (x, y),
    }
}

/// True when a persisted point sits in the top chrome band (legacy HiDPI clamp damage).
pub(crate) fn is_titlebar_danger_zone<R: Runtime>(app: &AppHandle<R>, x: i32, y: i32) -> bool {
    let Ok(Some(monitor)) = app.primary_monitor() else {
        return false;
    };
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let (bw, _) = bubble_box_physical(scale);
    let margin = (BUBBLE_MARGIN * scale).round() as i32;
    let top_safe = (BUBBLE_TOP_SAFE_LOGICAL * scale).round() as i32;
    let max_x = work.position.x + work.size.width as i32 - bw - margin;
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
    let (bw, bh) = bubble_box();
    let _ = win.set_size(tauri::LogicalSize::new(bw as f64, bh as f64));
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
pub fn ensure_ai_hub_bubble<R: Runtime>(
    app: &AppHandle<R>,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    // Never destroy an existing window here. CONTENT_READY stays false until the
    // webview mounts — treating that as "stale" caused close+sleep and froze IPC.
    if app.get_webview_window(AI_HUB_BUBBLE_LABEL).is_some() {
        return Ok(());
    }
    let (pos_x, pos_y) = resolve_bubble_position(app, x, y);
    let (bw, bh) = bubble_box();
    let url = WebviewUrl::App("index.html#/ai-hub-bubble".into());
    // Builder.position() is logical pixels — do NOT pass physical coords there.
    // Set PhysicalPosition immediately after build instead.
    let win = WebviewWindowBuilder::new(app, AI_HUB_BUBBLE_LABEL, url)
        .title("天玑")
        .inner_size(bw as f64, bh as f64)
        // 区间而不是钉死：窗仍 non-resizable，但 `set_ai_hub_bubble_box` 能在档位间切换
        // （菜单临时撑高也走这里）。钉死 min==max 会把 set_size 夹回原形。
        .min_inner_size(BUBBLE_BOX_MIN as f64, BUBBLE_BOX_MIN as f64)
        .max_inner_size(BUBBLE_BOX_MAX_W as f64, BUBBLE_BOX_MAX_H as f64)
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
    let _ = win.set_size(tauri::LogicalSize::new(bw as f64, bh as f64));
    let _ = win.set_position(tauri::PhysicalPosition::new(pos_x, pos_y));
    Ok(())
}

pub fn set_ai_hub_bubble_visible_inner<R: Runtime>(
    app: &AppHandle<R>,
    visible: bool,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    if !visible {
        BUBBLE_SHOULD_SHOW.store(false, Ordering::SeqCst);
        // Keep CONTENT_READY — reuse the same webview on next show.
        if let Some(win) = app.get_webview_window(AI_HUB_BUBBLE_LABEL) {
            park_bubble_window(&win);
        }
        return Ok(());
    }

    BUBBLE_SHOULD_SHOW.store(true, Ordering::SeqCst);
    ensure_ai_hub_bubble(app, x, y)?;
    let Some(win) = app.get_webview_window(AI_HUB_BUBBLE_LABEL) else {
        return Err("bubble window missing".into());
    };
    // Show only after frontend is ready (or immediately on subsequent toggles).
    if BUBBLE_CONTENT_READY.load(Ordering::SeqCst) {
        reveal_bubble_window(app, &win, x, y)?;
    } else {
        // Keep parked / click-through until ready callback reveals it.
        let _ = win.set_ignore_cursor_events(true);
        let (px, py) = resolve_bubble_position(app, x, y);
        let (bw, bh) = bubble_box();
        let _ = win.set_size(tauri::LogicalSize::new(bw as f64, bh as f64));
        let _ = win.set_position(tauri::PhysicalPosition::new(px, py));
    }
    Ok(())
}

/// Called by the bubble webview after CSS/transparent bg are applied.
#[tauri::command]
pub fn ai_hub_bubble_ready(app: AppHandle) -> Result<(), String> {
    BUBBLE_CONTENT_READY.store(true, Ordering::SeqCst);
    if !BUBBLE_SHOULD_SHOW.load(Ordering::SeqCst) {
        if let Some(win) = app.get_webview_window(AI_HUB_BUBBLE_LABEL) {
            park_bubble_window(&win);
        }
        return Ok(());
    }
    let Some(win) = app.get_webview_window(AI_HUB_BUBBLE_LABEL) else {
        return Ok(());
    };
    // Prefer the position already applied during ensure/show; fall back to default if unsafe.
    let (x, y) = match win.outer_position() {
        Ok(pos) if !is_titlebar_danger_zone(&app, pos.x, pos.y) => (Some(pos.x), Some(pos.y)),
        _ => (None, None),
    };
    reveal_bubble_window(&app, &win, x, y)
}

/// 底边中心锚定：宽的变化左右各让一半，高的变化全部让给顶部——桌宠"站"在原地不动。
fn bottom_center_anchor(pos: (i32, i32), old: (i32, i32), new_w: i32, new_h: i32) -> (i32, i32) {
    (pos.0 + (old.0 - new_w) / 2, pos.1 + (old.1 - new_h))
}

/// 桌宠窗换尺寸：档位切换与「就地菜单」开合都走这里。
///
/// 底边中心钉住不动——宠物是"站"在桌面下缘的，撑高只许往头顶长；否则每次开菜单
/// 它都会往下掉一截，看起来像窗在抽搐。窗本身 non-resizable，用户拖不出这个变化。
pub fn set_bubble_box<R: Runtime>(
    app: &AppHandle<R>,
    width: u32,
    height: u32,
    hit: Option<[u32; 4]>,
) -> Result<(), String> {
    let (bw, bh) = store_bubble_box(width, height);
    // 命中矩形跟尺寸一起下发：改尺寸（含开合菜单）时后端才能立刻按新的可交互区切穿透。
    store_hit_rect(hit);
    let Some(win) = app.get_webview_window(AI_HUB_BUBBLE_LABEL) else {
        // 还没建窗：builder 与 reveal 会读到刚存下的尺寸。
        return Ok(());
    };
    let scale = win.scale_factor().unwrap_or(1.0);
    let (new_w, new_h) = bubble_box_physical(scale);
    // 旧尺寸必须在 set_size 之前抓：之后再问 outer_size() 拿到的已经是新值，
    // 减出来恒等于 0，底边锚定就成了摆设 —— 表现就是右键菜单一开、宠物往下掉一截。
    let old = win.outer_size().ok().map(|s| (s.width as i32, s.height as i32));
    let _ = win.set_size(tauri::LogicalSize::new(bw as f64, bh as f64));
    let pos = match win.outer_position() {
        Ok(pos) => pos,
        Err(_) => return Ok(()),
    };
    // Parked off-screen: resize in place, never drag the hidden window back onto the desk.
    if pos.x <= BUBBLE_PARK_POS.0 + 100 || pos.y <= BUBBLE_PARK_POS.1 + 100 {
        return Ok(());
    }
    let (x, y) = match old {
        Some(old) => bottom_center_anchor((pos.x, pos.y), old, new_w, new_h),
        None => (pos.x, pos.y),
    };
    let (px, py) = clamp_bubble_position(app, x, y);
    let _ = win.set_position(tauri::PhysicalPosition::new(px, py));
    Ok(())
}

#[tauri::command]
pub fn set_ai_hub_bubble_box(
    app: AppHandle,
    width: u32,
    height: u32,
    hit: Option<[u32; 4]>,
) -> Result<(), String> {
    set_bubble_box(&app, width, height, hit)
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

pub fn toggle_ai_hub<R: Runtime>(app: &AppHandle<R>) {
    // Never build the window from this path: `WebviewWindowBuilder::build()` does NOT
    // return when it runs on a command thread — it creates the HWND and then blocks
    // forever, so the window stayed permanently hidden and every entry point (bubble
    // click, tray item, Ctrl+Alt+K) looked dead. The window is prepared on the main
    // thread at startup by `prebuild_ai_hub_window`; this path only toggles it.
    let Some(win) = app.get_webview_window(AI_HUB_LABEL) else {
        // Not materialised yet — schedule the prebuild and let the next click toggle it
        // instead of blocking here (see the note above).
        prebuild_ai_hub_window(app);
        return;
    };
    // Judge from the real window, not from `is_visible()` — tao's cached flag desyncs
    // when `ShowWindow` is ignored, which made this toggle a permanent no-op.
    let visible = vis::really_visible(&win).unwrap_or_else(|| win.is_visible().unwrap_or(false));
    // Covered by another app: raise it. Hide only when this window itself is the
    // one the user is looking at. The bubble click steals the foreground before
    // this runs, so the bubble being in front must not count — that turned the
    // first click after an app switch into a hide, and the second click into a show.
    let in_front = vis::is_foreground(&win).unwrap_or(false);
    if visible && in_front {
        if let Err(e) = win.hide() {
            eprintln!("[tray] hide ai-hub failed: {e}");
        }
        let _ = vis::force_visible(&win, false);
    } else {
        // Clear any zoom left behind by an older build before showing.
        let _ = win.set_zoom(1.0);
        if let Err(e) = win.show() {
            eprintln!("[tray] show ai-hub failed: {e}");
        }
        if let Err(e) = win.unminimize() {
            eprintln!("[tray] unminimize ai-hub failed: {e}");
        }
        if !vis::bring_to_front(&win) {
            if let Err(e) = win.set_focus() {
                eprintln!("[tray] focus ai-hub failed: {e}");
            }
            if !vis::force_visible(&win, true) {
                eprintln!("[tray] ai-hub still hidden after force_visible");
            }
        }
        let _ = app.emit("ai-hub-shown", ());
    }
}

pub fn show_ai_hub<R: Runtime>(app: &AppHandle<R>) {
    // Same rule as `toggle_ai_hub`: never build from here (see the comment there).
    let Some(win) = app.get_webview_window(AI_HUB_LABEL) else {
        prebuild_ai_hub_window(app);
        return;
    };
    // Clear any zoom left behind by an older build before showing.
    let _ = win.set_zoom(1.0);
    if let Err(e) = win.show() {
        eprintln!("[tray] show ai-hub failed: {e}");
    }
    if let Err(e) = win.unminimize() {
        eprintln!("[tray] unminimize ai-hub failed: {e}");
    }
    if !vis::bring_to_front(&win) {
        if let Err(e) = win.set_focus() {
            eprintln!("[tray] focus ai-hub failed: {e}");
        }
        if !vis::force_visible(&win, true) {
            eprintln!("[tray] ai-hub still hidden after force_visible");
        }
    }
    let _ = app.emit("ai-hub-shown", ());
}

/// Hide the ai-hub window for real. The frontend used `getCurrentWindow().hide()`,
/// which tao resolves through its cached flag diff — once that cache desyncs (because
/// the real window was shown with `SetWindowPos`) the call becomes a no-op, so Esc /
/// the close button could not dismiss the window. Route it through Rust instead.
pub fn set_ai_hub_pinned<R: Runtime>(app: &AppHandle<R>, pinned: bool) -> Result<(), String> {
    let Some(win) = app.get_webview_window(AI_HUB_LABEL) else {
        return Ok(());
    };
    win.set_always_on_top(pinned).map_err(|e| e.to_string())
}

pub fn hide_ai_hub<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window(AI_HUB_LABEL) {
        if let Err(e) = win.hide() {
            eprintln!("[tray] hide ai-hub failed: {e}");
        }
        vis::force_visible(&win, false);
    }
}

// 测试模块放文件末尾：`clippy::items_after_test_module` 不喜欢测试后面还有别的函数。
#[cfg(test)]
mod bubble_box_tests {
    use super::{bottom_center_anchor, clamp_bubble_box, clamp_to_work_area, WorkArea};

    /// 主屏 1920×1040（0,0）+ 右侧副屏 1920×1040（1920,0），都是物理像素。
    const PRIMARY: WorkArea = (0, 0, 1920, 1040);
    const SECONDARY: WorkArea = (1920, 0, 1920, 1040);

    #[test]
    fn growing_for_the_menu_keeps_the_feet_on_the_ground() {
        // 112 方窗开菜单 → 112×228：底边（y + h）必须落在同一像素上。
        let before = (1600, 900);
        let after = bottom_center_anchor(before, (112, 112), 112, 228);
        assert_eq!(before.1 + 112, after.1 + 228);
        assert_eq!(after, (1600, 784));
    }

    #[test]
    fn a_larger_dial_stays_centred_on_the_same_spot() {
        // 112 → 160：左右各让 24，底边仍然不动。
        let after = bottom_center_anchor((1000, 900), (112, 112), 160, 160);
        assert_eq!(after, (976, 852));
        assert_eq!(after.1 + 160, 900 + 112);
    }

    #[test]
    fn size_dial_values_pass_through_untouched() {
        for dial in [96, 112, 128, 160] {
            assert_eq!(clamp_bubble_box(dial, dial), (dial, dial));
        }
    }

    #[test]
    fn oversized_menu_box_is_clamped_not_rejected() {
        // 菜单撑高只往头顶长：越界夹进 builder 区间，不能让窗建不出来。
        assert_eq!(clamp_bubble_box(112, 4_000), (112, super::BUBBLE_BOX_MAX_H));
        assert_eq!(clamp_bubble_box(4_000, 200), (super::BUBBLE_BOX_MAX_W, 200));
        assert_eq!(clamp_bubble_box(10, 10), (super::BUBBLE_BOX_MIN, super::BUBBLE_BOX_MIN));
    }

    #[test]
    fn a_point_on_the_secondary_monitor_stays_on_it() {
        // 副屏上的正常位置原样通过：夹取只认拿到的这块 work area，不再回主屏。
        assert_eq!(
            clamp_to_work_area(SECONDARY, (192, 208), (3000, 700), 1.0),
            (3000, 700)
        );
        // 越出副屏左缘也只在副屏内收，不会算成主屏的坐标。
        assert_eq!(
            clamp_to_work_area(SECONDARY, (192, 208), (1900, 700), 1.0),
            (1944, 700)
        );
        // 副屏右/下越界同理（1920+1920-192-24 = 3624，1040-208-24 = 808）。
        assert_eq!(
            clamp_to_work_area(SECONDARY, (192, 208), (4000, 2000), 1.0),
            (3624, 808)
        );
    }

    #[test]
    fn opening_the_menu_on_the_secondary_monitor_keeps_the_feet() {
        // 副屏上撑高：底边不动，X 不跳回主屏。
        let before = (2900, 600);
        let anchored = bottom_center_anchor(before, (192, 208), 192, 248);
        let after = clamp_to_work_area(SECONDARY, (192, 248), anchored, 1.0);
        assert_eq!(after, (2900, 560));
        assert_eq!(before.1 + 208, after.1 + 248);
        assert!(after.0 >= SECONDARY.0, "不许被夹回主屏");
    }

    #[test]
    fn the_top_safe_band_holds_on_a_secondary_monitor() {
        // 顶部 96 逻辑像素一直留给主标题栏；副屏也照同一条规则。
        assert_eq!(
            clamp_to_work_area(SECONDARY, (192, 208), (2100, 10), 1.0),
            (2100, 96)
        );
    }

    #[test]
    fn margins_scale_with_the_monitor_dpr() {
        // 150%：边距 24→36、安全带 96→144，窗 192×208 → 288×312。
        let (x, y) = clamp_to_work_area((0, 0, 2560, 1400), (288, 312), (0, 0), 1.5);
        assert_eq!((x, y), (36, 144));
        let (max_x, max_y) = clamp_to_work_area((0, 0, 2560, 1400), (288, 312), (9_000, 9_000), 1.5);
        assert_eq!((max_x, max_y), (2560 - 288 - 36, 1400 - 312 - 36));
    }

    #[test]
    fn clamping_is_idempotent() {
        // 拖动松手会连夹两次（前端吸附 + 后端持久化），第二次不该再动。
        for work in [PRIMARY, SECONDARY] {
            let once = clamp_to_work_area(work, (192, 208), (9999, -40), 1.0);
            assert_eq!(clamp_to_work_area(work, (192, 208), once, 1.0), once);
        }
        // 同一组坐标在相邻两块屏上得到不同结果：夹取真的跟着 work area 走。
        assert_ne!(
            clamp_to_work_area(PRIMARY, (192, 208), (1900, 700), 1.0),
            clamp_to_work_area(SECONDARY, (192, 208), (1900, 700), 1.0)
        );
    }
}
