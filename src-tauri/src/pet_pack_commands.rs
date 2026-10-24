//! 桌宠自备素材包：用户在设置页挑图，前端规范化（抠背景 / 裁紧 / 同基线归一化）后写进
//! 应用数据目录，桌宠窗再读回来。**帧不入库**——随包分发第三方美术要先看清它的许可条款，
//! 市面上多数"好看的开源桌宠包"写着禁止商用或需要署名，不能直接塞进安装包。
//!
//! 目录：`%APPDATA%\com.ai-workbench.app\pets\custom\<情绪>[-序号].png|webm`
//! 文件名前缀即情绪名（见前端 lib/petSkins.ts 的 PET_MOODS + drag/tap），缺的情绪退回 idle。

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::Manager;

const PACK_SUBDIR: &str = "pets/custom";

/// 允许的文件名前缀：七个情绪 + 两个交互片段。
const PACK_NAMES: [&str; 9] = [
    "idle",
    "thinking",
    "working",
    "waiting",
    "happy",
    "error",
    "sleepy",
    "drag",
    "tap",
];

/// 写入口只收规范化产物；读目录多收两种格式，方便用户手动往目录里放素材。
const WRITE_EXTS: [&str; 2] = ["png", "webm"];
const READ_EXTS: [&str; 4] = ["png", "webp", "gif", "webm"];
const PICK_EXTS: [&str; 7] = ["png", "webp", "gif", "jpg", "jpeg", "bmp", "webm"];

/// 目录里的帧数与体积上限：素材是本地文件，护栏是给误操作准备的，不是给攻击者。
const MAX_FILES: usize = 64;
const MAX_WRITE_BYTES: usize = 2 << 20;
const MAX_READ_BYTES: usize = 8 << 20;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PetPackFile {
    /// 文件名（含扩展名）；前端按它认情绪、显示进度。
    pub name: String,
    pub data_url: String,
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "bmp" => "image/bmp",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

fn data_url(ext: &str, bytes: &[u8]) -> String {
    format!(
        "data:{};base64,{}",
        mime_of(ext),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn pack_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("拿不到应用数据目录: {e}"))?
        .join(PACK_SUBDIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建素材目录失败: {e}"))?;
    Ok(dir)
}

fn lower_ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// 只认 `<白名单前缀>[-序号].<png|webm>`，其余一律拒绝。
/// 返回的是按校验结果重新拼出来的文件名，不直接用调用方给的字符串。
fn pack_file_name(raw: &str) -> Result<String, String> {
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "文件名是空的".to_string())?;
    let (stem, ext) = name
        .split_once('.')
        .map(|(s, e)| (s.to_string(), e.to_ascii_lowercase()))
        .ok_or_else(|| "素材必须带扩展名".to_string())?;
    if !WRITE_EXTS.contains(&ext.as_str()) {
        return Err(format!("素材格式不支持: .{ext}"));
    }
    let base = stem.split(['-', '_']).next().unwrap_or("");
    if !PACK_NAMES.contains(&base) {
        return Err(format!("认不出的情绪名: {stem}"));
    }
    let tail = &stem[base.len()..];
    if !tail.is_empty() {
        let digits = tail.trim_start_matches(['-', '_']);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("序号只能是数字: {stem}"));
        }
    }
    if stem.len() > 24 {
        return Err("文件名太长了".to_string());
    }
    Ok(format!("{stem}.{ext}"))
}

fn read_frame(path: &Path, limit: usize) -> Result<Option<PetPackFile>, String> {
    let ext = lower_ext(path);
    if !READ_EXTS.contains(&ext.as_str()) {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("读取失败: {e}"))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > limit {
        return Err(format!(
            "{} 超过 {}MB 上限",
            path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
            limit / (1 << 20)
        ));
    }
    Ok(Some(PetPackFile {
        name: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string(),
        data_url: data_url(&ext, &bytes),
    }))
}

/// 弹系统文件框多选素材。rfd 是阻塞调用，必须丢进 spawn_blocking，别占着命令线程。
#[tauri::command]
pub async fn pick_pet_images() -> Result<Vec<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("选择桌宠素材图片")
            .add_filter("图片 / 透明视频", &PICK_EXTS)
            .pick_files()
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?
    .unwrap_or_default();
    Ok(picked
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// 把选中的图片读成 data URL，交给前端画布做抠像与归一化。
#[tauri::command]
pub async fn read_pet_images(paths: Vec<String>) -> Result<Vec<PetPackFile>, String> {
    let mut out = Vec::new();
    for path in paths {
        if out.len() >= MAX_FILES {
            break;
        }
        let p = Path::new(&path);
        let ext = lower_ext(p);
        if !PICK_EXTS.contains(&ext.as_str()) {
            return Err(format!("不支持的文件类型: {path}"));
        }
        let bytes = std::fs::read(p).map_err(|e| format!("读取失败 {path}: {e}"))?;
        if bytes.is_empty() || bytes.len() > MAX_READ_BYTES {
            return Err(format!("{path} 是空的或超过 8MB"));
        }
        out.push(PetPackFile {
            name: p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string(),
            data_url: data_url(&ext, &bytes),
        });
    }
    Ok(out)
}

/// 规范化产物落盘：先清掉目录里的旧素材，再逐个写。整批要么都写要么报错。
#[tauri::command]
pub async fn save_pet_pack(
    app: tauri::AppHandle,
    frames: Vec<PetPackFile>,
) -> Result<usize, String> {
    if frames.is_empty() {
        return Err("没有要保存的素材".to_string());
    }
    if frames.len() > MAX_FILES {
        return Err(format!("一次最多 {MAX_FILES} 帧"));
    }
    let dir = pack_dir(&app)?;
    let mut staged: Vec<(String, Vec<u8>)> = Vec::with_capacity(frames.len());
    for frame in &frames {
        let name = pack_file_name(&frame.name)?;
        if staged.iter().any(|(n, _)| *n == name) {
            return Err(format!("文件名撞了: {name}"));
        }
        let raw = frame
            .data_url
            .split_once(",")
            .map(|(_, b)| b)
            .ok_or_else(|| "data URL 少了逗号".to_string())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw)
            .map_err(|e| format!("{name} 解码失败: {e}"))?;
        if bytes.is_empty() || bytes.len() > MAX_WRITE_BYTES {
            return Err(format!("{name} 是空的或超过 2MB"));
        }
        staged.push((name, bytes));
    }
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("读目录失败: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
    }
    for (name, bytes) in &staged {
        std::fs::write(dir.join(name), bytes).map_err(|e| format!("写 {name} 失败: {e}"))?;
    }
    Ok(staged.len())
}

/// 桌宠窗与设置页都靠它拿素材；目录空就是空数组，前端据此退回悬浮球。
#[tauri::command]
pub async fn load_pet_pack(app: tauri::AppHandle) -> Result<Vec<PetPackFile>, String> {
    let dir = pack_dir(&app)?;
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("读目录失败: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() {
            entries.push(path);
        }
    }
    entries.sort();
    let mut out = Vec::new();
    for path in entries.into_iter().take(MAX_FILES) {
        if let Some(file) = read_frame(&path, MAX_READ_BYTES)? {
            out.push(file);
        }
    }
    Ok(out)
}

/// 只清这个目录里的文件，不动目录本身，也不碰应用数据目录的别处。
#[tauri::command]
pub async fn clear_pet_pack(app: tauri::AppHandle) -> Result<usize, String> {
    let dir = pack_dir(&app)?;
    let mut removed = 0usize;
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("读目录失败: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::pack_file_name;

    #[test]
    fn accepts_moods_and_numbered_frames() {
        for name in [
            "idle.png",
            "thinking.webm",
            "idle-2.png",
            "waiting_10.png",
            "sleepy-123.png",
        ] {
            assert_eq!(pack_file_name(name).unwrap(), name, "{name} 该被认下");
        }
    }

    #[test]
    fn rejects_anything_that_is_not_a_known_mood_frame() {
        // 认不出的情绪名 / 没有扩展名 / 别的格式 / 路径穿越 / 超长，一律拒绝
        for name in [
            "dancing.png",
            "idle",
            "idle.exe",
            "idle.PNG.exe",
            "../../../evil.png",
            "idle-1a.png",
            "idle--.png",
            "idle-123456789012345678901234.png",
        ] {
            assert!(pack_file_name(name).is_err(), "{name} 不该被认下");
        }
    }

    #[test]
    fn keeps_only_the_validated_file_name_not_the_given_path() {
        assert_eq!(pack_file_name(r"C:\x\evil\idle.png").unwrap(), "idle.png");
        assert_eq!(pack_file_name("a/b/happy.webm").unwrap(), "happy.webm");
    }
}
