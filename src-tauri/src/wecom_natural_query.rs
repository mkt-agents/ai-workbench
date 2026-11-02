//! DSH 排障的自然语言检索意图补全；纯解析与校验，不含 ACP 或网络调用。
//!
//! 这里的每一次放宽都是因为「模型答对了、形状不完美」曾被整体判死：模型只要在 JSON
//! 前面写一句人话、或套一层 ```json 围栏，`from_str` 就报
//! `expected value at line 1 column 1`，而调用方原先会据此中止整轮排障。
//! 现在的契约是：形状尽量容错，**值的可信度一步不让**——每个进入查询的字面量仍必须
//! 逐字出现在用户原话里，且不得含注入字符。

use regex::Regex;
use serde::Deserialize;

const MAX_VALUE_CHARS: usize = 128;
const MAX_ANCHORS: usize = 3;
const MAX_TARGETS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaturalQueryIntent {
    pub service: Option<String>,
    pub anchors: Vec<String>,
    pub answer_targets: Vec<String>,
    /// 只由宿主设置（`ERROR` 级别的有界兜底）；模型输出不参与该字段。
    pub level: Option<String>,
}

/// 抽取失败的原因。`Display` 只服务诊断日志：用户可见文案一律由
/// [`clarification_text`] 生成，避免把 serde 报文回显到企微。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NaturalQueryError {
    /// 输出里找不到任何完整的 JSON 对象。
    JsonShape,
    /// `confidence` 不是 high。
    LowConfidence,
    /// `anchors` 为空。
    MissingAnchor,
    /// 值不在用户原话里，或含注入字符。
    UnsafeValue(&'static str),
    /// 抽取回合本身没跑完（超时 / 取消 / 请求权限 / 试图调工具）。
    Unavailable,
}

impl NaturalQueryError {
    /// 写进 `wecom_diagnostics` 的稳定 token，前端据此分类。
    pub fn diagnostic(&self) -> &'static str {
        match self {
            Self::JsonShape => "json_shape",
            Self::LowConfidence => "low_confidence",
            Self::MissingAnchor => "missing_anchor",
            Self::UnsafeValue(_) => "unsafe_value",
            Self::Unavailable => "unavailable",
        }
    }
}

impl std::fmt::Display for NaturalQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JsonShape => write!(f, "no json object in model output"),
            Self::LowConfidence => write!(f, "confidence below high"),
            Self::MissingAnchor => write!(f, "no usable anchor"),
            Self::UnsafeValue(field) => write!(f, "{field} rejected as unsafe or non-literal"),
            Self::Unavailable => write!(f, "extraction turn did not complete"),
        }
    }
}

/// 抽取结果的三态处理结果。`Merge` 才允许访问日志平台。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryDisposition {
    Merge(NaturalQueryIntent),
    Clarify { reason: NaturalQueryError },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNaturalQuery {
    #[serde(default)]
    confidence: String,
    service: Option<String>,
    #[serde(default)]
    anchors: Vec<String>,
    #[serde(default)]
    answer_targets: Vec<String>,
}

/// Parse the one-shot DSH extraction response.
///
/// 形状容错（围栏 / 人话前缀 / 多余字段 / 大小写 / 超出条数），但值的约束不变：
/// 只有 high 置信度、且每个值都是用户原话字面量的抽取结果才会变成一次查询。
pub fn parse_high_confidence(
    raw: &str,
    original: &str,
) -> Result<NaturalQueryIntent, NaturalQueryError> {
    let frame = json_frame(raw).ok_or(NaturalQueryError::JsonShape)?;
    // 未知字段被忽略而不是采信：`serde` 只取这里声明的四个字段，
    // 模型多写的 `sql` / `reason` 之类永远不会流进查询。
    let mut value: RawNaturalQuery =
        serde_json::from_str(frame).map_err(|_| NaturalQueryError::JsonShape)?;
    if !value.confidence.trim().eq_ignore_ascii_case("high") {
        return Err(NaturalQueryError::LowConfidence);
    }
    // 模型多给了几个锚点属于「答得更全」，截断而不是判死。
    value.anchors.truncate(MAX_ANCHORS);
    if value.anchors.is_empty() {
        return Err(NaturalQueryError::MissingAnchor);
    }
    value.answer_targets.truncate(MAX_TARGETS);
    let service = validate_value(value.service, original, "service")?;
    let anchors = sanitize_anchors(validate_values(value.anchors, original, "anchors")?);
    if anchors.is_empty() {
        return Err(NaturalQueryError::MissingAnchor);
    }
    let answer_targets = validate_values(value.answer_targets, original, "answerTargets")?;
    Ok(NaturalQueryIntent { service, anchors, answer_targets, level: None })
}

pub fn decide(extraction: Result<NaturalQueryIntent, NaturalQueryError>) -> QueryDisposition {
    match extraction {
        Ok(intent) => QueryDisposition::Merge(intent),
        Err(reason) => QueryDisposition::Clarify { reason },
    }
}

/// 用户可见的澄清句：说清已经识别到什么、还缺什么。
///
/// 不包含 serde 报文、不包含 OAP 原文/URL/凭证；时间用可读形态而不是 RFC3339。
pub fn clarification_text(
    reason: &NaturalQueryError,
    service: Option<&str>,
    window: Option<(&str, &str)>,
) -> String {
    let mut known: Vec<String> = Vec::new();
    if let Some(service) = service.map(str::trim).filter(|s| !s.is_empty()) {
        known.push(format!("服务 {service}"));
    }
    if let Some((start, end)) = window {
        known.push(format!(
            "时间 {} 至 {}",
            readable_timestamp(start),
            readable_timestamp(end)
        ));
    }
    let known = if known.is_empty() {
        "暂未识别出服务或时间范围".to_string()
    } else {
        known.join("、")
    };
    let ask = match reason {
        NaturalQueryError::LowConfidence | NaturalQueryError::MissingAnchor => {
            "还缺一个能缩小范围的检索对象"
        }
        NaturalQueryError::JsonShape
        | NaturalQueryError::UnsafeValue(_)
        | NaturalQueryError::Unavailable => "还没拿到可核对的检索对象",
    };
    format!(
        "【查询条件待补充】已识别：{known}。{ask}（人名、订单号、requestId、错误码、接口路径或 traceId 任一即可）。本次未访问日志平台。"
    )
}

/// `2026-11-02T00:00:00+08:00` → `2026-11-02 00:00:00`；无法识别时原样返回。
pub(crate) fn readable_timestamp(value: &str) -> String {
    let Some((date, rest)) = value.split_once('T') else {
        return value.to_string();
    };
    let time = rest
        .find(['+', 'Z', '-'])
        .map(|i| &rest[..i])
        .unwrap_or(rest);
    let time = time.split('.').next().unwrap_or(time);
    if date.is_empty() || time.is_empty() {
        return value.to_string();
    }
    format!("{date} {time}")
}

/// 从模型原始输出里取一段 JSON 对象。
///
/// 容忍：BOM、```json / ``` 围栏（`\n`、`\r\n`、未闭合都算）、围栏前语言标签、
/// 围栏前后的人话、前导解释。用字符串/转义感知的花括号配对扫描定位，避免把
/// 字符串里的 `}` 当成结束。
fn json_frame(text: &str) -> Option<&str> {
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return None;
    }
    let body = match text.strip_prefix("```") {
        // 跳过 ``` 后面的语言标签行，例如 json / JSON5 / 空标签。
        Some(rest) => match rest.find('\n') {
            Some(newline) => &rest[newline + 1..],
            None => rest,
        },
        None => text,
    };
    let start = body.find('{')?;
    let candidate = &body[start..];
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in candidate.bytes().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&candidate[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 含这些词（**任意位置**）的候选都不是业务实体，而是被主题词/动词/量词包住的残片。
/// 用 `contains` 而不是相等，是为了挡掉「服务报错很多」「重复的文本会话」这类整句切片
/// ——把一句话当关键词会让 OAP 必然零命中，比不查更糟。
const GENERIC_WORDS: &[&str] = &[
    "服务", "应用", "系统", "日志", "错误", "报错", "异常", "问题", "情况", "一下", "帮忙",
    "看看", "看下", "查下", "查一下", "找下", "找一下", "找一找", "搜下", "搜一下", "帮看",
    "瞅瞅", "排查", "是啥", "什么", "怎么", "多少", "哪个", "为啥", "今天",
    "昨天", "明天", "时间", "调用链", "会话", "继续", "线上", "很多", "大量", "一直", "老是",
    "经常", "有些", "这个", "那个", "现在", "之前", "我们", "你们", "他们", "需要", "请问",
    "麻烦", "帮忙看", "新对话", "新会话", "新起会话",
    // 动作/泛化名词：剥完症状词只剩这些，说明句子里根本没有实体。
    "下单", "登录", "支付", "注册", "刷新", "提交", "加载", "打开", "点击", "查看", "查询",
    "处理", "调用", "返回", "请求", "响应", "数据", "页面", "用户", "接口", "订单", "消息",
    "内容", "文案", "功能", "操作", "结果", "状态", "流程", "原因", "办法", "方式", "时候",
    // 小标题/字段标签：用户从企微对话里整段贴过来时，「关键信息」这种 4 字标签会被
    // 当成实体。它们是封闭的一小撮（与 `parse_query` 里既有的协议键名过滤同源），
    // 当关键词会让 OAP 必然零命中。
    "关键信息", "详细信息", "相关信息", "背景信息", "附加信息", "补充信息", "备注信息",
    "上下文", "问题描述", "报错信息", "日志内容", "接口文档", "环境信息",
];

/// Exact search/imperative verbs that must never become OAP keywords.
const SEARCH_VERB_EXACT: &[&str] = &[
    "找下", "找一下", "找一找", "查下", "查一下", "看看", "看下", "看一下", "搜下", "搜一下",
    "帮看", "瞅瞅", "排查", "帮忙",
];

/// 中文实体名的长度上限：`孙武`(2) `五粮液`(3) `企业微信`(4) 都在内，整句切片一定超出。
const CJK_ANCHOR_MAX: usize = 4;

/// Unified quality gate for log-search anchors (local + model share this).
///
/// Rejects verb debris / generics / overlong CJK fragments. Accepts short Chinese
/// proper names, ASCII identifiers, and quoted-style literals that already passed
/// `safe_literal`.
pub fn is_search_anchor(value: &str) -> bool {
    let value = value.trim();
    let len = value.chars().count();
    if len < 2 || len > MAX_VALUE_CHARS {
        return false;
    }
    if !safe_literal(value) {
        return false;
    }
    if SEARCH_VERB_EXACT.iter().any(|w| value == *w) {
        return false;
    }
    if GENERIC_WORDS.iter().any(|word| value.contains(word)) {
        return false;
    }
    let cjk_only = value.chars().all(|c| matches!(c, '\u{4e00}'..='\u{9fff}'));
    if cjk_only && len > CJK_ANCHOR_MAX {
        return false;
    }
    true
}

/// True when at least one keyword is a usable retrieval entity (not verb noise).
pub fn has_usable_search_anchors(keywords: &[String]) -> bool {
    keywords.iter().any(|k| is_search_anchor(k))
}

/// Keep only anchors that pass the quality gate (order preserved).
/// Also collapses coverb/shell supersets: `里孙武` drops when `孙武` is kept.
pub fn sanitize_anchors(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = match normalize_slot_name(&value) {
            Some(normalized) => normalized,
            None => {
                let trimmed = value.trim().to_string();
                if is_search_anchor(&trimmed) {
                    trimmed
                } else {
                    continue;
                }
            }
        };
        if !is_search_anchor(&value) {
            continue;
        }
        out.retain(|seen: &String| {
            !(seen.chars().count() > value.chars().count() && seen.ends_with(&value))
        });
        if out.iter().any(|seen| {
            value.chars().count() > seen.chars().count() && value.ends_with(seen.as_str())
        }) {
            continue;
        }
        if out.iter().any(|seen: &String| seen.eq_ignore_ascii_case(&value)) {
            continue;
        }
        out.push(value);
        if out.len() >= MAX_ANCHORS {
            break;
        }
    }
    out
}

const SLOT_SHELLS: &[&str] = &[
    "服务里", "服务中", "服务内", "系统里", "系统中", "日志里", "平台里", "库里", "表里", "群里",
    "里面", "服务", "系统", "日志", "平台",
];
const SLOT_COVERBS: &[&str] = &[
    "里", "中", "内", "在", "从", "到", "对", "把", "给", "向", "和", "与", "跟", "上",
];

/// `服务里孙武` / `里孙武` / `在订单` → subject before 「的」.
/// Only peels location shells + coverbs; never invents a trailing fragment from a long sentence.
fn normalize_slot_name(raw: &str) -> Option<String> {
    let mut current = strip_trailing_particles(raw.trim());
    for _ in 0..6 {
        let mut stripped = false;
        for shell in SLOT_SHELLS {
            if let Some(rest) = current.strip_prefix(shell) {
                if rest.chars().count() >= 2 {
                    current = rest.trim_start();
                    stripped = true;
                    break;
                }
            }
        }
        if stripped {
            continue;
        }
        for coverb in SLOT_COVERBS {
            if let Some(rest) = current.strip_prefix(coverb) {
                if rest.chars().count() >= 2 {
                    current = rest.trim_start();
                    stripped = true;
                    break;
                }
            }
        }
        if !stripped {
            break;
        }
    }
    let len = current.chars().count();
    let cjk_only = current.chars().all(is_cjk_char);
    if cjk_only && (2..=CJK_ANCHOR_MAX).contains(&len) {
        return Some(current.to_string());
    }
    if !cjk_only && (2..=MAX_VALUE_CHARS).contains(&len) {
        return Some(current.to_string());
    }
    None
}

/// Possessive `X的field` only: if shells left a long CJK run, take a trailing 2–3 name.
fn trailing_possessive_name(raw: &str) -> Option<String> {
    let current = strip_trailing_particles(raw.trim());
    if !current.chars().all(is_cjk_char) {
        return None;
    }
    let chars: Vec<char> = current.chars().collect();
    if chars.len() < 2 {
        return None;
    }
    for take in [2usize, 3] {
        if chars.len() < take {
            continue;
        }
        let candidate: String = chars[chars.len() - take..].iter().collect();
        if is_search_anchor(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// High-confidence structural slots from sentence skeletons (not open business lexicon).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuralSlots {
    pub anchors: Vec<String>,
    pub answer_targets: Vec<String>,
}

/// `孙武的 openId 是啥` → anchors=孙武, answerTargets=openId.
pub fn extract_structural_slots(text: &str) -> StructuralSlots {
    let mut slots = StructuralSlots::default();
    // Allow a longer CJK run before 「的」 so 「服务里孙武的」 keeps the shell for peeling.
    let pattern = Regex::new(
        r"([\u{4e00}-\u{9fff}]{2,12})的\s*([A-Za-z][A-Za-z0-9_]{1,32})",
    )
    .expect("static structural slot regex");
    for capture in pattern.captures_iter(text) {
        let Some(name_m) = capture.get(1) else { continue };
        let Some(field_m) = capture.get(2) else { continue };
        let name = match normalize_slot_name(name_m.as_str()) {
            Some(value) => value,
            None => match trailing_possessive_name(name_m.as_str()) {
                Some(value) => value,
                None => continue,
            },
        };
        let field = field_m.as_str();
        if is_search_anchor(&name) && literal_contains(text, &name) {
            if !slots
                .anchors
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(&name))
            {
                slots.anchors.push(name);
            }
        }
        if safe_literal(field)
            && !slots
                .answer_targets
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(field))
        {
            slots.answer_targets.push(field.to_string());
        }
        if slots.anchors.len() >= MAX_ANCHORS && slots.answer_targets.len() >= MAX_TARGETS {
            break;
        }
    }
    for target in infer_answer_targets(text) {
        if slots
            .answer_targets
            .iter()
            .any(|seen| seen.eq_ignore_ascii_case(&target))
        {
            continue;
        }
        slots.answer_targets.push(target);
        if slots.answer_targets.len() >= MAX_TARGETS {
            break;
        }
    }
    slots.anchors.truncate(MAX_ANCHORS);
    slots.answer_targets.truncate(MAX_TARGETS);
    slots
}

/// 不依赖模型的确定性兜底抽取。
///
/// 复用 [`crate::wecom_intent::extract_business_phrases`]（「今天五粮液多少钱 → 五粮液」
/// 那个词法），再做清洗：剥尾缀虚词、剥首尾症状词、丢泛词。命中即意味着这一问不需要
/// 再花一次 ACP 往返去问模型。
///
/// 只产出 anchors / answerTargets；`service` 恒为 None，因为调用方手里的
/// `parse_query` 结果已经有服务名了。
pub fn infer_intent_from_text(text: &str) -> Option<NaturalQueryIntent> {
    let structural = extract_structural_slots(text);
    let mut anchors = structural.anchors;
    let answer_targets = structural.answer_targets;
    for raw in crate::wecom_intent::extract_business_phrases(text) {
        if anchors.len() >= MAX_ANCHORS {
            break;
        }
        let burst = raw.trim();
        let peeled = normalize_slot_name(burst);
        let candidate = if let Some(ref peeled) = peeled {
            match whole_anchor(peeled) {
                Some(value) => value.to_string(),
                None => match segment_anchor(burst)
                    .or_else(|| segment_anchor(peeled))
                    .map(str::to_string)
                {
                    Some(value) => value,
                    None => continue,
                },
            }
        } else {
            // 没有标点的中文会被 `extract_business_phrases` 整句当一个候选
            // （`孙武登录不上`），这时才需要把症状词从首尾剥掉再试。
            // 绝不对整句做「取末尾 2 字」——那会把「登录不上」收成「不上」。
            match whole_anchor(burst)
                .or_else(|| segment_anchor(burst))
                .map(str::to_string)
            {
                Some(value) => value,
                None => continue,
            }
        };
        if !is_search_anchor(&candidate) {
            continue;
        }
        if anchors.iter().any(|seen| seen.eq_ignore_ascii_case(&candidate)) {
            continue;
        }
        anchors.push(candidate);
    }
    let anchors = sanitize_anchors(anchors);
    if anchors.is_empty() {
        return None;
    }
    Some(NaturalQueryIntent {
        service: None,
        anchors,
        answer_targets,
        level: None,
    })
}

/// 整段就是一个实体（`孙武` / `txmsg` / `openId`）：中文 2–4 字，含 ASCII 的最长 128。
fn whole_anchor(text: &str) -> Option<&str> {
    let value = strip_trailing_particles(text.trim());
    let len = value.chars().count();
    if len < 2 {
        return None;
    }
    let cjk_only = value.chars().all(is_cjk_char);
    if cjk_only && len > CJK_ANCHOR_MAX {
        return None;
    }
    if !cjk_only && len > MAX_VALUE_CHARS {
        return None;
    }
    accept_anchor(text, value)
}

/// 从一段没有标点分隔的中文里切出一个实体名：`孙武登录不上` → `孙武`。
///
/// 把整句当关键词会让 OAP 必然零命中，比不查更糟，所以只接受剥完症状词后仍是
/// **2–4 个汉字**、且不含泛词的结果；剥不干净就返回 None，交给模型或澄清。
pub(crate) fn segment_anchor(text: &str) -> Option<&str> {
    let mut current = text.trim();
    loop {
        let before = current;
        current = strip_trailing_particles(current);
        current = strip_vocabulary_edges(current);
        if current == before {
            break;
        }
    }
    let len = current.chars().count();
    if !(2..=CJK_ANCHOR_MAX).contains(&len) || !current.chars().all(is_cjk_char) {
        return None;
    }
    accept_anchor(text, current)
}

/// 剥掉首尾命中的症状/排障词。单字词（`卡`/`慢`）只剥尾部——从前面剥会把
/// `卡布奇诺` 变成 `布奇诺`。
fn strip_vocabulary_edges(value: &str) -> &str {
    let mut current = value;
    loop {
        let mut changed = false;
        for word in crate::wecom_intent::symptom_vocabulary() {
            if word.is_empty() {
                continue;
            }
            if let Some(rest) = current.strip_suffix(word) {
                current = rest.trim_end();
                changed = true;
            }
            if word.chars().count() > 1 {
                if let Some(rest) = current.strip_prefix(word) {
                    current = rest.trim_start();
                    changed = true;
                }
            }
        }
        if !changed {
            return current;
        }
    }
}

/// 三个入口共用的收尾校验：非泛词、安全字面量、且确实出现在这一段原文里。
fn accept_anchor<'a>(source: &str, value: &'a str) -> Option<&'a str> {
    if !is_search_anchor(value) || !literal_contains(source, value) {
        return None;
    }
    Some(value)
}

fn is_cjk_char(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}')
}

/// 剥掉「的/了/吗/呢/啊/吧」这类尾缀虚词：`孙武的` → `孙武`。
fn strip_trailing_particles(value: &str) -> &str {
    let mut current = value;
    loop {
        let trimmed = current.trim_end();
        let Some(last) = trimmed.chars().next_back() else {
            return trimmed;
        };
        if matches!(last, '的' | '了' | '吗' | '呢' | '啊' | '吧' | '呀') {
            current = &trimmed[..trimmed.len() - last.len_utf8()];
        } else {
            return trimmed;
        }
    }
}

/// 「孙武的 openId 是啥」→ `["openId"]`：只认明确的「问某个字段」句式。
fn infer_answer_targets(text: &str) -> Vec<String> {
    let pattern = Regex::new(r"([A-Za-z][A-Za-z0-9_]{1,32})\s*(?:是啥|是什么|是多少|多少|叫啥|哪个|的值)")
        .expect("static answer target regex");
    let mut out: Vec<String> = Vec::new();
    for capture in pattern.captures_iter(text) {
        let Some(group) = capture.get(1) else { continue };
        let value = group.as_str();
        if !safe_literal(value) {
            continue;
        }
        if out.iter().any(|seen| seen.eq_ignore_ascii_case(value)) {
            continue;
        }
        out.push(value.to_string());
        if out.len() >= MAX_TARGETS {
            break;
        }
    }
    out
}

fn validate_values(
    values: Vec<String>,
    original: &str,
    field: &'static str,
) -> Result<Vec<String>, NaturalQueryError> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim().to_string();
        if value.is_empty() || value.chars().count() > MAX_VALUE_CHARS || !safe_literal(&value) {
            return Err(NaturalQueryError::UnsafeValue(field));
        }
        if !literal_contains(original, &value) {
            return Err(NaturalQueryError::UnsafeValue(field));
        }
        if out.iter().any(|seen: &String| seen.eq_ignore_ascii_case(&value)) {
            return Err(NaturalQueryError::UnsafeValue(field));
        }
        out.push(value);
    }
    Ok(out)
}

fn validate_value(
    value: Option<String>,
    original: &str,
    field: &'static str,
) -> Result<Option<String>, NaturalQueryError> {
    value
        .map(|v| validate_values(vec![v], original, field).map(|mut v| v.pop().unwrap()))
        .transpose()
}

/// 值必须逐字出现在用户原话里。ASCII 值按大小写不敏感比较（`openId` / `openid`
/// 都能对上原话），含中文的值按原样比较。
fn literal_contains(original: &str, value: &str) -> bool {
    if value.is_ascii() {
        original.to_ascii_lowercase().contains(&value.to_ascii_lowercase())
    } else {
        original.contains(value)
    }
}

fn safe_literal(value: &str) -> bool {
    !value.chars().any(|c| c.is_control())
        && !value.contains(['\n', '\r', '\t'])
        && !value.contains(&['&', '|', ';', '`', '$', '<', '>', '{', '}', '[', ']', '(', ')'][..])
        && !value.contains("://")
        && !value.to_ascii_lowercase().contains("select ")
        && !value.to_ascii_lowercase().contains("query")
        && !value.to_ascii_lowercase().split_whitespace().any(|token| matches!(token, "and" | "or" | "not"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORTED: &str = "排查日志，找下今天的日志，txmsg 服务，孙武的 openId是啥？";

    fn perfect_json() -> String {
        r#"{
          "confidence":"high",
          "service":"txmsg",
          "anchors":["孙武"],
          "answerTargets":["openId"]
        }"#
        .to_string()
    }

    #[test]
    fn accepts_literal_entity_and_answer_target_without_querying_target() {
        let original = "排查日志，找下今天的日志，txmsg 服务，孙武 的 openId 是啥？";
        let intent = parse_high_confidence(&perfect_json(), original).unwrap();

        assert_eq!(intent.service.as_deref(), Some("txmsg"));
        assert_eq!(intent.anchors, vec!["孙武"]);
        assert_eq!(intent.answer_targets, vec!["openId"]);
    }

    #[test]
    fn rejects_model_values_not_present_in_the_original_message() {
        let raw = r#"{
          "confidence":"high",
          "service":"orders-service",
          "anchors":["孙武"],
          "answerTargets":["openId"]
        }"#;

        let err = parse_high_confidence(raw, "查 txmsg 服务里孙武的 openId").unwrap_err();
        assert_eq!(err, NaturalQueryError::UnsafeValue("service"));
    }

    #[test]
    fn rejects_non_high_or_anchorless_output_without_falling_back_to_a_broad_query() {
        let low = r#"{"confidence":"low","service":"txmsg","anchors":[],"answerTargets":["openId"]}"#;
        assert_eq!(
            parse_high_confidence(low, "查 txmsg 服务孙武的 openId").unwrap_err(),
            NaturalQueryError::LowConfidence
        );

        let no_anchor =
            r#"{"confidence":"high","service":"txmsg","anchors":[],"answerTargets":["openId"]}"#;
        assert_eq!(
            parse_high_confidence(no_anchor, "查 txmsg 服务孙武的 openId").unwrap_err(),
            NaturalQueryError::MissingAnchor
        );
    }

    /// 未知字段被忽略而非采信：模型多写 `sql` 不构成错误，但注入值照样被拒。
    #[test]
    fn unknown_fields_are_ignored_while_injected_values_stay_rejected() {
        let extra = r#"{"confidence":"high","service":"txmsg","anchors":["孙武"],"answerTargets":[],"sql":"select *"}"#;
        let intent = parse_high_confidence(extra, "查 txmsg 服务孙武").unwrap();
        assert_eq!(intent.anchors, vec!["孙武"]);

        let query = r#"{"confidence":"high","service":"txmsg","anchors":["孙武 AND openId"],"answerTargets":[]}"#;
        assert_eq!(
            parse_high_confidence(query, "查 txmsg 服务孙武 AND openId").unwrap_err(),
            NaturalQueryError::UnsafeValue("anchors")
        );
    }

    /// 这一条钉住回归：模型把 JSON 前后包上人话/围栏时必须仍能解析。
    #[test]
    fn json_frame_accepts_every_observed_shape() {
        let body = r#"{"confidence":"high","service":"txmsg","anchors":["孙武"],"answerTargets":["openId"]}"#;
        let shapes = [
            body.to_string(),
            format!("```json\n{body}\n```"),
            format!("```json\r\n{body}\r\n```"),
            format!("```json\n{body}"),
            format!("```{body}```"),
            format!("根据原话，检索意图如下：\n{body}"),
            format!("\u{feff}  \n{body}"),
            format!("{body}\n以上为抽取结果。"),
            format!("好的。\n```json\n{body}\n```\n需要我继续吗？"),
        ];
        for shape in shapes {
            let intent = parse_high_confidence(&shape, REPORTED)
                .unwrap_or_else(|e| panic!("shape rejected: {e}\n{shape}"));
            assert_eq!(intent.anchors, vec!["孙武"], "{shape}");
        }
    }

    #[test]
    fn json_frame_returns_none_for_pure_prose() {
        let prose = "以下是抽取结果：查不到可用的检索对象";
        assert_eq!(
            parse_high_confidence(prose, REPORTED).unwrap_err(),
            NaturalQueryError::JsonShape
        );
    }

    /// 花括号配对必须感知字符串：`}` 出现在值里不能提前结束。
    #[test]
    fn json_frame_ignores_braces_inside_strings() {
        let raw = r#"{"confidence":"high","service":"txmsg","anchors":["孙武"],"answerTargets":["a}b"]}"#;
        let frame = json_frame(raw).unwrap();
        assert!(frame.ends_with('}'));
        assert!(frame.contains("a}b"));
    }

    #[test]
    fn parse_normalizes_confidence_case_and_truncates_extra_anchors() {
        let raw = r#"{"confidence":"High","service":"txmsg","anchors":["孙武","txmsg","openId","日志"],"answerTargets":[]}"#;
        let intent = parse_high_confidence(raw, "txmsg 服务 孙武 openId 日志").unwrap();
        assert_eq!(intent.anchors.len(), MAX_ANCHORS);
    }

    /// 本次线上回归的原句：不调模型也要拿到「孙武」。
    #[test]
    fn infer_intent_recovers_the_reported_sentence() {
        let intent = infer_intent_from_text(REPORTED).expect("deterministic anchors");
        assert_eq!(intent.service, None);
        assert_eq!(intent.anchors, vec!["孙武"]);
        assert_eq!(intent.answer_targets, vec!["openId"]);
    }

    #[test]
    fn infer_intent_drops_generic_words_and_sentence_fragments() {
        let intent = infer_intent_from_text(REPORTED).unwrap();
        for rejected in ["服务", "是啥", "找下", "找下今天的日志", "日志", "今天"] {
            assert!(
                !intent.anchors.iter().any(|a| a == rejected),
                "{rejected} should not become an anchor"
            );
        }
    }

    #[test]
    fn infer_intent_returns_none_when_only_generic_words_remain() {
        assert!(infer_intent_from_text("查一下今天的日志").is_none());
        assert!(infer_intent_from_text("").is_none());
    }

    /// 把一句话切片当关键词会让 OAP 必然零命中——比不查更糟，所以宁可退回模型/澄清。
    #[test]
    fn infer_intent_never_uses_a_verb_phrase_as_an_anchor() {
        for text in [
            "今天 txmsg 服务报错很多，帮我看看",
            "排查日志，重复的文本会话一直失败",
            "系统一直超时，看看怎么回事",
        ] {
            assert!(
                infer_intent_from_text(text).is_none(),
                "{text} produced {:?}",
                infer_intent_from_text(text)
            );
        }
    }

    #[test]
    fn segment_anchor_pulls_the_entity_out_of_an_unpunctuated_sentence() {
        assert_eq!(segment_anchor("孙武登录不上"), Some("孙武"));
        assert_eq!(segment_anchor("孙武登录不上啊"), Some("孙武")); // 虚词先剥
        assert_eq!(segment_anchor("孙武卡"), Some("孙武")); // 单字症状只剥尾部
        assert_eq!(segment_anchor("卡布奇诺"), Some("卡布奇诺")); // 单字词不从前剥
    }

    #[test]
    fn segment_anchor_refuses_verb_only_remainders_and_long_fragments() {
        // 剥完只剩动作词：不是实体。
        assert_eq!(segment_anchor("下单失败"), None);
        // 剥完仍含泛词。
        assert_eq!(segment_anchor("重复的文本会话"), None);
        // 剥完 5 字，超界。
        assert_eq!(segment_anchor("孙武一直在报错"), None);
        // 本来就只有症状词。
        assert_eq!(segment_anchor("登录不上"), None);
    }

    /// 断点复现：6 字整句绝不能变成锚点，否则 OAP 必然零命中。
    #[test]
    fn infer_intent_never_turns_a_whole_sentence_into_an_anchor() {
        let intent = infer_intent_from_text("孙武登录不上").expect("应切出实体");
        assert_eq!(intent.anchors, vec!["孙武"]);
        assert!(intent.service.is_none());
        assert!(infer_intent_from_text("登录不上").is_none());
        assert!(infer_intent_from_text("下单失败了，帮我看看").is_none());
    }

    #[test]
    fn clarify_never_leaks_extractor_internals() {
        let text = clarification_text(
            &NaturalQueryError::JsonShape,
            Some("txmsg"),
            Some(("2026-11-02T00:00:00+08:00", "2026-11-02T19:25:26+08:00")),
        );
        for banned in ["expected value", "line 1 column", "serde", "JSON"] {
            assert!(!text.contains(banned), "{banned} leaked: {text}");
        }
        assert!(text.contains("txmsg"), "{text}");
        assert!(text.contains("2026-11-02 00:00:00"), "{text}");
        assert!(text.contains("2026-11-02 19:25:26"), "{text}");
    }

    #[test]
    fn clarify_states_missing_window_instead_of_dropping_it_silently() {
        let text = clarification_text(&NaturalQueryError::Unavailable, None, None);
        assert!(text.contains("暂未识别出服务或时间范围"), "{text}");
    }

    #[test]
    fn decide_routes_ok_to_merge_and_err_to_clarify() {
        let ok = parse_high_confidence(&perfect_json(), REPORTED).unwrap();
        assert!(matches!(
            decide(Ok(ok)),
            QueryDisposition::Merge(_)
        ));
        assert_eq!(
            decide(Err(NaturalQueryError::JsonShape)),
            QueryDisposition::Clarify { reason: NaturalQueryError::JsonShape }
        );
    }

    #[test]
    fn structural_slots_pull_name_and_field() {
        let slots = extract_structural_slots(REPORTED);
        assert_eq!(slots.anchors, vec!["孙武"]);
        assert_eq!(slots.answer_targets, vec!["openId"]);
    }

    #[test]
    fn is_search_anchor_rejects_verb_debris() {
        for bad in ["找下", "查一下", "看看", "找下今天的日志", "日志"] {
            assert!(!is_search_anchor(bad), "{bad}");
        }
        assert!(is_search_anchor("孙武"));
        assert!(is_search_anchor("timeout"));
    }

    /// `natural_needed` 用同一扇门：只有动词残片时视为无可用锚点，强制模型补抽。
    #[test]
    fn usable_anchor_gate_matches_orchestrator_natural_needed() {
        assert!(!has_usable_search_anchors(&[
            "找下".into(),
            "日志".into()
        ]));
        assert!(has_usable_search_anchors(&["孙武".into()]));
        assert!(has_usable_search_anchors(&[
            "timeout".into(),
            "找下".into()
        ]));
    }

    #[test]
    fn model_verb_anchor_is_rejected() {
        let raw = r#"{"confidence":"high","service":"txmsg","anchors":["找下"],"answerTargets":["openId"]}"#;
        assert_eq!(
            parse_high_confidence(raw, REPORTED).unwrap_err(),
            NaturalQueryError::MissingAnchor
        );
    }

    #[test]
    fn model_verb_mixed_with_literal_drops_only_the_verb() {
        let raw = r#"{"confidence":"high","service":"txmsg","anchors":["找下","孙武"],"answerTargets":["openId"]}"#;
        let intent = parse_high_confidence(raw, REPORTED).expect("entity survives sanitize");
        assert_eq!(intent.anchors, vec!["孙武"]);
        assert_eq!(intent.answer_targets, vec!["openId"]);
    }

    #[test]
    fn blanked_remaining_verb_is_not_an_anchor() {
        // After parse_window blanks 「今天」, phrase debris looks like this.
        assert!(infer_intent_from_text("排查日志，找下 的日志，txmsg 服务").is_none());
        assert!(!infer_intent_from_text("找下的日志")
            .map(|i| i.anchors.iter().any(|a| a == "找下"))
            .unwrap_or(false));
    }

    #[test]
    fn variant_phrasings_still_recover_sunwu() {
        for text in [
            "新对话，排查日志，找下今天的日志，txmsg 服务，孙武的 openId 是啥？",
            "帮我看下 txmsg 里孙武的 openId",
            "txmsg 服务 孙武 openId",
        ] {
            let intent = infer_intent_from_text(text).expect(text);
            assert!(
                intent.anchors.iter().any(|a| a == "孙武"),
                "{text} -> {:?}",
                intent.anchors
            );
            assert!(
                !intent
                    .anchors
                    .iter()
                    .any(|a| a == "找下" || a == "里孙武" || a == "务里孙武" || a.ends_with("里孙武")),
                "{text} -> {:?}",
                intent.anchors
            );
        }
    }
}
