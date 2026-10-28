//! Deterministic follow-up protocol. No network/process execution; only bounded source reads.
use crate::wecom_investigation_query::InvestigationQuery;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Component, Path},
};

const MAX_SOURCE_BYTES: u64 = 512 * 1024;

#[derive(Debug)]
pub struct LookupPlan {
    /// Only arguments supported by wecom_skywalking::run, never model-supplied executable text.
    pub queries: Vec<Value>,
    /// Includes per-query rationale and explicit source-only candidate labels.
    pub reason: String,
    pub conclusion_level: String,
    pub gaps: Vec<String>,
    pub stop_reason: String,
}

// Missing is optional, explicit null is not part of this protocol.
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePlan {
    queries: Vec<WireQuery>,
    reason: String,
    #[serde(default, deserialize_with = "present")]
    conclusion_level: Option<String>,
    #[serde(default, deserialize_with = "present")]
    gaps: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present")]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireQuery {
    #[serde(default, deserialize_with = "present")]
    service: Option<String>,
    #[serde(default, deserialize_with = "present")]
    trace_id: Option<String>,
    #[serde(default, deserialize_with = "present")]
    keywords: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present")]
    start: Option<String>,
    #[serde(default, deserialize_with = "present")]
    end: Option<String>,
    reason: String,
    #[serde(default, deserialize_with = "present")]
    evidence_refs: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present")]
    code_ref: Option<CodeRef>,
    #[serde(default, deserialize_with = "present")]
    context_anchor: Option<String>,
    #[serde(default, deserialize_with = "present")]
    context_widen: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    page: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    page_size: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeRef {
    path: String,
    line: usize,
    excerpt: String,
}

fn nonempty(value: &str, limit: usize, name: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().count() > limit {
        return Err(format!(
            "plan_error: {name} must be nonempty and at most {limit} characters"
        ));
    }
    Ok(())
}

fn literal(value: &str, name: &str) -> Result<(), String> {
    nonempty(value, 256, name)?;
    let lower = value.to_ascii_lowercase();
    let command = lower.split_whitespace().next().unwrap_or("");
    let looks_like_command = lower.split_whitespace().nth(1).is_some()
        && [
            "sh",
            "bash",
            "cmd",
            "cmd.exe",
            "powershell",
            "powershell.exe",
            "pwsh",
            "curl",
            "wget",
            "rm",
            "del",
            "invoke-expression",
            "iex",
        ]
        .contains(&command);
    if looks_like_command
        || value != value.trim()
        || value
            .chars()
            .any(|c| c.is_control() || "`$;|&{}<>".contains(c))
        || lower.contains("://")
        || lower.starts_with("www.")
        || lower.starts_with("//")
        || ["javascript:", "data:", "file:", "mailto:"]
            .iter()
            .any(|p| lower.starts_with(p))
    {
        return Err(format!(
            "plan_error: {name} must be a literal, not URL/shell/GraphQL"
        ));
    }
    Ok(())
}

fn json_body(text: &str) -> Result<&str, String> {
    let text = text.trim();
    if let Some(body) = text
        .strip_prefix("```json\n")
        .or_else(|| text.strip_prefix("```json\r\n"))
    {
        return body
            .strip_suffix("```")
            .map(str::trim)
            .filter(|s| !s.contains("```"))
            .ok_or_else(|| "plan_error: expected exactly one json fence".into());
    }
    Ok(text)
}

fn evidence_id(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    let end = rest.find(']')?;
    let id = &rest[..end];
    let local_id = if let Some(round) = id.strip_prefix('R') {
        let end = round.find(['L', 'T'])?;
        let digits = &round[..end];
        if digits.is_empty()
            || digits.starts_with('0')
            || !digits.bytes().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        &round[end..]
    } else {
        id
    };
    let digits = local_id
        .strip_prefix('L')
        .or_else(|| local_id.strip_prefix('T'))?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let suffix = &rest[end + 1..];
    if !suffix.is_empty() && !suffix.starts_with(char::is_whitespace) {
        return None;
    }
    Some(id)
}

/// A label is a real record header, not an arbitrary L1 substring in prose.
fn referenced_text<'a>(evidence: &'a str, id: &str) -> Option<&'a str> {
    if evidence_id(&format!("[{id}]")) != Some(id) {
        return None;
    }
    let mut start = None;
    let mut offset = 0;
    for line in evidence.split_inclusive('\n') {
        if let Some(found) = evidence_id(line.trim_end_matches(['\r', '\n'])) {
            if let Some(begin) = start {
                return Some(&evidence[begin..offset]);
            }
            if found == id {
                start = Some(offset);
            }
        }
        offset += line.len();
    }
    start.map(|begin| &evidence[begin..])
}

/// Header metadata only: body text and model fields never establish a context boundary.
fn header_field<'a>(header: &'a str, key: &str) -> Result<Option<&'a str>, String> {
    let prefix = format!("{key}=");
    let mut values = header
        .split_whitespace()
        .filter_map(|token| token.strip_prefix(&prefix));
    let value = values.next();
    if values.next().is_some() {
        return Err(format!("plan_error: ambiguous anchor header {key}"));
    }
    Ok(value.filter(|value| !value.is_empty()))
}

fn context_args(
    id: &str,
    widen: bool,
    evidence: &str,
    original: &InvestigationQuery,
) -> Result<Value, String> {
    let block = referenced_text(evidence, id)
        .ok_or_else(|| format!("plan_error: missing context anchor [{id}]"))?;
    if !id.contains('L')
        || evidence
            .lines()
            .filter(|line| evidence_id(line) == Some(id))
            .count()
            != 1
    {
        return Err("plan_error: context_anchor requires one unambiguous log header".into());
    }
    let header = block.lines().next().unwrap_or("");
    let service =
        header_field(header, "service")?.ok_or("plan_error: anchor header requires service")?;
    let instance =
        header_field(header, "instance")?.ok_or("plan_error: anchor header requires instance")?;
    literal(service, "anchor.service")?;
    literal(instance, "anchor.instance")?;
    let timestamp = if let Some(ts) = header_field(header, "ts")? {
        DateTime::parse_from_rfc3339(ts).ok().or_else(|| {
            ts.parse::<i64>()
                .ok()
                .and_then(DateTime::<Utc>::from_timestamp_millis)
                .map(|ts| ts.fixed_offset())
        })
    } else {
        let timestamps: Vec<DateTime<FixedOffset>> = header
            .split_whitespace()
            .filter_map(|token| DateTime::parse_from_rfc3339(token).ok())
            .collect();
        if timestamps.len() == 1 {
            timestamps.first().copied()
        } else {
            None
        }
    }
    .ok_or("plan_error: anchor header requires unambiguous ts milliseconds or RFC3339")?;
    let start = DateTime::parse_from_rfc3339(&original.start)
        .map_err(|_| "plan_error: invalid original start")?;
    let end = DateTime::parse_from_rfc3339(&original.end)
        .map_err(|_| "plan_error: invalid original end")?;
    if start >= end || timestamp < start || timestamp > end {
        return Err("plan_error: context anchor lies outside original window".into());
    }
    let radius = Duration::seconds(if widen { 30 } else { 2 });
    let from = timestamp
        .checked_sub_signed(radius)
        .unwrap_or(start)
        .max(start);
    let to = timestamp.checked_add_signed(radius).unwrap_or(end).min(end);
    if from >= to {
        return Err("plan_error: empty context window".into());
    }
    let mut args = json!({"kind":"logs", "service":service, "instance":instance,
        "start":from.to_rfc3339(), "end":to.to_rfc3339(),
        "timezone_offset_minutes":original.timezone_offset_minutes,
        "context_anchor":id, "context_candidate":true, "context_widen":widen});
    // Preserve the real correlation anchor for supplemental matching; the model still
    // cannot supply trace_id alongside context_anchor.
    if let Some(trace) = header_field(header, "trace")? {
        literal(trace, "anchor.trace")?;
        args["trace_id"] = json!(trace);
    }
    for key in ["thread", "logger"] {
        if let Some(value) = header_field(header, key)? {
            literal(value, key)?;
            args[key] = json!(value);
        }
    }
    Ok(args)
}

fn evidence_matches(value: &str, evidence: &str, refs: &[&str]) -> bool {
    // Deliberately conservative textual evidence, never infer an entity from an ID alone.
    evidence.contains(value) && (refs.is_empty() || refs.iter().any(|block| block.contains(value)))
}

fn linked(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return metadata.file_attributes() & 0x400 != 0; // All reparse points, including junctions.
    }
    #[cfg(not(windows))]
    false
}

fn source_excerpt(code: &CodeRef, repo: &str) -> Result<String, String> {
    nonempty(&code.path, 1024, "code_ref.path")?;
    nonempty(&code.excerpt, 2048, "code_ref.excerpt")?;
    if code.line == 0 || code.excerpt.contains(['\r', '\n']) {
        return Err("plan_error: code_ref requires a positive line and single-line excerpt".into());
    }
    // Reject both separator conventions even when tests run on Unix; also excludes ADS/drive paths.
    if code.path.contains(':')
        || code
            .path
            .split(['/', '\\'])
            .any(|s| s == ".." || s.is_empty())
    {
        return Err("plan_error: source path must be workspace-relative without traversal".into());
    }
    let normalized = code.path.replace('\\', "/");
    let relative = Path::new(&normalized);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("plan_error: source path must contain only normal relative components".into());
    }
    let root = fs::canonicalize(repo).map_err(|_| "plan_error: workspace is not available")?;
    if !root.is_dir() {
        return Err("plan_error: workspace must be a directory".into());
    }
    let mut path = root.clone();
    for component in relative.components() {
        path.push(component);
        let meta =
            fs::symlink_metadata(&path).map_err(|_| "plan_error: source path is not available")?;
        if linked(&meta) {
            return Err("plan_error: source symlink/reparse point is not accepted".into());
        }
    }
    let canonical =
        fs::canonicalize(&path).map_err(|_| "plan_error: source path resolution failed")?;
    if !canonical.starts_with(&root) {
        return Err("plan_error: source escapes workspace".into());
    }
    let file = fs::File::open(&canonical).map_err(|_| "plan_error: source open failed")?;
    let meta = file
        .metadata()
        .map_err(|_| "plan_error: source metadata failed")?;
    if !meta.is_file() || meta.len() > MAX_SOURCE_BYTES {
        return Err("plan_error: source must be a regular file at most 512 KiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|_| "plan_error: source read failed")?;
    let text = String::from_utf8(bytes).map_err(|_| "plan_error: source must be UTF-8")?;
    let line = text
        .lines()
        .nth(code.line - 1)
        .ok_or("plan_error: source line does not exist")?;
    if !line.contains(&code.excerpt) {
        return Err("plan_error: excerpt does not match source line".into());
    }
    Ok(code.excerpt.clone())
}

pub fn parse_plan(
    text: &str,
    original: &InvestigationQuery,
    evidence: &str,
    repo: &str,
) -> Result<LookupPlan, String> {
    if text.len() > 32 * 1024 {
        return Err("plan_error: plan exceeds 32 KiB".into());
    }
    let plan: WirePlan = serde_json::from_str(json_body(text)?)
        .map_err(|e| format!("plan_error: strict JSON schema: {e}"))?;
    nonempty(&plan.reason, 2048, "reason")?;
    if plan.queries.len() > 3 {
        return Err("plan_error: at most three queries".into());
    }
    let conclusion_level = plan.conclusion_level.unwrap_or_else(|| "unknown".into());
    if !["result", "direct_cause", "trigger_verified", "unknown"]
        .contains(&conclusion_level.as_str())
    {
        return Err("plan_error: invalid conclusion_level".into());
    }
    let gaps = plan.gaps.unwrap_or_default();
    if gaps.len() > 20 {
        return Err("plan_error: at most twenty gaps".into());
    }
    for gap in &gaps {
        nonempty(gap, 2048, "gaps item")?;
    }
    if conclusion_level == "trigger_verified" {
        let cites_record = evidence
            .lines()
            .filter_map(evidence_id)
            .any(|id| plan.reason.contains(&format!("[{id}]")));
        if !gaps.is_empty() || !cites_record {
            return Err("plan_error: trigger_verified requires no gaps and reason citing actual [evidence_id]; a citation alone does not prove causality".into());
        }
    }
    let stop_reason = plan.stop_reason.unwrap_or_else(|| plan.reason.clone());
    nonempty(&stop_reason, 2048, "stop_reason")?;
    let mut result = LookupPlan {
        queries: Vec::new(),
        reason: plan.reason,
        conclusion_level,
        gaps,
        stop_reason,
    };
    // A stop plan does not require filesystem or time parsing.
    if plan.queries.is_empty() {
        return Ok(result);
    }
    let original_start = DateTime::parse_from_rfc3339(&original.start)
        .map_err(|_| "plan_error: invalid original start")?;
    let original_end = DateTime::parse_from_rfc3339(&original.end)
        .map_err(|_| "plan_error: invalid original end")?;
    if original_start >= original_end {
        return Err("plan_error: invalid original window".into());
    }
    for (index, q) in plan.queries.into_iter().enumerate() {
        nonempty(&q.reason, 2048, "query.reason")?;
        let page = q.page.unwrap_or(1);
        let page_size = q.page_size.unwrap_or(30);
        if !(1..=100).contains(&page) || !(1..=40).contains(&page_size) {
            return Err("plan_error: page must be 1..100 and page_size 1..40".into());
        }
        if let Some(id) = &q.context_anchor {
            if q.service.is_some()
                || q.trace_id.is_some()
                || q.keywords.is_some()
                || q.start.is_some()
                || q.end.is_some()
                || q.code_ref.is_some()
            {
                return Err("plan_error: context query boundaries derive only from anchor; omit service/trace_id/keywords/start/end/code_ref".into());
            }
            for reference in q.evidence_refs.as_deref().unwrap_or(&[]) {
                referenced_text(evidence, reference)
                    .ok_or_else(|| format!("plan_error: missing evidence record [{reference}]"))?;
            }
            let mut args = context_args(id, q.context_widen.unwrap_or(false), evidence, original)?;
            args["page"] = json!(page);
            args["page_size"] = json!(page_size);
            result.reason.push_str(&format!(
                "\nQ{}: {} [context candidate; 邻近上下文候选，非业务命中或根因证据]",
                index + 1,
                q.reason
            ));
            result.queries.push(args);
            continue;
        }
        if q.context_widen.is_some() {
            return Err("plan_error: context_widen requires context_anchor".into());
        }
        let start = q.start.as_deref().unwrap_or(&original.start);
        let end = q.end.as_deref().unwrap_or(&original.end);
        let from =
            DateTime::parse_from_rfc3339(start).map_err(|_| "plan_error: start must be RFC3339")?;
        let to =
            DateTime::parse_from_rfc3339(end).map_err(|_| "plan_error: end must be RFC3339")?;
        if from < original_start || to > original_end || from >= to {
            return Err("plan_error: follow-up must stay inside original window".into());
        }
        let mut refs = Vec::new();
        for id in q.evidence_refs.as_deref().unwrap_or(&[]) {
            refs.push(
                referenced_text(evidence, id)
                    .ok_or_else(|| format!("plan_error: missing evidence record [{id}]"))?,
            );
        }
        let excerpt = q
            .code_ref
            .as_ref()
            .map(|code| source_excerpt(code, repo))
            .transpose()?;
        let mut source_candidate = false;
        let keywords = q.keywords.as_deref().unwrap_or(&[]);
        if keywords.len() > 3 {
            return Err("plan_error: at most three keywords".into());
        }
        // An evidenced trace remains an exact correlation anchor. Without one, only the
        // header-derived context branch may leave the original business ID.
        let original_id = |value: &str| original.identifiers.iter().any(|id| id.value == value);
        if q.trace_id.is_none()
            && !original.identifiers.is_empty()
            && !keywords.iter().any(|word| original_id(word))
        {
            return Err("plan_error: ordinary follow-up must retain an original business identifier; use context_anchor for bounded context".into());
        }
        for (name, value) in q
            .service
            .iter()
            .map(|s| ("service", s))
            .chain(keywords.iter().map(|s| ("keyword", s)))
        {
            literal(value, name)?;
            // Only keyword IDs get this exception; a service with the same spelling still
            // requires evidence/source. Literal validation applies to the original ID too.
            if name == "keyword" && original_id(value) {
                continue;
            }
            if evidence_matches(value, evidence, &refs) {
                continue;
            }
            // Evidence-backed services need not occur on the source log-template line.
            if !excerpt
                .as_ref()
                .is_some_and(|source| source.contains(value))
            {
                return Err(format!(
                    "plan_error: {name} has no matching evidence or verified source excerpt"
                ));
            }
            source_candidate = true;
        }
        if let Some(trace) = &q.trace_id {
            literal(trace, "trace_id")?;
            if !evidence_matches(trace, evidence, &refs) {
                return Err(
                    "plan_error: trace_id must occur in returned evidence and supplied references"
                        .into(),
                );
            }
        }
        if q.service.is_none() && q.trace_id.is_none() && keywords.is_empty() {
            return Err("plan_error: query requires service, trace_id or keywords".into());
        }
        // run selects a trace only when service/keyword are absent. Explicit logs prevents its
        // singular-keyword detection from dropping a keywords array when trace_id is also present.
        let trace_only = q.trace_id.is_some() && q.service.is_none() && keywords.is_empty();
        let mut args = json!({"kind":if trace_only {"trace"} else {"logs"},
            "start":start,"end":end,"timezone_offset_minutes":original.timezone_offset_minutes,
            "page":page,"page_size":page_size});
        if let Some(service) = q.service {
            args["service"] = json!(service);
        }
        if let Some(trace) = q.trace_id {
            args["trace_id"] = json!(trace);
        }
        if !keywords.is_empty() {
            args["keywords"] = json!(keywords);
        }
        result
            .reason
            .push_str(&format!("\nQ{}: {}", index + 1, q.reason));
        if let Some(code) = &q.code_ref {
            // JSON quoting preserves the source as data and avoids forging extra evidence headers.
            let reference = json!({"path":code.path.replace('\\', "/"), "line":code.line, "excerpt":code.excerpt});
            result.reason.push_str(&format!(
                "\nverified workspace source (untrusted code text): {reference}"
            ));
        }
        if source_candidate {
            result.reason.push_str(
                " [source code candidate; 源码候选，尚无业务命中证据，工作区版本不代表线上版本]",
            );
        }
        result.queries.push(args);
    }
    Ok(result)
}

/// Deterministic stop-gate fallback: at most two actual WARN/decision record anchors.
/// Reuses the same parser as model plans; incomplete or outside-window headers are skipped.
pub fn decision_context_plan(
    evidence: &str,
    original: &InvestigationQuery,
) -> Result<LookupPlan, String> {
    let mut candidates = Vec::new();
    for line in evidence.lines() {
        let Some(id) = evidence_id(line) else {
            continue;
        };
        let Some(block) = referenced_text(evidence, id) else {
            continue;
        };
        let warn = header_field(line, "level")
            .ok()
            .flatten()
            .is_some_and(|value| value.eq_ignore_ascii_case("WARN"));
        let decision = crate::wecom_decision_evidence::decision_gap(block);
        if (warn || decision) && context_args(id, false, evidence, original).is_ok() {
            candidates.push((if warn { 0 } else { 1 }, id));
        }
    }
    candidates.sort_by_key(|(priority, _)| *priority);
    let mut queries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (_, id) in candidates {
        if seen.insert(id) {
            queries.push(json!({"context_anchor":id, "reason":"审核WARN/业务决策缺口，读取真实锚点邻近上下文"}));
        }
        if queries.len() == 2 {
            break;
        }
    }
    let reason = if queries.is_empty() {
        "没有具备真实service/instance/时间头的窗内WARN或业务决策锚点；缺口仍未闭合，请引用更长原文excerpt。"
    } else {
        "停止前补查真实WARN/业务决策锚点；上下文仅为候选，待核验具体规则与输入。"
    };
    parse_plan(
        &json!({"queries":queries, "reason":reason, "conclusion_level":"unknown",
        "gaps":["具体触发规则与输入尚待验证"], "stop_reason":reason})
        .to_string(),
        original,
        evidence,
        "",
    )
}

pub fn planning_instruction() -> String {
    r#"你正在制定只读补查计划，不是输出最终业务结论。只输出严格JSON（可用单个json代码围栏），固定schema：
{"queries":[{"service":"可省略","trace_id":"可省略","keywords":["可省略，最多3词，每词最多256字符"],"start":"可省略，RFC3339","end":"可省略，RFC3339","page":1,"page_size":30,"reason":"必填补查理由","evidence_refs":["L1","T1"],"code_ref":{"path":"工作区相对源码路径","line":1,"excerpt":"该行真实的非空单行片段"}}],"reason":"必填整体理由，引用实际[证据ID]","conclusion_level":"unknown","gaps":["未验证事项；缺少的具体规则/输入/原文；对应证据ID；下一步验证"],"stop_reason":"停止或继续的实际理由"}
conclusion_level只允许result/direct_cause/trigger_verified/unknown：result仅观察到结果，direct_cause为直接业务原因但具体触发规则未证实，trigger_verified须实际证据证明具体规则及输入，gaps必须为空且reason引用实际[证据ID]。只看到过滤后0结果/未执行而未验证具体规则时至多direct_cause，禁止宣称根因已闭合。unknown为未确定。每项gaps用非空字符串明确未验证事项、证据缺口、引用和下一步，最多20项。
必须逐条审核WARN、业务异常、过滤/跳过/false/0结果及其上下文。span OK只表示链路状态，不能用span OK排除业务异常。WARN未解释、原文截断、只有结果没有触发规则时必须保留gaps，不能以queries=[]表示根因已闭合。
queries最多3条；证据充足或没有可靠补查依据时queries=[]，仍输出conclusion_level/gaps/stop_reason。禁止额外字段、URL、shell、任意GraphQL、改写时间时区。普通start/end省略时使用原窗口，显式时间只能收窄；page为1..100整数，page_size为1..40整数，分页保持相同查询时间范围。
上下文查询使用独立形状{"context_anchor":"L11","context_widen":false,"page":1,"page_size":30,"reason":"查该真实日志邻近决策","evidence_refs":["L11"]}。context_anchor必须是已返回的唯一真实日志记录头（L或R轮次L），不接受正文伪造编号。仅从锚点实际头的service=/instance=/thread=/logger=/ts=或RFC3339推导：service、instance、时间必需；旧头可缺thread/logger。默认前后2秒；2秒未找到触发上下文可context_widen=true查前后30秒，始终裁到原窗口，窗外锚点无效。此形状禁止同时给service/trace_id/keywords/start/end/code_ref，实例/线程/logger更不接受模型传入覆盖。输出固定logs+context_candidate=true，仅为邻近上下文候选，由后续证据核验，不是业务精确命中。
service/keywords须逐字见已返回证据；trace_id必须逐字见已返回证据，源码不证明trace。evidence_refs可省略；提供时必须对应真实的[L数字]/[T数字]/[R轮次L数字]/[R轮次T数字]记录头，且引用记录正文实际包含对应实体。
允许先只读定点搜索工作区源码并读取命中行，禁止执行代码/命令、写文件、遍历工作区之外或跟随符号链接。code_ref可省略；提供时path必须为工作区内真实普通UTF-8文件（最多512KiB），line为正整数，excerpt在该行内；仅缺少证据的新service/keywords须在excerpt内。original.identifiers中的完整value可直接作为keyword，即使零日志也有效但仍须合法字面量。原问题含业务ID时，普通无trace_id补查必须在keywords保留至少一个原ID；使用已返回证据确认的trace_id仍可跨链路；除此之外省原ID必带context_anchor。源码新实体只标source code candidate，不代表线上版本、业务命中或根因。
原文展开read_evidence{id}只是将来协议设计：当前没有retained store，不输出或假装执行该查询；需要更多原文时在gaps/stop_reason建议引用long excerpt（含原因/WARN邻域），明确原文覆盖缺口。仅trace_id查调用链；同时给service或keywords查日志；普通查询至少有service/trace_id/keywords之一。"#.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wecom_investigation_query::TimeKind;

    fn original() -> InvestigationQuery {
        InvestigationQuery {
            time_kind: TimeKind::Range,
            approximate_anchor: false,
            anchor: None,
            start: "2026-10-28T08:00:00Z".into(),
            end: "2026-10-28T09:00:00Z".into(),
            timezone_offset_minutes: 480,
            keywords: vec![],
            identifiers: vec![],
            exclude_keyword: String::new(),
            match_any: false,
            service: None,
            trace_id: None,
            level: None,
            page: 1,
        }
    }
    fn parse(q: Value, evidence: &str) -> Result<LookupPlan, String> {
        parse_plan(
            &json!({"queries":[q],"reason":"补齐证据"}).to_string(),
            &original(),
            evidence,
            ".",
        )
    }
    #[test]
    fn pure_json_and_one_fence() {
        let text = r#"{"queries":[{"service":"orders","keywords":["timeout"],"reason":"日志验证","evidence_refs":["L1"]}],"reason":"补查"}"#;
        let evidence = "[L1] service=orders\ntimeout";
        for input in [text.to_string(), format!("```json\n{text}\n```")] {
            let plan = parse_plan(&input, &original(), evidence, "missing-repo").unwrap();
            assert_eq!(plan.queries[0]["kind"], "logs");
            assert_eq!(plan.queries[0]["timezone_offset_minutes"], 480);
            assert_eq!(plan.queries[0]["start"], original().start);
            assert!(plan.queries[0].get("reason").is_none());
        }
        assert!(parse_plan(&format!("before {text}"), &original(), evidence, ".").is_err());
        assert!(parse_plan(&format!("{text} {text}"), &original(), evidence, ".").is_err());
    }
    #[test]
    fn time_window_and_timezone_are_not_widened() {
        for (start, end) in [
            ("2026-10-28T07:59:59Z", "2026-10-28T09:00:00Z"),
            ("2026-10-28T08:00:00Z", "2026-10-28T09:00:01Z"),
            ("2026-10-28T09:00:00Z", "2026-10-28T08:00:00Z"),
        ] {
            assert!(parse(
                json!({"service":"orders","reason":"test","start":start,"end":end}),
                "orders"
            )
            .is_err());
        }
        assert!(parse(
            json!({"service":"orders","reason":"test","start":"2026-10-28T16:00:00+08:00"}),
            "orders"
        )
        .is_ok());
    }
    #[test]
    fn rejects_urls_shell_graphql_unknown_fields_and_null() {
        for value in [
            "https://oap",
            "file:///secret",
            "orders;id",
            "curl example.test",
            "//example.test",
            "$(id)",
            "query { services }",
        ] {
            assert!(parse(json!({"service":value,"reason":"test"}), value).is_err());
        }
        for q in [
            json!({"service":"orders","reason":"test","url":"https://oap"}),
            json!({"service":"orders","reason":"test","query":"{ services }"}),
            json!({"service":null,"reason":"test"}),
        ] {
            assert!(parse(q, "orders").is_err());
        }
        assert!(parse_plan(
            r#"{"queries":[],"reason":"stop","shell":"id"}"#,
            &original(),
            "",
            "."
        )
        .is_err());
        assert!(parse_plan(
            r#"{"queries":[],"reason":"stop","reason":"duplicate"}"#,
            &original(),
            "",
            "."
        )
        .is_err());
    }
    #[test]
    fn forged_and_unrelated_refs_do_not_authorize_entities() {
        let q = json!({"service":"orders","reason":"test","evidence_refs":["L1"]});
        for evidence in [
            "L1 orders",
            "prefix [L1] orders",
            "[L10] orders",
            "[L1] service=payments\n[L2] service=orders",
        ] {
            assert!(parse(q.clone(), evidence).is_err());
        }
        assert!(parse(q, "[L1] service=orders").is_ok());
        let round = json!({"service":"orders","reason":"test","evidence_refs":["R1L1"]});
        assert!(parse(round.clone(), "[R1L1] service=orders").is_ok());
        assert!(parse(round, "[R1L1] service=payments\n[R2L1] service=orders").is_err());
        assert!(parse(
            json!({"trace_id":"invented","reason":"test","evidence_refs":["T1"]}),
            "[T1] trace=actual"
        )
        .is_err());
        assert_eq!(
            parse(
                json!({"trace_id":"actual","reason":"test"}),
                "[T1] trace=actual"
            )
            .unwrap()
            .queries[0]["kind"],
            "trace"
        );
        assert_eq!(
            parse(
                json!({"trace_id":"actual","keywords":["boom"],"reason":"test"}),
                "[T1] trace=actual boom"
            )
            .unwrap()
            .queries[0]["kind"],
            "logs"
        );
    }
    #[test]
    fn source_traversal_is_rejected_before_reading() {
        for path in [
            "../secret.rs",
            "src/../../secret.rs",
            "..\\secret.rs",
            "/secret.rs",
            "C:\\secret.rs",
            "file.rs:stream",
        ] {
            assert!(parse(json!({"service":"orders","reason":"test","code_ref":{"path":path,"line":1,"excerpt":"orders"}}), "").is_err());
        }
    }
    struct TempRepo(std::path::PathBuf);
    impl TempRepo {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "lookup-plan-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn real_source_is_candidate_not_business_evidence() {
        let repo = TempRepo::new();
        fs::write(
            repo.0.join("source.rs"),
            "// header\nlet svc = \"orders timeout\";\n",
        )
        .unwrap();
        let mut q = json!({"service":"orders","keywords":["timeout"],"reason":"code lookup","code_ref":{"path":"source.rs","line":2,"excerpt":"orders timeout"}});
        let run = |q: &Value| {
            parse_plan(
                &json!({"queries":[q],"reason":"check"}).to_string(),
                &original(),
                "",
                repo.0.to_str().unwrap(),
            )
        };
        let plan = run(&q).unwrap();
        assert!(plan.reason.contains("source code candidate"));
        assert!(plan.queries[0].get("code_ref").is_none());
        q["trace_id"] = json!("source-only");
        assert!(run(&q).is_err());
        q.as_object_mut().unwrap().remove("trace_id");
        q["code_ref"]["line"] = json!(0);
        assert!(run(&q).is_err());
        q["code_ref"]["line"] = json!(1);
        assert!(run(&q).is_err());
        q["code_ref"]["line"] = json!(2);
        q["keywords"] = json!(["unseen"]);
        assert!(run(&q).is_err());
        fs::write(
            repo.0.join("source.rs"),
            vec![b'x'; MAX_SOURCE_BYTES as usize + 1],
        )
        .unwrap();
        assert!(run(&q).is_err());
    }
    #[test]
    fn evidence_service_and_source_template_need_not_share_a_line() {
        let repo = TempRepo::new();
        fs::write(
            repo.0.join("logger.rs"),
            "log::error!(\"payment timeout\");\n",
        )
        .unwrap();
        let text = json!({"queries":[{"service":"orders","keywords":["payment timeout"],"reason":"verify template","evidence_refs":["L1"],"code_ref":{"path":"logger.rs","line":1,"excerpt":"payment timeout"}}],"reason":"test"}).to_string();
        let plan = parse_plan(
            &text,
            &original(),
            "[L1] service=orders",
            repo.0.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(plan.queries[0]["service"], "orders");
        assert!(plan.reason.contains("source code candidate"));
        assert!(plan.reason.contains("\"path\":\"logger.rs\""));
        assert!(plan.reason.contains("\"line\":1"));
        assert!(plan.reason.contains("\"excerpt\":\"payment timeout\""));
        assert!(!plan.reason.contains(repo.0.to_str().unwrap()));
        assert!(parse_plan(
            &text,
            &original(),
            "[L1] service=unrelated",
            repo.0.to_str().unwrap()
        )
        .is_err());
    }
    #[test]
    fn zero_logs_source_template_retains_user_identifier() {
        use crate::wecom_investigation_query::BusinessIdentifier;
        let repo = TempRepo::new();
        fs::write(repo.0.join("source.rs"), "orders timeout\n").unwrap();
        let mut original = original();
        original.identifiers.push(BusinessIdentifier {
            field: "orderId".into(),
            value: "ORDER-123".into(),
        });
        let mut q = json!({"service":"orders","keywords":["ORDER-123","timeout"],"reason":"source lookup","code_ref":{"path":"source.rs","line":1,"excerpt":"orders timeout"}});
        let run = |q: &Value, evidence: &str| {
            parse_plan(
                &json!({"queries":[q],"reason":"check"}).to_string(),
                &original,
                evidence,
                repo.0.to_str().unwrap(),
            )
        };
        let plan = run(&q, "").unwrap();
        assert_eq!(plan.queries[0]["keywords"][0], "ORDER-123");
        assert!(plan.reason.contains("source code candidate"));
        q["keywords"] = json!(["timeout"]);
        assert!(run(&q, "").is_err());
        q["keywords"] = json!(["ORDER-12", "timeout"]);
        assert!(run(&q, "ORDER-12").is_err());
        // A confirmed cross-service trace is an alternative correlation anchor.
        q["keywords"] = json!(["timeout"]);
        q["trace_id"] = json!("trace-123");
        assert!(run(&q, "[T1] trace=trace-123").is_ok());
        assert!(run(&q, "").is_err());
        let unsafe_original = InvestigationQuery {
            identifiers: vec![BusinessIdentifier {
                field: String::new(),
                value: "$(id)".into(),
            }],
            ..original.clone()
        };
        let text =
            json!({"queries":[{"keywords":["$(id)"],"reason":"test"}],"reason":"test"}).to_string();
        assert!(parse_plan(&text, &unsafe_original, "", ".").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn source_symlink_is_rejected() {
        let repo = TempRepo::new();
        let outside = TempRepo::new();
        fs::write(outside.0.join("outside.rs"), "orders").unwrap();
        std::os::unix::fs::symlink(outside.0.join("outside.rs"), repo.0.join("link.rs")).unwrap();
        let text = json!({"queries":[{"service":"orders","reason":"test","code_ref":{"path":"link.rs","line":1,"excerpt":"orders"}}],"reason":"test"}).to_string();
        assert!(parse_plan(&text, &original(), "", repo.0.to_str().unwrap()).is_err());
    }
    fn anchor_evidence() -> &'static str {
        "[L11] service=orders instance=orders-1 thread=worker-7 logger=Quote level=WARN ts=2026-10-28T08:30:00Z\nfilteredCount=0"
    }
    #[test]
    fn context_uses_real_header_and_is_always_candidate() {
        let plan = parse(
            json!({"context_anchor":"L11","reason":"context"}),
            anchor_evidence(),
        )
        .unwrap();
        let args = &plan.queries[0];
        assert_eq!(args["kind"], "logs");
        assert_eq!(args["context_candidate"], true);
        assert_eq!(args["service"], "orders");
        assert_eq!(args["instance"], "orders-1");
        assert_eq!(args["thread"], "worker-7");
        assert_eq!(args["logger"], "Quote");
        assert_eq!(args["start"], "2026-10-28T08:29:58+00:00");
        assert_eq!(args["end"], "2026-10-28T08:30:02+00:00");
        assert!(args.get("keywords").is_none());
        let mut original = original();
        original
            .identifiers
            .push(crate::wecom_investigation_query::BusinessIdentifier {
                field: "orderId".into(),
                value: "ORDER-123".into(),
            });
        let text = json!({"queries":[{"context_anchor":"L11","reason":"context"}],"reason":"test"})
            .to_string();
        assert!(parse_plan(&text, &original, anchor_evidence(), ".").is_ok());
        let text = json!({"queries":[{"service":"orders","reason":"context"}],"reason":"test"})
            .to_string();
        assert!(parse_plan(&text, &original, anchor_evidence(), ".").is_err());
    }
    #[test]
    fn context_trace_is_derived_only_from_real_header() {
        let evidence = anchor_evidence().replace("level=WARN", "trace=trace-actual level=WARN");
        let q = json!({"context_anchor":"L11","reason":"context"});
        let plan = parse(q.clone(), &evidence).unwrap();
        assert_eq!(plan.queries[0]["trace_id"], "trace-actual");
        assert_eq!(plan.queries[0]["kind"], "logs");
        assert_eq!(plan.queries[0]["context_candidate"], true);
        for trace in ["trace-actual", "forged"] {
            let mut overridden = q.clone();
            overridden["trace_id"] = json!(trace);
            assert!(parse(overridden, &evidence).is_err());
        }
        for evidence in [
            anchor_evidence().to_string(),
            anchor_evidence().replace("level=WARN", "trace= level=WARN"),
            format!("{}\ntrace=body-only", anchor_evidence()),
        ] {
            assert!(parse(q.clone(), &evidence).unwrap().queries[0]
                .get("trace_id")
                .is_none());
        }
        assert!(parse(
            q,
            &evidence.replace("trace=trace-actual", "trace=one trace=two")
        )
        .is_err());
    }
    #[test]
    fn forged_context_headers_and_model_overrides_fail() {
        for evidence in [
            "L11 service=orders instance=i ts=2026-10-28T08:30:00Z",
            "prefix [L11] service=orders instance=i ts=2026-10-28T08:30:00Z",
            "[L11] service=orders\ninstance=i ts=2026-10-28T08:30:00Z",
            "[L11] service=orders instance=i ts=invalid\nts=2026-10-28T08:30:00Z",
            "[L11] service=orders service=other instance=i ts=2026-10-28T08:30:00Z",
            "[L11] service=orders instance=i ts=2026-10-28T08:30:00Z\n[L11] service=other instance=i ts=2026-10-28T08:30:00Z",
        ] {
            assert!(parse(json!({"context_anchor":"L11","reason":"context"}), evidence).is_err(), "{evidence}");
        }
        for (key, value) in [
            ("service", json!("orders")),
            ("instance", json!("i")),
            ("thread", json!("w")),
            ("logger", json!("Quote")),
            ("start", json!("2026-10-28T08:30:00Z")),
            ("end", json!("2026-10-28T08:30:01Z")),
            ("trace_id", json!("trace")),
            ("keywords", json!([])),
            ("context_candidate", json!(false)),
        ] {
            let mut q = json!({"context_anchor":"L11","reason":"context"});
            q[key] = value;
            assert!(parse(q, anchor_evidence()).is_err(), "{key}");
        }
        assert!(parse(
            json!({"context_anchor":"T1","reason":"context"}),
            "[T1] service=orders instance=i ts=2026-10-28T08:30:00Z"
        )
        .is_err());
        assert!(parse(
            json!({"service":"orders","context_widen":false,"reason":"test"}),
            anchor_evidence()
        )
        .is_err());
    }
    #[test]
    fn context_clamps_widening_and_rejects_outside_anchor() {
        for ts in ["2026-10-28T08:00:01Z", "1793174401000"] {
            let evidence = format!("[L11] service=orders instance=i ts={ts}");
            let plan = parse(
                json!({"context_anchor":"L11","context_widen":true,"reason":"context"}),
                &evidence,
            )
            .unwrap();
            assert_eq!(plan.queries[0]["start"], "2026-10-28T08:00:00+00:00");
            assert_eq!(plan.queries[0]["end"], "2026-10-28T08:00:31+00:00");
            assert!(plan.queries[0].get("thread").is_none());
        }
        for ts in ["2026-10-28T07:59:59Z", "2026-10-28T09:00:01Z"] {
            let evidence = format!("[L11] service=orders instance=i ts={ts}");
            assert!(parse(
                json!({"context_anchor":"L11","context_widen":true,"reason":"context"}),
                &evidence
            )
            .is_err());
        }
        let plan = parse(
            json!({"context_anchor":"L11","reason":"context"}),
            "[L11] service=orders instance=i 2026-10-28T08:59:59Z",
        )
        .unwrap();
        assert_eq!(plan.queries[0]["end"], "2026-10-28T09:00:00+00:00");
    }
    #[test]
    fn pagination_is_bounded_and_keeps_window() {
        let plan = parse(
            json!({"service":"orders","reason":"next page","page":100,"page_size":40}),
            "orders",
        )
        .unwrap();
        assert_eq!(plan.queries[0]["page"], 100);
        assert_eq!(plan.queries[0]["page_size"], 40);
        assert_eq!(plan.queries[0]["start"], original().start);
        assert_eq!(plan.queries[0]["end"], original().end);
        for (key, value) in [
            ("page", json!(0)),
            ("page", json!(101)),
            ("page_size", json!(0)),
            ("page_size", json!(41)),
            ("page", json!(1.5)),
            ("page", Value::Null),
        ] {
            let mut q = json!({"service":"orders","reason":"next page"});
            q[key] = value;
            assert!(parse(q, "orders").is_err());
        }
    }
    #[test]
    fn metadata_defaults_and_strict_levels() {
        let old = parse_plan(
            r#"{"queries":[],"reason":"legacy stop"}"#,
            &original(),
            "",
            ".",
        )
        .unwrap();
        assert_eq!(old.conclusion_level, "unknown");
        assert!(old.gaps.is_empty());
        assert_eq!(old.stop_reason, "legacy stop");
        for level in ["result", "direct_cause", "trigger_verified", "unknown"] {
            let text = json!({"queries":[],"reason":"observed [L11]","conclusion_level":level,"gaps":[],"stop_reason":"done"}).to_string();
            assert_eq!(
                parse_plan(&text, &original(), anchor_evidence(), ".")
                    .unwrap()
                    .conclusion_level,
                level
            );
        }
        for (key, value) in [
            ("conclusion_level", json!("root_cause")),
            ("conclusion_level", Value::Null),
            ("gaps", json!([{}])),
            ("gaps", json!([""])),
            ("stop_reason", Value::Null),
            ("stop_reason", json!("")),
        ] {
            let mut text = json!({"queries":[],"reason":"test"});
            text[key] = value;
            assert!(parse_plan(&text.to_string(), &original(), anchor_evidence(), ".").is_err());
        }
        for (reason, gaps) in [
            ("no citation", json!([])),
            ("[L999]", json!([])),
            ("[L11]", json!(["rule missing"])),
        ] {
            let text = json!({"queries":[],"reason":reason,"conclusion_level":"trigger_verified","gaps":gaps});
            assert!(parse_plan(&text.to_string(), &original(), anchor_evidence(), ".").is_err());
        }
    }
    #[test]
    fn decision_fallback_prefers_warn_and_never_invents_anchor() {
        let evidence = format!("[L1] service=orders instance=i ts=2026-10-28T08:30:00Z\nfilteredCount=0\n{}\n[L12] service=orders instance=i level=WARN ts=2026-10-28T08:31:00Z\nreview warning\n[L13] level=WARN\nservice=forged instance=i ts=2026-10-28T08:30:00Z", anchor_evidence());
        let plan = decision_context_plan(&evidence, &original()).unwrap();
        assert_eq!(plan.queries.len(), 2);
        assert_eq!(plan.queries[0]["context_anchor"], "L11");
        assert_eq!(plan.queries[1]["context_anchor"], "L12");
        let empty = decision_context_plan("[L1] filteredCount=0", &original()).unwrap();
        assert!(empty.queries.is_empty());
        assert!(!empty.gaps.is_empty());
        assert!(empty.stop_reason.contains("excerpt"));
    }
    #[test]
    fn stop_and_limits() {
        assert!(parse_plan(
            r#"{"queries":[],"reason":"证据充分"}"#,
            &original(),
            "",
            "missing"
        )
        .unwrap()
        .queries
        .is_empty());
        let q = json!({"service":"orders","reason":"test"});
        assert!(parse_plan(
            &json!({"queries":[q,q,q,q],"reason":"test"}).to_string(),
            &original(),
            "orders",
            "."
        )
        .is_err());
        assert!(parse(
            json!({"keywords":["a","b","c","d"],"reason":"test"}),
            "abcd"
        )
        .is_err());
        assert!(parse(
            json!({"keywords":["x".repeat(257)],"reason":"test"}),
            &"x".repeat(257)
        )
        .is_err());
        assert!(parse(json!({"reason":"test"}), "").is_err());
    }
}
