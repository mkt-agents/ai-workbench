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
    /// Present only after strict schema, real evidence and optional source checks pass.
    pub final_answer: Option<VerifiedAnswer>,
    /// Versioned final report, validated through the shared report contract.
    pub diagnostic_report: Option<crate::wecom_report::DiagnosticReport>,
    pub read_evidence_ids: Vec<String>,
    pub schema_version: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedAnswer {
    pub summary: String,
    pub claims: Vec<VerifiedClaim>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedClaim {
    pub text: String,
    #[serde(alias = "evidenceRefs")]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub code_refs: Vec<CodeRef>,
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
    // Only a final diagnostic report may omit queries; null remains a schema error.
    #[serde(default, deserialize_with = "present")]
    queries: Option<Vec<WireQuery>>,
    reason: String,
    // Serde aliases only normalize known field names; values stay byte-for-byte strings.
    // Alias + canonical name (or repeated names) is a duplicate-field schema error.
    #[serde(default, alias = "conclusionLevel", deserialize_with = "present")]
    conclusion_level: Option<String>,
    #[serde(default, deserialize_with = "present")]
    gaps: Option<Vec<String>>,
    #[serde(default, alias = "stopReason", deserialize_with = "present")]
    stop_reason: Option<String>,
    #[serde(default, alias = "finalAnswer", deserialize_with = "present")]
    final_answer: Option<VerifiedAnswer>,
    #[serde(default, deserialize_with = "present")]
    diagnostic_report: Option<crate::wecom_report::DiagnosticReport>,
    #[serde(default, alias = "readEvidenceIds", deserialize_with = "present")]
    read_evidence_ids: Option<Vec<String>>,
    #[serde(default, alias = "schemaVersion", deserialize_with = "present")]
    schema_version: Option<u8>,
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
    #[serde(default, alias = "evidenceRefs", deserialize_with = "present")]
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeRef {
    pub path: String,
    pub line: usize,
    pub excerpt: String,
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

/// Strict references need a unique producer-shaped header; embedded/indented labels are data.
fn unique_record<'a>(evidence: &'a str, id: &str) -> Result<&'a str, String> {
    let mut headers = evidence
        .lines()
        .filter(|line| evidence_id(line) == Some(id));
    let header = headers
        .next()
        .ok_or_else(|| format!("plan_error: missing evidence record [{id}]"))?;
    if headers.next().is_some()
        || !header
            .split_whitespace()
            .any(|v| v.starts_with("service=") || v.starts_with("trace="))
    {
        return Err(format!(
            "plan_error: ambiguous or malformed evidence record [{id}]"
        ));
    }
    referenced_text(evidence, id)
        .ok_or_else(|| format!("plan_error: missing evidence record [{id}]"))
}

/// Shared report lookup: require the same unique producer-shaped record as final claims.
/// Record boundaries and ID parsing remain owned by referenced_text/unique_record.
pub(crate) fn evidence_record<'a>(evidence: &'a str, id: &str) -> Option<&'a str> {
    unique_record(evidence, id).ok()
}

/// Pure display, never an executable instruction. Escape user/model markup into literal prose.
fn markdown_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\r' | '\n' => out.push(' '),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '#' | '!' | '|' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

pub fn render_answer(answer: &VerifiedAnswer, level: &str) -> String {
    render_answer_with_gaps(answer, level, &[], "")
}

/// Render a bounded conclusion without implying that its outstanding work is complete.
/// Callers must still validate the plan and enforce their evidence-level stop gate.
pub fn render_answer_with_gaps(
    answer: &VerifiedAnswer,
    level: &str,
    gaps: &[String],
    stop_reason: &str,
) -> String {
    let label = match level {
        "result" => "观察到的结果",
        "direct_cause" => "直接业务原因",
        "trigger_verified" => "具体触发条件已核对",
        _ => "未确定",
    };
    let mut out = format!(
        "## 排障结论\n\n{}\n\n结论层级：{label}\n\n### 事实与引用\n",
        markdown_text(&answer.summary)
    );
    for (i, claim) in answer.claims.iter().enumerate() {
        let refs = claim
            .evidence_refs
            .iter()
            .map(|id| format!("[{}]", markdown_text(id)))
            .collect::<Vec<_>>()
            .join("、");
        out.push_str(&format!(
            "\n{}. {}（证据：{refs}）\n",
            i + 1,
            markdown_text(&claim.text)
        ));
        for code in &claim.code_refs {
            out.push_str(&format!(
                "   - 工作区代码：{}:{}；片段：{}\n",
                markdown_text(&code.path.replace('\\', "/")),
                code.line,
                markdown_text(&code.excerpt)
            ));
        }
    }
    if !gaps.is_empty() {
        out.push_str("\n### 未完成项\n\n以下事项仍未验证，本轮结论不表示具体触发条件已闭合。\n");
        for gap in gaps {
            out.push_str(&format!("\n- {}\n", markdown_text(gap)));
        }
    }
    if !stop_reason.trim().is_empty() {
        out.push_str(&format!("\n停止原因：{}\n", markdown_text(stop_reason)));
    }
    out.push_str("\n> 证据引用不等于因果证明；引用已核对到已保留 excerpt，未额外读取 OAP 原始全文。工作区代码不自动代表线上版本。\n");
    out
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

/// Shared report source check; preserve the existing path, file and exact-line guards.
pub(crate) fn verify_code_ref(
    path: &str,
    line: usize,
    excerpt: &str,
    repo: &str,
) -> Result<(), String> {
    source_excerpt(
        &CodeRef {
            path: path.to_owned(),
            line,
            excerpt: excerpt.to_owned(),
        },
        repo,
    )
    .map(|_| ())
}

/// Stable, low-cardinality categories for timing/validation telemetry. No model text is logged.
pub fn validation_error_code(error: &str) -> &'static str {
    if let Some(report_error) = error.strip_prefix("report_error: ") {
        return match report_error {
            "unclosed json fence" => "format",
            "invalid original start" | "invalid original end" | "invalid original window" => {
                "window"
            }
            "missing, duplicate or ambiguous evidence reference"
            | "invalid legacy code evidence"
            | "invalid legacy fact reference" => "references",
            "duplicate legacy code ID"
            | "invalid or duplicate code ID"
            | "trigger needs a locally checked code reference" => "code",
            "report exceeds 96 KiB"
            | "collection exceeds report limits"
            | "timeline exceeds eight records"
            | "timeline exceeds 32 records"
            | "each factual claim needs 1..8 references"
            | "code reference join exceeds eight references" => "budget",
            "invalid diagnostic report schema"
            | "invalid level"
            | "invalid max_level"
            | "unsupported version or level above host ceiling"
            | "empty, oversized or control-bearing text"
            | "invalid legacy report bounds or blocking gaps"
            | "cause structure or blocking gaps contradict level" => "schema",
            _ => "other",
        };
    }
    let error = error.strip_prefix("plan_error: ").unwrap_or(error);
    if error.starts_with("strict JSON format:") || error == "expected exactly one json fence" {
        "format"
    } else if error.starts_with("strict JSON schema:") {
        "schema"
    } else if error.starts_with("source ")
        || error.starts_with("workspace ")
        || error.starts_with("code_ref")
        || error.starts_with("excerpt does not match source line")
    {
        "code"
    } else if error.contains("window")
        || error.starts_with("invalid original start")
        || error.starts_with("invalid original end")
        || error.starts_with("start must be RFC3339")
        || error.starts_with("end must be RFC3339")
        || error.starts_with("anchor header requires unambiguous ts")
    {
        "window"
    } else if error.starts_with("missing evidence record")
        || error.starts_with("ambiguous or malformed evidence record")
        || error.starts_with("missing context anchor")
        || error.starts_with("ambiguous anchor header")
        || error.starts_with("anchor header requires")
        || error.starts_with("context_anchor requires")
        || error.starts_with("ordinary follow-up must retain")
        || error.contains("no matching evidence")
        || error.starts_with("trace_id must occur")
        || error.starts_with("each claim requires")
    {
        "references"
    } else if error.starts_with("plan exceeds")
        || error.starts_with("at most ")
        || error.starts_with("read_evidence_ids allows at most")
        || error.starts_with("page must be")
        || error.contains("must be nonempty and at most")
        || error.starts_with("final_answer requires 1..20 claims")
    {
        "budget"
    } else if error.starts_with("final_answer requires")
        || error.starts_with("diagnostic_report requires")
        || error.starts_with("trigger_verified requires")
        || error.starts_with("invalid conclusion_level")
        || error.starts_with("schema_version must be")
        || error.starts_with("choose exactly one")
        || error.starts_with("context query boundaries")
        || error.starts_with("context_widen requires")
        || error.starts_with("query requires")
        || error.contains("must be a literal")
    {
        "schema"
    } else {
        "other"
    }
}

/// Preserve strict duplicate/unknown-field detection on both accepted wire shapes.
fn deserialize_plan(body: &str) -> Result<WirePlan, String> {
    match serde_json::from_str::<WirePlan>(body) {
        Ok(plan) => Ok(plan),
        Err(plan_error) => {
            match serde_json::from_str::<crate::wecom_report::DiagnosticReport>(body) {
                Ok(report) => Ok(WirePlan {
                    queries: Some(Vec::new()),
                    reason: "已返回诊断报告".into(),
                    conclusion_level: Some(report.level.clone()),
                    gaps: Some(
                        report
                            .gaps
                            .iter()
                            .filter(|gap| gap.blocking)
                            .map(|gap| gap.text.clone())
                            .collect(),
                    ),
                    stop_reason: Some("已返回诊断报告".into()),
                    final_answer: None,
                    diagnostic_report: Some(report),
                    read_evidence_ids: None,
                    schema_version: None,
                }),
                Err(report_error) => {
                    let category = match plan_error.classify() {
                        serde_json::error::Category::Data => "schema",
                        _ => "format",
                    };
                    Err(format!("plan_error: strict JSON {category}: {plan_error}; bare diagnostic report: {report_error}"))
                }
            }
        }
    }
}

/// Bind already declared code checks to trigger claims through shared, verified record IDs.
/// This is a mechanical reference join, not a causal/semantic proof or a level upgrade.
/// Validate before joining, then validate a clone in full before committing any additions.
fn normalize_report_code_links(
    report: &mut crate::wecom_report::DiagnosticReport,
    original: &InvestigationQuery,
    evidence: &str,
    repo: &str,
    max_level: &str,
) -> Result<(), String> {
    match crate::wecom_report::validate(report, original, evidence, repo, max_level) {
        Ok(()) => {}
        // This exact structural failure occurs after all code lines and factual refs pass.
        Err(error) if error == "report_error: trigger needs a locally checked code reference" => {}
        Err(error) => return Err(error),
    }
    if report.level != "trigger_verified" {
        return Ok(());
    }
    let mut normalized = report.clone();
    for trigger in &mut normalized.cause.trigger {
        if report
            .code_checks
            .iter()
            .any(|code| trigger.evidence_refs.contains(&code.id))
        {
            continue;
        }
        for code in &report.code_checks {
            if code
                .claim
                .evidence_refs
                .iter()
                .any(|id| trigger.evidence_refs.contains(id))
                && !trigger.evidence_refs.contains(&code.id)
            {
                if trigger.evidence_refs.len() >= 8 {
                    return Err("report_error: code reference join exceeds eight references".into());
                }
                trigger.evidence_refs.push(code.id.clone());
            }
        }
    }
    crate::wecom_report::validate(&normalized, original, evidence, repo, max_level)?;
    *report = normalized;
    Ok(())
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
    let mut plan = deserialize_plan(json_body(text)?)?;
    let queries = match plan.queries.take() {
        Some(queries) => queries,
        None if plan.diagnostic_report.is_some() => Vec::new(),
        None => return Err("plan_error: strict JSON schema: missing field `queries`".into()),
    };
    nonempty(&plan.reason, 2048, "reason")?;
    if queries.len() > 3 {
        return Err("plan_error: at most three queries".into());
    }
    let mut conclusion_level = plan.conclusion_level.unwrap_or_else(|| "unknown".into());
    if !["result", "direct_cause", "trigger_verified", "unknown"]
        .contains(&conclusion_level.as_str())
    {
        return Err("plan_error: invalid conclusion_level".into());
    }
    if plan.final_answer.is_some() && plan.gaps.is_none() {
        return Err("plan_error: final_answer requires explicit gaps array".into());
    }
    if plan.diagnostic_report.is_some() && plan.gaps.is_none() {
        return Err("plan_error: diagnostic_report requires explicit gaps array".into());
    }
    let mut gaps = plan.gaps.unwrap_or_default();
    if gaps.len() > 20 {
        return Err("plan_error: at most twenty gaps".into());
    }
    for gap in &gaps {
        nonempty(gap, 2048, "gaps item")?;
    }
    if plan.schema_version.is_some_and(|v| v != 1) {
        return Err("plan_error: schema_version must be 1 when present".into());
    }
    let read_evidence_ids = plan.read_evidence_ids.unwrap_or_default();
    if read_evidence_ids.len() > 6 {
        return Err("plan_error: read_evidence_ids allows at most six IDs".into());
    }
    for id in &read_evidence_ids {
        unique_record(evidence, id)?;
    }
    let modes = [
        !queries.is_empty(),
        !read_evidence_ids.is_empty(),
        plan.final_answer.is_some(),
        plan.diagnostic_report.is_some(),
    ];
    // Zero active modes remains the legacy stop plan; final payloads never mix with lookups.
    if modes.into_iter().filter(|active| *active).count() > 1 {
        return Err(
            "plan_error: choose exactly one of queries/read_evidence_ids/final_answer/diagnostic_report".into(),
        );
    }
    if let Some(report) = &mut plan.diagnostic_report {
        normalize_report_code_links(report, original, evidence, repo, &conclusion_level)?;
        // Legacy outer gaps have no classification: conservatively preserve them as blocking
        // in the actual report that gets rendered, including an existing nonblocking label.
        for text in &gaps {
            if let Some(gap) = report.gaps.iter_mut().find(|gap| gap.text == *text) {
                gap.blocking = true;
            } else {
                report.gaps.push(crate::wecom_report::Gap {
                    text: text.clone(),
                    blocking: true,
                });
            }
        }
        if report.gaps.len() > 8 {
            return Err(
                "plan_error: at most eight diagnostic_report gaps including outer gaps".into(),
            );
        }
        crate::wecom_report::validate(report, original, evidence, repo, &conclusion_level)?;
        // The outer level is only a ceiling: consumers must use the report's actual level.
        conclusion_level = report.level.clone();
        for gap in report.gaps.iter().filter(|gap| gap.blocking) {
            if !gaps.contains(&gap.text) {
                gaps.push(gap.text.clone());
            }
        }
        if gaps.len() > 20 {
            return Err("plan_error: at most twenty gaps".into());
        }
        if conclusion_level == "trigger_verified" && !gaps.is_empty() {
            return Err("plan_error: trigger_verified requires no gaps".into());
        }
    }
    if let Some(answer) = &plan.final_answer {
        if !queries.is_empty() || conclusion_level == "unknown" {
            return Err("plan_error: final_answer requires empty queries and an established conclusion_level".into());
        }
        if conclusion_level == "trigger_verified" && !gaps.is_empty() {
            return Err("plan_error: trigger_verified requires no gaps".into());
        }
        nonempty(&answer.summary, 4096, "final_answer.summary")?;
        if answer.claims.is_empty() || answer.claims.len() > 20 {
            return Err("plan_error: final_answer requires 1..20 claims".into());
        }
        for claim in &answer.claims {
            nonempty(&claim.text, 4096, "claim.text")?;
            if claim.evidence_refs.is_empty() || claim.evidence_refs.len() > 20 {
                return Err("plan_error: each claim requires 1..20 real evidence_refs".into());
            }
            for id in &claim.evidence_refs {
                unique_record(evidence, id)?;
            }
            if claim.code_refs.len() > 6 {
                return Err("plan_error: at most six code_refs per claim".into());
            }
            for code in &claim.code_refs {
                source_excerpt(code, repo)?;
            }
        }
    }
    if conclusion_level == "trigger_verified"
        && plan.final_answer.is_none()
        && plan.diagnostic_report.is_none()
    {
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
        final_answer: plan.final_answer,
        diagnostic_report: plan.diagnostic_report,
        read_evidence_ids,
        schema_version: plan.schema_version,
    };
    // A stop plan does not require filesystem or time parsing.
    if queries.is_empty() {
        return Ok(result);
    }
    let original_start = DateTime::parse_from_rfc3339(&original.start)
        .map_err(|_| "plan_error: invalid original start")?;
    let original_end = DateTime::parse_from_rfc3339(&original.end)
        .map_err(|_| "plan_error: invalid original end")?;
    if original_start >= original_end {
        return Err("plan_error: invalid original window".into());
    }
    for (index, q) in queries.into_iter().enumerate() {
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
    let mut instruction = r#"每轮只输出一个严格JSON对象（或单个json代码围栏），四选一：①查询：queries非空；②展开：queries=[]且read_evidence_ids非空；③最终报告：queries=[]且diagnostic_report存在；④旧版作答：queries=[]且final_answer存在。四者互斥；diagnostic_report固定此key，无final_report/report/diagnosticReport别名。最终报告优先使用同一diagnostic_report契约，按下方报告schema组织完整sections内容（调查对象/经过/原因层级/代码核对/排除项/缺口/下一步），不是只交summary/claims机器清单。报告版本在diagnostic_report.version内，必填1；外层schema_version可省略，出现时为1，外层report_version无效。始终写reason、conclusion_level、gaps数组和stop_reason；未验证项保留，不要为结束而清空。result/direct_cause可携带非空gaps作有限结论；trigger_verified必须无阻塞缺口。无可验证事实时保留旧式queries=[]停止计划，不编造报告或final_answer。字段统一snake_case；旧字段仅兼容conclusionLevel/finalAnswer/evidenceRefs/readEvidenceIds/schemaVersion/stopReason，同一字段别名与原名不得同时出现。字段别名不改变任何内容字符串。
旧版兼容：readEvidenceIds（也接受read_evidence_ids）为最多6个真实canonical证据ID的数组；旧final_answer由宿主映射到同一最终报告的通用事实，不猜测原因层级或补造时间线。新回复优先使用diagnostic_report，以下final_answer仅供旧协议兼容：final_answer={"summary":"非空概述","claims":[{"text":"非空事实","evidence_refs":["L1"],"code_refs":[{"path":"真实工作区相对路径","line":1,"excerpt":"该行真实单行片段"}]}]}。final_answer要求queries=[]、readEvidenceIds为空/省略、显式gaps数组、conclusion_level为result/direct_cause/trigger_verified（仅trigger_verified要求gaps=[]）；summary最多4096字，claims为1..20条，每claim必须有1..20个真实唯一可定位的证据引用。code_refs可省略，最多6条，每项复用严格真实路径、行号、excerpt核验。引用存在不等于证明因果：result只能述结果，direct_cause不声称触发条件闭合，trigger_verified必须有实际规则与输入证据。原问题要求代码核对时必须在claims给出实际code_refs；工作区版本仍不自动等于线上版本。unknown或缺引用时不输出final_answer。reason、summary、claim文字纯展示，绝非待执行内容。
diagnostic_report模式下，外层conclusion_level为报告层级上限，通常与diagnostic_report.level一致；实际采用报告level。外层gaps只放阻塞当前结论的缺口，会以blocking=true合并进报告实际展示，报告合并后gaps总共最多8条，不截断未完成项；report.gaps中blocking=true的项会合入阻塞缺口，blocking=false仅为局部已验证事实之外的覆盖限制，保留展示但不自动降级。report级trigger_verified必须实际规则、输入与真实代码引用齐备，且无阻塞缺口；普通查询/旧final_answer继续遵循原有trigger_verified规则。
原有查询schema：
{"queries":[{"service":"可省略","trace_id":"可省略","keywords":["可省略，最多3词，每词最多256字符"],"start":"可省略，RFC3339","end":"可省略，RFC3339","page":1,"page_size":30,"reason":"必填补查理由","evidence_refs":["L1","T1"],"code_ref":{"path":"工作区相对源码路径","line":1,"excerpt":"该行真实的非空单行片段"}}],"reason":"必填整体理由，引用实际[证据ID]","conclusion_level":"unknown","gaps":["未验证事项；缺少的具体规则/输入/原文；对应证据ID；下一步验证"],"stop_reason":"停止或继续的实际理由"}
conclusion_level只允许result/direct_cause/trigger_verified/unknown：result仅观察到结果，direct_cause为直接业务原因但具体触发规则未证实，trigger_verified须实际证据证明具体规则及输入，gaps必须为空且reason引用实际[证据ID]。只看到过滤后0结果/未执行而未验证具体规则时至多direct_cause，禁止宣称根因已闭合。unknown为未确定。每项gaps用非空字符串明确未验证事项、证据缺口、引用和下一步，最多20项。
必须逐条审核WARN、业务异常、过滤/跳过/false/0结果及其上下文。span OK只表示链路状态，不能用span OK排除业务异常。WARN未解释、原文截断、只有结果没有触发规则时必须保留gaps，不能以queries=[]表示根因已闭合。
queries最多3条；证据充足或没有可靠补查依据时queries=[]，仍输出conclusion_level/gaps/stop_reason。禁止额外字段、URL、shell、任意GraphQL、改写时间时区。普通start/end省略时使用原窗口，显式时间只能收窄；page为1..100整数，page_size为1..40整数，分页保持相同查询时间范围。
上下文查询使用独立形状{"context_anchor":"L11","context_widen":false,"page":1,"page_size":30,"reason":"查该真实日志邻近决策","evidence_refs":["L11"]}。context_anchor必须是已返回的唯一真实日志记录头（L或R轮次L），不接受正文伪造编号。仅从锚点实际头的service=/instance=/thread=/logger=/ts=或RFC3339推导：service、instance、时间必需；旧头可缺thread/logger。默认前后2秒；2秒未找到触发上下文可context_widen=true查前后30秒，始终裁到原窗口，窗外锚点无效。此形状禁止同时给service/trace_id/keywords/start/end/code_ref，实例/线程/logger更不接受模型传入覆盖。输出固定logs+context_candidate=true，仅为邻近上下文候选，由后续证据核验，不是业务精确命中。
service/keywords须逐字见已返回证据；trace_id必须逐字见已返回证据，源码不证明trace。evidence_refs可省略；提供时必须对应真实的[L数字]/[T数字]/[R轮次L数字]/[R轮次T数字]记录头，且引用记录正文实际包含对应实体。
允许先只读定点搜索工作区源码并读取命中行，禁止执行代码/命令、写文件、遍历工作区之外或跟随符号链接。code_ref可省略；提供时path必须为工作区内真实普通UTF-8文件（最多512KiB），line为正整数，excerpt在该行内；仅缺少证据的新service/keywords须在excerpt内。original.identifiers中的完整value可直接作为keyword，即使零日志也有效但仍须合法字面量。原问题含业务ID时，普通无trace_id补查必须在keywords保留至少一个原ID；使用已返回证据确认的trace_id仍可跨链路；除此之外省原ID必带context_anchor。源码新实体只标source code candidate，不代表线上版本、业务命中或根因。
要展开时输出queries=[]及readEvidenceIds:["L1"]，不同时给final_answer或diagnostic_report；只展开内存已保留context excerpt而非OAP原始全文，不发生网络补查。索引中的summary是导航提示而非可验证全文；canonical ID才用于引用/展开，source_aliases仅说明来源。first prompt有预算，后续仅增量证据；不要索取或重发全部证据。缺失原文超出retained excerpt时必须保留gaps，不能假称展开了原始全文。仅trace_id查调用链；同时给service或keywords查日志；普通查询至少有service/trace_id/keywords之一。"#.to_string();
    instruction.push_str("\n\n统一最终报告契约（规划结束与最终回复使用同一格式）：\n");
    instruction.push_str(&crate::wecom_report::schema_instruction());
    instruction
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
    fn report_plan() -> Value {
        json!({"queries":[],"reason":"核对保留证据","conclusion_level":"result","gaps":[],
            "diagnostic_report":{"version":1,"level":"result",
                "summary":{"text":"观察到过滤结果为零","evidence_refs":["L11"]},
                "timeline":[{"evidence_refs":["L11"],"interpretation":"日志显示过滤数量"}],
                "cause":{"result":[{"text":"filteredCount=0","evidence_refs":["L11"]}],"direct":[],"trigger":[]},
                "code_checks":[],"ruled_out":[],
                "gaps":[{"text":"具体规则与输入仍待核对","blocking":true}],
                "next_actions":[{"text":"查阅过滤规则输入","evidence_refs":[]}]}})
    }

    const RECOVERY_47: &str = include_str!("../tests/fixtures/wecom-report-recovery/seq-47.json");
    const RECOVERY_55: &str = include_str!("../tests/fixtures/wecom-report-recovery/seq-55.json");
    const RECOVERY_63: &str = include_str!("../tests/fixtures/wecom-report-recovery/seq-63.json");
    const RECOVERY_EVIDENCE: &str =
        include_str!("../tests/fixtures/wecom-report-recovery/evidence.txt");

    fn recovery_repo() -> TempRepo {
        let repo = TempRepo::new();
        fs::write(
            repo.0.join("rule.rs"),
            include_str!("../tests/fixtures/wecom-report-recovery/rule.rs"),
        )
        .unwrap();
        repo
    }

    #[test]
    fn captured_report_shapes_recover_without_discarding_report() {
        let repo = recovery_repo();
        for raw in [RECOVERY_47, RECOVERY_55, RECOVERY_63] {
            for text in [raw.to_string(), format!("```json\n{raw}\n```")] {
                let parsed = parse_plan(
                    &text,
                    &original(),
                    RECOVERY_EVIDENCE,
                    repo.0.to_str().unwrap(),
                )
                .unwrap();
                assert!(parsed.queries.is_empty() && parsed.final_answer.is_none());
                assert_eq!(parsed.conclusion_level, "trigger_verified");
                assert!(parsed.gaps.is_empty());
                let report = parsed.diagnostic_report.unwrap();
                assert_eq!(report.timeline.len(), 8);
                assert_eq!(report.gaps.len(), 3);
                assert_eq!(report.code_checks.len(), 4);
                assert_eq!(
                    report.cause.trigger[0].evidence_refs,
                    ["L5", "L6", "L8", "C1", "C2", "C3", "C4"]
                );
                let input: Value = serde_json::from_str(raw).unwrap();
                let input = input.get("diagnostic_report").unwrap_or(&input);
                assert_eq!(
                    report.summary.text,
                    input["summary"]["text"].as_str().unwrap()
                );
                assert_eq!(
                    report.cause.trigger[0].text,
                    input["cause"]["trigger"][0]["text"].as_str().unwrap()
                );
            }
        }
    }

    #[test]
    fn recovery_keeps_strict_wire_duplicates_unknowns_nulls_and_modes() {
        let repo = recovery_repo();
        let run = |raw: &str| {
            parse_plan(
                raw,
                &original(),
                RECOVERY_EVIDENCE,
                repo.0.to_str().unwrap(),
            )
        };
        for (field, value) in [
            ("queries", json!([])),
            ("report", json!({})),
            ("shell", json!("id")),
            ("verified", json!(true)),
        ] {
            let mut bare: Value = serde_json::from_str(RECOVERY_47).unwrap();
            bare[field] = value;
            assert!(run(&bare.to_string()).is_err(), "{field}");
        }
        for raw in [RECOVERY_47, RECOVERY_55, RECOVERY_63] {
            assert!(
                run(&raw.replacen("\"version\": 1", "\"version\": 1, \"version\": 1", 1)).is_err()
            );
            assert!(run(&raw.replacen(
                "\"evidence_refs\":",
                "\"evidenceRefs\": [], \"evidence_refs\":",
                1
            ))
            .is_err());
        }
        let base: Value = serde_json::from_str(RECOVERY_55).unwrap();
        for (field, value) in [
            ("queries", Value::Null),
            ("readEvidenceIds", json!(["L5"])),
            ("queries", json!([{"service":"quote","reason":"lookup"}])),
            ("final_answer", json!({"summary":"mixed","claims":[]})),
        ] {
            let mut bad = base.clone();
            bad[field] = value;
            assert!(run(&bad.to_string()).is_err(), "{field}");
        }
        for raw in [
            r#"{"reason":"stop"}"#,
            r#"{"reason":"read","read_evidence_ids":["L5"]}"#,
            r#"{"reason":"legacy","conclusion_level":"result","gaps":[],"final_answer":{"summary":"x","claims":[{"text":"x","evidence_refs":["L5"]}]}}"#,
        ] {
            assert!(run(raw).unwrap_err().contains("missing field `queries`"));
        }
        let mut bad = base;
        bad.as_object_mut().unwrap().remove("gaps");
        assert!(run(&bad.to_string()).is_err());
    }

    #[test]
    fn code_link_join_is_atomic_bounded_and_never_repairs_invalid_evidence() {
        let repo = recovery_repo();
        let original_report: crate::wecom_report::DiagnosticReport =
            serde_json::from_str(RECOVERY_47).unwrap();
        assert_eq!(
            crate::wecom_report::validate(
                &original_report,
                &original(),
                RECOVERY_EVIDENCE,
                repo.0.to_str().unwrap(),
                "trigger_verified"
            )
            .unwrap_err(),
            "report_error: trigger needs a locally checked code reference"
        );
        let normalize = |report: &mut crate::wecom_report::DiagnosticReport| {
            normalize_report_code_links(
                report,
                &original(),
                RECOVERY_EVIDENCE,
                repo.0.to_str().unwrap(),
                "trigger_verified",
            )
        };
        for mutation in 0..8 {
            let mut report = original_report.clone();
            match mutation {
                0 => report.code_checks[0].excerpt = "invented source".into(),
                1 => report.code_checks[0].path = "../rule.rs".into(),
                2 => report.code_checks[0].claim.evidence_refs = vec!["L999".into()],
                3 => report.cause.trigger[0].evidence_refs.push("C999".into()),
                4 => report.cause.trigger[0].evidence_refs.push("L5".into()),
                5 => report.cause.trigger[0].evidence_refs = vec!["L2".into()],
                6 => {
                    report.cause.trigger[0].evidence_refs =
                        (2..=9).map(|i| format!("L{i}")).collect()
                }
                _ => report.next_actions[0].evidence_refs = vec!["L999".into()],
            }
            let unchanged = report.clone();
            assert!(normalize(&mut report).is_err(), "mutation {mutation}");
            assert_eq!(report, unchanged, "mutation {mutation}");
        }
        let mut report = original_report.clone();
        let mut second = report.cause.trigger[0].clone();
        report.cause.trigger[0].evidence_refs.push("C1".into());
        second.evidence_refs = vec!["L8".into()];
        report.cause.trigger.push(second);
        normalize(&mut report).unwrap();
        assert_eq!(
            report.cause.trigger[0].evidence_refs,
            ["L5", "L6", "L8", "C1"]
        );
        assert_eq!(report.cause.trigger[1].evidence_refs, ["L8", "C4"]);
        let once = report.clone();
        normalize(&mut report).unwrap();
        assert_eq!(report, once);
    }

    #[test]
    fn bare_report_preserves_blocking_gaps_without_inventing_queries() {
        let report = report_plan()["diagnostic_report"].clone();
        let plan = parse_plan(
            &report.to_string(),
            &original(),
            anchor_evidence(),
            "missing",
        )
        .unwrap();
        assert_eq!(plan.reason, "已返回诊断报告");
        assert_eq!(plan.stop_reason, "已返回诊断报告");
        assert_eq!(plan.conclusion_level, "result");
        assert_eq!(plan.gaps, ["具体规则与输入仍待核对"]);
        assert!(plan.queries.is_empty());
        assert_eq!(plan.diagnostic_report.unwrap().gaps.len(), 1);
    }

    #[test]
    fn report_validation_errors_have_stable_categories() {
        for (error, code) in [
            ("report_error: invalid diagnostic report schema", "schema"),
            (
                "report_error: missing, duplicate or ambiguous evidence reference",
                "references",
            ),
            (
                "report_error: trigger needs a locally checked code reference",
                "code",
            ),
            ("report_error: collection exceeds report limits", "budget"),
            ("report_error: timeline exceeds 32 records", "budget"),
            ("report_error: invalid original window", "window"),
            ("report_error: unclosed json fence", "format"),
        ] {
            assert_eq!(validation_error_code(error), code);
        }
    }

    #[test]
    fn diagnostic_report_uses_real_contract_and_preserves_legacy_modes() {
        let value = report_plan();
        let parsed = parse_plan(
            &value.to_string(),
            &original(),
            anchor_evidence(),
            "missing",
        )
        .unwrap();
        assert!(parsed.final_answer.is_none());
        assert_eq!(parsed.diagnostic_report.as_ref().unwrap().version, 1);
        assert_eq!(parsed.gaps, vec!["具体规则与输入仍待核对"]);
        assert_eq!(parsed.conclusion_level, "result");
        let mut higher_ceiling = value.clone();
        higher_ceiling["conclusion_level"] = json!("trigger_verified");
        let lower = parse_plan(
            &higher_ceiling.to_string(),
            &original(),
            anchor_evidence(),
            "missing",
        )
        .unwrap();
        assert_eq!(lower.conclusion_level, "result");
        let mut low_ceiling = value.clone();
        low_ceiling["conclusion_level"] = json!("unknown");
        assert!(parse_plan(
            &low_ceiling.to_string(),
            &original(),
            anchor_evidence(),
            "."
        )
        .is_err());
        let legacy = json!({"queries":[],"reason":"legacy","conclusion_level":"result","gaps":[],
            "final_answer":{"summary":"zero","claims":[{"text":"zero","evidence_refs":["L11"]}]}});
        let old = parse_plan(&legacy.to_string(), &original(), anchor_evidence(), ".").unwrap();
        assert!(old.final_answer.is_some() && old.diagnostic_report.is_none());
        let old_stop = parse_plan(
            r#"{"queries":[],"reason":"stop"}"#,
            &original(),
            "",
            "missing",
        )
        .unwrap();
        assert!(old_stop.final_answer.is_none() && old_stop.diagnostic_report.is_none());
    }

    #[test]
    fn outer_report_gaps_are_rendered_as_blocking_without_silent_truncation() {
        let mut value = report_plan();
        value["gaps"] = json!(["外层缺少输入", "具体规则与输入仍待核对"]);
        let parsed = parse_plan(&value.to_string(), &original(), anchor_evidence(), ".").unwrap();
        let report = parsed.diagnostic_report.unwrap();
        assert_eq!(report.gaps.len(), 2);
        assert!(report
            .gaps
            .iter()
            .any(|gap| gap.text == "外层缺少输入" && gap.blocking));
        value["diagnostic_report"]["gaps"] = json!([{"text":"外层缺少输入","blocking":false}]);
        value["gaps"] = json!(["外层缺少输入"]);
        let parsed = parse_plan(&value.to_string(), &original(), anchor_evidence(), ".").unwrap();
        let report = parsed.diagnostic_report.unwrap();
        assert_eq!(report.gaps.len(), 1);
        assert!(report.gaps[0].blocking);
        value["diagnostic_report"]["gaps"] = json!([]);
        value["gaps"] = json!((0..8).map(|i| format!("gap {i}")).collect::<Vec<_>>());
        assert!(parse_plan(&value.to_string(), &original(), anchor_evidence(), ".").is_ok());
        value["gaps"] = json!((0..9).map(|i| format!("gap {i}")).collect::<Vec<_>>());
        assert!(
            parse_plan(&value.to_string(), &original(), anchor_evidence(), ".")
                .unwrap_err()
                .contains("at most eight diagnostic_report gaps")
        );
    }

    #[test]
    fn all_four_plan_modes_are_pairwise_exclusive() {
        let report = report_plan()["diagnostic_report"].clone();
        let modes = [
            ("queries", json!([{"service":"orders","reason":"lookup"}])),
            ("read_evidence_ids", json!(["L11"])),
            (
                "final_answer",
                json!({"summary":"zero","claims":[{"text":"zero","evidence_refs":["L11"]}]}),
            ),
            ("diagnostic_report", report),
        ];
        for a in 0..modes.len() {
            for b in (a + 1)..modes.len() {
                let mut value =
                    json!({"queries":[],"reason":"mixed","conclusion_level":"result","gaps":[]});
                value[modes[a].0] = modes[a].1.clone();
                value[modes[b].0] = modes[b].1.clone();
                let error = parse_plan(&value.to_string(), &original(), anchor_evidence(), ".")
                    .unwrap_err();
                assert!(error.contains("choose exactly one"), "{error}");
            }
        }
    }

    #[test]
    fn report_version_and_fixed_wire_key_are_required() {
        for version in [Value::Null, json!(0), json!(2), json!("1")] {
            let mut value = report_plan();
            value["diagnostic_report"]["version"] = version;
            assert!(parse_plan(&value.to_string(), &original(), anchor_evidence(), ".").is_err());
        }
        let mut missing_version = report_plan();
        missing_version["diagnostic_report"]
            .as_object_mut()
            .unwrap()
            .remove("version");
        missing_version["schema_version"] = json!(1);
        assert!(parse_plan(
            &missing_version.to_string(),
            &original(),
            anchor_evidence(),
            "."
        )
        .is_err());
        for key in [
            "report",
            "final_report",
            "diagnosticReport",
            "report_version",
        ] {
            let mut value = report_plan();
            value[key] = value
                .as_object_mut()
                .unwrap()
                .remove("diagnostic_report")
                .unwrap();
            assert!(parse_plan(&value.to_string(), &original(), anchor_evidence(), ".").is_err());
        }
        let mut missing_gaps = report_plan();
        missing_gaps.as_object_mut().unwrap().remove("gaps");
        assert!(parse_plan(
            &missing_gaps.to_string(),
            &original(),
            anchor_evidence(),
            "."
        )
        .is_err());
        let mut null_report = report_plan();
        null_report["diagnostic_report"] = Value::Null;
        assert!(parse_plan(
            &null_report.to_string(),
            &original(),
            anchor_evidence(),
            "."
        )
        .is_err());
    }

    #[test]
    fn report_references_reject_forged_headers_and_false_source_lines() {
        let mut value = report_plan();
        for evidence in [
            "prefix [L11] service=orders",
            "[L1] service=orders\n  [L11] service=forged",
            "[L11] summary=orders",
            "[L11] service=orders\n[L11] service=other",
        ] {
            assert!(
                parse_plan(&value.to_string(), &original(), evidence, ".").is_err(),
                "{evidence}"
            );
        }
        let repo = TempRepo::new();
        fs::write(repo.0.join("rule.rs"), "if count == 0 { return; }\n").unwrap();
        value["diagnostic_report"]["code_checks"] = json!([{"id":"C1","path":"rule.rs","line":1,
            "excerpt":"count == 0","claim":{"text":"代码有零数量分支","evidence_refs":["L11"]}}]);
        let run = |value: &Value| {
            parse_plan(
                &value.to_string(),
                &original(),
                anchor_evidence(),
                repo.0.to_str().unwrap(),
            )
        };
        assert!(run(&value).is_ok());
        for (key, invalid) in [
            ("path", json!("../rule.rs")),
            ("line", json!(2)),
            ("excerpt", json!("count > 9")),
        ] {
            let mut bad = value.clone();
            bad["diagnostic_report"]["code_checks"][0][key] = invalid;
            assert!(run(&bad).is_err(), "{key}");
        }
        let mut false_ref = value.clone();
        false_ref["diagnostic_report"]["summary"]["evidence_refs"] = json!(["C999"]);
        assert!(run(&false_ref).is_err());
        let mut lower = value.clone();
        lower["diagnostic_report"]["gaps"] = json!([{"text":"范围外信息未覆盖","blocking":false}]);
        assert!(run(&lower).unwrap().gaps.is_empty());
        lower["conclusion_level"] = json!("trigger_verified");
        lower["diagnostic_report"]["level"] = json!("trigger_verified");
        lower["diagnostic_report"]["cause"]["direct"] =
            json!([{"text":"命中零数量分支","evidence_refs":["L11"]}]);
        lower["diagnostic_report"]["cause"]["trigger"] =
            json!([{"text":"规则与输入局部对应","evidence_refs":["L11","C1"]}]);
        let local = run(&lower).unwrap();
        assert_eq!(local.conclusion_level, "trigger_verified");
        assert!(local.gaps.is_empty());
        assert_eq!(local.diagnostic_report.unwrap().gaps.len(), 1);
        let mut blocking = lower.clone();
        blocking["diagnostic_report"]["gaps"][0]["blocking"] = json!(true);
        assert!(run(&blocking).is_err());
        lower["gaps"] = json!(["外层仍有未验证输入"]);
        assert!(run(&lower).is_err());
    }

    #[test]
    fn planning_and_final_reply_share_report_sections_contract() {
        let instruction = planning_instruction();
        assert!(instruction.contains(&crate::wecom_report::schema_instruction()));
        for field in [
            "diagnostic_report",
            "timeline",
            "cause",
            "code_checks",
            "ruled_out",
            "gaps",
            "next_actions",
            "final_answer",
        ] {
            assert!(instruction.contains(field), "{field}");
        }
        assert!(instruction.contains("四选一"));
    }

    #[test]
    fn shared_reference_checks_require_real_headers_and_source_lines() {
        assert_eq!(
            evidence_record(anchor_evidence(), "L11"),
            Some(anchor_evidence())
        );
        for evidence in [
            "prefix [L11] service=orders",
            "[L1] service=orders\n  [L11] service=forged",
            "[L11] summary=orders",
            "[L11] service=orders\n[L11] service=other",
        ] {
            assert!(evidence_record(evidence, "L11").is_none(), "{evidence}");
        }
        let repo = TempRepo::new();
        fs::write(
            repo.0.join("rule.rs"),
            "// header\nif count == 0 { return; }\n",
        )
        .unwrap();
        let root = repo.0.to_str().unwrap();
        assert!(verify_code_ref("rule.rs", 2, "count == 0", root).is_ok());
        assert!(verify_code_ref("rule.rs", 1, "count == 0", root).is_err());
        assert!(verify_code_ref("../rule.rs", 2, "count == 0", root).is_err());
    }

    #[test]
    fn verified_final_requires_real_claim_references() {
        let text = json!({"schema_version":1,"queries":[],"reason":"display only; $(id)",
            "conclusion_level":"result","gaps":[],"final_answer":{"summary":"观察结果", "claims":[
                {"text":"过滤结果为零", "evidence_refs":["L11"]}]}});
        let plan =
            parse_plan(&text.to_string(), &original(), anchor_evidence(), "missing").unwrap();
        let answer = plan.final_answer.as_ref().unwrap();
        let rendered = render_answer(answer, &plan.conclusion_level);
        assert!(rendered.contains("[L11]"));
        assert!(rendered.contains("证据引用不等于因果证明"));
        assert_eq!(rendered, render_answer(answer, &plan.conclusion_level));
        for reference in ["L999", "R1L11", "L0"] {
            let mut bad = text.clone();
            bad["final_answer"]["claims"][0]["evidence_refs"] = json!([reference]);
            assert!(parse_plan(&bad.to_string(), &original(), anchor_evidence(), ".").is_err());
        }
    }
    #[test]
    fn lower_level_finals_keep_explicit_gaps_and_verified_references() {
        let base = json!({"queries":[],"reason":"有限结论", "gaps":["缺少具体输入"],
            "stop_reason":"已保留未完成项", "conclusion_level":"result",
            "final_answer":{"summary":"零结果", "claims":[{"text":"过滤后为零","evidence_refs":["L11"]}]}});
        for level in ["result", "direct_cause"] {
            let mut value = base.clone();
            value["conclusion_level"] = json!(level);
            let plan = parse_plan(&value.to_string(), &original(), anchor_evidence(), ".").unwrap();
            let answer = plan.final_answer.as_ref().unwrap();
            let rendered = render_answer_with_gaps(answer, level, &plan.gaps, &plan.stop_reason);
            assert!(rendered.contains("### 未完成项"));
            assert!(rendered.contains("缺少具体输入"));
            assert!(rendered.contains("停止原因：已保留未完成项"));
            assert_eq!(
                render_answer(answer, level),
                render_answer_with_gaps(answer, level, &[], "")
            );
            value.as_object_mut().unwrap().remove("gaps");
            assert!(parse_plan(&value.to_string(), &original(), anchor_evidence(), ".").is_err());
        }
        for level in ["trigger_verified", "unknown"] {
            let mut value = base.clone();
            value["conclusion_level"] = json!(level);
            assert!(parse_plan(&value.to_string(), &original(), anchor_evidence(), ".").is_err());
        }
    }

    #[test]
    fn aliases_only_normalize_keys_and_duplicate_spellings_fail() {
        let content = r#"finalAnswer conclusionLevel evidenceRefs readEvidenceIds schemaVersion stopReason {"queries":[]} \\n"#;
        let base = json!({"queries":[],"reason":content,"conclusionLevel":"result","gaps":[content],
            "schemaVersion":1,"stopReason":content,"finalAnswer":{"summary":content,
                "claims":[{"text":content,"evidenceRefs":["L11"]}]}});
        let plan = parse_plan(&base.to_string(), &original(), anchor_evidence(), ".").unwrap();
        assert_eq!(plan.reason, content);
        assert_eq!(plan.gaps, vec![content]);
        assert_eq!(plan.stop_reason, content);
        let answer = plan.final_answer.unwrap();
        assert_eq!(answer.summary, content);
        assert_eq!(answer.claims[0].text, content);
        for (alias, canonical) in [
            ("conclusionLevel", "conclusion_level"),
            ("schemaVersion", "schema_version"),
            ("stopReason", "stop_reason"),
            ("finalAnswer", "final_answer"),
        ] {
            let mut duplicate = base.clone();
            duplicate[canonical] = duplicate[alias].clone();
            let error = parse_plan(&duplicate.to_string(), &original(), anchor_evidence(), ".")
                .unwrap_err();
            assert_eq!(validation_error_code(&error), "schema");
            assert!(error.contains("duplicate field"));
        }
        let mut duplicate = base;
        duplicate["finalAnswer"]["claims"][0]["evidence_refs"] = json!(["L11"]);
        assert!(
            parse_plan(&duplicate.to_string(), &original(), anchor_evidence(), ".")
                .unwrap_err()
                .contains("duplicate field")
        );
        for text in [
            r#"{"queries":[],"reason":"read","readEvidenceIds":["L11"],"read_evidence_ids":["L11"]}"#,
            r#"{"queries":[{"service":"orders","reason":"q","evidenceRefs":["L11"],"evidence_refs":["L11"]}],"reason":"read"}"#,
        ] {
            assert!(parse_plan(text, &original(), anchor_evidence(), ".")
                .unwrap_err()
                .contains("duplicate field"));
        }
        assert!(parse(
            json!({"service":"orders","reason":"q","evidenceRefs":["L11"]}),
            anchor_evidence()
        )
        .is_ok());
        assert!(parse(
            json!({"service":"orders","reason":"q","traceId":"invented"}),
            anchor_evidence()
        )
        .is_err());
    }

    #[test]
    fn validation_errors_have_stable_categories() {
        for (message, expected) in [
            ("strict JSON format: EOF while parsing", "format"),
            ("expected exactly one json fence", "format"),
            ("strict JSON schema: duplicate field", "schema"),
            ("invalid conclusion_level", "schema"),
            ("missing evidence record [L999]", "references"),
            (
                "trace_id must occur in returned evidence and supplied references",
                "references",
            ),
            ("source line does not exist", "code"),
            ("excerpt does not match source line", "code"),
            (
                "code_ref.path must be nonempty and at most 1024 characters",
                "code",
            ),
            ("follow-up must stay inside original window", "window"),
            (
                "anchor header requires unambiguous ts milliseconds or RFC3339",
                "window",
            ),
            ("plan exceeds 32 KiB", "budget"),
            ("read_evidence_ids allows at most six IDs", "budget"),
            ("unrecognized error", "other"),
        ] {
            assert_eq!(
                validation_error_code(&format!("plan_error: {message}")),
                expected
            );
        }
        for (text, expected) in [("{", "format"), (r#"{"queries":[]}"#, "schema")] {
            let error = parse_plan(text, &original(), "", ".").unwrap_err();
            assert_eq!(validation_error_code(&error), expected);
        }
    }

    #[test]
    fn final_schema_modes_and_source_refs_fail_closed() {
        let repo = TempRepo::new();
        fs::write(repo.0.join("rule.rs"), "if count == 0 { return; }\n").unwrap();
        let base = json!({"queries":[],"reason":"display only", "gaps":[],"conclusion_level":"trigger_verified",
            "final_answer":{"summary":"rule matched","claims":[{"text":"count is zero","evidence_refs":["L11"],
                "code_refs":[{"path":"rule.rs","line":1,"excerpt":"count == 0"}]}]}});
        let run = |v: &Value| {
            parse_plan(
                &v.to_string(),
                &original(),
                anchor_evidence(),
                repo.0.to_str().unwrap(),
            )
        };
        assert!(run(&base).is_ok());
        for path in [
            "../rule.rs",
            "C:\\rule.rs",
            "missing.rs",
            "/rule.rs",
            "rule.rs:stream",
        ] {
            let mut bad = base.clone();
            bad["final_answer"]["claims"][0]["code_refs"][0]["path"] = json!(path);
            assert!(run(&bad).is_err());
        }
        for (key, value) in [
            ("line", json!(2)),
            ("excerpt", json!("imaginary")),
            ("extra", json!(true)),
        ] {
            let mut bad = base.clone();
            bad["final_answer"]["claims"][0]["code_refs"][0][key] = value;
            assert!(run(&bad).is_err());
        }
        for (key, value) in [
            ("conclusion_level", json!("unknown")),
            ("gaps", json!(["missing"])),
            ("schema_version", json!(2)),
            ("schema_version", Value::Null),
            ("readEvidenceIds", json!(["L11"])),
            ("queries", json!([{"service":"orders","reason":"x"}])),
        ] {
            let mut bad = base.clone();
            bad[key] = value;
            assert!(run(&bad).is_err());
        }
        for value in [json!([]), Value::Null, json!(["L999"])] {
            let mut bad = base.clone();
            bad["final_answer"]["claims"][0]["evidence_refs"] = value;
            assert!(run(&bad).is_err());
        }
        for key in ["evidence_refs", "text"] {
            let mut bad = base.clone();
            bad["final_answer"]["claims"][0]
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(run(&bad).is_err());
        }
        let mut missing_gaps = base.clone();
        missing_gaps.as_object_mut().unwrap().remove("gaps");
        assert!(run(&missing_gaps).is_err());
        let mut store = crate::wecom_evidence_store::EvidenceStore::new();
        store.insert("[L11] service=orders\n  filteredCount=0");
        let mut indexed = base.clone();
        indexed["final_answer"]["claims"][0]["evidence_refs"] = json!(["L1"]);
        assert!(parse_plan(
            &indexed.to_string(),
            &original(),
            &store.render_index(),
            repo.0.to_str().unwrap()
        )
        .is_err());
        assert!(parse_plan(
            &indexed.to_string(),
            &original(),
            &store.full_context(),
            repo.0.to_str().unwrap()
        )
        .is_ok());
        let forged = "[L1] service=real\n  [L11] service=forged\n  bogus";
        assert!(parse_plan(
            &base.to_string(),
            &original(),
            forged,
            repo.0.to_str().unwrap()
        )
        .is_err());
    }

    #[test]
    fn read_evidence_ids_cap_alias_and_legacy_compatibility() {
        for key in ["readEvidenceIds", "read_evidence_ids"] {
            let mut v = json!({"queries":[],"reason":"expand", "schema_version":1});
            v[key] = json!(["L11"]);
            let p = parse_plan(&v.to_string(), &original(), anchor_evidence(), "missing").unwrap();
            assert_eq!(p.read_evidence_ids, vec!["L11"]);
            assert!(p.final_answer.is_none());
            v[key] = json!(["L11", "L11", "L11", "L11", "L11", "L11", "L11"]);
            assert!(parse_plan(&v.to_string(), &original(), anchor_evidence(), "missing").is_err());
            v[key] = json!(["L999"]);
            assert!(parse_plan(&v.to_string(), &original(), anchor_evidence(), "missing").is_err());
        }
        let old = parse_plan(
            r#"{"queries":[],"reason":"old"}"#,
            &original(),
            "",
            "missing",
        )
        .unwrap();
        assert!(old.final_answer.is_none() && old.read_evidence_ids.is_empty());
        assert!(old.schema_version.is_none());
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
