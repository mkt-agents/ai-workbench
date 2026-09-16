mod git_commands;
mod hosts_commands;
mod plugin_commands;
mod db_commands;
pub mod cursor;
mod ai_commands;
mod dsh_commands;
mod runtime_commands;
mod cloudflared_commands;
pub mod tool_commands;
mod tray;
pub mod config;
pub mod cancellation;
mod cancellation_commands;

pub use config::*;

use std::sync::Mutex;
use std::fs;
use rusqlite::Connection;
use tauri::Manager;

pub struct DbState {
    pub conn: Mutex<Connection>,
}

#[derive(Clone, serde::Serialize)]
pub struct DshInstance {
    pub pid: u32,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_patch_warning: Option<String>,
}

pub struct DshState {
    pub instances: Mutex<Vec<DshInstance>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // Must be the FIRST plugin. A second instance would otherwise create a
    // second WebView2 environment on the same user-data-dir and deadlock its
    // UI thread (window shows "(Not Responding)" while the first instance
    // keeps running). single-instance forwards the launch to the running
    // app and exits the new process before any webview is created.
    //
    // Skipped in dev so the installed app and `tauri dev` can run side by side
    // for debugging — they share the same identifier, which would otherwise
    // make them mutually exclusive.
    #[cfg(all(desktop, not(debug_assertions)))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        use tauri::Manager;
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.show();
            let _ = win.unminimize();
            let _ = win.set_focus();
        }
    }));
    builder
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(DbState { conn: Mutex::new(Connection::open_in_memory().expect("failed to create in-memory placeholder")) })
        .manage(DshState { instances: Mutex::new(Vec::new()) })
        .manage(cloudflared_commands::CloudflaredState::default())
        .manage(tray::TrayTunnelUrls::default())
        .setup(|app| {
            let data_dir = app.path().app_data_dir().expect("failed to get app data dir");
            fs::create_dir_all(&data_dir).expect("failed to create app data dir");
            let db_path = data_dir.join("ai-workbench.db");

            let conn = Connection::open(&db_path).expect("failed to open sqlite db");

            conn.execute_batch(r#"
                CREATE TABLE IF NOT EXISTS git_accounts (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    email TEXT NOT NULL,
                    color TEXT NOT NULL,
                    note TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS git_repo_configs (
                    path TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    user_name TEXT NOT NULL,
                    email TEXT NOT NULL,
                    account_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS host_profiles (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    content TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS web_plugins (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    url TEXT NOT NULL,
                    local_path TEXT,
                    downloaded_at TEXT,
                    added_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS plugin_states (
                    plugin_id TEXT PRIMARY KEY,
                    enabled INTEGER NOT NULL,
                    config TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS recent_projects (
                    id INTEGER PRIMARY KEY,
                    path TEXT NOT NULL,
                    name TEXT,
                    last_opened_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS git_workspaces (
                    id INTEGER PRIMARY KEY,
                    path TEXT NOT NULL UNIQUE,
                    name TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS cursor_accounts (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    email TEXT NOT NULL,
                    color TEXT NOT NULL,
                    backup_path TEXT NOT NULL,
                    git_user_name TEXT,
                    git_email TEXT,
                    password TEXT,
                    notes TEXT,
                    is_logged_in INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS ai_models (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    api_key TEXT NOT NULL,
                    base_url TEXT NOT NULL,
                    model TEXT NOT NULL,
                    auth_type TEXT DEFAULT 'api',
                    temperature REAL DEFAULT 0.7,
                    max_tokens INTEGER DEFAULT 4096,
                    is_default INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS cloudflared_profiles (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    hostname TEXT NOT NULL,
                    local_url TEXT NOT NULL,
                    token TEXT,
                    auth_mode TEXT DEFAULT 'token',
                    config_path TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS snippets (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    content TEXT NOT NULL,
                    tags TEXT NOT NULL DEFAULT '',
                    params TEXT NOT NULL DEFAULT '',
                    use_count INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS web_plugin_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    plugin_id TEXT NOT NULL,
                    url TEXT NOT NULL,
                    name TEXT NOT NULL,
                    opened_at TEXT NOT NULL
                );
            "#).expect("failed to init schema");

            let _ = conn.execute_batch("ALTER TABLE git_accounts ADD COLUMN note TEXT;");
            let _ = conn.execute_batch("ALTER TABLE cursor_accounts ADD COLUMN notes TEXT;");
            let _ = conn.execute_batch("ALTER TABLE cursor_accounts ADD COLUMN profile_dir TEXT;");
            let _ = conn.execute_batch("ALTER TABLE cursor_accounts ADD COLUMN profile_initialized INTEGER DEFAULT 0;");
            let _ = conn.execute_batch("ALTER TABLE cursor_accounts ADD COLUMN password TEXT;");
            let _ = conn.execute_batch("ALTER TABLE ai_models ADD COLUMN auth_type TEXT DEFAULT 'api';");
            let _ = conn.execute_batch("ALTER TABLE cloudflared_profiles ADD COLUMN auth_mode TEXT DEFAULT 'token';");
            let _ = conn.execute_batch("ALTER TABLE cloudflared_profiles ADD COLUMN config_path TEXT;");
            let _ = conn.execute_batch("ALTER TABLE web_plugins ADD COLUMN \"group\" TEXT NOT NULL DEFAULT '';");
            let _ = conn.execute_batch("ALTER TABLE web_plugins ADD COLUMN tags TEXT NOT NULL DEFAULT '';");
            let _ = conn.execute_batch("ALTER TABLE web_plugins ADD COLUMN \"order\" INTEGER NOT NULL DEFAULT 0;");
            let _ = conn.execute_batch("ALTER TABLE web_plugins ADD COLUMN last_opened_at TEXT;");
            let _ = conn.execute_batch("ALTER TABLE web_plugins ADD COLUMN open_count INTEGER NOT NULL DEFAULT 0;");
            let _ = conn.execute_batch("ALTER TABLE web_plugins ADD COLUMN hotkey TEXT NOT NULL DEFAULT '';");
            let _ = conn.execute_batch("ALTER TABLE web_plugins ADD COLUMN is_preset INTEGER NOT NULL DEFAULT 0;");
            {
                let state = app.state::<DbState>();
                let mut guard = state.conn.lock().map_err(|e| e.to_string())?;
                *guard = conn;
            }

            // Cursor shared-workspace seed can copy multi-GB trees — never block
            // window creation / the UI thread on it.
            tauri::async_runtime::spawn(async move {
                let result = tokio::task::spawn_blocking(|| {
                    cursor::ensure_shared_workspace_migration()
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("[cursor] shared workspace migration failed: {e}"),
                    Err(e) => eprintln!("[cursor] shared workspace migration task failed: {e}"),
                }
            });

            #[cfg(desktop)]
            {
                app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new().build(),
                )?;
                if let Err(e) = tray::init_tray(app.handle()) {
                    eprintln!("[tray] init failed: {e}");
                }
                // Startup builds TWO webviews: the main window and the desktop bubble
                // (the orb has to be on screen from the beginning). The quick-ask window
                // is deliberately NOT built here — it is created on demand by the tray
                // item and the global shortcut (both go through toggle_quick_ask ->
                // ensure_quick_ask_window), which keeps a cold first launch after install
                // from initialising three WebView2 instances at once.
                //
                // NOTE: never create a webview outside setup() on a delay — neither from
                // a worker thread nor via run_on_main_thread. Creation needs the message
                // loop to pump, so doing it from a callback stalls the loop and the main
                // window freezes ("not responding").
                // Create+show bubble once; do not call ensure then visible separately
                // (ensure used to mis-detect !CONTENT_READY as stale and block the UI).
                let _ = tray::set_quick_ask_bubble_visible_inner(app.handle(), true, None, None);
                // Materialise the quick-ask window off the startup path: it cannot be
                // built from the toggle command (see tray::bubble::prebuild_quick_ask_window).
                tray::bubble::prebuild_quick_ask_window(app.handle());
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            git_commands::set_git_config,
            git_commands::set_repo_git_config,
            git_commands::get_repo_git_config,
            git_commands::get_git_config,
            git_commands::pick_directory,
            git_commands::git_repo_summary,
            git_commands::git_status,
            git_commands::git_stage,
            git_commands::git_unstage,
            git_commands::git_commit,
            git_commands::git_push,
            git_commands::git_pull,
            git_commands::git_diff,
            git_commands::git_discard,
            git_commands::git_undo_last_commit,
            git_commands::git_commit_context,
            git_commands::git_is_repo,
            git_commands::git_scan_repos,
            hosts_commands::read_system_hosts,
            hosts_commands::is_admin,
            hosts_commands::write_system_hosts,
            hosts_commands::list_host_backups,
            hosts_commands::restore_host_backup,
            hosts_commands::backup_hosts_now,
            cloudflared_commands::open_in_browser,
            plugin_commands::navigate_browser_window,
            plugin_commands::open_browser_window_with_toolbar,
            db_commands::db_load,
            db_commands::db_save,
            cursor::inspect_cursor_backup,
            cursor::get_cursor_login_status,
            cursor::is_cursor_running,
            cursor::init_account_profile,
            cursor::finish_account_profile,
            cursor::launch_cursor,
            cursor::get_cursor_profile_dir,
            cursor::switch_cursor_account,
            cursor::delete_cursor_backup,
            cursor::get_cursor_disk_usage,
            cursor::cleanup_cursor_full_backups,
            cursor::read_cursor_diagnostics,
            cursor::quit_cursor,
            cursor::list_cursor_backups,
            cursor::get_cursor_orphan_profiles,
            cursor::cleanup_cursor_orphan_profiles,
            cursor::dump_state_keys,
            cursor::dump_state_value,
            ai_commands::test_model_connection,
            ai_commands::list_provider_models,
            ai_commands::generate_text,
            ai_commands::generate_text_stream,
            dsh_commands::start_dsh,
            dsh_commands::stop_dsh,
            dsh_commands::list_dsh,
            dsh_commands::check_dsh_port,
            dsh_commands::check_dsh_http,
            dsh_commands::check_nodejs_installed,
            dsh_commands::get_dsh_version,
            dsh_commands::get_dsh_latest_version,
            dsh_commands::get_dsh_versions,
            dsh_commands::install_dsh,
            dsh_commands::update_dsh,
            dsh_commands::sync_model_to_dsh,
            dsh_commands::restore_dsh_auth,
            cancellation_commands::cancel_request,
            tool_commands::set_auto_start,
            tool_commands::get_auto_start,
            tool_commands::copy_to_clipboard,
            tool_commands::read_clipboard,
            tool_commands::export_data,
            tool_commands::import_data,
            tool_commands::save_text_file,
            tool_commands::pick_text_file,
            tray::open_quick_ask_with_text,
            tray::tray_toggle_quick_ask,
            tray::hide_quick_ask,
            tray::open_main_deepseek,
            tray::set_quick_ask_bubble_visible,
            tray::set_quick_ask_bubble_position,
            tray::bubble::quick_ask_bubble_ready,
            runtime_commands::list_runtime_versions,
            runtime_commands::get_active_runtime,
            runtime_commands::add_custom_runtime,
            runtime_commands::remove_custom_runtime,
            runtime_commands::switch_runtime,
            runtime_commands::plan_runtime_switch,
            runtime_commands::open_runtime_folder,
            runtime_commands::open_runtime_terminal,
            runtime_commands::list_installable_runtimes,
            runtime_commands::install_runtime,
            runtime_commands::uninstall_runtime,
            cloudflared_commands::cloudflared_status,
            cloudflared_commands::cloudflared_install,
            cloudflared_commands::cloudflared_open_download,
            cloudflared_commands::cloudflared_pick_binary,
            cloudflared_commands::cloudflared_set_binary_path,
            cloudflared_commands::cloudflared_clear_binary_path,
            cloudflared_commands::cloudflared_pick_config,
            cloudflared_commands::cloudflared_start_quick_tunnel,
            cloudflared_commands::cloudflared_start_named_tunnel,
            cloudflared_commands::cloudflared_stop_tunnel,
            cloudflared_commands::cloudflared_stop_all_tunnels,
            cloudflared_commands::cloudflared_tunnel_status,
            cloudflared_commands::cloudflared_setup_new_domain,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
