//! 桌宠视线：把全局光标算成「相对桌宠窗中心的方向」，按节拍推给桌宠窗。
//!
//! 桌宠窗只有一百来像素，指针几乎不会真的进来 —— 只靠窗口内的 pointermove，桌宠就
//! 永远不知道用户在屏幕哪儿，看着就是「呆」。这里在后台按固定节拍读一次 Win32 光标
//! 位置（`GetCursorPos` 是普通用户态查询，不需要任何权限，也不装钩子），只在桌宠窗
//! 真正可见、且方向确实变了的时候发一条事件。
//!
//! payload 是归一化方向：`{ x, y }` 落在 [-1, 1]，(0, 0) = 窗中心，x 向右为正、
//! y 向下为正。前端（`AiHubBubble`）只把它写进 CSS 变量，不产生 React 渲染。

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::bubble::AI_HUB_BUBBLE_LABEL;

/// 视线事件名（与前端 `petSkins.ts` 的 `PET_GAZE_EVENT` 一致，改名要一起改）。
pub const GAZE_EVENT: &str = "ai-hub-pet-gaze";

/// 轮询节拍：70ms ≈ 14 次/秒，比人追踪光标的节奏略慢，但已经够顺。
const POLL_MS: u64 = 70;
/// 方向变化小于这个量就不发：光标停着时，前端一条事件都不该收到。
const MIN_DELTA: f64 = 0.012;

#[derive(Clone, serde::Serialize)]
struct GazePayload {
    x: f64,
    y: f64,
}

/// 启动视线轮询。桌宠窗不存在 / 不可见时什么都不发，开销只够覆盖一次系统调用。
pub fn spawn_gaze_watcher<R: Runtime>(app: AppHandle<R>) {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn(async move {
            let mut last: Option<(f64, f64)> = None;
            loop {
                tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
                let gaze = app
                    .get_webview_window(AI_HUB_BUBBLE_LABEL)
                    .and_then(|win| gaze_of(&win));
                match gaze {
                    Some((x, y)) => {
                        let moved = last.is_none_or(|(lx, ly)| {
                            (x - lx).abs() > MIN_DELTA || (y - ly).abs() > MIN_DELTA
                        });
                        if moved {
                            last = Some((x, y));
                            let _ = app.emit_to(
                                AI_HUB_BUBBLE_LABEL,
                                GAZE_EVENT,
                                GazePayload { x, y },
                            );
                        }
                    }
                    // 窗收起来了（隐藏时被 park 到屏外）：清掉缓存，下次显窗必发一条最新方向。
                    None => last = None,
                }
            }
        });
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// 光标相对窗口中心的归一化方向；窗口不可见或几何读不到时返回 `None`。
#[cfg(windows)]
fn gaze_of<R: Runtime>(win: &tauri::WebviewWindow<R>) -> Option<(f64, f64)> {
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
        Some((nx, ny))
    }
}
