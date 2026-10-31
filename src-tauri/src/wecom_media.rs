//! 企微富媒体：入站落盘 + 出站回发。
//!
//! 出站：图片走 inline base64（`aibot_respond_msg` 的 image.base64）；
//! 文本文件直接贴回，二进制经 media/upload 拿 media_id（需业务 API 凭证）。
//! 入站：回调 url 优先下载；仅 media_id 时走 qyapi media/get（同样需要业务 API）。
//! 素材落盘 `%APPDATA%\com.ai-workbench.app\wecom-media\<msgid>\`，mtime 超过 7 天回收。

use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Manager};

use crate::wecom_channel::{OutTarget, WecomState};
use crate::wecom_proto::{self, MediaRef};

const QYAPI: &str = "https://qyapi.weixin.qq.com/cgi-bin";
const MEDIA_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_INBOUND_BYTES: usize = 12 * 1024 * 1024;

/// 入站媒体落盘后的本地引用。
#[derive(Debug, Clone)]
pub struct LocalMedia {
    pub kind: String,
    pub path: PathBuf,
    pub filename: String,
    pub media_type: String,
}

/// 只查当前请求，缺失时不回退到其它会话的目标。
fn lookup_target(
    targets: &HashMap<String, OutTarget>,
    request_id: &str,
) -> Result<(String, String), String> {
    targets
        .get(request_id)
        .map(|t| (t.req_id.clone(), t.stream_id.clone()))
        .ok_or_else(|| "当前请求没有进行中的企微对话，发送目标已失效".into())
}

/// 当前回合的企微出站目标（由 orchestrator 按 agent request_id 登记）。
async fn target(app: &AppHandle, request_id: &str) -> Result<(String, String), String> {
    let st = app.state::<WecomState>();
    let targets = st.out_targets.lock().await;
    lookup_target(&targets, request_id)
}

/// 把本地图片作为 inline base64 发回当前企微对话。
pub async fn send_image(
    app: &AppHandle,
    path: &str,
    request_id: &str,
) -> Result<String, String> {
    let (req_id, stream_id) = target(app, request_id).await?;
    let name = file_name(path);
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("读取图片失败：{e}"))?;
    let media_type = mime_of(path);
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let sock = app.state::<WecomState>().sock.clone();
    sock.send_raw(wecom_proto::respond_image(
        &req_id, &stream_id, &media_type, &data,
    ))
    .await
    .map_err(|e| format!("发送图片失败：{e}"))?;
    Ok(format!("已发送图片：{name}"))
}

/// 把本地文件发回当前企微对话：文本直接贴出，二进制尝试上传，失败兜底提示。
pub async fn send_file(
    app: &AppHandle,
    path: &str,
    request_id: &str,
) -> Result<String, String> {
    let (req_id, stream_id) = target(app, request_id).await?;
    let name = file_name(path);
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("读取文件失败：{e}"))?;
    let sock = app.state::<WecomState>().sock.clone();

    if let Ok(text) = std::str::from_utf8(&bytes) {
        let preview: String = if text.chars().count() > 4000 {
            format!("{}\n…(已截断)", text.chars().take(4000).collect::<String>())
        } else {
            text.to_string()
        };
        sock.send_raw(wecom_proto::respond_stream(
            &req_id,
            &stream_id,
            &format!("📄 {name}\n```\n{preview}\n```"),
            true,
        ))
        .await
        .map_err(|e| format!("发送文件内容失败：{e}"))?;
        return Ok(format!("已发送文件内容：{name}"));
    }

    match upload_media(app, &bytes, &mime_of(path)).await {
        Ok(media_id) => {
            sock.send_raw(wecom_proto::respond_file(
                &req_id,
                &stream_id,
                &media_id,
                &name,
            ))
            .await
            .map_err(|e| format!("发送文件失败：{e}"))?;
            Ok(format!("已发送文件：{name}"))
        }
        Err(e) => {
            sock.send_raw(wecom_proto::respond_stream(
                &req_id,
                &stream_id,
                &format!(
                    "📎 已生成文件「{name}」（二进制，无法经企微直接发送：{e}；请在桌面端查看）"
                ),
                true,
            ))
            .await
            .map_err(|e| format!("发送提示失败：{e}"))?;
            Ok(format!("文件已生成（文本兜底）：{name}"))
        }
    }
}

/// 媒体上传（best-effort）：用**自建应用凭证**换 token 后 POST 到 media/upload。
/// 复用 `wecom_biz::access_token`（自建应用 Corpid/Secret 优先、带缓存与 40013 可操作提示）；
/// 机器人 Bot ID/Secret 当 corpid 用会被企微拒（40013），所以绝不能直接拿它换 token。
/// 端点/字段以线上协议为准；失败由调用方兜底。
async fn upload_media(app: &AppHandle, bytes: &[u8], media_type: &str) -> Result<String, String> {
    let st = app.state::<WecomState>();
    let creds = st.applied.lock().await.clone().ok_or("企微未连接")?;
    let client = reqwest::Client::new();

    let token = crate::wecom_biz::access_token(&st, &creds).await?;
    let up = format!("{QYAPI}/media/upload?access_token={token}&type=file");
    let boundary = "----wecomrustbnd";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"media\"; filename=\"f\"\r\n",
    );
    body.extend_from_slice(format!("Content-Type: {media_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let r: Value = client
        .post(&up)
        .timeout(std::time::Duration::from_secs(20))
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .map_err(|e| format!("上传失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("上传响应解析失败：{e}"))?;
    let code = r.get("errcode").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        return Err(format!(
            "媒体上传失败 errcode={} {}",
            code,
            r.get("errmsg").and_then(|m| m.as_str()).unwrap_or("")
        ));
    }
    r.get("media_id")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "media_id 缺失".into())
}

/// 把入站 MediaRef 拉到本地；url 优先，其次 media_id（需业务 API）。失败项跳过并记日志。
pub async fn materialize_inbound(
    app: &AppHandle,
    msgid: &str,
    media: &[MediaRef],
) -> Vec<LocalMedia> {
    let _ = cleanup_old_media();
    let mut out = Vec::new();
    for (i, m) in media.iter().enumerate() {
        match fetch_inbound_bytes(app, m).await {
            Ok((bytes, media_type)) => {
                let name = m
                    .filename
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| default_name(&m.kind, &media_type, i));
                match write_inbound(msgid, &name, &bytes) {
                    Ok(path) => out.push(LocalMedia {
                        kind: m.kind.clone(),
                        path,
                        filename: name,
                        media_type,
                    }),
                    Err(e) => crate::wecom_log::write(
                        "media-in",
                        &format!("msgid={msgid} write failed: {e}"),
                    ),
                }
            }
            Err(e) => crate::wecom_log::write(
                "media-in",
                &format!("msgid={msgid} fetch failed: {e}"),
            ),
        }
    }
    out
}

async fn fetch_inbound_bytes(
    app: &AppHandle,
    m: &MediaRef,
) -> Result<(Vec<u8>, String), String> {
    if !m.url.trim().is_empty() {
        return download_url(m.url.trim()).await;
    }
    let media_id = m
        .media_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "媒体缺少 url 与 media_id".to_string())?;
    download_media_id(app, media_id).await
}

async fn download_url(url: &str) -> Result<(Vec<u8>, String), String> {
    let resp = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("下载失败：{e}"))?;
    let media_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .trim()
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取失败：{e}"))?;
    if bytes.len() > MAX_INBOUND_BYTES {
        return Err(format!("媒体超过 {}MB 上限", MAX_INBOUND_BYTES / (1024 * 1024)));
    }
    Ok((bytes.to_vec(), media_type))
}

/// 经自建应用凭证拉取临时素材；机器人 Bot Secret 不能换 token。
async fn download_media_id(app: &AppHandle, media_id: &str) -> Result<(Vec<u8>, String), String> {
    let st = app.state::<WecomState>();
    let creds = st.applied.lock().await.clone().ok_or("企微未连接")?;
    let token = crate::wecom_biz::access_token(&st, &creds).await?;
    let url = format!("{QYAPI}/media/get?access_token={token}&media_id={media_id}");
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("media/get 失败：{e}"))?;
    let media_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    // 企微错误时常常返回 JSON application/json。
    if media_type.contains("application/json") || media_type.contains("text/plain") {
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("media/get 错误体解析失败：{e}"))?;
        let code = v.get("errcode").and_then(|c| c.as_i64()).unwrap_or(-1);
        return Err(format!(
            "media/get errcode={} {}（需在设置里配置业务 API Corpid/应用 Secret）",
            code,
            v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("")
        ));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("media/get 读取失败：{e}"))?;
    if bytes.len() > MAX_INBOUND_BYTES {
        return Err(format!("媒体超过 {}MB 上限", MAX_INBOUND_BYTES / (1024 * 1024)));
    }
    let mt = media_type
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .trim()
        .to_string();
    Ok((bytes.to_vec(), mt))
}

fn media_root() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let dir = base.join("com.ai-workbench.app").join("wecom-media");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn write_inbound(msgid: &str, filename: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let root = media_root().ok_or_else(|| "无法创建 wecom-media 目录".to_string())?;
    let safe_msg: String = msgid
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let dir = root.join(&safe_msg);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败：{e}"))?;
    let safe_name = Path::new(filename)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file.bin");
    let path = dir.join(safe_name);
    std::fs::write(&path, bytes).map_err(|e| format!("写入失败：{e}"))?;
    Ok(path)
}

fn default_name(kind: &str, media_type: &str, index: usize) -> String {
    let ext = if media_type.contains("png") {
        "png"
    } else if media_type.contains("jpeg") || media_type.contains("jpg") {
        "jpg"
    } else if media_type.contains("gif") {
        "gif"
    } else if media_type.contains("webp") {
        "webp"
    } else if media_type.contains("pdf") {
        "pdf"
    } else if kind == "image" {
        "jpg"
    } else {
        "bin"
    };
    format!("attach-{index}.{ext}")
}

/// 删除超过 MEDIA_TTL 的入站目录（best-effort）。
pub fn cleanup_old_media() -> usize {
    let Some(root) = media_root() else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|d| d > MEDIA_TTL);
        if old {
            let ok = if path.is_dir() {
                std::fs::remove_dir_all(&path).is_ok()
            } else {
                std::fs::remove_file(&path).is_ok()
            };
            if ok {
                removed += 1;
            }
        }
    }
    removed
}

fn mime_of(p: &str) -> String {
    let lower = p.to_ascii_lowercase();
    let m = if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else if lower.ends_with(".csv") {
        "text/csv"
    } else if lower.ends_with(".txt") {
        "text/plain"
    } else if lower.ends_with(".md") {
        "text/markdown"
    } else {
        "application/octet-stream"
    };
    m.to_string()
}

fn file_name(p: &str) -> String {
    std::path::Path::new(p)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets() -> HashMap<String, OutTarget> {
        HashMap::from([
            (
                "agent-a".into(),
                OutTarget {
                    req_id: "callback-a".into(),
                    stream_id: "stream-a".into(),
                },
            ),
            (
                "agent-b".into(),
                OutTarget {
                    req_id: "callback-b".into(),
                    stream_id: "stream-b".into(),
                },
            ),
        ])
    }

    #[test]
    fn media_routes_each_request_to_its_own_target() {
        let targets = targets();
        assert_eq!(
            lookup_target(&targets, "agent-a").unwrap(),
            ("callback-a".into(), "stream-a".into())
        );
        assert_eq!(
            lookup_target(&targets, "agent-b").unwrap(),
            ("callback-b".into(), "stream-b".into())
        );
    }

    #[test]
    fn cleaning_up_a_preserves_b_and_never_redirects_a() {
        let mut targets = targets();
        targets.remove("agent-a");
        assert!(lookup_target(&targets, "agent-a").is_err());
        assert_eq!(
            lookup_target(&targets, "agent-b").unwrap(),
            ("callback-b".into(), "stream-b".into())
        );
    }

    #[test]
    fn unknown_or_callback_id_does_not_fall_back_to_active_target() {
        let targets = targets();
        for request_id in ["", "unknown", "callback-a", "stream-b"] {
            assert!(lookup_target(&targets, request_id).is_err());
        }
        assert!(lookup_target(&HashMap::new(), "agent-a").is_err());
    }

    #[test]
    fn media_display_uses_only_file_name() {
        assert_eq!(file_name("private/workspace/report.csv"), "report.csv");
        assert_eq!(file_name("/private/workspace/chart.png"), "chart.png");
        assert_eq!(file_name(""), "file");
        #[cfg(windows)]
        assert_eq!(file_name(r"C:\private\workspace\report.csv"), "report.csv");
    }
}
