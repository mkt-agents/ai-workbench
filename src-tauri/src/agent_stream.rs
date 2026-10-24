//! Streaming helpers for agent rounds: split an SSE byte stream into `data:` payloads,
//! pull the text delta / usage out of a chunk, and hide reasoning tags while streaming.
//! Pure, no IO — the request loop lives in `agent.rs`.

use std::time::Duration;

use serde_json::{json, Value};

/// Collects raw bytes and hands back complete `data:` payloads. Splitting only at `\n`
/// is UTF-8 safe: the newline byte never occurs inside a multi-byte character.
#[derive(Default)]
pub(crate) struct SseLines {
    pending: Vec<u8>,
}

impl SseLines {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(data) = line.trim().strip_prefix("data:") {
                let data = data.trim();
                if !data.is_empty() && data != "[DONE]" {
                    out.push(data.to_string());
                }
            }
        }
        out
    }
}

/// Visible text delta of one OpenAI-style stream chunk (reasoning fields are not answer text).
pub(crate) fn delta_content(chunk: &Value) -> Option<&str> {
    chunk
        .pointer("/choices/0/delta/content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// `(prompt, completion)` tokens when the chunk carries a usage block (usually the last one).
pub(crate) fn chunk_usage(chunk: &Value) -> Option<(u64, u64)> {
    let usage = chunk.get("usage")?;
    let prompt = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let completion = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    (prompt > 0 || completion > 0).then_some((prompt, completion))
}

/// Input tokens the provider served from its prompt cache.
/// OpenAI-style: `usage.prompt_cache_hit_tokens` (DeepSeek) or `prompt_tokens_details.cached_tokens`.
pub(crate) fn chunk_cache_hit(chunk: &Value) -> u64 {
    let direct = chunk.pointer("/usage/prompt_cache_hit_tokens").and_then(|v| v.as_u64());
    let nested = chunk
        .pointer("/usage/prompt_tokens_details/cached_tokens")
        .and_then(|v| v.as_u64());
    direct.or(nested).unwrap_or(0)
}

/// Folds an Anthropic SSE stream back into the shape of a non-streaming
/// `message` body (`{ "content": [...] }`), so one parser serves both paths.
///
/// Text arrives as `text_delta`; a `tool_use` block's input arrives as
/// `input_json_delta` fragments that only parse once the block stops.
#[derive(Default)]
pub(crate) struct AnthropicStream {
    /// Blocks in `index` order (Anthropic numbers them 0, 1, 2, … without gaps).
    blocks: Vec<Value>,
    /// Text / raw tool input per block, parallel to `blocks`.
    texts: Vec<String>,
    args: Vec<String>,
    input_tokens: u64,
    output_tokens: u64,
    /// Tokens served from the prompt cache (billed at a fraction of the input price).
    cache_read: u64,
}

impl AnthropicStream {
    /// Fold one `data:` payload; returns the visible text delta it carried.
    pub(crate) fn push(&mut self, data: &str) -> Result<Option<String>, String> {
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            // `ping` and other keep-alives; bodyless events are not our business.
            return Ok(None);
        };
        match event.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "message_start" => {
                if let Some(message) = event.get("message") {
                    if let Some(usage) = message.get("usage") {
                        self.add_usage(usage);
                    }
                    // A gateway may skip the per-block events and send the whole message at once.
                    if self.blocks.is_empty() {
                        if let Some(arr) = message.get("content").and_then(|v| v.as_array()) {
                            self.adopt(arr);
                        }
                    }
                }
                Ok(None)
            }
            "content_block_start" => {
                let index = block_index(&event);
                self.grow(index);
                let block = event
                    .get("content_block")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "text" }));
                let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    self.args[index] = tool_input_raw(block.get("input"));
                }
                self.texts[index] = text.clone();
                self.blocks[index] = block;
                Ok((!text.is_empty()).then_some(text))
            }
            "content_block_delta" => {
                let index = block_index(&event);
                let delta = event.get("delta").cloned().unwrap_or_else(|| json!({}));
                match delta.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "text_delta" => {
                        let piece = delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if piece.is_empty() {
                            return Ok(None);
                        }
                        self.grow(index);
                        self.texts[index].push_str(piece);
                        Ok(Some(piece.to_string()))
                    }
                    "input_json_delta" => {
                        let piece = delta.get("partial_json").and_then(|v| v.as_str()).unwrap_or("");
                        self.grow(index);
                        self.args[index].push_str(piece);
                        Ok(None)
                    }
                    // thinking_delta / signature_delta: not answer text, blocks are kept out.
                    _ => Ok(None),
                }
            }
            "content_block_stop" => {
                let index = block_index(&event);
                if index < self.args.len() && self.args[index].is_empty() && self.blocks[index].get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    // No input at all is a valid empty call, not a broken one.
                    self.args[index] = "{}".to_string();
                }
                Ok(None)
            }
            "message_delta" => {
                if let Some(usage) = event.get("usage") {
                    self.add_usage(usage);
                }
                Ok(None)
            }
            "error" => Err(event
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or("模型服务在流式中途报错")
                .to_string()),
            _ => Ok(None),
        }
    }

    /// `(prompt, completion)` tokens reported by the stream, when it reported any.
    pub(crate) fn usage(&self) -> Option<(u64, u64)> {
        (self.input_tokens > 0 || self.output_tokens > 0).then_some((self.input_tokens, self.output_tokens))
    }

    /// Input tokens that came from the prompt cache (0 when the provider said nothing).
    pub(crate) fn cache_read(&self) -> u64 {
        self.cache_read
    }

    /// The folded `message` body (`{ "content": [...] }`), ready for `parse_anthropic`.
    pub(crate) fn into_body(self) -> Value {
        let AnthropicStream { blocks, texts, args, .. } = self;
        // `thinking` / `redacted_thinking` blocks are dropped: replaying one without its
        // signature is rejected by the provider, and they are never shown as answer text.
        let content: Vec<Value> = blocks
            .into_iter()
            .enumerate()
            .filter(|(_, block)| matches!(block.get("type").and_then(|v| v.as_str()), Some("text") | Some("tool_use")))
            .map(|(i, mut block)| {
                match block.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "text" => block["text"] = json!(texts.get(i).cloned().unwrap_or_default()),
                    "tool_use" => {
                        let raw = args.get(i).map(|s| s.trim()).unwrap_or("");
                        block["input"] = serde_json::from_str::<Value>(raw)
                            .ok()
                            .filter(|v| v.is_object())
                            .unwrap_or_else(|| json!({}));
                    }
                    _ => {}
                }
                block
            })
            .collect();
        json!({ "content": content })
    }

    fn grow(&mut self, index: usize) {
        while self.blocks.len() <= index {
            self.blocks.push(json!({ "type": "text" }));
            self.texts.push(String::new());
            self.args.push(String::new());
        }
    }

    /// Take a whole `message.content` array (non-streaming body replayed in a start event).
    fn adopt(&mut self, arr: &[Value]) {
        self.blocks.clear();
        self.texts.clear();
        self.args.clear();
        for block in arr {
            let kind = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            self.blocks.push(block.clone());
            self.texts.push(if kind == "text" {
                block.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string()
            } else {
                String::new()
            });
            self.args.push(if kind == "tool_use" { tool_input_raw(block.get("input")) } else { String::new() });
        }
    }

    fn add_usage(&mut self, usage: &Value) {
        let count = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let cached = count("cache_read_input_tokens");
        let input = count("input_tokens") + count("cache_creation_input_tokens") + cached;
        let output = count("output_tokens");
        if input > 0 {
            self.input_tokens = input;
        }
        if cached > 0 {
            self.cache_read = cached;
        }
        if output > 0 {
            self.output_tokens = output;
        }
    }
}

fn block_index(event: &Value) -> usize {
    event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize
}

/// Tool input as a raw JSON string; empty when there is nothing to accumulate into.
fn tool_input_raw(input: Option<&Value>) -> String {
    match input {
        Some(v) if v.as_object().map(|o| !o.is_empty()).unwrap_or(true) => v.to_string(),
        _ => String::new(),
    }
}

/// How long to wait before retrying a failed model request, or `None` when the
/// failure is not worth retrying.
///
/// `429` / `5xx` / a connection error are transient (free routers answer 429 all
/// day); everything else is the caller's problem and is returned as-is. A
/// `Retry-After` header wins over our own backoff.
pub(crate) fn retry_plan(status: Option<u16>, attempt: u32, retry_after: Option<u64>) -> Option<Duration> {
    const MAX_RETRIES: u32 = 3;
    if attempt >= MAX_RETRIES {
        return None;
    }
    let transient = match status {
        Some(code) => matches!(code, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504),
        // No status at all: the send itself failed (DNS / reset / connect timeout).
        None => true,
    };
    if !transient {
        return None;
    }
    if let Some(secs) = retry_after {
        return Some(Duration::from_secs(secs.clamp(1, 60)));
    }
    // 300ms, 600ms, 1.2s + a little jitter so parallel turns do not sync up.
    let base = 300u64 << attempt.min(4);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_millis()) % 200)
        .unwrap_or(0);
    Some(Duration::from_millis(base + jitter))
}

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// Streaming twin of `ai_commands::strip_think_tags`: drops `<think>…</think>` blocks and a
/// lone `</think>` at the very start (provider moved the reasoning into another field),
/// even when a tag is split across chunks. Leading whitespace of the round is dropped too.
#[derive(Default)]
pub(crate) struct ThinkFilter {
    buf: String,
    in_think: bool,
    /// Some visible text has been emitted this round.
    started: bool,
}

impl ThinkFilter {
    pub(crate) fn push(&mut self, delta: &str) -> String {
        self.buf.push_str(delta);
        let mut out = String::new();
        loop {
            // ASCII lowercase keeps byte offsets aligned with `buf`.
            let lower = self.buf.to_ascii_lowercase();
            if self.in_think {
                match lower.find(CLOSE) {
                    Some(pos) => {
                        self.buf.drain(..pos + CLOSE.len());
                        self.in_think = false;
                        continue;
                    }
                    None => {
                        // Keep a possible partial `</think>` at the end, drop the rest.
                        let keep = partial_tag_start(&self.buf).unwrap_or(self.buf.len());
                        self.buf.drain(..keep);
                        return out;
                    }
                }
            }
            if !self.started {
                let lead = self.buf.len() - self.buf.trim_start().len();
                let rest = &lower[lead..];
                if rest.starts_with(CLOSE) {
                    self.buf.drain(..lead + CLOSE.len());
                    continue;
                }
                if rest.is_empty() || (rest.len() < CLOSE.len() && (CLOSE.starts_with(rest) || OPEN.starts_with(rest))) {
                    // Only whitespace or the beginning of a tag so far: wait for more.
                    return out;
                }
            }
            if let Some(pos) = lower.find(OPEN) {
                let before: String = self.buf.drain(..pos).collect();
                self.emit(&mut out, &before);
                self.buf.drain(..OPEN.len());
                self.in_think = true;
                continue;
            }
            if let Some(pos) = lower.find(CLOSE) {
                // A stray closing tag later in the round: drop just the tag.
                let before: String = self.buf.drain(..pos).collect();
                self.emit(&mut out, &before);
                self.buf.drain(..CLOSE.len());
                continue;
            }
            let keep = partial_tag_start(&self.buf).unwrap_or(self.buf.len());
            let ready: String = self.buf.drain(..keep).collect();
            self.emit(&mut out, &ready);
            return out;
        }
    }

    /// End of round: flush what was held back (an unfinished `<think>` stays hidden).
    pub(crate) fn finish(&mut self) -> String {
        let mut out = String::new();
        if !self.in_think {
            let rest = std::mem::take(&mut self.buf);
            self.emit(&mut out, &rest);
        }
        self.buf.clear();
        out
    }

    fn emit(&mut self, out: &mut String, text: &str) {
        let text = if self.started { text } else { text.trim_start() };
        if !text.is_empty() {
            self.started = true;
            out.push_str(text);
        }
    }
}

/// Byte index where a trailing, still-incomplete `<think>` / `</think>` begins.
fn partial_tag_start(buf: &str) -> Option<usize> {
    let lt = buf.rfind('<')?;
    let tail = buf[lt..].to_ascii_lowercase();
    (tail.len() < CLOSE.len() && (OPEN.starts_with(&tail) || CLOSE.starts_with(&tail))).then_some(lt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> String {
        let mut f = ThinkFilter::default();
        let mut out = String::new();
        for c in chunks {
            out.push_str(&f.push(c));
        }
        out.push_str(&f.finish());
        out
    }

    #[test]
    fn think_blocks_are_hidden_across_chunks() {
        assert_eq!(run(&["<think>想一想</think>\n\n答案"]), "答案");
        assert_eq!(run(&["<thi", "nk>想", "一想</th", "ink>答", "案"]), "答案");
        assert_eq!(run(&["前文<think>中间", "</think>后文"]), "前文后文");
        // Unfinished thinking at the end of the round stays hidden.
        assert_eq!(run(&["答案<think>还在想"]), "答案");
    }

    #[test]
    fn lone_closing_tag_at_the_start_is_dropped() {
        assert_eq!(run(&["</think>\n"]), "");
        assert_eq!(run(&["\n</thi", "nk>\n好的"]), "好的");
        // Later stray closing tags lose only the tag.
        assert_eq!(run(&["你好", "</think>世界"]), "你好世界");
    }

    #[test]
    fn plain_text_streams_through() {
        assert_eq!(run(&["a < b", " 且 c<d"]), "a < b 且 c<d");
        assert_eq!(run(&["  缩进", "\n第二行"]), "缩进\n第二行");
        // A held-back `<` is released once the next chunk shows it is not a tag.
        let mut f = ThinkFilter::default();
        assert_eq!(f.push("x <"), "x ");
        assert_eq!(f.push("= y"), "<= y");
    }

    #[test]
    fn sse_lines_split_on_newlines_only() {
        let mut lines = SseLines::default();
        let text = "data: {\"a\":1}\n\ndata: [DONE]\n";
        let bytes = text.as_bytes();
        let mut out = lines.push(&bytes[..7]);
        assert!(out.is_empty());
        out.extend(lines.push(&bytes[7..]));
        assert_eq!(out, vec!["{\"a\":1}".to_string()]);
        // Multi-byte characters split across pushes survive.
        let zh = "data: {\"t\":\"中文\"}\n".as_bytes();
        let mut out = lines.push(&zh[..14]);
        out.extend(lines.push(&zh[14..]));
        assert_eq!(out, vec!["{\"t\":\"中文\"}".to_string()]);
    }

    #[test]
    fn delta_and_usage_are_read_from_chunks() {
        let chunk = json!({ "choices": [{ "delta": { "content": "hi", "reasoning_content": "x" } }] });
        assert_eq!(delta_content(&chunk), Some("hi"));
        assert_eq!(delta_content(&json!({ "choices": [{ "delta": { "reasoning": "x" } }] })), None);
        let last = json!({ "choices": [], "usage": { "prompt_tokens": 12, "completion_tokens": 30 } });
        assert_eq!(chunk_usage(&last), Some((12, 30)));
        assert_eq!(chunk_usage(&chunk), None);
    }

    fn fold(events: &[&str]) -> AnthropicStream {
        let mut acc = AnthropicStream::default();
        for e in events {
            acc.push(e).expect("流式事件不该失败");
        }
        acc
    }

    #[test]
    fn anthropic_text_deltas_fold_into_one_text_block() {
        let mut acc = AnthropicStream::default();
        assert_eq!(acc.push(r#"{"type":"message_start","message":{"content":[],"usage":{"input_tokens":9}}}"#), Ok(None));
        assert_eq!(acc.push(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#), Ok(None));
        assert_eq!(acc.push(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你"}}"#), Ok(Some("你".into())));
        assert_eq!(acc.push(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"好"}}"#), Ok(Some("好".into())));
        acc.push(r#"{"type":"content_block_stop","index":0}"#).unwrap();
        acc.push(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#).unwrap();
        acc.push(r#"{"type":"message_stop"}"#).unwrap();
        assert_eq!(acc.usage(), Some((9, 4)));
        let body = acc.into_body();
        assert_eq!(body["content"], json!([{ "type": "text", "text": "你好" }]));
    }

    #[test]
    fn anthropic_tool_input_is_collected_until_the_block_stops() {
        let acc = fold(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"看看"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":".rs\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
        ]);
        let body = acc.into_body();
        assert_eq!(body["content"][0]["text"], json!("看看"));
        assert_eq!(body["content"][1]["type"], json!("tool_use"));
        assert_eq!(body["content"][1]["name"], json!("read_file"));
        assert_eq!(body["content"][1]["input"]["path"], json!("a.rs"));
    }

    #[test]
    fn anthropic_stream_survives_unknown_events_and_bad_input() {
        // Keep-alives and thinking deltas carry no answer text.
        let acc = fold(&[
            r#"{"type":"ping"}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"嗯"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            // Tool input cut off mid-JSON falls back to `{}` instead of failing the round.
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t","name":"grep","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"pattern\":"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
        ]);
        let body = acc.into_body();
        let blocks = body["content"].as_array().unwrap();
        // `thinking` blocks are dropped: replaying a half-filled one is rejected by the provider.
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["name"], json!("grep"));
        assert_eq!(blocks[0]["input"], json!({}));

        // An error event is surfaced, not swallowed.
        let mut acc = AnthropicStream::default();
        let err = acc.push(r#"{"type":"error","error":{"type":"overloaded_error","message":"服务繁忙"}}"#);
        assert_eq!(err, Err("服务繁忙".to_string()));
    }

    #[test]
    fn anthropic_message_start_may_carry_the_whole_message() {
        let acc = fold(&[r#"{"type":"message_start","message":{"content":[{"type":"text","text":"整段"}],"usage":{"input_tokens":3,"output_tokens":2}}}"#]);
        assert_eq!(acc.usage(), Some((3, 2)));
        assert_eq!(acc.into_body()["content"], json!([{ "type": "text", "text": "整段" }]));
    }

    #[test]
    fn cache_reads_are_picked_up_from_usage() {
        let mut acc = AnthropicStream::default();
        acc.push(
            r#"{"type":"message_start","message":{"content":[],"usage":{"input_tokens":7,"cache_read_input_tokens":1200,"cache_creation_input_tokens":100}}}"#,
        )
        .unwrap();
        acc.push(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#)
            .unwrap();
        assert_eq!(acc.cache_read(), 1200);
        // 命中部分仍算进 input（计费口径），但来源行要把命中数单独说出来
        assert_eq!(acc.usage(), Some((1307, 5)));
    }

    #[test]
    fn retry_only_for_transient_statuses() {
        assert!(retry_plan(Some(429), 0, None).is_some());
        assert!(retry_plan(Some(503), 2, None).is_some());
        assert!(retry_plan(None, 0, None).is_some(), "连接失败值得再试");
        assert!(retry_plan(Some(400), 0, None).is_none());
        assert!(retry_plan(Some(401), 0, None).is_none());
        assert!(retry_plan(Some(404), 0, None).is_none());
        // 3 次封顶
        assert!(retry_plan(Some(503), 3, None).is_none());
    }

    #[test]
    fn retry_after_wins_over_backoff() {
        assert_eq!(retry_plan(Some(429), 0, Some(7)), Some(Duration::from_secs(7)));
        // 离谱的取值夹紧，不照做
        assert_eq!(retry_plan(Some(429), 0, Some(0)), Some(Duration::from_secs(1)));
        assert_eq!(retry_plan(Some(429), 0, Some(9_999)), Some(Duration::from_secs(60)));
        let first = retry_plan(Some(503), 0, None).unwrap();
        assert!(first >= Duration::from_millis(300) && first < Duration::from_millis(600));
        let second = retry_plan(Some(503), 1, None).unwrap();
        assert!(second >= Duration::from_millis(600) && second < Duration::from_millis(900));
    }
}
