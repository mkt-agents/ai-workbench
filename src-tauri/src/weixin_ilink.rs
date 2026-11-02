//! 个人微信 iLink Bot HTTP 协议层（https://ilinkai.weixin.qq.com）。
//!
//! 仅允许 `*.weixin.qq.com` HTTPS；鉴权头 `AuthorizationType: ilink_bot_token` + Bearer。

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use url::Url;

pub const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const CHANNEL_VERSION: &str = "2.4.8";
const BOT_AGENT: &str = "AIWorkbench";
const APP_ID: &str = "bot";
/// 0x00020408 → decimal string, roughly channel 2.4.8
const APP_CLIENT_VERSION: &str = "132104";

const SHORT_TIMEOUT: Duration = Duration::from_secs(20);
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Debug, Clone)]
pub struct IlinkClient {
    pub base_url: String,
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrStart {
    pub qrcode: String,
    pub qrcode_img_content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrStatus {
    pub status: String,
    pub bot_token: Option<String>,
    pub ilink_bot_id: Option<String>,
    pub ilink_user_id: Option<String>,
    pub baseurl: Option<String>,
    pub redirect_host: Option<String>,
    pub verifycode_length: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WeixinMessage {
    pub seq: Option<i64>,
    pub message_id: Option<Value>,
    pub from_user_id: Option<String>,
    pub to_user_id: Option<String>,
    pub client_id: Option<String>,
    pub create_time_ms: Option<i64>,
    pub session_id: Option<String>,
    pub group_id: Option<String>,
    pub message_type: Option<i32>,
    pub message_state: Option<i32>,
    pub item_list: Option<Vec<MessageItem>>,
    pub context_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageItem {
    #[serde(rename = "type")]
    pub item_type: Option<i32>,
    pub text_item: Option<TextItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TextItem {
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GetUpdatesResp {
    pub ret: Option<i64>,
    pub errcode: Option<i64>,
    pub errmsg: Option<String>,
    pub msgs: Option<Vec<WeixinMessage>>,
    pub get_updates_buf: Option<String>,
    pub longpolling_timeout_ms: Option<u64>,
}

fn base_info() -> Value {
    json!({
        "channel_version": CHANNEL_VERSION,
        "bot_agent": BOT_AGENT,
    })
}

/// Only HTTPS hosts under weixin.qq.com (incl. subdomains).
pub fn assert_weixin_https(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url.trim()).map_err(|e| format!("非法 URL：{e}"))?;
    if parsed.scheme() != "https" {
        return Err("微信通道只允许 HTTPS".into());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL 缺少 host".to_string())?
        .to_ascii_lowercase();
    let ok = host == "weixin.qq.com" || host.ends_with(".weixin.qq.com");
    if !ok {
        return Err(format!("URL 不在微信域名白名单：{host}"));
    }
    Ok(parsed)
}

pub fn normalize_base_url(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    let u = if t.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        t.trim_end_matches('/').to_string()
    };
    let parsed = assert_weixin_https(&u)?;
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

fn random_uin_header() -> String {
    let mut buf = [0u8; 4];
    rand::rng().fill_bytes(&mut buf);
    let n = u32::from_le_bytes(buf);
    B64.encode(n.to_string().as_bytes())
}

impl IlinkClient {
    pub fn new(base_url: &str, token: Option<String>) -> Result<Self, String> {
        Ok(Self {
            base_url: normalize_base_url(base_url)?,
            token,
        })
    }

    fn client_short(&self) -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .timeout(SHORT_TIMEOUT)
            .build()
            .map_err(|e| format!("HTTP 客户端失败：{e}"))
    }

    fn client_long(&self) -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .timeout(LONG_POLL_TIMEOUT)
            .build()
            .map_err(|e| format!("HTTP 客户端失败：{e}"))
    }

    fn endpoint(&self, path: &str) -> Result<String, String> {
        let base = self.base_url.trim_end_matches('/');
        let full = format!("{base}/{}", path.trim_start_matches('/'));
        let _ = assert_weixin_https(&full)?;
        Ok(full)
    }

    fn apply_common_headers(
        &self,
        req: reqwest::RequestBuilder,
        with_auth: bool,
    ) -> reqwest::RequestBuilder {
        let mut req = req
            .header("Content-Type", "application/json")
            .header("iLink-App-Id", APP_ID)
            .header("iLink-App-ClientVersion", APP_CLIENT_VERSION)
            .header("AuthorizationType", "ilink_bot_token")
            .header("X-WECHAT-UIN", random_uin_header());
        if with_auth {
            if let Some(tok) = &self.token {
                if !tok.is_empty() {
                    req = req.header("Authorization", format!("Bearer {tok}"));
                }
            }
        }
        req
    }

    pub async fn get_bot_qrcode(&self) -> Result<QrStart, String> {
        let url = format!(
            "{}?bot_type=3",
            self.endpoint("ilink/bot/get_bot_qrcode")?
        );
        let client = self.client_short()?;
        let resp = self
            .apply_common_headers(client.post(&url), false)
            .json(&json!({ "local_token_list": [] }))
            .send()
            .await
            .map_err(|e| format!("申请微信二维码失败：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("申请微信二维码 HTTP {}", resp.status()));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("二维码响应解析失败：{e}"))?;
        let qrcode = v
            .get("qrcode")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "二维码响应缺少 qrcode".to_string())?
            .to_string();
        let content = v
            .get("qrcode_img_content")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "二维码响应缺少 qrcode_img_content".to_string())?
            .to_string();
        Ok(QrStart {
            qrcode,
            qrcode_img_content: content,
        })
    }

    pub async fn get_qrcode_status(
        &self,
        qrcode: &str,
        verify_code: Option<&str>,
    ) -> Result<QrStatus, String> {
        let mut url = format!(
            "{}?qrcode={}",
            self.endpoint("ilink/bot/get_qrcode_status")?,
            urlencoding_encode(qrcode)
        );
        if let Some(code) = verify_code.map(str::trim).filter(|s| !s.is_empty()) {
            url.push_str("&verify_code=");
            url.push_str(&urlencoding_encode(code));
        }
        let client = self.client_short()?;
        // QR status: app headers only, no Authorization / UIN per protocol notes.
        let resp = client
            .get(&url)
            .header("iLink-App-Id", APP_ID)
            .header("iLink-App-ClientVersion", APP_CLIENT_VERSION)
            .send()
            .await
            .map_err(|e| format!("轮询微信扫码失败：{e}"))?;
        if !resp.status().is_success() {
            // Treat transport blips as wait
            return Ok(QrStatus {
                status: "wait".into(),
                bot_token: None,
                ilink_bot_id: None,
                ilink_user_id: None,
                baseurl: None,
                redirect_host: None,
                verifycode_length: None,
            });
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("扫码状态解析失败：{e}"))?;
        Ok(QrStatus {
            status: v
                .get("status")
                .and_then(|x| x.as_str())
                .unwrap_or("wait")
                .to_string(),
            bot_token: v
                .get("bot_token")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            ilink_bot_id: v
                .get("ilink_bot_id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            ilink_user_id: v
                .get("ilink_user_id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            baseurl: v
                .get("baseurl")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            redirect_host: v
                .get("redirect_host")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            verifycode_length: v
                .get("verifycode_length")
                .and_then(|x| x.as_u64())
                .map(|n| n as u32),
        })
    }

    pub async fn get_updates(&self, cursor: &str) -> Result<GetUpdatesResp, String> {
        let url = self.endpoint("ilink/bot/getupdates")?;
        let client = self.client_long()?;
        let body = json!({
            "get_updates_buf": cursor,
            "base_info": base_info(),
        });
        let result = self
            .apply_common_headers(client.post(&url), true)
            .json(&body)
            .send()
            .await;
        match result {
            Ok(resp) => {
                if !resp.status().is_success() {
                    return Err(format!("getupdates HTTP {}", resp.status()));
                }
                resp.json()
                    .await
                    .map_err(|e| format!("getupdates 解析失败：{e}"))
            }
            Err(e) if e.is_timeout() || e.is_request() => Ok(GetUpdatesResp {
                ret: Some(0),
                msgs: Some(vec![]),
                get_updates_buf: Some(cursor.to_string()),
                ..Default::default()
            }),
            Err(e) => Err(format!("getupdates 失败：{e}")),
        }
    }

    pub async fn send_text(
        &self,
        to_user_id: &str,
        text: &str,
        context_token: &str,
    ) -> Result<(), String> {
        if context_token.trim().is_empty() {
            return Err("缺少 context_token，无法投递微信消息（需用户先发一条）".into());
        }
        let url = self.endpoint("ilink/bot/sendmessage")?;
        let client = self.client_short()?;
        let client_id = format!("awb-{}", uuid_like());
        let body = json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to_user_id,
                "client_id": client_id,
                "message_type": 2,
                "message_state": 2,
                "context_token": context_token,
                "item_list": [{
                    "type": 1,
                    "text_item": { "text": text }
                }]
            },
            "base_info": base_info(),
        });
        let resp = self
            .apply_common_headers(client.post(&url), true)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("sendmessage 失败：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("sendmessage HTTP {}", resp.status()));
        }
        let v: Value = resp
            .json()
            .await
            .unwrap_or_else(|_| json!({}));
        if let Some(ret) = v.get("ret").and_then(|x| x.as_i64()) {
            if ret != 0 {
                let errmsg = v
                    .get("errmsg")
                    .and_then(|x| x.as_str())
                    .unwrap_or("(none)");
                return Err(format!("sendmessage ret={ret} errmsg={errmsg}"));
            }
        }
        Ok(())
    }

    /// Split long replies into chunks under ~1800 chars (WeChat IM friendly).
    pub async fn send_text_chunked(
        &self,
        to_user_id: &str,
        text: &str,
        context_token: &str,
    ) -> Result<usize, String> {
        let chunks = chunk_text(text, 1800);
        for part in &chunks {
            self.send_text(to_user_id, part, context_token).await?;
        }
        Ok(chunks.len())
    }
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn uuid_like() -> String {
    let mut buf = [0u8; 16];
    rand::rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut n = 0usize;
    for ch in text.chars() {
        if n >= max_chars {
            out.push(std::mem::take(&mut buf));
            n = 0;
        }
        buf.push(ch);
        n += 1;
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

pub fn extract_text(msg: &WeixinMessage) -> String {
    let mut parts = Vec::new();
    if let Some(items) = &msg.item_list {
        for it in items {
            if it.item_type.unwrap_or(0) == 1 {
                if let Some(t) = it.text_item.as_ref().and_then(|x| x.text.clone()) {
                    if !t.trim().is_empty() {
                        parts.push(t);
                    }
                }
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_accepts_ilink_host() {
        assert!(assert_weixin_https("https://ilinkai.weixin.qq.com/ilink/bot/x").is_ok());
        assert!(assert_weixin_https("http://ilinkai.weixin.qq.com").is_err());
        assert!(assert_weixin_https("https://evil.example.com").is_err());
    }

    #[test]
    fn chunk_splits_long() {
        let s: String = "你".repeat(2000);
        let parts = chunk_text(&s, 1800);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].chars().count(), 1800);
    }

    #[test]
    fn extract_text_from_items() {
        let msg = WeixinMessage {
            item_list: Some(vec![MessageItem {
                item_type: Some(1),
                text_item: Some(TextItem {
                    text: Some("hello".into()),
                }),
            }]),
            ..Default::default()
        };
        assert_eq!(extract_text(&msg), "hello");
    }

    #[test]
    fn normalize_default_base() {
        assert_eq!(normalize_base_url("").unwrap(), DEFAULT_BASE_URL);
    }

    #[tokio::test]
    async fn send_requires_context_token() {
        let c = IlinkClient::new("", Some("tok".into())).unwrap();
        let err = c.send_text("user", "hi", "").await.unwrap_err();
        assert!(err.contains("context_token"));
    }
}
