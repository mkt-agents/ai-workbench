//! AI hub agent computer use (EXP, 完全权限 + vision model only): capture the primary
//! screen, move / click / scroll the mouse, type and press keys. Coordinates given by
//! the model are pixels of the latest screenshot and are scaled back here.

use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::agent_fs::arg_str;

/// Longest screenshot side sent to the model.
const SHOT_MAX: u32 = 1280;
const WAIT_MAX_MS: u64 = 10_000;
/// Cursor within this many pixels of the top-left corner = user pulled the emergency stop.
const FAILSAFE_PX: i32 = 2;

pub(crate) const FAILSAFE_MESSAGE: &str = "用户把鼠标移到了屏幕左上角，已紧急停止操作电脑。";

/// Physical pixels per screenshot pixel, set by the last screenshot (1.0 before any).
static SCALE: Mutex<f64> = Mutex::new(1.0);

/// Screenshot size for a `w`×`h` screen and the physical-per-screenshot scale.
pub(crate) fn shot_size(w: u32, h: u32) -> (u32, u32, f64) {
    let long = w.max(h);
    if long <= SHOT_MAX {
        return (w, h, 1.0);
    }
    let scale = long as f64 / SHOT_MAX as f64;
    (((w as f64) / scale).round() as u32, ((h as f64) / scale).round() as u32, scale)
}

/// Screenshot coordinate → physical pixel.
pub(crate) fn to_physical(v: f64, scale: f64) -> i32 {
    (v * scale).round() as i32
}

/// Box-filter downscale of tightly packed BGRA rows into RGB.
pub(crate) fn downscale_bgra_to_rgb(src: &[u8], w: u32, h: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dw * dh * 3) as usize);
    for y in 0..dh {
        let y0 = (y as u64 * h as u64 / dh as u64) as u32;
        let y1 = (((y + 1) as u64 * h as u64 / dh as u64) as u32).max(y0 + 1).min(h);
        for x in 0..dw {
            let x0 = (x as u64 * w as u64 / dw as u64) as u32;
            let x1 = (((x + 1) as u64 * w as u64 / dw as u64) as u32).max(x0 + 1).min(w);
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * w + sx) * 4) as usize;
                    b += src[i] as u64;
                    g += src[i + 1] as u64;
                    r += src[i + 2] as u64;
                    n += 1;
                }
            }
            out.extend([(r / n) as u8, (g / n) as u8, (b / n) as u8]);
        }
    }
    out
}

fn encode_png(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgb).map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

/// Virtual-key codes for a combo like `ctrl+shift+s` (modifiers first, as typed).
pub(crate) fn parse_combo(combo: &str) -> Result<Vec<(u16, bool)>, String> {
    let mut keys = Vec::new();
    for raw in combo.split('+').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) {
        // (virtual key, extended key flag)
        let key = match raw.as_str() {
            "ctrl" | "control" => (0x11, false),
            "shift" => (0x10, false),
            "alt" => (0x12, false),
            "win" | "windows" | "meta" | "cmd" => (0x5B, true),
            "enter" | "return" => (0x0D, false),
            "esc" | "escape" => (0x1B, false),
            "tab" => (0x09, false),
            "space" => (0x20, false),
            "backspace" => (0x08, false),
            "delete" | "del" => (0x2E, true),
            "insert" | "ins" => (0x2D, true),
            "home" => (0x24, true),
            "end" => (0x23, true),
            "pageup" | "pgup" => (0x21, true),
            "pagedown" | "pgdn" => (0x22, true),
            "up" => (0x26, true),
            "down" => (0x28, true),
            "left" => (0x25, true),
            "right" => (0x27, true),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => {
                (k.to_ascii_uppercase().as_bytes()[0] as u16, false)
            }
            k if k.starts_with('f') && k[1..].parse::<u16>().map_or(false, |n| (1..=24).contains(&n)) => {
                (0x6F + k[1..].parse::<u16>().unwrap_or(1), false)
            }
            other => return Err(format!("不认识的按键：{other}")),
        };
        keys.push(key);
    }
    if keys.is_empty() {
        return Err("缺少按键".into());
    }
    Ok(keys)
}

fn arg_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

fn scale() -> f64 {
    *SCALE.lock().expect("shot scale")
}

/// `(text, png_base64)` of the primary screen.
pub(crate) fn screenshot() -> Result<(String, String), String> {
    use base64::Engine;
    let (bgra, w, h) = win::capture_primary()?;
    let (dw, dh, s) = shot_size(w, h);
    *SCALE.lock().expect("shot scale") = s;
    let rgb = if s == 1.0 {
        bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect()
    } else {
        downscale_bgra_to_rgb(&bgra, w, h, dw, dh)
    };
    let png = encode_png(&rgb, dw, dh)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let (cx, cy) = win::cursor_pos();
    let text = format!(
        "截图 {dw}×{dh}（屏幕 {w}×{h}）。坐标按这张截图的像素给。当前鼠标在 ({}, {})。",
        (cx as f64 / s).round(),
        (cy as f64 / s).round()
    );
    Ok((text, b64))
}

/// Emergency stop: the user parked the cursor in the top-left corner.
pub(crate) fn failsafe_tripped() -> bool {
    let (x, y) = win::cursor_pos();
    x <= FAILSAFE_PX && y <= FAILSAFE_PX
}

pub(crate) fn mouse(args: &Value) -> Result<String, String> {
    if failsafe_tripped() {
        return Err(FAILSAFE_MESSAGE.into());
    }
    let s = scale();
    let action = arg_str(args, "action");
    let point = |kx: &str, ky: &str| -> Result<(i32, i32), String> {
        match (arg_f64(args, kx), arg_f64(args, ky)) {
            (Some(x), Some(y)) => Ok((to_physical(x, s), to_physical(y, s))),
            _ => Err(format!("{action} 需要坐标 {kx} / {ky}")),
        }
    };
    match action.as_str() {
        "move" => {
            let (x, y) = point("x", "y")?;
            win::move_to(x, y)?;
        }
        "click" | "double_click" | "right_click" => {
            if args.get("x").is_some() {
                let (x, y) = point("x", "y")?;
                win::move_to(x, y)?;
            }
            let right = action == "right_click";
            let times = if action == "double_click" { 2 } else { 1 };
            for _ in 0..times {
                win::click(right)?;
                std::thread::sleep(Duration::from_millis(60));
            }
        }
        "drag" => {
            let (x0, y0) = point("x", "y")?;
            let (x1, y1) = point("to_x", "to_y")?;
            win::drag(x0, y0, x1, y1)?;
        }
        "scroll" => {
            if args.get("x").is_some() {
                let (x, y) = point("x", "y")?;
                win::move_to(x, y)?;
            }
            // Positive = up, like the wheel; one notch = 120.
            let notches = arg_f64(args, "amount").unwrap_or(-3.0).clamp(-30.0, 30.0) as i32;
            win::scroll(notches)?;
        }
        other => return Err(format!("mouse 不支持 action={other}，可用 move / click / double_click / right_click / drag / scroll")),
    }
    std::thread::sleep(Duration::from_millis(150));
    Ok(format!("鼠标 {action} 完成。需要确认结果时再截图。"))
}

pub(crate) fn keyboard(args: &Value) -> Result<String, String> {
    if failsafe_tripped() {
        return Err(FAILSAFE_MESSAGE.into());
    }
    match arg_str(args, "action").as_str() {
        "type" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() {
                return Err("type 需要 text".into());
            }
            if text.chars().count() > 2_000 {
                return Err("一次最多输入 2000 字".into());
            }
            win::type_text(text)?;
            Ok(format!("已输入 {} 字", text.chars().count()))
        }
        "key" => {
            let combo = arg_str(args, "keys");
            let keys = parse_combo(&combo)?;
            win::press(&keys)?;
            std::thread::sleep(Duration::from_millis(150));
            Ok(format!("已按下 {combo}"))
        }
        other => Err(format!("keyboard 不支持 action={other}，可用 type / key")),
    }
}

pub(crate) fn wait(args: &Value) -> Result<String, String> {
    let ms = args.get("ms").and_then(|v| v.as_u64()).unwrap_or(1_000).min(WAIT_MAX_MS);
    std::thread::sleep(Duration::from_millis(ms));
    if failsafe_tripped() {
        return Err(FAILSAFE_MESSAGE.into());
    }
    Ok(format!("已等待 {ms} 毫秒"))
}

/// Keep the AI hub window itself out of screenshots (Windows 10 2004+), so the model
/// never sees or clicks its own chat window.
pub(crate) fn exclude_from_capture(app: &tauri::AppHandle) {
    #[cfg(windows)]
    {
        use tauri::Manager;
        if let Some(window) = app.get_webview_window(crate::tray::bubble::AI_HUB_LABEL) {
            if let Ok(hwnd) = window.hwnd() {
                win::exclude_hwnd(hwnd.0 as isize);
            }
        }
    }
    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetSystemMetrics, SetCursorPos, SetWindowDisplayAffinity, SM_CXSCREEN, SM_CYSCREEN,
        WINDOW_DISPLAY_AFFINITY,
    };

    pub(super) fn capture_primary() -> Result<(Vec<u8>, u32, u32), String> {
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w <= 0 || h <= 0 {
                return Err("读不到屏幕尺寸".into());
            }
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bmp.into());
            let blit = BitBlt(mem, 0, 0, w, h, Some(screen), 0, 0, SRCCOPY | CAPTUREBLT);
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    // Negative height = top-down rows.
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let lines = GetDIBits(mem, bmp, 0, h as u32, Some(buf.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            if blit.is_err() || lines == 0 {
                return Err("截屏失败".into());
            }
            Ok((buf, w as u32, h as u32))
        }
    }

    pub(super) fn cursor_pos() -> (i32, i32) {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        (p.x, p.y)
    }

    pub(super) fn move_to(x: i32, y: i32) -> Result<(), String> {
        unsafe { SetCursorPos(x, y) }.map_err(|e| format!("移动鼠标失败: {e}"))
    }

    fn mouse_input(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    fn send(inputs: &[INPUT]) -> Result<(), String> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err("系统拒绝了模拟输入（可能是有管理员权限的窗口在前台）".into())
        }
    }

    pub(super) fn click(right: bool) -> Result<(), String> {
        let (down, up) = if right {
            (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
        } else {
            (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
        };
        send(&[mouse_input(down, 0), mouse_input(up, 0)])
    }

    pub(super) fn drag(x0: i32, y0: i32, x1: i32, y1: i32) -> Result<(), String> {
        move_to(x0, y0)?;
        send(&[mouse_input(MOUSEEVENTF_LEFTDOWN, 0)])?;
        for i in 1..=10 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            move_to(x0 + (x1 - x0) * i / 10, y0 + (y1 - y0) * i / 10)?;
        }
        send(&[mouse_input(MOUSEEVENTF_LEFTUP, 0)])
    }

    pub(super) fn scroll(notches: i32) -> Result<(), String> {
        send(&[mouse_input(MOUSEEVENTF_WHEEL, notches * 120)])
    }

    fn key_input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    pub(super) fn type_text(text: &str) -> Result<(), String> {
        let mut inputs = Vec::new();
        for unit in text.encode_utf16() {
            inputs.push(key_input(0, unit, KEYEVENTF_UNICODE));
            inputs.push(key_input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        for chunk in inputs.chunks(200) {
            send(chunk)?;
        }
        Ok(())
    }

    pub(super) fn press(keys: &[(u16, bool)]) -> Result<(), String> {
        let flag = |ext: bool| if ext { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
        let mut inputs: Vec<INPUT> = keys.iter().map(|(vk, ext)| key_input(*vk, 0, flag(*ext))).collect();
        inputs.extend(keys.iter().rev().map(|(vk, ext)| key_input(*vk, 0, flag(*ext) | KEYEVENTF_KEYUP)));
        send(&inputs)
    }

    pub(super) fn exclude_hwnd(raw: isize) {
        // WDA_EXCLUDEFROMCAPTURE
        unsafe {
            let _ = SetWindowDisplayAffinity(HWND(raw as *mut _), WINDOW_DISPLAY_AFFINITY(0x11));
        }
    }
}

#[cfg(not(windows))]
mod win {
    pub(super) fn capture_primary() -> Result<(Vec<u8>, u32, u32), String> {
        Err("当前系统不支持".into())
    }
    pub(super) fn cursor_pos() -> (i32, i32) {
        (100, 100)
    }
    pub(super) fn move_to(_: i32, _: i32) -> Result<(), String> {
        Err("当前系统不支持".into())
    }
    pub(super) fn click(_: bool) -> Result<(), String> {
        Err("当前系统不支持".into())
    }
    pub(super) fn drag(_: i32, _: i32, _: i32, _: i32) -> Result<(), String> {
        Err("当前系统不支持".into())
    }
    pub(super) fn scroll(_: i32) -> Result<(), String> {
        Err("当前系统不支持".into())
    }
    pub(super) fn type_text(_: &str) -> Result<(), String> {
        Err("当前系统不支持".into())
    }
    pub(super) fn press(_: &[(u16, bool)]) -> Result<(), String> {
        Err("当前系统不支持".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shots_are_scaled_to_the_long_side() {
        assert_eq!(shot_size(1280, 720, ), (1280, 720, 1.0));
        let (w, h, s) = shot_size(2560, 1440);
        assert_eq!((w, h), (1280, 720));
        assert!((s - 2.0).abs() < 1e-9);
        assert_eq!(to_physical(640.0, s), 1280);
        let (w, h, _) = shot_size(1080, 1920);
        assert_eq!((w, h), (720, 1280));
    }

    #[test]
    fn combos_parse_to_virtual_keys() {
        assert_eq!(parse_combo("ctrl+s").unwrap(), vec![(0x11, false), (0x53, false)]);
        assert_eq!(parse_combo("Ctrl + Shift + F5").unwrap(), vec![(0x11, false), (0x10, false), (0x74, false)]);
        assert_eq!(parse_combo("alt+left").unwrap(), vec![(0x12, false), (0x25, true)]);
        assert_eq!(parse_combo("enter").unwrap(), vec![(0x0D, false)]);
        assert_eq!(parse_combo("win+r").unwrap(), vec![(0x5B, true), (0x52, false)]);
        assert!(parse_combo("ctrl+banana").is_err());
        assert!(parse_combo("").is_err());
    }

    #[test]
    fn downscale_averages_blocks() {
        // 2×2 BGRA: two red, two blue pixels → one purple-ish pixel.
        let src = [0, 0, 255, 255, 0, 0, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255];
        assert_eq!(downscale_bgra_to_rgb(&src, 2, 2, 1, 1), vec![127, 0, 127]);
    }
}
