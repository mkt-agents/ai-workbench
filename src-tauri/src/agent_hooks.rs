//! 项目 hooks：在工具调用前后 / 回合结束时跑用户脚本（`.ai-workbench/hooks.json`）。
//!
//! 约束（比 Claude Code 保守，因为这个 harness 默认面对的是本机任意目录）：
//! - 只读权限**不跑**：hooks 等价于任意命令执行，只读档不该有这条路；
//! - 与 `run_command` 共用同一套环境：`cmd /c`，cwd 是工作区根；
//! - 超时 1–120 秒，输出截到 2k（只回灌够判断的量）；
//! - `PreToolUse` 非零退出 = 拒绝这次调用，stderr 回灌给模型。

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::harness_config::HookEntry;

/// 输出回灌上限：hook 不是给模型读大段文本的。
const OUTPUT_CAP: usize = 2_000;

pub(crate) struct HookOutcome {
    pub ok: bool,
    pub output: String,
}

/// 工具名匹配：`*` / 空 = 全部；支持前缀或后缀通配；其余按子串（对用户更宽容）。
/// 这里只决定「跑不跑」，不代表授权——授权仍由闸门与审批决定。
pub(crate) fn matches(matcher: &str, tool: &str) -> bool {
    let matcher = matcher.trim();
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    if let Some(rest) = matcher.strip_prefix('*') {
        if let Some(suffix) = rest.strip_suffix('*') {
            return tool.contains(suffix);
        }
        return tool.ends_with(rest);
    }
    if let Some(prefix) = matcher.strip_suffix('*') {
        return tool.starts_with(prefix);
    }
    tool == matcher || tool.contains(matcher)
}

/// 跑一条 hook。payload 以环境变量传给脚本（不拼进命令行，避免引号地狱）。
pub(crate) fn run(entry: &HookEntry, cwd: &Path, payload: &str) -> HookOutcome {
    let mut command = Command::new("cmd");
    command
        .arg("/c")
        .arg(&entry.command)
        .current_dir(cwd)
        .env("AI_WORKBENCH_HOOK", payload)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return HookOutcome {
                ok: false,
                output: format!("hook 启动失败：{err}"),
            }
        }
    };
    let timeout = Duration::from_millis(entry.timeout_ms.clamp(1_000, 120_000));
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut text = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut text);
                }
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut text);
                }
                let output: String = text.trim().chars().take(OUTPUT_CAP).collect();
                return HookOutcome {
                    ok: status.success(),
                    output,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return HookOutcome {
                        ok: false,
                        output: "hook 超时".into(),
                    };
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                return HookOutcome {
                    ok: false,
                    output: format!("hook 等待失败：{err}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matchers_cover_wildcards_and_substrings() {
        assert!(matches("*", "write_file"));
        assert!(matches("", "write_file"));
        assert!(matches("write_*", "write_file"));
        assert!(!matches("write_*", "read_file"));
        assert!(matches("*_file", "edit_file"));
        assert!(matches("edit", "edit_file"));
        assert!(!matches("write_file", "read_file"));
    }

    #[test]
    fn a_failing_hook_reports_its_output() {
        let entry = HookEntry {
            event: "preToolUse".into(),
            matcher: "*".into(),
            command: "exit /b 3".into(),
            timeout_ms: 5_000,
            scope: "project".into(),
        };
        let outcome = run(&entry, &std::env::temp_dir(), "{}");
        assert!(!outcome.ok);
    }

    #[test]
    fn a_successful_hook_returns_stdout() {
        let entry = HookEntry {
            event: "preToolUse".into(),
            matcher: "*".into(),
            command: "echo ready".into(),
            timeout_ms: 5_000,
            scope: "project".into(),
        };
        let outcome = run(&entry, &std::env::temp_dir(), "{}");
        assert!(outcome.ok);
        assert!(outcome.output.contains("ready"));
    }
}
