//! 项目 hooks：在工具调用前后 / 回合结束时跑用户脚本（`.ai-workbench/hooks.json`）。
//!
//! 约束（比 Claude Code 保守，因为这个 harness 默认面对的是本机任意目录）：
//! - 只读权限**不跑**：hooks 等价于任意命令执行，只读档不该有这条路；
//! - 与 `run_command` 共用同一套环境：`cmd /c`，cwd 是工作区根；
//! - 超时 1–120 秒，输出截到 2k（只回灌够判断的量）；
//! - `PreToolUse` 非零退出 = 拒绝这次调用，stderr 回灌给模型。

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
    // hooks 与 `run_command` 共用黑名单：配置文件里写了也不等于可以绕过危险命令拦截。
    if crate::agent::command_blocked(&entry.command) {
        return HookOutcome {
            ok: false,
            output: "hook 命令命中危险命令黑名单，已拒绝执行".into(),
        };
    }
    // 输出写临时文件而不是管道：hook 输出超过管道缓冲（64KB）会把子进程卡住，
    // 最后只能超时收场，看起来像 hook 失败。环境变量也截断，Windows 上限 32k。
    let payload: String = payload.chars().take(2_000).collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let log_path = std::env::temp_dir().join(format!("aw-hook-{}-{stamp}.log", std::process::id()));
    let log = match std::fs::File::create(&log_path) {
        Ok(file) => file,
        Err(err) => {
            return HookOutcome {
                ok: false,
                output: format!("hook 日志创建失败：{err}"),
            }
        }
    };
    let log_err = match log.try_clone() {
        Ok(file) => file,
        Err(err) => {
            return HookOutcome {
                ok: false,
                output: format!("hook 日志句柄复制失败：{err}"),
            }
        }
    };
    let mut command = Command::new("cmd");
    command
        .arg("/c")
        .arg(&entry.command)
        .current_dir(cwd)
        .env("AI_WORKBENCH_HOOK", payload)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
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
                let text = std::fs::read_to_string(&log_path).unwrap_or_default();
                let _ = std::fs::remove_file(&log_path);
                let output: String = text.trim().chars().take(OUTPUT_CAP).collect();
                return HookOutcome {
                    ok: status.success(),
                    output,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = std::fs::remove_file(&log_path);
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
    fn dangerous_hooks_are_refused() {
        let entry = HookEntry {
            event: "preToolUse".into(),
            matcher: "*".into(),
            command: "shutdown /s /t 0".into(),
            timeout_ms: 5_000,
            scope: "project".into(),
        };
        let outcome = run(&entry, &std::env::temp_dir(), "{}");
        assert!(!outcome.ok);
        assert!(outcome.output.contains("黑名单"));
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
