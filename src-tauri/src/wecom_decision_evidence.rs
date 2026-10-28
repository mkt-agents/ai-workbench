//! Pure, bounded evidence helpers. Decision signals request explanation, never prove a root cause.
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// A business decision worth inspecting (including zero results / false / skip).
/// Generic zero/false values, unrelated exceptions and WARN alone are not decisions.
pub fn decision_gap(text: &str) -> bool {
    static SIGNAL: OnceLock<regex::Regex> = OnceLock::new();
    SIGNAL.get_or_init(|| regex::Regex::new(concat!(
        r"(?ix)\bBusinessException\b|\bSTATUS_FILTERED_EMPTY\b|\bskip(?:ped|ping)?\b|\bnot\s+executed\b|",
        r"(?:商品|报价|匹配|过滤|候选|有效|结果)[^\n]{0,12}数量\s*[=:：为]\s*0\b|",
        r"\b[a-z_]*successflag\s*[=:：]\s*false\b|",
        r"跳过|未执行|未触发|过滤(?:后|结果|数量|个数|条数)?[^\n]{0,32}(?:[=:：]\s*0\b|为\s*0\b|剩余\s*0\b)|",
        r"(?:filter(?:ed)?(?:count|size|result)?|eligible(?:count)?|matched(?:count)?|result(?:count|size)|remaining(?:count)?)\s*[=:：]\s*0\b|",
        r"(?:allow(?:ed)?|eligible|enabled|should[a-z_]*|can[a-z_]*|decision|accepted|success|result)\s*[=:：]\s*false\b|",
        r"(?:决策|判定|允许|执行条件|校验结果)[^\n]{0,16}(?:false|不满足|未通过)"
    )).expect("decision signal regex")).is_match(text)
}

pub fn digest(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// Keep identity at the head, reason/warning neighbourhoods and the tail.
/// `limit` bounds retained source characters, not the provenance labels. Offsets are
/// Unicode scalar offsets, end-exclusive; digest always covers the original UTF-8 bytes.
pub fn excerpt(content: &str, limit: usize) -> String {
    let chars: Vec<char> = content.chars().collect();
    let len = chars.len();
    if len <= limit {
        return content.to_owned();
    }
    let mut ranges = Vec::new();
    let head = (limit / 5).min(len);
    let tail = (limit / 5).min(len.saturating_sub(head));
    ranges.push((0, head));
    ranges.push((len - tail, len));
    let mut remaining = limit.saturating_sub(head + tail);
    let lower = content.to_ascii_lowercase();
    // Reason markers before warning: the latter often only appears in the header.
    let mut centers = Vec::new();
    for marker in [
        "caused by",
        "cause",
        "原因",
        "reason",
        "warning",
        "warn",
        "businessexception",
    ] {
        for (byte, _) in lower.match_indices(marker).take(3) {
            let at = content[..byte].chars().count();
            if at >= head && at < len - tail && !centers.iter().any(|c: &usize| c.abs_diff(at) < 64)
            {
                centers.push(at);
            }
        }
    }
    centers.truncate(3);
    if centers.is_empty() {
        ranges[0].1 += remaining;
    } else {
        let count = centers.len();
        for (index, center) in centers.into_iter().enumerate() {
            let width = remaining / (count - index);
            let start = center
                .saturating_sub(width / 5)
                .min(len.saturating_sub(width));
            ranges.push((start, start + width));
            remaining -= width;
        }
    }
    ranges.retain(|(start, end)| start < end);
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut().filter(|last| last.1 >= start) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    let mut out = format!(
        "[excerpt original_length={len} hash=sha256:{} truncated=true offset_unit=char]\n",
        digest(content)
    );
    for (start, end) in merged {
        out.push_str(&format!(
            "[offset={start}..{end}] {}\n",
            chars[start..end].iter().collect::<String>()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decisions_are_signals_not_generic_false_or_zero() {
        for text in [
            "filteredCount=0",
            "过滤后数量：0",
            "shouldQuote=false",
            "skip auto quote",
            "BusinessException: invalid quote",
        ] {
            assert!(decision_gap(text), "{text}");
        }
        for text in [
            "latency=0",
            "debug=false",
            "WARN healthy",
            "NullPointerException",
            "filteredCount=10",
        ] {
            assert!(!decision_gap(text), "{text}");
        }
    }
    #[test]
    fn excerpt_retains_late_reason_identity_and_tail_with_provenance() {
        let content = format!(
            "request=abc logger=Quote {} WARN decision {} 原因：库存不足 {} END",
            "x".repeat(900),
            "y".repeat(400),
            "z".repeat(600)
        );
        let result = excerpt(&content, 700);
        assert!(result.contains("request=abc"));
        assert!(result.contains("库存不足"));
        assert!(result.contains("WARN decision"));
        assert!(result.contains("END"));
        assert!(result.contains("offset="));
        assert!(result.contains(&digest(&content)));
        assert!(result.contains(&format!("original_length={}", content.chars().count())));
    }
    #[test]
    fn excerpt_zero_and_unicode_are_well_formed() {
        assert!(excerpt("中文原因🙂结束", 0).contains("truncated=true"));
        assert_eq!(excerpt("原文", 2), "原文");
        assert!(excerpt(&"中文🙂".repeat(800), 31).contains("offset_unit=char"));
    }
}
