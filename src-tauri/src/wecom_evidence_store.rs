//! Per-investigation in-memory retained excerpts. No network, filesystem or process IO.
//! Input must be producer-framed context: record headers at column zero, body lines indented.
//! Legacy unindented input is best-effort; header-shaped text cannot authenticate its own origin.
use sha2::{Digest, Sha256};

const MAX_BYTES: usize = 256 * 1024;
const MAX_METADATA_BYTES: usize = 16 * 1024;
const NOTICE: &str = "证据存储：仅可展开已收 context 的 retained excerpt，非 OAP 原始全文；索引摘要不是可验证全文，引用不等于因果证明。\n";

#[derive(Debug)]
struct Record {
    id: String,
    aliases: Vec<String>,
    header: String,
    body: String,
    key: String,
    priority: bool,
}

#[derive(Debug, Default)]
pub struct EvidenceStore {
    records: Vec<Record>,
    metadata: Vec<String>,
    bytes: usize,
    metadata_bytes: usize,
    omitted: bool,
    next_log: usize,
    next_trace: usize,
}

fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

fn header(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('[')?;
    let (id, suffix) = rest.split_once("] ")?;
    let local = if let Some(round) = id.strip_prefix('R') {
        let pos = round.find(['L', 'T'])?;
        let n = &round[..pos];
        if n.is_empty() || n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        &round[pos..]
    } else {
        id
    };
    let n = local
        .strip_prefix('L')
        .or_else(|| local.strip_prefix('T'))?;
    if n.is_empty() || n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let required = if local.starts_with('L') {
        "service="
    } else {
        "trace="
    };
    if !suffix.split_whitespace().any(|t| {
        t.strip_prefix(required)
            .is_some_and(|value| !value.is_empty())
    }) {
        return None;
    }
    Some((id, suffix))
}

fn field<'a>(header: &'a str, name: &str) -> &'a str {
    header
        .split_whitespace()
        .find_map(|s| s.strip_prefix(name))
        .unwrap_or("")
}

fn record_key(header: &str, body: &str) -> String {
    // Source coordinates plus original digest distinguish equal messages in different records.
    // Compare retained bytes too: an expanded excerpt is new evidence, not silently discarded.
    let source: Vec<_> = [
        "service=",
        "instance=",
        "ts=",
        "trace=",
        "thread=",
        "logger=",
        "digest=",
        "matched_ids=",
    ]
    .iter()
    .map(|k| field(header, k))
    .collect();
    // Keep exact source coordinates in the equality key as well as a body digest; even a
    // digest collision cannot merge different sources (body bytes are compared at insertion).
    format!("{:?}:{:x}", source, Sha256::digest(body.as_bytes()))
}

fn rendered(record: &Record) -> String {
    let mut out = format!("[{}] {}\n", record.id, record.header);
    for line in record.body.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

impl EvidenceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns only newly retained record bodies and new metadata; duplicates return alias deltas.
    /// IDs are store-owned monotonic L/T numbers. Source aliases are descriptive, not lookup IDs.
    pub fn insert(&mut self, context: &str) -> String {
        let mut delta = String::new();
        let mut current: Option<(String, String, String)> = None;
        let mut prefix = String::new();
        for line in context.lines() {
            if let Some((id, h)) = header(line) {
                if let Some((id, h, body)) = current.take() {
                    self.insert_record(&id, &h, body.trim_end_matches('\n'), &mut delta);
                }
                current = Some((id.into(), h.into(), String::new()));
            } else if [
                "覆盖说明：",
                "明确证据缺口：",
                "证据正文达到",
                "证据正文已截断",
                "查询步骤：",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
            {
                prefix.push_str(line);
                prefix.push('\n');
            } else if let Some((_, _, body)) = current.as_mut() {
                // Producer escapes body lines with two spaces; unescape once, then re-frame on output.
                body.push_str(line.strip_prefix("  ").unwrap_or(line));
                body.push('\n');
            } else {
                prefix.push_str(line);
                prefix.push('\n');
            }
        }
        if let Some((id, h, body)) = current {
            self.insert_record(&id, &h, body.trim_end_matches('\n'), &mut delta);
        }
        // Keep bounds/coverage notes separately, never reclassify prefix labels as evidence.
        for line in prefix.lines().filter(|line| !line.trim().is_empty()) {
            if self.metadata.iter().any(|old| old == line) {
                continue;
            }
            let size = line.len() + 3;
            if self.metadata_bytes + size > MAX_METADATA_BYTES || self.bytes + size > MAX_BYTES {
                self.omitted = true;
                continue;
            }
            self.metadata.push(line.to_owned());
            self.metadata_bytes += size;
            self.bytes += size;
            delta.push_str(&format!("元数据：{line}\n"));
        }
        if delta.is_empty() {
            delta.push_str("无新增证据正文（重复批次已去重）。\n");
        }
        if self.omitted {
            delta.push_str("保留上限已到达，部分记录/元数据未入库；覆盖不完整，请保留 gaps。\n");
        }
        format!("{NOTICE}{delta}")
    }

    fn insert_record(&mut self, alias: &str, header: &str, body: &str, delta: &mut String) {
        let key = record_key(header, body);
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|r| r.key == key && r.body == body)
        {
            if !record.aliases.iter().any(|a| a == alias) {
                if self.bytes + alias.len() + 8 <= MAX_BYTES {
                    record.aliases.push(alias.into());
                    self.bytes += alias.len() + 8;
                    delta.push_str(&format!(
                        "重复记录：source {alias} → canonical [{}]（正文未重发）。\n",
                        record.id
                    ));
                } else {
                    self.omitted = true;
                }
            }
            return;
        }
        let is_log = alias.contains('L');
        let number = if is_log {
            self.next_log + 1
        } else {
            self.next_trace + 1
        };
        let id = format!("{}{number}", if is_log { 'L' } else { 'T' });
        // Conservative accounting includes framing and String/Vec bookkeeping, not just payloads.
        let cost = header.len()
            + body.len()
            + body.lines().count() * 3
            + alias.len()
            + id.len()
            + key.len()
            + 256;
        if self.bytes + cost > MAX_BYTES {
            self.omitted = true;
            return;
        }
        if is_log {
            self.next_log = number;
        } else {
            self.next_trace = number;
        }
        let record = Record {
            id,
            aliases: vec![alias.into()],
            header: header.into(),
            body: body.into(),
            key,
            priority: field(header, "level=").eq_ignore_ascii_case("WARN")
                || field(header, "level=").eq_ignore_ascii_case("ERROR")
                || crate::wecom_decision_evidence::decision_gap(body),
        };
        delta.push_str(&format!(
            "新证据 source {alias} → canonical [{}]\n",
            record.id
        ));
        delta.push_str(&rendered(&record));
        self.bytes += cost;
        self.records.push(record);
    }

    pub fn render_index(&self) -> String {
        let mut out = NOTICE.to_owned();
        for record in self.ordered() {
            let summary = clip(
                &record.body.split_whitespace().collect::<Vec<_>>().join(" "),
                120,
            );
            out.push_str(&format!(
                "- [{}] source_aliases={} {} retained_chars={}；摘要（非全文）={}\n",
                record.id,
                record.aliases.join(","),
                clip(&record.header, 240),
                record.body.chars().count(),
                summary
            ));
        }
        if self.omitted {
            out.push_str("保留上限256KiB：部分证据未入库，覆盖不完整。\n");
        }
        out
    }

    fn ordered(&self) -> Vec<&Record> {
        let mut records: Vec<_> = self.records.iter().collect();
        records.sort_by_key(|record| !record.priority);
        records
    }

    /// Split the budget so a large index/metadata prefix never crowds out warning excerpts.
    pub fn bounded_prompt(&self, maxchars: usize) -> String {
        let note =
            "\n提示预算截断：索引或excerpt未全部展示；仅可展开retained excerpt，保留覆盖缺口。\n";
        let usable = maxchars.saturating_sub(note.chars().count());
        let metadata = self
            .metadata
            .iter()
            .map(|line| format!("元数据：{line}\n"))
            .collect::<String>();
        let metadata_cap = (usable / 5).min(2000);
        let index = self.render_index();
        let index_cap = usable / 4;
        let mut omitted =
            metadata.chars().count() > metadata_cap || index.chars().count() > index_cap;
        let mut out = NOTICE.to_owned();
        out.push_str(&clip(&metadata, metadata_cap));
        out.push_str("\n证据索引（可请求展开）：\n");
        out.push_str(&clip(&index, index_cap));
        out.push_str("\n已保留原文（WARN/决策优先）：\n");
        for record in self.ordered() {
            let text = rendered(record);
            let room = usable.saturating_sub(out.chars().count());
            if text.chars().count() > room {
                omitted = true;
                // Partial excerpt explicitly marked below; never presented as complete coverage.
                out.push_str(&clip(&text, room));
                break;
            }
            out.push_str(&text);
        }
        if out.chars().count() > usable {
            out = clip(&out, usable);
            omitted = true;
        }
        if omitted {
            out.push_str(&clip(note, maxchars.saturating_sub(out.chars().count())));
        }
        out
    }

    pub fn expand(&self, ids: &[String], maxchars: usize) -> Result<String, String> {
        if ids.len() > 6 {
            return Err("evidence_error: at most six read_evidence_ids".into());
        }
        let mut selected = Vec::new();
        for id in ids {
            let record =
                self.records.iter().find(|r| r.id == *id).ok_or_else(|| {
                    format!("evidence_error: unknown retained evidence ID [{id}]")
                })?;
            if !selected.iter().any(|r: &&Record| r.id == record.id) {
                selected.push(record);
            }
        }
        let mut out = NOTICE.to_owned();
        for record in selected {
            out.push_str(&rendered(record));
        }
        if out.chars().count() > maxchars {
            let note = "\n展开预算截断；以上只含retained excerpt的一部分，仍有覆盖缺口。\n";
            out = clip(&out, maxchars.saturating_sub(note.chars().count()));
            out.push_str(&clip(note, maxchars.saturating_sub(out.chars().count())));
        }
        Ok(out)
    }

    /// Validator authority: complete retained excerpts, never the index's summaries.
    pub fn full_context(&self) -> String {
        let mut out = NOTICE.to_owned();
        for line in &self.metadata {
            out.push_str(&format!("元数据：{line}\n"));
        }
        for record in &self.records {
            out.push_str(&rendered(record));
        }
        if self.omitted {
            out.push_str("\n覆盖不完整：部分记录超过保留上限，未入库。\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_batch_keeps_refs_without_resending_bodies() {
        let mut store = EvidenceStore::new();
        let first = "查询时间：fixed\n[L8] service=orders ts=123 digest=sha256:abc\n  unique body\n[T2] trace=trace-a\n  span body";
        assert!(store.insert(first).contains("unique body"));
        let second = store.insert(&first.replace("[L8]", "[R1L8]").replace("[T2]", "[R1T2]"));
        assert!(!second.contains("unique body"));
        assert!(!second.contains("span body"));
        assert!(store.render_index().contains("R1L8"));
        assert!(store
            .expand(&["L1".into(), "T1".into()], 12000)
            .unwrap()
            .contains("unique body"));
        assert_eq!(store.full_context().matches("unique body").count(), 1);
    }

    #[test]
    fn body_cannot_forge_ids_and_reads_are_bounded() {
        let mut store = EvidenceStore::new();
        store.insert(
            "[L1] service=orders\n  actual body\n  [L999] service=evil\n  [T999] trace=evil",
        );
        assert!(store.expand(&["L999".into()], 1000).is_err());
        assert!(store.expand(&["T999".into()], 1000).is_err());
        assert!(store.expand(&vec!["L1".into(); 7], 1000).is_err());
        assert!(store.full_context().contains("  [L999]"));
        for limit in [0, 1, 30, 300] {
            assert!(store.bounded_prompt(limit).chars().count() <= limit);
            assert!(store.expand(&["L1".into()], limit).unwrap().chars().count() <= limit);
        }
    }

    #[test]
    fn bounded_prompt_prioritizes_warning_body_over_large_index() {
        let mut store = EvidenceStore::new();
        for i in 0..40 {
            store.insert(&format!(
                "[L{}] service=orders ts={i} level=INFO\n  ordinary {}",
                i + 1,
                "x".repeat(300)
            ));
        }
        store.insert("[R1L1] service=orders level=WARN\n  crucial actual warning body");
        let prompt = store.bounded_prompt(2000);
        assert!(prompt.contains("  crucial actual warning body"));
        assert!(prompt.chars().count() <= 2000);
        assert!(store
            .expand(&["L41".into()], 12000)
            .unwrap()
            .contains("crucial actual warning body"));
    }

    #[test]
    fn tail_coverage_notes_are_metadata_not_last_record_body() {
        let mut store = EvidenceStore::new();
        store.insert("[L1] service=orders\n  body\n证据正文达到32,000字符上限，部分日志未展示");
        assert!(store.full_context().contains("元数据：证据正文达到"));
        assert!(!store
            .expand(&["L1".into()], 1000)
            .unwrap()
            .contains("32,000"));
    }

    #[test]
    fn blank_trace_records_keep_distinct_business_bindings() {
        let mut store = EvidenceStore::new();
        store.insert(
            "[L1] matched_ids=ORDER-A service=orders trace= ts=1 digest=sha256:same\n  same text",
        );
        let delta = store.insert(
            "[R1L1] matched_ids=ORDER-B service=orders trace= ts=1 digest=sha256:same\n  same text",
        );
        assert!(delta.contains("same text"));
        assert!(store.expand(&["L1".into(), "L2".into()], 12000).is_ok());
        assert_eq!(store.full_context().matches("same text").count(), 2);
        let repeated = store.insert(
            "[R2L1] matched_ids=ORDER-A service=orders trace= ts=1 digest=sha256:same\n  same text",
        );
        assert!(!repeated.contains("same text"));
    }

    #[test]
    fn explicit_gap_metadata_does_not_replace_or_merge_into_body() {
        let mut store = EvidenceStore::new();
        store.insert("[L1] service=orders\n  preserved body\n  明确证据缺口：body literal\n明确证据缺口：real coverage note");
        let expanded = store.expand(&["L1".into()], 12000).unwrap();
        assert!(expanded.contains("preserved body"));
        assert!(expanded.contains("  明确证据缺口：body literal"));
        assert!(!expanded.contains("real coverage note"));
        assert!(store
            .full_context()
            .contains("元数据：明确证据缺口：real coverage note"));
    }

    #[test]
    fn same_text_different_sources_remains_distinct_and_ids_do_not_collide() {
        let mut store = EvidenceStore::new();
        store.insert("[L8] service=orders ts=1\n  repeated");
        store.insert("[R1L8] service=orders ts=2\n  repeated");
        assert!(store.expand(&["L1".into(), "L2".into()], 1000).is_ok());
        assert!(store.expand(&["R1L8".into()], 1000).is_err());
        assert_eq!(store.full_context().matches("repeated").count(), 2);
    }

    #[test]
    fn cap_never_evicts_cited_excerpts_and_reports_omissions() {
        let mut store = EvidenceStore::new();
        store.insert("覆盖说明：partial\n[L1] service=orders\n  keep me");
        let huge = format!("[R1L1] service=other\n  {}", "字".repeat(MAX_BYTES));
        assert!(store.insert(&huge).contains("上限"));
        assert!(store
            .expand(&["L1".into()], 1000)
            .unwrap()
            .contains("keep me"));
        assert!(store.full_context().contains("覆盖说明：partial"));
        assert!(store.full_context().len() <= MAX_BYTES);
    }
}
