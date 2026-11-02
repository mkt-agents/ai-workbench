//! 逆向文档检索：在 `doc_dir/` 下按关键词搜 md/txt/docx/pdf，供联动问答预取。
//!
//! 纯本地、只读、封顶命中；调用方把结果注入 prompt，模型不再从零 `grep`。

use std::fs;
use std::path::{Path, PathBuf};

use crate::agent_docs;

/// 单次最多返回几篇命中文档。
const HIT_CAP: usize = 6;
/// 单篇摘录字符上限。
const EXCERPT_CHARS: usize = 2_400;
/// 全结果字符上限（注入 prompt 用）。
const TOTAL_CHARS: usize = 12_000;
/// 遍历文件上限，避免超大仓库卡死。
const FILE_CAP: usize = 800;

#[derive(Debug, Clone)]
pub struct DocHit {
    pub path: String,
    pub score: i32,
    pub excerpt: String,
}

#[derive(Debug, Clone, Default)]
pub struct DocSearchResult {
    pub hits: Vec<DocHit>,
    pub scanned: usize,
    pub partial: bool,
}

impl DocSearchResult {
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    /// 注入 prompt 的人类可读块；空则返回空串。
    pub fn format_block(&self) -> String {
        if self.hits.is_empty() {
            return String::new();
        }
        let mut out = format!(
            "【文档检索命中 {} 篇{}】\n",
            self.hits.len(),
            if self.partial {
                "（检索未穷尽）"
            } else {
                ""
            }
        );
        for (i, hit) in self.hits.iter().enumerate() {
            let piece = format!(
                "\n### 文档 {} · `{}`（相关度 {}）\n{}\n",
                i + 1,
                hit.path,
                hit.score,
                hit.excerpt.trim()
            );
            if out.chars().count() + piece.chars().count() > TOTAL_CHARS {
                out.push_str("\n…（后续文档摘录已截断）\n");
                break;
            }
            out.push_str(&piece);
        }
        out
    }
}

/// 在 `workspace/doc_dir` 下按关键词检索文档。
/// `keywords` 为空时用 `ask` 切出的词；两者都空则返回空结果。
pub fn search(workspace: &str, doc_dir: &str, keywords: &[String], ask: &str) -> DocSearchResult {
    let terms = collect_terms(keywords, ask);
    if terms.is_empty() {
        return DocSearchResult::default();
    }
    let Some(root) = resolve_doc_root(workspace, doc_dir) else {
        return DocSearchResult::default();
    };
    let (files, partial_walk) = list_doc_files(&root);
    let mut scored: Vec<DocHit> = Vec::new();
    let mut scanned = 0usize;
    let mut partial = partial_walk;
    for path in files {
        scanned += 1;
        let rel = path
            .strip_prefix(&root)
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or(""))
            .replace('\\', "/");
        let text = read_doc_text(&path);
        if text.trim().is_empty() {
            continue;
        }
        let score = score_doc(&rel, &text, &terms);
        if score <= 0 {
            continue;
        }
        let excerpt = pick_excerpt(&text, &terms, EXCERPT_CHARS);
        let dir = doc_dir.trim().trim_end_matches('/');
        let dir = if dir.is_empty() { "doc" } else { dir };
        scored.push(DocHit {
            path: format!("{dir}/{rel}").replace("//", "/"),
            score,
            excerpt,
        });
        if scored.len() > HIT_CAP * 3 {
            scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
            scored.truncate(HIT_CAP * 2);
            partial = true;
        }
    }
    scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    scored.truncate(HIT_CAP);
    DocSearchResult {
        hits: scored,
        scanned,
        partial,
    }
}

fn collect_terms(keywords: &[String], ask: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let push = |raw: &str, terms: &mut Vec<String>| {
        let t = raw.trim();
        if t.chars().count() < 2 {
            return;
        }
        if terms.iter().any(|x| x.eq_ignore_ascii_case(t)) {
            return;
        }
        terms.push(t.to_string());
    };
    for k in keywords {
        push(k, &mut terms);
    }
    for phrase in crate::wecom_intent::extract_business_phrases(ask) {
        push(&phrase, &mut terms);
    }
    for token in tokenize_ask(ask) {
        push(&token, &mut terms);
    }
    terms.truncate(8);
    terms
}

fn tokenize_ask(ask: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(re) = regex::Regex::new(r"[A-Za-z][A-Za-z0-9_\-]{2,}") {
        for m in re.find_iter(ask) {
            let s = m.as_str();
            if s.len() >= 3 {
                out.push(s.to_string());
            }
        }
    }
    let mut buf = String::new();
    for ch in ask.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            buf.push(ch);
        } else if !buf.is_empty() {
            if buf.chars().count() >= 2 {
                out.push(std::mem::take(&mut buf));
            } else {
                buf.clear();
            }
        }
    }
    if buf.chars().count() >= 2 {
        out.push(buf);
    }
    const STOP: &[&str] = &[
        "怎么", "如何", "什么", "介绍", "说明", "一下", "这个", "那个", "我们", "你们", "可以",
        "需要", "帮我", "请问", "看看", "什么是", "是什么",
    ];
    out.retain(|t| !STOP.iter().any(|s| t == s));
    out
}

fn resolve_doc_root(workspace: &str, doc_dir: &str) -> Option<PathBuf> {
    let ws = workspace.trim();
    if ws.is_empty() {
        return None;
    }
    let dir = {
        let d = doc_dir.trim();
        if d.is_empty() {
            "doc"
        } else {
            d
        }
    };
    let root = Path::new(ws).join(dir);
    root.is_dir().then_some(root)
}

fn list_doc_files(root: &Path) -> (Vec<PathBuf>, bool) {
    let mut out = Vec::new();
    let mut partial = false;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            partial = true;
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                partial = true;
                continue;
            };
            if meta.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(
                    name.as_str(),
                    ".git" | "node_modules" | "target" | "build" | "dist" | ".venv"
                ) {
                    continue;
                }
                stack.push(path);
            } else if meta.is_file() {
                if out.len() >= FILE_CAP {
                    partial = true;
                    return (out, partial);
                }
                if is_doc_file(&path) {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    (out, partial)
}

fn is_doc_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "md" | "markdown" | "txt" | "rst" | "adoc" | "docx" | "pdf" | "html" | "htm"
    )
}

fn read_doc_text(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "docx" | "pdf" | "xlsx" | "xls" => {
            agent_docs::read_document(path, 80_000).unwrap_or_default()
        }
        _ => fs::read_to_string(path)
            .map(|s| {
                if s.chars().count() > 80_000 {
                    s.chars().take(80_000).collect()
                } else {
                    s
                }
            })
            .unwrap_or_default(),
    }
}

fn score_doc(rel_path: &str, text: &str, terms: &[String]) -> i32 {
    let path_l = rel_path.to_lowercase();
    let text_l = text.to_lowercase();
    let mut score = 0i32;
    for term in terms {
        let t = term.to_lowercase();
        if path_l.contains(&t) {
            score += 40;
        }
        let head: String = text.chars().take(240).collect::<String>().to_lowercase();
        if head.contains(&t) {
            score += 25;
        }
        let count = text_l.matches(&t).count().min(8) as i32;
        score += count * 6;
    }
    score
}

fn pick_excerpt(text: &str, terms: &[String], cap: usize) -> String {
    let lower = text.to_lowercase();
    let mut best_pos = 0usize;
    let mut best_score = -1i32;
    for term in terms {
        let t = term.to_lowercase();
        if let Some(pos) = lower.find(&t) {
            let nearby = lower[pos.saturating_sub(40)..]
                .chars()
                .take(120)
                .collect::<String>();
            let sc = 10
                + terms
                    .iter()
                    .filter(|x| nearby.contains(&x.to_lowercase()))
                    .count() as i32;
            if sc > best_score {
                best_score = sc;
                best_pos = pos;
            }
        }
    }
    let start = {
        let mut s = best_pos.saturating_sub(80);
        while s > 0 && !text.is_char_boundary(s) {
            s -= 1;
        }
        s
    };
    let slice: String = text[start..].chars().take(cap).collect();
    let mut out = slice.trim().to_string();
    if start > 0 {
        out.insert_str(0, "…");
    }
    if text[start..].chars().count() > cap {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tokenize_keeps_identifiers_and_chinese() {
        let t = tokenize_ask("登录模块 OrderService 怎么实现");
        assert!(t.iter().any(|x| x == "OrderService"));
        assert!(t.iter().any(|x| x.contains("登录") || x == "登录模块"));
    }

    #[test]
    fn search_scores_filename_and_body() {
        let dir = tempfile_dir();
        let doc = dir.join("doc");
        fs::create_dir_all(&doc).unwrap();
        let mut f = fs::File::create(doc.join("login.md")).unwrap();
        writeln!(f, "# 登录模块\n\nOrderService 负责校验验证码。").unwrap();
        let r = search(
            dir.to_str().unwrap(),
            "doc",
            &["登录".into()],
            "登录怎么实现",
        );
        assert!(!r.hits.is_empty(), "{r:?}");
        assert!(r.hits[0].path.contains("login.md"));
        assert!(r.format_block().contains("文档检索命中"));
    }

    fn tempfile_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "wecom-doc-search-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }
}
