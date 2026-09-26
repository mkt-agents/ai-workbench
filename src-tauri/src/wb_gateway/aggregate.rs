//! Aggregate an upstream SSE stream into one OpenAI `chat.completion` JSON.
//!
//! The upstream chat endpoint is stream-only, but many OpenAI clients send
//! `stream:false`. For those requests the gateway rewrites the client body to
//! `stream:true`, consumes the upstream SSE and re-assembles a complete
//! non-streaming response. This module holds that logic as pure functions so
//! it can be unit-tested without a socket.
//!
//! This is the one deliberate exception to "SSE is only observed, never
//! rewritten" (see `sse.rs`): it only applies to the `stream:false` branch.

use std::collections::BTreeMap;

use super::sse::{usage_in_frame, Usage};

/// Ceiling on the text we accumulate. Beyond this the response is refused
/// rather than buffered forever.
const MAX_AGGREGATE_BYTES: usize = 64 * 1024 * 1024;

/// Rewrite a client body so the upstream gets `stream:true`. Drops
/// `stream_options` (some upstreams reject it with the forced stream) and
/// keeps everything else untouched.
pub fn force_stream_body(parsed: &serde_json::Value) -> Result<Vec<u8>, String> {
    let mut value = parsed.clone();
    if let Some(obj) = value.as_object_mut() {
        obj.insert("stream".to_string(), serde_json::Value::Bool(true));
        obj.remove("stream_options");
    }
    serde_json::to_vec(&value).map_err(|e| format!("请求体序列化失败: {e}"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum AggError {
    Overflow,
    Empty,
    /// The stream carried an explicit `error` object (or never produced choices).
    Upstream(String),
}

#[derive(Default)]
struct ChoiceAcc {
    role: Option<String>,
    content: String,
    reasoning: String,
    finish_reason: Option<serde_json::Value>,
}

/// Feed raw SSE chunks; `finish` flushes a trailing line without a newline.
#[derive(Default)]
pub struct SseAggregator {
    pending: Vec<u8>,
    choices: BTreeMap<i64, ChoiceAcc>,
    usage: Option<Usage>,
    id: Option<String>,
    error: Option<String>,
    accumulated: usize,
    overflow: bool,
}

impl SseAggregator {
    pub fn feed(&mut self, chunk: &[u8]) {
        if self.overflow {
            return;
        }
        self.accumulated = self.accumulated.saturating_add(chunk.len());
        if self.accumulated > MAX_AGGREGATE_BYTES {
            self.overflow = true;
            self.pending.clear();
            return;
        }
        self.pending.extend_from_slice(chunk);
        while let Some(nl) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=nl).collect();
            self.consume_line(&line);
        }
    }

    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.consume_line(&line);
        }
    }

    fn consume_line(&mut self, raw: &[u8]) {
        if self.overflow {
            return;
        }
        let text = String::from_utf8_lossy(raw);
        let line = text.trim_end_matches(['\r', '\n']);
        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
            return;
        };
        if payload.is_empty() || payload == "[DONE]" {
            return;
        }
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(payload) else {
            return;
        };
        if let Some(err) = frame.get("error") {
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| err.to_string());
            self.error = Some(message);
            return;
        }
        if self.id.is_none() {
            self.id = frame
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if let Some(usage) = usage_in_frame(payload) {
            self.usage = Some(usage);
        }
        let Some(choices) = frame.get("choices").and_then(|v| v.as_array()) else {
            return;
        };
        for choice in choices {
            let index = choice.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
            let acc = self.choices.entry(index).or_default();
            if let Some(delta) = choice.get("delta") {
                if acc.role.is_none() {
                    acc.role = delta
                        .get("role")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                }
                if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                    acc.content.push_str(content);
                }
                if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                    acc.reasoning.push_str(reasoning);
                }
            }
            if let Some(finish) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                acc.finish_reason = Some(finish.clone());
            }
        }
    }

    pub fn into_completion(mut self, model: &str, created: i64) -> Result<serde_json::Value, AggError> {
        if self.overflow {
            return Err(AggError::Overflow);
        }
        self.finish();
        if let Some(message) = self.error {
            return Err(AggError::Upstream(message));
        }
        if self.choices.is_empty() {
            return Err(AggError::Empty);
        }
        let choices: Vec<serde_json::Value> = self
            .choices
            .into_iter()
            .map(|(index, acc)| {
                let mut message = serde_json::Map::new();
                message.insert("role".to_string(), serde_json::Value::String(acc.role.unwrap_or_else(|| "assistant".to_string())));
                message.insert("content".to_string(), serde_json::Value::String(acc.content));
                if !acc.reasoning.is_empty() {
                    message.insert("reasoning_content".to_string(), serde_json::Value::String(acc.reasoning));
                }
                serde_json::json!({
                    "index": index,
                    "message": serde_json::Value::Object(message),
                    "finish_reason": acc.finish_reason.unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();
        let mut completion = serde_json::Map::new();
        completion.insert("id".to_string(), serde_json::Value::String(self.id.unwrap_or_else(new_completion_id)));
        completion.insert("object".to_string(), serde_json::Value::String("chat.completion".to_string()));
        completion.insert("created".to_string(), serde_json::Value::from(created));
        completion.insert("model".to_string(), serde_json::Value::String(model.to_string()));
        completion.insert("choices".to_string(), serde_json::Value::Array(choices));
        if let Some(usage) = self.usage.take() {
            completion.insert("usage".to_string(), serde_json::json!({
                "prompt_tokens": usage.prompt_tokens.unwrap_or(0),
                "completion_tokens": usage.completion_tokens.unwrap_or(0),
            }));
        }
        Ok(serde_json::Value::Object(completion))
    }
}

fn new_completion_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or_default();
    format!("chatcmpl-{nanos:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn feed_all(agg: &mut SseAggregator, frames: &[&str]) {
        for frame in frames {
            agg.feed(format!("{frame}\n").as_bytes());
        }
        agg.finish();
    }

    #[test]
    fn a_plain_stream_becomes_one_completion() {
        let mut agg = SseAggregator::default();
        feed_all(
            &mut agg,
            &[
                r#"data: {"id":"c1","choices":[{"index":0,"delta":{"role":"assistant","content":"你"}}]}"#,
                r#"data: {"choices":[{"index":0,"delta":{"content":"好"}}]}"#,
                r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":2}}"#,
                "data: [DONE]",
            ],
        );
        let completion = agg.into_completion("deepseek-chat", 1000).unwrap();
        assert_eq!(completion["id"], "c1");
        assert_eq!(completion["object"], "chat.completion");
        assert_eq!(completion["model"], "deepseek-chat");
        assert_eq!(completion["choices"][0]["message"]["role"], "assistant");
        assert_eq!(completion["choices"][0]["message"]["content"], "你好");
        assert_eq!(completion["choices"][0]["finish_reason"], "stop");
        assert_eq!(completion["usage"]["prompt_tokens"], 4);
        assert_eq!(completion["usage"]["completion_tokens"], 2);
    }

    #[test]
    fn content_split_across_chunks_is_reassembled() {
        let frame = r#"data: {"choices":[{"index":0,"delta":{"content":"中断处"}}]}"#;
        let bytes = format!("{frame}\n").into_bytes();
        let mid = frame.len() / 2;
        let mut agg = SseAggregator::default();
        agg.feed(&bytes[..mid]);
        agg.feed(&bytes[mid..]);
        agg.feed("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"接上了\"}}]}\r\n".as_bytes());
        agg.finish();
        let completion = agg.into_completion("m", 1).unwrap();
        assert_eq!(completion["choices"][0]["message"]["content"], "中断处接上了");
    }

    #[test]
    fn multiple_choices_keep_their_indexes_and_finish_reasons() {
        let mut agg = SseAggregator::default();
        feed_all(
            &mut agg,
            &[
                r#"data: {"choices":[{"index":1,"delta":{"content":"乙"}},{"index":0,"delta":{"content":"甲"}}]}"#,
                r#"data: {"choices":[{"index":1,"delta":{},"finish_reason":"length"}]}"#,
            ],
        );
        let completion = agg.into_completion("m", 1).unwrap();
        assert_eq!(completion["choices"][0]["index"], 0);
        assert_eq!(completion["choices"][0]["message"]["content"], "甲");
        assert_eq!(completion["choices"][1]["index"], 1);
        assert_eq!(completion["choices"][1]["message"]["content"], "乙");
        assert_eq!(completion["choices"][1]["finish_reason"], "length");
    }

    #[test]
    fn missing_index_is_treated_as_zero() {
        let mut agg = SseAggregator::default();
        feed_all(&mut agg, &[r#"data: {"choices":[{"delta":{"content":"x"}}]}"#]);
        let completion = agg.into_completion("m", 1).unwrap();
        assert_eq!(completion["choices"][0]["index"], 0);
    }

    #[test]
    fn reasoning_content_is_carried_when_present() {
        let mut agg = SseAggregator::default();
        feed_all(
            &mut agg,
            &[
                r#"data: {"choices":[{"delta":{"reasoning_content":"想想"}}]}"#,
                r#"data: {"choices":[{"delta":{"content":"答案"}}]}"#,
            ],
        );
        let completion = agg.into_completion("m", 1).unwrap();
        assert_eq!(completion["choices"][0]["message"]["reasoning_content"], "想想");
        assert_eq!(completion["choices"][0]["message"]["content"], "答案");
    }

    #[test]
    fn the_last_usage_frame_wins() {
        let mut agg = SseAggregator::default();
        feed_all(
            &mut agg,
            &[
                r#"data: {"choices":[{"index":0,"delta":{"content":"x"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
                r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":8}}"#,
            ],
        );
        let completion = agg.into_completion("m", 1).unwrap();
        assert_eq!(completion["usage"]["prompt_tokens"], 9);
    }

    #[test]
    fn a_missing_usage_is_omitted_not_zeroed() {
        let mut agg = SseAggregator::default();
        feed_all(&mut agg, &[r#"data: {"choices":[{"delta":{"content":"x"}}]}"#]);
        let completion = agg.into_completion("m", 1).unwrap();
        assert!(completion.get("usage").is_none());
    }

    #[test]
    fn done_comments_and_broken_frames_are_ignored() {
        let mut agg = SseAggregator::default();
        feed_all(
            &mut agg,
            &[
                ": keep-alive comment",
                "event: ping",
                "data: ",
                "data: [DONE]",
                "data: not json",
                r#"data: {"choices":[{"index":0,"delta":{"content":"好"}}]}"#,
            ],
        );
        let completion = agg.into_completion("m", 1).unwrap();
        assert_eq!(completion["choices"][0]["message"]["content"], "好");
    }

    #[test]
    fn an_error_frame_surfaces_its_message() {
        let mut agg = SseAggregator::default();
        feed_all(&mut agg, &[r#"data: {"error":{"message":"配额用尽"}}"#]);
        assert_eq!(agg.into_completion("m", 1), Err(AggError::Upstream("配额用尽".to_string())));
    }

    #[test]
    fn a_stream_without_choices_is_empty_not_a_completion() {
        let mut agg = SseAggregator::default();
        feed_all(&mut agg, &["data: [DONE]"]);
        assert_eq!(agg.into_completion("m", 1), Err(AggError::Empty));
        let agg = SseAggregator::default();
        assert_eq!(agg.into_completion("m", 1), Err(AggError::Empty));
    }

    #[test]
    fn overflowing_the_buffer_is_refused() {
        let mut agg = SseAggregator::default();
        let chunk = vec![b'd'; 1024 * 1024];
        for _ in 0..70 {
            agg.feed(&chunk);
        }
        assert_eq!(agg.into_completion("m", 1), Err(AggError::Overflow));
    }

    #[test]
    fn non_utf8_bytes_do_not_panic() {
        let mut agg = SseAggregator::default();
        agg.feed(&[0xff, 0xfe, b'\n']);
        agg.feed(r#"data: {"choices":[{"delta":{"content":"好"}}]}"#.as_bytes());
        agg.finish();
        assert!(agg.into_completion("m", 1).is_ok());
    }

    #[test]
    fn a_trailing_frame_without_newline_is_flushed() {
        let mut agg = SseAggregator::default();
        agg.feed(r#"data: {"choices":[{"index":0,"delta":{"content":"尾"}}]}"#.as_bytes());
        agg.finish();
        let completion = agg.into_completion("m", 1).unwrap();
        assert_eq!(completion["choices"][0]["message"]["content"], "尾");
    }

    #[test]
    fn force_stream_body_sets_stream_and_drops_stream_options() {
        let parsed = json!({
            "model": "deepseek-chat",
            "stream": false,
            "stream_options": {"include_usage": true},
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.7,
        });
        let body: serde_json::Value = serde_json::from_slice(&force_stream_body(&parsed).unwrap()).unwrap();
        assert_eq!(body["stream"], true);
        assert!(body.get("stream_options").is_none());
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn force_stream_body_defaults_stream_when_absent() {
        let parsed = json!({"model": "m", "messages": []});
        let body: serde_json::Value = serde_json::from_slice(&force_stream_body(&parsed).unwrap()).unwrap();
        assert_eq!(body["stream"], true);
    }
}
