//! 上下文管道：把「项目文档 / 记忆 / 技能索引 / @ 引用文件」拼成一段带来源标注的文本，
//! 再交给 `agent.rs` 追加到系统提示。
//!
//! 为什么单独一层：以前上下文只在前端 `useAskContext` 里按 git 视图采集，非代码任务
//! （写文档、看表格、做调研）完全没有可注入的项目知识；`@` 引用的文件也没有通道。
//! 现在每个 provider 只负责「我有什么」，拼接、预算与来源标注由这里统一做。
//!
//! 全部只读；`.ai-workbench/` 与 `@` 引用都必须落在工作区内（防目录穿越）。

use std::path::Path;

use serde::Serialize;

use crate::harness_config::{self, HarnessConfig};

/// 一整段上下文的字符上限（约 6k token）：再多就会挤掉工具结果与历史。
pub(crate) const BUNDLE_CAP: usize = 24_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextSource {
    pub id: String,
    pub label: String,
    /// 文件路径；技能索引这类没有单一文件时为空。
    pub path: String,
    pub scope: String,
    pub chars: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextBundle {
    /// 直接追加到系统提示的文本（空 = 没有可注入的东西）。
    pub text: String,
    pub sources: Vec<ContextSource>,
}

// `sources` 直接序列化给前端，不需要额外的摘要方法。

struct Section {
    id: &'static str,
    label: String,
    path: String,
    scope: String,
    body: String,
    truncated: bool,
}

/// 采集本轮可注入的上下文。`ask` 是这一问的原文，用来发现 `/skill:name`。
pub(crate) fn collect(config: &HarnessConfig, root: Option<&Path>, ask: &str) -> ContextBundle {
    let mut sections: Vec<Section> = Vec::new();
    push_project_doc(config, root, &mut sections);
    push_memory(config, &mut sections);
    push_skills(config, &mut sections);
    push_skill_bodies(config, ask, &mut sections);

    let mut bundle = ContextBundle::default();
    let mut used = 0usize;
    for section in sections {
        if used >= BUNDLE_CAP {
            break;
        }
        let room = BUNDLE_CAP - used;
        let mut body = section.body;
        let truncated = section.truncated || body.chars().count() > room;
        if truncated {
            body = body.chars().take(room).collect();
        }
        used += body.chars().count();
        bundle.text.push_str(&format!(
            "\n\n[{} · {}]\n{}\n",
            section.label,
            if section.path.is_empty() { section.scope.clone() } else { format!("{} · {}", section.scope, section.path) },
            body.trim()
        ));
        bundle.sources.push(ContextSource {
            id: section.id.to_string(),
            label: section.label,
            path: section.path,
            scope: section.scope,
            chars: body.chars().count(),
            truncated,
        });
    }
    if !bundle.text.is_empty() {
        bundle.text = format!(
            "\n\n以下是本机自动带上的项目资料（只读，可能过时；与用户当前要求冲突时以用户为准）：{}",
            bundle.text
        );
    }
    bundle
}

fn push_project_doc(config: &HarnessConfig, root: Option<&Path>, out: &mut Vec<Section>) {
    if let Some(context) = &config.context {
        out.push(Section {
            id: "projectDoc",
            label: "项目上下文".into(),
            path: context.path.clone(),
            scope: context.scope.clone(),
            body: context.text.clone(),
            truncated: context.truncated,
        });
        return;
    }
    // 没写 `.ai-workbench/context.md` 就找仓库里现成的 AGENTS.md / CLAUDE.md
    let Some(root) = root else { return };
    let Some(path) = harness_config::fallback_context_files(root) else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else { return };
    if raw.trim().is_empty() {
        return;
    }
    let truncated = raw.chars().count() > harness_config::CONTEXT_CAP;
    let body: String = raw.chars().take(harness_config::CONTEXT_CAP).collect();
    out.push(Section {
        id: "projectDoc",
        label: "项目上下文".into(),
        path: path.to_string_lossy().into_owned(),
        scope: "project".into(),
        body,
        truncated,
    });
}

fn push_memory(config: &HarnessConfig, out: &mut Vec<Section>) {
    let Some(memory) = &config.memory else { return };
    out.push(Section {
        id: "memory",
        label: "长期记忆（可能过时）".into(),
        path: memory.path.clone(),
        scope: memory.scope.clone(),
        body: memory.text.clone(),
        truncated: memory.truncated,
    });
}

fn push_skills(config: &HarnessConfig, out: &mut Vec<Section>) {
    if config.skills.is_empty() {
        return;
    }
    // 只放索引，不注入正文：正文在用户 `/skill:name` 命中时才作为一条消息加入。
    let body = config
        .skills
        .iter()
        .map(|skill| {
            let hint = if skill.when_to_use.is_empty() { &skill.description } else { &skill.when_to_use };
            format!("- /skill:{} — {}", skill.name, hint)
        })
        .collect::<Vec<_>>()
        .join("\n");
    out.push(Section {
        id: "skills",
        label: "可用技能（用户提到相关任务时可以建议它）".into(),
        path: String::new(),
        scope: format!("{} 个", config.skills.len()),
        body,
        truncated: false,
    });
}

/// `@` 之外的第二种显式引用：用户输入里的 `/skill:name`。
/// 只注入被点名的技能正文（索引在 `push_skills` 里已经给过），避免把全部技能塞进提示。
fn push_skill_bodies(config: &HarnessConfig, ask: &str, out: &mut Vec<Section>) {
    for name in skill_names_in(ask) {
        let Some(skill) = config.skill(&name) else { continue };
        if skill.body.trim().is_empty() {
            continue;
        }
        out.push(Section {
            id: "skill",
            label: format!("技能 /skill:{}", skill.name),
            path: skill.path.clone(),
            scope: skill.scope.clone(),
            body: skill.body.clone(),
            truncated: false,
        });
    }
}

/// 找出文本里所有 `/skill:名字`（名字允许字母数字、`-`、`_` 与 `/`）。
pub(crate) fn skill_names_in(ask: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = ask;
    while let Some(pos) = rest.find("/skill:") {
        let after = &rest[pos + "/skill:".len()..];
        let end = after
            .char_indices()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '/'))
            .map(|(idx, _)| idx)
            .unwrap_or(after.len());
        if end > 0 {
            out.push(after[..end].to_string());
        }
        rest = &after[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness_config::HarnessConfig;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aw-ctx-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fallback_context_file_is_used_when_no_harness_dir() {
        let root = temp_dir("agents");
        std::fs::write(root.join("AGENTS.md"), "项目规矩").unwrap();
        let bundle = collect(&HarnessConfig::default(), Some(&root), "");
        assert_eq!(bundle.sources.len(), 1);
        assert_eq!(bundle.sources[0].id, "projectDoc");
        assert!(bundle.text.contains("项目规矩"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skill_mentions_are_parsed() {
        assert_eq!(skill_names_in("用 /skill:review 看下这段"), vec!["review"]);
        assert_eq!(
            skill_names_in("/skill:frontend/review 和 /skill:test-x"),
            vec!["frontend/review", "test-x"]
        );
        assert!(skill_names_in("没有命令").is_empty());
        assert!(skill_names_in("/skill:").is_empty());
    }

    #[test]
    fn named_skill_body_is_injected() {
        let config = HarnessConfig {
            skills: vec![harness_config::SkillEntry {
                name: "review".into(),
                description: "代码审查".into(),
                when_to_use: String::new(),
                allowed_tools: Vec::new(),
                mode: String::new(),
                path: "x".into(),
                scope: "project".into(),
                body: "按这个清单审查".into(),
            }],
            ..HarnessConfig::default()
        };
        let bundle = collect(&config, None, "请 /skill:review 一下");
        assert!(bundle.sources.iter().any(|s| s.id == "skill"));
        assert!(bundle.text.contains("按这个清单审查"));
    }

    #[test]
    fn bundle_is_capped() {
        let config = HarnessConfig {
            context: Some(harness_config::LoadedText {
                path: "x".into(),
                scope: "project".into(),
                text: "字".repeat(BUNDLE_CAP + 100),
                truncated: false,
            }),
            ..HarnessConfig::default()
        };
        let bundle = collect(&config, None, "");
        assert!(bundle.sources[0].truncated);
        assert!(bundle.text.chars().count() < BUNDLE_CAP + 200);
    }
}
