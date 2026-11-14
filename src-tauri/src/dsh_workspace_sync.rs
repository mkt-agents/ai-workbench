//! DSH 桌面端的「工作区实时同步」插件安装器。
//!
//! 背景：DSH 的工作区表只在宿主启动时读一次盘，之后全走内存；外部进程写
//! `~/.dsh/storages/workspace.json` 既看不到、又会被宿主下次保存覆盖。所以企微
//! 通道（独立的 `dsh --profile acp` 进程）新建的会话，只有宿主自己调
//! `Workspace.attachSession(id)` 才能立刻出现在侧边栏。
//!
//! 安装方式：把 `dsh_workspace_sync.mjs` 落到应用数据目录，再往
//! `~/.dsh/profiles/desktop/cordis.patch.yml` 追加一段**带标记**的 `insert` 行。
//! 实测（DSH 0.2.2）：`patchReload: live` **不会**装入新增的插件条目，宿主只在
//! 启动时读 profile，所以第一次开启后需要重启一次 Harness；之后插件常驻，新会话由
//! 插件自己 fs.watch 实时挂载。关闭开关时按标记整块删除，首次修改前备份原文件。
//!
//! 只做这一件事：不装依赖、不改 DSH 安装包、不碰其它 profile。

use std::path::{Path, PathBuf};

use serde_json::json;

/// 标记块起止行。删除/重装都靠它们定位，绝不整文件重写用户配置。
const BLOCK_START: &str = "# >>> ai-workbench workspace-sync >>>";
const BLOCK_END: &str = "# <<< ai-workbench workspace-sync <<<";
const PLUGIN_ID: &str = "ai-workbench-workspace-sync";
const PLUGIN_FILE: &str = "workspace-sync.mjs";
const PATCH_FILE: &str = "cordis.patch.yml";
const EMBEDDED_PATCH: &str = "embedded.patch.yml";
const BACKUP_SUFFIX: &str = ".ai-workbench.bak";
/// 兜底扫描周期（与插件默认值一致）：正常靠 fs.watch 触发，这个只防丢事件。
const SCAN_INTERVAL_MS: u64 = 30_000;

/// 开关状态：前端据此显示「已注入 / 未注入 / 未装 DSH 桌面端」。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSyncStatus {
    /// 桌面端 profile 的 patch 里是否有我们的标记块
    pub installed: bool,
    /// 宿主是否真的把插件跑起来了（插件 apply 时写的回执）
    pub active: bool,
    /// 回执时间，ISO 字符串
    pub applied_at: Option<String>,
    /// 插件文件路径（落盘在应用数据目录）
    pub plugin_path: Option<String>,
    /// 目标 patch 文件
    pub profile_patch: Option<String>,
    /// 插件扫描的会话根
    pub sessions_root: Option<String>,
    /// 不可用原因（没装桌面端 / 读写出错），正常为 None
    pub detail: Option<String>,
}

/// 插件与覆盖层所在的目录：`%APPDATA%\com.ai-workbench.app\dsh-workspace-sync`
fn app_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = base.join("com.ai-workbench.app").join("dsh-workspace-sync");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn plugin_path() -> Option<PathBuf> {
    app_dir().map(|dir| dir.join(PLUGIN_FILE))
}

fn sessions_root() -> Option<PathBuf> {
    crate::dsh_workspace_registry::dsh_home().map(|home| home.join("sessions"))
}

/// 用户 DSH 桌面端 profile 的 patch 文件。
fn desktop_patch() -> Option<PathBuf> {
    crate::dsh_workspace_registry::dsh_home().map(|home| {
        home.join("profiles")
            .join("desktop")
            .join(PATCH_FILE)
    })
}

/// 插件 apply 时写的回执：宿主只在启动时读 profile，所以「已注入但没有回执」
/// 就说明用户还没重启 Harness。
fn read_marker() -> Option<serde_json::Value> {
    let path = app_dir()?.join("applied.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn read_status() -> WorkspaceSyncStatus {
    let plugin = plugin_path();
    let patch = desktop_patch();
    let sessions = sessions_root();
    let marker = read_marker();
    let applied_at = marker
        .as_ref()
        .and_then(|value| value.get("at"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut detail = None;
    let installed = match patch.as_deref() {
        Some(path) if path.is_file() => match std::fs::read_to_string(path) {
            Ok(text) => text.contains(BLOCK_START),
            Err(error) => {
                detail = Some(format!("读取 DSH 配置失败：{error}"));
                false
            }
        },
        Some(_) => {
            detail = Some("未检测到 DeepSeek Harness 桌面端配置（~/.dsh/profiles/desktop）".into());
            false
        }
        None => {
            detail = Some("找不到用户目录".into());
            false
        }
    };
    WorkspaceSyncStatus {
        installed,
        active: installed && marker.is_some(),
        applied_at,
        plugin_path: plugin.map(|path| path.to_string_lossy().to_string()),
        profile_patch: patch.map(|path| path.to_string_lossy().to_string()),
        sessions_root: sessions.map(|path| path.to_string_lossy().to_string()),
        detail,
    }
}

/// 生产入口：当前是否已注入（内嵌 Harness 启动也按它决定是否加 `--patch`）。
pub(crate) fn is_installed() -> bool {
    desktop_patch()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|text| text.contains(BLOCK_START))
}

/// 设置页读取状态。
pub fn status() -> WorkspaceSyncStatus {
    read_status()
}

/// 开关：`true` 注入、`false` 完整卸载。失败返回给前端展示，不静默。
pub fn set_enabled(enabled: bool) -> Result<WorkspaceSyncStatus, String> {
    let patch = desktop_patch().ok_or_else(|| "找不到用户目录".to_string())?;
    if enabled {
        let plugin = plugin_path().ok_or_else(|| "找不到应用数据目录".to_string())?;
        let sessions = sessions_root().ok_or_else(|| "找不到用户目录".to_string())?;
        // 先清掉上一轮的回执：宿主真正加载插件时会重写；不清的话，插件实际
        // 加载失败时残留的旧回执会让「已激活」误报。
        if let Some(marker) = app_dir().map(|dir| dir.join("applied.json")) {
            let _ = std::fs::remove_file(marker);
        }
        std::fs::write(&plugin, include_str!("dsh_workspace_sync.mjs"))
            .map_err(|e| format!("写入同步插件失败：{e}"))?;
        install_into(&patch, &plugin, &sessions)?;
    } else {
        uninstall_from(&patch)?;
        // 回执只在「宿主确实加载过」时才有意义；卸载后清掉，重新打开时状态才是准的。
        if let Some(marker) = app_dir().map(|dir| dir.join("applied.json")) {
            let _ = std::fs::remove_file(marker);
        }
    }
    Ok(read_status())
}

/// 内嵌 Harness（`dsh web`）的覆盖层：只在开关打开时生成，返回给启动参数用。
pub(crate) fn embedded_patch() -> Option<PathBuf> {
    if !is_installed() {
        return None;
    }
    let dir = app_dir()?;
    let plugin = plugin_path()?;
    // 应用数据目录被清过而 patch 块还在时，宿主会加载一个不存在的插件、
    // 直接起不来。这里顺手把插件文件补齐（编译期内嵌，幂等）。
    std::fs::write(&plugin, include_str!("dsh_workspace_sync.mjs")).ok()?;
    let path = dir.join(EMBEDDED_PATCH);
    let rows = row_yaml(&plugin, &sessions_root()?).ok()?;
    let body =
        format!("# ai-workbench 生成：内嵌 Harness 的工作区同步覆盖层，每次启动重写。\n{rows}");
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// 一条 `insert` 行（不含标记块注释）。
fn row_yaml(plugin: &Path, sessions_root: &Path) -> Result<String, String> {
    let rows = vec![json!({"insert": [{
        "id": PLUGIN_ID,
        "name": plugin.to_string_lossy(),
        "config": {
            "sessionsRoot": sessions_root.to_string_lossy(),
            "scanIntervalMs": SCAN_INTERVAL_MS,
        },
    }]})];
    serde_yaml::to_string(&rows)
        .map(|body| body.trim_start_matches("---\n").trim_end().to_string())
        .map_err(|e| e.to_string())
}

/// 生成带标记的追加块（含一条 `insert` 行）。路径交给 serde_yaml 转义。
fn block_yaml(plugin: &Path, sessions_root: &Path) -> Result<String, String> {
    Ok(format!(
        "{BLOCK_START}\n# 由 AI Workbench 注入：把新会话实时挂到对应工作区；关闭开关会整块删除。\n{}\n{BLOCK_END}\n",
        row_yaml(plugin, sessions_root)?
    ))
}

/// 幂等安装：先剥旧块再追加，重复调用不会叠加。
fn install_into(patch: &Path, plugin: &Path, sessions_root: &Path) -> Result<(), String> {
    let original = std::fs::read_to_string(patch)
        .map_err(|e| format!("读取 DSH 配置失败（{patch:?}）：{e}"))?;
    let (stripped, _) = strip_block(&original);
    let mut next = stripped;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&block_yaml(plugin, sessions_root)?);
    backup_once(patch, &original)?;
    write_atomic(patch, &next)
}

fn uninstall_from(patch: &Path) -> Result<bool, String> {
    let original = match std::fs::read_to_string(patch) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("读取 DSH 配置失败：{error}")),
    };
    let (stripped, found) = strip_block(&original);
    if !found {
        return Ok(false);
    }
    write_atomic(patch, &stripped)?;
    Ok(true)
}

/// 去掉标记块（含标记行与块后多余空行）。返回 (新文本, 是否找到过)。
fn strip_block(text: &str) -> (String, bool) {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    let mut found = false;
    for line in text.lines() {
        if !inside && line.trim() == BLOCK_START {
            inside = true;
            found = true;
            continue;
        }
        if inside {
            if line.trim() == BLOCK_END {
                inside = false;
            }
            continue;
        }
        out.push(line);
    }
    let mut result = out.join("\n");
    if text.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    (result, found)
}

/// 首次改动前留一份原样备份；已存在就不覆盖（保留最原始的内容）。
fn backup_once(patch: &Path, original: &str) -> Result<(), String> {
    let mut backup = patch.as_os_str().to_owned();
    backup.push(BACKUP_SUFFIX);
    let backup = PathBuf::from(backup);
    if backup.exists() {
        return Ok(());
    }
    std::fs::write(&backup, original).map_err(|e| format!("备份 DSH 配置失败：{e}"))
}

/// 临时文件 + rename：避免写到一半让 DSH 加载到半截 patch。
fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "配置路径没有父目录".to_string())?;
    let temp = parent.join(format!(
        ".{}.aw-tmp-{}",
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "patch".into()),
        std::process::id()
    ));
    std::fs::write(&temp, body).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("写入临时配置失败：{e}")
    })?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("替换配置失败：{e}")
    })
}

/// 设置页读取同步开关状态。
#[tauri::command]
pub async fn wecom_dsh_workspace_sync_status() -> Result<WorkspaceSyncStatus, String> {
    tokio::task::spawn_blocking(status)
        .await
        .map_err(|_| "WORKSPACE_SYNC_TASK_FAILED".to_string())
}

/// 设置页切换同步开关：打开即注入、关闭即整块卸载。
#[tauri::command]
pub async fn wecom_dsh_workspace_sync_set(enabled: bool) -> Result<WorkspaceSyncStatus, String> {
    tokio::task::spawn_blocking(move || set_enabled(enabled))
        .await
        .map_err(|_| "WORKSPACE_SYNC_TASK_FAILED".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aw-ws-sync-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn strip_block_removes_marked_section_only() {
        let text = "a: 1\n# >>> ai-workbench workspace-sync >>>\n- insert: []\n# <<< ai-workbench workspace-sync <<<\nb: 2\n";
        let (stripped, found) = strip_block(text);
        assert!(found);
        assert_eq!(stripped, "a: 1\nb: 2\n");
        let (again, found_again) = strip_block(&stripped);
        assert!(!found_again);
        assert_eq!(again, stripped);
    }

    #[test]
    fn block_is_valid_yaml_and_escapes_windows_paths() {
        let plugin = Path::new(r"D:\data\com.ai-workbench.app\dsh-workspace-sync\workspace-sync.mjs");
        let sessions = Path::new(r"C:\Users\me\.dsh\sessions");
        // 覆盖层里的正文（内嵌 Harness 用这份，不带标记）。
        let body = row_yaml(plugin, sessions).unwrap();
        let block = block_yaml(plugin, sessions).unwrap();
        assert!(block.starts_with(BLOCK_START) && block.trim_end().ends_with(BLOCK_END));
        let parsed: serde_yaml::Value = serde_yaml::from_str(&body).expect("insert 行应是合法 YAML");
        let entry = &parsed.as_sequence().unwrap()[0]["insert"][0];
        assert_eq!(entry["id"].as_str(), Some(PLUGIN_ID));
        assert_eq!(
            entry["name"].as_str(),
            Some(r"D:\data\com.ai-workbench.app\dsh-workspace-sync\workspace-sync.mjs")
        );
        assert_eq!(
            entry["config"]["sessionsRoot"].as_str(),
            Some(r"C:\Users\me\.dsh\sessions")
        );
    }

    #[test]
    fn install_then_uninstall_is_idempotent_and_keeps_backup() {
        let dir = temp_dir("install");
        let patch = dir.join(PATCH_FILE);
        let plugin = dir.join(PLUGIN_FILE);
        let sessions = dir.join("sessions");
        let original = "# 用户自己的 patch\n- id: ui-chat\n  config:\n    enabled: true\n";
        std::fs::write(&patch, original).unwrap();
        std::fs::write(&plugin, "// plugin").unwrap();

        install_into(&patch, &plugin, &sessions).unwrap();
        let installed = std::fs::read_to_string(&patch).unwrap();
        assert!(installed.contains(BLOCK_START));
        assert!(installed.starts_with("# 用户自己的 patch"));

        // 重复安装不会叠加块。
        install_into(&patch, &plugin, &sessions).unwrap();
        assert_eq!(std::fs::read_to_string(&patch).unwrap().matches(BLOCK_START).count(), 1);

        // 备份保留的是最初的原文。
        let backup = PathBuf::from(format!("{}{BACKUP_SUFFIX}", patch.display()));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

        uninstall_from(&patch).unwrap();
        assert_eq!(std::fs::read_to_string(&patch).unwrap(), original);
        assert!(!uninstall_from(&patch).unwrap(), "第二次卸载应视为无操作");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn install_reports_missing_profile() {
        let dir = temp_dir("missing");
        let patch = dir.join(PATCH_FILE);
        let plugin = dir.join(PLUGIN_FILE);
        let error = install_into(&patch, &plugin, &dir.join("sessions")).unwrap_err();
        assert!(error.contains("读取 DSH 配置失败"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plugin_source_exports_expected_shape() {
        let source = include_str!("dsh_workspace_sync.mjs");
        assert!(source.contains("export const name = 'ai-workbench-workspace-sync'"));
        assert!(source.contains("export const inject = ['workspaceRegistry']"));
        assert!(source.contains("export function apply(ctx, config)"));
        assert!(source.contains("attachSession("), "必须走宿主自己的 attachSession");
    }
}
