//! System tray icon, menu, and status polling.
//!
//! The tray shows DSH/tunnel run state in its tooltip and menu, and hosts the
//! thin `#[tauri::command]` entry points that the frontend invokes. Window
//! management for the quick-ask bubble lives in the `bubble` submodule.

pub mod bubble;

// `ensure_quick_ask_window` stays internal to `bubble` — it is called on demand by the
// toggle/show paths, never from the app setup.
pub use bubble::set_quick_ask_bubble_visible_inner;

use std::collections::HashMap;
use std::sync::Mutex;
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
    let show_main = MenuItem::with_id(app, "show_main", "显示主窗口", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let quick_ask = MenuItem::with_id(app, "quick_ask", "打开快问", true, None::<&str>)
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
        vec![&show_main, &quick_ask, &sep1, &dsh_toggle, &sep2];
    for item in &tunnel_items {
        refs.push(item);
    }
    if !tunnel_items.is_empty() {
        refs.push(&sep3);
    }
    refs.push(&quit);

    let menu = Menu::with_items(app, &refs).map_err(|e| e.to_string())?;
    if let Some(tray) = app.tray_by_id("main_tray") {
        let _ = tray.set_menu(Some(menu));
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
    let quick_ask = MenuItem::with_id(app, "quick_ask", "打开快问", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let sep1 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let dsh_toggle = MenuItem::with_id(app, "dsh_toggle", "启动 DeepSeek", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let sep2 = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>).map_err(|e| e.to_string())?;
    let menu = Menu::with_items(
        app,
        &[&show_main, &quick_ask, &sep1, &dsh_toggle, &sep2, &quit],
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
                "quick_ask" => bubble::show_quick_ask(app),
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
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                bubble::show_main_window(tray.app_handle());
            }
        })
        .build(app)
        .map_err(|e| e.to_string())?;

    if let Some(main) = app.get_webview_window(bubble::MAIN_LABEL) {
        let app_handle = app.clone();
        main.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Some(win) = app_handle.get_webview_window(bubble::MAIN_LABEL) {
                    let _ = win.hide();
                }
                let _ = app_handle.emit("tray-minimized", ());
            }
        });
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
    if ports.is_empty() {
        let _ = crate::dsh_commands::start_dsh(None, window);
    } else {
        let _ = crate::dsh_commands::stop_dsh(ports[0], window);
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
pub fn open_quick_ask_with_text(
    app: AppHandle,
    text: String,
    task: Option<String>,
) -> Result<(), String> {
    bubble::show_quick_ask(&app);
    // Emit to both quick-ask windows with a unique event ID so each frontend
    // instance can deduplicate (they run in separate processes).
    let event_id = format!("{}-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis(), text.len());
    let mut payload = serde_json::json!({ "id": event_id, "text": text });
    if let Some(task) = task {
        payload["task"] = serde_json::Value::String(task);
    }
    if let Some(win) = app.get_webview_window(bubble::QUICK_ASK_LABEL) {
        let _ = win.emit("quick-ask-prefill", payload.clone());
    }
    if let Some(win) = app.get_webview_window(bubble::QUICK_ASK_BUBBLE_LABEL) {
        let _ = win.emit("quick-ask-prefill", payload);
    }
    Ok(())
}

#[tauri::command]
pub fn tray_toggle_quick_ask(app: AppHandle) -> Result<(), String> {
    bubble::toggle_quick_ask(&app);
    Ok(())
}

#[tauri::command]
pub fn hide_quick_ask(app: AppHandle) -> Result<(), String> {
    bubble::hide_quick_ask(&app);
    Ok(())
}

#[tauri::command]
pub fn open_main_deepseek(app: AppHandle) -> Result<(), String> {
    bubble::show_main_window(&app);
    let _ = app.emit("navigate-tab", "ai-chat");
    Ok(())
}

/// Show the main window and switch it to a whitelisted tab (quick-ask tools panel jumps).
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
        "test-manager",
        "devtools",
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
pub fn set_quick_ask_bubble_visible(
    app: AppHandle,
    visible: bool,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(), String> {
    bubble::set_quick_ask_bubble_visible_inner(&app, visible, x, y)
}

#[tauri::command]
pub fn set_quick_ask_bubble_position(app: AppHandle, x: i32, y: i32) -> Result<(), String> {
    // Reject / remap legacy top-right positions that cover the main titlebar.
    let (px, py) = if bubble::is_titlebar_danger_zone(&app, x, y) {
        bubble::default_bubble_position(&app)
    } else {
        bubble::clamp_bubble_position(&app, x, y)
    };
    bubble::ensure_quick_ask_bubble(&app, Some(px), Some(py))?;
    if let Some(win) = app.get_webview_window(bubble::QUICK_ASK_BUBBLE_LABEL) {
        let _ = win.set_position(tauri::PhysicalPosition::new(px, py));
    }
    Ok(())
}
