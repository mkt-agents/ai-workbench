//! Streaming helpers for agent rounds: split an SSE byte stream into `data:` payloads,
//! pull the text delta / usage out of a chunk, and hide reasoning tags while streaming.
//! Pure, no IO — the request loop lives in `agent.rs`.

use serde_json::Value;

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
    use serde_json::json;

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
}
