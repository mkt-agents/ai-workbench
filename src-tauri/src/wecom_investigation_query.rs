//! 确定性的日志查询/追问解析；无 IO，时间由调用方传入。
//! 不把中文症状整句当日志字面量；未识别的时间表达式返回澄清错误。
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone};
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationQuery {
    pub start: String,
    pub end: String,
    pub timezone_offset_minutes: i32,
    pub keywords: Vec<String>,
    pub exclude_keyword: String,
    /// false = AND，true = OR；执行层需明确支持 OR，不应静默降为 AND。
    pub match_any: bool,
    pub service: Option<String>,
    pub trace_id: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    /// 一起始页码。
    pub page: i64,
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static investigation query regex")
}

pub fn is_followup(text: &str) -> bool {
    [
        "扩大",
        "扩展",
        "放大",
        "只看",
        "仅看",
        "下一页",
        "下页",
        "继续",
        "再查",
        "换订单",
        "换个订单",
        "换一个订单",
        "改查",
        "排除",
        "不包含",
    ]
    .iter()
    .any(|word| text.contains(word))
        || re(r"(?i)^\s*(?:keywords?|关键词|关键字|service|服务名|服务|traceid|trace_id|level|级别)\s*[=:：]")
            .is_match(text)
}

fn expanding(text: &str) -> bool {
    ["扩大", "扩展", "放大"].iter().any(|s| text.contains(s))
}

/// previous 仅在明确追问时继承；独立新问题默认重新取最近 4h。
/// 无时区日期按 now 的固定偏移解释，不隐式查询主机时区。
pub fn parse_query(
    text: &str,
    previous: Option<&InvestigationQuery>,
    now: DateTime<FixedOffset>,
) -> Result<InvestigationQuery, String> {
    let followup = is_followup(text) && previous.is_some();
    if previous.is_none()
        && (expanding(text)
            || ["下一页", "下页", "继续下游"]
                .iter()
                .any(|w| text.contains(w)))
    {
        return Err("此追问没有上一条查询，请补充查询条件。".into());
    }
    let mut query = if followup {
        previous.cloned().ok_or("缺少上一条查询")?
    } else {
        InvestigationQuery {
            start: subtract(now, Duration::hours(4))?.to_rfc3339(),
            end: now.to_rfc3339(),
            timezone_offset_minutes: now.offset().local_minus_utc() / 60,
            keywords: Vec::new(),
            exclude_keyword: String::new(),
            match_any: false,
            service: None,
            trace_id: None,
            level: None,
            page: 1,
        }
    };
    // 即使反序来自持久化的 previous，也在纯函数边界验证。
    validate_window(&query)?;
    let mut remaining = text.to_string();
    let fields = extract_fields(text);
    let mut explicit_keywords = None;
    let mut explicit_service = None;
    let mut explicit_trace = None;
    let mut explicit_exclude = None;
    let mut explicit_level = None;
    for field in &fields {
        match field.kind.as_str() {
            "keyword" => explicit_keywords = Some(keyword_values(&field.value)),
            "service" => explicit_service = Some(unquote(&field.value).to_string()),
            "trace" => explicit_trace = Some(unquote(&field.value).to_string()),
            "exclude" => explicit_exclude = Some(unquote(&field.value).to_string()),
            "level" => explicit_level = Some(parse_level(&field.value)?),
            _ => {}
        }
    }
    // 保持所有捕获的 UTF-8 坐标：按原字节长度换成空格。
    for field in fields.iter().rev() {
        remaining.replace_range(field.start..field.end, &" ".repeat(field.end - field.start));
    }
    // 自然语言过滤在语义提取前消耗，防止 timeout/ERROR 进入正向关键词。
    let natural = re(
        r#"(?:排除|不包含)\s*(?:"([^"]*)"|'([^']*)'|“([^”]*)”|([A-Za-z0-9_.-]+))|(?:只看|仅看|只要)(?:错误|error)(?:日志)?"#,
    );
    let natural_matches: Vec<_> = natural.captures_iter(&remaining).collect();
    let mut natural_ranges = Vec::new();
    let quotes = re(r#""[^"]*"|'[^']*'|“[^”]*”"#);
    let quoted_ranges: Vec<_> = quotes
        .find_iter(&remaining)
        .map(|m| (m.start(), m.end()))
        .collect();
    for cap in natural_matches {
        let m = cap.get(0).unwrap();
        if quoted_ranges
            .iter()
            .any(|(start, end)| m.start() >= *start && m.start() < *end)
        {
            continue;
        }
        if let Some(value) = (1..=4).find_map(|i| cap.get(i)) {
            if explicit_exclude.is_none() {
                explicit_exclude = Some(value.as_str().to_string());
            }
        } else if explicit_level.is_none() {
            explicit_level = Some(Some("ERROR".into()));
        }
        natural_ranges.push((m.start(), m.end()));
    }
    for (start, end) in natural_ranges.into_iter().rev() {
        remaining.replace_range(start..end, &" ".repeat(end - start));
    }
    let mut time_text = remaining.clone();
    let quote_pattern = re(r#""[^"]*"|'[^']*'|“[^”]*”"#);
    let quote_ranges: Vec<_> = quote_pattern
        .find_iter(&remaining)
        .map(|m| (m.start(), m.end()))
        .collect();
    for (start, end) in quote_ranges.into_iter().rev() {
        time_text.replace_range(start..end, &" ".repeat(end - start));
    }
    let (window, time_ranges) = parse_window(&time_text, &query, now, followup)?;
    if let Some((start, end)) = window {
        if start >= end {
            return Err("开始时间必须早于结束时间；跨午夜请写明两端日期。".into());
        }
        // OAP 的解释时区由调用方配置，与用户给出的 RFC3339 表示偏移独立。
        query.timezone_offset_minutes = now.offset().local_minus_utc() / 60;
        query.start = start.to_rfc3339();
        query.end = end.to_rfc3339();
    }
    for (start, end) in time_ranges.into_iter().rev() {
        remaining.replace_range(start..end, &" ".repeat(end - start));
    }
    let trace = explicit_trace.or_else(|| crate::wecom_intent::extract_trace_id(&remaining));
    let service = explicit_service.or_else(|| extract_service_filter(&remaining));
    let quotes = quoted_values(&remaining);
    // 引号短语作为完整字面量，不再从内部重复推测 error/ID。
    let quote_ranges: Vec<_> = quote_pattern
        .find_iter(&remaining)
        .map(|m| (m.start(), m.end()))
        .collect();
    for (start, end) in quote_ranges.into_iter().rev() {
        remaining.replace_range(start..end, &" ".repeat(end - start));
    }
    let analysis = crate::wecom_intent::analyze(&remaining);
    let ids = crate::wecom_intent::extract_identifiers(&remaining, trace.as_deref());
    let changed_subject =
        followup && !ids.is_empty() && ids.iter().any(|id| !query.keywords.contains(id));
    // 新订单独立于之前自动追查到的 trace；同时丢掉旧订单/错误关键词。
    if changed_subject
        || (followup
            && ["换订单", "换个订单", "换一个订单", "改查"]
                .iter()
                .any(|s| text.contains(s)))
    {
        query.trace_id = None;
        query.keywords.clear();
        query.exclude_keyword.clear();
        query.match_any = false;
    }
    let mut keywords = explicit_keywords.unwrap_or_else(|| {
        let mut out = quotes;
        for id in ids {
            push_unique(&mut out, id);
        }
        for value in analysis.keywords {
            // 旧 analyze 的兜底可能是自然语言整句或服务名；这里只接纳错误信号。
            if is_error_keyword(&value) {
                push_unique(&mut out, value);
            }
        }
        for token in remaining.split(|c: char| !c.is_ascii_alphanumeric()) {
            if token.eq_ignore_ascii_case("error") {
                push_unique(&mut out, token.to_string());
            }
        }
        out
    });
    if let Some(t) = trace.as_ref() {
        keywords.retain(|k| k != t);
    }
    if !keywords.is_empty() || fields.iter().any(|f| f.kind == "keyword") {
        if followup && trace.is_none() && query.keywords != keywords {
            query.trace_id = None;
        }
        query.keywords = keywords;
        query.match_any = false;
    }
    if let Some(value) = explicit_exclude {
        query.exclude_keyword = value;
    }
    if let Some(value) = explicit_level {
        query.level = value;
    }
    if let Some(value) = service {
        query.service = if value.is_empty() { None } else { Some(value) };
    }
    if let Some(value) = trace {
        query.trace_id = if value.is_empty() { None } else { Some(value) };
    }
    if text.contains("任一") || text.contains("或者") || re(r"(?i)\bOR\b").is_match(text) {
        query.match_any = true;
    } else if text.contains("全部")
        || text.contains("同时包含")
        || re(r"(?i)\bAND\b").is_match(text)
    {
        query.match_any = false;
    }
    // 仅无过滤变化的翻页继承页码；其它追问从第 1 页重新取。
    let next_page = text.contains("下一页") || text.contains("下页");
    let unchanged = previous
        .map(|p| {
            p.start == query.start
                && p.end == query.end
                && p.keywords == query.keywords
                && p.exclude_keyword == query.exclude_keyword
                && p.service == query.service
                && p.trace_id == query.trace_id
                && p.match_any == query.match_any
                && p.level == query.level
        })
        .unwrap_or(false);
    query.page = if next_page && unchanged {
        query.page.checked_add(1).ok_or("页码超出支持范围")?
    } else if followup && unchanged {
        query.page
    } else {
        1
    };
    validate_window(&query)?;
    Ok(query)
}

fn validate_window(query: &InvestigationQuery) -> Result<(), String> {
    let start =
        DateTime::parse_from_rfc3339(&query.start).map_err(|_| "上一条查询开始时间格式错误")?;
    let end = DateTime::parse_from_rfc3339(&query.end).map_err(|_| "上一条查询结束时间格式错误")?;
    if start >= end {
        return Err("开始时间必须早于结束时间。".into());
    }
    if query.page < 1 {
        return Err("页码必须大于零。".into());
    }
    Ok(())
}

fn subtract(
    time: DateTime<FixedOffset>,
    duration: Duration,
) -> Result<DateTime<FixedOffset>, String> {
    time.checked_sub_signed(duration)
        .ok_or_else(|| "时间范围超出支持范围。".into())
}

struct Field {
    kind: String,
    value: String,
    start: usize,
    end: usize,
}
fn extract_fields(text: &str) -> Vec<Field> {
    let pattern = re(
        r"(?i)(exclude_keyword|exclude|排除关键词|排除关键字|排除|不包含|keywords?|关键词|关键字|service|服务名|服务|应用|trace_id|traceid|trace id|level|级别)\s*[=:：]\s*",
    );
    let matches: Vec<_> = pattern.captures_iter(text).collect();
    let mut out = Vec::new();
    for (index, cap) in matches.iter().enumerate() {
        let m = cap.get(0).unwrap();
        // 防止 myservice= 被当成 service=。
        if m.start() > 0 && text.as_bytes()[m.start() - 1].is_ascii_alphanumeric() {
            continue;
        }
        let limit = matches
            .get(index + 1)
            .and_then(|c| c.get(0))
            .map(|m| m.start())
            .unwrap_or(text.len());
        let rest = &text[m.end()..limit];
        let label = cap[1].to_ascii_lowercase();
        let kind = match label.as_str() {
            "keyword" | "keywords" | "关键词" | "关键字" => "keyword",
            "service" | "服务名" | "服务" | "应用" => "service",
            "trace_id" | "traceid" | "trace id" => "trace",
            "level" | "级别" => "level",
            _ => "exclude",
        };
        // 引号短语支持空格；service/trace 非引号值只读一个 ASCII token。
        let value_len = if kind == "service" || kind == "trace" || kind == "level" {
            leading_value_len(rest)
        } else {
            keyword_field_len(rest)
        };
        let value = rest[..value_len]
            .trim()
            .trim_end_matches(['，', ','])
            .trim()
            .to_string();
        out.push(Field {
            kind: kind.into(),
            value,
            start: m.start(),
            end: m.end() + value_len,
        });
    }
    out
}
// 未加引号的关键词字段在分号、换行或明确时间表达式前终止。
// 引号内部的时间/分号始终属于字面量。
fn keyword_field_len(rest: &str) -> usize {
    let temporal = re(
        r"^(?:今天|昨天|昨晚|今晚|最近|过去|近[0-9零一二两三四五六七八九十]|[0-9]{4}-[0-9]{2}-[0-9]{2}|[0-9]{1,2}:[0-9]{2}|[0-9零一二两三四五六七八九十]+点)",
    );
    let mut closing = None;
    for (index, ch) in rest.char_indices() {
        if let Some(close) = closing {
            if ch == close {
                closing = None;
            }
            continue;
        }
        closing = match ch {
            '"' => Some('"'),
            '\'' => Some('\''),
            '“' => Some('”'),
            _ => None,
        };
        if closing.is_some() {
            continue;
        }
        if matches!(ch, ';' | '；' | '\n') {
            return index;
        }
        let boundary = index == 0
            || rest[..index]
                .chars()
                .next_back()
                .map(|c| c.is_whitespace() || matches!(c, ',' | '，'))
                .unwrap_or(false);
        if boundary && temporal.is_match(&rest[index..]) {
            return index;
        }
    }
    rest.len()
}
fn leading_value_len(rest: &str) -> usize {
    if let Some(first) = rest.chars().next() {
        let close = match first {
            '"' => Some('"'),
            '\'' => Some('\''),
            '“' => Some('”'),
            _ => None,
        };
        if let Some(close) = close {
            if let Some(pos) = rest[first.len_utf8()..].find(close) {
                return first.len_utf8() + pos + close.len_utf8();
            }
        }
    }
    rest.char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .map(|(i, _)| i)
        .unwrap_or(rest.len())
}
fn unquote(value: &str) -> &str {
    value.trim().trim_matches(['"', '\'', '“', '”']).trim()
}
fn quoted_values(text: &str) -> Vec<String> {
    re(r#""([^"]+)"|'([^']+)'|“([^”]+)”"#)
        .captures_iter(text)
        .filter_map(|c| {
            (1..=3)
                .find_map(|i| c.get(i))
                .map(|m| m.as_str().trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect()
}
fn keyword_values(text: &str) -> Vec<String> {
    let token = re(r#""([^"]*)"|'([^']*)'|“([^”]*)”|([^\s,，、]+)"#);
    let mut out = Vec::new();
    for cap in token.captures_iter(text) {
        let value = (1..=4).find_map(|i| cap.get(i)).unwrap().as_str();
        for part in if cap.get(4).is_some() {
            value.split("或者").collect::<Vec<_>>()
        } else {
            vec![value]
        } {
            if !["AND", "OR", "任一", "全部", "和"]
                .iter()
                .any(|w| part.eq_ignore_ascii_case(w))
            {
                push_unique(&mut out, part.to_string());
            }
        }
    }
    out
}
fn push_unique(out: &mut Vec<String>, value: String) {
    let value = value.trim().to_string();
    if !value.is_empty() && !out.iter().any(|v| v.eq_ignore_ascii_case(&value)) {
        out.push(value);
    }
}
fn parse_level(value: &str) -> Result<Option<String>, String> {
    let value = unquote(value).to_ascii_uppercase();
    if value.is_empty() || value == "ALL" {
        return Ok(None);
    }
    if ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"].contains(&value.as_str()) {
        Ok(Some(value))
    } else {
        Err("日志级别请使用 TRACE/DEBUG/INFO/WARN/ERROR/FATAL 或 ALL。".into())
    }
}
fn is_error_keyword(value: &str) -> bool {
    value.ends_with("Exception")
        || value.ends_with("Error")
        || [
            "500",
            "502",
            "503",
            "504",
            "429",
            "400",
            "401",
            "403",
            "404",
            "timeout",
            "OutOfMemory",
            "deadlock",
            "connection refused",
            "connection reset",
            "NullPointer",
            "error",
        ]
        .iter()
        .any(|w| value.eq_ignore_ascii_case(w))
}
fn extract_service_filter(text: &str) -> Option<String> {
    // 「只看服务 payment」「只看 payment 服务」允许没有后缀的明确 ASCII 服务名。
    for pattern in [
        r"(?:只看|仅看)\s*(?:服务|应用)?\s*([A-Za-z][A-Za-z0-9_.-]*)",
        r"(?:服务|应用)\s+([A-Za-z][A-Za-z0-9_.-]*)",
    ] {
        if let Some(c) = re(pattern).captures(text) {
            return Some(c[1].to_string());
        }
    }
    crate::wecom_intent::extract_service(text)
}

type Window = (DateTime<FixedOffset>, DateTime<FixedOffset>);
type TimeParse = (Option<Window>, Vec<(usize, usize)>);

fn parse_window(
    text: &str,
    previous: &InvestigationQuery,
    now: DateTime<FixedOffset>,
    followup: bool,
) -> Result<TimeParse, String> {
    if ["前天", "明天", "后天", "凌晨", "中午", "上周", "上个月"]
        .iter()
        .any(|w| text.contains(w))
    {
        return Err("此日期或时段表达式有歧义，请写明完整日期和24小时时刻。".into());
    }
    // RFC3339 优先；中文时刻允许共用日期与晚间修饰词。
    let clock = re(
        r"(?i)\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:\d{2})|(?:(?:\d{4}-\d{2}-\d{2}|今天|昨天|昨晚|今晚)\s*)?(?:(?:上午|下午|晚上|晚间|早上)\s*)?(?:\d{1,2}:\d{2}(?::\d{2})?|[0-9零一二两三四五六七八九十]{1,3}点(?:半|[0-9零一二两三四五六七八九十]{1,3}分?)?)",
    );
    let clocks: Vec<_> = clock.find_iter(text).collect();
    if !clocks.is_empty() {
        if clocks.len() != 2 {
            return Err("请给出完整起止时间，例如今天10点到11点。".into());
        }
        let between = text[clocks[0].end()..clocks[1].start()].trim();
        if !["至", "到", "~", "～", "—", "-", "to", "TO"].contains(&between) {
            return Err("两个时刻之间请使用「到」或「至」。".into());
        }
        let (start, evening) =
            parse_clock(clocks[0].as_str(), now.date_naive(), *now.offset(), false)?;
        let end_base = if ["今天", "昨天", "昨晚", "今晚"]
            .iter()
            .any(|w| clocks[1].as_str().contains(w))
        {
            now.date_naive()
        } else {
            start.date_naive()
        };
        let (end, _) = parse_clock(clocks[1].as_str(), end_base, *start.offset(), evening)?;
        return Ok((
            Some((start, end)),
            clocks.iter().map(|m| (m.start(), m.end())).collect(),
        ));
    }
    let duration = re(
        r"(?i)(?:最近|过去|近|扩大到|扩展到|放大到|扩大|扩展|放大|增加|再查)\s*([0-9零一二两三四五六七八九十百]+|半)\s*(小时|分钟|天|hours?|days?|minutes?|h|min)",
    );
    if let Some(cap) = duration.captures(text) {
        let value = &cap[1];
        let multiplier = match cap[2].to_ascii_lowercase().as_str() {
            "小时" | "hour" | "hours" | "h" => 3600,
            "天" | "day" | "days" => 86400,
            _ => 60,
        };
        let seconds = if value == "半" {
            multiplier / 2
        } else {
            number(value)?
                .checked_mul(multiplier)
                .ok_or("时间长度溢出")?
        };
        if seconds <= 0 {
            return Err("时间长度必须大于零。".into());
        }
        let mut delta = Duration::try_seconds(seconds).ok_or("时间长度超出支持范围")?;
        let end = if followup && expanding(text) {
            let end =
                DateTime::parse_from_rfc3339(&previous.end).map_err(|_| "上一条结束时间错误")?;
            let start =
                DateTime::parse_from_rfc3339(&previous.start).map_err(|_| "上一条开始时间错误")?;
            let old = end.signed_duration_since(start);
            if text.contains("再扩大") || text.contains("再扩展") {
                delta = delta.checked_add(&old).ok_or("时间长度溢出")?;
            } else if delta < old {
                return Err("扩大后的窗口比原窗口更小，请明确新的起止时间。".into());
            }
            end
        } else {
            now
        };
        let m = cap.get(0).unwrap();
        return Ok((
            Some((subtract(end, delta)?, end)),
            vec![(m.start(), m.end())],
        ));
    }
    if followup && expanding(text) {
        // 不猜扩大倍数/边界；调用方将错误作为追问澄清，不回退旧查询。
        return Err("请说明扩大到多久，例如「扩大到8小时」。".into());
    }
    let days = re(r"今天|昨天");
    let day_matches: Vec<_> = days.find_iter(text).collect();
    if day_matches.len() > 1 {
        return Err("多个日期缺少明确起止时刻。".into());
    }
    if let Some(day) = day_matches.first() {
        let date = if day.as_str() == "昨天" {
            now.date_naive().pred_opt().ok_or("日期超出支持范围")?
        } else {
            now.date_naive()
        };
        let start = at(
            date,
            NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
            *now.offset(),
        )?;
        let end = if day.as_str() == "今天" {
            now
        } else {
            start
                .checked_add_signed(Duration::days(1))
                .ok_or("日期超出支持范围")?
        };
        return Ok((Some((start, end)), vec![(day.start(), day.end())]));
    }
    if [
        "最近",
        "过去",
        "昨晚",
        "今晚",
        "凌晨",
        "上周",
        "前天",
        "上个月",
    ]
    .iter()
    .any(|w| text.contains(w))
        || re(r"\d{4}-\d{2}-\d{2}").is_match(text)
    {
        return Err("时间表达式未识别，请写明日期和起止时刻。".into());
    }
    if text.contains("刚才") || text.contains("刚刚") {
        return Ok((
            Some((subtract(now, Duration::minutes(15))?, now)),
            Vec::new(),
        ));
    }
    Ok((None, Vec::new()))
}

fn at(
    date: NaiveDate,
    time: NaiveTime,
    offset: FixedOffset,
) -> Result<DateTime<FixedOffset>, String> {
    offset
        .from_local_datetime(&date.and_time(time))
        .single()
        .ok_or_else(|| "日期时间超出支持范围。".into())
}
fn parse_clock(
    text: &str,
    base: NaiveDate,
    offset: FixedOffset,
    inherit_evening: bool,
) -> Result<(DateTime<FixedOffset>, bool), String> {
    if let Ok(value) = DateTime::parse_from_rfc3339(text) {
        return Ok((value, false));
    }
    let date_re = re(r"^\d{4}-\d{2}-\d{2}");
    let explicit_date = date_re.find(text);
    let date = if let Some(m) = explicit_date {
        NaiveDate::parse_from_str(m.as_str(), "%Y-%m-%d").map_err(|_| "日期格式错误")?
    } else if text.contains("昨天") || text.contains("昨晚") {
        base.pred_opt().ok_or("日期超出支持范围")?
    } else {
        base
    };
    let explicit_period = ["上午", "下午", "晚上", "晚间", "早上", "昨晚", "今晚"]
        .iter()
        .any(|w| text.contains(w));
    let evening = ["下午", "晚上", "晚间", "昨晚", "今晚"]
        .iter()
        .any(|w| text.contains(w))
        || (!explicit_period && explicit_date.is_none() && inherit_evening);
    let rest = explicit_date.map(|m| &text[m.end()..]).unwrap_or(text);
    let (mut hour, minute, second) =
        if let Some(cap) = re(r"(\d{1,2}):(\d{2})(?::(\d{2}))?").captures(rest) {
            (
                number(&cap[1])?,
                number(&cap[2])?,
                cap.get(3)
                    .map(|m| number(m.as_str()))
                    .transpose()?
                    .unwrap_or(0),
            )
        } else {
            let cap = re(
            r"([0-9零一二两三四五六七八九十]{1,3})点(半|[0-9零一二两三四五六七八九十]{1,3}分?)?",
        )
        .captures(rest)
        .ok_or("时刻格式错误")?;
            let minute = match cap.get(2).map(|m| m.as_str()) {
                Some("半") => 30,
                Some(value) => number(value.trim_end_matches('分'))?,
                None => 0,
            };
            (number(&cap[1])?, minute, 0)
        };
    if evening && hour < 12 {
        hour += 12;
    }
    let time = NaiveTime::from_hms_opt(
        u32::try_from(hour).map_err(|_| "小时越界")?,
        u32::try_from(minute).map_err(|_| "分钟越界")?,
        u32::try_from(second).map_err(|_| "秒越界")?,
    )
    .ok_or("时刻超出有效范围")?;
    Ok((at(date, time, offset)?, evening))
}
fn number(text: &str) -> Result<i64, String> {
    if text.chars().all(|c| c.is_ascii_digit()) {
        return text.parse::<i64>().map_err(|_| "数字超出支持范围。".into());
    }
    let digit = |c| match c {
        '零' => Some(0),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    };
    if let Some((left, right)) = text.split_once('十') {
        let tens = if left.is_empty() {
            1
        } else {
            left.chars()
                .next()
                .and_then(digit)
                .filter(|_| left.chars().count() == 1)
                .ok_or("中文数字格式未识别")?
        };
        let ones = if right.is_empty() {
            0
        } else {
            right
                .chars()
                .next()
                .and_then(digit)
                .filter(|_| right.chars().count() == 1)
                .ok_or("中文数字格式未识别")?
        };
        return Ok(tens * 10 + ones);
    }
    text.chars()
        .next()
        .and_then(digit)
        .filter(|_| text.chars().count() == 1)
        .ok_or_else(|| "中文数字请使用零到九十九，或阿拉伯数字。".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-28T17:30:00+08:00").unwrap()
    }
    fn parse(text: &str) -> InvestigationQuery {
        parse_query(text, None, now()).unwrap()
    }
    #[test]
    fn default_and_symptom_are_not_literal_keywords() {
        let q = parse("下单一直失败帮我排查下");
        assert!(q.keywords.is_empty());
        assert_eq!(q.start, "2026-10-28T13:30:00+08:00");
        assert!(!q.match_any);
    }
    #[test]
    fn date_and_relative_windows() {
        for (text, start, end) in [
            (
                "今天10点到11点",
                "2026-10-28T10:00:00+08:00",
                "2026-10-28T11:00:00+08:00",
            ),
            (
                "昨晚8点到9点",
                "2026-10-27T20:00:00+08:00",
                "2026-10-27T21:00:00+08:00",
            ),
            (
                "2026-10-28 10:00:00 至10:30:00",
                "2026-10-28T10:00:00+08:00",
                "2026-10-28T10:30:00+08:00",
            ),
            (
                "10:00 至 10:30",
                "2026-10-28T10:00:00+08:00",
                "2026-10-28T10:30:00+08:00",
            ),
            (
                "最近两小时",
                "2026-10-28T15:30:00+08:00",
                "2026-10-28T17:30:00+08:00",
            ),
            (
                "昨天",
                "2026-10-27T00:00:00+08:00",
                "2026-10-28T00:00:00+08:00",
            ),
            (
                "今天",
                "2026-10-28T00:00:00+08:00",
                "2026-10-28T17:30:00+08:00",
            ),
            (
                "2026-10-27 23:00到2026-10-28 01:00",
                "2026-10-27T23:00:00+08:00",
                "2026-10-28T01:00:00+08:00",
            ),
        ] {
            let q = parse(text);
            assert_eq!((q.start.as_str(), q.end.as_str()), (start, end), "{text}");
        }
    }
    #[test]
    fn rfc3339_offsets_are_preserved() {
        let q = parse("2026-10-28T10:00:00Z至2026-10-28T10:30:00Z error");
        assert_eq!(q.timezone_offset_minutes, 480);
        assert_eq!(q.start, "2026-10-28T10:00:00+00:00");
        assert_eq!(q.keywords, vec!["error"]);
    }
    #[test]
    fn reversed_invalid_or_ambiguous_times_return_errors() {
        for text in [
            "今天11点到10点",
            "23:00到01:00",
            "2026-02-30 10:00到11:00",
            "25:00到26:00",
            "最近9223372036854775807天",
            "昨晚",
            "今天10点",
            "过去三百小时",
        ] {
            assert!(parse_query(text, None, now()).is_err(), "{text}");
        }
    }
    #[test]
    fn keywords_fields_quotes_and_identifiers() {
        let q = parse("订单202610281234567890失败 error");
        assert_eq!(q.trace_id, None);
        assert_eq!(q.keywords, vec!["202610281234567890", "error"]);
        let q = parse("关键词：\"payment failed\" 或者 timeout; 排除：health; service=pay-api");
        assert_eq!(q.keywords, vec!["payment failed", "timeout"]);
        assert_eq!(q.exclude_keyword, "health");
        assert_eq!(q.service.as_deref(), Some("pay-api"));
        assert!(q.match_any);
        assert_eq!(parse("查\"用户不存在\"日志").keywords, vec!["用户不存在"]);
    }
    #[test]
    fn followups_inherit_reset_and_paginate() {
        let q = parse("订单202610281234567890 error traceId=abc service=pay-api");
        let page = parse_query("下一页", Some(&q), now()).unwrap();
        assert_eq!(page.page, 2);
        assert_eq!(page.trace_id, q.trace_id);
        let next = parse_query("继续下游", Some(&page), now()).unwrap();
        assert_eq!(next, page);
        let service = parse_query("只看服务 settle", Some(&page), now()).unwrap();
        assert_eq!(service.service.as_deref(), Some("settle"));
        assert_eq!(service.page, 1);
        assert_eq!(service.start, q.start);
        let expanded = parse_query("扩大到8小时", Some(&q), now()).unwrap();
        assert_eq!(expanded.start, "2026-10-28T09:30:00+08:00");
        assert_eq!(expanded.keywords, q.keywords);
        let changed = parse_query("换订单202610289999999999", Some(&q), now()).unwrap();
        assert_eq!(changed.trace_id, None);
        assert_eq!(changed.keywords, vec!["202610289999999999"]);
        assert!(parse_query("扩大窗口", Some(&q), now()).is_err());
        assert!(parse_query("下一页", None, now()).is_err());
    }
    #[test]
    fn explicit_fields_do_not_eat_time_and_quotes_stay_literal() {
        let q = parse("关键词=error 最近两小时");
        assert_eq!(q.keywords, vec!["error"]);
        assert_eq!(q.start, "2026-10-28T15:30:00+08:00");
        let q = parse("查\"昨天10:00;失败\"日志");
        assert_eq!(q.keywords, vec!["昨天10:00;失败"]);
        let q = parse("昨天23点到今天1点");
        assert_eq!(q.start, "2026-10-27T23:00:00+08:00");
        assert_eq!(q.end, "2026-10-28T01:00:00+08:00");
        let q = parse("traceId=abc");
        let changed = parse_query("keyword=error", Some(&q), now()).unwrap();
        assert!(changed.trace_id.is_none());
        assert_eq!(changed.keywords, vec!["error"]);
        assert!(parse_query("前天10点到11点", None, now()).is_err());
    }

    #[test]
    fn natural_exclusion_and_level_are_filters_not_keywords() {
        let original = parse("关键词=ORD123456 traceId=abc service=pay-api");
        let q = parse_query("排除 timeout 只看错误", Some(&original), now()).unwrap();
        assert_eq!(q.keywords, original.keywords);
        assert_eq!(q.trace_id, original.trace_id);
        assert_eq!(q.exclude_keyword, "timeout");
        assert_eq!(q.level.as_deref(), Some("ERROR"));
        let page = parse_query("下一页", Some(&q), now()).unwrap();
        assert_eq!(page.level, q.level);
        assert_eq!(page.exclude_keyword, q.exclude_keyword);
        assert_eq!(page.page, 2);
        let changed = parse_query("level=INFO", Some(&page), now()).unwrap();
        assert_eq!(changed.level.as_deref(), Some("INFO"));
        assert_eq!(changed.page, 1);
        assert_eq!(
            parse("级别：ERROR 订单ORD123456").level.as_deref(),
            Some("ERROR")
        );
        assert_eq!(
            parse("不包含\"connection timeout\" service=pay-api").exclude_keyword,
            "connection timeout"
        );
        assert_eq!(
            parse("查\"排除 timeout\"日志").keywords,
            vec!["排除 timeout"]
        );
        assert!(parse_query("level=MYSTERY", None, now()).is_err());
    }

    #[test]
    fn time_values_are_not_business_identifiers() {
        let q = parse("2026-10-28 10:00:00至10:30:00 下单失败");
        assert!(q.keywords.is_empty());
    }
}
