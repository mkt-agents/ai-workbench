//! 企业微信智能机器人长连接 JSON 协议（官方 path 101463）。
//! 只覆盖本迭代需要的命令：订阅、心跳、消息/事件回调、流式回复、欢迎语。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const WS_URL: &str = "wss://openws.work.weixin.qq.com";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameHeaders {
    pub req_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InboundFrame {
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub headers: Option<FrameHeaders>,
    #[serde(default)]
    pub body: Option<Value>,
    #[serde(default)]
    pub errcode: Option<i64>,
    #[serde(default)]
    pub errmsg: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MediaRef {
    pub kind: String, // "image" | "file"
    /// 平台直接给出的可下载 URL（优先）。
    pub url: String,
    /// 临时素材 id；无 url 时由宿主经业务 API / media/get 拉取。
    pub media_id: Option<String>,
    pub filename: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MsgCallback {
    pub req_id: String,
    pub msgid: String,
    pub chattype: String,
    pub chatid: Option<String>,
    pub userid: String,
    /// 平台原始类型；分流逻辑在 extract_content 里完成，这里保留备查
    #[allow(dead_code)]
    pub msgtype: String,
    pub text: String,
    pub quote_text: Option<String>,
    /// 图片 / 文件附件（智能机器人回调里直接带可下载 url）
    pub media: Vec<MediaRef>,
    /// 语音消息但企微未回传转写文本（否则会静默丢消息）
    pub voice_empty: bool,
    /// 平台回调里显式的 @ 标志（`mentioned` / `is_mentioned` 字段）；无该字段为 None。
    /// 群聊闸门优先用它；缺失时回落到「平台已过滤非@投递」的宽松假设。
    pub body_mentioned: Option<bool>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct EventCallback {
    pub req_id: String,
    pub msgid: String,
    pub chattype: Option<String>,
    pub chatid: Option<String>,
    pub userid: Option<String>,
    pub eventtype: String,
}

pub fn subscribe(req_id: &str, bot_id: &str, secret: &str) -> String {
    json!({
        "cmd": "aibot_subscribe",
        "headers": { "req_id": req_id },
        "body": { "bot_id": bot_id, "secret": secret }
    })
    .to_string()
}

pub fn ping(req_id: &str) -> String {
    json!({
        "cmd": "ping",
        "headers": { "req_id": req_id }
    })
    .to_string()
}

pub fn respond_stream(req_id: &str, stream_id: &str, content: &str, finish: bool) -> String {
    // Preserve reply content verbatim; JSON serialization handles transport escaping.
    json!({
        "cmd": "aibot_respond_msg",
        "headers": { "req_id": req_id },
        "body": {
            "msgtype": "stream",
            "stream": {
                "id": stream_id,
                "finish": finish,
                "content": content
            }
        }
    })
    .to_string()
}

pub fn respond_welcome_text(req_id: &str, content: &str) -> String {
    json!({
        "cmd": "aibot_respond_welcome_msg",
        "headers": { "req_id": req_id },
        "body": {
            "msgtype": "text",
            "text": { "content": content }
        }
    })
    .to_string()
}

/// 出站图片：aibot 以 inline base64 回发（字段名以线上协议为准，待联调验证）。
pub fn respond_image(req_id: &str, stream_id: &str, media_type: &str, data_base64: &str) -> String {
    json!({
        "cmd": "aibot_respond_msg",
        "headers": { "req_id": req_id },
        "body": {
            "msgtype": "image",
            "stream_id": stream_id,
            "image": { "base64": data_base64, "media_type": media_type }
        }
    })
    .to_string()
}

/// 出站文件：需先经企微媒体上传拿到 media_id（见 `wecom_media::upload_media`）。
/// 若上传不可用，调用方会回退为文本提示。
pub fn respond_file(req_id: &str, stream_id: &str, media_id: &str, filename: &str) -> String {
    json!({
        "cmd": "aibot_respond_msg",
        "headers": { "req_id": req_id },
        "body": {
            "msgtype": "file",
            "stream_id": stream_id,
            "file": { "media_id": media_id, "filename": filename }
        }
    })
    .to_string()
}

pub fn parse_inbound(raw: &str) -> Result<InboundFrame, String> {
    serde_json::from_str(raw).map_err(|e| format!("wecom frame json: {e}"))
}

pub fn parse_msg_callback(frame: &InboundFrame) -> Option<MsgCallback> {
    if frame.cmd.as_deref() != Some("aibot_msg_callback") {
        return None;
    }
    let req_id = frame.headers.as_ref()?.req_id.clone();
    let body = frame.body.as_ref()?;
    let msgid = body.get("msgid")?.as_str()?.to_string();
    let chattype = body
        .get("chattype")
        .and_then(|v| v.as_str())
        .unwrap_or("single")
        .to_string();
    let chatid = body
        .get("chatid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let userid = body
        .get("from")
        .and_then(|f| f.get("userid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let msgtype = body
        .get("msgtype")
        .and_then(|v| v.as_str())
        .unwrap_or("text")
        .to_string();
    let (text, media, voice_empty) = extract_content(body, &msgtype);
    let quote_text = body.get("quote").and_then(extract_quote_text);
    let body_mentioned = extract_mentioned(body);
    Some(MsgCallback {
        req_id,
        msgid,
        chattype,
        chatid,
        userid,
        msgtype,
        text,
        quote_text,
        media,
        voice_empty,
        body_mentioned,
    })
}

/// 从回调 body 里读显式 @ 标志：兼容 `mentioned` / `is_mentioned`（bool 或 0/1）。
/// 没有这些字段返回 None（表示平台未显式标注，由上层按宽松假设处理）。
fn extract_mentioned(body: &Value) -> Option<bool> {
    for key in ["mentioned", "is_mentioned", "at_me", "ismentioned"] {
        if let Some(v) = body.get(key) {
            if let Some(b) = v.as_bool() {
                return Some(b);
            }
            if let Some(n) = v.as_i64() {
                return Some(n != 0);
            }
        }
    }
    None
}

pub fn parse_event_callback(frame: &InboundFrame) -> Option<EventCallback> {
    if frame.cmd.as_deref() != Some("aibot_event_callback") {
        return None;
    }
    let req_id = frame.headers.as_ref()?.req_id.clone();
    let body = frame.body.as_ref()?;
    let msgid = body
        .get("msgid")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let eventtype = body
        .get("event")
        .and_then(|e| e.get("eventtype"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chattype = body
        .get("chattype")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let chatid = body
        .get("chatid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let userid = body
        .get("from")
        .and_then(|f| f.get("userid"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(EventCallback {
        req_id,
        msgid,
        chattype,
        chatid,
        userid,
        eventtype,
    })
}

/// 抽出 (文本, 媒体列表, 语音是否空转写)。图片/文件只记可下载 url，无法下载的记成文本线索。
fn extract_content(body: &Value, msgtype: &str) -> (String, Vec<MediaRef>, bool) {
    match msgtype {
        "text" => (text_of(body), Vec::new(), false),
        "voice" => {
            let rec = body
                .get("voice")
                .and_then(|t| t.get("content").or_else(|| t.get("recognition")))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let empty = rec.trim().is_empty();
            (rec, Vec::new(), empty)
        }
        "image" => {
            let m = body.get("image").and_then(|i| media_from(i, "image"));
            (String::new(), m.into_iter().collect(), false)
        }
        "file" => {
            let m = body.get("file").and_then(|i| media_from(i, "file"));
            (String::new(), m.into_iter().collect(), false)
        }
        "mixed" => {
            let items = body
                .get("mixed")
                .and_then(|m| m.get("msg_item"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut parts = Vec::new();
            let mut media = Vec::new();
            for item in items {
                let t = item.get("msgtype").and_then(|v| v.as_str()).unwrap_or("");
                match t {
                    "text" => {
                        if let Some(c) = item
                            .get("text")
                            .and_then(|x| x.get("content"))
                            .and_then(|v| v.as_str())
                        {
                            if !c.trim().is_empty() {
                                parts.push(c.to_string());
                            }
                        }
                    }
                    "image" => {
                        if let Some(m) = item.get("image").and_then(|i| media_from(i, "image")) {
                            media.push(m);
                        }
                    }
                    "file" => {
                        if let Some(m) = item.get("file").and_then(|i| media_from(i, "file")) {
                            media.push(m);
                        }
                    }
                    _ => {}
                }
            }
            (parts.join("\n"), media, false)
        }
        _ => (String::new(), Vec::new(), false),
    }
}

fn text_of(body: &Value) -> String {
    body.get("text")
        .and_then(|t| t.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// 从 image/file 子对象取可下载 url 或 media_id；两者皆空时返回 None。
fn media_from(v: &Value, kind: &str) -> Option<MediaRef> {
    let url = v
        .get("url")
        .or_else(|| v.get("image_url"))
        .or_else(|| v.get("file_url"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    let media_id = v
        .get("media_id")
        .and_then(|m| m.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if url.trim().is_empty() && media_id.is_none() {
        return None;
    }
    let filename = v
        .get("filename")
        .or_else(|| v.get("name"))
        .and_then(|f| f.as_str())
        .map(|s| s.to_string());
    Some(MediaRef {
        kind: kind.to_string(),
        url,
        media_id,
        filename,
    })
}

fn extract_quote_text(quote: &Value) -> Option<String> {
    let msgtype = quote.get("msgtype")?.as_str()?;
    let text = extract_content(quote, msgtype).0;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// 去掉群聊里的 @机器人（支持前缀 `@名`、任意位置 `@显示名`、全角空格）。
pub fn strip_mention(content: &str, bot_display_name: &str) -> (String, bool) {
    let trimmed = content.trim();
    let name = bot_display_name.trim();

    // 1) 正文以 `@` 开头：剥第一段 @token
    if trimmed.starts_with('@') {
        let after_at = &trimmed[1..];
        let rest = if !name.is_empty() && after_at.starts_with(name) {
            after_at[name.len()..].trim_start_matches([' ', '\u{3000}', '\t'])
        } else if let Some(pos) = after_at.find([' ', '\u{3000}', '\t', '\n']) {
            after_at[pos..].trim_start()
        } else {
            ""
        };
        return (rest.trim().to_string(), true);
    }

    // 2) 任意位置的 `@显示名`（平台可能已剥前导 @，或 @ 不在开头）
    if !name.is_empty() {
        let needle = format!("@{name}");
        if let Some(pos) = trimmed.find(&needle) {
            let before = trimmed[..pos].trim_end();
            let after = trimmed[pos + needle.len()..].trim_start_matches([' ', '\u{3000}', '\t']);
            let merged = if before.is_empty() {
                after.to_string()
            } else if after.is_empty() {
                before.to_string()
            } else {
                format!("{before} {after}")
            };
            return (merged.trim().to_string(), true);
        }
    }

    (trimmed.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_mention_with_name() {
        let (t, m) = strip_mention("@天玑 帮我看下", "天玑");
        assert!(m);
        assert_eq!(t, "帮我看下");
    }

    #[test]
    fn strip_mention_without_name() {
        let (t, m) = strip_mention("@Bot\u{3000}hello", "");
        assert!(m);
        assert_eq!(t, "hello");
    }

    #[test]
    fn markdown_reply_boundary_preserves_content_without_touching_protocol_ids() {
        let content = "## 结果\n**未自动报价**\n- 项目 `fixture.parent.pom\\doc/`\n- matchId=1454805448406827008\n> WARN count=0 [L1]\nAuthorization: Bearer fixture-secret";
        let wire: Value = serde_json::from_str(&respond_stream("transport-id", "stream-id", content, true)).unwrap();
        let shown = wire["body"]["stream"]["content"].as_str().unwrap();
        assert_eq!(shown, content);
        let welcome: Value = serde_json::from_str(&respond_welcome_text("welcome-id", content)).unwrap();
        assert_eq!(welcome["body"]["text"]["content"], content);
        assert!(shown.starts_with("## 结果\n**未自动报价**"));
        assert!(shown.contains("> WARN count=0 [L1]"));
        assert_eq!(wire["headers"]["req_id"], "transport-id");
        assert_eq!(wire["body"]["stream"]["id"], "stream-id");
        assert_eq!(wire["body"]["msgtype"], "stream");
    }

    #[test]
    fn no_mention() {
        let (t, m) = strip_mention("普通消息", "天玑");
        assert!(!m);
        assert_eq!(t, "普通消息");
    }

    #[test]
    fn strip_mention_mid_text() {
        let (t, m) = strip_mention("帮我看下 @天玑 这个报错", "天玑");
        assert!(m);
        assert_eq!(t, "帮我看下 这个报错");
    }

    #[test]
    fn parse_text_msg() {
        let raw = r#"{
            "cmd":"aibot_msg_callback",
            "headers":{"req_id":"r1"},
            "body":{
                "msgid":"m1",
                "chattype":"group",
                "chatid":"c1",
                "from":{"userid":"u1"},
                "msgtype":"text",
                "text":{"content":"@Bot hi"}
            }
        }"#;
        let frame = parse_inbound(raw).unwrap();
        let msg = parse_msg_callback(&frame).unwrap();
        assert_eq!(msg.userid, "u1");
        assert_eq!(msg.chattype, "group");
        assert_eq!(msg.text, "@Bot hi");
    }

    #[test]
    fn parse_image_msg_captures_media() {
        let raw = r#"{
            "cmd":"aibot_msg_callback",
            "headers":{"req_id":"r2"},
            "body":{
                "msgid":"m2","chattype":"single",
                "from":{"userid":"u2"},"msgtype":"image",
                "image":{"url":"https://example.com/a.png","md5":"x"}
            }
        }"#;
        let frame = parse_inbound(raw).unwrap();
        let msg = parse_msg_callback(&frame).unwrap();
        assert!(msg.text.is_empty());
        assert_eq!(msg.media.len(), 1);
        assert_eq!(msg.media[0].kind, "image");
        assert_eq!(msg.media[0].url, "https://example.com/a.png");
        assert!(msg.media[0].media_id.is_none());
    }

    #[test]
    fn parse_image_msg_accepts_media_id_without_url() {
        let raw = r#"{
            "cmd":"aibot_msg_callback",
            "headers":{"req_id":"r2b"},
            "body":{
                "msgid":"m2b","chattype":"single",
                "from":{"userid":"u2"},"msgtype":"image",
                "image":{"media_id":"MEDIA123","filename":"shot.png"}
            }
        }"#;
        let msg = parse_msg_callback(&parse_inbound(raw).unwrap()).unwrap();
        assert_eq!(msg.media.len(), 1);
        assert_eq!(msg.media[0].media_id.as_deref(), Some("MEDIA123"));
        assert!(msg.media[0].url.trim().is_empty());
        assert_eq!(msg.media[0].filename.as_deref(), Some("shot.png"));
    }

    #[test]
    fn parse_voice_empty_recognition() {
        let raw = r#"{
            "cmd":"aibot_msg_callback",
            "headers":{"req_id":"r3"},
            "body":{"msgid":"m3","chattype":"single",
                "from":{"userid":"u3"},"msgtype":"voice",
                "voice":{"media_id":"M1"}}
        }"#;
        let frame = parse_inbound(raw).unwrap();
        let msg = parse_msg_callback(&frame).unwrap();
        assert!(msg.text.is_empty());
        assert!(msg.voice_empty);
    }

    #[test]
    fn parse_group_mentioned_flag() {
        // 显式 is_mentioned=false → body_mentioned Some(false)
        let raw = r#"{
            "cmd":"aibot_msg_callback",
            "headers":{"req_id":"r4"},
            "body":{"msgid":"m4","chattype":"group","chatid":"c1",
                "from":{"userid":"u4"},"msgtype":"text",
                "text":{"content":"hi"},"is_mentioned":false}
        }"#;
        let msg = parse_msg_callback(&parse_inbound(raw).unwrap()).unwrap();
        assert_eq!(msg.body_mentioned, Some(false));

        // 数字 1 → true
        let raw2 = r#"{"cmd":"aibot_msg_callback","headers":{"req_id":"r5"},
            "body":{"msgid":"m5","chattype":"group","chatid":"c1",
                "from":{"userid":"u5"},"msgtype":"text",
                "text":{"content":"hi"},"mentioned":1}}"#;
        let msg2 = parse_msg_callback(&parse_inbound(raw2).unwrap()).unwrap();
        assert_eq!(msg2.body_mentioned, Some(true));

        // 无字段 → None
        let raw3 = r#"{"cmd":"aibot_msg_callback","headers":{"req_id":"r6"},
            "body":{"msgid":"m6","chattype":"group","chatid":"c1",
                "from":{"userid":"u6"},"msgtype":"text","text":{"content":"hi"}}}"#;
        let msg3 = parse_msg_callback(&parse_inbound(raw3).unwrap()).unwrap();
        assert_eq!(msg3.body_mentioned, None);
    }
}
