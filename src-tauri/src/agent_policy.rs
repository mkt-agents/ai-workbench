//! AI hub agent permission levels: which tools a level offers, which calls run
//! straight away / ask the user / go through the AI reviewer / are refused, how
//! "allow for this turn" groups calls, and the matching system-prompt policy.
//! Pure functions only (see `agent.rs` and the `agent_*` tool modules for IO).

use std::path::Path;

use serde_json::Value;

use crate::agent::{command_blocked, command_is_readonly};

/// Ordered from least to most open; matches the picker in the AI hub composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Permission {
    /// 只读权限: read, search, read-only git. Nothing that changes state.
    Read,
    /// 编辑权限: write files in the repo freely; other changes ask first.
    Workspace,
    /// 审查托管: every change is checked by an AI reviewer first.
    Review,
    /// 完全权限: no confirmation, whole machine (protected paths and blocked commands still apply).
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
    /// Ask the user, unless they already allowed this kind for the turn (see `action_kind`).
    Ask,
    /// Ask the AI reviewer; anything but a clear "allow" escalates to the user.
    Review,
    Deny,
}

/// Tools that only look: never gated.
const LOOK_TOOLS: &[&str] = &[
    "read_file", "list_dir", "find_files", "grep", "open_page", "process_output", "list_processes", "find_port",
];
/// Tools that change something outside a file write: ask in 编辑权限.
const CHANGE_TOOLS: &[&str] = &[
    "delete_path", "move_path", "stop_process", "kill_process", "kill_port", "open_target", "clipboard_write",
    "launch_app",
];
/// Operate the computer (screenshot + mouse / keyboard): 完全权限 only.
pub(crate) const COMPUTER_TOOLS: &[&str] = &["screenshot", "mouse", "keyboard", "wait"];

/// Read-only actions of the grouped workbench tools.
fn workbench_action_is_read(tool: &str, action: &str) -> bool {
    match tool {
        "hosts" => action == "read",
        "tunnel" => action == "status",
        "runtime" => matches!(action, "list" | "active"),
        _ => false,
    }
}

/// What happens to one tool call under `permission`. `arg` is the command for
/// `run_command` / `start_process` and the `action` for the grouped workbench tools.
pub(crate) fn tool_gate(permission: Permission, tool: &str, arg: &str) -> Gate {
    let mutating = |workspace: Gate| match permission {
        Permission::Read => Gate::Deny,
        Permission::Workspace => workspace,
        Permission::Review => Gate::Review,
        Permission::Full => Gate::Allow,
    };
    if LOOK_TOOLS.contains(&tool) {
        return Gate::Allow;
    }
    if CHANGE_TOOLS.contains(&tool) {
        return mutating(Gate::Ask);
    }
    if COMPUTER_TOOLS.contains(&tool) {
        return if permission == Permission::Full { Gate::Allow } else { Gate::Deny };
    }
    match tool {
        "run_command" | "start_process" => {
            if command_blocked(arg) {
                Gate::Deny
            } else if tool == "run_command" && command_is_readonly(arg) {
                Gate::Allow
            } else {
                mutating(Gate::Ask)
            }
        }
        "write_file" | "edit_file" => mutating(Gate::Allow),
        // Clipboard can hold passwords: even read-only asks first.
        "clipboard_read" => match permission {
            Permission::Read | Permission::Workspace => Gate::Ask,
            Permission::Review => Gate::Review,
            Permission::Full => Gate::Allow,
        },
        "hosts" | "tunnel" | "runtime" => {
            if workbench_action_is_read(tool, arg) {
                Gate::Allow
            } else {
                mutating(Gate::Ask)
            }
        }
        // Unknown tools fall through to the executor's "unknown tool" error.
        _ => Gate::Allow,
    }
}

/// One `git …` inside a command line: lowercased subcommand, arguments as written
/// (git flags are case-sensitive: `-s` strategy vs `-S` sign), with git's global
/// options (`-C <dir>`, `-c k=v`, `--git-dir=…`) skipped.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GitCall {
    pub sub: String,
    pub args: Vec<String>,
    /// `-C <dir>` when given (original spelling).
    pub dir: Option<String>,
}

fn unquote(word: &str) -> &str {
    word.trim_matches(|c| c == '"' || c == '\'')
}

fn is_git_program(word: &str) -> bool {
    let w = unquote(word).to_lowercase();
    let name = w.rsplit(['\\', '/']).next().unwrap_or(&w);
    name == "git" || name == "git.exe"
}

/// Every git invocation in `command`; `&&`, `&`, `|`, `;` and parentheses end one.
pub(crate) fn git_calls(command: &str) -> Vec<GitCall> {
    let mut out = Vec::new();
    for segment in command.split(|c: char| matches!(c, '&' | '|' | ';' | '(' | ')' | '\n')) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let Some(start) = words.iter().position(|w| is_git_program(w)) else {
            continue;
        };
        let mut i = start + 1;
        let mut dir = None;
        while i < words.len() {
            let w = unquote(words[i]);
            match w {
                "-C" => {
                    dir = words.get(i + 1).map(|d| unquote(d).to_string());
                    i += 2;
                }
                "-c" | "--git-dir" | "--work-tree" | "--namespace" => i += 2,
                _ if w.starts_with("--git-dir=")
                    || w.starts_with("--work-tree=")
                    || w.starts_with("--namespace=")
                    || w.starts_with("--exec-path")
                    || matches!(w, "--no-pager" | "-p" | "--paginate" | "--bare" | "--no-replace-objects") =>
                {
                    i += 1
                }
                _ => break,
            }
        }
        let Some(sub) = words.get(i) else {
            continue;
        };
        out.push(GitCall {
            sub: unquote(sub).to_lowercase(),
            args: words[i + 1..].iter().map(|w| unquote(w).to_string()).collect(),
            dir,
        });
    }
    out
}

/// Git forms no permission level may run: they force-overwrite, rewrite history or
/// resolve conflicts on the user's behalf (both pick-a-side and discard-the-merge).
pub(crate) fn git_call_blocked(call: &GitCall) -> bool {
    let long = |flag: &str| call.args.iter().any(|a| a.eq_ignore_ascii_case(flag));
    // Short flag clusters such as `-fd` / `-uf` / `-Xtheirs`.
    let short_has = |c: char| {
        call.args
            .iter()
            .any(|a| a.len() > 1 && a.starts_with('-') && !a.starts_with("--") && a[1..].contains(c))
    };
    // -X <option> / -s <strategy> pick a side for every conflict (-S is GPG signing).
    let picks_strategy = || {
        call.args.iter().any(|a| {
            a.starts_with("-X") || a.starts_with("-s") || a.to_ascii_lowercase().starts_with("--strategy")
        })
    };
    match call.sub.as_str() {
        "rebase" | "filter-branch" | "filter-repo" => true,
        "pull" => {
            picks_strategy()
                || call.args.iter().any(|a| a == "-r")
                || call.args.iter().any(|a| {
                    let a = a.to_ascii_lowercase();
                    a == "--rebase" || (a.starts_with("--rebase=") && a != "--rebase=false")
                })
        }
        "merge" => picks_strategy(),
        "checkout" | "restore" => long("--ours") || long("--theirs") || (call.sub == "checkout" && short_has('f')),
        "push" => short_has('f') || long("--mirror") || call.args.iter().any(|a| a.starts_with('+')),
        "stash" => matches!(call.args.first().map(|a| a.to_ascii_lowercase()).as_deref(), Some("drop" | "clear")),
        "reset" => long("--hard") || long("--keep") || long("--merge"),
        "clean" => short_has('f') || short_has('x') || short_has('X') || long("--force"),
        _ => false,
    }
}

/// Grouping for "允许本轮同类": commands by program (`git`, `npm`…), workbench tools by
/// action, everything else by tool name. Git groups by subcommand: allowing
/// `git add` must not also allow `git pull` or `git merge`.
pub(crate) fn action_kind(tool: &str, arg: &str) -> String {
    match tool {
        "run_command" | "start_process" => {
            let arg = arg.trim();
            // A quoted program path may contain spaces: `"C:\Program Files\nodejs\npm.cmd" test`.
            let first = match arg.strip_prefix('"') {
                Some(rest) => rest.split('"').next().unwrap_or(""),
                None => arg.split_whitespace().next().unwrap_or(""),
            };
            let name = first.rsplit(['\\', '/']).next().unwrap_or(first).to_lowercase();
            let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".cmd")).unwrap_or(&name);
            if name == "git" {
                if let Some(call) = git_calls(arg).first() {
                    return format!("cmd:git {}", call.sub);
                }
            }
            format!("cmd:{name}")
        }
        "hosts" | "tunnel" | "runtime" => format!("{tool}:{arg}"),
        _ => tool.to_string(),
    }
}

/// Reason handed back to the model (and shown in the step log) for a `Gate::Deny`.
pub(crate) fn deny_reason(permission: Permission, tool: &str, arg: &str) -> String {
    if matches!(tool, "run_command" | "start_process") && command_blocked(arg) {
        return "这条命令被拒绝：危险命令在任何权限下都不允许执行。".into();
    }
    if COMPUTER_TOOLS.contains(&tool) {
        return "只有完全权限、并且模型支持看图时才能操作电脑。没有执行。".into();
    }
    if permission == Permission::Read {
        let what = match tool {
            "write_file" | "edit_file" => "改文件",
            "delete_path" | "move_path" => "删除或移动文件",
            "launch_app" | "open_target" => "打开程序或文件",
            "kill_process" | "kill_port" | "stop_process" => "结束进程",
            "clipboard_write" => "写剪贴板",
            "hosts" | "tunnel" | "runtime" => "修改系统设置",
            _ => "执行会修改的命令",
        };
        return format!("只读权限下不能{what}。没有执行。请告诉用户：要切换到编辑权限或更高的档位才能做这一步。");
    }
    "这个操作被拒绝，没有执行。".into()
}

/// What the agent can reach this turn.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reach {
    /// File and command tools are available (a repo is selected, or 完全权限 works machine-wide).
    pub files: bool,
    /// `tools: false` (text-only tasks) offers nothing.
    pub tools: bool,
    /// The model accepts images: needed for operating the computer.
    pub vision: bool,
}

/// Tool names offered to the model, in a stable order.
pub(crate) fn offered_tools(permission: Permission, reach: Reach) -> Vec<&'static str> {
    if !reach.tools {
        return Vec::new();
    }
    let writes = permission != Permission::Read;
    let mut names: Vec<&'static str> = Vec::new();
    if reach.files {
        names.extend(["read_file", "list_dir", "find_files", "grep", "run_command"]);
        if writes {
            names.extend([
                "write_file", "edit_file", "delete_path", "move_path", "start_process", "process_output", "stop_process",
            ]);
        }
    }
    names.extend(["open_page", "list_processes", "find_port", "clipboard_read", "hosts", "tunnel", "runtime"]);
    if writes {
        names.extend(["launch_app", "open_target", "clipboard_write", "kill_process", "kill_port"]);
    }
    if permission == Permission::Full && reach.vision {
        names.extend(COMPUTER_TOOLS);
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
///
/// `services`: when `root` is a workspace folder (not itself a repository), the
/// repositories inside it the user picked, as paths relative to `root`.
pub(crate) fn policy_text(permission: Permission, root: Option<&Path>, services: &[String], reach: Reach) -> String {
    if !reach.tools {
        return String::new();
    }
    let anywhere = permission == Permission::Full;
    let kind = if services.is_empty() { "repository" } else { "workspace folder" };
    let mut place = match (root, anywhere) {
        (Some(root), false) => format!(
            "You work in the {kind} at {}. Stay inside that directory; paths outside it are refused.",
            root.display()
        ),
        (Some(root), true) => format!(
            "The current {kind} is {}; relative paths resolve there. You may also use absolute paths anywhere on this computer.",
            root.display()
        ),
        (None, true) => "No repository is selected. Relative paths resolve against the user's home folder; absolute paths work anywhere on this computer.".into(),
        (None, false) => "No repository is selected, so you cannot read or change files or run commands. Answer directly; you may open AI Workbench pages or check processes and ports.".into(),
    };
    if root.is_some() && !services.is_empty() {
        place.push_str(&format!(
            "\nThe workspace folder itself is not a git repository. It holds these repositories (services) the user picked: {}. \
Each is its own repository: run git with cwd set to that service folder (or git -C <service>), and say which service \
a change belongs to.",
            services.join(", ")
        ));
    }
    let mode = match permission {
        Permission::Read => {
            "Permission: read only. You may read and search files and run read-only git commands \
(status, diff, log, show, branch, remote -v). Changes of any kind are refused. If the user asks for a change, \
explain what you would do and tell them to switch to the edit permission."
        }
        Permission::Workspace => {
            "Permission: edit. File writes and edits inside the repository run directly. Other commands, deleting or moving \
files, launching programs and system changes ask the user first."
        }
        Permission::Review => {
            "Permission: auto review. Every change is checked by an automatic reviewer first; risky ones go to the user."
        }
        Permission::Full => {
            "Permission: full access. Changes run without asking, anywhere on this computer. Destructive commands and \
system folders (Windows, Program Files) are still refused, so do not try them."
        }
    };
    let files = if reach.files {
        "\nFiles: read big files in pages with read_file offset/limit. Change existing files with edit_file (exact, unique \
`old` text); use write_file only for new files or full rewrites. delete_path moves to the Recycle Bin. \
Use find_files to locate files by name and grep (regex optional) to search contents.\n\
Commands: run_command runs cmd.exe by default (quote with double quotes; single quotes are literal) or PowerShell with \
shell=\"powershell\". A failed command's output starts with `exit N`. For servers and watchers (npm run dev, etc.) use \
start_process, then process_output to read it and stop_process when done; never wait on them with run_command.\n\
Do not create helper files (for example a commit message file); pass text inline with -m. If you did create one, \
delete it before you finish and never commit it.\n\
Never use git push --force, reset --hard, or clean. git push and pull use the system proxy; a push may show a Windows \
credential window and take minutes, do not retry a push that is still running.\n\
Merge conflicts belong to the user. If a pull, merge, cherry-pick or stash pop reports conflicts, a push is rejected, \
or git status shows unmerged paths: stop. Do not resolve or discard them yourself (no -X ours/theirs, checkout --ours/--theirs, \
editing conflict markers, git add of conflicted files, merge --abort, force push). Tell the user which files conflict and that \
they can resolve them in Git 管理 → 提交页; while the repository is mid-merge your git writes and file edits there are refused.\n\
Never stage or commit obvious temp files (*.tmp, *.log, *.bak, *.swp, ~$*, .DS_Store, Thumbs.db) unless asked; mention them instead."
    } else {
        ""
    };
    let computer = if anywhere && reach.vision {
        "\nComputer: you can see and operate the screen. Take a screenshot first and after every few actions to check \
the result. Coordinates are pixels of the latest screenshot. Prefer keyboard shortcuts over precise clicks. The user can \
stop you at any time by moving the mouse to the top-left corner."
    } else {
        ""
    };
    format!(
        "\n\n[Agent]\n{place}\n{mode}{files}{computer}\n\
If the same action keeps failing, do not retry it verbatim: first explain the failure from its output, then try a \
different approach.\nWhen you are done, answer in the user's language without dumping raw tool output."
    )
}

pub(crate) const REVIEW_SYSTEM: &str = "You review one action that an AI agent wants to take on the user's computer. \
Reply with JSON only, no other text: {\"verdict\":\"allow\"|\"ask\",\"reason\":\"<one short sentence in the user's language>\"}.\n\
Choose allow only when the action is clearly part of what the user asked for and is low-risk or easy to undo: \
editing source or docs, git add / commit, running tests, builds or linters, git push to a normal branch, starting a dev server.\n\
Choose ask for anything that deletes or overwrites many files, rewrites git history, pulls or merges other branches in, \
resolves or aborts merge conflicts, touches credentials, keys or system settings \
(hosts, environment, runtimes), kills processes the user did not mention, installs software globally, downloads and runs \
scripts, sends data elsewhere, or is unrelated to the request. When unsure, choose ask.";

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

    const ALL: [Permission; 4] = [Permission::Read, Permission::Workspace, Permission::Review, Permission::Full];

    fn expect(tool: &str, arg: &str, row: [Gate; 4]) {
        for (p, g) in ALL.iter().zip(row) {
            assert_eq!(tool_gate(*p, tool, arg), g, "{tool} {arg} at {p:?}");
        }
    }

    #[test]
    fn gate_table_matches_the_plan() {
        use Gate::{Allow, Ask, Deny, Review};
        for tool in LOOK_TOOLS {
            expect(tool, "", [Allow; 4]);
        }
        expect("run_command", "git status", [Allow; 4]);
        expect("run_command", "git push --force", [Deny; 4]);
        expect("start_process", "format C:", [Deny; 4]);
        expect("run_command", "git commit -m \"x\"", [Deny, Ask, Review, Allow]);
        // A background process is never "read-only", even for a read-only git command.
        expect("start_process", "git status", [Deny, Ask, Review, Allow]);
        expect("write_file", "", [Deny, Allow, Review, Allow]);
        expect("edit_file", "", [Deny, Allow, Review, Allow]);
        for tool in CHANGE_TOOLS {
            expect(tool, "", [Deny, Ask, Review, Allow]);
        }
        expect("clipboard_read", "", [Ask, Ask, Review, Allow]);
        expect("hosts", "read", [Allow; 4]);
        expect("hosts", "write", [Deny, Ask, Review, Allow]);
        expect("tunnel", "status", [Allow; 4]);
        expect("tunnel", "start_quick", [Deny, Ask, Review, Allow]);
        expect("runtime", "list", [Allow; 4]);
        expect("runtime", "switch", [Deny, Ask, Review, Allow]);
        for tool in COMPUTER_TOOLS {
            expect(tool, "", [Deny, Deny, Deny, Allow]);
        }
    }

    #[test]
    fn action_kind_groups_by_program() {
        // Git by subcommand: allowing `git add` for the turn must not allow `git pull`.
        assert_eq!(action_kind("run_command", "git commit -m x"), "cmd:git commit");
        assert_ne!(action_kind("run_command", "git add -A"), action_kind("run_command", "git pull"));
        assert_eq!(action_kind("run_command", "git -C ../x status"), "cmd:git status");
        assert_eq!(action_kind("run_command", "\"C:\\Program Files\\nodejs\\npm.cmd\" test"), "cmd:npm");
        assert_eq!(action_kind("start_process", "NPM.exe run dev"), "cmd:npm");
        assert_eq!(action_kind("run_command", "D:\\tools\\rg.exe foo"), "cmd:rg");
        assert_eq!(action_kind("hosts", "write"), "hosts:write");
        assert_eq!(action_kind("delete_path", "a.txt"), "delete_path");
        assert_ne!(action_kind("run_command", "git add ."), action_kind("run_command", "del a.txt"));
    }

    #[test]
    fn git_calls_skip_global_options_and_split_chains() {
        let calls = git_calls("cd x && git -C ../repo -c core.pager=less status -s & \"git.exe\" push origin main");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].sub, "status");
        assert_eq!(calls[0].dir.as_deref(), Some("../repo"));
        assert_eq!(calls[1].sub, "push");
        assert_eq!(calls[1].args, vec!["origin".to_string(), "main".to_string()]);
        assert!(git_calls("npm test").is_empty());
        assert!(git_calls("echo git").is_empty());
    }

    #[test]
    fn git_blacklist_is_per_subcommand_and_case_aware() {
        let blocked = |cmd: &str| git_calls(cmd).iter().any(git_call_blocked);
        for cmd in [
            "git push -f",
            "git push -uf origin x",
            "git push origin +main",
            "git push --mirror",
            "git merge -X theirs other",
            "git merge -s ours other",
            "git pull -Xours",
            "git pull --rebase",
            "git pull -r",
            "git -C sub rebase main",
            "git rebase --skip",
            "git checkout --theirs a.txt",
            "git restore --ours a.txt",
            "git checkout -f main",
            "git stash drop",
            "git stash clear",
            "git reset  --hard",
            "git reset --keep HEAD~1",
            "git clean -df",
            "git clean -Xn",
        ] {
            assert!(blocked(cmd), "{cmd}");
        }
        for cmd in [
            "git merge -S other",
            "git merge --no-ff other",
            "git pull",
            "git pull --ff-only",
            "git pull --rebase=false",
            "git checkout main",
            "git stash list",
            "git stash pop",
            "git reset HEAD a.txt",
            "git push origin main",
            "git log --grep=rebase",
            "git commit -S -m x",
            "git clean -n",
        ] {
            assert!(!blocked(cmd), "{cmd}");
        }
    }

    #[test]
    fn policy_tells_the_agent_to_hand_conflicts_over() {
        let reach = Reach { files: true, tools: true, vision: false };
        let text = policy_text(Permission::Full, Some(Path::new("D:\\repo")), &[], reach);
        assert!(text.contains("Merge conflicts belong to the user") && text.contains("提交页"));
        assert!(REVIEW_SYSTEM.contains("merge conflicts"));
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
    fn offered_tools_follow_level_reach_and_vision() {
        let all = Reach { files: true, tools: true, vision: true };
        assert!(offered_tools(Permission::Full, Reach { tools: false, ..all }).is_empty());
        let read = offered_tools(Permission::Read, all);
        assert!(read.contains(&"read_file") && read.contains(&"run_command") && read.contains(&"hosts"));
        assert!(!read.contains(&"write_file") && !read.contains(&"launch_app") && !read.contains(&"screenshot"));
        let edit = offered_tools(Permission::Workspace, all);
        assert!(edit.contains(&"edit_file") && edit.contains(&"start_process") && edit.contains(&"kill_port"));
        assert!(!edit.contains(&"screenshot"));
        let full = offered_tools(Permission::Full, all);
        assert!(full.contains(&"screenshot") && full.contains(&"keyboard"));
        let blind = offered_tools(Permission::Full, Reach { vision: false, ..all });
        assert!(!blind.contains(&"mouse"));
        let no_files = offered_tools(Permission::Workspace, Reach { files: false, ..all });
        assert!(!no_files.contains(&"read_file") && no_files.contains(&"open_page"));
    }

    #[test]
    fn filter_specs_keeps_order() {
        let specs = json!([{ "name": "a" }, { "name": "b" }, { "name": "c" }]);
        let kept = filter_specs(&specs, &["c", "a"]);
        let names: Vec<_> = kept.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn policy_mentions_level_reach_and_computer() {
        let root = std::env::temp_dir();
        let files = Reach { files: true, tools: true, vision: false };
        assert!(policy_text(Permission::Read, Some(&root), &[], files).contains("read only"));
        assert!(policy_text(Permission::Workspace, Some(&root), &[], files).contains("paths outside it are refused"));
        assert!(policy_text(Permission::Full, Some(&root), &[], files).contains("anywhere on this computer"));
        assert!(policy_text(Permission::Full, None, &[], files).contains("home folder"));
        let none = Reach { files: false, tools: true, vision: false };
        assert!(policy_text(Permission::Read, None, &[], none).contains("cannot read or change files"));
        assert!(!policy_text(Permission::Full, Some(&root), &[], files).contains("Computer:"));
        let vision = Reach { vision: true, ..files };
        assert!(policy_text(Permission::Full, Some(&root), &[], vision).contains("Computer:"));
        assert!(policy_text(Permission::Full, Some(&root), &[], Reach { tools: false, ..files }).is_empty());
    }

    #[test]
    fn workspace_root_lists_services_and_per_repo_git() {
        let root = Path::new("D:\\work\\p1");
        let files = Reach { files: true, tools: true, vision: false };
        let services = vec!["order".to_string(), "user".to_string()];
        let text = policy_text(Permission::Workspace, Some(root), &services, files);
        assert!(text.contains("workspace folder at D:\\work\\p1"), "{text}");
        assert!(text.contains("order, user") && text.contains("git -C"), "{text}");
        let repo = policy_text(Permission::Workspace, Some(root), &[], files);
        assert!(repo.contains("repository at") && !repo.contains("git -C <service>"), "{repo}");
    }

    #[test]
    fn deny_reason_names_the_blocked_action() {
        assert!(deny_reason(Permission::Read, "edit_file", "").contains("改文件"));
        assert!(deny_reason(Permission::Read, "kill_port", "").contains("结束进程"));
        assert!(deny_reason(Permission::Full, "run_command", "git reset --hard").contains("危险命令"));
        assert!(deny_reason(Permission::Workspace, "mouse", "").contains("操作电脑"));
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
