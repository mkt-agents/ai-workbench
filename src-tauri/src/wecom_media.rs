//! 企微出站富媒体：把本地生成的图片/文件经机器人通道发回当前对话。
//!
//! 图片走 inline base64（`aibot_respond_msg` 的 image.base64），不依赖媒体上传端点；
//! 文本文件直接以文本形式贴回，二进制文件尝试经媒体上传拿到 media_id 后发送，
//! 上传不可用时回退为提示。媒体上传端点/字段以线上协议为准，待联调验证。

use base64::Engine;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::wecom_channel::WecomState;
use crate::wecom_proto;

const QYAPI: &str = "https://qyapi.weixin.qq.com/cgi-bin";

/// 当前回合的企微出站目标（由 orchestrator 在处理消息时写入）。
async fn target(app: &AppHandle) -> Result<(String, String), String> {
    let st = app.state::<WecomState>();
    let g = st.last_target.lock().await;
    match g.as_ref() {
        Some(t) => Ok((t.req_id.clone(), t.stream_id.clone())),
        None => Err("当前没有进行中的企微对话，无法定位发送目标".into()),
    }
}

/// 把本地图片作为 inline base64 发回当前企微对话。
pub async fn send_image(app: &AppHandle, path: &str) -> Result<String, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("读取图片失败：{e}"))?;
    let media_type = mime_of(path);
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let (req_id, stream_id) = target(app).await?;
    let sock = app.state::<WecomState>().sock.clone();
    sock.send_raw(wecom_proto::respond_image(
        &req_id, &stream_id, &media_type, &data,
    ))
    .await
    .map_err(|e| format!("发送图片失败：{e}"))?;
    Ok(format!("已发送图片：{path}"))
}

/// 把本地文件发回当前企微对话：文本直接贴出，二进制尝试上传，失败兜底提示。
pub async fn send_file(app: &AppHandle, path: &str) -> Result<String, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("读取文件失败：{e}"))?;
    let (req_id, stream_id) = target(app).await?;
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
            &format!("📄 {path}\n```\n{preview}\n```"),
            true,
        ))
        .await
        .map_err(|e| format!("发送文件内容失败：{e}"))?;
        return Ok(format!("已发送文件内容：{path}"));
    }

    match upload_media(app, &bytes, &mime_of(path)).await {
        Ok(media_id) => {
            sock.send_raw(wecom_proto::respond_file(
                &req_id,
                &stream_id,
                &media_id,
                &file_name(path),
            ))
            .await
            .map_err(|e| format!("发送文件失败：{e}"))?;
            Ok(format!("已发送文件：{path}"))
        }
        Err(e) => {
            sock.send_raw(wecom_proto::respond_stream(
                &req_id,
                &stream_id,
                &format!(
                    "📎 已生成文件「{path}」（二进制，无法经企微直接发送：{e}；请在桌面端查看）"
                ),
                true,
            ))
            .await
            .map_err(|e| format!("发送提示失败：{e}"))?;
            Ok(format!("文件已生成（文本兜底）：{path}"))
        }
    }
}

/// 媒体上传（best-effort）：用机器人凭证换 token 后 POST 到 media/upload。
/// 端点/字段以线上协议为准；失败由调用方兜底。
async fn upload_media(app: &AppHandle, bytes: &[u8], media_type: &str) -> Result<String, String> {
    let st = app.state::<WecomState>();
    let creds = st.applied.lock().await.clone().ok_or("企微未连接")?;
    let client = reqwest::Client::new();

    let token_url = format!(
        "{QYAPI}/gettoken?corpid={}&corpsecret={}",
        creds.bot_id, creds.secret
    );
    let v: Value = client
        .get(&token_url)
        .send()
        .await
        .map_err(|e| format!("换取 token 失败：{e}"))?
        .json()
        .await
        .map_err(|e| format!("token 解析失败：{e}"))?;
    let code = v.get("errcode").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        return Err(format!(
            "企微 token 失败 errcode={} {}",
            code,
            v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("")
        ));
    }
    let token = v
        .get("access_token")
        .and_then(|t| t.as_str())
        .ok_or("token 缺失")?
        .to_string();

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
