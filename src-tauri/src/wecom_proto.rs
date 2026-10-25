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
pub struct MsgCallback {
    pub req_id: String,
    pub msgid: String,
    pub chattype: String,
    pub chatid: Option<String>,
    pub userid: String,
    pub msgtype: String,
    pub text: String,
    pub quote_text: Option<String>,
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
    let text = extract_text(body, &msgtype);
    let quote_text = body.get("quote").and_then(extract_quote_text);
    Some(MsgCallback {
        req_id,
        msgid,
        chattype,
        chatid,
        userid,
        msgtype,
        text,
        quote_text,
    })
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

fn extract_text(body: &Value, msgtype: &str) -> String {
    match msgtype {
        "text" => body
            .get("text")
            .and_then(|t| t.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "voice" => body
            .get("voice")
            .and_then(|t| t.get("content").or_else(|| t.get("recognition")))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "mixed" => {
            // 图文混排：拼接文本项
            let items = body
                .get("mixed")
                .and_then(|m| m.get("msg_item"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut parts = Vec::new();
            for item in items {
                let t = item
                    .get("msgtype")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if t == "text" {
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
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

fn extract_quote_text(quote: &Value) -> Option<String> {
    let msgtype = quote.get("msgtype")?.as_str()?;
    let text = extract_text(quote, msgtype);
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
}
