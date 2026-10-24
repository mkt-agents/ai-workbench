//! 长期记忆：追加式 Markdown，项目一份、全局一份。
//!
//! 为什么用文件而不是表：记忆要能跟着仓库走（提交入库、review 时看得见），
//! 和 `.ai-workbench/context.md` 是同一套心智。写入只走 [`append`]，
//! 由 `remember` 工具触发，带体积与条数上限——记忆是提示，不是数据库。

use std::path::{Path, PathBuf};

use chrono::Local;

/// 单份记忆文件上限（字节）：再大就会挤掉上下文预算。
pub(crate) const MAX_BYTES: usize = 32 * 1024;
/// 最多保留多少条。
pub(crate) const MAX_ENTRIES: usize = 200;

fn dir_for(root: Option<&Path>, scope: &str) -> Result<PathBuf, String> {
    if scope == "global" {
        return Ok(crate::harness_config::global_dir());
    }
    let root = root.ok_or("项目记忆需要先选工作区")?;
    Ok(root.join(crate::harness_config::PROJECT_DIR))
}

/// 记忆文件位置（目前只有测试直接用它；`append` 内部复用同一规则）。
#[cfg(test)]
pub(crate) fn memory_path(root: Option<&Path>, scope: &str) -> Result<PathBuf, String> {
    Ok(dir_for(root, scope)?.join("memory.md"))
}

/// 追加一条记忆，返回写入的文件路径。
pub(crate) fn append(root: Option<&Path>, text: &str, scope: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("没有要记住的内容".into());
    }
    let dir = dir_for(root, scope)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("建记忆目录失败：{e}"))?;
    let path = dir.join("memory.md");
    let mut body = std::fs::read_to_string(&path).unwrap_or_default();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    // 一条一行：换行折成空格，免得一条记忆在文件里看起来像多条
    body.push_str(&format!(
        "- {}：{}\n",
        Local::now().format("%Y-%m-%d"),
        text.replace(['\r', '\n'], " ")
    ));
    std::fs::write(&path, trim_to_limits(&body)).map_err(|e| format!("写记忆失败：{e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// 超限时从最旧的条目开始丢（保留最近 `MAX_ENTRIES` 条与 `MAX_BYTES` 字节）。
fn trim_to_limits(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut kept: Vec<&str> = lines.iter().rev().take(MAX_ENTRIES).rev().copied().collect();
    let mut text = format!("{}\n", kept.join("\n"));
    while text.len() > MAX_BYTES && kept.len() > 1 {
        kept.remove(0);
        text = format!("{}\n", kept.join("\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aw-memory-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn appends_one_line_per_fact() {
        let root = temp_dir("append");
        let first = append(Some(&root), "发布前先跑 npm run build", "project").unwrap();
        append(Some(&root), "多行\n内容", "project").unwrap();
        let body = std::fs::read_to_string(&first).unwrap();
        assert_eq!(body.lines().count(), 2);
        assert!(body.contains("发布前先跑 npm run build"));
        assert!(body.contains("多行 内容"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_text_is_rejected() {
        assert!(append(None, "   ", "project").is_err());
    }

    #[test]
    fn old_entries_are_dropped_first() {
        let body = (1..=MAX_ENTRIES + 20)
            .map(|i| format!("- 第 {i} 条"))
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = trim_to_limits(&body);
        assert_eq!(trimmed.lines().count(), MAX_ENTRIES);
        assert!(!trimmed.contains("第 1 条"));
        assert!(trimmed.contains(&format!("第 {} 条", MAX_ENTRIES + 20)));
    }

    #[test]
    fn global_scope_uses_the_global_dir() {
        let path = memory_path(None, "global").unwrap();
        assert!(path.ends_with("memory.md"));
        assert!(path.to_string_lossy().contains("harness"));
    }
}
