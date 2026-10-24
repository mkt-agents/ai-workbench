//! Token budget for one agent turn. There is no tokenizer here: sizes are estimated
//! from characters (ASCII ~4 per token, CJK ~1 per token), which is close enough to
//! decide what to trim. Trimming escalates in three steps, cheapest loss first:
//!
//! 1. shrink the tool results (old logs / file dumps),
//! 2. drop the oldest conversation rounds (the newest task is always kept),
//! 3. clip the system prompt.
//!
//! Tool schemas are never trimmed: the model needs them to act at all.
//! Pure, no IO — the loop in `agent.rs` calls `fit_context` before every round.

use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Context budget per turn. Conservative on purpose: long contexts are slow and
/// expensive, and a small local model degrades far earlier than its window.
pub(crate) const CONTEXT_TOKENS: usize = 48_000;
/// Tokens always left for the model's own answer.
const ANSWER_RESERVE: usize = 8_000;
/// Newest user turns that level 2 never drops.
pub(crate) const KEEP_ROUNDS: usize = 6;
/// Level 1 never shrinks a tool result below this (it would stop being useful).
const MIN_RESULT_TOKENS: usize = 600;

/// 估算系数（×100，100 = 字符分类默认值）。每轮拿到 provider 的真实 usage 就校准一次，
/// 新旧对半取平均，防一轮异常数据把系数带飞。中英混合的会话里，这个系数比纯字符分类准。
static CALIBRATION_X100: AtomicUsize = AtomicUsize::new(100);

/// Actual tokens / estimated tokens for one round. Ratios outside 40%–300% are refused:
/// a missing usage field or a router that reports characters would poison the factor.
pub(crate) fn calibrate_from_usage(estimated: usize, actual: usize) {
    if estimated < 200 || actual == 0 {
        return;
    }
    let ratio = actual * 100 / estimated;
    if !(40..=300).contains(&ratio) {
        return;
    }
    let old = CALIBRATION_X100.load(Ordering::Relaxed);
    CALIBRATION_X100.store((old + ratio) / 2, Ordering::Relaxed);
}

/// Quarter-token cost of one char: ASCII averages ~4 chars/token, CJK ~1 char/token.
fn char_cost(c: char) -> usize {
    if (c as u32) < 128 {
        1
    } else {
        4
    }
}

/// Rough token count of a piece of text, scaled by the calibrated factor.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    let quarters: usize = text.chars().map(char_cost).sum();
    let base = quarters.div_ceil(4);
    (base * CALIBRATION_X100.load(Ordering::Relaxed) + 50) / 100
}

fn content_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    out.push_str(t);
                } else if let Some(t) = part.get("content").and_then(|v| v.as_str()) {
                    out.push_str(t);
                } else if let Some(t) = part.get("thinking").and_then(|v| v.as_str()) {
                    out.push_str(t);
                } else if part.get("type").and_then(|v| v.as_str()) == Some("image") {
                    // An image's cost depends on its resolution; charge a flat estimate.
                    out.push_str(&" ".repeat(3_200));
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// Rough token cost of one message, including protocol overhead per message.
pub(crate) fn message_tokens(message: &Value) -> usize {
    estimate_tokens(&content_text(message)) + 4
}

pub(crate) fn messages_tokens(messages: &[Value]) -> usize {
    messages.iter().map(message_tokens).sum()
}

/// Characters the round actually carries (system + every message part, images included).
/// Shown on the answer source line; the model sees tokens, the user reads characters.
pub(crate) fn context_chars(system: &str, messages: &[Value]) -> usize {
    system.chars().count() + messages.iter().map(|m| content_text(m).chars().count()).sum::<usize>()
}

fn is_tool_result(message: &Value) -> bool {
    if message.get("role").and_then(|v| v.as_str()) == Some("tool") {
        return true;
    }
    message
        .get("content")
        .and_then(|v| v.as_array())
        .map(|parts| parts.iter().any(|p| p.get("type").and_then(|v| v.as_str()) == Some("tool_result")))
        .unwrap_or(false)
}

/// A real user turn: the prompt itself, not a carrier of tool results.
fn is_user_turn(message: &Value) -> bool {
    message.get("role").and_then(|v| v.as_str()) == Some("user") && !is_tool_result(message)
}

/// Head + tail cut with a note, so the model still sees how the output started and ended.
fn head_tail(text: &str, max_tokens: usize) -> String {
    let quarters = max_tokens.saturating_mul(4);
    let head_budget = quarters * 2 / 3;
    let tail_budget = quarters / 3;

    let mut used = 0;
    let mut head_end = 0;
    for (i, c) in text.char_indices() {
        let cost = char_cost(c);
        if used + cost > head_budget {
            head_end = i;
            break;
        }
        used += cost;
        head_end = i + c.len_utf8();
    }
    let mut used = 0;
    let mut tail_start = text.len();
    for (i, c) in text.char_indices().rev() {
        let cost = char_cost(c);
        if used + cost > tail_budget {
            tail_start = i + c.len_utf8();
            break;
        }
        used += cost;
        tail_start = i;
    }
    let tail_start = tail_start.max(head_end);
    let cut = text.chars().count() - text[..head_end].chars().count() - text[tail_start..].chars().count();
    format!(
        "{}\n\n…(上下文压缩：这里省略了 {cut} 字符，需要细节请分步查)\n\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

/// Shrink one tool-result message in place; keeps tool_use / tool_result pairing intact.
fn shrink_tool_message(message: &mut Value, max_tokens: usize) -> bool {
    if let Some(Value::String(text)) = message.get_mut("content") {
        if estimate_tokens(text) > max_tokens {
            *text = head_tail(text, max_tokens);
            return true;
        }
        return false;
    }
    let Some(Value::Array(parts)) = message.get_mut("content") else {
        return false;
    };
    let mut changed = false;
    for part in parts.iter_mut() {
        if part.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
            continue;
        }
        match part.get_mut("content") {
            Some(Value::String(text)) => {
                if estimate_tokens(text) > max_tokens {
                    *text = head_tail(text, max_tokens);
                    changed = true;
                }
            }
            Some(Value::Array(inner)) => {
                for item in inner.iter_mut() {
                    if let Some(Value::String(text)) = item.get_mut("text") {
                        if estimate_tokens(text) > max_tokens {
                            *text = head_tail(text, max_tokens);
                            changed = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    changed
}

/// Level 1: halve the biggest tool results until their total fits `cap`.
pub(crate) fn shrink_tool_results(messages: &mut [Value], cap: usize) -> usize {
    let mut shrunk = 0;
    loop {
        let sizes: Vec<(usize, usize)> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| is_tool_result(m))
            .map(|(i, m)| (i, message_tokens(m)))
            .collect();
        if sizes.iter().map(|(_, t)| *t).sum::<usize>() <= cap {
            return shrunk;
        }
        let Some(&(index, size)) = sizes.iter().max_by_key(|(_, t)| *t) else {
            return shrunk;
        };
        let target = (size / 2).max(MIN_RESULT_TOKENS);
        if target >= size || !shrink_tool_message(&mut messages[index], target) {
            return shrunk;
        }
        shrunk += 1;
    }
}

/// Level 2: drop the oldest rounds, keeping the newest `keep` user turns and a leading
/// system message. Cutting at a real user turn keeps both protocols valid — an
/// Anthropic `tool_result` is never separated from its `tool_use`.
pub(crate) fn drop_old_rounds(messages: &mut Vec<Value>, keep: usize) -> usize {
    let base = usize::from(
        messages
            .first()
            .map(|m| m.get("role").and_then(|v| v.as_str()) == Some("system"))
            .unwrap_or(false),
    );
    let starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .skip(base)
        .filter(|(_, m)| is_user_turn(m))
        .map(|(i, _)| i)
        .collect();
    let keep = keep.max(1);
    if starts.len() <= keep {
        return 0;
    }
    let cut = starts[starts.len() - keep];
    messages.drain(base..cut).count()
}

/// Level 3: clip the system prompt, keeping head and tail (the policy text trails it).
pub(crate) fn clip_system(system: &str, cap: usize) -> String {
    if estimate_tokens(system) <= cap {
        return system.to_string();
    }
    head_tail(system, cap)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Trim {
    /// Estimated tokens before / after the trim (system + messages).
    pub before: usize,
    pub after: usize,
    pub shrunk_results: usize,
    pub dropped_messages: usize,
    pub system_clipped: bool,
}

impl Trim {
    pub(crate) fn any(&self) -> bool {
        self.shrunk_results > 0 || self.dropped_messages > 0 || self.system_clipped
    }
}

/// Fit one turn into `limit` tokens. `fixed_tokens` is what the prompt cannot give up
/// (tool schemas); `limit` is the whole context budget.
pub(crate) fn fit_context(
    system: &mut String,
    messages: &mut Vec<Value>,
    fixed_tokens: usize,
    limit: usize,
    keep_rounds: usize,
) -> Trim {
    let limit = limit.max(4_000);
    let room = limit.saturating_sub(fixed_tokens + ANSWER_RESERVE).max(2_000);
    let mut trim = Trim { before: estimate_tokens(system) + messages_tokens(messages), ..Default::default() };

    // Level 1: tool results may take at most 60% of the conversation room.
    let cap = (room * 3 / 5).max(MIN_RESULT_TOKENS);
    trim.shrunk_results = shrink_tool_results(messages, cap);

    // Level 2: still too big → drop the oldest rounds, down to a single user turn.
    let mut keep = keep_rounds.max(1);
    while estimate_tokens(system) + messages_tokens(messages) > room && keep >= 1 {
        let dropped = drop_old_rounds(messages, keep);
        trim.dropped_messages += dropped;
        if dropped == 0 {
            if keep == 1 {
                break;
            }
            keep -= 1;
        }
    }

    // Level 3: still too big → clip the system prompt to whatever room is left.
    if estimate_tokens(system) + messages_tokens(messages) > room {
        let left = room.saturating_sub(messages_tokens(messages)).max(1_000);
        let clipped = clip_system(system, left);
        if clipped != *system {
            *system = clipped;
            trim.system_clipped = true;
        }
    }
    trim.after = estimate_tokens(system) + messages_tokens(messages);
    trim
}

/// 校准系数是进程级全局，断言绝对 token 数的测试要独占它，免得并行测试互相干扰。
#[cfg(test)]
static CAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod chars_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_chars_counts_system_and_both_content_shapes() {
        let messages = vec![
            json!({ "role": "user", "content": "abcd" }),
            json!({ "role": "user", "content": [{ "type": "tool_result", "content": "ef" }] }),
        ];
        assert_eq!(context_chars("xy", &messages), 8);
        assert_eq!(context_chars("", &[]), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_msg(text: &str) -> Value {
        json!({ "role": "tool", "content": text })
    }

    fn anthropic_tool_msg(text: &str) -> Value {
        json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t", "content": text }] })
    }

    #[test]
    fn tokens_are_estimated_per_character_class() {
        let _guard = CAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        CALIBRATION_X100.store(100, Ordering::Relaxed);
        assert_eq!(estimate_tokens(""), 0);
        // 4 ASCII chars ≈ 1 token
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        // CJK ≈ 1 char per token
        assert_eq!(estimate_tokens("上下文"), 3);
        assert!(estimate_tokens("中文") > estimate_tokens("ab"));
        assert!(message_tokens(&tool_msg("abcd")) > estimate_tokens("abcd"), "每条消息有固定开销");
    }

    /// 真实 usage 会把估算系数拉向实际值；离谱的比率被拒收，太小的样本也不收。
    #[test]
    fn usage_calibrates_the_estimate_factor() {
        let _guard = CAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        CALIBRATION_X100.store(100, Ordering::Relaxed);
        // 基线：8000 个 ASCII 字符 ≈ 2000 token
        assert_eq!(estimate_tokens(&"a".repeat(8_000)), 2_000);
        // 实际总是估的 2 倍 → 系数向 200 平滑：(100+200)/2 = 150
        calibrate_from_usage(2_000, 4_000);
        assert_eq!(estimate_tokens(&"a".repeat(8_000)), 3_000);
        // 再来一次同样的偏差 → (150+200)/2 = 175（对半收敛，不一步到位）
        calibrate_from_usage(2_000, 4_000);
        assert_eq!(estimate_tokens(&"a".repeat(8_000)), 3_500);
        // 离谱比率（路由报了字符数）不收：系数停在 175
        calibrate_from_usage(2_000, 400_000);
        assert_eq!(estimate_tokens(&"a".repeat(8_000)), 3_500);
        // 样本太小不收
        calibrate_from_usage(10, 999);
        assert_eq!(estimate_tokens(&"a".repeat(8_000)), 3_500);
        CALIBRATION_X100.store(100, Ordering::Relaxed);
    }

    #[test]
    fn level_one_shrinks_the_biggest_tool_results_and_keeps_their_shape() {
        let big = "x".repeat(40_000);
        let mut messages = vec![
            json!({ "role": "user", "content": "看看日志" }),
            tool_msg(&big),
            anthropic_tool_msg(&big),
        ];
        let shrunk = shrink_tool_results(&mut messages, 2_000);
        // 一次减半不够就再减半，直到总量进预算或碰到下限
        assert!(shrunk >= 2);
        let first = messages[1]["content"].as_str().unwrap();
        assert!(first.len() < big.len());
        assert!(first.starts_with("xxx"));
        assert!(first.contains("上下文压缩"), "截断要说明原因");
        // The Anthropic block keeps its id, only the text got shorter.
        assert_eq!(messages[2]["content"][0]["tool_use_id"], json!("t"));
        let inner = messages[2]["content"][0]["content"].as_str().unwrap();
        assert!(inner.len() < big.len());
        // Already small enough: nothing changes.
        let mut small = vec![tool_msg("ok")];
        assert_eq!(shrink_tool_results(&mut small, 1_000), 0);
        assert_eq!(small[0]["content"], json!("ok"));
    }

    #[test]
    fn level_two_drops_oldest_rounds_and_keeps_tool_pairing() {
        let mut messages = vec![
            json!({ "role": "user", "content": "旧问题 1" }),
            json!({ "role": "assistant", "content": "答 1" }),
            json!({ "role": "user", "content": "旧问题 2" }),
            json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": "a", "name": "read_file", "input": {} }] }),
            anthropic_tool_msg("结果 a"),
            json!({ "role": "assistant", "content": "答 2" }),
            json!({ "role": "user", "content": "当前问题" }),
            json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": "b", "name": "grep", "input": {} }] }),
            anthropic_tool_msg("结果 b"),
        ];
        let dropped = drop_old_rounds(&mut messages, 1);
        assert_eq!(dropped, 6);
        // Starts at a real user turn: no orphaned `tool_use` / `tool_result`.
        assert_eq!(messages[0]["content"], json!("当前问题"));
        assert_eq!(messages.len(), 3);
        // One user turn is the floor.
        assert_eq!(drop_old_rounds(&mut messages, 1), 0);
    }

    #[test]
    fn level_two_keeps_the_leading_system_message() {
        let mut messages = vec![
            json!({ "role": "system", "content": "系统" }),
            json!({ "role": "user", "content": "旧" }),
            json!({ "role": "assistant", "content": "答" }),
            json!({ "role": "user", "content": "新" }),
        ];
        assert_eq!(drop_old_rounds(&mut messages, 1), 2);
        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(messages[1]["content"], json!("新"));
    }

    #[test]
    fn a_small_turn_is_left_alone() {
        let mut system = "你是助手".to_string();
        let mut messages = vec![json!({ "role": "user", "content": "你好" })];
        let trim = fit_context(&mut system, &mut messages, 500, 48_000, 6);
        assert!(!trim.any());
        assert_eq!(trim.before, trim.after);
        assert_eq!(system, "你是助手");
    }

    #[test]
    fn oversized_turns_end_up_under_the_limit() {
        let _guard = CAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        CALIBRATION_X100.store(100, Ordering::Relaxed);
        let mut system = "规则".repeat(2_000);
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(json!({ "role": "user", "content": format!("问题 {i} {}", "问".repeat(3_000)) }));
            history.push(json!({ "role": "assistant", "content": format!("答 {i}") }));
        }
        history.push(tool_msg(&"日志行\n".repeat(20_000)));
        let mut messages = history;
        let trim = fit_context(&mut system, &mut messages, 4_000, 20_000, 3);
        assert!(trim.any());
        assert!(trim.after <= 20_000, "裁剪后 {} 应回到上限内", trim.after);
        assert!(trim.before > trim.after);
        // The newest turn survives.
        assert_eq!(messages.last().unwrap()["role"], json!("tool"));
        assert!(
            messages.iter().any(|m| m["content"].as_str().map(|s| s.starts_with("问题 19")).unwrap_or(false)),
            "最新一轮的用户提问必须留下"
        );
    }
}
