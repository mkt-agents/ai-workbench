//! 项目级 / 全局 harness 配置的发现与合并。
//!
//! 布局（项目内，可提交入库）：
//! ```text
//! .ai-workbench/
//! ├── context.md     项目上下文（等价 CLAUDE.md / AGENTS.md，自动注入）
//! ├── memory.md      项目记忆，追加式
//! ├── skills/*.md    YAML frontmatter + 正文
//! └── hooks.json     PreToolUse / PostToolUse / Stop
//! ```
//! 全局目录是同名结构，放在 `%APPDATA%\com.ai-workbench.app\harness\`。
//!
//! 合并规则：**项目优先，全局兜底**。标量取项目；技能按 `name` 去重、项目覆盖全局；
//! hooks 取并集（同一事件下项目的先跑）。这里只读不写——写记忆走 `agent_memory`。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 注入上限：上下文与记忆都只取前这么多字符，避免一份大文件把预算吃光。
pub(crate) const CONTEXT_CAP: usize = 8_000;
pub(crate) const MEMORY_CAP: usize = 4_000;
/// 技能正文注入上限（按需命中时使用）。
pub(crate) const SKILL_CAP: usize = 8_000;

pub(crate) const PROJECT_DIR: &str = ".ai-workbench";
pub(crate) const GLOBAL_DIR: &str = "harness";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoadedText {
    /// 绝对路径，前端用来显示来源。
    pub path: String,
    /// `project` / `global`。
    pub scope: String,
    pub text: String,
    /// 因为超过上限被截断。
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillEntry {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub when_to_use: String,
    /// 命中后允许用的工具（空 = 不额外收窄）。
    pub allowed_tools: Vec<String>,
    /// 建议模式（`code` / `doc` / …）；空 = 任意。
    pub mode: String,
    pub path: String,
    pub scope: String,
    /// 正文，注入时截到 `SKILL_CAP`。
    #[serde(skip)]
    pub body: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HookEntry {
    /// `preToolUse` / `postToolUse` / `stop`。
    pub event: String,
    /// 工具名正则；`*` 表示全部。
    pub matcher: String,
    pub command: String,
    #[serde(default = "default_hook_timeout")]
    pub timeout_ms: u64,
    pub scope: String,
}

fn default_hook_timeout() -> u64 {
    30_000
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HarnessConfig {
    pub project_dir: Option<String>,
    pub global_dir: Option<String>,
    pub context: Option<LoadedText>,
    pub memory: Option<LoadedText>,
    pub skills: Vec<SkillEntry>,
    pub hooks: Vec<HookEntry>,
}

impl HarnessConfig {
    pub fn skill(&self, name: &str) -> Option<&SkillEntry> {
        self.skills.iter().find(|skill| skill.name == name)
    }
}

/// 全局配置目录：`%APPDATA%\com.ai-workbench.app\harness`。
/// 取不到 APPDATA（非 Windows 或测试环境）时退回当前目录下的 `.ai-workbench-global`。
pub(crate) fn global_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(|base| PathBuf::from(base).join("com.ai-workbench.app").join(GLOBAL_DIR))
        .unwrap_or_else(|| PathBuf::from(".ai-workbench-global"))
}

/// 发现并加载配置。`root` 是工作区根目录（没有就只读全局那一层）。
pub(crate) fn load(root: Option<&Path>) -> HarnessConfig {
    let mut config = HarnessConfig::default();
    let global = global_dir();
    // 项目优先：先读项目，再读全局，后者只补缺。
    if let Some(project) = root.map(|r| r.join(PROJECT_DIR)).filter(|p| p.is_dir()) {
        config.project_dir = Some(display(&project));
        read_layer(&project, "project", &mut config, true);
    }
    if global.is_dir() {
        config.global_dir = Some(display(&global));
        read_layer(&global, "global", &mut config, false);
    }
    config
}

fn read_layer(dir: &Path, scope: &str, config: &mut HarnessConfig, override_layer: bool) {
    if config.context.is_none() {
        config.context = read_text(dir, "context.md", scope, CONTEXT_CAP);
    }
    if config.memory.is_none() {
        config.memory = read_text(dir, "memory.md", scope, MEMORY_CAP);
    }
    for skill in read_skills(dir, scope) {
        if let Some(existing) = config.skills.iter_mut().find(|s| s.name == skill.name) {
            // 项目层已经定义过同名技能：全局那份不覆盖
            if !override_layer {
                continue;
            }
            *existing = skill;
        } else {
            config.skills.push(skill);
        }
    }
    let mut hooks = read_hooks(dir, scope);
    config.hooks.append(&mut hooks);
}

/// `path` 展开符号链接之后是否仍在 `dir` 之下。
/// 仓库里可以把 `.ai-workbench/context.md` 提交成指向 `~/.ssh/id_rsa` 的链接，
/// 内容会直接进系统提示，所以每次读都要确认落点没跑出这一层目录。
fn within(dir: &Path, path: &Path) -> bool {
    let (Ok(dir), Ok(path)) = (std::fs::canonicalize(dir), std::fs::canonicalize(path)) else {
        return false;
    };
    path.starts_with(dir)
}

fn read_text(dir: &Path, name: &str, scope: &str, cap: usize) -> Option<LoadedText> {
    let path = dir.join(name);
    if !within(dir, &path) {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    let truncated = text.chars().count() > cap;
    let text: String = if truncated { text.chars().take(cap).collect() } else { text };
    Some(LoadedText {
        path: display(&path),
        scope: scope.to_string(),
        text,
        truncated,
    })
}

/// 项目上下文还可以从 `AGENTS.md` / `CLAUDE.md` 回落（很多人已经写了）。
pub(crate) fn fallback_context_files(root: &Path) -> Option<PathBuf> {
    ["AGENTS.md", "CLAUDE.md"]
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
}

fn read_skills(dir: &Path, scope: &str) -> Vec<SkillEntry> {
    let skills_dir = dir.join("skills");
    let Ok(entries) = std::fs::read_dir(&skills_dir) else {
        return Vec::new();
    };
    let mut out: Vec<SkillEntry> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
        .filter(|path| within(&skills_dir, path))
        .filter_map(|path| parse_skill(&path, scope))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `---` 开头的 YAML frontmatter + 正文。没有 frontmatter 时用文件名当技能名。
fn parse_skill(path: &Path, scope: &str) -> Option<SkillEntry> {
    let raw = std::fs::read_to_string(path).ok()?;
    let fallback_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("skill").to_string();
    let (meta, body) = split_frontmatter(&raw);
    let meta: SkillMeta = meta.and_then(|yaml| serde_yaml::from_str(&yaml).ok()).unwrap_or_default();
    let name = if meta.name.trim().is_empty() { fallback_name } else { meta.name };
    let body: String = body.chars().take(SKILL_CAP).collect();
    Some(SkillEntry {
        name,
        description: meta.description,
        when_to_use: meta.when_to_use,
        allowed_tools: meta
            .allowed_tools
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        mode: meta.mode,
        path: display(path),
        scope: scope.to_string(),
        body,
    })
}

#[derive(Debug, Default, Deserialize)]
struct SkillMeta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    when_to_use: String,
    #[serde(default)]
    allowed_tools: String,
    #[serde(default)]
    mode: String,
}

fn split_frontmatter(raw: &str) -> (Option<String>, String) {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let Some(rest) = text.strip_prefix("---") else {
        return (None, text.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (None, text.to_string());
    };
    let yaml = rest[..end].trim_start_matches(['\r', '\n']).to_string();
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();
    (Some(yaml), body)
}

fn read_hooks(dir: &Path, scope: &str) -> Vec<HookEntry> {
    let path = dir.join("hooks.json");
    if !within(dir, &path) {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        // 配错了不阻断提问：这一层的 hooks 直接当没有
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, event) in [
        ("preToolUse", "preToolUse"),
        ("postToolUse", "postToolUse"),
        ("stop", "stop"),
    ] {
        let Some(items) = value.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in items {
            let Some(command) = item.get("command").and_then(|v| v.as_str()).map(str::trim) else {
                continue;
            };
            if command.is_empty() {
                continue;
            }
            out.push(HookEntry {
                event: event.to_string(),
                matcher: item
                    .get("matcher")
                    .and_then(|v| v.as_str())
                    .unwrap_or("*")
                    .trim()
                    .to_string(),
                command: command.to_string(),
                timeout_ms: item
                    .get("timeoutMs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(default_hook_timeout())
                    .clamp(1_000, 120_000),
                scope: scope.to_string(),
            });
        }
    }
    out
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `@` 补全：按子串列出工作区内的文件（最多 200 条，跳过 .gitignore 与构建产物）。
#[tauri::command]
pub fn harness_list_files(path: String, query: String) -> Result<Vec<String>, String> {
    let root = PathBuf::from(path.trim());
    if !root.is_dir() {
        return Err("工作区不存在".into());
    }
    let needle = query.trim().to_lowercase();
    let mut out: Vec<String> = Vec::new();
    // 补全是勤调用路径：大仓库不为它走完整棵树，两万条之后就返回已匹配的部分。
    let mut scanned = 0usize;
    crate::agent_fs::walk(&root, "", false, |p| {
        scanned += 1;
        if scanned > 20_000 {
            return false;
        }
        let rel = p
            .strip_prefix(&root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        if needle.is_empty() || rel.to_lowercase().contains(&needle) {
            out.push(rel);
        }
        out.len() < 200
    })?;
    Ok(out)
}

/// 设置页「扩展」卡：列出一份工作区实际加载到的配置（来源、上下文/记忆长度、技能、hooks）。
#[tauri::command]
pub fn harness_config_load(path: String) -> Result<HarnessConfig, String> {
    let raw = path.trim();
    let root = if raw.is_empty() { None } else { Some(PathBuf::from(raw)) };
    if let Some(root) = &root {
        if !root.is_dir() {
            return Err(format!("工作区不存在：{}", root.to_string_lossy()));
        }
    }
    Ok(load(root.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aw-harness-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn frontmatter_is_split_and_body_kept() {
        let raw = "---\nname: review\ndescription: 代码审查\nallowed-tools: read_file, grep\nmode: code\n---\n看这段代码。\n";
        let (meta, body) = split_frontmatter(raw);
        assert!(meta.unwrap().contains("review"));
        assert_eq!(body.trim(), "看这段代码。");

        let (none, body) = split_frontmatter("没有 frontmatter");
        assert!(none.is_none());
        assert_eq!(body, "没有 frontmatter");
    }

    #[test]
    fn project_layer_wins_over_global() {
        let project = temp_dir("project");
        std::fs::create_dir_all(project.join("skills")).unwrap();
        std::fs::write(project.join("context.md"), "项目上下文").unwrap();
        std::fs::write(
            project.join("skills").join("a.md"),
            "---\nname: same\ndescription: 项目版\n---\n项目正文",
        )
        .unwrap();
        std::fs::write(
            project.join("hooks.json"),
            r#"{ "preToolUse": [{ "matcher": "write_file", "command": "echo pre" }] }"#,
        )
        .unwrap();

        let mut config = HarnessConfig::default();
        read_layer(&project, "project", &mut config, true);
        assert_eq!(config.context.as_ref().unwrap().text, "项目上下文");
        assert_eq!(config.skills.len(), 1);
        assert_eq!(config.skills[0].description, "项目版");
        assert_eq!(config.skills[0].allowed_tools, Vec::<String>::new());
        assert_eq!(config.hooks.len(), 1);
        assert_eq!(config.hooks[0].event, "preToolUse");
        assert_eq!(config.hooks[0].timeout_ms, 30_000);

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn caps_truncate_instead_of_dropping() {
        let dir = temp_dir("caps");
        std::fs::write(dir.join("memory.md"), "x".repeat(MEMORY_CAP + 50)).unwrap();
        let text = read_text(&dir, "memory.md", "project", MEMORY_CAP).unwrap();
        assert!(text.truncated);
        assert_eq!(text.text.chars().count(), MEMORY_CAP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_hooks_file_is_ignored() {
        let dir = temp_dir("hooks");
        std::fs::write(dir.join("hooks.json"), "{ not json").unwrap();
        assert!(read_hooks(&dir, "project").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
