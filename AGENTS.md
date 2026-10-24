# AGENTS.md

面向 AI 编码代理的项目指南。改代码前先读本文件；深入细节再看 `docs/CODE_WIKI.md` 与 `交接文档.md`。

## 项目定位

**已确认定位（见 `ROADMAP.md`）**：**AI 驱动的开发工具箱**——以 **DeepSeek 本地服务**为核心 AI 引擎，Git / Cursor 身份管理为基础能力，AI 模型配置与系统工具（Hosts / 内网穿透等）为辅助。

- **愿景**：本地优先的桌面 AI 助手；多模型配置（提示词独立页已下线，天玑优化仍可用），管理开发者的 Git/Cursor 多身份与本机隧道/Hosts 等上下文。**所有数据留在本机，不上传密钥**。
- **功能分层**：核心 = DSH / 模型配置 / Cursor（提示词独立页已下线，代码保留）；基础 = Git 多仓（仓库 / 提交 / 报告）/ 运行时切换；辅助 = Hosts / Cloudflared / 网页工具；系统工具 = 工具箱（devtools）。
- **已移除的功能，不要再去找**：自动化测试执行 / 测试用例管理 / 覆盖率 / 漏洞扫描整套能力已删除，其历史文档（`test-assistant-*.md`）也已清理，相关数据表在 `lib.rs` setup 中被 `DROP TABLE`。Git 报告页是**静态分析 + 按需 AI**：只读 git diff，不执行测试、不产出覆盖率。若需查阅旧实现，`git log -- docs/test-assistant-summary.md` 仍可回溯（最后出现在 `4a03f74`）。工具箱页的 **HTTP 客户端**同样已整体删除：`HttpClientTool.tsx` / `devtools_commands.rs`（该域只剩这一条命令，文件一起删了）/ `http_saved_requests` + `http_history` 两张表（setup 里 `DROP TABLE`）/ devtools i18n 的 `http.*` 与 `group.network`，工具箱现在是 10 个（转换 / 生成 / 解析 / 计算 / 系统五组）。
- **AI 能力在范围内**：天玑流式对话、仓库工具代理、报告与提交里的模型调用都可以继续做。天玑定位是本机 Harness：每个问题都走 `agent_run`，按四档权限决定能做什么（`agent_policy.rs` 的 `tool_gate` / `offered_tools` 是唯一事实，档位字符串解析 fail-closed：未知值回落只读）：**只读权限**（默认）只能读文件、搜索、跑只读 git；**编辑权限**可直接改工作区内文件，其它命令、删除移动、启动或结束程序、系统设置（hosts / 隧道 / 运行时）先问用户，审批可选「本轮同类都允许」或「以后都允许」（同类按 `action_kind`：命令按程序名分，git 按子命令分，允许 `git add` 不等于允许 `git pull`；「以后都允许」记在**最细的已知范围**：有当前分支就写 `settings.agentAlwaysAllowByBranch`（仓库 → 分支 → kind，分支由 `git_current_branch` 问一次，切分支 / 天玑窗重新获得焦点 / 每轮跑完都会重查，detached HEAD 与非 git 目录回落到仓库层）、只有仓库写 `settings.agentAlwaysAllowByRepo`、都没有才写 `settings.agentAlwaysAllow`，每层上限 20 条，设置页可一键清空，只在 `Gate::Ask` 出现——审查档与完全权限不给，改前确认降下来的那一档也不给）；设置项 `settings.agentDiffConfirm` 开着时编辑档的 `write_file` / `edit_file` 先弹带 diff 的审批卡（`agent_policy::needs_diff_confirm`，拒绝即不落盘）；**审查托管**（EXP）先让 AI 审，只有明确放行才不问用户；**完全权限**不再询问，并且**可以访问整台电脑**（其它档位不出当前工作区；选整个工作区时根目录是工作区文件夹，见 `aiHub/repoSelection.ts` 的 `agentTarget`），必须由用户在菜单里主动选、首次二次确认，快捷键不会轮到它。模型标了「支持看图」时，完全权限还提供截屏和鼠标键盘操作（`agent_computer.rs`，左上角紧急停止，天玑窗口不进截图，首次使用确认截图会发给模型服务商）。任何档位都不放开：Git 冲突交给用户（仓库合并中 / 有未合并路径时，git 写操作与改文件一律拒绝并提示去 Git 管理 → 提交页，`agent.rs` 的 `conflict_block`；含冲突标记的文件不让 AI 改；`-X`/`-s` 选边、`checkout --ours/--theirs`、强推、`rebase`、`stash drop/clear` 在黑名单里），危险命令黑名单（`command_blocked`，git 部分按子命令和参数判定：`agent_policy::git_call_blocked`）、受保护路径（`agent_fs::Protected`：Windows、Program Files、本应用数据目录、任何 `.git` 目录不能写；磁盘根目录和用户主目录不能删 / 移）、删除一律进回收站。**等用户确认时不要静默失败**：审批 5 分钟超时自动拒绝，所以 `agent.rs` 的 `emit_approval` 会 `request_user_attention` 闪任务栏（天玑窗 `skip_taskbar`，主窗一起闪）、payload 带 `deadlineAt`，前端答案滚出视野时吸一条审批条；「等确认时唤回窗口」是用户主动开的 localStorage `qa-approval-focus`（默认关，只在 `useAskStream` 里调既有 `show_ai_hub`，不改 `bubble.rs`）。代理改过工作区（写文件 / git 写操作）后 `aiHub/config.ts` 的 `stepsMutatedWorkspace` 会作废 120s 上下文缓存，避免下一问还带着改动之前的 diff；缓存条目另外带一份 `workspace_fingerprint` 摘要（`agent_freshness.rs`），`useAskContext` 在 TTL 内命中前先比对，编辑器保存 / 外部 `git checkout` 也会触发重采。长任务由模型自己的 `update_plan` 工具产出计划卡（`agent-plan` 事件 → `AiHubTurn.plan` → `AgentPlanCard`，全档位可用的纯展示工具，不占动作步数），多轮上下文由 `agent_budget::fit_context` 每轮压到 48k（工具结果 → 最老轮次 → 系统提示，三级）。2026-10-23（M4）又加了四样：`symbols`（启发式定义定位，全档可用，`agent_symbols.rs`）、`web_fetch`（只认 https + `settings.agentWebAllow` 白名单，白名单空则连工具都不下发，跳转逐跳重过白名单（≤3 跳），读/编辑档 Ask，`agent_web.rs`）、`task`（只读子代理，16 轮 / 800 字结论 / 不能嵌套也不能写，默认关，`settings.agentSubagents` 打开才下发）和「撤销这次改动」（`agent_undo.rs`：写文件前快照到 `%APPDATA%\com.ai-workbench.app\agent-snapshots\<requestId>\`，保留 20 轮 / 50MB，`emit_done` 带 `undoToken` → `AiHubTurn.undoToken` → 答案下方按钮）。MCP 客户端（`mcp.rs`）：设置页「天玑 → MCP 服务」里配的服务（表 `mcp_servers`）只把启用且填全的随 `agent_run` 下发，后端每轮开场为每个服务起一次进程列工具（`cmd /c <command> <args>`，换行分隔 JSON-RPC，`initialize` → `tools/list`，一次调用一个进程、用完即关，服务失败只让这一轮少几个工具），工具名 `mcp_<server>_<tool>`，派发靠本轮列到的 (server, tool) 表查找；闸门是 `tool_gate` 的前缀规则——完全权限 `Allow`、审查 `Review`、其余一律 `Ask`（工具能力由服务决定，不能沿用"未知工具即放行"）。截图和文字上下文一样只发给所选模型服务商，不落盘、不上传密钥。
- **明确不做**：多平台（仅 Windows）、插件浏览器深度开发。
- 平台：仅 Windows 10/11
- 命名：产品叫 **AI Workbench**，里面的 AI 助手 2026-10-23 由「智枢」改名 **天玑**（中文文案、文档、错误提示统一用「天玑」；英文侧仍是 `AI Hub`）。代码标识不变：窗口标签 `ai-hub` / `ai-hub-bubble`、路由 `#/ai-hub`、目录 `src/components/aiHub/`、设置键 `aiHub*`、事件 `agent-*`。旧文档、旧 commit 里出现的「智枢」指的就是天玑。
- 标识：`com.ai-workbench.app`，当前版本 `0.2.34`
- 用户数据：`%APPDATA%\com.ai-workbench.app\ai-workbench.db`（SQLite；`ai_models.api_key` 落库用 Windows DPAPI `enc:v1:` 前缀，导出 JSON 仍为明文并有警告；勿打包进安装包）
- 侧栏分组：AI 工作台（Harness/协同办公/工作暂存/模板片段/模型管理/快捷应用；顺序只由 `App.tsx` 的 `NAV_NODES` 决定，2026-10-21 把 `desktop-apps` 上移到 Harness 之后（同一轮由「桌面应用」改名「协同办公」，接管本机应用窗口并嵌入，文案走 `navigation.json` + `desktopApps.json`，id 与路由 `#/desktop-apps` 未变），并把 `app-launcher` 从系统工具移进来放在末尾（id、路由、`.devtools-tool` 尺寸来源都没变）；2026-10-22 模型管理（同日由「模型配置」改名，labelKey `modelConfig` 未变；页面 Tab 随之改为「模型列表 / 模型对比」，数量徽标移到列表 Tab 上、页面大标题移除）先是移到工作暂存之后，短暂移进账号资产分组后又移回 AI 工作台，最终顺序在工作暂存之后、快捷应用之前；页内含场景模型绑定、本机 Ollama/LM Studio 探测；天玑另有 Profile（内置 + 自定义 systemRole / temperature，存 `settings.aiHubProfiles`）/ 轻附件（选择器给真实文件名，也能从资源管理器拖进来，超 8000 字符会明示）/ `/` 片段（焦点留在输入框、打字即过滤 + `↑↓` 选择），用量台账在设置页（天玑的「下一步提示词」预测可用 `settings.aiHubSuggest` 关掉）；**提示词页已从侧栏与路由下线**，`PromptOptimizer.tsx` 与 `lib/promptOptimize.ts` 保留，恢复只需在 `App.tsx` 加回 Tab / `NAV_NODES` / 渲染与 `import`；天玑里的「优化提示词」不受影响）→ 账号资产（Cursor/账号配额；WorkBuddy 页 2026-10-22 已整体删除（页面壳 `WorkBuddyManager.tsx`/`.css` 与仅被它引用的 `AccountPoolTable`/`CheckinPanel`/`ProtocolPanel`），`components/workbuddy/` 只剩账号配额页复用的 `ScanAddFlow`/`ManualAddModal`/`CredentialForm` + `WbModal.css`，`lib/workbuddy.ts` 与双语 `workbuddy.json` 保留；扫码加号、签到、自动签到设置都在账号配额页）→ 工程管理（Git 管理/环境变量）→ 网络管理（网页工具/内网穿透/Hosts 管理）→ 系统工具（工具箱 `devtools`，2026-10-21 快捷应用移走后只剩它一个）→ 设置。快捷应用页面根节点带 `.devtools-tool`，与工具箱页共用尺寸来源（`App.tsx` 的 `NAV_NODES` 是唯一事实，文案以 `src/locales/*/navigation.json` 为准），页内只剩应用网格一个视图。**「端口查杀」已于 2026-10-21 从快捷应用页搬进工具箱**，是 `group.system` 分组下的第 10 个工具（`src/components/devtools/PortKillerTool.tsx`，后端 `port_commands.rs` 的 `find_port_listeners` / `kill_port_listeners`）——工具箱其余 9 个仍然是纯前端、零后端命令，它是唯一带命令的那个；因此它不进天玑的工具面板（那一栏刻意只嵌零命令的纯前端工具）
- **网页工具-脚本（2026-10-23）**：预设模板体系升级——内置模板（`SCRIPT_PRESETS` 写死在 `PluginBrowser.tsx`，SkyWalking 大脚本来自 `scripts/skywalking-log-query.user.js` 经 `?raw` 注入，**v7.1.2 起液态玻璃质感浅色/深色双主题**）之外新增**用户自定义模板**：SQLite 表 `script_templates`（name/description/match_patterns/code，走 `db_load`/`db_save` 白名单），面板分组「内置模板 / 我的模板」，卡片点击=用模板新建、hover 操作组=分享（导出 JSON）/编辑/删除；脚本编辑弹窗底部「存为模板」一键把当前表单内容存成模板；`···` 菜单含模板导入/导出（JSON `kind: "ai-workbench-script-templates"`，按名称+代码判重合并）。头部操作区按**容器宽度渐进收纳**（`.plugin-header` 是 `container-type: inline-size` 容器，`.header-flat-btn[data-flat=…]` 由 `plugin-tabs.css` 的 `@container` 断点逐级放出：≥720 导入/导出、≥900 从网址导入/模板导入导出、≥1080 全部平铺并隐藏 `···`），**预设模板按钮常驻不收纳**

## 技术栈与目录

| 层 | 技术 |
|---|---|
| 前端 | React 18 + TypeScript 5.5 + Vite 5 |
| 后端 | Tauri v2 + Rust（rusqlite bundled） |
| 状态 | Zustand 5（业务数据→SQLite，settings→localStorage key `workbench-settings`） |
| i18n | i18next（`src/locales/zh-CN` / `en-US`） |
| 样式 | 纯 CSS（`src/styles/` 按域分片，`index.css` 汇总，`data-theme` 驱动） |

```
src/                  # React 前端
  App.tsx             # 主容器 + 侧栏导航（gitMounted/dshMounted 保活）
  main.tsx            # hash 路由：默认主窗 / #/ai-hub / #/ai-hub-bubble
  core/               # store.ts（全局状态）、types.ts、storage.ts、boot.ts、constants.ts
  components/         # 单文件单组件，页面级组件 + 各类 Modal
  lib/                # 业务工具（theme、browser、webTools、pluginHotkeys…）
  hooks/              # useTauriEvent、useInvokeError
src-tauri/src/        # Rust 后端
  lib.rs              # Tauri Builder + invoke_handler 注册 + setup（建表/迁移/托盘）
  *_commands.rs       # 按域拆分的 IPC 命令（git/report/ai/dsh/hosts/cursor/cloudflared/plugin/
                      # runtime/tool/db/cancellation/app_launcher/port/update/pet_pack…）；共 245 条注册
                      # （计数口径：`generate_handler!` 列表里 `模块::命令,` 的行；含
                      # `workspace_fingerprint`（2026-10-23 M2：工作区新鲜度摘要，见 `agent_freshness.rs`）/
                      # `agent_undo`（2026-10-23 M4：撤销一整轮的写文件，见 `agent_undo.rs`）/
                      # `set_ai_hub_bubble_box`（2026-10-24 桌宠改造：桌宠窗尺寸运行时化，
                      # 见「架构约定 8」）/
                      # `pet_pack_commands` 五条（2026-10-24 自备形象：挑图 / 读图 / 落盘 / 读目录 / 清除，
                      # 素材只进应用数据目录，见「架构约定 8」④）/
                      # `detect_local_ai_endpoints` / `regenerate_test_report_ai_section` /
                      # `git_fetch_branch` / `git_publish_branch`（2026-10-22 批量建分支用：前者只推进
                      # 一个远端跟踪引用，后者在 origin 上建分支、不碰工作区）/
                      # `git_pending_commits`（2026-10-22 推送预览用：只读，列出这次推送会带走的提交
                      # 与每条改动的文件，对比范围三档由 `pending_commit_scope` 判定）；
                      # 原 `devtools_commands.rs` 随 HTTP 客户端一起删除，该文件已不存在）
  纯函数层             # git_merge.rs：合并 / 变基状态、冲突列表、diff3 分段与冲突解决命令（冲突只交给用户）；
 # change_report.rs：Git 报告的模块 / 层级 / 风险 / 缺测 / API 判定 +
                      # AI 分块与幻觉校验；report_commands.rs 里的提示词构造函数
                      # （requirement_block / output_rules / build_ai_*）——可单测、不含 IO；
                      # agent_policy.rs：天玑四档权限（tool_gate / action_kind / 工具列表 / 策略提示词 / 审查结论解析）；
                      # agent_stream.rs：代理回合 SSE 切行、流式 <think> 过滤、Anthropic SSE 折叠回非流式 body；
                      # agent_budget.rs：token 估算与三级裁剪（工具结果 → 最老轮次 → 系统提示），
                      # 每轮 `model_turn` 前由 `fit_context` 压到 48k；agent_freshness.rs：工作区指纹
                      # （git HEAD + status + dirty mtime；非 git 走封顶 3000 文件的 mtime/size 遍历）；
                      # agent_symbols.rs：`symbols` 启发式定义定位（关键字 + 名字，复用 agent_fs::walk）；
                      # agent_web.rs：`web_fetch`（https + 域名白名单 + 跳转逐跳重过白名单 + html 转文本）；
                      # agent_undo.rs：写文件前的逐字节快照与撤销（保留 20 轮 / 50MB，命令 `agent_undo`）；
                      # mcp.rs：MCP 客户端（stdio + 换行 JSON-RPC，一次调用一个进程，工具名 `mcp_<server>_<tool>`）；
                      # agent_fs.rs（路径范围 / 受保护路径 / 文件工具 / diff）、agent_proc.rs（cmd / PowerShell /
                      # 后台进程）、agent_sys.rs（系统与工具箱能力）、agent_computer.rs（截屏 / 键鼠）
  cursor/ tray/       # Cursor 多身份、系统托盘与天玑窗
  codebuddy/          # WorkBuddy 域：adapter.rs（上游协议全 JSON 化，端点不写死）+
                      # credential.rs（token/cookie 解析、JWT exp、掩码）
  wb_*.rs             # WorkBuddy Manager：wb_commands（账号池）/ wb_checkin（签到 + 调度 loop）/
                      # wb_capture（扫码登录窗 + 凭证回传）。反代网关/密钥/IP 管控已移除（2026-09-27）
docs/                 # CODE_WIKI.md（架构百科）、cloudflared.md、各计划文档
```

新后端命令必须同时改三处：命令实现文件 → `lib.rs` 的 `mod` + `invoke_handler` 注册 → 前端 `src/core/store/invocations.ts`（或直接 invoke 处）。

## 常用命令

```bash
npm install
npm run dev              # 仅前端 Vite（无桌面壳）
npm run tauri -- dev     # 完整桌面开发
npm run build            # tsc && vite build（本项目唯一的类型检查入口，无独立 lint）
npm run tauri -- build   # 发布构建（NSIS 安装包）
cargo test --manifest-path src-tauri/Cargo.toml   # Rust 后端单测
```

改完前端跑 `npm run build` 验证类型；改完 Rust 跑 `cargo test`（在 `src-tauri/` 下）。

`npm run build` 若报 `[vite:esbuild-transpile] remove C:\...\Temp\esbuild-xxx: Access is denied`，不是代码问题：本机（杀软 / 文件锁）不让 esbuild 删系统 Temp 里的临时文件。把临时目录指到工作区再跑即可（`.build-tmp/` 已存在且不入库）：

```powershell
$env:TEMP = (Resolve-Path .build-tmp).Path; $env:TMP = $env:TEMP; npm run build
```

## 主题与视觉风格

设计系统在 `src/styles/`：`tokens.css`（全部主题块与 token，**字面色值只该出现在这里**）、`theme-glass.css`（磨砂主题共享覆盖层）、`base.css`（含 `body.resizing` 清单）、`layout.css`（标题栏/侧栏/全局滚动条）、`components.css` / `controls.css` / `modal.css` 等通用件，其余按页面一域一文件（`git-pages.css`、`ai-hub.css`、`devtools.css`…）。`index.css` 按原级联顺序 `@import` 全部分片，**同优先级规则靠源顺序决胜，调整 import 顺序等于改样式**；新页面加分片就追加在末尾。主题由 `html[data-theme]` 驱动，类型 `AppTheme = 'light' | 'dark' | 'system' | 'glass' | 'ice' | 'silver'`（默认 `glass`）。

| 主题 | 关键字 | 风格 |
|---|---|---|
| `light` | 默认浅色 · 白雾磨砂 | 白雾半透明背景 + slate 蓝 accent `#356a9e` |
| `dark` | 深色 | 深底高对比 |
| `glass` | 香槟金 Pearl Champagne | 默认主题；卡片磨砂（`.card` 有 `backdrop-filter`） |
| `ice` | 冰蓝玻璃 | 深底 + 冰蓝 accent `#7EB6D9` + 径向渐变光斑 |
| `silver` | 冷银玻璃 | 深底 + 冷银 accent `#B8C0CC` + 径向渐变光斑 |
| `system` | 跟随系统 | 移除 `data-theme` 属性 |

**Token 约定**（新增颜色/间距一律用 token，禁止散落硬编码色值）：

- 品牌：`--accent` / `--accent-strong` / `--accent-dim` / `--accent-border` / `--on-accent`
- 语义色（略降饱和）：`--green` / `--red` / `--yellow` 及对应 `*-dim` / `*-border`；需要别的透明度用 `color-mix(in srgb, var(--red) 14%, transparent)`，不要写 rgba 字面量
- 叠加层：`--overlay-1..4`（hover / 分隔 / 选中等叠在表面上的浅层，深色主题是白、浅色主题是墨蓝，约 3% / 5% / 8% / 12%）；需要自定透明度的阴影写 `rgba(var(--ink-rgb), a)`
- 背景层级：`--bg-0`（常为 transparent）/ `--bg-1` / `--bg-2` / `--bg-3` / `--bg-hover` / `--input-bg`
- 文字四级：`--text-0`（主）/ `--text-1` / `--text-2` / `--text-3`（最弱）
- 边框：`--border` / `--border-subtle`；圆角 `--radius-sm|md|lg|xl|full`；间距 `--space-1..8`
- 阴影：`--shadow-xs|sm|md|lg`、`--card-shadow`、`--surface-highlight`（卡片 / 次要按钮的 1px 顶边内高光，与投影用逗号叠加）、`--chrome-bg` / `--chrome-line`（标题栏/侧栏 chrome，**每个主题块都要定义**，漏了会回退到浅色的半透明白）
- 圆角刻度 sm 5 / md 8（控件）/ lg 12（卡片）/ xl 16（弹框）/ full（胶囊）；只有 1–2px 的细节圆角可以写 px
- 有意写死的颜色（Windows 关闭键红 `#e81123`、DEV 徽标琥珀、厂商品牌色、原生 `select option` 实底）就地注释原因；`npm run check:css`（`scripts/check-css-tokens.mjs`）会拦 `transition: all`、写死的蓝色、刻度内的 px 圆角、未定义变量和多余的 `var(--x, 回退)`，行尾 `/* token-ok: 原因 */` 可豁免。它不挂在 `build` 上，改样式后手动跑

**视觉语言**：磨砂/半透明分层（玻璃主题）、极轻阴影、发丝边框、`Inter`（正文，`--font-sans`，基础字号 12.5px，`tabular-nums`）+ `JetBrains Mono`（代码 `--font-mono`）；图标统一 `lucide-react`；玻璃主题下侧栏/标题栏/卡片走 `:is([data-theme="glass"], [data-theme="ice"], [data-theme="silver"])` 共享覆盖层（`theme-glass.css`）。

**主题应用**：`src/lib/theme.ts` 的 `applyDocumentTheme()` 写 `data-theme`；`system` 时移除属性。多窗口（天玑）通过 localStorage `workbench-settings` + `app-theme-changed` 事件同步。新主题色必须在 `tokens.css` 各主题块补齐全部 token，不能只改一处。

**Logo**：母版 `docs/brand/logo-silver.png`（冷银）/ `logo-ice.png`（冰蓝）；打包图标 `src-tauri/icons/`；侧栏 `src/assets/logo-silver.png`。

## 项目规范

### 代码组织

- **组件**：`src/components/` 单文件单组件（PascalCase `.tsx`）；页面级组件与 Modal 平铺；弹窗外壳统一用 `ModalTitleRow`（Esc/关闭/busy 规则）+ 确认框走 `ConfirmModal` 的 `useConfirm` / `useConfirmChoice`（不要手写 `window.confirm`）。
- **状态**：业务数据一律经 Zustand `store.ts` → `storage.ts`（`db_load`/`db_save`）持久化；仅 UI 偏好（settings）走 localStorage persist，键 `workbench-settings`。乐观更新用 `optimisticUpdate`，invoke 封装用 `tauriInvoke` / `withTable`。
- **IPC 透传**：前端调用优先经 `src/core/store/invocations.ts`（薄封装），不要在组件里散落裸 `invoke` 字符串重复定义。
- **Rust**：按域拆 `*_commands.rs`；注册三件套缺一不可（实现 → `lib.rs` `mod` + `invoke_handler` → 前端 invocations）。错误结构化用 `config.rs` 的 `ErrorCode` / `WorkbenchError`。
- **命名**：TS camelCase / 接口 PascalCase；Rust snake_case；DB 列 snake_case；Tauri 命令 snake_case 动词开头（`git_commit`、`run_test`）；事件 kebab-case（`generate-text-chunk`、`test-run-output`）。

### 交互与 UX

- 天玑/托盘/全局快捷键等多窗口行为以 `交接文档.md`「天玑窗要点」为准，不信任窗口缓存可见性。
- 长任务必须可取消（`CancelGuard`）并有实时进度事件；报告侧是 `test-report-collect-progress`（多仓采集）与 `test-report-ai-progress`（AI 分块）。
- **任何「Enter 即执行」的输入框，先挡输入法组合**：`if (e.nativeEvent.isComposing || e.keyCode === 229) return;`——否则拼音选词的回车会把半截内容发送或提交。已覆盖天玑提问框、片段搜索、历史重命名、参数填充。
- 危险操作（删账号、覆盖 Hosts、批量推拉）必须二次确认并给出影响范围列表。
- **设置页是「左栏分类 + 右栏卡片」**：分类由 `Settings.tsx` 的 `SETTINGS_TABS` 决定（常规设置 / 菜单配置 / 天玑助手 / 联网扩展 / 数据用量 / 关于版本：外观两项相邻、能力两项相邻，中文统一四个字，窄窗下导航变成顶部横向一行），卡片用 `hidden={tab !== id}` **挂起而不是卸载**——切分类时网页白名单输入框、MCP 新增行的草稿不能丢。新增设置项时先挂到对应分类的卡片里：行与行之间靠 `.card-body > .setting-row + .setting-row` 的分隔线（行下面带说明的成套项要包一层 `.setting-item`——分隔线画在项上，否则夹了 `p.setting-hint` 邻接规则就断档；只有一行没说明的项也用它，节奏才一致），卡片内再分组就用 `<h3 className="settings-group">`（天玑助手的卡内页签是三个：交互权限 / 助手能力 / 预算兼容——「权限」两项目前并进交互，单独一页太薄；助手能力放「并发读取 / 自动重试 / 搜索时跳过 .gitignore / 只读子代理」，预算兼容放「上下文预算 / 工具结果上限 / Anthropic 流式」，MCP 总开关放在联网扩展）。**桌面宠物（开关 / 形象 / 导入素材 / 大小 / 安静模式）自成一张卡**，挂在「常规设置」页，不跟天玑助手混在一起。除交互权限那几项（含 `agentDiffConfirm` / `agentAlwaysAllow`）外都是 `AppSettings` 里的 `agent*` 项，前端经 `AiHubApp` 映射成 `AgentRunRequest` 的同名字段，后端在 `ToolCtx` 上取 `max_parallel` / `max_retries` / `grep_ignore` / `result_cap`，`context_tokens` 直接喂给 `fit_context`。**「菜单配置」那一栏不是卡片**：它直接挂 `MenuManager`（原侧栏「系统工具 → 菜单管理」，2026-10-23 搬进设置并改名），组件自带吸顶工具条与分组卡，外层用 `.settings-pane` + `hidden` 包一层即可，别再套 `.card`。侧栏项与 `Tab` 联合类型在 `lib/navStructure.tsx`，删掉那一项后用户存下的 `navCustom` 里的旧 id 会被 `applyNavCustomization` 自然忽略。
- 空态、加载态、错误态要区分（如 Git 提交页区分「筛选内无改动」vs「真正无改动」）。
- 文案中文为主、通过 i18n 双语；数字/代码/路径用等宽字体展示。

### 质量门槛

- 前端：`npm run build`（tsc）零错误；无独立 ESLint 配置。改了 CSS 再跑 `npm run check:css`。
- 后端：`cargo test`（单测内联 `#[cfg(test)] mod tests`，纯函数层优先覆盖解析/判定逻辑）。
- 新增带 `backdrop-filter` 的 UI：同步加入 `body.resizing` 关闭清单。
- 提交信息风格：`feat:` / `fix:` / `refactor:` + 中文简述（见 `git log`）。

## 架构约定

1. **IPC 读写表只走 `db_load` / `db_save`**（Rust 侧表白名单，整表全量写）。Git 报告**本体不落库**：采集结果只存在 `report_commands.rs` 的进程内缓存（`REPORT_CACHE`，上限 16 条 FIFO），AI 步骤靠 `reportId` 取回同一份数据。唯一持久化的是 **AI 结论历史**（`report_history.rs` 的 `report_ai_history`，上限 50 条 FIFO）。这个区分是有意的：报告是可重算的投影，AI 结论要花一次模型往返，删了就得重跑。
2. **Schema 迁移**：`CREATE TABLE IF NOT EXISTS` 不会给已有库加列；加列必须走 `pragma_table_info` 判存在再 `ALTER`，或 `lib.rs` setup 里的幂等 `ALTER TABLE` 行。
3. **命名映射**：TypeScript 用 camelCase，SQLite 用 snake_case，`storage.ts` 负责双向转换。
4. **三层分层（报告类模块）**：纯函数（serde/标准库，`change_report.rs`）→ 命令层（tauri、起 git 进程、发事件，`report_commands.rs`）。新逻辑优先落在纯函数层以便 `cargo test`。
5. **AI 提示词格式只有一份实现**：section 列表与逐行格式由 `report_commands::output_rules()` 生成，前端 `src/lib/reportAi.ts` 的 `parseAiLine()` 负责解析回来——改用例行格式必须同时改这两处（后端单测 `output_rules_only_add_the_coverage_section_with_an_ask` 钉住了契约）。
6. **取消机制**：AI 流式（天玑）用 `request_id`，报告 AI 用 `reportId` 作 token，共用 `CancelGuard`（`cancellation.rs`）。
7. **页面保活**：Git / DSH 面板用 `gitMounted` / `dshMounted` + `.page-panel.is-active`，切走隐藏不卸载。
8. **桌宠（`ai-hub-bubble`）四条事实**：① 情绪状态机 `PetMood`（`lib/petSkins.ts`）只有七个值，改名要三处一起改——`lib/petBrain.ts` 的事件映射表、`components/aiHub/PetStage.tsx` 的 FX 层、`styles/ai-hub-bubble.css` 的 `mood-*` 规则。② 桌宠窗尺寸是**运行时值**（`settings.aiHubPetSize`，96/112/128/160；builder 用区间 `BUBBLE_BOX_MIN..MAX_W/MAX_H` 而不是钉死 min==max，否则 `set_size` 会被夹回原形），改尺寸与「就地菜单」撑高都走 `set_ai_hub_bubble_box`，**底边中心锚定**（`bottom_center_anchor`，高只往头顶长），前端 mount 时必须按「位置 → 尺寸 → `ai_hub_bubble_ready`」的顺序调用，否则非默认档位第一帧会跳。③ 桌宠不接自己的后端通道：`app.emit` 是广播，它直接 `listen` `agent-step / agent-plan / agent-approval / generate-text-*` 演状态；形象 / 尺寸 / 安静模式由主窗设置页经 `ai-hub-pet-changed` 事件通知（另一个 webview 的 store 不会自动刷新），广播 payload 里的 `packRev` 变了它才去重读素材目录。④ **四种皮肤与素材来源**：`PetSkinId` = `panda`（默认）/ `artisan`（电玩小匠）/ `orb`（水晶圆盘）/ `custom`（自备包）；前两个是本仓库自己生成的原创帧。**内置形象的帧与节拍表都在 `src/assets/pets/<名字>/`**：唯一姿势 PNG + `timeline.json`（`{"fps":12,"clips":{"情绪":[["姿势文件",连续格数],…]}}`，由一次性生成脚本产出：脚本不进仓库、产物进仓库）。`PetStage` 是定频轮播，所以「同一张图连续占几格」就是停顿——节奏全在这张表里，运行时不加新概念（`lib/petSkins.ts` 的 `builtInSkin()` 负责展开，两个形象共用这条装配）。**别把每个姿势钉成一格**：那样眨眼和敲键盘同速，看着呆；idle 要 6s 一轮、眨眼约 3s 一次才自然。作画网格定 128 是因为它必须 ≥ 最大显示尺寸（160 档图片是 0.82×160≈131 CSS px）——64 网格在那儿要被放大两倍，发丝描边就糊成方块。角色四周留 8 格空白，否则耳朵顶到窗口边、和定位在 `top:10~12%` 的情绪特效（zzz、思考点）打架。**custom** 走 `%APPDATA%\com.ai-workbench.app\pets\custom\<情绪>[-序号].png|webm`（文件名前缀即情绪、缺的情绪退回 idle），导入走 `pet_pack_commands.rs` 五条命令 + `lib/petPack.ts` 归一化管线：抠背景（四角主色，容差外一圈按混色反解回前景本色，否则角色外会套一圈背景毛边）→ 裁到紧包围盒 → **全部塞进同一边长的正方形、脚贴底、水平居中**（缩放比不一致角色就忽大忽小，基线不一致切情绪就跳位）；**自备包是空的时 `petSkin("custom")` 回落到内置熊猫**，而不是给一扇看不见的窗。曾经内置过的 GIF（皮卡丘/机甲等）与自绘矢量形象（麻薯/超级玛丽）都已下线。**红线只针对第三方美术**：`src/assets/` 不收来路不明的角色——2026-10-24 实测调研：颜值能打的开源桌宠几乎都不可随包分发（`PC2005-cloud/dsh-pet` 代码 MIT 但素材写明「禁止商用」且角色是 CC BY-NC-SA，`WindowPet` 内置角色是原神/宝可梦盗图，`VPet`/`BongoCat`/`Taromati2`/Sprout Lands 非商用或需另行授权），与本仓库 MIT 冲突；CC0 素材（`luizmelo/pet-cat-pack`、`styloo/dog2d`、Kenney）虽然可商用，但要走浏览器会话才能从 itch.io 取。**本仓库自己生成的原创帧不受此限**（熊猫与电玩小匠都是这么来的）。**但「借题材」不等于「抄设计」**：2026-10-24 有人要求照《王者荣耀》鲁班七号「电玩小子」皮肤做桌宠，那是复刻受保护的角色设计（发型、帽徽、服装色块分布、机甲纹样），已拒绝；改成只做「爱打电玩的小工匠」这个类型 —— 侧戴棒球帽、头戴耳机、蓝夹克配紫袖、别扳手、跟一台小无人机，配色与版型自己定（就是 `artisan`）。换别人的形象仍走导入，素材许可由导入者承担。

## 样式硬约定（窗口 resize 性能）

主窗无边框 + 自定义标题栏，resize 时整篇 CSS 重栅格化：

- **禁止 `transition: all`**，禁止把布局属性（width/padding/margin/font-size/gap…）放进 transition。只用 `--transition-colors-fast` / `--transition-colors`（仅 color/border/shadow/opacity/transform）。
- 唯一例外：`.sidebar` 折叠宽度、`.dsh-progress-fill`；`App.tsx` resize 期间给 `body` 加 `.resizing` 临时关闭。
- **新增带 `backdrop-filter` 的界面必须加进 `src/styles/base.css` 的 `body.resizing :is(...)` 清单**。
- 拖拽区用 `data-tauri-drag-region`（不是 Electron 的 `-webkit-app-region`）。

## 页面尺寸与自适应约定（大小窗口都要好看）

- **页面内容宽度只有一个来源**：`tokens.css` 的 `:root { --page-max-width: 1440px }`，由 `.page-scrollable`（≥1400 视口的宽屏块）、`.snippets-page`、`.drafts-page`、`.runtime-page`、`.models-container`、`.git-manager-column`、工具页 `.devtools-tool`、网页工具页 `.plugin-browser` 共同引用。**新页面不要再写自己的 `max-width` 常量**（历史上 960 / 1080 / 1440 三套并存，导致各页占比忽高忽低）。**只有 `.page-scrollable` 自带居中**（`responsive.css` 在 ≥1400 给它 `margin: auto`）；像网页工具页那样列表自己滚动、直接挂在 `.page-in` 下的整页，必须自己补 `margin-inline: auto`，否则宽窗口下 1440 的内容贴左、右边空一整条（2026-10-21 修的就是这个）。卡片网格已定：快捷应用 260 / 模板片段与工作暂存 320 / 模型配置与环境变量 340。
- **弹框宽度只有三档**：`tokens.css` 的 `--modal-sm`（400，默认 `.modal` / 确认框）/ `--modal-md`（560）/ `--modal-lg`（640，`92vw` / `88vh`）。在 `modal.css` 用语义类挂档（如 `.models-modal` → lg、`.drafts-modal` → md）；也可显式加 `.modal-md` / `.modal-lg`。快捷应用 `.modal-card` / `.modal-card-wide` 对齐 sm / lg。**域样式不要再写独立 px 宽度。**
- **卡片列表用 `repeat(auto-fill, minmax(min(100%, <N>px), 1fr))`，不要用 `@media` 切 `1fr 1fr`**。已定：快捷应用 260 / 模板片段与工作暂存 320 / 模型配置与环境变量 340。断点写死列数在宽屏下会把卡片拉到 700px 高、内容飘在顶部。
- **输入/输出区靠 flex 长高填满窗口**：`.dt-content` 是 flex 列，工具根节点 `flex: 1 0 auto`（长高但不低于自然高，内容多时页面照常滚动），主区 `flex: 1 1 auto; min-height: <原 px>`；原来的 `max-height: 280/340/360/420px`、`60vh` 等死上限改成 `min-height` 地板。有意不填满的只有：时间戳（单行 input）、HTTP 参数/请求头表格——撑开只会得到空盒子。
- **textarea 高度交给容器，统一 `resize: none`**（只删声明会让浏览器回退成 `resize: both`，反而能横向拖）。
- **吸顶工具条**：`position: sticky; top: 0; z-index` 之外，滚动宿主必须 `padding-top: 0`（间距改由吸顶元素 `margin-top` 提供），否则内容会从宿主那圈上内边距里露在吸顶条上方。磨砂表面用 `--chrome-bg` + `blur(20px) saturate(1.08)`（与侧栏同配方），不要 `color-mix(--bg-1 …, transparent)`——`--bg-1` 在磨砂主题下本身就只有 3.5%~4.5% alpha，二次稀释等于没有底色。
- 布局改动落点：验证别只信截图——内嵌浏览器视口只有 ~590 CSS px（dpr=2），量宽度用 `getBoundingClientRect()` / `scrollWidth` 反推，或注入临时 `<style>` 强制容器宽度；往 React 容器里手工 `appendChild` 假数据不可靠，组件状态刷新会把非它创建的节点抹掉。

## 发布红线

- **开发标识零泄漏**：`[DEV]` 标题/托盘前缀、前端 DEV 徽标必须是编译期常量（`cfg!(debug_assertions)` / `import.meta.env.DEV`），禁止改成运行时开关。
- **版本号三处一致**：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`（`Cargo.toml` 最容易漏，决定 exe 文件属性版本）。
- `src-tauri/src/bin/` 内部诊断工具由 `dev-tools` feature 门控，默认不进安装包。
- 不把 `%APPDATA%\com.ai-workbench.app\` 用户数据打进安装包。
- **应用内升级依赖资产命名**：release 里的 NSIS 资产名必须以 `_x64-setup.exe` 结尾（`update_commands.rs` 的 `pick_setup_asset` 按此后缀匹配，MSI 认 `.msi`）。改名 = 升级弹窗静默退化成「去浏览器下载」，不会报错，务必发布前核对。仓库名同时写在 `src/lib/version.ts` 的 `APP_GITHUB_REPO` 与 `src-tauri/src/update_commands.rs` 的 `GITHUB_REPO`，改一处必须改两处。

## 危险操作提示

- 改 Cursor 逻辑：`%APPDATA%\Cursor` 是单一数据源，删除/替换链接时**只摘链、不跟随递归删**；SQLite 同步必须保留原生类型（BLOB 误写会导致 Cursor 黑屏，已有 `repair_blob_typed_state` 兜底）。
- 天玑窗：**不能在命令线程建窗**（builder 挂住）；可见性统一走 `tray::bubble::vis::{really_visible, force_visible}`，不要信任 tao 缓存；前端隐藏走 `invoke("hide_ai_hub")`。
- Hosts 写入需管理员，走 `.tmp` + `ReplaceFileW` 原子替换，写前自动备份。
- Cloudflared token 不进命令行，写临时 YAML 用 `--config` 传路径。
- DSH 认证补丁会改本机 DeepSeek 安装文件（有 `.ai-workbench-backup` 备份与还原命令）。

## i18n 与文档

- UI 文案必须同时补 `src/locales/zh-CN/*.json` 与 `en-US/*.json`，按域分文件（如 `plugins.json`）。
- 改功能时同步更新 `README.md` / `交接文档.md` / `ROADMAP.md` 中对应描述（仓库有「文档与代码一致」约定）。

## 参考文档

| 文件 | 内容 |
|---|---|
| `README.md` | 产品功能、打包、隐私说明 |
| `docs/CODE_WIKI.md` | 架构、DB 表结构、IPC 命令清单（2026-09-30 已与代码核对：版本号、工具箱命令条数、目录树；仍可能有滞后，以代码为准） |
| `交接文档.md` | 历史状态、Cursor/天玑等踩坑记录（必读） |
| `ROADMAP.md` | 阶段规划 |
| `docs/cloudflared.md` | 内网穿透专题 |
