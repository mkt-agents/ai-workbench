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
    #[serde(default, deserialize_with = "deserialize_subject")]
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

fn deserialize_subject<'de, D>(deserializer: D) -> Result<Option<Claim>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Subject {
        Text(String),
        Claim(Claim),
    }
    Ok(
        Option::<Subject>::deserialize(deserializer)?.map(|value| match value {
            Subject::Text(text) => Claim {
                text,
                evidence_refs: vec![],
            },
            Subject::Claim(claim) => claim,
        }),
    )
}

pub fn schema_instruction() -> String {
    r#"diagnostic_report is a strict version-1 object (no unknown fields or verification booleans):
{"version":1,"level":"result","summary":{"text":"short conclusion","evidence_refs":["L1"]},"subject":null,"timeline":[{"evidence_refs":["L1"],"interpretation":"model interpretation, not quoted log content"}],"cause":{"result":[{"text":"observed result","evidence_refs":["L1"]}],"direct":[],"trigger":[]},"code_checks":[],"ruled_out":[],"gaps":[{"text":"missing trigger evidence","blocking":true}],"next_actions":[{"text":"suggest collecting the missing decision input","evidence_refs":[]}]}
level is exactly unknown/result/direct_cause/trigger_verified, never above the host max_level. Each factual claim needs real retained record IDs (L1/T1/R1L1) or verified code IDs C1. A code_check is {"id":"C1","path":"src/file.rs","line":1,"excerpt":"actual single source line fragment","claim":{"text":"code observation","evidence_refs":["L1"]}}; it must reference real log/trace records, not itself or other code IDs. timeline accepts record IDs only; timestamp/service/instance/content are read locally from evidence, never supplied by the model. subject accepts string/Claim/null and is ignored for query display: the host query owns the title. Render sections are 一、结果（现象）；二、直接原因；三、具体触发原因；下游与伴随信号；未完成项, with a short 证据范围 blockquote and no wide table or repeated original dump. Separate cause.result (observable outcome), cause.direct (immediate reason) and cause.trigger (specific rule plus actual decision values, not merely filteredCount=0). Do not invent a domain-specific trigger. Each claim is rendered with its own real short log quotations. summary <=240 characters; timeline <=32 entries/32 distinct record IDs (display only 8 key records and 6 original excerpts); code_checks <=8; ruled_out <=3; cause arrays <=8 each; gaps and next_actions <=8. trigger_verified requires result/direct/trigger claims, a referenced code check, and zero blocking gaps. Nonblocking limitations (such as global truncation) do not negate verified local facts. next_actions are unexecuted suggestions and may have no refs. Source-line and record-existence verification does NOT prove causality or deployed-version identity. Do not assert verified=true or invent timestamps, service names, WARN numbers, source lines or original quotations."#.into()
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
    if report.timeline.len() > 32
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
        // Compatibility metadata is not a factual claim or a title authority.
        text_bound(&subject.text, 240)?;
        check_refs(&subject.evidence_refs, evidence, &codes, false)?;
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
    if timeline_ids.len() > 32 {
        return Err("report_error: timeline exceeds 32 records".into());
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
            Some(zone) => ts.with_timezone(&zone).format("%H:%M:%S%.3f").to_string(),
            None => ts.format("%H:%M:%S%.3f").to_string(),
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

fn query_subject(original: Option<&InvestigationQuery>) -> String {
    let Some(q) = original else {
        return "当前问题".into();
    };
    if !q.identifiers.is_empty() {
        q.identifiers
            .iter()
            .map(|id| id.value.clone())
            .collect::<Vec<_>>()
            .join("；")
    } else if !q.keywords.is_empty() {
        q.keywords.join("；")
    } else {
        "当前问题".into()
    }
}

fn query_header(original: Option<&InvestigationQuery>) -> String {
    let Some(q) = original else {
        return "> 证据范围：查询条件未解析。\n".into();
    };
    let mut scope = vec![format!("{} — {}", q.start, q.end)];
    for (key, value) in [
        ("服务", &q.service),
        ("trace", &q.trace_id),
        ("级别", &q.level),
    ] {
        if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
            scope.push(format!("{key}={value}"));
        }
    }
    format!("> 证据范围：{}。\n", display(&scope.join("；")))
}

fn record_body(block: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static EXCERPT: OnceLock<Regex> = OnceLock::new();
    static RANGE: OnceLock<Regex> = OnceLock::new();
    let header = block.lines().next().unwrap_or("");
    let lines: Vec<_> = block
        .lines()
        .skip(1)
        .take_while(|line| line.is_empty() || line.starts_with(char::is_whitespace))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    // Only a complete first-line producer envelope followed by an offset range owns
    // range labels. Metadata is cross-checked when present; legacy record heads omit it.
    // An identical-looking phrase elsewhere in business text is ordinary source data.
    let envelope = EXCERPT.get_or_init(|| Regex::new(
        r"^\[excerpt original_length=([0-9]+) hash=(sha256:[a-f0-9]{64}) truncated=true offset_unit=char\]$"
    ).expect("excerpt envelope regex"));
    let range = RANGE.get_or_init(|| {
        Regex::new(r"^\[offset=([0-9]+)\.\.([0-9]+)\] (.*)$").expect("excerpt range regex")
    });
    let generated = lines
        .first()
        .and_then(|line| envelope.captures(line))
        .is_some_and(|caps| {
            field(header, "truncated").is_none_or(|v| v == "true")
                && field(header, "full_length")
                    .is_none_or(|v| Some(v) == caps.get(1).map(|v| v.as_str()))
                && field(header, "digest")
                    .is_none_or(|v| Some(v) == caps.get(2).map(|v| v.as_str()))
                && lines.get(1).is_some_and(|line| range.is_match(line))
        });
    if generated {
        lines
            .iter()
            .skip(1)
            .map(|line| {
                range
                    .captures(line)
                    .map_or_else(|| (*line).to_owned(), |caps| caps[3].to_owned())
            })
            .collect::<Vec<_>>()
            .join(" … ")
    } else {
        lines.join(" ")
    }
}

/// Select a contiguous original business fragment, not a timestamp/thread/logger prefix.
/// Signals are domain-neutral. Never clip in the middle of a WARN numeric token.
fn business_excerpt(body: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static SIGNAL: OnceLock<Regex> = OnceLock::new();
    let business = body.split_once(" - ").map_or(body, |(_, text)| text).trim();
    let signal = SIGNAL.get_or_init(|| Regex::new(
        r"(?i)原因|判定|规则|条件|\breason\b|\bcause(?:d by)?\b|\bWARN(?:ING)?\b|\bBusinessException\b|\b[a-z_]*(?:limit|actual|threshold|count|allowed|eligible|successflag)\s*[=:]"
    ).expect("business excerpt regex"));
    let chars: Vec<_> = business.chars().collect();
    if chars.len() <= 240 {
        return business.to_owned();
    }
    let at = signal
        .find(business)
        .map_or(0, |m| business[..m.start()].chars().count());
    let mut start = at.saturating_sub(32).min(chars.len().saturating_sub(240));
    while start > 0 && !chars[start - 1].is_whitespace() && chars[start - 1].is_ascii_digit() {
        start -= 1;
    }
    let mut end = (start + 240).min(chars.len());
    // A number and its sign/fraction/unit are a single display token.
    while end < chars.len()
        && !chars[end].is_whitespace()
        && (chars[end].is_ascii_digit() || matches!(chars[end], '.' | '_' | '%' | '+' | '-'))
    {
        end += 1;
    }
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        chars[start..end].iter().collect::<String>(),
        if end < chars.len() { "…" } else { "" }
    )
}

struct EvidenceDisplay<'a> {
    report: &'a DiagnosticReport,
    evidence: &'a str,
    offset: i32,
    key_ids: Vec<String>,
    quoted: BTreeSet<String>,
    shown: BTreeSet<String>,
}

impl<'a> EvidenceDisplay<'a> {
    fn new(report: &'a DiagnosticReport, evidence: &'a str, offset: i32) -> Self {
        let mut key_ids = Vec::new();
        // Prefer causal evidence over incidental timeline rows when the mobile budget fills.
        for id in report
            .cause
            .trigger
            .iter()
            .chain(&report.cause.direct)
            .chain(&report.cause.result)
            .chain(report.legacy_facts.iter().flatten())
            .chain(std::iter::once(&report.summary))
            .flat_map(|claim| &claim.evidence_refs)
            .chain(
                report
                    .timeline
                    .iter()
                    .flat_map(|entry| &entry.evidence_refs),
            )
            .chain(
                report
                    .ruled_out
                    .iter()
                    .flat_map(|claim| &claim.evidence_refs),
            )
        {
            let expanded: Vec<_> = if record(evidence, id).is_some() {
                vec![id]
            } else {
                report
                    .code_checks
                    .iter()
                    .find(|code| &code.id == id)
                    .map(|code| code.claim.evidence_refs.iter().collect())
                    .unwrap_or_default()
            };
            for id in expanded {
                if key_ids.len() < 8 && record(evidence, id).is_some() && !key_ids.contains(id) {
                    key_ids.push(id.clone());
                }
            }
        }
        Self {
            report,
            evidence,
            offset,
            key_ids,
            quoted: BTreeSet::new(),
            shown: BTreeSet::new(),
        }
    }

    fn evidence(&mut self, out: &mut String, ids: &[String]) {
        let mut expanded = Vec::new();
        for id in ids {
            if record(self.evidence, id).is_some() {
                expanded.push(id.clone());
            } else if let Some(code) = self.report.code_checks.iter().find(|code| &code.id == id) {
                expanded.extend(code.claim.evidence_refs.clone());
            }
        }
        let mut seen = BTreeSet::new();
        for id in expanded {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some(block) = record(self.evidence, &id) else {
                continue;
            };
            if !self.key_ids.contains(&id) {
                continue;
            }
            self.shown.insert(id.clone());
            let header = block.lines().next().unwrap_or("");
            let logger = field(header, "logger")
                .filter(|s| !s.is_empty())
                .and_then(|s| s.rsplit('.').next())
                .unwrap_or("类名未记录");
            out.push_str(&format!(
                "\n  > [{}] {} · {} · {}",
                display(&id),
                display(&timestamp(header, self.offset)),
                display(logger),
                display(field(header, "level").unwrap_or("级别未记录"))
            ));
            if self.quoted.contains(&id) {
                out.push_str("（原文见上）\n");
            } else if self.key_ids.iter().take(6).any(|key| key == &id) {
                let body = business_excerpt(&record_body(block));
                if !body.is_empty() {
                    self.quoted.insert(id);
                    out.push_str(&format!("：{}\n", display(&body)));
                } else {
                    out.push_str("（原文未保留）\n");
                }
            } else {
                out.push_str("（原文未展开）\n");
            }
        }
    }

    fn claim(&mut self, out: &mut String, claim: &Claim) {
        out.push_str(&format!(
            "\n- {}{}\n",
            display(&claim.text),
            if claim.evidence_refs.is_empty() {
                String::new()
            } else {
                format!("（{}）", refs(&claim.evidence_refs))
            }
        ));
        self.evidence(out, &claim.evidence_refs);
    }

    fn claims(&mut self, out: &mut String, claims: &[Claim], empty: &str) {
        if claims.is_empty() {
            out.push_str(&format!("\n{empty}\n"));
        }
        for claim in claims {
            self.claim(out, claim);
        }
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
    let mut out = format!(
        "## 排障结论：{}\n\n{}",
        display(&short(&query_subject(original), 240)),
        query_header(original)
    );
    let mut view = EvidenceDisplay::new(
        report,
        evidence,
        original.map_or(0, |q| q.timezone_offset_minutes),
    );
    out.push_str("\n### 一、结果（现象）\n");
    if let Some(facts) = &report.legacy_facts {
        // Legacy summaries may qualify facts without repeating their individual wording.
        view.claim(
            &mut out,
            &Claim {
                text: short(&report.summary.text, 240),
                evidence_refs: report.summary.evidence_refs.clone(),
            },
        );
        out.push_str(
            "\n已验证事实（旧版映射，未自动归类）：以下保留旧事实，不据句式猜测结果或原因。\n",
        );
        view.claims(&mut out, facts, "尚无已核对事实。");
    } else if report.cause.result.is_empty() {
        view.claim(&mut out, &report.summary);
    } else {
        view.claims(&mut out, &report.cause.result, "尚未确认。");
    }
    out.push_str("\n### 二、直接原因\n");
    view.claims(&mut out, &report.cause.direct, "尚未确认直接原因。");
    out.push_str("\n### 三、具体触发原因\n");
    view.claims(
        &mut out,
        &report.cause.trigger,
        "尚缺规则及实际判定值；结果为零本身不等于触发原因。",
    );
    if !report.code_checks.is_empty() {
        out.push_str("\n**规则与代码核对**\n");
        for code in &report.code_checks {
            out.push_str(&format!(
                "\n- [{}] {}:{}：{}\n  > {}\n",
                display(&code.id),
                display(&code.path.replace('\\', "/")),
                code.line,
                display(&code.claim.text),
                display(&short(&code.excerpt, 240))
            ));
            view.evidence(&mut out, &code.claim.evidence_refs);
        }
        out.push_str("\n引用和代码匹配不证明因果关系；当前工作区与线上版本仍需对应。\n");
    }
    out.push_str("\n### 下游与伴随信号\n");
    view.claims(
        &mut out,
        &report.ruled_out,
        "未列出已核对的下游或伴随结论；时间相邻、同 trace、WARN 级别不单独证明根因。",
    );
    for entry in report.timeline.iter().take(32) {
        let unused: Vec<_> = entry
            .evidence_refs
            .iter()
            .filter(|id| view.key_ids.contains(id) && !view.shown.contains(*id))
            .cloned()
            .collect();
        if !unused.is_empty() {
            view.claim(
                &mut out,
                &Claim {
                    text: entry.interpretation.clone(),
                    evidence_refs: unused,
                },
            );
        }
    }
    out.push_str("\n### 未完成项\n");
    if report.gaps.is_empty() {
        out.push_str("\n未列出额外缺口；不代表全局事实完整。\n");
    }
    for gap in &report.gaps {
        out.push_str(&format!(
            "\n- {}：{}\n",
            if gap.blocking {
                "待核对"
            } else {
                "非阻断限制"
            },
            display(&gap.text)
        ));
    }
    for action in &report.next_actions {
        let action = Claim {
            text: format!("建议（未执行）：{}", action.text),
            evidence_refs: action.evidence_refs.clone(),
        };
        view.claim(&mut out, &action);
    }
    if report.next_actions.is_empty() {
        out.push_str("\n建议（未执行）：补充决策输入、规则和线上版本后再核对。\n");
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
        assert_eq!(actual, "## 排障结论：ORDER-1\n\n> 证据范围：2026-10-29T10:00:00+08:00 — 2026-10-29T10:10:00+08:00；服务=orders。\n\n### 一、结果（现象）\n\n- 记录包含拒绝结果。（[L1]）\n\n  > [L1] 10:00:01.000 · 类名未记录 · WARN：WARN rejected limit=17 actual=23\n\n### 二、直接原因\n\n尚未确认直接原因。\n\n### 三、具体触发原因\n\n尚缺规则及实际判定值；结果为零本身不等于触发原因。\n\n### 下游与伴随信号\n\n未列出已核对的下游或伴随结论；时间相邻、同 trace、WARN 级别不单独证明根因。\n\n### 未完成项\n\n- 待核对：尚缺决策输入。\n\n- 建议（未执行）：采集相邻决策日志。\n");
        assert!(!actual.lines().any(|line| line.starts_with('|')));
        assert_eq!(
            actual.matches("WARN rejected limit=17 actual=23").count(),
            1
        );
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
        assert!(out.contains("[L1] 时间未记录 · 类名未记录 · WARN"));
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
        report.cause.result[0].text = report.summary.text.clone();
        let out = render(&report, &original(), evidence);
        assert!(out.contains("&lt;script&gt;"));
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
        assert!(out.contains(&short(&answer.summary, 240)));
        assert_eq!(out.matches("WARN rejected limit=17 actual=23").count(), 1);
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
                "## 排障结论：",
                "### 一、结果（现象）",
                "### 二、直接原因",
                "### 三、具体触发原因",
                "### 下游与伴随信号",
                "### 未完成项",
            ] {
                assert!(out.contains(heading));
            }
            assert!(!out.contains("unverified model secret"));
            assert!(out.contains("WARN rejected limit=17 actual=23"));
        }
    }

    #[test]
    fn subject_accepts_string_claim_null_without_overriding_host_query() {
        for subject in [
            serde_json::json!("FAKE 为什么付款失败"),
            serde_json::json!({"text":"FAKE", "evidence_refs":["L1"]}),
            serde_json::Value::Null,
        ] {
            let mut value = serde_json::to_value(sample()).unwrap();
            value["subject"] = subject;
            let report =
                parse_report(&value.to_string(), &original(), EVIDENCE, ".", "result").unwrap();
            let out = render(&report, &original(), EVIDENCE);
            assert!(out.starts_with("## 排障结论：ORDER-1"));
            assert!(!out.contains("FAKE"));
        }
    }

    #[test]
    fn real_producer_excerpt_is_unwrapped_and_long_logger_does_not_hide_warn_values() {
        let logger = format!("com.{}.PaymentDecision", "verylongpackage.".repeat(22));
        let raw = format!("2026-10-29 10:00:01.123 [worker-thread-123] WARN {logger} - WARN refused limit=17.50 actual=23001.25 retry=1000001");
        let hash = "a".repeat(64);
        let block = format!("[L1] service=payments logger={logger} level=WARN ts=2026-10-29T10:00:01.123+08:00 full_length=9999 digest=sha256:{hash} truncated=true\n  [excerpt original_length=9999 hash=sha256:{hash} truncated=true offset_unit=char]\n  [offset=0..700] {raw}\n  [offset=9900..9999] end\n");
        let out = render(&sample(), &original(), &block);
        assert!(out.contains("10:00:01.123 · PaymentDecision · WARN"));
        assert!(out.contains("WARN refused limit=17.50 actual=23001.25 retry=1000001"));
        for metadata in [
            "original_length",
            "offset_unit",
            "truncated=true",
            "hash=",
            "verylongpackage",
            "worker-thread",
            "| ---",
        ] {
            assert!(!out.contains(metadata), "{metadata}: {out}");
        }
        assert_eq!(out.matches("WARN refused limit=17.50").count(), 1);
        assert!(!out.contains("库存"));
    }

    #[test]
    fn excerpt_lookalikes_inside_business_body_are_not_headers() {
        let envelope = format!(
            "[excerpt original_length=9999 hash=sha256:{} truncated=true offset_unit=char]",
            "a".repeat(64)
        );
        let block = format!(
            "[L1] service=payments\n  note {envelope}\n  [offset=0..9] real business text\n"
        );
        let body = record_body(&block);
        assert!(body.contains(&envelope));
        assert!(body.contains("[offset=0..9]"));
        let block =
            format!("[L1] service=payments\n  {envelope}\n  [offset=0..9] real business text\n");
        assert_eq!(record_body(&block), "real business text");
        let block =
            format!("[L1] service=payments\n  {envelope}\n  ordinary text, not an offset range\n");
        assert!(record_body(&block).contains(&envelope));
    }

    #[test]
    fn business_excerpt_finds_generic_rule_values_and_keeps_boundary_number_complete() {
        let body = format!(
            "prefix.logger WARN - {} reason=rate_limit actual=12345678901234567890 limit=5 {}",
            "x ".repeat(180),
            "tail ".repeat(80)
        );
        let shown = business_excerpt(&body);
        assert!(shown.contains("reason=rate_limit actual=12345678901234567890 limit=5"));
        assert!(!shown.contains("prefix.logger"));
        let body = format!("{}12345678901234567890 end", "x ".repeat(117));
        assert!(business_excerpt(&body).contains("12345678901234567890"));
    }

    #[test]
    fn timeline_accepts_32_records_but_mobile_display_is_eight_keys_six_excerpts() {
        let mut report = sample();
        let mut evidence = String::new();
        report.timeline.clear();
        for n in 1..=32 {
            evidence.push_str(&format!("[L{n}] service=payments level=INFO ts=2026-10-29T10:00:01+08:00\n  retained-message-{n}\n"));
            report.timeline.push(TimelineEntry {
                evidence_refs: vec![format!("L{n}")],
                interpretation: format!("观察 {n}"),
            });
        }
        assert!(validate(&report, &original(), &evidence, ".", "result").is_ok());
        let before = report.clone();
        let out = render(&report, &original(), &evidence);
        assert_eq!(report, before);
        assert_eq!(out.matches(" · INFO").count(), 8);
        assert_eq!(out.matches("retained-message-").count(), 6);
        assert!(!out.contains("[L9]"));
        assert!(!out.lines().any(|line| line.starts_with('|')));
        report.timeline.push(TimelineEntry {
            evidence_refs: vec!["L1".into()],
            interpretation: "extra".into(),
        });
        assert!(validate(&report, &original(), &evidence, ".", "result").is_err());
    }

    #[test]
    fn each_cause_has_real_evidence_but_shared_original_is_not_dumped_twice() {
        let mut report = sample();
        report.cause.direct = vec![Claim {
            text: "直接原因".into(),
            evidence_refs: vec!["L1".into()],
        }];
        report.cause.trigger = vec![Claim {
            text: "规则 limit=17；实际 actual=23".into(),
            evidence_refs: vec!["L1".into()],
        }];
        let out = render(&report, &original(), EVIDENCE);
        assert_eq!(out.matches("WARN rejected limit=17 actual=23").count(), 1);
        assert_eq!(out.matches("[L1] 10:00:01.000").count(), 3);
        assert_eq!(out.matches("（原文见上）").count(), 2);
    }

    #[test]
    fn production_warn_business_ids_survive_long_runtime_prefix() {
        let evidence = "[L1] 查询范围内 service=pmys.saas.match.service instance=10.80.85.103 level=WARN ts=1793159847950 thread=wxSalesMatchDisruptor-thread-7 logger=com.pmys.saas.match.capability.AbstractMatchPackageCapabilityAction\n  11:57:27.950 [wxSalesMatchDisruptor-thread-7] WARN  com.pmys.saas.match.capability.AbstractMatchPackageCapabilityAction - pmys.saas.match.service-[|]-prod-[|]-10.80.85.103-[|]-MATCH_PUSH_TOPIC-[|]-无能力，跳过 pkid=1454805448406827008 nlpId=4209234431769610323_wxid_w1r8igtdjjmi29 accountId=1429785674462261248\n";
        let out = render(&sample(), &original(), evidence);
        assert!(out.contains("11:57:27.950 · AbstractMatchPackageCapabilityAction · WARN"));
        for text in [
            "无能力，跳过",
            "1454805448406827008",
            "4209234431769610323",
            "1429785674462261248",
        ] {
            assert!(out.contains(text), "missing {text}: {out}");
        }
        assert!(!out.contains("wxSalesMatchDisruptor-thread-7"));
        assert!(!out.contains("com.pmys.saas.match.capability."));
        assert!(!out.lines().any(|line| line.starts_with('|')));
    }
}
