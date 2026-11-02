//! 文档 / 代码 / 日志联动查询编排。
//!
//! 按意图计划决定预取哪些来源，把结果合成一块可注入 prompt 的上下文。
//! 日志采集仍由 `wecom_investigation` 负责；这里只负责文档检索、代码线索与统一排版。

use crate::wecom_doc_search::{self, DocSearchResult};
use crate::wecom_intent::{self, QaAnalysis, QaIntent};
use serde::Serialize;
use std::sync::{Mutex, OnceLock};

/// 需要预取的证据来源（有序 = 注入时的展示顺序建议）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QaSource {
    Docs,
    Code,
    Logs,
}

/// 一条问答的执行计划：意图 + 要拉的来源。
#[derive(Debug, Clone)]
pub struct QaPlan {
    pub analysis: QaAnalysis,
    pub sources: Vec<QaSource>,
    /// 强制全联动（回答模式 `linked`）时为 true。
    pub linked: bool,
}

/// 联动预取结果（日志正文由调用方填入，避免本模块依赖 SkyWalking）。
#[derive(Debug, Clone, Default)]
pub struct CrossQueryBundle {
    pub docs: DocSearchResult,
    pub code_hints: String,
    pub log_context: String,
    pub log_evidence_count: usize,
    pub intent: Option<QaIntent>,
    pub linked: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CrossQuerySnapshot {
    pub request_id: String,
    pub engine: String,
    pub doc_hits: usize,
    pub code_hits: usize,
    pub log_hits: usize,
    pub summary: String,
    pub linked: bool,
}

fn last_slot() -> &'static Mutex<Option<CrossQuerySnapshot>> {
    static SLOT: OnceLock<Mutex<Option<CrossQuerySnapshot>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Remember the latest cross-query counts for the settings status bar.
pub fn note_latest(snap: CrossQuerySnapshot) {
    if let Ok(mut g) = last_slot().lock() {
        *g = Some(snap);
    }
}

pub fn last_diagnostic() -> Option<serde_json::Value> {
    last_slot()
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .and_then(|s| serde_json::to_value(s).ok())
}

impl CrossQueryBundle {
    pub fn doc_hits(&self) -> usize {
        self.docs.hits.len()
    }
    pub fn code_hits(&self) -> usize {
        if self.code_hints.trim().is_empty() {
            0
        } else {
            self.code_hints
                .lines()
                .filter(|l| l.contains("候选路径") || l.contains("栈坐标") || l.contains("关键词"))
                .count()
                .max(1)
        }
    }
    pub fn log_hits(&self) -> usize {
        self.log_evidence_count
    }

    pub fn to_snapshot(&self, request_id: &str, engine: &str) -> CrossQuerySnapshot {
        CrossQuerySnapshot {
            request_id: request_id.to_string(),
            engine: engine.to_string(),
            doc_hits: self.doc_hits(),
            code_hits: if self.code_hints.trim().is_empty() {
                0
            } else {
                self.code_hits()
            },
            log_hits: self.log_hits(),
            summary: self.source_summary_line(),
            linked: self.linked,
        }
    }

    /// 注入 system / user 上下文的完整块。
    pub fn format_context(&self) -> String {
        let mut parts = Vec::new();
        let docs = self.docs.format_block();
        if !docs.is_empty() {
            parts.push(docs);
        }
        if !self.code_hints.trim().is_empty() {
            parts.push(format!("【代码线索】\n{}", self.code_hints.trim()));
        }
        if !self.log_context.trim().is_empty() {
            parts.push(format!("【日志 / 调用链证据】\n{}", self.log_context.trim()));
        }
        if parts.is_empty() {
            return String::new();
        }
        let header = match (self.linked, self.intent) {
            (true, _) => "【联动上下文：文档 + 代码 + 日志】",
            (_, Some(QaIntent::Troubleshoot)) => "【排障联动上下文】",
            (_, Some(QaIntent::ReadCode)) => "【代码联动上下文】",
            (_, Some(QaIntent::DocLookup)) => "【文档联动上下文】",
            _ => "【联动上下文】",
        };
        format!("{header}\n\n{}", parts.join("\n\n"))
    }

    /// 前端 / 诊断用的来源分布摘要。
    pub fn source_summary_line(&self) -> String {
        format!(
            "文档 {} 篇 · 代码 {} 处 · 日志 {} 条",
            self.doc_hits(),
            if self.code_hints.trim().is_empty() {
                0
            } else {
                self.code_hits()
            },
            self.log_hits()
        )
    }
}

/// 根据用户话 + 回答模式生成计划。
/// `answer_mode`: `auto` | `doc` | `doc_code` | `code` | `linked`
pub fn plan_for(user_text: &str, answer_mode: &str) -> QaPlan {
    let analysis = wecom_intent::analyze(user_text);
    let mode = answer_mode.trim().to_ascii_lowercase();
    let linked = mode == "linked";
    let sources = if linked {
        vec![QaSource::Docs, QaSource::Code, QaSource::Logs]
    } else {
        match mode.as_str() {
            "doc" => vec![QaSource::Docs],
            "code" => vec![QaSource::Code],
            "doc_code" => {
                if wecom_intent::mentions_code(user_text) || analysis.intent == QaIntent::ReadCode {
                    vec![QaSource::Docs, QaSource::Code]
                } else {
                    vec![QaSource::Docs]
                }
            }
            _ => match analysis.intent {
                QaIntent::Troubleshoot => vec![QaSource::Logs, QaSource::Code, QaSource::Docs],
                QaIntent::ReadCode => vec![QaSource::Code, QaSource::Docs],
                QaIntent::DocLookup => {
                    if wecom_intent::mentions_code(user_text) {
                        vec![QaSource::Docs, QaSource::Code]
                    } else {
                        vec![QaSource::Docs]
                    }
                }
            },
        }
    };
    QaPlan {
        analysis,
        sources,
        linked,
    }
}

/// 是否需要拉 SkyWalking / 日志。
pub fn wants_logs(plan: &QaPlan) -> bool {
    plan.sources.contains(&QaSource::Logs)
}

/// 是否需要搜文档。
pub fn wants_docs(plan: &QaPlan) -> bool {
    plan.sources.contains(&QaSource::Docs)
}

/// 是否需要代码线索。
pub fn wants_code(plan: &QaPlan) -> bool {
    plan.sources.contains(&QaSource::Code)
}

/// 预取文档（同步、可 spawn_blocking）。`ask` 为用户原话。
pub fn prefetch_docs(
    workspace: &str,
    doc_dir: &str,
    plan: &QaPlan,
    ask: &str,
) -> DocSearchResult {
    if !wants_docs(plan) {
        return DocSearchResult::default();
    }
    let mut r = wecom_doc_search::search(workspace, doc_dir, &plan.analysis.keywords, ask);
    if r.is_empty() {
        r = wecom_doc_search::search(workspace, doc_dir, &[], ask);
    }
    r
}

/// 从日志证据生成代码线索（复用 source_hints）。
pub fn prefetch_code_from_logs(evidence: &str, workspace: &str) -> String {
    if evidence.trim().is_empty() || workspace.trim().is_empty() {
        return String::new();
    }
    crate::wecom_source_hints::hints(evidence, workspace)
}

/// 无日志时：按关键词在工作区做轻量符号定位（只读、封顶）。
pub fn prefetch_code_by_keywords(workspace: &str, keywords: &[String], ask: &str) -> String {
    if workspace.trim().is_empty() {
        return String::new();
    }
    let Ok(root) = crate::agent_fs::repo_root(workspace) else {
        return String::new();
    };
    let scope = crate::agent_fs::Scope::new(Some(root), false);
    let mut terms: Vec<String> = keywords.to_vec();
    for phrase in crate::wecom_intent::extract_business_phrases(ask) {
        if !terms.iter().any(|x| x.eq_ignore_ascii_case(&phrase)) {
            terms.push(phrase);
        }
    }
    if let Ok(re) = regex::Regex::new(r"[A-Za-z][A-Za-z0-9_]{2,}") {
        for m in re.find_iter(ask) {
            let s = m.as_str();
            if s.len() >= 3 && !terms.iter().any(|x| x.eq_ignore_ascii_case(s)) {
                terms.push(s.to_string());
            }
        }
    }
    terms.truncate(5);
    if terms.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut hits = 0usize;
    for term in &terms {
        if hits >= 5 {
            break;
        }
        let args = serde_json::json!({ "query": term, "max": 3 });
        let Ok(found) = crate::agent_symbols::symbols(&scope, &args, "wecom-cross-query") else {
            continue;
        };
        if found.contains("没有找到") {
            continue;
        }
        hits += 1;
        out.push_str(&format!("\n### 关键词 `{term}`\n{found}\n"));
    }
    if out.is_empty() {
        String::new()
    } else {
        format!("按关键词定位的源码线索：{out}")
    }
}

/// 组装联动包：文档 +（可选）日志证据对应代码 +（可选）关键词代码。
pub fn build_bundle(
    plan: &QaPlan,
    workspace: &str,
    doc_dir: &str,
    ask: &str,
    log_context: &str,
    log_evidence_count: usize,
) -> CrossQueryBundle {
    let docs = if wants_docs(plan) {
        prefetch_docs(workspace, doc_dir, plan, ask)
    } else {
        DocSearchResult::default()
    };

    let mut code_hints = String::new();
    if wants_code(plan) {
        if !log_context.trim().is_empty() {
            code_hints = prefetch_code_from_logs(log_context, workspace);
        }
        if code_hints.trim().is_empty() {
            code_hints = prefetch_code_by_keywords(workspace, &plan.analysis.keywords, ask);
        }
    }

    let logs = if wants_logs(plan) {
        log_context.to_string()
    } else {
        String::new()
    };

    CrossQueryBundle {
        docs,
        code_hints,
        log_context: logs,
        log_evidence_count: if wants_logs(plan) {
            log_evidence_count
        } else {
            0
        },
        intent: Some(plan.analysis.intent),
        linked: plan.linked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn troubleshoot_plan_includes_logs_code_docs() {
        let p = plan_for("帮我排查线上 500 报错", "auto");
        assert_eq!(p.analysis.intent, QaIntent::Troubleshoot);
        assert!(wants_logs(&p));
        assert!(wants_code(&p));
        assert!(wants_docs(&p));
    }

    #[test]
    fn doc_plan_skips_logs() {
        let p = plan_for("登录模块怎么实现的", "auto");
        assert_eq!(p.analysis.intent, QaIntent::DocLookup);
        assert!(!wants_logs(&p));
        assert!(wants_docs(&p));
    }

    #[test]
    fn linked_mode_always_all_sources() {
        let p = plan_for("随便问问", "linked");
        assert!(p.linked);
        assert!(wants_logs(&p) && wants_code(&p) && wants_docs(&p));
    }

    #[test]
    fn format_context_lists_sections() {
        let mut b = CrossQueryBundle {
            intent: Some(QaIntent::Troubleshoot),
            linked: true,
            log_context: "ERROR boom".into(),
            log_evidence_count: 1,
            code_hints: "候选路径：Foo.java".into(),
            ..Default::default()
        };
        b.docs.hits.push(crate::wecom_doc_search::DocHit {
            path: "doc/a.md".into(),
            score: 10,
            excerpt: "登录".into(),
        });
        let text = b.format_context();
        assert!(text.contains("联动上下文"));
        assert!(text.contains("文档检索命中"));
        assert!(text.contains("代码线索"));
        assert!(text.contains("日志"));
        assert!(b.source_summary_line().contains("文档 1"));
        let snap = b.to_snapshot("req-1", "dsh");
        assert_eq!(snap.doc_hits, 1);
        note_latest(snap);
        assert!(last_diagnostic().is_some());
    }
}
