//! System tray icon, menu, and status polling.
//!
//! The tray shows DSH/tunnel run state in its tooltip and menu, and hosts the
//! thin `#[tauri::command]` entry points that the frontend invokes. Window
//! management for the ai-hub bubble lives in the `bubble` submodule.

pub mod bubble;
pub mod gaze;

// `ensure_ai_hub_window` stays internal to `bubble` — it is called on demand by the
// toggle/show paths, never from the app setup.
pub use bubble::set_ai_hub_bubble_visible_inner;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};

use crate::config::TRAY_POLL_INTERVAL_SECS;
use crate::cloudflared_commands::CloudflaredState;

#[derive(Default)]
pub struct TrayTunnelUrls(pub Mutex<HashMap<String, String>>);

/// 托盘菜单内容指纹：5s 轮询里内容没变就别 `set_menu`。
/// Windows 上替换 HMENU 会拆掉正在打开的右键菜单，点「显示主窗口」等于点空气。
static LAST_TRAY_MENU_FP: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));

fn tray_menu_fingerprint(dsh_ports: &[u16], tunnels: &[(String, Option<String>)]) -> String {
    let mut fp = String::new();
    if dsh_ports.is_empty() {
        fp.push_str("dsh:off;");
    } else {
        fp.push_str(&format!("dsh:{};", dsh_ports[0]));
    }
    for (name, url) in tunnels.iter().take(6) {
        fp.push_str(name);
        fp.push('|');
        if let Some(u) = url {
            fp.push_str(u);
        }
        fp.push(';');
    }
    fp
}

fn build_tooltip(dsh_ports: &[u16], tunnel_count: usize) -> String {
    let dsh = if dsh_ports.is_empty() {
        "DSH 未运行".to_string()
    } else {
        let ports: Vec<String> = dsh_ports.iter().map(|p| format!(":{p}")).collect();
        format!("DSH {} 运行中", ports.join(" "))
    };
    let base = format!("{dsh} · 隧道 x{tunnel_count}");
    // Dev-instance marker, compile-time stripped from release builds
    // (same rule as the "[DEV]" window title suffix in lib.rs).
    if cfg!(debug_assertions) {
        format!("[DEV] {base}")
    } else {
        base
    }
}

fn apply_tray_menu<R: Runtime>(
    app: &AppHandle<R>,
    dsh_ports: &[u16],
    tunnels: &[(String, Option<String>)],
) -> Result<(), String> {
    let fp = tray_menu_fingerprint(dsh_ports, tunnels);
    if LAST_TRAY_MENU_FP
        .lock()
        .map(|last| *last == fp)
        .unwrap_or(false)
    {
        return Ok(());
    }

    let show_main = MenuItem::with_id(app, "show_main", "显示主窗口", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let ai_hub = MenuItem::with_id(app, "ai_hub", "打开天玑", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    // 桌宠只能从它自己的右键菜单「隐藏桌宠」收起，托盘必须留一条把它叫回来的路。
    // 状态（settings 在 localStorage）与位置都只有主窗知道，所以这里只发一条意向事件。
    let pet_toggle = MenuItem::with_id(app, "pet_toggle", "显示/隐藏桌宠", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let sep1 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;

    let dsh_label = if dsh_ports.is_empty() {
        "启动 DeepSeek".to_string()
    } else {
        format!("停止 DeepSeek (:{})", dsh_ports[0])
    };
    let dsh_toggle = MenuItem::with_id(app, "dsh_toggle", dsh_label, true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let sep2 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;

    let mut tunnel_items = Vec::new();
    {
        let map = app.state::<TrayTunnelUrls>();
        let mut guard = map.0.lock().map_err(|e| e.to_string())?;
        guard.clear();
        for (i, (name, url)) in tunnels.iter().enumerate().take(6) {
            let id = format!("tunnel_copy_{i}");
            let label = if url.is_some() {
                format!("复制隧道 URL · {name}")
            } else {
                format!("隧道 · {name}")
            };
            if let Ok(item) = MenuItem::with_id(app, &id, &label, url.is_some(), None::<&str>) {
                if let Some(u) = url {
                    guard.insert(id, u.clone());
                }
                tunnel_items.push(item);
            }
        }
    }

    let sep3 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>).map_err(|e| e.to_string())?;

    let mut refs: Vec<&dyn tauri::menu::IsMenuItem<R>> =
        vec![&show_main, &ai_hub, &pet_toggle, &sep1, &dsh_toggle, &sep2];
    for item in &tunnel_items {
        refs.push(item);
    }
    if !tunnel_items.is_empty() {
        refs.push(&sep3);
    }
    refs.push(&quit);

    let menu = Menu::with_items(app, &refs).map_err(|e| e.to_string())?;
    if let Some(tray) = app.tray_by_id("main_tray") {
        tray.set_menu(Some(menu)).map_err(|e| e.to_string())?;
        if let Ok(mut last) = LAST_TRAY_MENU_FP.lock() {
            *last = fp;
        }
    }
    Ok(())
}

pub fn init_tray(app: &AppHandle) -> Result<(), String> {
    let tooltip = build_tooltip(&[], 0);

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| "missing default window icon".to_string())?;

    let show_main = MenuItem::with_id(app, "show_main", "显示主窗口", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let ai_hub = MenuItem::with_id(app, "ai_hub", "打开天玑", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let pet_toggle = MenuItem::with_id(app, "pet_toggle", "显示/隐藏桌宠", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let sep1 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let dsh_toggle = MenuItem::with_id(app, "dsh_toggle", "启动 DeepSeek", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let sep2 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>).map_err(|e| e.to_string())?;
    let menu = Menu::with_items(
        app,
        &[&show_main, &ai_hub, &pet_toggle, &sep1, &dsh_toggle, &sep2, &quit],
    )
    .map_err(|e| e.to_string())?;

    TrayIconBuilder::with_id("main_tray")
        .icon(icon)
        .tooltip(&tooltip)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            match id {
                "show_main" => bubble::show_main_window(app),
                "ai_hub" => bubble::show_ai_hub(app),
                // 桌宠的显隐与位置都记在主窗的 settings 里（localStorage），
                // 这里只发一条意向让主窗去真正落盘 + 调 set_ai_hub_bubble_visible。
                "pet_toggle" => {
                    let _ = app.emit("ai-hub-pet-toggle", ());
                }
                "dsh_toggle" => handle_dsh_toggle(app),
                "quit" => {
                    // Stop all cloudflared tunnels before exiting so they don't
                    // survive as orphaned processes.
                    crate::cloudflared_commands::stop_all_sessions(
                        &app.state::<CloudflaredState>(),
                    );
                    app.exit(0);
                }
                other if other.starts_with("tunnel_copy_") => {
                    let url = app
                        .state::<TrayTunnelUrls>()
                        .0
                        .lock()
                        .ok()
                        .and_then(|map| map.get(other).cloned());
                    if let Some(url) = url {
                        tauri::async_runtime::spawn(async move {
                            let copied = tauri::async_runtime::spawn_blocking(move || {
                                crate::tool_commands::copy_to_clipboard_blocking(url)
                            })
                            .await;
                            match copied {
                                Ok(Ok(())) => {}
                                Ok(Err(e)) => eprintln!("[tray] copy tunnel url failed: {e}"),
                                Err(e) => eprintln!("[tray] copy tunnel url task failed: {e}"),
                            }
                        });
                    }
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            // 左键抬起唤出主窗（Windows 托盘溢出区经常只可靠地给 UP，
            // DOWN/双击会被 shell 吃掉做拖拽，所以不能只认 DoubleClick）。
            // show_main_window 幂等：双击序列里的第二次 UP 再进一次也无害。
            // 右键仍走菜单；「显示主窗口」是同一条 show_main_window。
            let left_up = matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            );
            if left_up {
                bubble::show_main_window(tray.app_handle());
            }
        })
        .build(app)
        .map_err(|e| e.to_string())?;

    // 与上面初始菜单对齐，避免首轮 5s 轮询无意义地拆掉 HMENU。
    if let Ok(mut last) = LAST_TRAY_MENU_FP.lock() {
        *last = tray_menu_fingerprint(&[], &[]);
    }

    if let Some(main) = app.get_webview_window(bubble::MAIN_LABEL) {
        bubble::attach_main_close_handler(app, &main);
    } else {
        eprintln!("[tray] init: main window missing, close-to-tray disabled");
    }

    let poll_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(TRAY_POLL_INTERVAL_SECS)).await;
            let _ = refresh_tray_status(&poll_app);
        }
    });

    Ok(())
}

fn handle_dsh_toggle(app: &AppHandle) {
    let Some(win) = app.get_webview_window(bubble::MAIN_LABEL) else {
        return;
    };
    let window = win.as_ref().window();
    let ports: Vec<u16> = {
        let state = app.state::<crate::DshState>();
        let ports = match state.instances.lock() {
            Ok(g) => g.iter().map(|i| i.port).collect(),
            Err(_) => return,
        };
        ports
    };
    // 托盘菜单是同步回调：future 必须 spawn 出去。写成 `let _ = ...` 等于把 future
    // 直接丢掉，一个字节的启动 / 停止逻辑都不会执行（clippy::let_underscore_future）。
    if ports.is_empty() {
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            let _ = crate::dsh_commands::start_dsh(None, window).await;
        });
    } else {
        let port = ports[0];
        let window = window.clone();
        tauri::async_runtime::spawn(async move {
            let _ = crate::dsh_commands::stop_dsh(port, window).await;
        });
    }
    let _ = refresh_tray_status(app);
}

pub fn refresh_tray_status(app: &AppHandle) -> Result<(), String> {
    let dsh_ports: Vec<u16> = {
        let state = app.state::<crate::DshState>();
        let ports = state
            .instances
            .lock()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|i| i.port)
            .collect();
        ports
    };

    let tunnels: Vec<(String, Option<String>)> = {
        let list =
            crate::cloudflared_commands::cloudflared_tunnel_status(app.state());
        list.into_iter()
            .filter(|t| t.running)
            .map(|t| {
                let name = t
                    .profile_id
                    .clone()
                    .or_else(|| t.public_url.clone())
                    .unwrap_or_else(|| t.id.clone());
                (name, t.public_url)
            })
            .collect()
    };

    let tooltip = build_tooltip(&dsh_ports, tunnels.len());

    if let Some(tray) = app.tray_by_id("main_tray") {
        let _ = tray.set_tooltip(Some(tooltip.as_str()));
    }
    let _ = apply_tray_menu(app, &dsh_ports, &tunnels);

    Ok(())
}

#[tauri::command]
pub fn open_ai_hub_with_text(
    app: AppHandle,
    text: String,
    task: Option<String>,
) -> Result<(), String> {
    bubble::show_ai_hub(&app);
    // Emit to both ai-hub windows with a unique event ID so each frontend
    // instance can deduplicate (they run in separate processes).
    let event_id = format!("{}-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis(), text.len());
    let mut payload = serde_json::json!({ "id": event_id, "text": text });
    if let Some(task) = task {
        payload["task"] = serde_json::Value::String(task);
    }
    if let Some(win) = app.get_webview_window(bubble::AI_HUB_LABEL) {
        let _ = win.emit("ai-hub-prefill", payload.clone());
    }
    if let Some(win) = app.get_webview_window(bubble::AI_HUB_BUBBLE_LABEL) {
        let _ = win.emit("ai-hub-prefill", payload);
    }
    Ok(())
}

#[tauri::command]
pub fn tray_toggle_ai_hub(app: AppHandle) -> Result<(), String> {
    bubble::toggle_ai_hub(&app);
    Ok(())
}

#[tauri::command]
pub fn show_ai_hub(app: AppHandle) -> Result<(), String> {
    bubble::show_ai_hub(&app);
    Ok(())
}

#[tauri::command]
pub fn hide_ai_hub(app: AppHandle) -> Result<(), String> {
    bubble::hide_ai_hub(&app);
    Ok(())
}

#[tauri::command]
pub fn set_ai_hub_pinned(app: AppHandle, pinned: bool) -> Result<(), String> {
    bubble::set_ai_hub_pinned(&app, pinned)
}

/// Show the main window without switching tabs (pet context menu "打开主界面").
#[tauri::command]
pub fn open_main(app: AppHandle) -> Result<(), String> {
    bubble::show_main_window(&app);
    Ok(())
}

#[tauri::command]
pub fn open_main_deepseek(app: AppHandle) -> Result<(), String> {
    bubble::show_main_window(&app);
    let _ = app.emit("navigate-tab", "ai-chat");
    Ok(())
}

/// Show the main window and switch it to a whitelisted tab (ai-hub tools panel jumps).
#[tauri::command]
pub fn open_main_tab(app: AppHandle, tab: String) -> Result<(), String> {
    const ALLOWED: &[&str] = &[
        "ai-chat",
        "ai-models",
        "ai-prompt",
        "snippets",
        "cursor-accounts",
        "git",
        "runtime",
        "hosts",
        "cloudflared",
        "plugins",
        "devtools",
        "drafts",
        "account-quota",
        "app-launcher",
        "desktop-apps",
        "settings",
    ];
    if !ALLOWED.contains(&tab.as_str()) {
        return Err(format!("unknown tab: {tab}"));
    }
    bubble::show_main_window(&app);
    let _ = app.emit("navigate-tab", tab.as_str());
    Ok(())
}

#[tauri::command]
pub fn set_ai_hub_bubble_visible(
    app: AppHandle,
    visible: bool,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    bubble::set_ai_hub_bubble_visible_inner(&app, visible, x, y)
}

#[tauri::command]
pub fn set_ai_hub_bubble_position(app: AppHandle, x: i32, y: i32) -> Result<(), String> {
    // Reject / remap legacy top-right positions that cover the main titlebar.
    let (px, py) = if bubble::is_titlebar_danger_zone(&app, x, y) {
        bubble::default_bubble_position(&app)
    } else {
        bubble::clamp_bubble_position(&app, x, y)
    };
    bubble::ensure_ai_hub_bubble(&app, Some(px), Some(py))?;
    if let Some(win) = app.get_webview_window(bubble::AI_HUB_BUBBLE_LABEL) {
        let _ = win.set_position(tauri::PhysicalPosition::new(px, py));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::tray_menu_fingerprint;

    #[test]
    fn tray_menu_fingerprint_stable_for_same_content() {
        let a = tray_menu_fingerprint(&[3080], &[("t1".into(), Some("https://x".into()))]);
        let b = tray_menu_fingerprint(&[3080], &[("t1".into(), Some("https://x".into()))]);
        assert_eq!(a, b);
        assert_ne!(a, tray_menu_fingerprint(&[], &[]));
        assert_ne!(
            a,
            tray_menu_fingerprint(&[3080], &[("t1".into(), None)])
        );
    }
}
