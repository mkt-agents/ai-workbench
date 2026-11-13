//! 只读 git 查询工具（`git_read`）：让模型自己看完整 diff / 历史 / blame。
//!
//! 与 `run_command` 分家的理由：文档任务禁止通用命令（`type D:\other` 能绕过
//! 文件 scope），但「看完整变更」又是逆向文档写深的必需品。这里只暴露白名单
//! 子命令，参数逐 token 校验，直接用 `Command::new("git")` 执行（不走 shell），
//! 不写任何文件。
//!
//! 安全边界：
//! - 子命令白名单（只读）；
//! - 拒绝一切能写文件 / 调外部程序 / 换仓库的 flag（`--output`、`--ext-diff`、
//!   `--textconv`、`--no-index`、`--git-dir`、`--work-tree`、`-C`、`-c`…）；
//! - `path` 走 `Scope::resolve`，文档任务下出不了工作区；
//! - 输出按字符上限截断，超时交给调用方（只读查询通常秒级）。

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::agent_fs::Scope;

/// 单次输出上限：与工具结果预算同量级，避免 `git log` 整库历史把上下文吃光。
const OUTPUT_CAP: usize = 60_000;

/// 允许的子命令（全部只读）。
const ALLOWED_SUBS: &[&str] = &[
    "log",
    "show",
    "diff",
    "blame",
    "status",
    "rev-parse",
    "merge-base",
    "describe",
    "shortlog",
    "ls-files",
];

/// 拒绝的 flag：写文件、调外部程序、换仓库/工作树、改配置。
const DENIED_EXACT: &[&str] = &[
    "-o",
    "-O",
    "-C",
    "-c",
    "--ext-diff",
    "--textconv",
    "--no-index",
    "--upload-pack",
    "--receive-pack",
    "--exec-path",
    "--config-env",
    "--git-dir",
    "--work-tree",
    "--open-files-in-pager",
    "--paginate",
    "--stdin",
];

fn denied_arg(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    DENIED_EXACT.iter().any(|d| lower == d.to_ascii_lowercase())
        || lower.starts_with("--output")
        || lower.starts_with("--ext-diff")
        || lower.starts_with("--textconv")
        || lower.starts_with("--git-dir")
        || lower.starts_with("--work-tree")
        || lower.starts_with("--exec-path")
        || lower.starts_with("--config-env")
        || lower.starts_with("--upload-pack")
        || lower.starts_with("--receive-pack")
}

fn clip(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let head: String = text.chars().take(cap).collect();
    format!("{head}\n…（输出已截断，可用更窄的范围重查）")
}

/// 校验并执行一次只读 git 查询。
pub(crate) fn git_read(scope: &Scope, args: &Value) -> Result<String, String> {
    let root = scope
        .root
        .as_ref()
        .ok_or_else(|| "git_read 需要工作区：先选择仓库".to_string())?;
    let sub = args
        .get("sub")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if sub.is_empty() {
        return Err(format!(
            "缺少 sub：可用 {}",
            ALLOWED_SUBS.join(" / ")
        ));
    }
    if !ALLOWED_SUBS.contains(&sub.as_str()) {
        return Err(format!(
            "git_read 只支持只读子命令 {}（不支持 {sub}）",
            ALLOWED_SUBS.join(" / ")
        ));
    }
    let raw_args = args.get("args").and_then(|v| v.as_str()).unwrap_or("");
    if raw_args.chars().count() > 1_200 {
        return Err("git_read 参数过长：请缩小范围（例如限定路径或提交数）".into());
    }
    let tokens = crate::mcp::split_args(raw_args);
    if tokens.len() > 32 {
        return Err("git_read 参数过多：请一次只查一件事".into());
    }
    for token in &tokens {
        if denied_arg(token) {
            return Err(format!(
                "git_read 拒绝参数 `{token}`：该 flag 会写文件、调用外部程序或切换仓库"
            ));
        }
    }
    let cwd: PathBuf = match args.get("path").and_then(|v| v.as_str()) {
        Some(raw) if !raw.trim().is_empty() => {
            let resolved = scope.resolve(raw)?;
            if !resolved.is_dir() {
                return Err(format!("path 不是目录：{}", raw.trim()));
            }
            resolved
        }
        _ => root.clone(),
    };

    let mut cmd = Command::new("git");
    cmd.arg("--no-pager")
        .arg(&sub)
        .args(&tokens)
        .current_dir(&cwd)
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("无法执行 git：{e}（是否安装了 Git？）"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.lines().rev().take(6).collect::<Vec<_>>();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        return Err(format!(
            "git {sub} 失败：{}",
            if tail.trim().is_empty() {
                "命令返回非零退出码"
            } else {
                tail.trim()
            }
        ));
    }
    if stdout.trim().is_empty() {
        return Ok("（无输出）".into());
    }
    Ok(clip(&stdout, OUTPUT_CAP))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_write_and_escape_flags() {
        for bad in [
            "--output=/tmp/x",
            "--ext-diff",
            "--textconv",
            "--no-index",
            "--git-dir=D:/other/.git",
            "--work-tree=D:/other",
            "-C",
            "-c",
            "--upload-pack=calc",
        ] {
            assert!(denied_arg(bad), "{bad} 应该被拒绝");
        }
        for ok in ["-p", "--stat", "--oneline", "-n", "20", "HEAD~3..HEAD", "--", "src/"] {
            assert!(!denied_arg(ok), "{ok} 应该放行");
        }
    }

    #[test]
    fn rejects_unknown_subcommands_before_running() {
        let root = std::env::temp_dir();
        let scope = crate::agent_fs::Scope::new(Some(root), false);
        let err = git_read(&scope, &json!({ "sub": "reset" })).unwrap_err();
        assert!(err.contains("只读子命令"), "{err}");
        let err = git_read(&scope, &json!({ "sub": "" })).unwrap_err();
        assert!(err.contains("缺少 sub"), "{err}");
        // 参数里带写文件 flag：在真正执行之前就被拒绝。
        let err = git_read(&scope, &json!({ "sub": "log", "args": "--output=D:/x.txt" })).unwrap_err();
        assert!(err.contains("拒绝参数"), "{err}");
    }

    #[test]
    fn clips_long_output() {
        let text = "x".repeat(OUTPUT_CAP + 10);
        let clipped = clip(&text, OUTPUT_CAP);
        assert!(clipped.contains("输出已截断"));
        assert!(clipped.chars().count() <= OUTPUT_CAP + 40);
    }
}
