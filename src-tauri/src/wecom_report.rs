//! Versioned diagnostic reports. Model prose is not verification authority.
//! `validate` checks record identities and bounded workspace line excerpts, not causality.
use crate::{wecom_investigation_plan as plan, wecom_investigation_query::InvestigationQuery};
use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub text: String,
    #[serde(alias = "evidenceRefs")]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TimelineEntry {
    #[serde(alias = "evidenceRefs")]
    pub evidence_refs: Vec<String>,
    /// An interpretation, never a model-supplied timestamp, service or log quotation.
    pub interpretation: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Cause {
    pub result: Vec<Claim>,
    pub direct: Vec<Claim>,
    pub trigger: Vec<Claim>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeCheck {
    pub id: String,
    pub path: String,
    pub line: usize,
    pub excerpt: String,
    pub claim: Claim,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Gap {
    pub text: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticReport {
    pub version: u8,
    pub level: String,
    pub summary: Claim,
    /// Compatibility only. The renderer always uses the original host query as subject.
    #[serde(default)]
    pub subject: Option<Claim>,
    pub timeline: Vec<TimelineEntry>,
    pub cause: Cause,
    #[serde(alias = "codeChecks")]
    pub code_checks: Vec<CodeCheck>,
    #[serde(alias = "ruledOut")]
    pub ruled_out: Vec<Claim>,
    pub gaps: Vec<Gap>,
    #[serde(alias = "nextActions")]
    pub next_actions: Vec<Claim>,
    /// Host-only compatibility facts; serde rejects an incoming `legacy_facts` key.
    #[serde(skip)]
    pub legacy_facts: Option<Vec<Claim>>,
}

pub fn schema_instruction() -> String {
    r#"diagnostic_report is a strict version-1 object (no unknown fields or verification booleans):
{"version":1,"level":"result","summary":{"text":"short conclusion","evidence_refs":["L1"]},"subject":null,"timeline":[{"evidence_refs":["L1"],"interpretation":"model interpretation, not quoted log content"}],"cause":{"result":[{"text":"observed result","evidence_refs":["L1"]}],"direct":[],"trigger":[]},"code_checks":[],"ruled_out":[],"gaps":[{"text":"missing trigger evidence","blocking":true}],"next_actions":[{"text":"suggest collecting the missing decision input","evidence_refs":[]}]}
level is exactly unknown/result/direct_cause/trigger_verified, never above the host max_level. Each factual claim needs real retained record IDs (L1/T1/R1L1) or verified code IDs C1. A code_check is {"id":"C1","path":"src/file.rs","line":1,"excerpt":"actual single source line fragment","claim":{"text":"code observation","evidence_refs":["L1"]}}; it must reference real log/trace records, not itself or other code IDs. timeline accepts record IDs only; timestamp/service/instance/content are read locally from evidence, never supplied by the model. subject is optional and ignored for query display. summary <=240 characters; timeline <=8 entries/8 distinct record IDs; code_checks <=8; ruled_out <=3; cause arrays <=8 each; gaps and next_actions <=8. trigger_verified requires result/direct/trigger claims, a referenced code check, and zero blocking gaps. Nonblocking limitations (such as global truncation) do not negate verified local facts. next_actions are unexecuted suggestions and may have no refs. Source-line and record-existence verification does NOT prove causality or deployed-version identity. Do not assert verified=true or invent timestamps, service names, WARN numbers, source lines or original quotations."#.into()
}

pub fn parse_report(
    text: &str,
    original: &InvestigationQuery,
    evidence: &str,
    repo: &str,
    max_level: &str,
) -> Result<DiagnosticReport, String> {
    if text.len() > 96 * 1024 {
        return Err("report_error: report exceeds 96 KiB".into());
    }
    let text = text.trim();
    let body = if let Some(body) = text
        .strip_prefix("```json\n")
        .or_else(|| text.strip_prefix("```json\r\n"))
    {
        body.strip_suffix("```")
            .ok_or("report_error: unclosed json fence")?
            .trim()
    } else {
        text
    };
    let report: DiagnosticReport = serde_json::from_str(body)
        .map_err(|_| "report_error: invalid diagnostic report schema".to_string())?;
    validate(&report, original, evidence, repo, max_level)?;
    Ok(report)
}

fn rank(level: &str) -> Option<u8> {
    match level {
        "unknown" => Some(0),
        "result" => Some(1),
        "direct_cause" => Some(2),
        "trigger_verified" => Some(3),
        _ => None,
    }
}

fn text_bound(text: &str, max: usize) -> Result<(), String> {
    if text.trim().is_empty()
        || text.chars().count() > max
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err("report_error: empty, oversized or control-bearing text".into());
    }
    Ok(())
}

fn record<'a>(evidence: &'a str, id: &str) -> Option<&'a str> {
    // The shared parser requires a unique producer-shaped header, never an inline label.
    plan::evidence_record(evidence, id)
}

fn check_refs(
    refs: &[String],
    evidence: &str,
    codes: &BTreeSet<String>,
    required: bool,
) -> Result<(), String> {
    if refs.len() > 8 || (required && refs.is_empty()) {
        return Err("report_error: each factual claim needs 1..8 references".into());
    }
    let mut unique = BTreeSet::new();
    for id in refs {
        if !unique.insert(id) || (!codes.contains(id) && record(evidence, id).is_none()) {
            return Err("report_error: missing, duplicate or ambiguous evidence reference".into());
        }
    }
    Ok(())
}

/// Local boolean checks are record existence and exact source-line membership only.
/// Success is not a semantic entailment, production-version or causal-proof assertion.
pub fn validate(
    report: &DiagnosticReport,
    original: &InvestigationQuery,
    evidence: &str,
    repo: &str,
    max_level: &str,
) -> Result<(), String> {
    let level = rank(&report.level).ok_or("report_error: invalid level")?;
    let maximum = rank(max_level).ok_or("report_error: invalid max_level")?;
    if report.version != 1 || level > maximum {
        return Err("report_error: unsupported version or level above host ceiling".into());
    }
    let start = DateTime::parse_from_rfc3339(&original.start)
        .map_err(|_| "report_error: invalid original start")?;
    let end = DateTime::parse_from_rfc3339(&original.end)
        .map_err(|_| "report_error: invalid original end")?;
    if start >= end {
        return Err("report_error: invalid original window".into());
    }
    if let Some(facts) = &report.legacy_facts {
        text_bound(&report.summary.text, 4096)?;
        if facts.is_empty()
            || facts.len() > 20
            || report.code_checks.len() > 120
            || (level == 3 && report.gaps.iter().any(|gap| gap.blocking))
        {
            return Err("report_error: invalid legacy report bounds or blocking gaps".into());
        }
        let mut code_ids = BTreeSet::new();
        for code in &report.code_checks {
            if !code_ids.insert(code.id.clone()) {
                return Err("report_error: duplicate legacy code ID".into());
            }
            plan::verify_code_ref(&code.path, code.line, &code.excerpt, repo)?;
            if code.claim.evidence_refs.is_empty()
                || code.claim.evidence_refs.len() > 20
                || code
                    .claim
                    .evidence_refs
                    .iter()
                    .any(|id| record(evidence, id).is_none())
            {
                return Err("report_error: invalid legacy code evidence".into());
            }
        }
        for fact in facts {
            text_bound(&fact.text, 4096)?;
            if fact.evidence_refs.is_empty()
                || fact.evidence_refs.len() > 26
                || fact
                    .evidence_refs
                    .iter()
                    .any(|id| !code_ids.contains(id) && record(evidence, id).is_none())
            {
                return Err("report_error: invalid legacy fact reference".into());
            }
        }
        return Ok(());
    }
    if report.timeline.len() > 8
        || report.code_checks.len() > 8
        || report.ruled_out.len() > 3
        || report.gaps.len() > 8
        || report.next_actions.len() > 8
        || [
            &report.cause.result,
            &report.cause.direct,
            &report.cause.trigger,
        ]
        .iter()
        .any(|v| v.len() > 8)
    {
        return Err("report_error: collection exceeds report limits".into());
    }
    if report.legacy_facts.is_none()
        && ((level < 2 && !report.cause.direct.is_empty())
            || (level < 3 && !report.cause.trigger.is_empty())
            || (level >= 1 && report.cause.result.is_empty())
            || (level >= 2 && report.cause.direct.is_empty())
            || (level == 3
                && (report.cause.trigger.is_empty()
                    || report.code_checks.is_empty()
                    || report.gaps.iter().any(|gap| gap.blocking))))
    {
        return Err("report_error: cause structure or blocking gaps contradict level".into());
    }
    let empty = BTreeSet::new();
    let mut codes = BTreeSet::new();
    for code in &report.code_checks {
        let digits = code.id.strip_prefix('C').unwrap_or("");
        if digits.is_empty()
            || digits.starts_with('0')
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || !codes.insert(code.id.clone())
        {
            return Err("report_error: invalid or duplicate code ID".into());
        }
        text_bound(&code.claim.text, 800)?;
        check_refs(&code.claim.evidence_refs, evidence, &empty, true)?;
        plan::verify_code_ref(&code.path, code.line, &code.excerpt, repo)?;
    }
    let check_claim = |claim: &Claim, required: bool, max: usize| -> Result<(), String> {
        text_bound(&claim.text, max)?;
        check_refs(&claim.evidence_refs, evidence, &codes, required)
    };
    check_claim(&report.summary, level > 0, 240)?;
    if let Some(subject) = &report.subject {
        check_claim(subject, true, 240)?;
    }
    for claims in [
        &report.cause.result,
        &report.cause.direct,
        &report.cause.trigger,
        &report.ruled_out,
    ] {
        for claim in claims {
            check_claim(claim, true, 800)?;
        }
    }
    if level == 3
        && !report
            .cause
            .trigger
            .iter()
            .any(|claim| claim.evidence_refs.iter().any(|id| codes.contains(id)))
    {
        return Err("report_error: trigger needs a locally checked code reference".into());
    }
    let mut timeline_ids = BTreeSet::new();
    for item in &report.timeline {
        text_bound(&item.interpretation, 400)?;
        check_refs(&item.evidence_refs, evidence, &empty, true)?;
        timeline_ids.extend(item.evidence_refs.iter());
    }
    if timeline_ids.len() > 8 {
        return Err("report_error: timeline exceeds eight records".into());
    }
    for gap in &report.gaps {
        text_bound(&gap.text, 600)?;
    }
    for action in &report.next_actions {
        check_claim(action, false, 600)?;
    }
    Ok(())
}

fn short(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn display(text: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static PRIVATE: OnceLock<Regex> = OnceLock::new();
    let text = crate::wecom_sanitize::redact_local_paths(text, "");
    let text = PRIVATE.get_or_init(|| Regex::new(
        r#"(?i)(?:\b(?:digest|sha256|offset|byte_offset|full_length)\s*[:=]\s*\S+|\b(?:password|passwd|secret|token|access_token|refresh_token|authorization|api_key)["']?\s*[:=]\s*(?:"[^"]*"|'[^']*'|\S+)|\bBearer\s+[a-z0-9._~+/=-]+|\b[a-f0-9]{64}\b|/(?:Users|home|root|tmp|var|private)/[^\s"'<>]+)"#
    ).expect("static report redaction")).replace_all(&text, "[已隐藏]");
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\r' | '\n' | '\t' => out.push(' '),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '!' | '|' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control()
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c) => {}
            c => out.push(c),
        }
    }
    out
}

fn refs(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("[{}]", display(id)))
        .collect::<Vec<_>>()
        .join("、")
}

fn field<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    let mut values = header
        .split_whitespace()
        .filter_map(|v| v.strip_prefix(&prefix));
    let first = values.next().filter(|v| !v.is_empty())?;
    if values.next().is_some() {
        None
    } else {
        Some(first)
    }
}

fn timestamp(header: &str, offset: i32) -> String {
    let ts = field(header, "ts")
        .and_then(|ts| {
            DateTime::parse_from_rfc3339(ts).ok().or_else(|| {
                ts.parse::<i64>()
                    .ok()
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                    .map(|t| t.fixed_offset())
            })
        })
        .or_else(|| {
            let found: Vec<_> = header
                .split_whitespace()
                .filter_map(|v| DateTime::parse_from_rfc3339(v).ok())
                .collect();
            if found.len() == 1 {
                found.first().copied()
            } else {
                None
            }
        });
    match ts {
        Some(ts) => match offset.checked_mul(60).and_then(FixedOffset::east_opt) {
            Some(zone) => ts.with_timezone(&zone).to_rfc3339(),
            None => ts.to_rfc3339(),
        },
        None => "时间未记录".into(),
    }
}

fn evidence_ids(evidence: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in evidence.lines() {
        if let Some(id) = line
            .strip_prefix('[')
            .and_then(|s| s.split_once(']').map(|(id, _)| id))
        {
            if record(evidence, id).is_some() && !ids.iter().any(|old| old == id) {
                ids.push(id.to_string());
            }
        }
        if ids.len() == 8 {
            break;
        }
    }
    ids
}

fn query_header(original: Option<&InvestigationQuery>) -> String {
    let Some(q) = original else {
        return "查询对象：未解析\n查询时间：未解析\n宿主筛选：未解析\n".into();
    };
    let objects = if !q.identifiers.is_empty() {
        q.identifiers
            .iter()
            .map(|id| {
                if id.field.is_empty() {
                    id.value.clone()
                } else {
                    format!("{}={}", id.field, id.value)
                }
            })
            .collect::<Vec<_>>()
            .join("；")
    } else if !q.keywords.is_empty() {
        q.keywords.join("；")
    } else {
        "未指定业务对象".into()
    };
    format!(
        "查询对象：{}\n查询时间：{} — {}\n宿主筛选：service={}；trace={}；level={}\n",
        display(&objects),
        display(&q.start),
        display(&q.end),
        display(q.service.as_deref().unwrap_or("未限定")),
        display(q.trace_id.as_deref().unwrap_or("未限定")),
        display(q.level.as_deref().unwrap_or("未限定"))
    )
}

fn record_body(block: &str) -> String {
    // Producer indents body lines. Unindented collection summaries/footer are not log content.
    block
        .lines()
        .skip(1)
        .take_while(|line| line.is_empty() || line.starts_with(char::is_whitespace))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_evidence(
    out: &mut String,
    ids: &[String],
    timeline: &[TimelineEntry],
    evidence: &str,
    offset: i32,
) {
    out.push_str("\n### 一、关键证据链\n\n");
    let records: Vec<_> = ids
        .iter()
        .filter_map(|id| record(evidence, id).map(|block| (id, block)))
        .take(8)
        .collect();
    if records.is_empty() {
        out.push_str("尚无可展示的已核对记录。\n");
        return;
    }
    out.push_str("| 证据 / 时间 | 服务 / 实例 | 级别 | 原文短摘录 |\n| --- | --- | --- | --- |\n");
    for (id, block) in &records {
        let header = block.lines().next().unwrap_or("");
        let body = record_body(block);
        out.push_str(&format!(
            "| [{}] {} | {} / {} | {} | {} |\n",
            display(id),
            display(&timestamp(header, offset)),
            display(field(header, "service").unwrap_or("未记录")),
            display(field(header, "instance").unwrap_or("未记录")),
            display(field(header, "level").unwrap_or("未记录")),
            display(&short(&body, 100))
        ));
    }
    for entry in timeline.iter().take(8) {
        if entry
            .evidence_refs
            .iter()
            .all(|id| record(evidence, id).is_some())
        {
            out.push_str(&format!(
                "\n- 模型解释 {}：{}\n",
                refs(&entry.evidence_refs),
                display(&short(&entry.interpretation, 400))
            ));
        }
    }
    out.push_str("\n原文片段（仅保留短摘录，不代表完整日志）：\n");
    for (id, block) in records.iter().take(6) {
        let body = record_body(block);
        if !body.is_empty() {
            out.push_str(&format!(
                "\n> [{}] {}\n",
                display(id),
                display(&short(&body, 200))
            ));
        }
    }
}

fn render_claims(out: &mut String, label: &str, claims: &[Claim]) {
    out.push_str(&format!("\n**{label}**\n"));
    if claims.is_empty() {
        out.push_str("\n尚未确认。\n");
    }
    for claim in claims {
        out.push_str(&format!(
            "\n- {}（引用：{}）\n",
            display(&claim.text),
            refs(&claim.evidence_refs)
        ));
    }
}

/// Render a locally validated report, or an already-verified legacy mapping.
/// No model-generated timestamp/content is used as an original evidence quotation.
pub fn render(report: &DiagnosticReport, original: &InvestigationQuery, evidence: &str) -> String {
    render_inner(report, Some(original), evidence)
}

fn render_inner(
    report: &DiagnosticReport,
    original: Option<&InvestigationQuery>,
    evidence: &str,
) -> String {
    let label = match report.level.as_str() {
        "result" => "观察到的结果",
        "direct_cause" => "直接原因（证据支持的解释）",
        "trigger_verified" => "具体触发条件（引用与代码已核对）",
        _ => "未确定 / 部分报告",
    };
    let mut out = format!(
        "## 排查结论\n\n{}\n\n结论层级：{label}\n\n{}",
        display(&short(&report.summary.text, 240)),
        query_header(original)
    );
    let mut ids = Vec::new();
    for id in report
        .timeline
        .iter()
        .flat_map(|e| &e.evidence_refs)
        .chain(report.summary.evidence_refs.iter())
        .chain(
            report
                .code_checks
                .iter()
                .flat_map(|code| &code.claim.evidence_refs),
        )
        .chain(
            report
                .legacy_facts
                .iter()
                .flatten()
                .flat_map(|claim| &claim.evidence_refs),
        )
        .chain(
            report
                .cause
                .result
                .iter()
                .chain(&report.cause.direct)
                .chain(&report.cause.trigger)
                .flat_map(|c| &c.evidence_refs),
        )
    {
        if record(evidence, id).is_some() && !ids.contains(id) {
            ids.push(id.clone());
        }
        if ids.len() == 8 {
            break;
        }
    }
    render_evidence(
        &mut out,
        &ids,
        &report.timeline,
        evidence,
        original.map_or(0, |q| q.timezone_offset_minutes),
    );
    out.push_str("\n### 二、原因与代码核对\n");
    if let Some(facts) = &report.legacy_facts {
        out.push_str("\n层级沿用旧协议核对结果；旧事实未自动归类为结果、直接原因或触发条件。\n");
        render_claims(&mut out, "已验证事实（旧版映射，未自动归类）", facts);
    }
    render_claims(&mut out, "结果 / 已验证事实", &report.cause.result);
    render_claims(&mut out, "直接原因", &report.cause.direct);
    render_claims(&mut out, "具体触发条件", &report.cause.trigger);
    out.push_str("\n**代码核对**\n");
    if report.code_checks.is_empty() {
        out.push_str("\n本报告未附代码行核对。\n");
    }
    for code in &report.code_checks {
        out.push_str(&format!(
            "\n- [{}] {}:{}；{}（引用：{}）\n  原文：{}\n",
            display(&code.id),
            display(&code.path.replace('\\', "/")),
            code.line,
            display(&code.claim.text),
            refs(&code.claim.evidence_refs),
            display(&short(&code.excerpt, 240))
        ));
    }
    out.push_str(
        "\n引用和代码匹配不证明因果关系；代码按当前工作区核对，线上版本可能不同。未确认项见下。\n",
    );
    out.push_str("\n### 三、容易误判的线索\n");
    if report.ruled_out.is_empty() {
        out.push_str("\n尚无已核对的排除项；日志级别、时间相邻和同一 trace 本身不等于根因。\n");
    }
    for claim in report.ruled_out.iter().take(3) {
        out.push_str(&format!(
            "\n- {}（引用：{}）\n",
            display(&claim.text),
            refs(&claim.evidence_refs)
        ));
    }
    out.push_str("\n### 四、未确认项与下一步\n");
    if report.gaps.is_empty() {
        out.push_str("\n未列出额外缺口；这不表示全局事实完整。\n");
    }
    for gap in &report.gaps {
        out.push_str(&format!(
            "\n- {}：{}\n",
            if gap.blocking {
                "阻断触发结论"
            } else {
                "非阻断限制"
            },
            display(&gap.text)
        ));
    }
    if report.next_actions.is_empty() {
        out.push_str(
            "\n建议（未执行）：补充缺失的决策输入、对应上下文和线上版本信息，再核对触发条件。\n",
        );
    }
    for action in &report.next_actions {
        out.push_str(&format!(
            "\n- 建议（未执行）：{}{}\n",
            display(&action.text),
            if action.evidence_refs.is_empty() {
                String::new()
            } else {
                format!("（依据：{}）", refs(&action.evidence_refs))
            }
        ));
    }
    out
}

/// Legacy facts remain generic facts, even if the old level used causal terminology.
/// The mapper does not guess which sentence is a direct cause or trigger.
pub fn from_legacy(
    answer: &plan::VerifiedAnswer,
    level: &str,
    gaps: &[String],
    stop: &str,
) -> DiagnosticReport {
    let mut code_checks = Vec::new();
    let mut result = Vec::new();
    for legacy in &answer.claims {
        let mut claim = Claim {
            text: legacy.text.clone(),
            evidence_refs: legacy.evidence_refs.clone(),
        };
        for code in &legacy.code_refs {
            let id = format!("C{}", code_checks.len() + 1);
            code_checks.push(CodeCheck {
                id: id.clone(),
                path: code.path.clone(),
                line: code.line,
                excerpt: code.excerpt.clone(),
                claim: Claim {
                    text: "旧协议已核对的代码片段（未分类为原因）".into(),
                    evidence_refs: legacy.evidence_refs.clone(),
                },
            });
            claim.evidence_refs.push(id);
        }
        result.push(claim);
    }
    let mut seen = BTreeSet::new();
    let refs: Vec<String> = answer
        .claims
        .iter()
        .flat_map(|c| &c.evidence_refs)
        .filter(|id| seen.insert((*id).clone()))
        .take(8)
        .cloned()
        .collect();
    let mut report_gaps: Vec<_> = gaps
        .iter()
        .map(|text| Gap {
            text: text.clone(),
            blocking: true,
        })
        .collect();
    if !stop.trim().is_empty() {
        report_gaps.push(Gap {
            text: format!("本轮停止说明：{stop}"),
            blocking: false,
        });
    }
    DiagnosticReport {
        version: 1,
        level: if rank(level).is_some() {
            level
        } else {
            "unknown"
        }
        .into(),
        summary: Claim {
            text: answer.summary.clone(),
            evidence_refs: refs.clone(),
        },
        subject: None,
        timeline: refs
            .into_iter()
            .map(|id| TimelineEntry {
                evidence_refs: vec![id],
                interpretation: "旧协议保留的事实引用，不额外推断因果。".into(),
            })
            .collect(),
        cause: Cause::default(),
        code_checks,
        ruled_out: vec![],
        gaps: report_gaps,
        next_actions: vec![],
        legacy_facts: Some(result),
    }
}

/// Never echo unverified model prose or raw errors. Only local reason categories are displayed.
pub fn fallback(original: Option<&InvestigationQuery>, evidence: &str, reason: &str) -> String {
    let summary = if reason == "cancelled" {
        "本轮排查已停止，以下为已保留证据的部分报告。"
    } else if reason == "budget" {
        "本轮采集预算已用完，以下为已保留证据的部分报告。"
    } else {
        "尚未形成通过本地核对的诊断结论，以下仅展示已保留的证据。"
    };
    let ids = evidence_ids(evidence);
    let report = DiagnosticReport {
        version: 1,
        level: "unknown".into(),
        summary: Claim {
            text: summary.into(),
            evidence_refs: ids,
        },
        subject: None,
        timeline: vec![],
        cause: Cause::default(),
        code_checks: vec![],
        ruled_out: vec![],
        gaps: vec![Gap {
            text: "原因与触发条件尚待核对；未使用未通过验证的模型文字。".into(),
            blocking: true,
        }],
        next_actions: vec![],
        legacy_facts: None,
    };
    render_inner(&report, original, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    const EVIDENCE: &str = "[L1] service=orders instance=host-a level=WARN trace=t1 ts=2026-10-29T10:00:01+08:00 digest=sha256:private offset=123\n  WARN rejected limit=17 actual=23\n[L2] service=worker instance=host-b level=INFO ts=2026-10-29T10:00:02+08:00\n  completed result=0\n";
    fn original() -> InvestigationQuery {
        serde_json::from_value(serde_json::json!({"start":"2026-10-29T10:00:00+08:00","end":"2026-10-29T10:10:00+08:00","timezoneOffsetMinutes":480,"keywords":["ORDER-1"],"excludeKeyword":"","matchAny":false,"service":"orders","traceId":null,"level":null,"page":1})).unwrap()
    }
    fn sample() -> DiagnosticReport {
        DiagnosticReport {
            version: 1,
            level: "result".into(),
            summary: Claim {
                text: "已观察到拒绝结果，触发原因仍待核对。".into(),
                evidence_refs: vec!["L1".into()],
            },
            subject: None,
            timeline: vec![TimelineEntry {
                evidence_refs: vec!["L1".into()],
                interpretation: "拒绝日志是结果证据。".into(),
            }],
            cause: Cause {
                result: vec![Claim {
                    text: "记录包含拒绝结果。".into(),
                    evidence_refs: vec!["L1".into()],
                }],
                ..Cause::default()
            },
            code_checks: vec![],
            ruled_out: vec![],
            gaps: vec![Gap {
                text: "尚缺决策输入。".into(),
                blocking: true,
            }],
            next_actions: vec![Claim {
                text: "采集相邻决策日志。".into(),
                evidence_refs: vec![],
            }],
            legacy_facts: None,
        }
    }
    #[test]
    fn parses_report_and_render_is_byte_deterministic() {
        let text = serde_json::to_string(&sample()).unwrap();
        let report = parse_report(&text, &original(), EVIDENCE, ".", "result").unwrap();
        assert_eq!(report, sample());
        let actual = render(&report, &original(), EVIDENCE);
        assert_eq!(actual, render(&report, &original(), EVIDENCE));
        assert!(actual.starts_with(
            "## 排查结论\n\n已观察到拒绝结果，触发原因仍待核对。\n\n结论层级：观察到的结果\n"
        ));
        assert!(actual.contains("| [L1] 2026-10-29T10:00:01+08:00 | orders / host-a | WARN | WARN rejected limit=17 actual=23 |"));
        assert_eq!(actual, "## 排查结论\n\n已观察到拒绝结果，触发原因仍待核对。\n\n结论层级：观察到的结果\n\n查询对象：ORDER-1\n查询时间：2026-10-29T10:00:00+08:00 — 2026-10-29T10:10:00+08:00\n宿主筛选：service=orders；trace=未限定；level=未限定\n\n### 一、关键证据链\n\n| 证据 / 时间 | 服务 / 实例 | 级别 | 原文短摘录 |\n| --- | --- | --- | --- |\n| [L1] 2026-10-29T10:00:01+08:00 | orders / host-a | WARN | WARN rejected limit=17 actual=23 |\n\n- 模型解释 [L1]：拒绝日志是结果证据。\n\n原文片段（仅保留短摘录，不代表完整日志）：\n\n> [L1] WARN rejected limit=17 actual=23\n\n### 二、原因与代码核对\n\n**结果 / 已验证事实**\n\n- 记录包含拒绝结果。（引用：[L1]）\n\n**直接原因**\n\n尚未确认。\n\n**具体触发条件**\n\n尚未确认。\n\n**代码核对**\n\n本报告未附代码行核对。\n\n引用和代码匹配不证明因果关系；代码按当前工作区核对，线上版本可能不同。未确认项见下。\n\n### 三、容易误判的线索\n\n尚无已核对的排除项；日志级别、时间相邻和同一 trace 本身不等于根因。\n\n### 四、未确认项与下一步\n\n- 阻断触发结论：尚缺决策输入。\n\n- 建议（未执行）：采集相邻决策日志。\n");
    }

    #[test]
    fn metadata_comes_from_evidence_and_original_not_subject_or_body() {
        let mut report = sample();
        report.subject = Some(Claim {
            text: "fake-object fake-service 1999-01-01".into(),
            evidence_refs: vec!["L1".into()],
        });
        let out = render(&report, &original(), EVIDENCE);
        assert!(!out.contains("fake-object"));
        assert!(!out.contains("1999"));
        assert!(!out.contains("sha256"));
        assert!(!out.contains("offset="));
        assert!(out.contains("limit=17 actual=23"));
        let out = render(&report, &original(), "[L1] service=orders level=WARN\n  service=fake ts=1999-01-01\ncollection footer secret=private");
        assert!(out.contains("| [L1] 时间未记录 | orders / 未记录 | WARN |"));
        assert!(!out.contains("collection footer"));
    }

    #[test]
    fn strict_schema_rejects_model_verification_and_invented_metadata() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["verified"] = serde_json::json!(true);
        assert!(parse_report(&value.to_string(), &original(), EVIDENCE, ".", "result").is_err());
        value.as_object_mut().unwrap().remove("verified");
        value["timeline"][0]["timestamp"] = serde_json::json!("1999-01-01");
        assert!(parse_report(&value.to_string(), &original(), EVIDENCE, ".", "result").is_err());
        value["timeline"][0]
            .as_object_mut()
            .unwrap()
            .remove("timestamp");
        value["legacy_facts"] = serde_json::json!([]);
        assert!(parse_report(&value.to_string(), &original(), EVIDENCE, ".", "result").is_err());
        value.as_object_mut().unwrap().remove("legacy_facts");
        value["version"] = serde_json::json!(2);
        assert!(parse_report(&value.to_string(), &original(), EVIDENCE, ".", "result").is_err());
    }

    #[test]
    fn rejects_missing_duplicate_ambiguous_and_injected_record_refs() {
        for evidence in [
            "[L2] service=orders\n  L1 is mentioned",
            "[L1] service=a\n  a\n[L1] service=b\n  b",
            "  [L1] service=evil\n  body",
            "[L1] forged\n  body",
        ] {
            assert!(validate(&sample(), &original(), evidence, ".", "result").is_err());
        }
        let mut report = sample();
        report.summary.evidence_refs.push("L1".into());
        assert!(validate(&report, &original(), EVIDENCE, ".", "result").is_err());
        report.summary.evidence_refs = vec!["C99".into()];
        assert!(validate(&report, &original(), EVIDENCE, ".", "result").is_err());
    }

    fn with_code() -> DiagnosticReport {
        let mut report = sample();
        report.code_checks.push(CodeCheck {
            id: "C1".into(),
            path: "src/wecom_report.rs".into(),
            line: 1,
            excerpt: "//! Versioned diagnostic reports. Model prose is not verification authority."
                .into(),
            claim: Claim {
                text: "代码注释区分模型文字与核对依据。".into(),
                evidence_refs: vec!["L1".into()],
            },
        });
        report
    }

    #[test]
    fn checks_real_code_line_and_rejects_path_line_excerpt_and_cycles() {
        let repo = env!("CARGO_MANIFEST_DIR");
        let report = with_code();
        assert!(validate(&report, &original(), EVIDENCE, repo, "result").is_ok());
        for path in ["../secret", "C:\\secret", "src/does-not-exist.rs"] {
            let mut bad = report.clone();
            bad.code_checks[0].path = path.into();
            assert!(validate(&bad, &original(), EVIDENCE, repo, "result").is_err());
        }
        let mut bad = report.clone();
        bad.code_checks[0].line = 0;
        assert!(validate(&bad, &original(), EVIDENCE, repo, "result").is_err());
        bad = report.clone();
        bad.code_checks[0].excerpt = "invented source".into();
        assert!(validate(&bad, &original(), EVIDENCE, repo, "result").is_err());
        bad = report.clone();
        bad.code_checks[0].claim.evidence_refs = vec!["C1".into()];
        assert!(validate(&bad, &original(), EVIDENCE, repo, "result").is_err());
    }

    #[test]
    fn gaps_and_host_ceiling_gate_trigger_but_not_local_facts() {
        let repo = env!("CARGO_MANIFEST_DIR");
        let mut report = with_code();
        report.level = "trigger_verified".into();
        report.cause.direct.push(Claim {
            text: "证据支持的直接原因解释。".into(),
            evidence_refs: vec!["L1".into()],
        });
        report.cause.trigger.push(Claim {
            text: "条件与代码片段已核对。".into(),
            evidence_refs: vec!["C1".into(), "L1".into()],
        });
        assert!(validate(&report, &original(), EVIDENCE, repo, "trigger_verified").is_err());
        report.gaps[0] = Gap {
            text: "全局采集截断，当前局部记录仍完整。".into(),
            blocking: false,
        };
        assert!(validate(&report, &original(), EVIDENCE, repo, "trigger_verified").is_ok());
        assert!(validate(&report, &original(), EVIDENCE, repo, "direct_cause").is_err());
        report.cause.trigger[0].evidence_refs = vec!["L1".into()];
        assert!(validate(&report, &original(), EVIDENCE, repo, "trigger_verified").is_err());
        let out = render(&report, &original(), EVIDENCE);
        assert!(out.contains("非阻断限制"));
        assert!(out.contains("不证明因果关系"));
    }

    #[test]
    fn escapes_model_and_evidence_markup_without_changing_warn_values() {
        let mut report = sample();
        report.summary.text = "<script>x</script> [link](https://evil) | **bold**".into();
        let evidence = "[L1] service=orders ts=2026-10-29T10:00:01+08:00 level=WARN\n  WARN actual=23 | <img> secret=abc digest=sha256:abc offset=7 C:\\private\\file.txt";
        let out = render(&report, &original(), evidence);
        assert!(!out.contains("<script>"));
        assert!(!out.contains("<img>"));
        assert!(!out.contains("[link]("));
        assert!(!out.contains("secret=abc"));
        assert!(!out.contains("sha256:abc"));
        assert!(!out.contains("offset=7"));
        assert!(!out.contains("C:\\private"));
        assert!(out.contains("actual=23"));
        assert!(out.contains("&lt;img&gt;"));
    }

    #[test]
    fn legacy_preserves_facts_and_never_invents_cause() {
        let answer = plan::VerifiedAnswer {
            summary: "长摘要".repeat(120),
            claims: vec![plan::VerifiedClaim {
                text: "已核对的旧事实".repeat(100),
                evidence_refs: vec!["L1".into()],
                code_refs: vec![],
            }],
        };
        let report = from_legacy(
            &answer,
            "trigger_verified",
            &["仍有缺口".into()],
            "预算结束",
        );
        assert_eq!(report.level, "trigger_verified");
        assert!(report.cause.result.is_empty());
        assert!(report.cause.direct.is_empty());
        assert!(report.cause.trigger.is_empty());
        assert_eq!(
            report.legacy_facts.as_ref().unwrap()[0].text,
            answer.claims[0].text
        );
        let out = render(&report, &original(), EVIDENCE);
        assert!(out.contains(&answer.claims[0].text));
        assert!(out.contains("仍有缺口"));
    }

    #[test]
    fn accepts_known_camel_aliases_but_rejects_duplicates_and_limits() {
        let mut value = serde_json::to_value(sample()).unwrap();
        for (snake, camel) in [
            ("code_checks", "codeChecks"),
            ("ruled_out", "ruledOut"),
            ("next_actions", "nextActions"),
        ] {
            let field = value.as_object_mut().unwrap().remove(snake).unwrap();
            value[camel] = field;
        }
        assert!(parse_report(&value.to_string(), &original(), EVIDENCE, ".", "result").is_ok());
        value["code_checks"] = serde_json::json!([]);
        assert!(parse_report(&value.to_string(), &original(), EVIDENCE, ".", "result").is_err());
        let mut report = sample();
        report.ruled_out = vec![report.summary.clone(); 4];
        assert!(validate(&report, &original(), EVIDENCE, ".", "result").is_err());
    }

    #[test]
    fn legacy_mapping_validates_long_summary_without_causal_reclassification() {
        let answer = plan::VerifiedAnswer {
            summary: "旧版摘要".repeat(100),
            claims: vec![plan::VerifiedClaim {
                text: "已核对事实。".into(),
                evidence_refs: vec!["L1".into()],
                code_refs: vec![],
            }],
        };
        let report = from_legacy(&answer, "direct_cause", &["待确认线上版本".into()], "停止");
        assert!(validate(&report, &original(), EVIDENCE, ".", "direct_cause").is_ok());
        assert!(report.cause.direct.is_empty());
        assert!(
            render(&report, &original(), EVIDENCE).contains("已验证事实（旧版映射，未自动归类）")
        );
    }

    #[test]
    fn fallback_has_same_sections_and_does_not_echo_unverified_error() {
        for original in [None, Some(original())] {
            let out = fallback(
                original.as_ref(),
                EVIDENCE,
                "<script>unverified model secret</script>",
            );
            for heading in [
                "## 排查结论",
                "### 一、关键证据链",
                "### 二、原因与代码核对",
                "### 三、容易误判的线索",
                "### 四、未确认项与下一步",
            ] {
                assert!(out.contains(heading));
            }
            assert!(!out.contains("unverified model secret"));
            assert!(out.contains("WARN rejected limit=17 actual=23"));
        }
    }
}
