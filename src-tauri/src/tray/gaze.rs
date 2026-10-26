//! 桌宠视线 + 命中：把全局光标算成「相对桌宠窗中心的方向」，按节拍推给桌宠窗；
//! 同时按前端下发的「可交互矩形」切 `set_ignore_cursor_events`，让窗内透明留白
//! （头顶说话的 96px、角色两侧的空隙）不再吃掉下面窗口的点击。
//!
//! 桌宠窗只有一百来像素，指针几乎不会真的进来 —— 只靠窗口内的 pointermove，桌宠就
//! 永远不知道用户在屏幕哪儿，看着就是「呆」。这里在后台按固定节拍读一次 Win32 光标
//! 位置（`GetCursorPos` 是普通用户态查询，不需要任何权限，也不装钩子），只在桌宠窗
//! 真正可见、且值确实变了的时候发事件。
//!
//! 视线 payload 是归一化方向：`{ x, y }` 落在 [-1, 1]，(0, 0) = 窗中心，x 向右为正、
//! y 向下为正。命中 payload 是 `{ hot, x, y }`：`hot` = 光标是否压在那块可交互矩形里，
//! `x`/`y` 是它在该矩形内的归一化位置（0..1），前端靠它区分「摸头 / 戳肚 / 裙边」。
//! 前端（`AiHubBubble`）只把它们写进 CSS 变量 / class，不产生重渲染潮。

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::bubble::{bubble_box, bubble_hit_rect, AI_HUB_BUBBLE_LABEL};

/// 视线事件名（与前端 `petSkins.ts` 的 `PET_GAZE_EVENT` 一致，改名要一起改）。
pub const GAZE_EVENT: &str = "ai-hub-pet-gaze";
/// 命中状态事件名（与前端 `petSkins.ts` 的 `PET_HOT_EVENT` 一致）。
pub const HOT_EVENT: &str = "ai-hub-pet-hot";

/// 轮询节拍：70ms ≈ 14 次/秒，比人追踪光标的节奏略慢，但已经够顺。
const POLL_MS: u64 = 70;
/// 窗不可见 / 被 park 到屏外时降到这个间隔：只为下一次显窗能尽快复位，不需要 14Hz。
const POLL_IDLE_MS: u64 = 400;
/// 方向变化小于这个量就不发：光标停着时，前端一条事件都不该收到。
const MIN_DELTA: f64 = 0.012;

#[derive(Clone, serde::Serialize)]
struct GazePayload {
    x: f64,
    y: f64,
}

#[derive(Clone, serde::Serialize)]
struct HotPayload {
    hot: bool,
    /// 光标在可交互矩形里的归一化位置；`hot == false` 时无意义，固定 0。
    x: f64,
    y: f64,
}

/// 一次采样的结果。
struct Probe {
    gaze: (f64, f64),
    hot: bool,
    hit_pos: (f64, f64),
}

/// 启动光标观察者。桌宠窗不存在 / 不可见时只做一次系统调用就进入慢速节拍。
pub fn spawn_gaze_watcher<R: Runtime>(app: AppHandle<R>) {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn(async move {
            let mut last_gaze: Option<(f64, f64)> = None;
            let mut last_hot: Option<bool> = None;
            let mut ignoring: Option<bool> = None;
            loop {
                let Some(win) = app.get_webview_window(AI_HUB_BUBBLE_LABEL) else {
                    last_gaze = None;
                    last_hot = None;
                    ignoring = None;
                    tokio::time::sleep(Duration::from_millis(POLL_IDLE_MS)).await;
                    continue;
                };
                let Some(probe) = probe(&win) else {
                    // 收起来了（隐藏时被 park 到屏外）：清缓存，下次显窗必发一条最新方向。
                    last_gaze = None;
                    if last_hot != Some(false) {
                        last_hot = Some(false);
                        let _ = app.emit_to(
                            AI_HUB_BUBBLE_LABEL,
                            HOT_EVENT,
                            HotPayload {
                                hot: false,
                                x: 0.0,
                                y: 0.0,
                            },
                        );
                    }
                    // 显窗路径（reveal）自己会把穿透关掉，这里只管下次重新判定。
                    ignoring = None;
                    tokio::time::sleep(Duration::from_millis(POLL_IDLE_MS)).await;
                    continue;
                };

                if last_hot != Some(probe.hot) {
                    last_hot = Some(probe.hot);
                    let _ = app.emit_to(
                        AI_HUB_BUBBLE_LABEL,
                        HOT_EVENT,
                        HotPayload {
                            hot: probe.hot,
                            x: probe.hit_pos.0,
                            y: probe.hit_pos.1,
                        },
                    );
                }

                // 左键按住（点按 / 拖动）期间绝不切到穿透：拖到一半丢事件会让拖拽永远收不了尾。
                // 只在状态翻转时才调用，避免每帧切一次窗口样式。
                let want_ignore = !probe.hot && !left_button_down();
                if ignoring != Some(want_ignore) {
                    ignoring = Some(want_ignore);
                    let _ = win.set_ignore_cursor_events(want_ignore);
                }

                let moved = last_gaze.is_none_or(|(lx, ly)| {
                    (probe.gaze.0 - lx).abs() > MIN_DELTA || (probe.gaze.1 - ly).abs() > MIN_DELTA
                });
                if moved {
                    last_gaze = Some(probe.gaze);
                    let _ = app.emit_to(
                        AI_HUB_BUBBLE_LABEL,
                        GAZE_EVENT,
                        GazePayload {
                            x: probe.gaze.0,
                            y: probe.gaze.1,
                        },
                    );
                }

                tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
            }
        });
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// 光标方向 + 命中判定；窗口不可见或几何读不到时返回 `None`。
#[cfg(windows)]
fn probe<R: Runtime>(win: &tauri::WebviewWindow<R>) -> Option<Probe> {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect, IsWindowVisible};

    // tauri 的 HWND 是 `isize` 包装，windows crate 的要裸指针，这里手转一次。
    let hwnd = HWND(win.hwnd().ok()?.0 as *mut _);
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let mut cursor = POINT::default();
        GetCursorPos(&mut cursor).ok()?;
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        // 两者都是物理像素，DPR 在相除时自动约掉，前端不需要再换算。
        let w = (rect.right - rect.left) as f64;
        let h = (rect.bottom - rect.top) as f64;
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let nx = ((cursor.x as f64 - (rect.left as f64 + w / 2.0)) / (w / 2.0)).clamp(-1.0, 1.0);
        let ny = ((cursor.y as f64 - (rect.top as f64 + h / 2.0)) / (h / 2.0)).clamp(-1.0, 1.0);

        // 可交互矩形是逻辑像素 → 物理像素：拿窗的真实宽度与逻辑宽度做比例，
        // 不查 DPI，窗口被拖到不同缩放比的屏幕上时也自然对齐。
        let (logic_w, _) = bubble_box();
        let scale = if logic_w > 0 { w / logic_w as f64 } else { 1.0 };
        let (hx, hy, hw, hh) = bubble_hit_rect();
        let (hit_left, hit_top, hit_w, hit_h) = if hw == 0 || hh == 0 {
            // 还没收到命中矩形（或菜单打开要求整窗可点）
            (rect.left as f64, rect.top as f64, w, h)
        } else {
            (
                rect.left as f64 + hx as f64 * scale,
                rect.top as f64 + hy as f64 * scale,
                hw as f64 * scale,
                hh as f64 * scale,
            )
        };
        let cx = cursor.x as f64;
        let cy = cursor.y as f64;
        let hot = cx >= hit_left
            && cx < hit_left + hit_w
            && cy >= hit_top
            && cy < hit_top + hit_h;
        let hit_pos = if hot {
            (
                ((cx - hit_left) / hit_w.max(1.0)).clamp(0.0, 1.0),
                ((cy - hit_top) / hit_h.max(1.0)).clamp(0.0, 1.0),
            )
        } else {
            (0.0, 0.0)
        };
        Some(Probe {
            gaze: (nx, ny),
            hot,
            hit_pos,
        })
    }
}

/// 左键当前是否按着（最高位 = 按下）。`GetAsyncKeyState` 是普通用户态查询，不装钩子。
#[cfg(windows)]
fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    (unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000) != 0
}
