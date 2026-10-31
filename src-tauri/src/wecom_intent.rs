//! 企微问答「自动」模式的意图分类 + 槽位抽取（纯函数、可单测、不含 IO）。
//!
//! 旧实现靠两个扁平子串表做 bool 二选一，误判多（「写日志模块怎么实现」被当排障、
//! 「500 行代码」被当 HTTP 5xx），且判出排障后不知道查哪个服务/什么错误/多大时间窗。
//! 这里改为「打分分类 + 抽槽」：产物同时驱动提示词 SOP 与 SkyWalking 预取查询参数。

/// 一条问答消息的意图类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QaIntent {
    /// 排查线上问题（走日志/调用链/指标 + 代码）。
    Troubleshoot,
    /// 读代码（直接看源码）。
    ReadCode,
    /// 文档查询（实现/原理/「做什么」，文档优先）。
    DocLookup,
}

/// 意图分析结果：类别 + 置信度 + 排障槽位。
#[derive(Debug, Clone)]
pub struct QaAnalysis {
    pub intent: QaIntent,
    /// 0..100；弱信号时提示词用「疑似」措辞。
    pub confidence: u8,
    pub trace_id: Option<String>,
    pub service: Option<String>,
    /// 日志关键词列表（有序）：业务标识符（订单号/用户ID/手机号/requestId…）在前，
    /// 错误特征（异常类名/HTTP 码/故障词）在后，封顶 3 个。多个词按 AND 收窄。
    pub keywords: Vec<String>,
    /// 时间窗（分钟，封顶 24h）。
    pub minutes: Option<i64>,
    /// 日志级别过滤（命中「只看错误」→ ERROR）。
    pub level: Option<String>,
}

const MAX_WINDOW_MIN: i64 = 24 * 60;
/// 单次查询最多用几个关键词（AND 收窄，太多易查空）。
const MAX_KEYWORDS: usize = 3;

/// 排障强信号词（命中即高分）。
const TROUBLE_STRONG: &[&str] = &[
    "报错",
    "报警",
    "告警",
    "崩溃",
    "宕机",
    "堆栈",
    "超时",
    "死锁",
    "timeout",
    "exception",
    "oom",
    "panic",
    "stacktrace",
    "stack trace",
    "deadlock",
    "connection refused",
    "503",
    "502",
    "504",
    "500 错误",
];
/// 排障弱信号词（单独出现不足以定性，需配合语境）。
const TROUBLE_WEAK: &[&str] = &[
    "排查",
    "线上",
    "故障",
    "异常",
    "日志",
    "调用链",
    "trace",
    "报错了",
    "挂了",
    "不通",
    "失败",
    "troubleshoot",
    "online issue",
    "production",
    "incident",
];
/// 业务症状词（中权重：用户常用大白话描述线上问题，不带「报错/异常」等技术词）。
const TROUBLE_SYMPTOM: &[&str] = &[
    "慢",
    "卡",
    "卡顿",
    "卡住",
    "用不了",
    "打不开",
    "没反应",
    "没响应",
    "无响应",
    "不好使",
    "出问题",
    "有问题",
    "不对",
    "崩了",
    "炸了",
    "报错",
    "错误",
    "fail",
    "failed",
    "slow",
    "stuck",
    "hang",
    "broken",
    "not working",
    "no response",
];
/// 读代码信号词。
const CODE_HINTS: &[&str] = &[
    "读代码",
    "看代码",
    "从代码",
    "看源码",
    "读源码",
    "源代码",
    "源码",
    "code",
];
/// 文档/实现类信号词（压制排障弱信号，避免「写日志模块怎么实现」误判排障）。
const DOC_HINTS: &[&str] = &[
    "怎么实现",
    "如何实现",
    "实现原理",
    "原理",
    "怎么做的",
    "做什么",
    "是什么",
    "介绍",
    "讲讲",
    "说明",
    "设计",
    "流程",
    "架构",
    "模块",
];

/// 入口：分析一条用户消息。
pub fn analyze(text: &str) -> QaAnalysis {
    let lower = text.to_lowercase();
    let trace_id = extract_trace_id(text);

    // —— 各意图打分 ——
    let mut trouble = 0i32;
    if trace_id.is_some() {
        trouble += 60; // 带 traceId 基本就是排障
    }
    for h in TROUBLE_STRONG {
        if lower.contains(&h.to_lowercase()) {
            trouble += 40;
        }
    }
    for h in TROUBLE_WEAK {
        if lower.contains(&h.to_lowercase()) {
            trouble += 18;
        }
    }
    for h in TROUBLE_SYMPTOM {
        if lower.contains(&h.to_lowercase()) {
            trouble += 25;
        }
    }
    if has_http_error_code(&lower) {
        trouble += 40;
    }
    if has_exception_class(text) {
        trouble += 45;
    }
    // 组合提权：「问题/故障」+（服务/接口/线上/服务名后缀）更像线上排障。
    let mentions_problem = lower.contains("问题") || lower.contains("故障");
    let mentions_target = lower.contains("服务")
        || lower.contains("接口")
        || lower.contains("线上")
        || extract_service(text).is_some();
    if mentions_problem && mentions_target {
        trouble += 20;
    }

    let mut code = 0i32;
    for h in CODE_HINTS {
        if lower.contains(&h.to_lowercase()) {
            code += 35;
        }
    }

    let mut doc = 10i32; // 兜底基线：没别的信号就当文档查询
    for h in DOC_HINTS {
        if lower.contains(h) {
            doc += 22;
        }
    }
    // 文档/实现语境压制排障弱信号：「写日志模块怎么实现」这类不该算排障。
    if doc >= 32 && trace_id.is_none() && !has_http_error_code(&lower) && !has_exception_class(text)
    {
        trouble -= 30;
    }

    let trouble = trouble.max(0);
    // 平手优先级：排障 > 读代码 > 文档（线上问题优先，契合诉求）。
    let (intent, top) = if trouble >= code && trouble >= doc && trouble > 0 {
        (QaIntent::Troubleshoot, trouble)
    } else if code >= doc && code > 0 {
        (QaIntent::ReadCode, code)
    } else {
        (QaIntent::DocLookup, doc)
    };
    let confidence = top.clamp(0, 100) as u8;

    // —— 排障槽位（仅排障意图才费力抽取）——
    let (service, keywords, minutes, level) = if intent == QaIntent::Troubleshoot {
        (
            extract_service(text),
            build_keywords(text, trace_id.as_deref()),
            extract_minutes(&lower),
            extract_level(&lower),
        )
    } else {
        (None, Vec::new(), None, None)
    };

    QaAnalysis {
        intent,
        confidence,
        trace_id,
        service,
        keywords,
        minutes,
        level,
    }
}

/// 组关键词列表（NL 优先）：旁证正文业务短语 → 非账户类标识符 → 错误特征，去重封顶。
/// `exclude_trace`：已识别为 traceId 的值不再当普通关键词（避免和 relatedTrace 重复）。
fn build_keywords(text: &str, exclude_trace: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |v: String, out: &mut Vec<String>| {
        let v = v.trim().to_string();
        if v.is_empty() {
            return;
        }
        if let Some(t) = exclude_trace {
            if v == t {
                return;
            }
        }
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&v)) && out.len() < MAX_KEYWORDS {
            out.push(v);
        }
    };
    // 1) 仅旁证正文的业务短语（如 content「今天五粮液多少钱」→「五粮液」）
    //    不问句整句抽症状——「下单失败」很少原样出现在日志里。
    for content in extract_sidecar_contents(text) {
        for phrase in extract_business_phrases(&content) {
            push(phrase, &mut out);
        }
    }
    // 2) 业务标识符，跳过账户类长数字（toAccountId/from 等）
    for id in extract_identifiers(text, exclude_trace) {
        if is_likely_account_id(text, &id) {
            continue;
        }
        push(id, &mut out);
    }
    // 3) 错误特征补位（不要用 pick_keyword 整句/账户数字兜底）
    if let Some(k) = extract_error_keyword(text) {
        if !is_likely_account_id(text, &k)
            && (k.ends_with("Exception")
                || k.ends_with("Error")
                || [
                    "500", "502", "503", "504", "429", "400", "401", "403", "404", "timeout",
                    "超时", "OutOfMemory", "deadlock", "connection refused", "connection reset",
                    "NullPointer", "error",
                ]
                .iter()
                .any(|w| k.eq_ignore_ascii_case(w)))
        {
            push(k, &mut out);
        }
    }
    out
}

/// 用户可见 NL 语料 = 原消息 + 旁证 JSON 里抽出的正文（content）。
pub fn nl_corpus(text: &str) -> String {
    let mut parts = vec![text.to_string()];
    for content in extract_sidecar_contents(text) {
        if !parts.iter().any(|p| p.contains(&content)) {
            parts.push(content);
        }
    }
    parts.join("\n")
}

/// 从粘贴的 JSON / 结构化旁证里抽出可读正文（`"content":"..."` / `text.content`）。
pub fn extract_sidecar_contents(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // "content": "..." 或 "content":"..."（含转义 \"）
    let re = regex::Regex::new(r#""content"\s*:\s*"((?:\\.|[^"\\])*)""#).ok();
    let Some(re) = re else {
        return out;
    };
    for cap in re.captures_iter(text) {
        let raw = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let unescaped = raw.replace("\\\"", "\"").replace("\\n", "\n");
        let s = unescaped.trim().to_string();
        if s.chars().count() >= 2 && !out.iter().any(|x| x == &s) {
            out.push(s);
        }
    }
    out
}

/// 抽中文/业务短短语（≥2 个汉字），供日志关键词。
/// 「今天五粮液多少钱」→「五粮液」：剥时间/疑问壳，保留中间业务名。
pub fn extract_business_phrases(text: &str) -> Vec<String> {
    const PREFIX: &[&str] = &[
        "今天", "昨天", "明天", "刚才", "刚刚", "帮我", "请问", "麻烦", "查一下", "看一下",
        "排查", "新起会话", "新会话", "新对话",
    ];
    const SUFFIX: &[&str] = &[
        "多少钱", "怎么回事", "什么情况", "怎么了", "出问题了", "报错了", "失败了", "超时了",
        "没反应", "打不开", "用不了",
    ];
    const STOP: &[&str] = &[
        "今天", "昨天", "明天", "排查", "帮我", "请问", "多少钱", "怎么回事", "什么情况",
        "继续", "日志", "调用链", "新起会话", "新会话", "新对话",
    ];
    let mut out = Vec::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut Vec<String>| {
        let mut s = buf.trim().to_string();
        buf.clear();
        if s.chars().count() < 2 {
            return;
        }
        for p in PREFIX {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest.to_string();
            }
        }
        for p in SUFFIX {
            if let Some(rest) = s.strip_suffix(p) {
                s = rest.to_string();
            }
        }
        s = s.trim().to_string();
        if s.chars().count() < 2 || STOP.iter().any(|w| s == *w) {
            return;
        }
        if !out.iter().any(|x| x == &s) {
            out.push(s);
        }
    };
    for ch in text.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            buf.push(ch);
        } else if ch.is_ascii_alphanumeric() && !buf.is_empty() {
            buf.push(ch);
        } else {
            flush(&mut buf, &mut out);
        }
    }
    flush(&mut buf, &mut out);
    // 中等长度优先：太长像整句，太短噪声多；同长度保持原序
    out.sort_by(|a, b| {
        let score = |s: &str| {
            let n = s.chars().count();
            if (2..=8).contains(&n) {
                100 - n as i32
            } else {
                -(n as i32)
            }
        };
        score(b).cmp(&score(a))
    });
    out.truncate(MAX_KEYWORDS);
    out
}

/// 长纯数字且出现在账户/主体字段旁 → 不当作默认业务检索主键。
pub fn is_likely_account_id(text: &str, value: &str) -> bool {
    if value.len() < 10 || !value.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let Some(pos) = lower.find(value) else {
        return false;
    };
    // 看数字前是否挂着账户类键（JSON `"from":"…"` / `toAccountId=…`）；跳过中文前缀占的字节。
    let mut start = pos.saturating_sub(96);
    while start < pos && !lower.is_char_boundary(start) {
        start += 1;
    }
    let ctx = &lower[start..pos];
    [
        "toaccountid",
        "\"from\"",
        "corpid",
        "agentid",
        "userid",
        "accountid",
        "toaccount",
        "staffid",
        "creator",
    ]
    .iter()
    .any(|key| ctx.contains(key))
}

/// 从消息里抽一个疑似 traceId（32/16 位十六进制，或 SkyWalking 的点分段 id）。
pub fn extract_trace_id(t: &str) -> Option<String> {
    // 显式字段先于任何形状猜测；to_ascii_lowercase 保持 UTF-8 字节坐标。
    let lower = t.to_ascii_lowercase();
    for label in ["traceid", "trace_id", "trace id"] {
        for (pos, _) in lower.match_indices(label) {
            if pos > 0 && lower.as_bytes()[pos - 1].is_ascii_alphanumeric() {
                continue;
            }
            let rest = t[pos + label.len()..].trim_start();
            let rest = rest
                .strip_prefix('=')
                .or_else(|| rest.strip_prefix(':'))
                .or_else(|| rest.strip_prefix('：'));
            if let Some(rest) = rest {
                let value: String = rest
                    .trim_start()
                    .trim_start_matches(['\"', '\''])
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                    .collect();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
    }
    // 字段赋值是明确业务语义，优先于 hex/点分段的形状猜测。
    let mut unlabeled = t.to_string();
    let mut ranges = Vec::new();
    for (equal, _) in t.match_indices('=') {
        let key_end = t[..equal].trim_end().len();
        let key_start = t[..key_end]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map(|i| i + t[i..].chars().next().unwrap().len_utf8())
            .unwrap_or(0);
        if key_start == key_end || !t.as_bytes()[key_start].is_ascii_alphabetic() {
            continue;
        }
        let rest = &t[equal + 1..];
        let value_start = equal + 1 + rest.len() - rest.trim_start().len();
        let value_end = value_start
            + t[value_start..]
                .chars()
                .take_while(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '"' | '\'')
                })
                .map(char::len_utf8)
                .sum::<usize>();
        ranges.push((key_start, value_end));
    }
    for (start, end) in ranges.into_iter().rev() {
        unlabeled.replace_range(start..end, &" ".repeat(end - start));
    }
    for tok in
        unlabeled.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
    {
        let tok = tok.trim_matches('.');
        let hex = matches!(tok.len(), 16 | 32)
            && tok.chars().all(|c| c.is_ascii_hexdigit())
            && tok.chars().any(|c| c.is_ascii_alphabetic());
        let dotted = tok.matches('.').count() >= 2
            && tok.len() >= 20
            && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '.');
        if hex || dotted {
            return Some(tok.to_string());
        }
    }
    None
}

/// 用户是否明确要求「读代码/看源码」（供固定模式临时放行用）。
pub fn mentions_code(t: &str) -> bool {
    let lower = t.to_lowercase();
    CODE_HINTS.iter().any(|h| lower.contains(&h.to_lowercase()))
}

/// 是否含 HTTP 错误码语境：`4xx/5xx` 且像状态码（独立成词），
/// 或邻近有 错误/状态/code/响应 等语境词，避免「500 行代码」误判。
fn has_http_error_code(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 3 <= n {
        // 找连续 3 位数字
        if bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
        {
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let after = i + 3;
            // 后面不能紧跟更多数字（排除 5000 这类）
            let after_digit = after < n && bytes[after].is_ascii_digit();
            let d0 = bytes[i];
            let is_45xx = (d0 == b'4' || d0 == b'5') && !after_digit && before_ok;
            if is_45xx {
                // 紧跟计量词（行/个/条/次/毫秒/ms/kb/mb…）的不是状态码，跳过（排除「500 行代码」）。
                let tail = lower[after..].trim_start();
                let is_measure = [
                    "行", "个", "条", "次", "毫秒", "秒", "ms", "kb", "mb", "gb", "元", "人", "万",
                ]
                .iter()
                .any(|m| tail.starts_with(m));
                if is_measure {
                    // 跳过整段数字后继续扫描
                    let mut j = i;
                    while j < n && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                // 常见错误码直接算；否则要有语境词
                let code = &lower[i..i + 3];
                let common = matches!(
                    code,
                    "500" | "502" | "503" | "504" | "400" | "401" | "403" | "404" | "429"
                );
                if common {
                    return true;
                }
                // 其它 4xx/5xx：窗口内有语境词才算（避免「512 行」这类）
                let mut ctx_start = i.saturating_sub(12);
                let mut ctx_end = (i + 15).min(n);
                while !lower.is_char_boundary(ctx_start) {
                    ctx_start += 1;
                }
                while !lower.is_char_boundary(ctx_end) {
                    ctx_end -= 1;
                }
                let ctx = &lower[ctx_start..ctx_end];
                if ctx.contains("error")
                    || ctx.contains("status")
                    || ctx.contains("code")
                    || ctx.contains("响应")
                    || ctx.contains("状态")
                    || ctx.contains("返回")
                    || ctx.contains("报错")
                {
                    return true;
                }
            }
            // 跳过整段数字
            let mut j = i;
            while j < n && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

/// 是否含 `XxxException` / `XxxError` 类名（强排障信号）。
fn has_exception_class(text: &str) -> bool {
    extract_exception_class(text).is_some()
}

/// 抽取 `XxxException` / `XxxError` 类名（驼峰、首字母大写、以 Exception/Error 结尾）。
fn extract_exception_class(text: &str) -> Option<String> {
    for tok in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_')) {
        let name = tok.rsplit('.').next().unwrap_or(tok); // 去包名
        if name.len() >= 6
            && name.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
            && name.chars().all(|c| c.is_ascii_alphanumeric())
            && (name.ends_with("Exception") || name.ends_with("Error"))
            // 排除裸 "Error"/"Exception"（信息量低）
            && name != "Error"
            && name != "Exception"
        {
            return Some(name.to_string());
        }
    }
    None
}

/// 抽取服务名：`XxxService` / `xxx-service|svc|api|server` / 「服务/应用 <token>」。
pub(crate) fn extract_service(text: &str) -> Option<String> {
    // ASCII token 边界也包括紧贴服务名的中文，如「排查payment-api超时」。
    for tok in text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))) {
        let t = tok.trim();
        if t.len() < 4 {
            continue;
        }
        let lower = t.to_lowercase();
        let suffix_ok = lower.ends_with("-service")
            || lower.ends_with("-svc")
            || lower.ends_with("-api")
            || lower.ends_with("-server")
            || (t.ends_with("Service")
                && t.chars()
                    .next()
                    .map(|c| c.is_ascii_uppercase())
                    .unwrap_or(false));
        if suffix_ok
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Some(t.to_string());
        }
    }
    None
}

/// 日志关键词：错误特征优先（异常类名 > HTTP 码 > 典型故障词），否则回退最长词。
fn extract_error_keyword(text: &str) -> Option<String> {
    if let Some(cls) = extract_exception_class(text) {
        return Some(cls);
    }
    let lower = text.to_lowercase();
    // HTTP 错误码
    for code in [
        "500", "502", "503", "504", "429", "400", "401", "403", "404",
    ] {
        if word_present(&lower, code) {
            return Some(code.to_string());
        }
    }
    // 典型故障英文词（原样大小写不重要，取英文形式作关键词）
    for (needle, kw) in [
        ("timeout", "timeout"),
        ("超时", "超时"),
        ("oom", "OutOfMemory"),
        ("outofmemory", "OutOfMemory"),
        ("deadlock", "deadlock"),
        ("connection refused", "connection refused"),
        ("connection reset", "connection reset"),
        ("npe", "NullPointer"),
    ] {
        if lower.contains(needle) {
            return Some(kw.to_string());
        }
    }
    pick_keyword(text)
}

/// 抽业务标识符（不依赖空格，按字符类切 token）：手机号 / 长数字串 / 字母数字混合码 /
/// UUID 或长 hex / 带连字符或下划线的 ID。排除 HTTP 状态码、已识别的 traceId 与计量词数字。
/// 结果按「更像标识符」排序（手机号 > 带前缀混合码 > 长数字/hex），去重后最多 MAX_KEYWORDS 个。
pub fn extract_identifiers(text: &str, exclude_trace: Option<&str>) -> Vec<String> {
    // 显式键值优先：「订单号/单号/用户id/userId/手机号/requestId/商户号 : <值>」
    let mut scored: Vec<(i32, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let add = |score: i32, tok: &str, scored: &mut Vec<(i32, String)>, seen: &mut Vec<String>| {
        let tok = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if tok.chars().count() < 4 {
            return;
        }
        if let Some(t) = exclude_trace {
            if tok == t {
                return;
            }
        }
        // 纯 HTTP 状态码不当标识符
        if tok.len() == 3 && tok.chars().all(|c| c.is_ascii_digit()) {
            return;
        }
        if seen.iter().any(|x| x.eq_ignore_ascii_case(tok)) {
            return;
        }
        seen.push(tok.to_string());
        scored.push((score, tok.to_string()));
    };

    // token 化：按「非字母数字、非 - 、非 _」切分（允许 ID 里的连字符/下划线）
    for raw in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        let tok = raw.trim_matches(|c: char| c == '-' || c == '_');
        let bytes = tok.as_bytes();
        let date_prefix = bytes.len() >= 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[..4]
                .iter()
                .chain(&bytes[5..7])
                .chain(&bytes[8..10])
                .all(u8::is_ascii_digit)
            && (bytes.len() == 10 || matches!(bytes[10], b'T' | b't'));
        if tok.is_empty() || date_prefix {
            continue;
        }
        let digits = tok.chars().filter(|c| c.is_ascii_digit()).count();
        let alphas = tok.chars().filter(|c| c.is_ascii_alphabetic()).count();
        let has_sep = tok.contains('-') || tok.contains('_');
        let all_hex = tok.chars().all(|c| c.is_ascii_hexdigit());
        let len = tok.chars().count();

        // 手机号：1 开头 11 位纯数字
        if digits == 11
            && alphas == 0
            && tok.starts_with('1')
            && tok.chars().all(|c| c.is_ascii_digit())
        {
            add(100, tok, &mut scored, &mut seen);
            continue;
        }
        // 字母数字混合业务码：字母≥2 且数字≥4（ORD20241028 / PO123456）
        if alphas >= 2 && digits >= 4 {
            add(80, tok, &mut scored, &mut seen);
            continue;
        }
        // 带连字符/下划线的 ID（req-123-abc / order_998877），且含数字
        if has_sep && digits >= 2 && len >= 6 {
            add(75, tok, &mut scored, &mut seen);
            continue;
        }
        // 长数字串 ≥6 位（订单号/用户ID），排除计量数字由上游压制
        if alphas == 0 && digits >= 6 {
            add(70, tok, &mut scored, &mut seen);
            continue;
        }
        // 长 hex（≥8，含字母，区别于普通英文词；traceId 已被排除）
        if all_hex && len >= 8 && digits >= 1 && alphas >= 1 {
            add(60, tok, &mut scored, &mut seen);
            continue;
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored
        .into_iter()
        .map(|(_, v)| v)
        .take(MAX_KEYWORDS)
        .collect()
}

/// 独立成词地包含 `needle`（数字码用，避免 5000 命中 500）。
fn word_present(lower: &str, needle: &str) -> bool {
    let bytes = lower.as_bytes();
    let nb = needle.as_bytes();
    let n = bytes.len();
    let m = nb.len();
    if m == 0 || m > n {
        return false;
    }
    let mut i = 0;
    while i + m <= n {
        if &bytes[i..i + m] == nb {
            let before = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let after = i + m == n || !bytes[i + m].is_ascii_alphanumeric();
            if before && after {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// 回退关键词：按中英文常见分隔符切词，丢掉纯标点与过短词，取最长含字母数字的片段。
pub fn pick_keyword(text: &str) -> Option<String> {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '，' | ','
                    | '。'
                    | '、'
                    | '：'
                    | ':'
                    | '；'
                    | ';'
                    | '（'
                    | '）'
                    | '('
                    | ')'
                    | '【'
                    | '】'
                    | '['
                    | ']'
                    | '"'
                    | '\''
                    | '?'
                    | '？'
                    | '!'
                    | '！'
            )
    })
    .map(str::trim)
    .filter(|w| w.chars().count() >= 3 && w.chars().any(|c| c.is_alphanumeric()))
    .max_by_key(|w| w.chars().count())
    .filter(|w| !w.is_empty())
    .map(str::to_string)
}

/// 解析时间窗 → 分钟：最近N分/小时/天、今天、刚才/刚刚。封顶 24h。
fn extract_minutes(lower: &str) -> Option<i64> {
    // 刚才/刚刚 → 15 分钟
    if lower.contains("刚才") || lower.contains("刚刚") {
        return Some(15);
    }
    // 今天 → 用默认（较大窗口，这里给 240）
    if lower.contains("今天") {
        return Some(240);
    }
    // 显式「最近N<单位>」/「N<单位>内」/「过去N<单位>」
    let num = parse_leading_number_near(
        lower,
        &["分钟", "分", "小时", "h", "时", "天", "day", "hour", "min"],
    );
    if let Some((n, unit)) = num {
        let minutes = match unit.as_str() {
            "分钟" | "分" | "min" => n,
            "小时" | "时" | "h" | "hour" => n.saturating_mul(60),
            "天" | "day" => n.saturating_mul(1440),
            _ => n,
        };
        return Some(minutes.clamp(1, MAX_WINDOW_MIN));
    }
    None
}

/// 在 `最近/过去/近` 后面找「数字+单位」。返回 (数字, 单位)。
fn parse_leading_number_near(lower: &str, units: &[&str]) -> Option<(i64, String)> {
    // 简化：扫描所有「数字串 + 紧邻单位」组合，取第一个命中已知单位的。
    let bytes = lower.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < n && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let num: i64 = lower[start..i].parse().unwrap_or(0);
            if num > 0 {
                let rest = &lower[i..];
                // 单位按长度降序匹配，避免「分钟」被「分」截断
                let mut sorted: Vec<&&str> = units.iter().collect();
                sorted.sort_by_key(|u| std::cmp::Reverse(u.len()));
                for u in sorted {
                    if rest.starts_with(*u) {
                        return Some((num, (*u).to_string()));
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// 命中「只看错误/只要 error/错误日志」→ ERROR。
fn extract_level(lower: &str) -> Option<String> {
    if lower.contains("只看错误")
        || lower.contains("错误日志")
        || lower.contains("只要错误")
        || lower.contains("error 日志")
        || lower.contains("error日志")
        || lower.contains("only error")
    {
        return Some("ERROR".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_labeled_business_ids_are_not_trace_guesses() {
        assert_eq!(
            extract_trace_id("matchId=abcdef1234567890 nlpIdOriginal=8414548098086706628"),
            None
        );
        assert_eq!(
            extract_trace_id("requestId=abc.def.1234567890123456 traceId=real-trace"),
            Some("real-trace".into())
        );
        assert!(extract_identifiers("2026-10-28 11:31:12", None).is_empty());
        assert!(extract_identifiers("2026-10-28T11:31:12+08:00", None).is_empty());
    }

    #[test]
    fn identifiers_from_business_ids() {
        // 订单号（长数字）
        let a = analyze("查订单 2024102812345 为什么下单失败");
        assert_eq!(a.intent, QaIntent::Troubleshoot);
        assert!(a.keywords.iter().any(|k| k == "2024102812345"), "{a:?}");
        // 手机号
        let b = analyze("用户 13800138000 登录报错了排查下");
        assert!(b.keywords.iter().any(|k| k == "13800138000"), "{b:?}");
        // 字母数字混合码
        let c = analyze("ORD20241028 这单没支付排查");
        assert!(
            c.keywords
                .iter()
                .any(|k| k.eq_ignore_ascii_case("ORD20241028")),
            "{c:?}"
        );
        // 带连字符 requestId，且不与 traceId 冲突
        let d = analyze("requestId req-123-abcdef 这笔超时");
        assert!(d.keywords.iter().any(|k| k == "req-123-abcdef"), "{d:?}");
    }

    #[test]
    fn identifiers_exclude_http_code_and_short() {
        let ids = extract_identifiers("接口 500 报错，只有 12 这种短数字", None);
        assert!(
            !ids.iter().any(|k| k == "500"),
            "HTTP 码不当标识符：{ids:?}"
        );
        assert!(!ids.iter().any(|k| k == "12"), "短数字不当标识符：{ids:?}");
    }

    #[test]
    fn identifiers_keep_order_identifier_before_error() {
        // 订单号 + error → 标识符在前，错误特征在后
        let a = analyze("订单 998877665544 报 NullPointerException");
        assert_eq!(
            a.keywords.first().map(String::as_str),
            Some("998877665544"),
            "{a:?}"
        );
        assert!(
            a.keywords.iter().any(|k| k == "NullPointerException"),
            "{a:?}"
        );
    }

    #[test]
    fn troubleshoot_basic() {
        let a = analyze("帮我排查下线上 500 报错");
        assert_eq!(a.intent, QaIntent::Troubleshoot);
        assert!(a.confidence >= 40);
        assert_eq!(a.keywords.first().map(String::as_str), Some("500"));
    }

    #[test]
    fn doc_not_mistaken_for_trouble() {
        // 「写日志模块怎么实现」不该被「日志」带偏成排障
        let a = analyze("写日志模块怎么实现的");
        assert_eq!(a.intent, QaIntent::DocLookup, "{a:?}");
    }

    #[test]
    fn line_count_is_not_http_code() {
        // 「500 行代码」不该判成 HTTP 5xx → 不是排障
        let a = analyze("这段 500 行代码帮我看下怎么实现");
        assert_ne!(a.intent, QaIntent::Troubleshoot, "{a:?}");
    }

    #[test]
    fn exception_class_drives_troubleshoot_and_keyword() {
        let a = analyze("看下 OrderService 的 NullPointerException 怎么回事");
        assert_eq!(a.intent, QaIntent::Troubleshoot);
        assert_eq!(a.service.as_deref(), Some("OrderService"));
        assert!(
            a.keywords.iter().any(|k| k == "NullPointerException"),
            "{a:?}"
        );
    }

    #[test]
    fn business_symptom_phrases_are_troubleshoot() {
        // 大白话症状描述（无「报错/异常/日志」技术词）也要判排障
        for q in [
            "撮合服务怎么又出问题了",
            "下单一直失败",
            "接口好像卡住了没反应",
            "用户反馈支付没响应",
            "这个功能线上用不了",
            "页面打不开了帮看下",
        ] {
            assert_eq!(analyze(q).intent, QaIntent::Troubleshoot, "应判排障：{q}");
        }
    }

    #[test]
    fn symptom_does_not_overfire_on_pure_doc() {
        // 纯实现/原理问题不能被症状词带偏
        assert_eq!(analyze("这个模块是怎么设计的").intent, QaIntent::DocLookup);
        assert_eq!(analyze("介绍下登录流程").intent, QaIntent::DocLookup);
    }

    #[test]
    fn read_code_and_doc() {
        assert_eq!(analyze("看下源码里怎么写的").intent, QaIntent::ReadCode);
        assert_eq!(analyze("登录功能怎么实现的").intent, QaIntent::DocLookup);
        assert_eq!(analyze("这个类做什么的").intent, QaIntent::DocLookup);
    }

    #[test]
    fn trace_id_forces_troubleshoot() {
        let a = analyze("查一下 a1b2c3d4e5f60718 这条");
        assert_eq!(a.intent, QaIntent::Troubleshoot);
        assert_eq!(a.trace_id.as_deref(), Some("a1b2c3d4e5f60718"));
    }

    #[test]
    fn minutes_parsing() {
        assert_eq!(analyze("排查最近30分钟的报错").minutes, Some(30));
        assert_eq!(analyze("看下最近2小时的异常日志").minutes, Some(120));
        // 封顶 24h：3 天 → 1440
        assert_eq!(analyze("最近3天的线上故障排查").minutes, Some(1440));
        assert_eq!(analyze("刚才接口报错了排查下").minutes, Some(15));
        assert_eq!(analyze("今天线上超时排查").minutes, Some(240));
    }

    #[test]
    fn level_only_error() {
        assert_eq!(
            analyze("排查下只看错误日志").level.as_deref(),
            Some("ERROR")
        );
    }

    #[test]
    fn service_suffix_forms() {
        assert_eq!(
            analyze("排查 user-service 超时").service.as_deref(),
            Some("user-service")
        );
        assert_eq!(
            analyze("payment-api 报错排查").service.as_deref(),
            Some("payment-api")
        );
    }

    #[test]
    fn unicode_context_and_huge_duration_do_not_panic() {
        // 非常见 5xx 会读取左右上下文；窗口边缘故意落在中文/emoji 字节中间。
        for prefix in ["中", "中文", "中文前缀", "🦀中文前缀"] {
            let text = format!("{prefix}状态599响应有问题中文后缀🦀");
            assert!(has_http_error_code(&text));
        }
        assert_eq!(
            analyze("最近9223372036854775807小时排查超时").minutes,
            Some(1440)
        );
        assert_eq!(
            analyze("最近9223372036854775807天排查超时").minutes,
            Some(1440)
        );
    }

    #[test]
    fn explicit_trace_precedes_heuristics_and_numeric_orders() {
        assert_eq!(extract_trace_id("订单202610281234567890"), None);
        assert_eq!(
            extract_trace_id("a1b2c3d4e5f60718 traceId=real-trace"),
            Some("real-trace".into())
        );
        assert_eq!(
            extract_trace_id("排查traceId：1234567890123456超时"),
            Some("1234567890123456".into())
        );
        assert_eq!(
            extract_trace_id("İ中文traceId：abc-123"),
            Some("abc-123".into())
        );
    }

    #[test]
    fn service_adjacent_to_chinese() {
        assert_eq!(
            analyze("排查payment-api超时").service.as_deref(),
            Some("payment-api")
        );
        assert_eq!(
            analyze("看OrderService报错").service.as_deref(),
            Some("OrderService")
        );
    }

    #[test]
    fn helpers_back_compat() {
        assert_eq!(analyze("帮我排查下线上报错").intent, QaIntent::Troubleshoot);
        assert_ne!(analyze("介绍下登录模块").intent, QaIntent::Troubleshoot);
        assert!(mentions_code("帮我看源码"));
        assert!(!mentions_code("这个功能怎么实现"));
        assert_eq!(extract_trace_id("排查登录超时"), None);
    }

    #[test]
    fn sidecar_content_yields_business_phrase_not_account_id() {
        let text = r#"新起会话，排查这段会话
{"msgId":"74456xxx","toAccountId":"1455450650727948288","from":"1688858671020835","text":{"content":"今天五粮液多少钱"}}"#;
        let contents = extract_sidecar_contents(text);
        assert!(
            contents.iter().any(|c| c.contains("五粮液")),
            "{contents:?}"
        );
        assert!(nl_corpus(text).contains("五粮液"));
        assert_eq!(
            extract_business_phrases("今天五粮液多少钱"),
            vec!["五粮液".to_string()]
        );
        let a = analyze(text);
        assert_eq!(a.intent, QaIntent::Troubleshoot);
        assert!(
            a.keywords.iter().any(|k| k == "五粮液"),
            "keywords={:?}",
            a.keywords
        );
        assert!(
            !a.keywords.iter().any(|k| k == "1455450650727948288"),
            "account id must not be primary keyword: {:?}",
            a.keywords
        );
        assert!(is_likely_account_id(text, "1455450650727948288"));
        assert!(!is_likely_account_id(text, "202610281234567890"));
    }
}
