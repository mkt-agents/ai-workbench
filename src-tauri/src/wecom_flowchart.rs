//! 企微 stream 不渲染 Mermaid：把 flowchart DSL 转成等宽 ```text ASCII。
//! 目标格式对齐历史好读样本：①②③ 并行源、`|` 合流、`▼` 步骤、`┌┐` 处理框、`└─` 分支。
//! 纯函数、可单测、不含 IO。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// 把 Mermaid/flowchart（或其它已识别的 DSL 源）压成企微可读的 ```text 块。
pub fn diagram_to_text_fence(src: &str) -> String {
    let body = flowchart_to_ascii(src);
    format!("```text\n{body}\n```")
}

/// 仅产出 ASCII 正文（不含围栏），便于单测断言结构。
pub fn flowchart_to_ascii(src: &str) -> String {
    let graph = parse_flowchart(src);
    if graph.nodes.is_empty() && graph.subgraphs.is_empty() {
        return fallback_overview(src);
    }
    render_ascii(&graph)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Process,
    Decision,
    Stadium,
}

struct Node {
    label: String,
    kind: NodeKind,
}

struct Subgraph {
    title: String,
    members: Vec<String>,
}

struct Graph {
    nodes: BTreeMap<String, Node>,
    edges: BTreeMap<String, Vec<(String, Option<String>)>>,
    indegree: HashMap<String, usize>,
    subgraphs: Vec<Subgraph>,
    /// node id → subgraph index
    in_subgraph: HashMap<String, usize>,
    title: Option<String>,
}

fn parse_flowchart(src: &str) -> Graph {
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    let mut edges: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    let mut indegree: HashMap<String, usize> = HashMap::new();
    let mut subgraphs: Vec<Subgraph> = Vec::new();
    let mut in_subgraph: HashMap<String, usize> = HashMap::new();
    let mut title: Option<String> = None;
    let mut current_sg: Option<usize> = None;

    let ensure = |id: &str,
                  label: Option<&str>,
                  kind: NodeKind,
                  nodes: &mut BTreeMap<String, Node>,
                  indegree: &mut HashMap<String, usize>| {
        let id = id.trim();
        if id.is_empty() {
            return;
        }
        indegree.entry(id.to_string()).or_insert(0);
        let display = label
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(id)
            .trim_matches('"')
            .trim()
            .to_string();
        nodes
            .entry(id.to_string())
            .and_modify(|existing| {
                if existing.label == id && display != *id {
                    existing.label = display.clone();
                } else if existing.label.chars().count() < display.chars().count() {
                    existing.label = display.clone();
                }
                if kind != NodeKind::Process {
                    existing.kind = kind;
                }
            })
            .or_insert(Node {
                label: display,
                kind,
            });
    };

    for raw in src.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("%%")
            || trimmed.starts_with("classDef")
            || trimmed.starts_with("class ")
            || trimmed.starts_with("linkStyle")
            || trimmed.starts_with("style ")
            || trimmed.starts_with("click ")
            || trimmed.starts_with("@startuml")
            || trimmed.starts_with("@enduml")
            || trimmed.starts_with("digraph")
            || trimmed.starts_with("sequenceDiagram")
            || trimmed.starts_with("classDiagram")
        {
            continue;
        }
        if trimmed.starts_with("flowchart") || trimmed.starts_with("graph ") {
            // optional: graph TD title via comment already skipped
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("subgraph ") {
            let title_sg = parse_subgraph_title(rest);
            let idx = subgraphs.len();
            subgraphs.push(Subgraph {
                title: title_sg,
                members: Vec::new(),
            });
            current_sg = Some(idx);
            continue;
        }
        if trimmed == "end" {
            current_sg = None;
            continue;
        }
        // Mermaid direction / title directives inside subgraph
        if matches!(trimmed, "direction TB" | "direction TD" | "direction LR" | "direction BT") {
            continue;
        }

        for (id, label, kind) in extract_node_defs(trimmed) {
            ensure(&id, Some(&label), kind, &mut nodes, &mut indegree);
            if let Some(sg) = current_sg {
                if !subgraphs[sg].members.iter().any(|m| m == &id) {
                    subgraphs[sg].members.push(id.clone());
                }
                in_subgraph.insert(id, sg);
            }
        }

        for (from, to, edge_label, from_label, to_label, from_kind, to_kind) in
            extract_edges_rich(trimmed)
        {
            ensure(
                &from,
                from_label.as_deref(),
                from_kind,
                &mut nodes,
                &mut indegree,
            );
            ensure(&to, to_label.as_deref(), to_kind, &mut nodes, &mut indegree);
            edges
                .entry(from.clone())
                .or_default()
                .push((to.clone(), edge_label));
            *indegree.entry(to.clone()).or_insert(0) += 1;
            if let Some(sg) = current_sg {
                for id in [&from, &to] {
                    if !subgraphs[sg].members.iter().any(|m| m == id) {
                        subgraphs[sg].members.push(id.clone());
                    }
                    in_subgraph.entry(id.clone()).or_insert(sg);
                }
            }
        }

        // Heuristic title: first Chinese-heavy quoted / 【】 heading outside graph syntax
        if title.is_none() {
            if let Some(t) = extract_title_hint(trimmed) {
                title = Some(t);
            }
        }
    }

    Graph {
        nodes,
        edges,
        indegree,
        subgraphs,
        in_subgraph,
        title,
    }
}

fn parse_subgraph_title(rest: &str) -> String {
    let rest = rest.trim();
    // subgraph id [Title] / subgraph Title
    if let Some(open) = rest.find('[') {
        if let Some(close) = rest.rfind(']') {
            if close > open {
                let inner = rest[open + 1..close].trim().trim_matches('"').trim();
                if !inner.is_empty() {
                    return inner.to_string();
                }
            }
        }
    }
    rest.trim_matches('"').trim().to_string()
}

fn extract_title_hint(line: &str) -> Option<String> {
    if let Some(start) = line.find('【') {
        if let Some(end) = line[start..].find('】') {
            let s = &line[start..start + end + '】'.len_utf8()];
            if s.chars().count() >= 3 {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn extract_node_defs(line: &str) -> Vec<(String, String, NodeKind)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let id = &line[start..i];
            if let Some((kind, label, next)) = take_shape_at(line, i) {
                if diagram_label_useful(&label) {
                    out.push((id.to_string(), label, kind));
                }
                i = next;
                continue;
            }
            continue;
        }
        i += 1;
    }
    out
}

fn take_shape_at(line: &str, at: usize) -> Option<(NodeKind, String, usize)> {
    let rest = line.get(at..)?;
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    // Stadium: ([label]) or [[label]]
    if rest.starts_with("([") {
        let end = find_matching(rest, 1, '[', ']')?;
        if rest.as_bytes().get(end + 1) == Some(&b')') {
            let label = clean_label(&rest[2..end]);
            return Some((NodeKind::Stadium, label, at + end + 2));
        }
    }
    if rest.starts_with("[[") {
        let end = find_matching(rest, 1, '[', ']')?;
        if rest.as_bytes().get(end + 1) == Some(&b']') {
            let label = clean_label(&rest[2..end]);
            return Some((NodeKind::Process, label, at + end + 2));
        }
    }
    let open = bytes[0] as char;
    let (close, kind) = match open {
        '[' => (']', NodeKind::Process),
        '{' => ('}', NodeKind::Decision),
        '(' => (')', NodeKind::Stadium),
        _ => return None,
    };
    let end = find_matching(rest, 0, open, close)?;
    let label = clean_label(&rest[1..end]);
    Some((kind, label, at + end + close.len_utf8()))
}

fn clean_label(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .replace("<br/>", " ")
        .replace("<br>", " ")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_edges_rich(
    line: &str,
) -> Vec<(
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    NodeKind,
    NodeKind,
)> {
    let mut out = Vec::new();
    let mut rest = line;
    let mut prev: Option<(String, Option<String>, NodeKind)> = None;
    while !rest.is_empty() {
        let trimmed = rest.trim_start();
        rest = trimmed;
        if rest.is_empty() {
            break;
        }

        if let Some((consumed, edge_label)) = take_arrow(rest) {
            rest = &rest[consumed..];
            let (to, to_label, to_kind, after) = take_id_with_shape(rest);
            if let (Some((from, from_label, from_kind)), Some(to)) = (prev.take(), to) {
                out.push((
                    from,
                    to.clone(),
                    edge_label,
                    from_label,
                    to_label.clone(),
                    from_kind,
                    to_kind,
                ));
                prev = Some((to, to_label, to_kind));
                rest = after;
                continue;
            }
            break;
        }

        let (id, label, kind, after) = take_id_with_shape(rest);
        if let Some(id) = id {
            prev = Some((id, label, kind));
            rest = after;
            continue;
        }

        let ch = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        rest = &rest[ch..];
    }
    out
}

fn take_id_with_shape(s: &str) -> (Option<String>, Option<String>, NodeKind, &str) {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    if bytes.is_empty() || !(bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_') {
        return (None, None, NodeKind::Process, s);
    }
    let mut i = 1usize;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    let id = s[..i].to_string();
    let mut rest = &s[i..];
    let mut label = None;
    let mut kind = NodeKind::Process;
    if let Some((k, lab, next)) = take_shape_at(rest, 0) {
        kind = k;
        label = Some(lab);
        rest = &rest[next..];
    }
    (Some(id), label, kind, rest)
}

fn take_arrow(s: &str) -> Option<(usize, Option<String>)> {
    let s = s.trim_start();
    for prefix in ["-->|", "--|", "-.->|", "==>|"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let end = rest.find('|')?;
            let label = rest[..end].trim();
            let label = if label.is_empty() {
                None
            } else {
                Some(label.to_string())
            };
            return Some((prefix.len() + end + 1, label));
        }
    }
    for prefix in ["-->", "-.->", "==>", "---", "->>", "->"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let rest_trim = rest.trim_start();
            let pad = rest.len() - rest_trim.len();
            if rest_trim.starts_with('|') {
                let inner = &rest_trim[1..];
                let end = inner.find('|')?;
                let label = inner[..end].trim();
                let label = if label.is_empty() {
                    None
                } else {
                    Some(label.to_string())
                };
                return Some((prefix.len() + pad + 1 + end + 1, label));
            }
            return Some((prefix.len() + pad, None));
        }
    }
    None
}

const CIRCLED: &[&str] = &["①", "②", "③", "④", "⑤", "⑥", "⑦", "⑧", "⑨", "⑩"];

fn circled_mark(i: usize) -> String {
    CIRCLED
        .get(i)
        .copied()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}.", i + 1))
}

fn render_ascii(graph: &Graph) -> String {
    let roots: Vec<String> = graph
        .nodes
        .keys()
        .filter(|id| graph.indegree.get(*id).copied().unwrap_or(0) == 0)
        .filter(|id| !graph.in_subgraph.contains_key(*id))
        .cloned()
        .collect();
    let roots = if roots.is_empty() {
        graph
            .nodes
            .keys()
            .filter(|id| graph.indegree.get(*id).copied().unwrap_or(0) == 0)
            .cloned()
            .collect()
    } else {
        roots
    };

    let mut lines: Vec<String> = Vec::new();
    // 仅源里带【】标题时才输出；不写死「【流程】」以免线性题被带偏。
    if let Some(heading) = graph.title.as_ref() {
        if heading.starts_with('【') {
            lines.push(format!("**{heading}**"));
        } else {
            lines.push(format!("**【{heading}】**"));
        }
    }

    let mut emitted = HashSet::new();
    let mut rendered_sg = HashSet::new();
    let mut step = 0usize;

    if roots.len() >= 2 {
        for (i, root) in roots.iter().enumerate() {
            lines.push(format!("{} {}", circled_mark(i), label_of(graph, root)));
            if i + 1 < roots.len() {
                lines.push("|".into());
            }
            emitted.insert(root.clone());
        }
        lines.push("|".into());
        let mut next: VecDeque<String> = VecDeque::new();
        for root in &roots {
            push_outs(graph, root, &mut next);
        }
        // 合流后用 ▼，避免与源编号冲突
        walk_chain(
            graph,
            &mut next,
            &mut emitted,
            &mut rendered_sg,
            &mut lines,
            &mut step,
            false,
        );
    } else {
        let start = roots
            .first()
            .cloned()
            .or_else(|| graph.nodes.keys().next().cloned())
            .unwrap_or_default();
        if !start.is_empty() {
            if looks_like_factory(graph, &start) {
                emitted.insert(start.clone());
                let child_ids = emit_factory_box(graph, &start, &mut emitted, &mut lines);
                let mut next = VecDeque::new();
                for child in child_ids {
                    push_outs(graph, &child, &mut next);
                }
                walk_chain(
                    graph,
                    &mut next,
                    &mut emitted,
                    &mut rendered_sg,
                    &mut lines,
                    &mut step,
                    true,
                );
            } else {
                // 图1：单根线性 → ①②③… + |
                let mut next = VecDeque::from([start]);
                walk_chain(
                    graph,
                    &mut next,
                    &mut emitted,
                    &mut rendered_sg,
                    &mut lines,
                    &mut step,
                    true,
                );
            }
        }
    }

    for (idx, sg) in graph.subgraphs.iter().enumerate() {
        if rendered_sg.contains(&idx) {
            continue;
        }
        emit_subgraph_box(graph, idx, sg, &mut emitted, &mut lines);
        rendered_sg.insert(idx);
    }

    if lines.is_empty() {
        return fallback_overview_from_labels(graph);
    }
    lines.join("\n")
}

fn walk_chain(
    graph: &Graph,
    next: &mut VecDeque<String>,
    emitted: &mut HashSet<String>,
    rendered_sg: &mut HashSet<usize>,
    lines: &mut Vec<String>,
    step: &mut usize,
    numbered: bool,
) {
    while let Some(id) = next.pop_front() {
        if emitted.contains(&id) {
            continue;
        }
        if let Some(&sg_idx) = graph.in_subgraph.get(&id) {
            if !rendered_sg.contains(&sg_idx) {
                if !lines.is_empty() && !lines.last().map(|l| l == "|").unwrap_or(false) {
                    lines.push("|".into());
                }
                emit_subgraph_box(
                    graph,
                    sg_idx,
                    &graph.subgraphs[sg_idx],
                    emitted,
                    lines,
                );
                rendered_sg.insert(sg_idx);
                for m in &graph.subgraphs[sg_idx].members {
                    if let Some(outs) = graph.edges.get(m) {
                        for (to, _) in outs {
                            if !graph
                                .in_subgraph
                                .get(to)
                                .map(|i| *i == sg_idx)
                                .unwrap_or(false)
                            {
                                next.push_back(to.clone());
                            }
                        }
                    }
                }
            }
            continue;
        }
        if looks_like_factory(graph, &id) && emitted.insert(id.clone()) {
            if !lines.is_empty() && !lines.last().map(|l| l == "|").unwrap_or(false) {
                lines.push("|".into());
            }
            let child_ids = emit_factory_box(graph, &id, emitted, lines);
            for child in child_ids {
                if let Some(outs) = graph.edges.get(&child) {
                    for (to, _) in outs {
                        if to != &id && !emitted.contains(to) {
                            next.push_back(to.clone());
                        }
                    }
                }
            }
            continue;
        }
        emit_step(graph, &id, numbered, step, emitted, lines);
        enqueue_outs(graph, &id, next, lines, emitted);
    }
}

/// 单后继静默入队；多后继先画 L- 分支再入队。
fn enqueue_outs(
    graph: &Graph,
    id: &str,
    next: &mut VecDeque<String>,
    lines: &mut Vec<String>,
    emitted: &mut HashSet<String>,
) {
    let Some(outs) = graph.edges.get(id) else {
        return;
    };
    if outs.len() <= 1 {
        for (to, _) in outs {
            next.push_back(to.clone());
        }
        return;
    }
    for (to, edge) in outs {
        let tip = match edge.as_deref() {
            Some(e) if !e.is_empty() => format!("|   L-{e} → {}", label_of(graph, to)),
            _ => format!("|   L- → {}", label_of(graph, to)),
        };
        lines.push(tip);
        let has_more = graph
            .edges
            .get(to)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if has_more {
            next.push_back(to.clone());
        } else {
            emitted.insert(to.clone());
        }
    }
}

fn emit_step(
    graph: &Graph,
    id: &str,
    numbered: bool,
    step: &mut usize,
    emitted: &mut HashSet<String>,
    lines: &mut Vec<String>,
) {
    if !emitted.insert(id.to_string()) {
        return;
    }
    if !lines.is_empty() && !lines.last().map(|l| l.starts_with('|')).unwrap_or(false) {
        lines.push("|".into());
    }
    let label = label_of(graph, id);
    let body = if graph.nodes.get(id).map(|n| n.kind) == Some(NodeKind::Decision) {
        format!("◇ {label}")
    } else {
        label
    };
    if numbered {
        lines.push(format!("{} {body}", circled_mark(*step)));
        *step += 1;
    } else {
        lines.push(format!("▼ {body}"));
    }
}

fn looks_like_factory(graph: &Graph, id: &str) -> bool {
    let label = label_of(graph, id);
    let outs = graph.edges.get(id).map(|v| v.len()).unwrap_or(0);
    let ins = graph.indegree.get(id).copied().unwrap_or(0);
    let name_hit = ["处理厂", "helper", "Helper", "引擎", "流水线", "工厂"]
        .iter()
        .any(|k| label.contains(k));
    // High fan-in hub with several linear children encoded as outs — rare; prefer name.
    name_hit || (ins >= 2 && outs >= 3)
}

fn emit_factory_box(
    graph: &Graph,
    id: &str,
    emitted: &mut HashSet<String>,
    lines: &mut Vec<String>,
) -> Vec<String> {
    let title = label_of(graph, id);
    let width = title.chars().count().clamp(8, 24);
    let bar = "─".repeat(width);
    lines.push(format!("┌─ {title} ─┐"));
    let mut children = Vec::new();
    if let Some(outs) = graph.edges.get(id) {
        for (i, (to, _)) in outs.iter().enumerate() {
            if emitted.insert(to.clone()) {
                lines.push(format!("│ {}. {}", i + 1, label_of(graph, to)));
                children.push(to.clone());
            }
        }
    }
    lines.push(format!("└{bar}┘"));
    children
}

fn emit_subgraph_box(
    graph: &Graph,
    _idx: usize,
    sg: &Subgraph,
    emitted: &mut HashSet<String>,
    lines: &mut Vec<String>,
) {
    let title = if sg.title.is_empty() {
        "处理单元".into()
    } else {
        sg.title.clone()
    };
    let width = title.chars().count().clamp(8, 24);
    let bar = "─".repeat(width);
    lines.push(format!("┌─ {title} ─┐"));
    let mut step = 1usize;
    for m in &sg.members {
        emitted.insert(m.clone());
        // Prefer members that look like steps (not only hubs): list all members
        lines.push(format!("│ {step}. {}", label_of(graph, m)));
        step += 1;
    }
    lines.push(format!("└{bar}┘"));
}

fn push_outs(graph: &Graph, id: &str, next: &mut VecDeque<String>) {
    if let Some(outs) = graph.edges.get(id) {
        for (to, _) in outs {
            next.push_back(to.clone());
        }
    }
}

fn label_of(graph: &Graph, id: &str) -> String {
    graph
        .nodes
        .get(id)
        .map(|n| n.label.clone())
        .unwrap_or_else(|| id.to_string())
}

fn fallback_overview(src: &str) -> String {
    let mut steps: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("flowchart")
            || trimmed.starts_with("graph ")
            || trimmed.starts_with("%%")
            || trimmed.starts_with("subgraph")
            || trimmed == "end"
        {
            continue;
        }
        for (_id, label, _) in extract_node_defs(trimmed) {
            if seen.insert(label.clone()) {
                steps.push(label);
            }
        }
    }
    if steps.is_empty() {
        let brief: String = src.chars().take(200).collect();
        return format!("① {brief}");
    }
    let mut out = String::new();
    for (i, s) in steps.iter().enumerate() {
        if i > 0 {
            out.push_str("|\n");
        }
        out.push_str(&format!("{} {s}\n", circled_mark(i)));
    }
    out.trim_end().to_string()
}

fn fallback_overview_from_labels(graph: &Graph) -> String {
    let mut out = String::new();
    for (i, node) in graph.nodes.values().enumerate() {
        if i > 0 {
            out.push_str("|\n");
        }
        out.push_str(&format!("{} {}\n", circled_mark(i), node.label));
    }
    out.trim_end().to_string()
}

fn diagram_label_useful(label: &str) -> bool {
    if label.is_empty() || label.starts_with('|') {
        return false;
    }
    label.chars().count() > 2 || label.chars().any(|c| !c.is_ascii_alphanumeric())
}

fn find_matching(s: &str, open_idx: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0i32;
    for (idx, ch) in s[open_idx..].char_indices() {
        let abs = open_idx + idx;
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(abs);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_chain_uses_circled_numbers_and_pipes() {
        let src = r#"flowchart TD
  A[客户发消息]
  B[渠道接收]
  C[AI 识别]
  A --> B --> C
"#;
        let out = flowchart_to_ascii(src);
        assert!(out.contains("① 客户发消息"), "{out}");
        assert!(out.contains("|"), "{out}");
        assert!(out.contains("② 渠道接收"), "{out}");
        assert!(out.contains("③ AI 识别"), "{out}");
        assert!(!out.contains("┌─"), "{out}");
        let fenced = diagram_to_text_fence(src);
        assert!(fenced.contains("```text"), "{fenced}");
    }

    #[test]
    fn branches_render_with_l_prefix() {
        let src = r#"flowchart TD
  A[拦截闸门]
  X[消息丢弃]
  D[AI 识别拆词]
  A -->|命中黑名单| X
  A -->|通过| D
"#;
        let out = flowchart_to_ascii(src);
        assert!(out.contains("① 拦截闸门") || out.contains("拦截闸门"), "{out}");
        assert!(out.contains("L-命中黑名单"), "{out}");
        assert!(out.contains("消息丢弃"), "{out}");
        assert!(!out.contains("┌─"), "{out}");
    }

    #[test]
    fn linear_quote_pipeline_fig1_style() {
        let src = r#"flowchart TD
  A[客户发消息]
  B[拦截闸门]
  C[AI 识别]
  D[生成商机]
  E{能否自动报价}
  F[发给客户]
  G[手工报价]
  A --> B --> C --> D --> E
  E -->|能| F
  E -->|不能| G
"#;
        let out = flowchart_to_ascii(src);
        assert!(out.contains("①"), "{out}");
        assert!(out.contains("②"), "{out}");
        assert!(out.contains("L-能") || out.contains("L-不能"), "{out}");
        assert!(!out.contains("┌─"), "线性链不应出处理框: {out}");
        assert!(!out.contains("**【流程】**"), "{out}");
    }

    #[test]
    fn three_roots_converge_with_circled_and_pipes() {
        let src = r#"flowchart TD
  A[Web全网找货]
  B[运营商找货群]
  C[个微渠道]
  D[txmsg 接消息]
  E[投递 MQ]
  A --> D
  B --> D
  C --> D
  D --> E
"#;
        let out = flowchart_to_ascii(src);
        assert!(out.contains("①"), "{out}");
        assert!(out.contains("②"), "{out}");
        assert!(out.contains("③"), "{out}");
        assert!(out.matches('|').count() >= 3, "{out}");
        assert!(out.contains("▼ txmsg"), "{out}");
        assert!(out.contains("▼ 投递 MQ"), "{out}");
    }

    #[test]
    fn subgraph_renders_as_box() {
        let src = r#"flowchart TD
  A[投递 MQ]
  subgraph MH [matchhelper 全网商机处理厂]
    B[去重]
    C[按行业拆词匹配]
    D[落库]
  end
  E[卖家报价]
  A --> B
  D --> E
"#;
        let out = flowchart_to_ascii(src);
        assert!(out.contains("┌─ matchhelper"), "{out}");
        assert!(out.contains("│ 1. 去重") || out.contains("│ 1."), "{out}");
        assert!(out.contains("└"), "{out}");
        assert!(out.contains("卖家报价"), "{out}");
    }

    #[test]
    fn decision_node_marked() {
        let src = r#"flowchart TD
  A[开始]
  B{是否命中}
  C[继续]
  A --> B --> C
"#;
        let out = flowchart_to_ascii(src);
        assert!(out.contains("◇") || out.contains("是否命中"), "{out}");
    }

    #[test]
    fn edge_inline_labels_are_kept() {
        let src = r#"flowchart TD
  A[上游] --> B[下游]
"#;
        let out = flowchart_to_ascii(src);
        assert!(out.contains("上游"), "{out}");
        assert!(out.contains("下游"), "{out}");
    }
}
