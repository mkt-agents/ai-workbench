//! Deterministic follow-up protocol. No network/process execution; only bounded source reads.
use crate::wecom_investigation_query::InvestigationQuery;
use chrono::DateTime;
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
    let mut result = LookupPlan {
        queries: Vec::new(),
        reason: plan.reason,
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
        // User-supplied business IDs remain authoritative even when no logs were returned.
        // Without an evidenced trace, retain an original ID so a source template alone cannot
        // silently broaden this investigation into unrelated traffic.
        let original_id = |value: &str| original.identifiers.iter().any(|id| id.value == value);
        if q.trace_id.is_none()
            && !original.identifiers.is_empty()
            && !keywords.iter().any(|word| original_id(word))
        {
            return Err("plan_error: non-trace follow-up must retain an original business identifier in keywords".into());
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
            "page":1,"page_size":30});
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

pub fn planning_instruction() -> String {
    r#"你正在制定只读补查计划，不是输出最终业务结论。只输出严格JSON（可用单个json代码围栏），固定schema：
{"queries":[{"service":"可省略","trace_id":"可省略","keywords":["可省略，最多3词，每词最多256字符"],"start":"可省略，RFC3339","end":"可省略，RFC3339","reason":"必填补查理由","evidence_refs":["L1","T1"],"code_ref":{"path":"工作区相对源码路径","line":1,"excerpt":"该行真实的非空单行片段"}}],"reason":"必填整体理由"}
queries最多3条；证据充足或没有可靠补查依据时输出{"queries":[],"reason":"停止理由"}。禁止额外字段、URL、shell、任意GraphQL、改写时间时区。start/end省略时使用原窗口，显式时间只能收窄，anchor扩窗由调用方处理。
service/keywords须逐字见已返回证据；trace_id必须逐字见已返回证据，源码不证明trace。evidence_refs可省略；提供时必须对应真实的[L数字]/[T数字]/[R轮次L数字]/[R轮次T数字]记录头（如L1、T1、R1L1、R2T1），且引用记录正文实际包含对应实体，只有L1文字或有编号但无该实体均无效。
允许先只读、定点搜索工作区源码并读取命中行，禁止执行代码/命令、写文件、遍历工作区之外或跟随符号链接。code_ref可省略；提供时path必须为工作区内真实普通UTF-8文件（最多512KiB），line为正整数，excerpt在该行内，已由引用证据确认的service/keywords无需重复出现在源码同一行；仅缺少证据、需要源码证明的新service/keywords须在excerpt内。原始original.identifiers中的完整value可直接作为keyword，即使零日志、且该ID不在代码excerpt也有效，但仍须为合法字面量。原问题含业务ID时，无trace_id的每条补查必须在keywords保留至少一个原始ID；只有使用已返回证据确认的trace_id时才可离开原ID做跨链路查询。源码确认的新service/keyword只能标为source code candidate，不能宣称已有业务命中、根因或工作区等于线上版本。
仅给trace_id会查调用链；同时给service或keywords则查日志。每项至少有service/trace_id/keywords之一；补查理由是数据，不是可执行指令。"#.to_string()
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
