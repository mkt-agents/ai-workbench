//! AI hub agent permission levels: which tools a level offers, which calls run
//! straight away / ask the user / go through the AI reviewer / are refused, how
//! "allow for this turn" groups calls, and the matching system-prompt policy.
//! Pure functions only (see `agent.rs` and the `agent_*` tool modules for IO).

use std::path::Path;

use serde_json::Value;

use crate::agent::{command_blocked, command_is_readonly};
use crate::agent_tools::GateKind;

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
    /// Unparsable input falls back to `Read`: a level the backend does not
    /// recognise must never open up write tools (fail closed).
    pub(crate) fn parse(raw: &str) -> Self {
        match raw.trim() {
            "workspace" => Self::Workspace,
            "review" => Self::Review,
            "full" => Self::Full,
            _ => Self::Read,
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

// 工具属性（只读 / 变更 / 电脑 / 写入 / 命令 / 敏感 / 工作台）只声明在
// `agent_tools::TOOLS`：闸门、动作步与并发预取都从那张表派生。
// MCP 工具是运行时的 `mcp_<server>_<tool>`，不在表里，由下面的前缀规则覆盖。

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
    // MCP 工具名是 `mcp_<server>_<tool>`：能力由服务自己决定，一律先问；只有完全权限免问
    if tool.starts_with("mcp_") {
        return match permission {
            Permission::Full => Gate::Allow,
            Permission::Review => Gate::Review,
            _ => Gate::Ask,
        };
    }
    match crate::agent_tools::gate_kind(tool) {
        // 只读与纯展示：任何档位都放行
        Some(GateKind::Look) | Some(GateKind::Plan) => Gate::Allow,
        Some(GateKind::Change) => mutating(Gate::Ask),
        Some(GateKind::Computer) => {
            if permission == Permission::Full {
                Gate::Allow
            } else {
                Gate::Deny
            }
        }
        Some(GateKind::Command) => {
            if command_blocked(arg) {
                Gate::Deny
            } else if tool == "run_command" && command_is_readonly(arg) {
                Gate::Allow
            } else {
                mutating(Gate::Ask)
            }
        }
        Some(GateKind::Write) => mutating(Gate::Allow),
        // 出网同样是"读到了不该读的东西"；剪贴板里可能是密码：只读档也要问
        Some(GateKind::Sensitive) => match permission {
            Permission::Read | Permission::Workspace => Gate::Ask,
            Permission::Review => Gate::Review,
            Permission::Full => Gate::Allow,
        },
        Some(GateKind::Workbench) => {
            if workbench_action_is_read(tool, arg) {
                Gate::Allow
            } else {
                mutating(Gate::Ask)
            }
        }
        // 注册表里没有的名字一律拒绝：`offered_tools` 手写了注册表没有的工具、
        // 或者新增工具漏登记时，宁可让执行器报「未知工具」，也不能全档位静默放行。
        None => Gate::Deny,
    }
}

/// 编辑档里本来直接落地的写入，开了「改前确认」就先给用户看 diff（`write_file` / `edit_file`）。
/// 只降一级，不提权：Deny / Ask / Review 的原样返回。
pub(crate) fn needs_diff_confirm(permission: Permission, tool: &str, gate: Gate, confirm: bool) -> bool {
    confirm && gate == Gate::Allow && permission == Permission::Workspace
        && matches!(tool, "write_file" | "edit_file")
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
    /// 子命令前面出现了不认识的全局选项（`git --foo status`）：判不出意图，
    /// 按「可能危险」处理，不当作普通子命令继续往下判。
    pub unknown_option: bool,
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
    for segment in command.split(['&', '|', ';', '(', ')', '\n']) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let Some(start) = words.iter().position(|w| is_git_program(w)) else {
            continue;
        };
        let mut i = start + 1;
        let mut dir = None;
        let mut unknown_option = false;
        while i < words.len() {
            let w = unquote(words[i]);
            if w == "-C" {
                dir = words.get(i + 1).map(|d| unquote(d).to_string());
                i += 2;
                continue;
            }
            if matches!(w, "-c" | "--git-dir" | "--work-tree" | "--namespace") {
                i += 2;
                continue;
            }
            // 粘连写法：`git -C../repo status` / `git -ccore.pager=less status`。
            // 旧实现只认独立成词的 `-C`，粘连时把选项本身当成子命令，黑名单整条失效。
            if let Some(d) = w.strip_prefix("-C") {
                dir = Some(d.to_string());
                i += 1;
                continue;
            }
            if w.len() > 2 && w.starts_with("-c") {
                i += 1;
                continue;
            }
            if w.starts_with("--git-dir=")
                || w.starts_with("--work-tree=")
                || w.starts_with("--namespace=")
                || w.starts_with("--exec-path")
                || w.starts_with("--super-prefix=")
                || w.starts_with("--config-env=")
                || matches!(
                    w,
                    "--no-pager" | "-p" | "-P" | "--paginate" | "--bare" | "--no-replace-objects"
                        | "--no-optional-locks" | "--literal-pathspecs" | "--glob-pathspecs"
                        | "--noglob-pathspecs" | "--icase-pathspecs" | "--no-lazy-fetch"
                        | "--no-advice" | "--version" | "--help" | "-h"
                        | "--html-path" | "--man-path" | "--info-path"
                )
            {
                i += 1;
                continue;
            }
            if w.starts_with('-') {
                // 认不出的全局选项：不知道它会不会改工作区，按可能危险处理
                unknown_option = true;
                i += 1;
                continue;
            }
            break;
        }
        let Some(sub) = words.get(i) else {
            continue;
        };
        out.push(GitCall {
            sub: unquote(sub).to_lowercase(),
            args: words[i + 1..].iter().map(|w| unquote(w).to_string()).collect(),
            dir,
            unknown_option,
        });
    }
    out
}

/// Git forms no permission level may run: they force-overwrite, rewrite history or
/// resolve conflicts on the user's behalf (both pick-a-side and discard-the-merge).
pub(crate) fn git_call_blocked(call: &GitCall) -> bool {
    // 判不出子命令就不放行：`git -C../repo ` 这类粘连写法以前会漏到这里。
    if call.unknown_option {
        return true;
    }
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
    if crate::agent_tools::is_computer(tool) {
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
    /// The user allowed at least one domain, so `web_fetch` is worth offering.
    pub web: bool,
}

/// Tool names offered to the model, in a stable order.
pub(crate) fn offered_tools(permission: Permission, reach: Reach) -> Vec<&'static str> {
    if !reach.tools {
        return Vec::new();
    }
    let writes = permission != Permission::Read;
    let mut names: Vec<&'static str> = Vec::new();
    if reach.files {
        names.extend([
            "read_file",
            "list_dir",
            "find_files",
            "grep",
            "symbols",
            "run_command",
            // 文档 / 表格：只读，非代码任务的主力
            "read_document",
            "read_table",
            "table_query",
        ]);
        if writes {
            names.extend([
                "write_file", "edit_file", "delete_path", "move_path", "start_process", "process_output", "stop_process",
            ]);
        }
    }
    names.extend([
        // Planning is offered even without a repo: it is how the user follows a long turn.
        "update_plan",
        "open_page",
        "list_processes",
        "find_port",
        "clipboard_read",
        "hosts",
        "tunnel",
        "runtime",
        // 只读子代理：默认不下发，`agent.rs` 按 `settings.agentSubagents` 过滤
        "task",
        // 企微业务：文档 / 日程 / 智能表格（复用机器人凭证；未配置时执行会明确报错）
        "wecom_doc_list",
        "wecom_doc_read",
        "wecom_calendar_list",
        "wecom_sheet_list",
        // 企微出站富媒体：把本地文件发回当前对话（Sensitive，只读档也要问）
        "wecom_send_image",
        "wecom_send_file",
    ]);
    if writes {
        names.extend([
            "launch_app",
            "open_target",
            "clipboard_write",
            "kill_process",
            "kill_port",
            "wecom_doc_create",
            "wecom_calendar_create",
            "wecom_sheet_create",
            "wecom_sheet_add_row",
            // 长期记忆：写 memory.md，编辑档先问一次
            "remember",
        ]);
    }
    // 白名单是空的就等于没联网能力：连工具都不下发，省得模型反复试
    if reach.web {
        names.push("web_fetch");
    }
    if permission == Permission::Full && reach.vision {
        names.extend(crate::agent_tools::by_gate(GateKind::Computer));
    }
    names
}

/// Keep only the specs whose `name` is in `names` (spec order preserved).
pub(crate) fn filter_specs(specs: &Value, names: &[&str]) -> Vec<Value> {
    specs
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|s| s.get("name").and_then(|v| v.as_str()).is_some_and(|n| names.contains(&n)))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// `[Agent]` block appended to the system prompt.
///
/// `services`: when `root` is a workspace folder (not itself a repository), the
/// repositories inside it the user picked, as paths relative to `root`.
pub(crate) fn policy_text(permission: Permission, root: Option<&Path>, services: &[String], reach: Reach, hide_abs_path: bool) -> String {
    if !reach.tools {
        return String::new();
    }
    let anywhere = permission == Permission::Full;
    let kind = if services.is_empty() { "repository" } else { "workspace folder" };
    let root_label = |root: &Path| -> String {
        if hide_abs_path {
            root.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "the workspace".into())
        } else {
            root.display().to_string()
        }
    };
    let mut place = match (root, anywhere) {
        (Some(root), false) => format!(
            "You work in the {kind} at {}. Stay inside that directory; paths outside it are refused.",
            root_label(root)
        ),
        (Some(root), true) => format!(
            "The current {kind} is {}; relative paths resolve there. You may also use absolute paths anywhere on this computer.",
            root_label(root)
        ),
        (None, true) => "No workspace is selected. Relative paths resolve against the user's home folder; absolute paths work anywhere on this computer.".into(),
        (None, false) => "No workspace is selected, so you cannot read or change files or run commands. Answer directly; you may open AI Workbench pages or check processes and ports.".into(),
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
            "Permission: edit. File writes and edits inside the workspace run directly. Other commands, deleting or moving \
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
    let web = if reach.web {
        "\nWeb: web_fetch downloads an https page (only hosts the user allowlisted) and returns its text. \
Do not follow redirects yourself by guessing: if it reports one, call it again with the final address. \
Quote the URL when the answer relies on it."
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
        "\n\n[Agent]\n{place}\n{mode}{files}{web}{computer}\n\
For a task that needs more than two tool rounds, call update_plan first with a short list of the steps, then call it \
again whenever a step starts or finishes (send the whole list each time, at most 8 steps). Skip it for simple questions \
and one-step edits.\n\
If the same action keeps failing, do not retry it verbatim: first explain the failure from its output, then try a \
different approach.\nLooking for where a symbol is defined? Try symbols first (it is heuristic: keyword + name), then \
grep for the call sites; do not guess a definition you have not read.\nWhen you are done, answer in the user's \
language without dumping raw tool output."
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
        for tool in crate::agent_tools::by_gate(GateKind::Look)
            .chain(crate::agent_tools::by_gate(GateKind::Plan))
        {
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
        for tool in crate::agent_tools::by_gate(GateKind::Change) {
            expect(tool, "", [Deny, Ask, Review, Allow]);
        }
        // 出网 / 读剪贴板 / 往企微发文件：只读档也要问
        for tool in crate::agent_tools::by_gate(GateKind::Sensitive) {
            expect(tool, "", [Ask, Ask, Review, Allow]);
        }
        expect("clipboard_read", "", [Ask, Ask, Review, Allow]);
        expect("hosts", "read", [Allow; 4]);
        expect("hosts", "write", [Deny, Ask, Review, Allow]);
        expect("tunnel", "status", [Allow; 4]);
        expect("tunnel", "start_quick", [Deny, Ask, Review, Allow]);
        expect("runtime", "list", [Allow; 4]);
        expect("runtime", "switch", [Deny, Ask, Review, Allow]);
        for tool in crate::agent_tools::by_gate(GateKind::Computer) {
            expect(tool, "", [Deny, Deny, Deny, Allow]);
        }
        // 出网（web_fetch）与只读子代理（task）：两者都不改本机，但都有成本 / 外发风险
        expect("web_fetch", "", [Ask, Ask, Review, Allow]);
        expect("task", "", [Allow; 4]);
        // MCP 工具名是运行时的：能做什么由服务决定，只有完全权限不问
        expect("mcp_fs_write_file", "", [Ask, Ask, Review, Allow]);
        // 没登记的工具不是"未知就放行"：闸门先拒，执行器再报 unknown
        expect("no_such_tool", "", [Deny; 4]);
    }

    /// `offered_tools` 里出现注册表没有的名字，会让闸门走「未知」分支。
    /// 这条测试保证下发清单永远是注册表的子集。
    #[test]
    fn offered_tools_are_all_registered() {
        let all = Reach { files: true, tools: true, vision: true, web: true };
        for p in ALL {
            for name in offered_tools(p, all) {
                assert!(
                    crate::agent_tools::get(name).is_some(),
                    "{name} 下发了但不在 agent_tools::TOOLS 里（{p:?}）"
                );
            }
        }
    }

    /// 只读档不能下发「注定被拒」的工具：模型试一次就白扣一个动作步。
    /// Workbench / Sensitive 例外——它们的读动作或确认路径在只读档仍然有效。
    #[test]
    fn readonly_tools_are_not_silently_denied() {
        let all = Reach { files: true, tools: true, vision: true, web: true };
        for name in offered_tools(Permission::Read, all) {
            let kind = crate::agent_tools::gate_kind(name).expect("已由上一条测试保证");
            assert!(
                !matches!(kind, GateKind::Write | GateKind::Change | GateKind::Computer),
                "{name} 在只读档必被拒，不该下发"
            );
        }
    }

    /// 工具表新增一项却没在 `tool_gate` / `offered_tools` 里做决定时，这条会失败。
    /// 附录 A 的矩阵是"唯一事实"，这条测试是它的守门人。
    #[test]
    fn every_tool_in_the_spec_table_has_a_gate_row() {
        let specs = crate::agent::tool_specs();
        let mut in_specs: Vec<String> = specs
            .as_array()
            .expect("tool_specs 是数组")
            .iter()
            .filter_map(|s| s.get("name").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        in_specs.sort();
        let mut known: Vec<String> = crate::agent_tools::names().map(str::to_string).collect();
        known.sort();
        assert_eq!(
            in_specs, known,
            "agent.rs 的 tool_specs 与 agent_tools::TOOLS 不一致：新增工具要同时进注册表、下发列表和附录 A"
        );
        // 反过来：每个声明过的工具都必须真的能下发（漏进 offered_tools 等于下线）
        let all = Reach { files: true, tools: true, vision: true, web: true };
        let offered = offered_tools(Permission::Full, all);
        for name in crate::agent_tools::names() {
            assert!(offered.contains(&name), "{name} 在任何档位都不会下发");
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

    /// 粘连写法（`-C../repo`、`-ccore.pager=less`）必须被剥掉，否则子命令变成选项本身，
    /// 黑名单整条失效。认不出的全局选项一律 fail-closed。
    #[test]
    fn git_calls_handle_glued_global_options() {
        let blocked = |cmd: &str| git_calls(cmd).iter().any(git_call_blocked);
        let calls = git_calls("git -C../repo status");
        assert_eq!(calls[0].sub, "status");
        assert_eq!(calls[0].dir.as_deref(), Some("../repo"));
        assert!(!calls[0].unknown_option);
        assert_eq!(git_calls("git -ccore.pager=less status")[0].sub, "status");
        // 以前这四种全部漏判
        for cmd in [
            "git -C../repo reset --hard",
            "git -C../repo push -f",
            "git -C../repo clean -fd",
            "git -C../repo rebase main",
            "git -ccore.pager=less push -f",
        ] {
            assert!(blocked(cmd), "{cmd} 应该被拦");
        }
        // 认不出的全局选项按危险处理，普通查询不受影响
        assert!(blocked("git --upload-pack=x status"));
        assert!(!blocked("git --no-optional-locks status"));
        assert!(!blocked("git --version"));
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
        let reach = Reach { files: true, tools: true, vision: false, web: false };
        let text = policy_text(Permission::Full, Some(Path::new("D:\\repo")), &[], reach, false);
        assert!(text.contains("Merge conflicts belong to the user") && text.contains("提交页"));
        assert!(REVIEW_SYSTEM.contains("merge conflicts"));
    }

    #[test]
    fn permission_parse_fails_closed() {
        assert_eq!(Permission::parse("read"), Permission::Read);
        assert_eq!(Permission::parse(" workspace "), Permission::Workspace);
        assert_eq!(Permission::parse("review"), Permission::Review);
        assert_eq!(Permission::parse(" full "), Permission::Full);
        // Unknown / missing must not hand out write tools.
        assert_eq!(Permission::parse(""), Permission::Read);
        assert_eq!(Permission::parse("whatever"), Permission::Read);
    }

    #[test]
    fn mcp_tools_always_ask_below_full_access() {
        let name = "mcp_fs_write_file";
        assert_eq!(tool_gate(Permission::Read, name, ""), Gate::Ask);
        assert_eq!(tool_gate(Permission::Workspace, name, ""), Gate::Ask);
        assert_eq!(tool_gate(Permission::Review, name, ""), Gate::Review);
        assert_eq!(tool_gate(Permission::Full, name, ""), Gate::Allow);
        // 前缀规则不能顺手把 `mcpfs_read` 当 MCP 工具：它没登记，按未知工具拒绝
        assert_eq!(tool_gate(Permission::Workspace, "mcpfs_read", ""), Gate::Deny);
    }

    #[test]
    fn diff_confirm_only_demotes_edit_level_writes() {
        assert!(needs_diff_confirm(Permission::Workspace, "write_file", Gate::Allow, true));
        assert!(needs_diff_confirm(Permission::Workspace, "edit_file", Gate::Allow, true));
        // 开关关着 / 不是写入 / 不是编辑档：一律不动
        assert!(!needs_diff_confirm(Permission::Workspace, "write_file", Gate::Allow, false));
        assert!(!needs_diff_confirm(Permission::Workspace, "delete_path", Gate::Allow, true));
        assert!(!needs_diff_confirm(Permission::Workspace, "run_command", Gate::Allow, true));
        assert!(!needs_diff_confirm(Permission::Full, "write_file", Gate::Allow, true));
        assert!(!needs_diff_confirm(Permission::Read, "write_file", Gate::Allow, true));
        // 只降 Allow 这一级：Deny 不会被开关变成 Ask
        assert!(!needs_diff_confirm(Permission::Workspace, "write_file", Gate::Deny, true));
    }

    #[test]
    fn offered_tools_follow_level_reach_and_vision() {
        let all = Reach { files: true, tools: true, vision: true, web: true };
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
        // 计划是纯展示的，任何档位、有没有仓库都能用
        assert!(read.contains(&"update_plan") && full.contains(&"update_plan"));
        assert!(no_files.contains(&"update_plan"));
        for p in ALL {
            assert_eq!(tool_gate(p, "update_plan", ""), Gate::Allow, "{p:?}");
        }
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
        let files = Reach { files: true, tools: true, vision: false, web: false };
        assert!(policy_text(Permission::Read, Some(&root), &[], files, false).contains("read only"));
        assert!(policy_text(Permission::Workspace, Some(&root), &[], files, false).contains("paths outside it are refused"));
        assert!(policy_text(Permission::Full, Some(&root), &[], files, false).contains("anywhere on this computer"));
        assert!(policy_text(Permission::Full, None, &[], files, false).contains("home folder"));
        let none = Reach { files: false, tools: true, vision: false, web: false };
        assert!(policy_text(Permission::Read, None, &[], none, false).contains("cannot read or change files"));
        assert!(!policy_text(Permission::Full, Some(&root), &[], files, false).contains("Computer:"));
        let vision = Reach { vision: true, ..files };
        assert!(policy_text(Permission::Full, Some(&root), &[], vision, false).contains("Computer:"));
        assert!(policy_text(Permission::Full, Some(&root), &[], Reach { tools: false, ..files }, false).is_empty());
    }

    #[test]
    fn workspace_root_lists_services_and_per_repo_git() {
        let root = Path::new("D:\\work\\p1");
        let files = Reach { files: true, tools: true, vision: false, web: false };
        let services = vec!["order".to_string(), "user".to_string()];
        let text = policy_text(Permission::Workspace, Some(root), &services, files, false);
        assert!(text.contains("workspace folder at D:\\work\\p1"), "{text}");
        assert!(text.contains("order, user") && text.contains("git -C"), "{text}");
        let repo = policy_text(Permission::Workspace, Some(root), &[], files, false);
        assert!(repo.contains("repository at") && !repo.contains("git -C <service>"), "{repo}");
    }

    #[test]
    fn policy_text_hides_abs_path_for_wecom() {
        let root = Path::new(r"D:\ai_project\ai-workbench");
        let files = Reach { files: true, tools: true, vision: false, web: false };
        let text = policy_text(Permission::Workspace, Some(root), &[], files, true);
        assert!(!text.contains(r"D:\"));
        assert!(!text.contains("ai_project"));
        assert!(text.contains("ai-workbench"));
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
