//! 天玑 harness 的消息协议：内容块（text / image / tool_use / tool_result）。
//!
//! 为什么单独一层：以前前端只把每一轮压成 `{ role, content }` 的纯文本，工具调用在
//! 历史里就消失了——模型看不到自己上一轮调过什么、拿到了什么。这一层让请求、回灌、
//! 将来跨轮复用的工具结果都用同一种块结构表达；真正的 provider 形状（OpenAI 的
//! `tool_calls` + `role:"tool"`、Anthropic 的 `tool_use` / `tool_result` 内容块）
//! 只在 [`provider_messages`] 里翻译一次。
//!
//! 前端镜像在 `src/components/aiHub/harness.ts`：两边字段名必须一致（camelCase）。
//! 旧的 `history + user` 纯文本入参仍然可用，由 `agent.rs` 包成 Text 块后走同一条路径。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 一条消息里的内容块。`kind` 用 snake_case（跨语言稳定），字段用 camelCase。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Block {
    #[serde(rename = "text")]
    Text {
        text: String,
    },
    /// 图片一律 base64 内联（截图、附件预览都一样），不落盘。
    #[serde(rename = "image", rename_all = "camelCase")]
    Image {
        media_type: String,
        data: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// 枚举上的 `rename_all` 只管变体名，字段名要在变体上单独声明。
    #[serde(rename = "tool_result", rename_all = "camelCase")]
    ToolResult {
        tool_use_id: String,
        output: String,
        #[serde(default)]
        is_error: bool,
    },
}

/// 展示用的附加信息，不参与 provider 翻译。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageMeta {
    /// 会话摘要轮：前端把很老的轮次折成一条，界面显示成灰色块。
    #[serde(default)]
    pub digest: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// `user` / `assistant`；其它值一律当 user（fail open 在这一层没有安全含义）。
    pub role: String,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub meta: Option<MessageMeta>,
}

impl Message {
    pub fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            blocks: vec![Block::Text { text: text.into() }],
            meta: None,
        }
    }

    /// 这条消息里所有 Text 块拼起来（空校验、摘要、日志用）。
    pub fn joined_text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_assistant(&self) -> bool {
        self.role == "assistant"
    }
}

/// 翻译成 provider 原生的 `messages` 数组。
///
/// - OpenAI：system 作为首条消息；assistant 的 tool_use 变成 `tool_calls`，
///   tool_result 变成独立的 `role:"tool"` 消息，图片走 `image_url` data URL。
/// - Anthropic：system 由调用方单独放进 body（这里不产出），tool_use / tool_result
///   都是内容块；同一轮的多条 tool_result 会留在各自的消息里，顺序不变。
pub fn provider_messages(messages: &[Message], anthropic: bool, system: &str) -> Vec<Value> {
    if anthropic {
        anthropic_messages(messages)
    } else {
        openai_messages(messages, system)
    }
}

fn openai_messages(messages: &[Message], system: &str) -> Vec<Value> {
    let mut out = vec![json!({ "role": "system", "content": system })];
    for msg in messages {
        let mut text = String::new();
        let mut images: Vec<Value> = Vec::new();
        let mut calls: Vec<Value> = Vec::new();
        let mut results: Vec<(String, String, bool)> = Vec::new();
        for block in &msg.blocks {
            match block {
                Block::Text { text: t } => {
                    if !t.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
                Block::Image { media_type, data } => images.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{media_type};base64,{data}") },
                })),
                Block::ToolUse { id, name, input } => calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": input.to_string() },
                })),
                Block::ToolResult {
                    tool_use_id,
                    output,
                    is_error,
                } => results.push((tool_use_id.clone(), output.clone(), *is_error)),
            }
        }
        if msg.is_assistant() {
            // assistant 里的 tool_result 不是合法形状：当文本回灌，别丢掉内容。
            for (_, output, _) in results.drain(..) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&output);
            }
            if !calls.is_empty() {
                let content = if text.is_empty() { Value::Null } else { json!(text) };
                let mut m = json!({ "role": "assistant", "content": content });
                m["tool_calls"] = json!(calls);
                out.push(m);
            } else if !text.is_empty() {
                out.push(json!({ "role": "assistant", "content": text }));
            }
            continue;
        }
        if !text.is_empty() || !images.is_empty() {
            if images.is_empty() {
                out.push(json!({ "role": "user", "content": text }));
            } else {
                let mut parts = Vec::new();
                if !text.is_empty() {
                    parts.push(json!({ "type": "text", "text": text }));
                }
                parts.extend(images);
                out.push(json!({ "role": "user", "content": parts }));
            }
        }
        for (id, output, is_error) in results {
            let content = if is_error {
                format!("错误：{output}")
            } else {
                output
            };
            out.push(json!({ "role": "tool", "tool_call_id": id, "content": content }));
        }
    }
    out
}

fn anthropic_messages(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for msg in messages {
        let mut blocks: Vec<Value> = Vec::new();
        for block in &msg.blocks {
            match block {
                Block::Text { text } => {
                    if !text.is_empty() {
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                }
                Block::Image { media_type, data } => blocks.push(json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data },
                })),
                Block::ToolUse { id, name, input } => blocks.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                })),
                Block::ToolResult {
                    tool_use_id,
                    output,
                    is_error,
                } => {
                    let mut b = json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": output,
                    });
                    if *is_error {
                        b["is_error"] = json!(true);
                    }
                    blocks.push(b);
                }
            }
        }
        if blocks.is_empty() {
            // 空消息会被上游 400（Anthropic 明确拒绝空 content）
            continue;
        }
        out.push(json!({
            "role": if msg.is_assistant() { "assistant" } else { "user" },
            "content": blocks,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_msg(role: &str, text: &str) -> Message {
        Message::text(role, text)
    }

    #[test]
    fn plain_text_becomes_a_system_plus_user_pair_for_openai() {
        let msgs = vec![text_msg("user", "你好")];
        let out = provider_messages(&msgs, false, "SYS");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[0]["content"], "SYS");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[1]["content"], "你好");
    }

    #[test]
    fn anthropic_keeps_system_out_of_messages() {
        let msgs = vec![text_msg("user", "你好")];
        let out = provider_messages(&msgs, true, "SYS");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["content"][0]["type"], "text");
        assert_eq!(out[0]["content"][0]["text"], "你好");
    }

    #[test]
    fn tool_round_trip_survives_both_protocols() {
        let msgs = vec![
            text_msg("user", "读一下 a.txt"),
            Message {
                role: "assistant".into(),
                blocks: vec![
                    Block::Text { text: "我去看看".into() },
                    Block::ToolUse {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        input: json!({ "path": "a.txt" }),
                    },
                ],
                meta: None,
            },
            Message {
                role: "user".into(),
                blocks: vec![Block::ToolResult {
                    tool_use_id: "call_1".into(),
                    output: "hello".into(),
                    is_error: false,
                }],
                meta: None,
            },
        ];

        let openai = provider_messages(&msgs, false, "SYS");
        // system + user + assistant(tool_calls) + tool
        assert_eq!(openai.len(), 4);
        assert_eq!(openai[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(openai[2]["tool_calls"][0]["function"]["name"], "read_file");
        assert_eq!(openai[2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a.txt\"}");
        assert_eq!(openai[3]["role"], "tool");
        assert_eq!(openai[3]["tool_call_id"], "call_1");
        assert_eq!(openai[3]["content"], "hello");

        let anthropic = provider_messages(&msgs, true, "SYS");
        assert_eq!(anthropic.len(), 3);
        assert_eq!(anthropic[1]["content"][1]["type"], "tool_use");
        assert_eq!(anthropic[2]["content"][0]["type"], "tool_result");
        assert_eq!(anthropic[2]["content"][0]["content"], "hello");
    }

    #[test]
    fn errors_are_marked_in_both_shapes() {
        let msgs = vec![Message {
            role: "user".into(),
            blocks: vec![Block::ToolResult {
                tool_use_id: "c".into(),
                output: "bad".into(),
                is_error: true,
            }],
            meta: None,
        }];
        let openai = provider_messages(&msgs, false, "SYS");
        assert_eq!(openai[1]["content"], "错误：bad");
        let anthropic = provider_messages(&msgs, true, "SYS");
        assert_eq!(anthropic[0]["content"][0]["is_error"], true);
    }

    #[test]
    fn empty_messages_are_dropped_for_anthropic() {
        let msgs = vec![Message {
            role: "user".into(),
            blocks: vec![Block::Text { text: String::new() }],
            meta: None,
        }];
        assert!(provider_messages(&msgs, true, "SYS").is_empty());
        // OpenAI 侧同样不产出空 user，但 system 一直在
        assert_eq!(provider_messages(&msgs, false, "SYS").len(), 1);
    }

    #[test]
    fn digest_meta_is_display_only() {
        let msg = Message {
            role: "user".into(),
            blocks: vec![Block::Text { text: "旧对话".into() }],
            meta: Some(MessageMeta { digest: true }),
        };
        let out = provider_messages(&[msg], false, "SYS");
        assert_eq!(out[1]["content"], "旧对话");
    }

    #[test]
    fn block_fields_match_frontend_camel_case() {
        let raw = json!([
            { "kind": "tool_result", "toolUseId": "hist-0-git", "output": "ok", "isError": true },
            { "kind": "image", "mediaType": "image/png", "data": "AAAA" },
        ]);
        let blocks: Vec<Block> = serde_json::from_value(raw).expect("camelCase blocks");
        assert_eq!(
            blocks[0],
            Block::ToolResult { tool_use_id: "hist-0-git".into(), output: "ok".into(), is_error: true }
        );
        assert_eq!(
            blocks[1],
            Block::Image { media_type: "image/png".into(), data: "AAAA".into() }
        );
        let back = serde_json::to_value(&blocks[0]).unwrap();
        assert_eq!(back["toolUseId"], "hist-0-git");
    }
}
