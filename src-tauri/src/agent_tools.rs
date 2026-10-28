//! 工具注册表：名字、分组、闸门类别、能否并发预取、是否消耗动作步，只在这里声明一次。
//!
//! 以前这些属性散在四处：`agent.rs` 的 `round_is_action` / `is_readonly_tool`、
//! `agent_policy.rs` 的 `LOOK_TOOLS` / `CHANGE_TOOLS` / `COMPUTER_TOOLS`、以及闸门里
//! 的一串 `match`。新增工具时漏掉任何一处都会让「只读 / 要审批 / 算动作」三者对不上，
//! 甚至出现「闸门放行但执行器拒绝」的静默不一致。现在它们都从 [`TOOLS`] 派生。
//!
//! 这里不含 schema（仍在 `agent.rs::tool_specs`，由 `tools_registry_matches_specs`
//! 测试保证两边名字一致）与 handler（在 `agent.rs::execute_tool`）。

/// 能力分组：模式（对话 / 代码 / 文档 / 数据 / 调研 / 运维）按它筛工具子集。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolGroup {
    /// 仓库、文件、命令、git 相关。
    Code,
    /// 文档抽取与写作。
    Doc,
    /// 表格 / 数据查询。
    Data,
    /// 出网抓取。
    Web,
    /// 本机系统：进程、端口、hosts、隧道、运行时、打开东西。
    System,
    /// 规划、子代理、界面跳转这类不直接改状态的能力。
    Meta,
}

/// 闸门类别：档位换算仍由 `agent_policy::tool_gate` 做（那里还要看参数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GateKind {
    /// 只读：任何档位直接放行。
    Look,
    /// 纯展示（`update_plan`）：放行且不消耗动作步。
    Plan,
    /// 文件写入：编辑档直接落地，只读档拒绝。
    Write,
    /// 命令：黑名单优先，只读命令放行，其余按档位。
    Command,
    /// 改状态但不是文件写入：编辑档先问。
    Change,
    /// 出网 / 读剪贴板：只读档也要问。
    Sensitive,
    /// 分组工作台工具：按 `action` 参数判断是否只读。
    Workbench,
    /// 操作电脑：仅完全权限 + 模型支持看图。
    Computer,
}

pub(crate) struct ToolDef {
    pub name: &'static str,
    pub group: ToolGroup,
    pub gate: GateKind,
    /// 能否整块搬进 `spawn_blocking` 并发预取（只有读文件 / 搜索这类值得）。
    pub parallel: bool,
    /// 是否消耗「动作步」。只读与纯展示不算，免得查几次资料就把 32 步用完。
    pub action: bool,
}

pub(crate) const TOOLS: &[ToolDef] = &[
    // —— 规划 / 子代理 / 界面 ——
    ToolDef { name: "update_plan", group: ToolGroup::Meta, gate: GateKind::Plan, parallel: false, action: false },
    // 计划闸门：plan_first 开启时先只读规划，用户批准后才升档执行。
    ToolDef { name: "exit_plan_mode", group: ToolGroup::Meta, gate: GateKind::Plan, parallel: false, action: false },
    // 结构化澄清：无副作用，全档放行，但执行时会等用户选答。
    ToolDef { name: "ask_user", group: ToolGroup::Meta, gate: GateKind::Look, parallel: false, action: false },
    ToolDef { name: "task", group: ToolGroup::Meta, gate: GateKind::Look, parallel: false, action: false },
    ToolDef { name: "remember", group: ToolGroup::Meta, gate: GateKind::Change, parallel: false, action: true },
    // 代码智能（stdio LSP）：不可用时返回结构化错误，不下线工具。
    ToolDef { name: "lsp", group: ToolGroup::Code, gate: GateKind::Look, parallel: true, action: false },
    // —— 文档 / 表格：非代码任务的主力 ——
    ToolDef { name: "read_document", group: ToolGroup::Doc, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "read_table", group: ToolGroup::Data, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "table_query", group: ToolGroup::Data, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "open_page", group: ToolGroup::Meta, gate: GateKind::Look, parallel: false, action: true },
    // —— 代码：读 ——
    ToolDef { name: "read_file", group: ToolGroup::Code, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "list_dir", group: ToolGroup::Code, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "find_files", group: ToolGroup::Code, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "grep", group: ToolGroup::Code, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "symbols", group: ToolGroup::Code, gate: GateKind::Look, parallel: true, action: false },
    // —— 代码：写 ——
    ToolDef { name: "write_file", group: ToolGroup::Code, gate: GateKind::Write, parallel: false, action: true },
    ToolDef { name: "edit_file", group: ToolGroup::Code, gate: GateKind::Write, parallel: false, action: true },
    ToolDef { name: "delete_path", group: ToolGroup::Code, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "move_path", group: ToolGroup::Code, gate: GateKind::Change, parallel: false, action: true },
    // —— 命令 / 后台进程 ——
    ToolDef { name: "run_command", group: ToolGroup::Code, gate: GateKind::Command, parallel: false, action: true },
    ToolDef { name: "start_process", group: ToolGroup::Code, gate: GateKind::Command, parallel: false, action: true },
    ToolDef { name: "process_output", group: ToolGroup::Code, gate: GateKind::Look, parallel: false, action: false },
    ToolDef { name: "stop_process", group: ToolGroup::Code, gate: GateKind::Change, parallel: false, action: true },
    // —— 网页 ——
    ToolDef { name: "web_fetch", group: ToolGroup::Web, gate: GateKind::Sensitive, parallel: false, action: true },
    // 仅当用户配置了搜索 API 时才下发（见 agent_policy::offered_tools / AgentRunRequest）。
    ToolDef { name: "web_search", group: ToolGroup::Web, gate: GateKind::Sensitive, parallel: false, action: true },
    // MCP resources（有启用的 MCP 服务时才有意义；agent.rs 在 attach 后追加下发）
    ToolDef { name: "list_mcp_resources", group: ToolGroup::Meta, gate: GateKind::Look, parallel: false, action: false },
    ToolDef { name: "read_mcp_resource", group: ToolGroup::Meta, gate: GateKind::Look, parallel: false, action: false },
    // —— 系统 ——
    ToolDef { name: "launch_app", group: ToolGroup::System, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "open_target", group: ToolGroup::System, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "clipboard_read", group: ToolGroup::System, gate: GateKind::Sensitive, parallel: false, action: true },
    ToolDef { name: "clipboard_write", group: ToolGroup::System, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "list_processes", group: ToolGroup::System, gate: GateKind::Look, parallel: false, action: false },
    ToolDef { name: "kill_process", group: ToolGroup::System, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "find_port", group: ToolGroup::System, gate: GateKind::Look, parallel: false, action: false },
    ToolDef { name: "kill_port", group: ToolGroup::System, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "hosts", group: ToolGroup::System, gate: GateKind::Workbench, parallel: false, action: true },
    ToolDef { name: "tunnel", group: ToolGroup::System, gate: GateKind::Workbench, parallel: false, action: true },
    ToolDef { name: "runtime", group: ToolGroup::System, gate: GateKind::Workbench, parallel: false, action: true },
    // —— 操作电脑（完全权限 + 看图） ——
    ToolDef { name: "screenshot", group: ToolGroup::System, gate: GateKind::Computer, parallel: false, action: false },
    ToolDef { name: "mouse", group: ToolGroup::System, gate: GateKind::Computer, parallel: false, action: true },
    ToolDef { name: "keyboard", group: ToolGroup::System, gate: GateKind::Computer, parallel: false, action: true },
    ToolDef { name: "wait", group: ToolGroup::System, gate: GateKind::Computer, parallel: false, action: false },
    // —— 企微业务（复用机器人 Bot ID/Secret，走 Open API；只读查询不占动作步）——
    ToolDef { name: "wecom_doc_list", group: ToolGroup::Doc, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "wecom_doc_read", group: ToolGroup::Doc, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "wecom_doc_create", group: ToolGroup::Doc, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "wecom_calendar_list", group: ToolGroup::Doc, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "wecom_calendar_create", group: ToolGroup::Doc, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "wecom_sheet_list", group: ToolGroup::Data, gate: GateKind::Look, parallel: true, action: false },
    ToolDef { name: "wecom_sheet_create", group: ToolGroup::Data, gate: GateKind::Change, parallel: false, action: true },
    ToolDef { name: "wecom_sheet_add_row", group: ToolGroup::Data, gate: GateKind::Change, parallel: false, action: true },
    // —— 企微出站富媒体：把本地生成的图片/文件发回当前企微对话 ——
    // 发出去就是数据外发（可能是任意本地文件），按 Sensitive 处理：只读 / 编辑档先问。
    ToolDef { name: "wecom_send_image", group: ToolGroup::Doc, gate: GateKind::Sensitive, parallel: false, action: true },
    ToolDef { name: "wecom_send_file", group: ToolGroup::Doc, gate: GateKind::Sensitive, parallel: false, action: true },
    // —— SkyWalking 只读查询：排查线上问题取日志/调用链（地址来自企微渠道配置，空则不下发）——
    ToolDef { name: "skywalking_query", group: ToolGroup::Doc, gate: GateKind::Look, parallel: false, action: false },
];

pub(crate) fn get(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|tool| tool.name == name)
}

pub(crate) fn gate_kind(name: &str) -> Option<GateKind> {
    get(name).map(|tool| tool.gate)
}

/// 未知工具按「算动作」处理：和以前不在排除列表里的行为一致。
pub(crate) fn counts_as_action(name: &str) -> bool {
    get(name).map(|tool| tool.action).unwrap_or(true)
}

pub(crate) fn can_prefetch(name: &str) -> bool {
    get(name).map(|tool| tool.parallel).unwrap_or(false)
}

pub(crate) fn is_computer(name: &str) -> bool {
    matches!(gate_kind(name), Some(GateKind::Computer))
}

/// 全部工具名（守门测试用；运行时不枚举，避免多一份名单）。
#[allow(dead_code)]
pub(crate) fn names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().map(|tool| tool.name)
}

pub(crate) fn by_gate(kind: GateKind) -> impl Iterator<Item = &'static str> {
    TOOLS.iter().filter(move |tool| tool.gate == kind).map(|tool| tool.name)
}

/// `plan_first` 锁定阶段只保留只读 / 计划 / 提问；写、命令、MCP、系统变更一律等 `exit_plan_mode`。
pub(crate) fn is_plan_phase_tool(name: &str) -> bool {
    matches!(name, "ask_user" | "exit_plan_mode" | "update_plan")
        || matches!(gate_kind(name), Some(GateKind::Look) | Some(GateKind::Plan))
}

/// 某个模式下这个工具该不该下发。
///
/// 读（Look）、纯展示（Plan）与写文件（Write）在文档 / 数据 / 调研 / 运维下都给——写报告也要落盘；
/// 收窄的是命令、系统操作与网页：调研模式不该去杀进程，运维模式也不必抓网页。
/// 「对话」是最小集：只读 + 计划 + 记住事实，不写文件、不跑命令、不碰系统与网页。
/// 空模式与 `code` 保持原样（全给），未知模式同样不收窄，避免新增模式忘记登记就静默下线工具。
pub(crate) fn mode_allows(mode: &str, tool: &str) -> bool {
    let Some(def) = get(tool) else {
        // MCP 等运行时工具不在这里判断
        return true;
    };
    if matches!(def.gate, GateKind::Look | GateKind::Plan) {
        // 只读工具在任何模式都给；「对话」只碰代码 / 文档 / 数据，不驱动系统与网页。
        if mode == "chat" {
            return matches!(
                def.group,
                ToolGroup::Code | ToolGroup::Doc | ToolGroup::Data | ToolGroup::Meta
            );
        }
        return true;
    }
    if def.gate == GateKind::Write {
        // 写文件：除「对话」外任何模式都给——文档 / 数据要交报告，运维也要落盘。
        return mode != "chat";
    }
    match mode {
        // 命令 / 系统 / 网页 / 电脑：对话模式一律不给；「记住一条事实」例外（属于对话本身的能力）
        "chat" => def.gate == GateKind::Change && def.group == ToolGroup::Meta,
        // run_command 例外：文档/数据轨偶尔要 pandoc、python 一类只读管道，闸门仍走 Ask。
        "doc" | "data" => {
            matches!(def.group, ToolGroup::Doc | ToolGroup::Data | ToolGroup::Meta | ToolGroup::Web)
                || tool == "run_command"
        }
        "research" => matches!(def.group, ToolGroup::Web | ToolGroup::Meta),
        "ops" => matches!(def.group, ToolGroup::System | ToolGroup::Meta),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_is_unique() {
        let mut names: Vec<&str> = names().collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "注册表里有重名工具");
    }

    #[test]
    fn pure_presentation_never_counts_as_an_action() {
        // `update_plan` 只发一张清单，任何档位都不能因为它把动作步耗掉。
        for tool in TOOLS {
            if tool.gate == GateKind::Plan {
                assert!(!tool.action, "{} 是纯展示工具，不该消耗动作步", tool.name);
            }
        }
        // 只读不等于免费：`open_page` 会切走用户当前页面，仍算一次动作（保持旧行为）。
        let open = get("open_page").expect("open_page 在注册表里");
        assert_eq!(open.gate, GateKind::Look);
        assert!(open.action);
    }

    #[test]
    fn only_readers_run_in_parallel() {
        for tool in TOOLS {
            if tool.parallel {
                assert!(
                    matches!(tool.group, ToolGroup::Code | ToolGroup::Doc | ToolGroup::Data),
                    "{} 不是读取类工具",
                    tool.name
                );
                assert_eq!(tool.gate, GateKind::Look, "{} 会改东西，不能并发预取", tool.name);
            }
        }
    }

    #[test]
    fn modes_narrow_commands_but_keep_reads_and_writes() {
        // 读、写文件、计划：任何模式下都在（写文档也要落盘）
        for mode in ["doc", "data", "research", "ops"] {
            assert!(mode_allows(mode, "read_file"), "{mode} 不能读文件");
            assert!(mode_allows(mode, "write_file"), "{mode} 不能写文件");
            assert!(mode_allows(mode, "update_plan"), "{mode} 不能出计划");
        }
        // 调研模式不该杀进程；运维模式也不必抓网页
        assert!(!mode_allows("research", "kill_process"));
        assert!(!mode_allows("ops", "web_fetch"));
        assert!(mode_allows("research", "web_fetch"));
        assert!(mode_allows("ops", "kill_process"));
        // 文档/数据轨允许 run_command（闸门 Ask），方便 pandoc 一类流水线
        assert!(mode_allows("doc", "run_command"));
        assert!(mode_allows("data", "run_command"));
        assert!(!mode_allows("doc", "kill_process"));
        assert!(!mode_allows("chat", "run_command"));
        // 对话模式：只读 + 计划 + 记住事实，不写文件、不跑命令、不碰系统与网页
        assert!(mode_allows("chat", "read_file"));
        assert!(mode_allows("chat", "update_plan"));
        assert!(mode_allows("chat", "ask_user"));
        assert!(mode_allows("chat", "remember"));
        assert!(!mode_allows("chat", "write_file"));
        assert!(!mode_allows("chat", "run_command"));
        assert!(!mode_allows("chat", "web_fetch"));
        assert!(!mode_allows("chat", "kill_process"));
        // 空模式与未知模式不收窄，行为与改造前一致
        assert!(mode_allows("", "kill_process"));
        assert!(mode_allows("whatever", "web_fetch"));
    }

    #[test]
    fn plan_phase_keeps_reads_and_blocks_writes() {
        assert!(is_plan_phase_tool("read_file"));
        assert!(is_plan_phase_tool("ask_user"));
        assert!(is_plan_phase_tool("exit_plan_mode"));
        assert!(is_plan_phase_tool("update_plan"));
        assert!(is_plan_phase_tool("lsp"));
        assert!(!is_plan_phase_tool("write_file"));
        assert!(!is_plan_phase_tool("run_command"));
        assert!(!is_plan_phase_tool("web_fetch"));
    }

    #[test]
    fn computer_tools_are_the_clicking_ones() {
        let names: Vec<&str> = by_gate(GateKind::Computer).collect();
        assert_eq!(names, vec!["screenshot", "mouse", "keyboard", "wait"]);
        assert!(is_computer("mouse"));
        assert!(!is_computer("read_file"));
    }
}
