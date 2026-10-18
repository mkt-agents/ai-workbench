//! AI hub agent permission levels: which tools a level offers, which calls run
//! straight away / ask the user / go through the AI reviewer / are refused, and the
//! matching system-prompt policy. Pure functions only (see `agent.rs` for IO).

use std::path::Path;

use serde_json::Value;

use crate::agent::{command_blocked, command_is_readonly};

/// Ordered from least to most open; matches the picker in the AI hub composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Permission {
    /// 只读权限: read, search, read-only git. Nothing that changes state.
    Read,
    /// 编辑权限: write files in the repo freely; other commands / apps ask first.
    Workspace,
    /// 审查托管: every write / command / app launch is checked by an AI reviewer first.
    Review,
    /// 完全权限: no confirmation at all (blocked commands and repo bounds still apply).
    Full,
}

impl Permission {
    /// Unknown / empty falls back to `Workspace`, the behaviour before levels existed.
    pub(crate) fn parse(raw: &str) -> Self {
        match raw.trim() {
            "read" => Self::Read,
            "review" => Self::Review,
            "full" => Self::Full,
            _ => Self::Workspace,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    Allow,
    /// Ask the user (once per kind per turn, see the caller).
    Ask,
    /// Ask the AI reviewer; anything but a clear "allow" escalates to the user.
    Review,
    Deny,
}

/// What happens to one tool call under `permission`. `command` is only read for `run_command`.
pub(crate) fn tool_gate(permission: Permission, tool: &str, command: &str) -> Gate {
    let mutating = |p: Permission, workspace: Gate| match p {
        Permission::Read => Gate::Deny,
        Permission::Workspace => workspace,
        Permission::Review => Gate::Review,
        Permission::Full => Gate::Allow,
    };
    match tool {
        "read_file" | "list_dir" | "grep" | "open_page" => Gate::Allow,
        "run_command" => {
            if command_blocked(command) {
                Gate::Deny
            } else if command_is_readonly(command) {
                Gate::Allow
            } else {
                mutating(permission, Gate::Ask)
            }
        }
        "write_file" => mutating(permission, Gate::Allow),
        "launch_app" => mutating(permission, Gate::Ask),
        // Unknown tools fall through to the executor's "unknown tool" error.
        _ => Gate::Allow,
    }
}

/// Reason handed back to the model (and shown in the step log) for a `Gate::Deny`.
pub(crate) fn deny_reason(permission: Permission, tool: &str, command: &str) -> String {
    if tool == "run_command" && command_blocked(command) {
        return "这条命令被拒绝：危险命令在任何权限下都不允许执行。".into();
    }
    if permission == Permission::Read {
        let what = match tool {
            "write_file" => "写文件",
            "launch_app" => "启动应用",
            _ => "执行会修改的命令",
        };
        return format!("只读权限下不能{what}。没有执行。请告诉用户：要切换到编辑权限或更高的档位才能做这一步。");
    }
    "这个操作被拒绝，没有执行。".into()
}

/// Tool names offered to the model. `tools = false` (text-only tasks) offers none;
/// without a repository only the app-level tools remain.
pub(crate) fn offered_tools(permission: Permission, has_repo: bool, tools: bool) -> Vec<&'static str> {
    if !tools {
        return Vec::new();
    }
    let mut names: Vec<&'static str> = Vec::new();
    if has_repo {
        names.extend(["read_file", "list_dir", "grep", "run_command"]);
        if permission != Permission::Read {
            names.push("write_file");
        }
    }
    names.push("open_page");
    if permission != Permission::Read {
        names.push("launch_app");
    }
    names
}

/// Keep only the specs whose `name` is in `names` (spec order preserved).
pub(crate) fn filter_specs(specs: &Value, names: &[&str]) -> Vec<Value> {
    specs
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|s| s.get("name").and_then(|v| v.as_str()).map_or(false, |n| names.contains(&n)))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// `[Agent]` block appended to the system prompt.
pub(crate) fn policy_text(permission: Permission, root: Option<&Path>, tools: bool) -> String {
    if !tools {
        return String::new();
    }
    let Some(root) = root else {
        return "\n\n[Agent]\nNo repository is selected, so you cannot read or change files. Answer directly. \
You may open AI Workbench pages if the user asks."
            .into();
    };
    let mode = match permission {
        Permission::Read => {
            "Permission: read only. You may read files, search, and run read-only git commands \
(status, diff, log, show, branch, remote -v). Writing files, other commands and launching apps are refused. \
If the user asks for a change, explain what you would change and tell them to switch to the edit permission."
        }
        Permission::Workspace => {
            "Permission: edit. You may create and edit files inside the repository directly. \
Other shell commands and launching apps ask the user first (only the first one in this turn; later ones do not)."
        }
        Permission::Review => {
            "Permission: auto review. File writes, shell commands and app launches are checked by an automatic \
reviewer first; risky ones are sent to the user for confirmation."
        }
        Permission::Full => {
            "Permission: full access. File writes, shell commands and app launches run without asking. \
Destructive commands are still refused, so do not try them."
        }
    };
    format!(
        "\n\n[Agent]\nYou can use tools to work in the repository at {}.\nStay inside that directory. Read before you write.\n{mode}\n\
Shell commands run in Windows cmd.exe: quote arguments with double quotes; single quotes are passed literally. \
A failed command's output starts with `exit N`.\n\
Do not create helper files in the repository (for example a commit message file); pass text inline with -m. \
If you did create one, delete it before you finish and never commit it.\n\
Never use git push --force, reset --hard, or clean.\n\
git push and pull use the same system proxy as the commit page. A push may show a Windows credential window and can take several minutes. Do not retry a push that is still running.\n\
If the same command keeps failing, do not retry it verbatim: first explain the failure from its output (quoting, paths, staging state), then try a different approach.\n\
Never stage or commit obvious temp files (*.tmp, *.log, *.bak, *.swp, ~$*, .DS_Store, Thumbs.db) unless the user explicitly asks; mention them instead.\n\
When you are done, answer in the user's language without dumping raw tool output.\n\
There is no computer-control tool.",
        root.display()
    )
}

pub(crate) const REVIEW_SYSTEM: &str = "You review one action that an AI coding agent wants to take in the user's repository. \
Reply with JSON only, no other text: {\"verdict\":\"allow\"|\"ask\",\"reason\":\"<one short sentence in the user's language>\"}.\n\
Choose allow only when the action is clearly part of what the user asked for, stays inside the repository, and is low-risk or easy to undo: \
editing source or docs, git add / commit, running tests, builds or linters, git push to a normal branch.\n\
Choose ask for anything that deletes or overwrites many files, rewrites git history, touches credentials, keys or system settings, \
installs software globally, downloads and runs scripts, sends data elsewhere, or is unrelated to the request. When unsure, choose ask.";

/// Reviewer reply → (allow, reason). Only an explicit `"verdict": "allow"` passes;
/// anything unparsable escalates to the user.
pub(crate) fn parse_review_verdict(raw: &str) -> (bool, String) {
    let text = crate::ai_commands::strip_think_tags(raw);
    let parsed = match (text.find('{'), text.rfind('}')) {
        (Some(start), Some(end)) if end > start => serde_json::from_str::<Value>(&text[start..=end]).ok(),
        _ => None,
    };
    let Some(json) = parsed else {
        return (false, String::new());
    };
    let verdict = json.get("verdict").and_then(|v| v.as_str()).unwrap_or("").trim().to_lowercase();
    let reason = json.get("reason").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    (verdict == "allow", reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gate_table_matches_the_picker() {
        use Gate::{Allow, Ask, Deny};
        let all = [Permission::Read, Permission::Workspace, Permission::Review, Permission::Full];
        // Reads, read-only git and page navigation never ask.
        for p in all {
            assert_eq!(tool_gate(p, "read_file", ""), Allow);
            assert_eq!(tool_gate(p, "grep", ""), Allow);
            assert_eq!(tool_gate(p, "open_page", ""), Allow);
            assert_eq!(tool_gate(p, "run_command", "git status"), Allow);
            // Blocked commands are refused at every level.
            assert_eq!(tool_gate(p, "run_command", "git push --force"), Deny);
            assert_eq!(tool_gate(p, "run_command", "format C:"), Deny);
        }
        let expect = |tool: &str, cmd: &str, row: [Gate; 4]| {
            for (p, g) in all.iter().zip(row) {
                assert_eq!(tool_gate(*p, tool, cmd), g, "{tool} {cmd} at {p:?}");
            }
        };
        expect("write_file", "", [Deny, Allow, Gate::Review, Allow]);
        expect("run_command", "git commit -m \"x\"", [Deny, Ask, Gate::Review, Allow]);
        expect("launch_app", "", [Deny, Ask, Gate::Review, Allow]);
    }

    #[test]
    fn permission_parse_defaults_to_workspace() {
        assert_eq!(Permission::parse("read"), Permission::Read);
        assert_eq!(Permission::parse(" full "), Permission::Full);
        assert_eq!(Permission::parse("review"), Permission::Review);
        assert_eq!(Permission::parse(""), Permission::Workspace);
        assert_eq!(Permission::parse("whatever"), Permission::Workspace);
    }

    #[test]
    fn offered_tools_follow_level_repo_and_task() {
        assert!(offered_tools(Permission::Full, true, false).is_empty());
        let read = offered_tools(Permission::Read, true, true);
        assert!(read.contains(&"read_file") && read.contains(&"run_command"));
        assert!(!read.contains(&"write_file") && !read.contains(&"launch_app"));
        let edit = offered_tools(Permission::Workspace, true, true);
        assert!(edit.contains(&"write_file") && edit.contains(&"launch_app"));
        // No repository: nothing that needs a working directory.
        assert_eq!(offered_tools(Permission::Read, false, true), vec!["open_page"]);
        assert_eq!(offered_tools(Permission::Full, false, true), vec!["open_page", "launch_app"]);
    }

    #[test]
    fn filter_specs_keeps_order() {
        let specs = json!([{ "name": "a" }, { "name": "b" }, { "name": "c" }]);
        let kept = filter_specs(&specs, &["c", "a"]);
        let names: Vec<_> = kept.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn policy_mentions_the_level_and_handles_no_repo() {
        let root = std::env::temp_dir();
        assert!(policy_text(Permission::Read, Some(&root), true).contains("read only"));
        assert!(policy_text(Permission::Full, Some(&root), true).contains("full access"));
        assert!(policy_text(Permission::Read, None, true).contains("No repository"));
        assert!(policy_text(Permission::Full, Some(&root), false).is_empty());
    }

    #[test]
    fn deny_reason_names_the_blocked_action() {
        assert!(deny_reason(Permission::Read, "write_file", "").contains("写文件"));
        assert!(deny_reason(Permission::Full, "run_command", "git reset --hard").contains("危险命令"));
    }

    #[test]
    fn review_verdict_only_passes_explicit_allow() {
        assert_eq!(
            parse_review_verdict(r#"{"verdict":"allow","reason":"普通提交"}"#),
            (true, "普通提交".into())
        );
        let (ok, reason) = parse_review_verdict("<think>hmm</think>结论：{\"verdict\": \"ASK\", \"reason\": \"会删除目录\"}");
        assert!(!ok);
        assert_eq!(reason, "会删除目录");
        assert_eq!(parse_review_verdict("allow"), (false, String::new()));
        assert_eq!(parse_review_verdict(""), (false, String::new()));
    }
}
