//! 图片 OCR（方案 B：外部 Tesseract）。
//!
//! 取舍：不引 FFI/链接库，和项目既有「spawn 外部工具」风格一致（如 cloudflared）。
//! 解析顺序：环境变量 `AI_WORKBENCH_TESSERACT` → 打包资源 `resources/tesseract/tesseract(.exe)`
//! → PATH 上的 `tesseract`。三者都没有就明确报「未安装 OCR 引擎」，绝不静默返回空串。
//!
//! 语言：默认 `chi_sim+eng`，可用 `langs` 覆盖；需要对应的 `*.traineddata` 在 tessdata 里。
//! Tesseract 把识别结果打到 stdout（`stdout` 作为输出文件名），这里读 stdout 即可。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 单张图片体积上限：和文档读取对齐（10MB），再大多半是不该直接 OCR 的扫描大图。
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// 默认识别语言：简体中文 + 英文。
const DEFAULT_LANGS: &str = "chi_sim+eng";
/// OCR 进程的墙钟上限（毫秒）：超时即判失败，不让一张坏图把回合吊死。
const OCR_TIMEOUT_MS: u64 = 60_000;

/// 受支持的图片扩展名（Tesseract 经 leptonica 能读这些）。
pub(crate) fn is_ocr_image(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tif" | "tiff" | "gif"
    )
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}

/// 解析可用的 tesseract 可执行文件。`resource_dir` 是应用打包资源目录（可为 None）。
pub(crate) fn resolve_tesseract(resource_dir: Option<&Path>) -> Option<PathBuf> {
    // 1) 显式覆盖（整机或打包机自备路径）
    if let Ok(p) = std::env::var("AI_WORKBENCH_TESSERACT") {
        let path = PathBuf::from(p.trim());
        if path.is_file() {
            return Some(path);
        }
    }
    // 2) 打包资源：resources/tesseract/tesseract(.exe)
    if let Some(dir) = resource_dir {
        let exe = dir.join("tesseract").join(bin_name());
        if exe.is_file() {
            return Some(exe);
        }
    }
    // 3) PATH 上的 tesseract
    which_tesseract()
}

#[cfg(windows)]
fn bin_name() -> &'static str {
    "tesseract.exe"
}
#[cfg(not(windows))]
fn bin_name() -> &'static str {
    "tesseract"
}

/// 在 PATH 里找 tesseract（跨平台：Windows 补 .exe 后缀）。
fn which_tesseract() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(bin_name());
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// 把一张图片 OCR 成文本。`resource_dir` 供解析打包的 tesseract；`langs` 为空用默认。
/// 返回的文本已 `trim`；识别不到任何字时返回明确提示，而不是空串。
pub(crate) fn ocr_image(
    resource_dir: Option<&Path>,
    path: &Path,
    langs: Option<&str>,
) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("读不到图片：{e}"))?;
    if !meta.is_file() {
        return Err("不是文件".into());
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(format!("图片超过 {}MB，先压缩再识别", MAX_IMAGE_BYTES / 1024 / 1024));
    }
    if !is_ocr_image(path) {
        return Err("不是可识别的图片格式（支持 png/jpg/webp/bmp/tiff/gif）".into());
    }
    let tess = resolve_tesseract(resource_dir).ok_or_else(|| {
        "没有找到 OCR 引擎（Tesseract）。随安装包分发时它在 resources/tesseract 下；\
开发机可装 tesseract 到 PATH，或用环境变量 AI_WORKBENCH_TESSERACT 指定可执行文件。"
            .to_string()
    })?;
    let langs = {
        let l = langs.unwrap_or("").trim();
        if l.is_empty() { DEFAULT_LANGS } else { l }
    };

    // tesseract <img> stdout -l <langs> ：结果打到标准输出。
    let mut cmd = Command::new(&tess);
    cmd.arg(path).arg("stdout").arg("-l").arg(langs);
    // 若打包目录带 tessdata，显式指过去，免得撞上系统里版本不符的语言包。
    if let Some(dir) = resource_dir {
        let tessdata = dir.join("tesseract").join("tessdata");
        if tessdata.is_dir() {
            cmd.env("TESSDATA_PREFIX", &tessdata);
        }
    }
    hide_console(&mut cmd);

    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动 OCR 引擎失败：{e}"))?;

    let out = wait_with_timeout(child, OCR_TIMEOUT_MS)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // 最常见：缺语言包。把 tesseract 自己的话透给用户。
        return Err(format!(
            "OCR 失败：{}",
            err.trim().chars().take(400).collect::<String>()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return Err("没有识别到文字（可能图里没有清晰文本，或缺少对应语言包）".into());
    }
    Ok(text)
}

/// 等待子进程并施加超时：超时就杀掉并报错，避免吊死回合。
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout_ms: u64,
) -> Result<std::process::Output, String> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("读取 OCR 输出失败：{e}"));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("OCR 超时".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("等待 OCR 进程失败：{e}")),
        }
    }
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn recognises_ocr_image_extensions() {
        assert!(is_ocr_image(Path::new("a.png")));
        assert!(is_ocr_image(Path::new("b.JPG")));
        assert!(is_ocr_image(Path::new("c.tiff")));
        assert!(!is_ocr_image(Path::new("d.pdf")));
        assert!(!is_ocr_image(Path::new("e.txt")));
        assert!(!is_ocr_image(Path::new("noext")));
    }

    #[test]
    fn missing_engine_is_explicit_not_silent() {
        // 用一个不存在的资源目录 + 清掉环境覆盖，若 PATH 也没有 tesseract 则必须明确报错。
        // 为了稳定，不依赖机器是否装了 tesseract：只断言「解析不到时 ocr_image 给出引擎缺失错误」。
        std::env::remove_var("AI_WORKBENCH_TESSERACT");
        if resolve_tesseract(None).is_none() {
            // 造一个临时 png 文件头（内容不重要，解析引擎前就会因找不到引擎返回错误）
            let dir = std::env::temp_dir().join("aiwb-ocr-test");
            let _ = std::fs::create_dir_all(&dir);
            let img = dir.join("x.png");
            let _ = std::fs::write(&img, [0x89, 0x50, 0x4E, 0x47]);
            let err = ocr_image(None, &img, None).unwrap_err();
            assert!(err.contains("OCR 引擎") || err.contains("Tesseract"), "应报引擎缺失：{err}");
            let _ = std::fs::remove_file(&img);
        }
    }
}
