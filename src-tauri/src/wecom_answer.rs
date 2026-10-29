//! User-facing answer boundary. Investigation output stays private until final validation.
use crate::wecom_investigation_query::InvestigationQuery;

const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// `protected` is only for investigation turns. Keep the progress placeholder visible,
/// not model tokens: even a Markdown prefix can be followed by internal JSON later.
/// Unprotected turns retain ordinary cumulative streaming, including user-requested JSON.
pub struct AnswerBuffer {
    protected: bool,
    text: String,
    overflowed: bool,
    diagnostic_request: Option<String>,
}

impl AnswerBuffer {
    pub fn new(protected: bool) -> Self {
        Self {
            protected,
            text: String::new(),
            overflowed: false,
            diagnostic_request: None,
        }
    }

    pub fn with_request(protected: bool, request: &str) -> Self {
        let mut buffer = Self::new(protected);
        buffer.diagnostic_request = Some(request.to_string());
        buffer
    }

    fn failure(
        &self,
        original: Option<&InvestigationQuery>,
        evidence: &str,
        _stage: &str,
        error: &str,
    ) -> String {
        if let Some(request) = &self.diagnostic_request {
            let code = crate::wecom_investigation_plan::validation_error_code(error);
            crate::wecom_performance::note_validation(request, code);
            #[cfg(not(test))]
            crate::wecom_log::write(
                "answer-validation",
                &serde_json::json!({"requestId":request,"stage":_stage,"code":code}).to_string(),
            );
        }
        unverified(original, evidence)
    }

    pub fn push(&mut self, chunk: &str) -> Option<String> {
        if !self.overflowed {
            let remaining = MAX_ANSWER_BYTES - self.text.len();
            self.overflowed = chunk.len() > remaining;
            self.text.push_str(byte_prefix(chunk, remaining));
        }
        (!self.protected).then(|| self.text.clone())
    }

    pub fn finish(
        &self,
        original: Option<&InvestigationQuery>,
        evidence: &str,
        repo: &str,
        max_level: &str,
    ) -> String {
        self.finish_with_report_ceiling(original, evidence, repo, max_level, max_level)
    }

    /// New reports carry claim-local blocking gaps and source checks. Their host ceiling
    /// may differ from the conservative whole-collection ceiling of the legacy protocol.
    pub fn finish_with_report_ceiling(
        &self,
        original: Option<&InvestigationQuery>,
        evidence: &str,
        repo: &str,
        legacy_max: &str,
        report_max: &str,
    ) -> String {
        if !self.protected {
            return self.text.clone();
        }
        // All protected paths share the report renderer. In particular, plain Markdown
        // is unverified model text, not an alternate report format for a final-model turn.
        // Never copy that text (including an overflow prefix) into the fallback report.
        if self.overflowed {
            return self.failure(original, evidence, "buffer", "plan_error: plan exceeds answer budget");
        }
        let text = self
            .text
            .trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if !protocol_candidate(text) {
            return self.failure(original, evidence, "format", "plan_error: strict JSON format: missing report protocol");
        }
        let Some(original) = original else {
            return self.failure(None, evidence, "query", "plan_error: invalid original window");
        };
        let plan = match crate::wecom_investigation_plan::parse_plan(text, original, evidence, repo)
        {
            Ok(plan) => plan,
            Err(error) => return self.failure(Some(original), evidence, "parse", &error),
        };
        let max_level = if plan.diagnostic_report.is_some() {
            report_max
        } else {
            legacy_max
        };
        if level_rank(&plan.conclusion_level) > level_rank(max_level) {
            return self.failure(Some(original), evidence, "ceiling", "report_error: unsupported version or level above host ceiling");
        }
        let report = if let Some(report) = plan.diagnostic_report {
            // Recheck the report's own level against the runtime coverage ceiling,
            // rather than relying only on the outer plan label.
            if let Err(error) =
                crate::wecom_report::validate(&report, original, evidence, repo, max_level)
            {
                return self.failure(Some(original), evidence, "validate", &error);
            }
            report
        } else if let Some(answer) = plan.final_answer {
            // parse_plan already checked the legacy schema and all record/source refs.
            // Its older text/collection bounds differ from the new wire schema; do not
            // reject that valid compatibility mapping using the new schema's limits.
            crate::wecom_report::from_legacy(
                &answer,
                &plan.conclusion_level,
                &plan.gaps,
                &plan.stop_reason,
            )
        } else {
            return self.failure(Some(original), evidence, "report_missing", "plan_error: strict JSON schema: no final report");
        };
        if level_rank(&report.level) > level_rank(max_level) {
            return self.failure(Some(original), evidence, "ceiling", "report_error: unsupported version or level above host ceiling");
        }
        let rendered = crate::wecom_report::render(&report, original, evidence);
        if rendered.len() > MAX_ANSWER_BYTES {
            self.failure(Some(original), evidence, "render", "plan_error: plan exceeds rendered answer budget")
        } else {
            #[cfg(not(test))]
            if let Some(request) = &self.diagnostic_request {
                crate::wecom_diagnostics::record(request, "report", "validated", None, None, serde_json::json!({}));
            }
            rendered
        }
    }
}

/// Classification is over the completed accumulated text, never a single stream chunk.
/// Decode JSON key escapes as well, so a unicode-escaped protocol key is not a bypass.
fn protocol_candidate(text: &str) -> bool {
    if text.starts_with('{') {
        return true;
    }
    let trimmed = text.trim_end();
    if !trimmed.is_empty() && "```json".starts_with(trimmed) {
        return true;
    }
    if let Some(body) = trimmed.strip_prefix("```json") {
        // A complete ordinary JSON fence is not an internal protocol candidate.
        // Protected completion still turns ordinary content into a report fallback.
        let Some(body) = body.trim().strip_suffix("```") else {
            return true;
        };
        if serde_json::from_str::<serde_json::Value>(body.trim()).is_err() {
            return true;
        }
    }
    let keys = [
        "queries",
        "final_answer",
        "finalAnswer",
        "diagnostic_report",
        "diagnosticReport",
        "conclusion_level",
        "conclusionLevel",
        "read_evidence_ids",
        "readEvidenceIds",
    ];
    let mut remaining = text;
    while let Some(start) = remaining.find('"') {
        remaining = &remaining[start..];
        let mut escaped = false;
        let mut end = None;
        // Every quote is a candidate: prose before JSON need not have balanced quotes.
        // Keys are at most 16 ASCII characters (96 bytes when unicode-escaped).
        for (index, ch) in remaining.char_indices().skip(1).take(128) {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                end = Some(index + 1);
                break;
            }
        }
        if let Some(end) = end {
            let key = serde_json::from_str::<String>(&remaining[..end]).ok();
            let tail = &remaining[end..];
            if key.as_deref().is_some_and(|key| keys.contains(&key))
                && (tail.trim_start().starts_with(':') || tail.trim().is_empty())
            {
                return true;
            }
        } else {
            let partial = &remaining[1..];
            if text.contains('{') && keys.iter().any(|key| key.starts_with(partial)) {
                return true;
            }
        }
        remaining = &remaining[1..];
    }
    false
}

fn byte_prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn level_rank(level: &str) -> u8 {
    match level {
        "result" => 1,
        "direct_cause" => 2,
        "trigger_verified" => 3,
        _ => 0,
    }
}

fn unverified(original: Option<&InvestigationQuery>, evidence: &str) -> String {
    crate::wecom_report::fallback(original, evidence, "invalid_model_report")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original() -> InvestigationQuery {
        crate::wecom_investigation_query::parse_query(
            "检查日志",
            None,
            chrono::DateTime::parse_from_rfc3339("2026-10-28T09:00:00+08:00").unwrap(),
        )
        .unwrap()
    }

    fn final_protocol() -> String {
        serde_json::json!({"queries":[],"reason":"完成观察","conclusion_level":"result","gaps":[],
            "final_answer":{"summary":"记录存在业务警告","claims":[{"text":"记录返回零结果","evidence_refs":["L1"]}]}}).to_string()
    }

    #[test]
    fn every_early_fallback_records_a_request_scoped_reason() {
        let query = original();
        for (request, text, with_query, code) in [
            ("fallback-plain", "ordinary markdown".to_string(), true, "format"),
            ("fallback-overflow", "x".repeat(MAX_ANSWER_BYTES + 1), true, "budget"),
            ("fallback-query", "{}".to_string(), false, "window"),
            ("fallback-no-report", r#"{"queries":[],"reason":"stopped"}"#.to_string(), true, "schema"),
        ] {
            let mut buffer = AnswerBuffer::with_request(true, request);
            buffer.push(&text);
            let shown = buffer.finish(if with_query { Some(&query) } else { None }, "", ".", "unknown");
            assert!(shown.contains("排障"));
            let metrics = crate::wecom_performance::summary(request).unwrap();
            assert_eq!(metrics["validationFailureCount"], 1, "{request}");
            assert_eq!(metrics["validationFailures"][0], code, "{request}");
        }
    }

    #[test]
    fn protocol_one_character_chunks_render_only_after_validation() {
        let raw = format!("\u{feff} \n```json\n{}\n```", final_protocol());
        let mut buffer = AnswerBuffer::new(true);
        for ch in raw.chars() {
            assert_eq!(buffer.push(&ch.to_string()), None);
        }
        let answer = buffer.finish(
            Some(&original()),
            "[L1] service=orders WARN result=0",
            ".",
            "result",
        );
        assert!(answer.contains("## 排障结论"));
        assert!(answer.contains("[L1]"));
        assert!(!answer.contains("final_answer"));
        assert!(!answer.contains("queries"));
    }

    #[test]
    fn protocol_after_plain_text_is_never_released() {
        let mut buffer = AnswerBuffer::new(true);
        assert_eq!(buffer.push("先核对日志。\n"), None);
        for ch in final_protocol().chars() {
            assert_eq!(buffer.push(&ch.to_string()), None);
        }
        assert_eq!(
            buffer.finish(
                Some(&original()),
                "[L1] service=orders WARN result=0",
                ".",
                "result"
            ),
            unverified(Some(&original()), "[L1] service=orders WARN result=0")
        );
    }

    #[test]
    fn maximum_level_rejects_claims_not_just_the_label() {
        let raw = final_protocol().replace("\"result\"", "\"trigger_verified\"");
        assert!(crate::wecom_investigation_plan::parse_plan(
            &raw,
            &original(),
            "[L1] service=orders WARN result=0",
            "."
        )
        .is_ok());
        let mut buffer = AnswerBuffer::new(true);
        buffer.push(&raw);
        let answer = buffer.finish(
            Some(&original()),
            "[L1] service=orders WARN result=0",
            ".",
            "direct_cause",
        );
        assert!(answer.contains("尚未形成通过本地核对的诊断结论"));
        assert!(!answer.contains("记录存在业务警告"));
    }

    #[test]
    fn output_is_byte_bounded_and_overflow_never_hides_protocol_tail() {
        for protected in [false, true] {
            let mut buffer = AnswerBuffer::new(protected);
            let prefix = "测".repeat(30_000);
            if let Some(visible) = buffer.push(&prefix) {
                assert!(visible.len() <= 64 * 1024);
            }
            buffer.push(&final_protocol());
            let answer = buffer.finish(Some(&original()), "[L1] service=orders", ".", "result");
            assert!(answer.len() <= 64 * 1024);
            if protected {
                assert_eq!(answer, unverified(Some(&original()), "[L1] service=orders"));
            }
        }
    }

    #[test]
    fn incomplete_protocol_and_partial_fences_never_become_visible() {
        for raw in [
            "{",
            "{\"qu",
            "```",
            "```j",
            "```json\n{",
            "```json\n{}",
            "前文\n{\"final_ans",
        ] {
            let mut buffer = AnswerBuffer::new(true);
            for ch in raw.chars() {
                assert_eq!(buffer.push(&ch.to_string()), None);
            }
            assert_eq!(
                buffer.finish(Some(&original()), "", ".", "result"),
                unverified(Some(&original()), ""),
                "{raw}"
            );
        }
    }

    #[test]
    fn invalid_references_and_trigger_gaps_keep_strict_plan_validation() {
        let valid: serde_json::Value = serde_json::from_str(&final_protocol()).unwrap();
        let mut invalid_ref = valid.clone();
        invalid_ref["final_answer"]["claims"][0]["evidence_refs"] = serde_json::json!(["L999"]);
        let mut invalid_code = valid.clone();
        invalid_code["final_answer"]["claims"][0]["code_refs"] =
            serde_json::json!([{"path":"../missing.rs","line":1,"excerpt":"secret"}]);
        let mut open_gaps = valid;
        open_gaps["conclusion_level"] = serde_json::json!("trigger_verified");
        open_gaps["gaps"] = serde_json::json!(["缺触发依据"]);
        for value in [invalid_ref, invalid_code, open_gaps] {
            let mut buffer = AnswerBuffer::new(true);
            buffer.push(&value.to_string());
            assert_eq!(
                buffer.finish(
                    Some(&original()),
                    "[L1] service=orders",
                    ".",
                    "trigger_verified"
                ),
                unverified(Some(&original()), "[L1] service=orders")
            );
        }
    }

    #[test]
    fn lower_level_final_renders_gaps_but_never_exceeds_maximum_level() {
        for level in ["result", "direct_cause"] {
            let mut value: serde_json::Value = serde_json::from_str(&final_protocol()).unwrap();
            value["conclusion_level"] = serde_json::json!(level);
            value["gaps"] = serde_json::json!(["缺触发依据 <tag>\n# forged"]);
            value["stop_reason"] = serde_json::json!("预算已用完，保留缺口");
            let mut buffer = AnswerBuffer::new(true);
            for ch in value.to_string().chars() {
                assert_eq!(buffer.push(&ch.to_string()), None);
            }
            let rendered = buffer.finish(Some(&original()), "[L1] service=orders", ".", level);
            assert!(rendered.contains("## 排障结论"));
            assert!(rendered.contains("### 未完成项"));
            assert!(rendered.contains("缺触发依据 &lt;tag&gt; \\# forged"));
            assert!(rendered.contains("本轮停止说明：预算已用完，保留缺口"));
            assert!(!rendered.contains("final_answer"));
            assert!(!rendered.contains("\"gaps\""));
            let limited = buffer.finish(Some(&original()), "[L1] service=orders", ".", "unknown");
            assert!(limited.contains("尚未形成通过本地核对的诊断结论"));
            assert!(!limited.contains("记录存在业务警告"));
        }
    }

    #[test]
    fn aliased_protocol_is_rendered_or_suppressed_never_leaked() {
        let raw = r#"{"queries":[],"reason":"观察结束","conclusionLevel":"result","gaps":["缺具体规则"],"finalAnswer":{"summary":"有限观察","claims":[{"text":"零结果","evidenceRefs":["L1"]}]}}"#;
        let mut buffer = AnswerBuffer::new(true);
        buffer.push(raw);
        let rendered = buffer.finish(Some(&original()), "[L1] service=orders", ".", "result");
        assert!(rendered.contains("有限观察"));
        assert!(rendered.contains("缺具体规则"));
        assert!(!rendered.contains("finalAnswer"));
        for raw in [
            format!("前文 {raw}"),
            r#"前文 {"finalAnswer": {}}"#.into(),
            r#"前文 {"conclusionLevel": "result"}"#.into(),
            r#"前文 {"\u0066inalAnswer": {}}"#.into(),
            r#"前文 {"readEvidenceIds": []}"#.into(),
        ] {
            let mut buffer = AnswerBuffer::new(true);
            buffer.push(&raw);
            assert_eq!(
                buffer.finish(Some(&original()), "[L1] service=orders", ".", "result"),
                unverified(Some(&original()), "[L1] service=orders")
            );
        }
    }

    #[test]
    fn queries_only_and_missing_query_context_get_readable_fallback() {
        for raw in [
            r#"{"queries":[],"reason":"仍有缺口","conclusion_level":"result"}"#.to_string(),
            final_protocol(),
        ] {
            let mut buffer = AnswerBuffer::new(true);
            buffer.push(&raw);
            assert_eq!(
                buffer.finish(None, "[L1] service=orders", ".", "result"),
                unverified(None, "[L1] service=orders")
            );
        }
        let mut buffer = AnswerBuffer::new(true);
        buffer.push(r#"{"queries":[],"reason":"仍有缺口"}"#);
        assert_eq!(
            buffer.finish(Some(&original()), "", ".", "result"),
            unverified(Some(&original()), "")
        );
    }

    #[test]
    fn ordinary_json_and_markdown_stream_verbatim_when_not_protected() {
        for raw in [
            "{}".to_string(),
            final_protocol(),
            "## 结果\n正常文本".into(),
        ] {
            let mut buffer = AnswerBuffer::new(false);
            let mut expected = String::new();
            for ch in raw.chars() {
                expected.push(ch);
                assert_eq!(buffer.push(&ch.to_string()), Some(expected.clone()));
            }
            assert_eq!(buffer.finish(None, "", ".", "unknown"), raw);
        }
    }

    #[test]
    fn protected_code_and_ordinary_json_fences_become_structured_fallback() {
        for raw in [
            "```rust\nfn check() { println!(\"ok\"); }\n```",
            "```json\n{\"ok\":true}\n```",
            "```text\n没有更多记录\n```",
        ] {
            let mut buffer = AnswerBuffer::new(true);
            for ch in raw.chars() {
                assert_eq!(buffer.push(&ch.to_string()), None);
            }
            let rendered = buffer.finish(None, "", ".", "unknown");
            assert_eq!(rendered, unverified(None, ""));
            assert!(!rendered.contains(raw));
        }
    }

    #[test]
    fn unmatched_prose_quote_does_not_hide_a_later_protocol_key() {
        let mut buffer = AnswerBuffer::new(true);
        buffer.push("说明中有一个\"引号\n{\"queries\": []}");
        assert_eq!(
            buffer.finish(Some(&original()), "", ".", "result"),
            unverified(Some(&original()), "")
        );
    }

    #[test]
    fn escaped_protocol_keys_in_middle_are_not_a_display_escape_hatch() {
        let mut buffer = AnswerBuffer::new(true);
        buffer.push(r#"说明 {"\u0071ueries": []}"#);
        assert_eq!(
            buffer.finish(Some(&original()), "", ".", "result"),
            unverified(Some(&original()), "")
        );
    }

    #[test]
    fn protected_markdown_waits_until_finish_and_becomes_structured_fallback() {
        let markdown = "\u{feff}  ## 结果\n\n未经验证的根因 <script>alert(1)</script>\n";
        for query in [None, Some(original())] {
            let mut buffer = AnswerBuffer::new(true);
            for ch in markdown.chars() {
                assert_eq!(buffer.push(&ch.to_string()), None);
            }
            let rendered = buffer.finish(query.as_ref(), "[L1] service=orders", ".", "result");
            assert_eq!(rendered, unverified(query.as_ref(), "[L1] service=orders"));
            assert!(!rendered.contains("未经验证的根因"));
            assert!(!rendered.contains("alert(1)"));
        }
    }

    #[test]
    fn direct_and_buffered_legacy_use_exactly_the_same_report_renderer() {
        let query = original();
        let evidence = "[L1] service=orders WARN result=0";
        let raw = final_protocol();
        let plan =
            crate::wecom_investigation_plan::parse_plan(&raw, &query, evidence, ".").unwrap();
        let report = crate::wecom_report::from_legacy(
            plan.final_answer.as_ref().unwrap(),
            &plan.conclusion_level,
            &plan.gaps,
            &plan.stop_reason,
        );
        let direct = crate::wecom_report::render(&report, &query, evidence);
        let mut buffer = AnswerBuffer::new(true);
        assert_eq!(buffer.push(&raw), None);
        assert_eq!(buffer.finish(Some(&query), evidence, ".", "result"), direct);

        let fallback = unverified(Some(&query), evidence);
        let sections = |text: &str| {
            text.lines()
                .filter(|line| line.starts_with("##"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert!(!sections(&direct).is_empty());
        assert_eq!(sections(&direct), sections(&fallback));
    }

    #[test]
    fn valid_legacy_bounds_are_not_rejected_by_new_wire_schema_limits() {
        let query = original();
        let evidence = "[L1] service=orders WARN result=0";
        let mut value: serde_json::Value = serde_json::from_str(&final_protocol()).unwrap();
        value["final_answer"]["summary"] = serde_json::json!("业务警告".repeat(100));
        value["final_answer"]["claims"] = serde_json::json!((0..12)
            .map(|index| serde_json::json!({"text": format!("记录结果 {index}"), "evidence_refs": ["L1"]}))
            .collect::<Vec<_>>());
        let raw = value.to_string();
        let plan =
            crate::wecom_investigation_plan::parse_plan(&raw, &query, evidence, ".").unwrap();
        let mapped = crate::wecom_report::from_legacy(
            plan.final_answer.as_ref().unwrap(),
            &plan.conclusion_level,
            &plan.gaps,
            &plan.stop_reason,
        );
        let direct = crate::wecom_report::render(&mapped, &query, evidence);
        let mut buffer = AnswerBuffer::new(true);
        assert_eq!(buffer.push(&raw), None);
        let rendered = buffer.finish(Some(&query), evidence, ".", "result");
        assert_eq!(rendered, direct);
        assert!(!rendered.contains("尚未形成通过本地核对的诊断结论"));
    }

    #[test]
    fn recorded_shapes_keep_the_cause_in_picture_one_layout() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/wecom-report-recovery");
        let evidence = std::fs::read_to_string(root.join("evidence.txt")).unwrap();
        let q = original();
        for name in ["seq-47.json", "seq-55.json", "seq-63.json"] {
            let text = std::fs::read_to_string(root.join(name)).unwrap();
            let mut buffer = AnswerBuffer::new(true);
            for c in text.chars() {
                assert!(buffer.push(&c.to_string()).is_none());
            }
            let body = buffer.finish_with_report_ceiling(
                Some(&q),
                &evidence,
                root.to_str().unwrap(),
                "direct_cause",
                "trigger_verified",
            );
            assert!(body.contains("库存"), "{name}: {body}");
            assert!(body.contains("PRODUCT-A"), "{name}: {body}");
            assert!(body.contains("STATUS"), "{name}: {body}");
            assert!(!body.contains("尚未形成通过本地核对"), "{name}: {body}");
            assert!(!body.contains("| --- |"));
            assert!(!body.contains("diagnostic_report"));
        }
    }

    fn diagnostic_protocol() -> serde_json::Value {
        serde_json::json!({
            "queries": [], "reason": "完成观察", "conclusion_level": "result", "gaps": [],
            "diagnostic_report": {
                "version": 1, "level": "result",
                "summary": {"text": "已观察业务警告", "evidence_refs": ["L1"]},
                "timeline": [{"evidence_refs": ["L1"], "interpretation": "零结果仅为观察"}],
                "cause": {"result": [{"text": "结果是零 <tag>\n# forged", "evidence_refs": ["L1"]}], "direct": [], "trigger": []},
                "code_checks": [], "ruled_out": [], "gaps": [], "next_actions": []
            }
        })
    }

    #[test]
    fn diagnostic_report_plain_and_fenced_render_exactly_like_direct_report() {
        let query = original();
        let evidence = "[L1] service=orders WARN result=0";
        let json = diagnostic_protocol().to_string();
        for raw in [json.clone(), format!("```json\n{json}\n```")] {
            let plan =
                crate::wecom_investigation_plan::parse_plan(&raw, &query, evidence, ".").unwrap();
            let direct = crate::wecom_report::render(
                plan.diagnostic_report.as_ref().unwrap(),
                &query,
                evidence,
            );
            let mut buffer = AnswerBuffer::new(true);
            for ch in raw.chars() {
                assert_eq!(buffer.push(&ch.to_string()), None);
            }
            let rendered = buffer.finish(Some(&query), evidence, ".", "result");
            assert_eq!(rendered, direct);
            assert!(rendered.contains("结果是零 &lt;tag&gt; \\# forged"));
            assert!(!rendered.contains("<tag>"));
            assert!(!rendered.contains("\n# forged"));
            assert!(!rendered.contains("diagnostic_report"));
            assert!(!rendered.contains("diagnosticReport"));
            assert_eq!(
                buffer.finish(Some(&query), evidence, ".", "unknown"),
                unverified(Some(&query), evidence)
            );
        }
    }

    #[test]
    fn report_and_legacy_use_independent_host_ceilings() {
        let query = original();
        let evidence = "[L1] service=orders WARN result=0";
        for (raw, is_report) in [
            (diagnostic_protocol().to_string(), true),
            (final_protocol(), false),
        ] {
            let mut buffer = AnswerBuffer::new(true);
            assert_eq!(buffer.push(&raw), None);
            let accepted = buffer.finish(Some(&query), evidence, ".", "result");
            let fallback = unverified(Some(&query), evidence);
            assert_ne!(accepted, fallback);
            assert_eq!(
                buffer.finish_with_report_ceiling(Some(&query), evidence, ".", "result", "result"),
                accepted
            );
            assert_eq!(
                buffer.finish_with_report_ceiling(Some(&query), evidence, ".", "unknown", "result"),
                if is_report { &accepted } else { &fallback }.to_owned()
            );
            assert_eq!(
                buffer.finish_with_report_ceiling(Some(&query), evidence, ".", "result", "unknown"),
                if is_report { &fallback } else { &accepted }.to_owned()
            );
        }
        // A higher new-report ceiling never replaces its structural/source validation.
        let mut invalid = diagnostic_protocol();
        invalid["conclusion_level"] = serde_json::json!("trigger_verified");
        invalid["diagnostic_report"]["level"] = serde_json::json!("trigger_verified");
        let mut buffer = AnswerBuffer::new(true);
        buffer.push(&invalid.to_string());
        assert_eq!(
            buffer.finish_with_report_ceiling(
                Some(&query),
                evidence,
                ".",
                "result",
                "trigger_verified"
            ),
            unverified(Some(&query), evidence)
        );
    }

    #[test]
    fn invalid_diagnostic_report_never_displays_unverified_claims() {
        let evidence = "[L1] service=orders";
        let valid = diagnostic_protocol();
        let mut invalid_ref = valid.clone();
        invalid_ref["diagnostic_report"]["summary"]["evidence_refs"] = serde_json::json!(["L999"]);
        let mut elevated = valid.clone();
        elevated["diagnostic_report"]["level"] = serde_json::json!("trigger_verified");
        let mut duplicate = valid;
        duplicate["diagnosticReport"] = duplicate["diagnostic_report"].clone();
        for value in [invalid_ref, elevated, duplicate] {
            let mut buffer = AnswerBuffer::new(true);
            assert_eq!(buffer.push(&value.to_string()), None);
            let rendered = buffer.finish(Some(&original()), evidence, ".", "result");
            assert_eq!(rendered, unverified(Some(&original()), evidence));
            assert!(!rendered.contains("已观察业务警告"));
        }
    }

    #[test]
    fn diagnostic_protocol_keys_and_aliases_are_detected_without_escape_bypass() {
        for raw in [
            r#"前文 {"diagnostic_report": {}}"#,
            r#"前文 {"diagnosticReport": {}}"#,
            r#"前文 {"\u0064iagnostic_report": {}}"#,
            r#"前文 {"\u0064iagnosticReport": {}}"#,
            r#"前文 {"diagnostic_rep"#,
        ] {
            assert!(protocol_candidate(raw), "{raw}");
            let mut buffer = AnswerBuffer::new(true);
            for ch in raw.chars() {
                assert_eq!(buffer.push(&ch.to_string()), None);
            }
            assert_eq!(
                buffer.finish(Some(&original()), "", ".", "result"),
                unverified(Some(&original()), "")
            );
        }
        // Ordinary unescaped JSON is not mistaken for the internal report protocol,
        // but protection still replaces it with a structured fallback at finish.
        let ordinary = r#"说明 {"ok": true}"#;
        assert!(!protocol_candidate(ordinary));
        let mut buffer = AnswerBuffer::new(true);
        assert_eq!(buffer.push(ordinary), None);
        assert_eq!(
            buffer.finish(None, "", ".", "unknown"),
            unverified(None, "")
        );
    }
}
