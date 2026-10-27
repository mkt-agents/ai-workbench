#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// HKCU Run key — must include hive prefix for `reg.exe`.
const AUTOSTART_REG_PATH: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_VALUE: &str = "AIWorkbench";

const EXPORT_TABLE_NAMES: &[&str] = &[
    "git_accounts",
    "git_repo_configs",
    "host_profiles",
    "web_plugins",
    "user_scripts",
    "plugin_states",
    "recent_projects",
    "git_workspaces",
    "cursor_accounts",
    "ai_models",
    "cloudflared_profiles",
    "snippets",
];

#[cfg(windows)]
fn reg_command() -> std::process::Command {
    let mut cmd = std::process::Command::new("reg");
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Enable or disable auto-start at login by writing to the Windows registry
/// (HKCU\...\Run). No admin rights required since it's the current user's key.
#[tauri::command]
pub async fn set_auto_start(enabled: bool) -> Result<(), String> {
    tokio::task::spawn_blocking(move || set_auto_start_sync(enabled))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn set_auto_start_sync(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("无法解析程序路径: {}", e))?
            .to_string_lossy()
            .to_string();
        // Quote path so spaces work in REG_SZ data.
        let quoted = format!("\"{}\"", exe_path);

        let output = if enabled {
            reg_command()
                .args([
                    "ADD",
                    AUTOSTART_REG_PATH,
                    "/v",
                    AUTOSTART_VALUE,
                    "/t",
                    "REG_SZ",
                    "/d",
                    &quoted,
                    "/f",
                ])
                .output()
                .map_err(|e| format!("写入注册表失败: {}", e))?
        } else {
            reg_command()
                .args(["DELETE", AUTOSTART_REG_PATH, "/v", AUTOSTART_VALUE, "/f"])
                .output()
                .map_err(|e| format!("删除注册表项失败: {}", e))?
        };

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{} {}", stderr, stdout).to_ascii_lowercase();

        // Disabling when the value is already absent is success.
        if !enabled
            && (combined.contains("unable to find")
                || combined.contains("找不到")
                || combined.contains("error:  the system was unable to find"))
        {
            return Ok(());
        }

        let detail = stderr.trim();
        if detail.is_empty() {
            Err("注册表操作失败".into())
        } else {
            Err(format!("注册表操作失败: {}", detail))
        }
    }

    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err("当前系统不支持开机自启".into())
    }
}

/// Check whether auto-start is currently enabled for this app.
#[tauri::command]
pub async fn get_auto_start() -> Result<bool, String> {
    tokio::task::spawn_blocking(get_auto_start_sync)
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

/// 系统双击间隔（毫秒）。桌宠的「双击打开天玑」跟用户自己的系统设置走：
/// 写死 400ms 在慢手 / 触控板上会像"点了没反应"，在快手用户那儿又容易把两次
/// 独立点击误判成双击。`GetDoubleClickTime` 是普通用户态查询，返回值本身就是毫秒。
#[tauri::command]
pub fn system_double_click_ms() -> u32 {
    #[cfg(windows)]
    {
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }
    }
    #[cfg(not(windows))]
    {
        500
    }
}

fn get_auto_start_sync() -> Result<bool, String> {
    #[cfg(windows)]
    {
        let output = reg_command()
            .args(["QUERY", AUTOSTART_REG_PATH, "/v", AUTOSTART_VALUE])
            .output()
            .map_err(|e| format!("查询注册表失败: {}", e))?;
        Ok(output.status.success())
    }

    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

/// Copy text to system clipboard. Blocking; call from `spawn_blocking`.
pub fn copy_to_clipboard_blocking(text: String) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("打开剪贴板失败: {e}"))?;
    clipboard
        .set_text(text)
        .map_err(|e| format!("写入剪贴板失败: {e}"))?;
    Ok(())
}

/// Copy text to system clipboard.
///
/// Uses the `arboard` crate for native clipboard access — no temp files, no
/// PowerShell, no plaintext secrets written to disk.
#[tauri::command]
pub async fn copy_to_clipboard(text: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || copy_to_clipboard_blocking(text))
        .await
        .map_err(|e| format!("剪贴板任务异常: {e}"))?
}

/// Read current clipboard text.
#[tauri::command]
pub async fn read_clipboard() -> Result<String, String> {
    tokio::task::spawn_blocking(|| {
        let mut clipboard = arboard::Clipboard::new()
            .map_err(|e| format!("打开剪贴板失败: {e}"))?;
        clipboard
            .get_text()
            .map(|s| s.trim_end_matches(['\r', '\n']).to_string())
            .map_err(|e| format!("读取剪贴板失败: {e}"))
    })
    .await
    .map_err(|e| format!("剪贴板任务异常: {e}"))?
}

/// Paths of the files the user copied in Explorer (empty when the clipboard holds
/// text or an image instead).
///
/// The webview cannot see them: pasting from Explorer only carries the `CF_HDROP`
/// clipboard format, and Chromium hands the page `File` objects without paths. So the
/// paths have to come from the OS side. `Clipboard.GetFileDropList()` is exactly that
/// read; going through PowerShell keeps this dependency-free (a direct `CF_HDROP` +
/// `DragQueryFileW` version is the obvious later optimisation, but it needs another
/// `windows` feature and it is not worth the added unsafe surface for one paste).
#[tauri::command]
pub async fn read_clipboard_files() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(read_clipboard_files_sync)
        .await
        .map_err(|e| format!("剪贴板任务异常: {e}"))?
}

#[cfg(windows)]
fn read_clipboard_files_sync() -> Result<Vec<String>, String> {
    // 一行一个路径。先钉 UTF-8：中文路径按控制台代码页解码会变乱码。
    const SCRIPT: &str = "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; \
        Add-Type -AssemblyName System.Windows.Forms; \
        [System.Windows.Forms.Clipboard]::GetFileDropList() | ForEach-Object { $_ }";
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", SCRIPT])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("读取剪贴板文件失败: {e}"))?;
    if !output.status.success() {
        // 剪贴板里没有文件时 GetFileDropList 会抛，按「没有文件」返回就好
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    // 附件上限本来就只有 3 个，别为一堆文件起一长串读盘
    out.truncate(8);
    Ok(out)
}

#[cfg(not(windows))]
fn read_clipboard_files_sync() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// 一张可以直接塞进对话的图片。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineImage {
    pub media_type: String,
    /// base64，不带 `data:` 前缀（协议层自己拼）。
    pub data: String,
}

/// 视觉附件的文件上限。base64 会再涨 ~33%，而 Anthropic 单图上限 5MB。
const IMAGE_FILE_CAP: u64 = 3 * 1024 * 1024;

/// Read an image file into a base64 payload for the vision models.
///
/// 只认模型服务都支持的四种格式：想发 bmp / tiff 得先转，否则各家会直接 400。
#[tauri::command]
pub async fn read_image_file(path: String) -> Result<InlineImage, String> {
    tokio::task::spawn_blocking(move || read_image_file_sync(&path))
        .await
        .map_err(|e| format!("读取图片任务异常: {e}"))?
}

fn read_image_file_sync(path: &str) -> Result<InlineImage, String> {
    use base64::Engine;
    let file = std::path::Path::new(path);
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let media_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return Err("只支持 png / jpg / webp / gif 图片".into()),
    };
    let meta = std::fs::metadata(file).map_err(|e| format!("读不到图片：{e}"))?;
    if !meta.is_file() {
        return Err("不是文件".into());
    }
    if meta.len() > IMAGE_FILE_CAP {
        return Err(format!(
            "图片超过 {}MB，先压缩再发",
            IMAGE_FILE_CAP / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(file).map_err(|e| format!("读取图片失败：{e}"))?;
    Ok(InlineImage {
        media_type: media_type.into(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn row_to_json(row: &rusqlite::Row, columns: &[String]) -> Result<serde_json::Value, String> {
    let mut map = serde_json::Map::new();
    for (i, name) in columns.iter().enumerate() {
        let value = match row.get_ref(i).map_err(|e| e.to_string())? {
            rusqlite::types::ValueRef::Integer(n) => {
                serde_json::Value::Number(serde_json::Number::from(n))
            }
            rusqlite::types::ValueRef::Real(f) => serde_json::Number::from_f64(f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            rusqlite::types::ValueRef::Text(s) => {
                serde_json::Value::String(String::from_utf8_lossy(s).to_string())
            }
            rusqlite::types::ValueRef::Blob(b) => {
                serde_json::Value::String(b.iter().map(|byte| format!("{:02x}", byte)).collect())
            }
            rusqlite::types::ValueRef::Null => serde_json::Value::Null,
        };
        map.insert(name.clone(), value);
    }
    Ok(serde_json::Value::Object(map))
}

/// Export all whitelisted tables to a JSON file (user picks path).
#[tauri::command]
pub async fn export_data(state: tauri::State<'_, crate::DbState>) -> Result<String, String> {
    // Dialog first, DB read second: never hold the connection mutex while the
    // save dialog is open (other commands would block on it).
    let path = tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("导出工作台数据")
            .set_file_name("ai-workbench-backup.json")
            .add_filter("JSON", &["json"])
            .save_file()
            .ok_or_else(|| "已取消导出".to_string())
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))??;

    let conn = std::sync::Arc::clone(&state.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|e| e.to_string())?;
        export_data_sync(&guard, path)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

fn export_data_sync(
    conn: &std::sync::MutexGuard<'_, rusqlite::Connection>,
    path: std::path::PathBuf,
) -> Result<String, String> {
    let mut tables = serde_json::Map::new();
    for name in EXPORT_TABLE_NAMES {
        // Whitelisted tables carry their own capped/ordered read query, so the
        // export cannot balloon with every stored 64KB run output.
        let sql = match crate::db_commands::table_from_key(name) {
            Some(table) => crate::db_commands::load_sql(table).to_string(),
            None => format!("SELECT * FROM {name}"),
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => {
                tables.insert((*name).to_string(), serde_json::Value::Array(vec![]));
                continue;
            }
        };
        let cols: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
        let mut rows_iter = stmt.query([]).map_err(|e| e.to_string())?;
        let mut arr = Vec::new();
        while let Some(row) = rows_iter.next().map_err(|e| e.to_string())? {
            arr.push(row_to_json(row, &cols)?);
        }
        tables.insert((*name).to_string(), serde_json::Value::Array(arr));
    }

    let payload = serde_json::json!({
        "version": 1,
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "warning": "Contains plaintext API keys / tokens. Do not share.",
        "tables": tables,
    });

    let text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("写入失败: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// Import JSON dump via db_save for each known table (destructive).
///
/// All-or-nothing: every table is parsed and validated BEFORE any write
/// happens, so a single malformed table cannot leave the database half-migrated.
#[tauri::command]
pub async fn import_data(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::DbState>,
) -> Result<String, String> {
    // Dialog + file read off the main thread; the write phase below is async.
    let (path, text) = tauri::async_runtime::spawn_blocking(|| {
        let path = rfd::FileDialog::new()
            .set_title("导入工作台数据")
            .add_filter("JSON", &["json"])
            .pick_file()
            .ok_or_else(|| "已取消导入".to_string())?;
        let text = std::fs::read_to_string(&path).map_err(|e| format!("读取失败: {e}"))?;
        Ok::<_, String>((path, text))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))??;
    let root: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("JSON 无效: {e}"))?;
    let tables = root
        .get("tables")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "缺少 tables 字段".to_string())?;

    // Phase 1: parse and validate every table in memory. If any table is
    // malformed, fail before writing anything.
    let mut parsed: Vec<(crate::db_commands::DbTable, Vec<serde_json::Value>)> = Vec::new();
    for (key, value) in tables {
        let Some(table) = crate::db_commands::table_from_key(key) else {
            continue;
        };
        let rows = value
            .as_array()
            .ok_or_else(|| format!("表 {key} 不是数组"))?
            .clone();
        for (i, row) in rows.iter().enumerate() {
            if !row.is_object() {
                return Err(format!("表 {key} 第 {i} 行不是对象"));
            }
        }
        parsed.push((table, rows));
    }

    // Phase 2: write. Each db_save is itself transactional (delete + insert),
    // and we've already ruled out malformed input.
    for (table, rows) in &parsed {
        crate::db_commands::db_save(state.clone(), *table, rows.clone()).await?;
    }

    let _ = app;
    Ok(path.to_string_lossy().to_string())
}

/// Save arbitrary text via native save dialog (browser `<a download>` does not work in Tauri WebView).
#[tauri::command]
pub async fn save_text_file(
    content: String,
    default_name: String,
    title: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || save_text_file_sync(content, default_name, title))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn save_text_file_sync(
    content: String,
    default_name: String,
    title: Option<String>,
) -> Result<String, String> {
    let path = rfd::FileDialog::new()
        .set_title(title.as_deref().unwrap_or("保存文件"))
        .set_file_name(&default_name)
        .add_filter("JSON", &["json"])
        .add_filter("所有文件", &["*"])
        .save_file()
        .ok_or_else(|| "已取消导出".to_string())?;

    std::fs::write(&path, content).map_err(|e| format!("写入失败: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// Read a text file via native open dialog.
#[tauri::command]
pub async fn pick_text_file(title: Option<String>) -> Result<String, String> {
    tokio::task::spawn_blocking(move || pick_text_file_sync(title))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn pick_text_file_sync(title: Option<String>) -> Result<String, String> {
    let path = rfd::FileDialog::new()
        .set_title(title.as_deref().unwrap_or("选择文件"))
        .add_filter("JSON", &["json"])
        .add_filter("所有文件", &["*"])
        .pick_file()
        .ok_or_else(|| "已取消导入".to_string())?;

    std::fs::read_to_string(&path).map_err(|e| format!("读取失败: {e}"))
}

/// Read a source file from disk (used by the test generator's file picker).
/// Capped so a huge file cannot blow up an AI prompt.
#[tauri::command]
pub async fn read_text_file(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || read_text_file_sync(path))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}
fn read_text_file_sync(path: String) -> Result<String, String> {
    const MAX_SOURCE_BYTES: u64 = 200 * 1024;
    let file = std::fs::File::open(&path).map_err(|e| format!("读取失败: {e}"))?;
    let mut reader = std::io::Read::take(file, MAX_SOURCE_BYTES);
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf)
        .map_err(|e| format!("读取失败: {e}"))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Open a native file picker for a source file and return its path. The test
/// generator needs the path (to derive a sibling test-file name) and the content
/// (to send to the model), so the two are kept as separate commands — pairing this
/// with `read_text_file` — rather than collapsing them into one.
#[tauri::command]
pub async fn pick_source_file(title: Option<String>) -> Result<String, String> {
    tokio::task::spawn_blocking(move || pick_source_file_sync(title))
        .await
        .map_err(|e| format!("Task failed: {}", e))?
}

fn pick_source_file_sync(title: Option<String>) -> Result<String, String> {
    let path = rfd::FileDialog::new()
        .set_title(title.as_deref().unwrap_or("选择源文件"))
        .add_filter(
            "Source",
            &["ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go", "java", "cs", "cpp", "c", "h"],
        )
        .add_filter("文档", &["docx", "pdf", "xlsx", "xls", "csv", "tsv", "md", "txt"])
        .add_filter("图片", &["png", "jpg", "jpeg", "webp", "gif"])
        .add_filter("所有文件", &["*"])
        .pick_file()
        .ok_or_else(|| "已取消选择".to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// 读文档类附件（docx / pdf / xlsx / xls / csv / tsv 等）为纯文本，供天玑轻附件注入上下文。
/// 复用 `agent_docs::read_document` 的解析与体积闸门；`cap` 对齐附件单文件字符预算。
#[tauri::command]
pub async fn read_document_file(path: String, cap: Option<usize>) -> Result<String, String> {
    let cap = cap.unwrap_or(8000).clamp(200, 200_000);
    tokio::task::spawn_blocking(move || {
        crate::agent_docs::read_document(std::path::Path::new(&path), cap)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

/// 图片 OCR（Tesseract，方案 B）：把图片里的文字识别成纯文本，供轻附件在纯文本模型下也能用。
/// 引擎解析顺序见 `agent_ocr`：环境变量 → 打包 resources/tesseract → PATH。`langs` 空用默认 chi_sim+eng。
#[tauri::command]
pub async fn ocr_image_file(
    app: tauri::AppHandle,
    path: String,
    langs: Option<String>,
) -> Result<String, String> {
    use tauri::Manager;
    let resource_dir = app.path().resource_dir().ok();
    tokio::task::spawn_blocking(move || {
        crate::agent_ocr::ocr_image(
            resource_dir.as_deref(),
            std::path::Path::new(&path),
            langs.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}
