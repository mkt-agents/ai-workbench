//! Side-channel usage scanner for streamed upstream responses.
//!
//! The gateway forwards SSE bytes verbatim — re-serialising them would change
//! framing and break strict OpenAI clients — so token accounting has to happen
//! by *observing* the stream. This scanner is fed raw chunks and reports the
//! last `usage` object it saw, without ever holding more than a bounded window
//! of the stream in memory.

use serde::{Deserialize, Serialize};

/// Tokens as reported by the upstream. All fields optional because plenty of
/// compatible servers omit usage on streamed responses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.prompt_tokens.is_none() && self.completion_tokens.is_none()
    }
}

/// Largest line we will buffer while hunting for a usage frame. A single SSE
/// `data:` line carrying a whole base64 image can be megabytes; we would rather
/// miss its usage than grow without bound.
const MAX_LINE_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub struct UsageScan {
    pending: Vec<u8>,
    usage: Option<Usage>,
    lines_dropped: usize,
}

impl UsageScan {
    /// Feed one raw chunk from the upstream stream.
    pub fn feed(&mut self, chunk: &[u8]) {


        self.pending.extend_from_slice(chunk);
        // Only complete lines are parseable; keep the trailing partial.
        while let Some(nl) = find_byte(&self.pending, b'\n') {
            let line = self.pending.drain(..=nl).collect::<Vec<u8>>();
            self.consume_line(&line);
        }
        if self.pending.len() > MAX_LINE_BYTES {
            self.lines_dropped += 1;
            self.pending.clear();
        }
    }

    /// Flush a final line that arrived without a trailing newline.
    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.consume_line(&line);
        }
    }

    fn consume_line(&mut self, raw: &[u8]) {
        let text = String::from_utf8_lossy(raw);
        let line = text.trim_end_matches(['\r', '\n']);
        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
            return;
        };
        if payload.is_empty() || payload == "[DONE]" {
            return;
        }
        if let Some(usage) = usage_in_frame(payload) {
            self.usage = Some(usage);
        }
    }

    pub fn usage(&self) -> Option<Usage> {
        self.usage.clone()
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    fn dropped_lines(&self) -> usize {
        self.lines_dropped
    }
}

fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    hay.iter().position(|b| *b == needle)
}

/// Parse one SSE payload as JSON and lift `usage` out of it. Accepts both the
/// OpenAI shape (`usage` at the top level) and a nested `message.usage`.
pub fn usage_in_frame(payload: &str) -> Option<Usage> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let raw = value
        .get("usage")
        .or_else(|| value.get("message").and_then(|m| m.get("usage")))?;
    let pick = |keys: [&str; 2]| {
        keys
            .iter()
            .find_map(|k| raw.get(*k).and_then(|v| v.as_i64()))
    };
    let usage = Usage {
        prompt_tokens: pick(["prompt_tokens", "input_tokens"]),
        completion_tokens: pick(["completion_tokens", "output_tokens"]),
    };
    (!usage.is_empty()).then_some(usage)
}

/// Non-streaming responses carry the same `usage` object in one JSON body.
pub fn usage_from_body(body: &[u8]) -> Option<Usage> {
    let payload = String::from_utf8_lossy(body);
    usage_in_frame(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(usage: &str) -> String {
        format!(r#"data: {{"id":"1","choices":[],"usage":{usage}}}"#)
    }

    #[test]
    fn picks_usage_from_a_single_complete_frame() {
        let mut scan = UsageScan::default();
        scan.feed(frame(r#"{"prompt_tokens":11,"completion_tokens":7}"#).as_bytes());
        assert_eq!(scan.usage(), None, "a frame without its newline is not complete yet");
        scan.feed(b"\n");
        let usage = scan.usage().unwrap();
        assert_eq!(usage.prompt_tokens, Some(11));
        assert_eq!(usage.completion_tokens, Some(7));
    }

    #[test]
    fn survives_a_frame_split_across_chunks() {
        let payload = frame(r#"{"prompt_tokens":3,"completion_tokens":4}"#);
        let bytes = payload.as_bytes();
        let mid = bytes.len() / 2;
        let mut scan = UsageScan::default();
        scan.feed(&bytes[..mid]);
        assert_eq!(scan.usage(), None, "half a line must not be parsed");
        scan.feed(&bytes[mid..]);
        scan.feed(b"\n");
        assert_eq!(scan.usage().unwrap().prompt_tokens, Some(3));
    }

    #[test]
    fn keeps_the_last_usage_frame_seen() {
        let mut scan = UsageScan::default();
        scan.feed(frame(r#"{"prompt_tokens":1,"completion_tokens":1}"#).as_bytes());
        scan.feed(b"\n");
        scan.feed(frame(r#"{"prompt_tokens":9,"completion_tokens":8}"#).as_bytes());
        scan.feed(b"\n");
        assert_eq!(scan.usage().unwrap().prompt_tokens, Some(9));
    }

    #[test]
    fn ignores_done_pings_content_and_broken_json() {
        let mut scan = UsageScan::default();
        for line in [
            "data: [DONE]\n",
            "data: \n",
            ": comment\n",
            "event: message\n",
            "data: not json\n",
            "\n",
            r#"data: {"choices":[{"delta":{"content":"hi"}}]}"#,
        ] {
            scan.feed(line.as_bytes());
        }
        scan.finish();
        assert_eq!(scan.usage(), None);
    }

    #[test]
    fn accepts_anthropic_style_and_nested_usage() {
        let mut scan = UsageScan::default();
        scan.feed(br#"data: {"usage":{"input_tokens":5,"output_tokens":6}}"#);
        scan.feed(b"\n");
        assert_eq!(scan.usage().unwrap(), Usage { prompt_tokens: Some(5), completion_tokens: Some(6) });
        scan.feed(br#"data: {"message":{"usage":{"prompt_tokens":1}}}"#);
        scan.feed(b"\n");
        assert_eq!(scan.usage().unwrap().prompt_tokens, Some(1));
    }

    #[test]
    fn usage_without_numbers_is_not_reported() {
        assert!(usage_in_frame(r#"{"usage":{"foo":1}}"#).is_none());
        assert!(usage_in_frame(r#"{"usage":{}}"#).is_none());
    }

    #[test]
    fn non_utf8_garbage_does_not_panic() {
        let mut scan = UsageScan::default();
        scan.feed(&[0xff, 0xfe, b'\n', 0x00, b'd', b'a', b't', b'a', b':', b' ', 0xc3, b'\n']);
        scan.finish();
        assert_eq!(scan.usage(), None);
    }

    #[test]
    fn a_giant_line_is_dropped_instead_of_buffered_forever() {
        let mut scan = UsageScan::default();
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..8 {
            scan.feed(&chunk);
        }
        assert!(scan.dropped_lines() > 0, "the oversized frame must be discarded");
        assert!(scan.pending_len() <= MAX_LINE_BYTES, "buffering must stay bounded");
        scan.feed(b"\n");
        assert_eq!(scan.usage(), None);
    }

    #[test]
    fn finish_flushes_a_trailing_line_without_newline() {
        let mut scan = UsageScan::default();
        scan.feed(frame(r#"{"prompt_tokens":2,"completion_tokens":2}"#).as_bytes());
        assert_eq!(scan.usage(), None);
        scan.finish();
        assert_eq!(scan.usage().unwrap().prompt_tokens, Some(2));
    }

    #[test]
    fn a_usage_frame_still_lands_after_interleaved_garbage() {
        let mut scan = UsageScan::default();
        scan.feed(b"data: \n\n\ndata: {\"choices\":[]}\n");
        scan.feed(frame(r#"{"prompt_tokens":6,"completion_tokens":6}"#).as_bytes());
        scan.feed(b"\n");
        assert_eq!(scan.usage().unwrap().prompt_tokens, Some(6));
    }

    #[test]
    fn usage_from_body_reads_a_plain_json_response() {
        let body = br#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3}}"#;
        assert_eq!(usage_from_body(body).unwrap().completion_tokens, Some(3));
        assert!(usage_from_body(b"not json").is_none());
    }
}
