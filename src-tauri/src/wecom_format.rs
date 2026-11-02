//! 出站正文的形态判定：哪些答复值得单独发一条 markdown 消息、气泡收尾怎么起标题。
//! 纯函数，无 IO，也不碰任何模型/网络状态。
//!
//! 背景：企微智能机器人的 `stream` 不是 markdown 类型，模型正文里的 `##` / `**` / 表格
//! 会原样露给用户；想真正渲染富文本只能单发一条 `msgtype=markdown` 消息。两者互斥，
//! 所以这里只负责判断"值不值得拆成两条"，发送顺序由调用方保证。

/// 企微 `markdown.content` 上限（UTF-8 字节，官方文档 20480）。
pub const MARKDOWN_MAX_BYTES: usize = 20_480;
/// 短回答留在流式气泡里更自然，不值得为它多开一条消息。
const MARKDOWN_MIN_CHARS: usize = 40;
/// 收尾气泡里的标题长度（按字符截，不是字节）。
const HEADLINE_MAX_CHARS: usize = 42;
/// 收尾气泡没有可用标题时的兜底。
const FALLBACK_HEADLINE: &str = "已完成";

/// 正文是否适合作为独立 markdown 消息发送。
///
/// 条件：非空、超过最短长度、不超上限、并且确实有块级结构（标题 / 列表 / 代码围栏 / 表格）。
/// 只有行内 `**` 的短句不拆——那种情况多一条消息只会更吵。
pub fn prefers_markdown(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.len() > MARKDOWN_MAX_BYTES {
        return false;
    }
    // 代码围栏哪怕很短也值得单独发：流式气泡里 ``` 会原样露出来。
    if trimmed.contains("```") {
        return true;
    }
    if trimmed.chars().count() < MARKDOWN_MIN_CHARS {
        return false;
    }
    trimmed.lines().any(|line| {
        let line = line.trim_start();
        let heading = line.starts_with('#');
        let bullet = line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ");
        let ordered = ordered_item(line);
        let quote = line.starts_with("> ");
        let table = line.starts_with('|') && line.matches('|').count() >= 2;
        heading || bullet || ordered || quote || table
    })
}

/// `1. ` / `2) ` 这类有序列表项（最多两位数字，避免把 `2026. 年` 误判成列表）。
fn ordered_item(line: &str) -> bool {
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 2 {
        return false;
    }
    let rest = &line[digits..];
    rest.starts_with(". ") || rest.starts_with(") ")
}

/// 收尾气泡用的一句话标题：取首个有内容的行，压掉 markdown 前缀标记后截断。
pub fn headline(text: &str) -> String {
    let mut title = String::new();
    for line in text.lines() {
        let mut line = line.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        // 去掉标题井号、引用箭头、列表符号与行内加粗标记。
        line = line.trim_start_matches('#').trim_start();
        line = line.trim_start_matches('>').trim_start();
        for prefix in ["- ", "* ", "+ "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                line = rest.trim_start();
                break;
            }
        }
        title = line.replace("**", "").replace("__", "").replace('`', "");
        if !title.is_empty() {
            break;
        }
    }
    let title = title.trim();
    if title.is_empty() {
        return FALLBACK_HEADLINE.to_owned();
    }
    if title.chars().count() <= HEADLINE_MAX_CHARS {
        return title.to_owned();
    }
    let mut out: String = title.chars().take(HEADLINE_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_or_flat_replies_stay_in_the_stream_bubble() {
        assert!(!prefers_markdown(""));
        assert!(!prefers_markdown("好的，已处理。"));
        // 只有行内加粗的短句不算结构
        assert!(!prefers_markdown("这个接口 **没问题**，可以直接回滚。"));
        // 超过上限（按字节）也不拆
        let huge = format!("## 标题\n{}", "字".repeat(MARKDOWN_MAX_BYTES));
        assert!(!prefers_markdown(&huge));
    }

    #[test]
    fn structured_replies_go_to_a_markdown_message() {
        let report = "## 商机流程\n\n1. 线索录入\n2. 商机评估\n\n- 审批节点\n- 转化节点\n\n| 阶段 | 负责人 |\n| --- | --- |\n| 录入 | 销售 |";
        assert!(prefers_markdown(report));
        assert!(prefers_markdown("```rust\nfn main() {}\n```"));
        assert!(prefers_markdown(
            "> 引用一段足够长的说明，用来确认引用块会被识别成块级结构，而不是被当成普通的一行文字处理。"
        ));
    }

    #[test]
    fn two_digit_years_are_not_list_items() {
        assert!(!ordered_item("2026. 年销量目标"));
        assert!(ordered_item("1. 第一条"));
        assert!(ordered_item("12) 第十二条"));
    }

    #[test]
    fn headline_strips_markup_and_truncates() {
        assert_eq!(headline("## 商机流程说明\n正文"), "商机流程说明");
        assert_eq!(headline("\n\n- **结论**：可以回滚\n细节"), "结论：可以回滚");
        assert_eq!(headline("```\nfn main() {}\n```"), "fn main() {}");
        assert_eq!(headline("\n```\n```\n"), FALLBACK_HEADLINE);
        let long = "一".repeat(HEADLINE_MAX_CHARS + 10);
        let short = headline(&long);
        assert_eq!(short.chars().count(), HEADLINE_MAX_CHARS);
        assert!(short.ends_with('…'));
    }
}
