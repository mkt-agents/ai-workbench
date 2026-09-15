use tauri::Manager;

fn is_safe_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Navigate existing "browser" webview window. Returns true if navigated, false if window missing.
#[tauri::command]
pub fn navigate_browser_window(app: tauri::AppHandle, url: String) -> Result<bool, String> {
    if !is_safe_url(&url) {
        return Err("仅允许 http/https 地址".to_string());
    }
    let parsed: url::Url = url
        .parse()
        .map_err(|e| format!("无效 URL: {}", e))?;
    match app.get_webview_window("browser") {
        Some(w) => {
            w.navigate(parsed).map_err(|e| format!("导航失败: {}", e))?;
            let _ = w.set_focus();
            Ok(true)
        }
        None => Ok(false),
    }
}
