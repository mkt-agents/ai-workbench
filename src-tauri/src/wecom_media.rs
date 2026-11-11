//! 企微富媒体：入站落盘 + 出站回发。
//!
//! 出站：图片走 inline base64（`aibot_respond_msg` 的 image.base64）；
//! 文本文件直接贴回，二进制经 media/upload 拿 media_id（需业务 API 凭证）。
//! 入站：回调 url 优先经 rustls 下载（避开部分 Windows Schannel `SEC_E_NO_CREDENTIALS`）；
//! 长连接 `image/file.url` 返回密文，解密语义对齐 `@wecom/aibot-node-sdk` 的 `decryptFile`
//! 与 dsh-im-connect（AES-256-CBC，IV=密钥前 16 字节，PKCS#7 至多 32；aeskey Base64
//! 按 Node `Buffer.from(..., 'base64')` 容忍缺 `=`）；URL 失败且已配业务 API 时可选回退
//! `media/get`；仅 media_id 时走 qyapi media/get（明文，需业务 API）。宿主不做 OCR。
//! 素材落盘 `%APPDATA%\com.ai-workbench.app\wecom-media\<msgid>\`，mtime 超过 7 天回收；
//! 同一 msgid 短时缓存，供准入后预取与组 prompt 复用。

use aes::Aes256;
use base64::Engine;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Manager};

use crate::wecom_channel::{AppliedCreds, OutTarget, WecomState};
use crate::wecom_proto::{self, MediaRef};

const QYAPI: &str = "https://qyapi.weixin.qq.com/cgi-bin";
const MEDIA_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_INBOUND_BYTES: usize = 12 * 1024 * 1024;

/// 入站 COS 临时链走 rustls，避开部分 Windows Schannel 上的 SEC_E_NO_CREDENTIALS。
fn rustls_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// 同一 msgid 的落盘结果短时复用，避免排队后再下一次撞上 COS 过期。
fn inbound_cache() -> &'static Mutex<HashMap<String, Vec<LocalMedia>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<LocalMedia>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn biz_api_configured(creds: &AppliedCreds) -> bool {
    matches!(
        (creds.corp_id.as_deref(), creds.corp_secret.as_deref()),
        (Some(id), Some(sec)) if !id.trim().is_empty() && !sec.trim().is_empty()
    )
}

/// 把下载/TLS/过期错误收成可操作的中文提示（不逼用户必须配业务 API）。
pub fn friendly_media_fetch_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("sec_e_no_credentials")
        || lower.contains("schannel")
        || (lower.contains("certificate") && lower.contains("tls"))
        || lower.contains("native-tls")
        || (lower.contains("tls")
            && (lower.contains("handshake") || lower.contains("connector")))
    {
        return format!(
            "{raw}。本机 TLS/代理可能拦截了企微图片下载；请检查系统时间、公司 HTTPS 代理，或稍后重发图片。"
        );
    }
    if lower.contains("403")
        || lower.contains("401")
        || lower.contains("410")
        || lower.contains("过期")
        || lower.contains("expired")
        || lower.contains("signature")
    {
        return format!("{raw}。临时下载链接可能已过期，请重新发送图片后再试。");
    }
    raw.to_string()
}

fn http_status_error(status: reqwest::StatusCode, body_hint: &str) -> String {
    let code = status.as_u16();
    if matches!(code, 401 | 403 | 410) {
        format!("下载失败：HTTP {code}（临时链接可能已过期或无权限）{body_hint}")
    } else {
        format!("下载失败：HTTP {code}{body_hint}")
    }
}

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
) -> Result<(String, String, String), String> {
    targets
        .get(request_id)
        .map(|t| (t.bot_id.clone(), t.req_id.clone(), t.stream_id.clone()))
        .ok_or_else(|| "当前请求没有进行中的企微对话，发送目标已失效".into())
}

/// 当前回合的企微出站目标（由 orchestrator 按 agent request_id 登记）。
async fn target_sock(
    app: &AppHandle,
    request_id: &str,
) -> Result<(Arc<crate::wecom_ws::WecomSocket>, String, String), String> {
    let st = app.state::<WecomState>();
    let (bot_id, req_id, stream_id) = {
        let targets = st.out_targets.lock().await;
        lookup_target(&targets, request_id)?
    };
    let sock = st
        .bot(&bot_id)
        .await
        .ok_or_else(|| format!("机器人 {bot_id} 未连接"))?
        .sock
        .clone();
    Ok((sock, req_id, stream_id))
}

/// 把本地图片作为 inline base64 发回当前企微对话。
pub async fn send_image(
    app: &AppHandle,
    path: &str,
    request_id: &str,
) -> Result<String, String> {
    let (sock, req_id, stream_id) = target_sock(app, request_id).await?;
    let name = file_name(path);
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("读取图片失败：{e}"))?;
    let media_type = mime_of(path);
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
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
    let (sock, req_id, stream_id) = target_sock(app, request_id).await?;
    let name = file_name(path);
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("读取文件失败：{e}"))?;

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

    match upload_media(app, request_id, &bytes, &mime_of(path)).await {
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
async fn upload_media(
    app: &AppHandle,
    request_id: &str,
    bytes: &[u8],
    media_type: &str,
) -> Result<String, String> {
    let st = app.state::<WecomState>();
    let bot_id = st
        .out_targets
        .lock()
        .await
        .get(request_id)
        .map(|t| t.bot_id.clone())
        .ok_or("企微未连接")?;
    let bot = st
        .bot(&bot_id)
        .await
        .ok_or_else(|| format!("机器人 {bot_id} 未连接"))?;
    let creds = bot.applied.lock().await.clone().ok_or("企微未连接")?;
    let client = reqwest::Client::new();

    let token = crate::wecom_biz::access_token(&bot, &creds).await?;
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

/// 入站落盘结果：成功项 + 失败摘要（给模型/用户看，不含 COS URL）。
#[derive(Debug, Clone, Default)]
pub struct MaterializeOutcome {
    pub locals: Vec<LocalMedia>,
    pub errors: Vec<String>,
}

/// 模型可见的失败文案；永不嵌入 http(s) URL。
pub fn media_fetch_failure_text(errors: &[String]) -> String {
    if errors.is_empty() {
        "[图片获取失败：下载或解密失败，请重发图片或粘贴文字]".into()
    } else {
        format!("[图片获取失败：{}]", errors.join("；"))
    }
}

/// 把入站 MediaRef 拉到本地；url 优先（rustls），失败且已配业务 API 时可选 media/get。
/// 同一 msgid 命中短时缓存则直接复用，避免二次下载。
pub async fn materialize_inbound(
    app: &AppHandle,
    bot_id: &str,
    msgid: &str,
    media: &[MediaRef],
) -> MaterializeOutcome {
    if media.is_empty() {
        return MaterializeOutcome::default();
    }
    if let Ok(guard) = inbound_cache().lock() {
        if let Some(cached) = guard.get(msgid) {
            return MaterializeOutcome {
                locals: cached.clone(),
                errors: Vec::new(),
            };
        }
    }
    let _ = cleanup_old_media();
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (i, m) in media.iter().enumerate() {
        match fetch_inbound_bytes(app, bot_id, m).await {
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
                    Err(e) => {
                        let tip = format!("写入失败：{e}");
                        crate::wecom_log::write(
                            "media-in",
                            &format!("msgid={msgid} write failed: {tip}"),
                        );
                        errors.push(tip);
                    }
                }
            }
            Err(e) => {
                let tip = friendly_media_fetch_error(&e);
                crate::wecom_log::write(
                    "media-in",
                    &format!("msgid={msgid} fetch failed: {tip}"),
                );
                errors.push(tip);
            }
        }
    }
    if !out.is_empty() {
        if let Ok(mut guard) = inbound_cache().lock() {
            // 简单上限：超过 32 条按插入序不保证，清空再放本条，避免无限涨。
            if guard.len() >= 32 {
                guard.clear();
            }
            guard.insert(msgid.to_string(), out.clone());
        }
    }
    MaterializeOutcome {
        locals: out,
        errors,
    }
}

async fn fetch_inbound_bytes(
    app: &AppHandle,
    bot_id: &str,
    m: &MediaRef,
) -> Result<(Vec<u8>, String), String> {
    if !m.url.trim().is_empty() {
        match download_and_decrypt_url(m).await {
            Ok(v) => return Ok(v),
            Err(url_err) => {
                if let Some(id) = m
                    .media_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if biz_api_ready(app, bot_id).await {
                        match download_media_id(app, bot_id, id).await {
                            Ok(v) => {
                                crate::wecom_log::write(
                                    "media-in",
                                    &format!(
                                        "url failed ({}); fell back to media/get",
                                        friendly_media_fetch_error(&url_err)
                                    ),
                                );
                                return Ok(v);
                            }
                            Err(e) => {
                                return Err(format!(
                                    "{}；业务 API 回退也失败：{}",
                                    friendly_media_fetch_error(&url_err),
                                    friendly_media_fetch_error(&e)
                                ));
                            }
                        }
                    }
                }
                return Err(friendly_media_fetch_error(&url_err));
            }
        }
    }
    let media_id = m
        .media_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "媒体缺少 url 与 media_id".to_string())?;
    download_media_id(app, bot_id, media_id)
        .await
        .map_err(|e| friendly_media_fetch_error(&e))
}

async fn biz_api_ready(app: &AppHandle, bot_id: &str) -> bool {
    let st = app.state::<WecomState>();
    let Some(bot) = st.bot(bot_id).await else {
        return false;
    };
    let Some(creds) = bot.applied.lock().await.clone() else {
        return false;
    };
    biz_api_configured(&creds)
}

async fn download_and_decrypt_url(m: &MediaRef) -> Result<(Vec<u8>, String), String> {
    let (raw, hinted) = download_url(m.url.trim()).await?;
    let bytes = match m.aeskey.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(key) => decrypt_wecom_media(&raw, key)?,
        None => {
            // 长连接图片几乎总带 aeskey；没有密钥却像密文时明确失败，避免把乱码当图喂模型。
            if looks_like_image_magic(&raw) || m.kind != "image" {
                raw
            } else {
                return Err(
                    "入站图片缺少 aeskey，且下载内容不是可识别图片（长连接媒体需 AES 解密）"
                        .into(),
                );
            }
        }
    };
    let media_type = sniff_media_type(&bytes).unwrap_or(hinted);
    Ok((bytes, media_type))
}

async fn download_url(url: &str) -> Result<(Vec<u8>, String), String> {
    let resp = rustls_client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("下载失败：{e}"))?;
    let status = resp.status();
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
    if !status.is_success() {
        let body = resp.bytes().await.unwrap_or_default();
        let hint = if body.is_empty() {
            String::new()
        } else {
            let preview = String::from_utf8_lossy(&body);
            let short: String = preview.chars().take(120).collect();
            format!("：{short}")
        };
        return Err(http_status_error(status, &hint));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取失败：{e}"))?;
    if bytes.len() > MAX_INBOUND_BYTES {
        return Err(format!("媒体超过 {}MB 上限", MAX_INBOUND_BYTES / (1024 * 1024)));
    }
    Ok((bytes.to_vec(), media_type))
}

type Aes256CbcDec = cbc::Decryptor<Aes256>;

/// 对齐 Node `Buffer.from(aesKey, 'base64')` / SDK `decryptFile`：企微常下发无 `=` 的
/// 32 字节密钥 Base64；Rust `STANDARD` 会报 Invalid padding，需补齐后再解。
pub fn decode_wecom_media_aeskey(raw: &str) -> Result<[u8; 32], String> {
    let mut s: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if s.is_empty() {
        return Err("aeskey 为空".into());
    }
    // URL-safe 偶发出现；SDK 主路径是标准字母表，这里顺手归一。
    s = s.replace('-', "+").replace('_', "/");
    let rem = s.len() % 4;
    if rem != 0 {
        s.extend(std::iter::repeat('=').take(4 - rem));
    }
    let decoded = match base64::engine::general_purpose::STANDARD.decode(&s) {
        Ok(v) => v,
        Err(e_std) => base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(s.trim_end_matches('='))
            .map_err(|e_np| format!("aeskey base64 无效：{e_std} / no-pad: {e_np}"))?,
    };
    if decoded.len() != 32 {
        return Err(format!(
            "aeskey 解码后应为 32 字节（AES-256），实际 {}",
            decoded.len()
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&decoded);
    Ok(key)
}

/// 企微智能机器人长连接媒体解密，语义对齐 `@wecom/aibot-node-sdk` `decryptFile`
/// 与 dsh-im-connect：AES-256-CBC，密钥为 aeskey 的 Base64（32 字节），IV=密钥前 16 字节，
/// `setAutoPadding(false)` 后手动去 PKCS#7（pad ∈ [1, 32]）。
pub fn decrypt_wecom_media(encrypted: &[u8], aeskey_b64: &str) -> Result<Vec<u8>, String> {
    if encrypted.is_empty() {
        return Err("加密媒体为空".into());
    }
    if encrypted.len() % 16 != 0 {
        return Err(format!(
            "加密媒体长度 {} 不是 16 的倍数，无法 AES-CBC 解密",
            encrypted.len()
        ));
    }
    let key = decode_wecom_media_aeskey(aeskey_b64)?;
    let mut buf = encrypted.to_vec();
    let plain = Aes256CbcDec::new_from_slices(&key, &key[..16])
        .map_err(|e| format!("AES 初始化失败：{e}"))?
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|e| format!("AES 解密失败：{e}"))?;
    strip_pkcs7(plain, 32)
}

fn strip_pkcs7(data: &[u8], max_pad: u8) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Err("解密结果为空".into());
    }
    let pad = *data.last().unwrap();
    if pad < 1 || pad > max_pad || pad as usize > data.len() {
        return Err(format!("PKCS#7 填充非法：pad={pad}"));
    }
    let cut = data.len() - pad as usize;
    if data[cut..].iter().any(|&b| b != pad) {
        return Err("PKCS#7 填充字节不一致".into());
    }
    Ok(data[..cut].to_vec())
}

fn looks_like_image_magic(bytes: &[u8]) -> bool {
    sniff_media_type(bytes).is_some_and(|m| m.starts_with("image/"))
}

fn sniff_media_type(bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 3 && bytes[0] == 0xff && bytes[1] == 0xd8 && bytes[2] == 0xff {
        return Some("image/jpeg".into());
    }
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png".into());
    }
    if bytes.len() >= 6
        && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
    {
        return Some("image/gif".into());
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp".into());
    }
    if bytes.len() >= 5 && bytes.starts_with(b"%PDF-") {
        return Some("application/pdf".into());
    }
    None
}

/// 经自建应用凭证拉取临时素材；机器人 Bot Secret 不能换 token。
async fn download_media_id(
    app: &AppHandle,
    bot_id: &str,
    media_id: &str,
) -> Result<(Vec<u8>, String), String> {
    let st = app.state::<WecomState>();
    let bot = st
        .bot(bot_id)
        .await
        .ok_or_else(|| format!("机器人 {bot_id} 未连接"))?;
    let creds = bot.applied.lock().await.clone().ok_or("企微未连接")?;
    let token = crate::wecom_biz::access_token(&bot, &creds).await?;
    let url = format!("{QYAPI}/media/get?access_token={token}&media_id={media_id}");
    let resp = rustls_client()
        .get(&url)
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
                    bot_id: "bot-a".into(),
                    req_id: "callback-a".into(),
                    stream_id: "stream-a".into(),
                },
            ),
            (
                "agent-b".into(),
                OutTarget {
                    bot_id: "bot-b".into(),
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
            ("bot-a".into(), "callback-a".into(), "stream-a".into())
        );
        assert_eq!(
            lookup_target(&targets, "agent-b").unwrap(),
            ("bot-b".into(), "callback-b".into(), "stream-b".into())
        );
    }

    #[test]
    fn cleaning_up_a_preserves_b_and_never_redirects_a() {
        let mut targets = targets();
        targets.remove("agent-a");
        assert!(lookup_target(&targets, "agent-a").is_err());
        assert_eq!(
            lookup_target(&targets, "agent-b").unwrap(),
            ("bot-b".into(), "callback-b".into(), "stream-b".into())
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

    fn pkcs7_pad_32(data: &[u8]) -> Vec<u8> {
        let rem = data.len() % 32;
        let pad = if rem == 0 { 32 } else { 32 - rem };
        let mut out = data.to_vec();
        out.extend(std::iter::repeat(pad as u8).take(pad));
        out
    }

    fn encrypt_wecom_media_fixture(plain: &[u8], key32: &[u8; 32]) -> Vec<u8> {
        use cbc::cipher::{BlockEncryptMut, KeyIvInit};
        type Aes256CbcEnc = cbc::Encryptor<Aes256>;
        let mut buf = pkcs7_pad_32(plain);
        let len = buf.len();
        Aes256CbcEnc::new_from_slices(key32, &key32[..16])
            .unwrap()
            .encrypt_padded_mut::<NoPadding>(&mut buf, len)
            .unwrap();
        buf
    }

    #[test]
    fn decrypt_wecom_media_roundtrips_jpeg_fixture() {
        let key = *b"0123456789abcdef0123456789abcdef";
        let aeskey = base64::engine::general_purpose::STANDARD.encode(key);
        // Minimal JPEG SOI + filler so magic sniff works after decrypt.
        let mut plain = vec![0xff, 0xd8, 0xff, 0xe0];
        plain.extend_from_slice(b"JFIF-fixture-body");
        let cipher = encrypt_wecom_media_fixture(&plain, &key);
        assert_ne!(&cipher[..3], &[0xff, 0xd8, 0xff], "密文不应再带 JPEG 头");
        assert_eq!(cipher.len() % 16, 0);
        let out = decrypt_wecom_media(&cipher, &aeskey).unwrap();
        assert_eq!(out, plain);
        assert_eq!(sniff_media_type(&out).as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn decrypt_wecom_media_accepts_unpadded_aeskey_like_node_sdk() {
        // 线上 Invalid padding：企微常下发无 trailing '=' 的 32 字节 key Base64。
        let key = *b"0123456789abcdef0123456789abcdef";
        let padded = base64::engine::general_purpose::STANDARD.encode(key);
        let unpadded = padded.trim_end_matches('=').to_string();
        assert!(
            !unpadded.ends_with('='),
            "fixture 应去掉填充：{unpadded}"
        );
        assert_ne!(unpadded.len() % 4, 0, "无填充长度通常非 4 倍数");
        let mut plain = vec![0xff, 0xd8, 0xff, 0xe0];
        plain.extend_from_slice(b"unpadded-aeskey");
        let cipher = encrypt_wecom_media_fixture(&plain, &key);
        let out = decrypt_wecom_media(&cipher, &unpadded).expect("应对齐 Node Buffer.from base64");
        assert_eq!(out, plain);
        // 中间空白也应可解（消息帧偶发换行）。
        let spaced = format!("{} {}", &unpadded[..16], &unpadded[16..]);
        assert_eq!(decrypt_wecom_media(&cipher, &spaced).unwrap(), plain);
        assert_eq!(decode_wecom_media_aeskey(&padded).unwrap(), key);
        assert_eq!(decode_wecom_media_aeskey(&unpadded).unwrap(), key);
    }

    #[test]
    fn decrypt_wecom_media_rejects_bad_key_and_padding() {
        let key = *b"0123456789abcdef0123456789abcdef";
        let aeskey = base64::engine::general_purpose::STANDARD.encode(key);
        let cipher = encrypt_wecom_media_fixture(b"hello-wecom-media", &key);
        assert!(decrypt_wecom_media(&cipher, "not-base64!!!").is_err());
        assert!(decode_wecom_media_aeskey("").is_err());
        let short = base64::engine::general_purpose::STANDARD.encode(b"too-short");
        assert!(decrypt_wecom_media(&cipher, &short).is_err());
        assert!(decrypt_wecom_media(&cipher[..15], &aeskey).is_err());
        assert!(decrypt_wecom_media(&[], &aeskey).is_err());
        // 错误密钥：CBC 能出字节，PKCS#7 校验应失败。
        let wrong = *b"ffffffffffffffffffffffffffffffff";
        let wrong_b64 = base64::engine::general_purpose::STANDARD.encode(wrong);
        let err = decrypt_wecom_media(&cipher, &wrong_b64).unwrap_err();
        assert!(
            err.contains("PKCS#7") || err.contains("填充"),
            "错 key 应落在 PKCS#7：{err}"
        );
    }

    #[test]
    fn sniff_media_type_recognizes_common_headers() {
        assert_eq!(
            sniff_media_type(&[0xff, 0xd8, 0xff, 0xe0]).as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(
            sniff_media_type(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).as_deref(),
            Some("image/png")
        );
        assert!(sniff_media_type(&[0xf8, 0xdc, 0x65, 0x8c]).is_none());
    }

    #[test]
    fn media_fetch_failure_text_never_embeds_url() {
        let empty = media_fetch_failure_text(&[]);
        assert!(empty.contains("图片获取失败"), "{empty}");
        assert!(!empty.to_ascii_lowercase().contains("http"), "{empty}");

        let with_err = media_fetch_failure_text(&[
            "下载失败：HTTP 403（临时链接可能已过期或无权限）".into(),
        ]);
        assert!(with_err.contains("图片获取失败"), "{with_err}");
        assert!(with_err.contains("403"), "{with_err}");
        assert!(
            !with_err.contains("https://") && !with_err.contains("myqcloud"),
            "不得把 COS URL 塞进失败提示: {with_err}"
        );
    }

    #[test]
    fn friendly_media_fetch_error_maps_tls_and_expiry() {
        let tls = friendly_media_fetch_error(
            "下载失败：error sending request for url: error trying to connect: SEC_E_NO_CREDENTIALS",
        );
        assert!(tls.contains("代理") || tls.contains("TLS"), "{tls}");
        assert!(tls.contains("SEC_E_NO_CREDENTIALS"), "{tls}");

        let expired = friendly_media_fetch_error("下载失败：HTTP 403（临时链接可能已过期或无权限）");
        assert!(expired.contains("重新发送"), "{expired}");

        assert_eq!(
            friendly_media_fetch_error("媒体超过 12MB 上限"),
            "媒体超过 12MB 上限"
        );
    }

    #[test]
    fn http_status_error_marks_auth_and_gone() {
        let e = http_status_error(reqwest::StatusCode::FORBIDDEN, "");
        assert!(e.contains("403"), "{e}");
        assert!(e.contains("过期") || e.contains("无权限"), "{e}");
        let e2 = http_status_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "：oops");
        assert!(e2.contains("500") && e2.contains("oops"), "{e2}");
    }

    #[test]
    fn biz_api_configured_requires_both_fields() {
        let empty = AppliedCreds {
            bot_id: "b".into(),
            secret: "s".into(),
            corp_id: None,
            corp_secret: None,
        };
        assert!(!biz_api_configured(&empty));
        let partial = AppliedCreds {
            bot_id: "b".into(),
            secret: "s".into(),
            corp_id: Some("ww".into()),
            corp_secret: None,
        };
        assert!(!biz_api_configured(&partial));
        let ok = AppliedCreds {
            bot_id: "b".into(),
            secret: "s".into(),
            corp_id: Some("ww".into()),
            corp_secret: Some("sec".into()),
        };
        assert!(biz_api_configured(&ok));
    }
}
