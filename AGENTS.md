# AGENTS.md

面向 AI 编码代理的项目指南。改代码前先读本文件；深入细节再看 `docs/CODE_WIKI.md` 与 `交接文档.md`。

## 项目定位

**已确认定位（见 `ROADMAP.md`）**：**AI 驱动的开发工具箱**——以 **DeepSeek 本地服务**为核心 AI 引擎，Git / Cursor 身份管理为基础能力，AI 模型配置与系统工具（Hosts / 内网穿透等）为辅助。

- **愿景**：本地优先的桌面 AI 助手；多模型配置（提示词独立页已下线，智枢优化仍可用），管理开发者的 Git/Cursor 多身份与本机隧道/Hosts 等上下文。**所有数据留在本机，不上传密钥**。
- **功能分层**：核心 = DSH / 模型配置 / Cursor（提示词独立页已下线，代码保留）；基础 = Git 多仓（仓库 / 提交 / 报告）/ 运行时切换；辅助 = Hosts / Cloudflared / 网页工具；系统工具 = 工具箱（devtools）。
- **已移除的功能，不要再去找**：自动化测试执行 / 测试用例管理 / 覆盖率 / 漏洞扫描整套能力已删除，其历史文档（`test-assistant-*.md`）也已清理，相关数据表在 `lib.rs` setup 中被 `DROP TABLE`。Git 报告页是**静态分析 + 按需 AI**：只读 git diff，不执行测试、不产出覆盖率。若需查阅旧实现，`git log -- docs/test-assistant-summary.md` 仍可回溯（最后出现在 `4a03f74`）。工具箱页的 **HTTP 客户端**同样已整体删除：`HttpClientTool.tsx` / `devtools_commands.rs`（该域只剩这一条命令，文件一起删了）/ `http_saved_requests` + `http_history` 两张表（setup 里 `DROP TABLE`）/ devtools i18n 的 `http.*` 与 `group.network`，工具箱现在是 9 个（转换 / 生成 / 解析 / 计算四组）。
- **AI 能力在范围内**：智枢流式对话、仓库工具代理、报告与提交里的模型调用都可以继续做。智枢定位是本机 Harness：每个问题都走 `agent_run`，按四档权限决定能做什么（`agent_policy.rs` 的 `tool_gate` / `offered_tools` 是唯一事实）：**只读权限**（默认）只能读文件、搜索、跑只读 git；**编辑权限**可直接改仓库内文件，其它命令、删除移动、启动或结束程序、系统设置（hosts / 隧道 / 运行时）先问用户，审批可选「本轮同类都允许」（同类按 `action_kind`：命令按程序名分，git 按子命令分，允许 `git add` 不等于允许 `git pull`）；**审查托管**（EXP）先让 AI 审，只有明确放行才不问用户；**完全权限**不再询问，并且**可以访问整台电脑**（其它档位不出当前工作区或仓库；选整个工作区时根目录是工作区文件夹，见 `aiHub/repoSelection.ts` 的 `agentTarget`），必须由用户在菜单里主动选、首次二次确认，快捷键不会轮到它。模型标了「支持看图」时，完全权限还提供截屏和鼠标键盘操作（`agent_computer.rs`，左上角紧急停止，智枢窗口不进截图，首次使用确认截图会发给模型服务商）。任何档位都不放开：Git 冲突交给用户（仓库合并中 / 有未合并路径时，git 写操作与改文件一律拒绝并提示去 Git 管理 → 提交页，`agent.rs` 的 `conflict_block`；含冲突标记的文件不让 AI 改；`-X`/`-s` 选边、`checkout --ours/--theirs`、强推、`rebase`、`stash drop/clear` 在黑名单里），危险命令黑名单（`command_blocked`，git 部分按子命令和参数判定：`agent_policy::git_call_blocked`）、受保护路径（`agent_fs::Protected`：Windows、Program Files、本应用数据目录、任何 `.git` 目录不能写；磁盘根目录和用户主目录不能删 / 移）、删除一律进回收站。截图和文字上下文一样只发给所选模型服务商，不落盘、不上传密钥。
- **明确不做**：多平台（仅 Windows）、插件浏览器深度开发。
- 平台：仅 Windows 10/11
- 标识：`com.ai-workbench.app`，当前版本 `0.2.8`
- 用户数据：`%APPDATA%\com.ai-workbench.app\ai-workbench.db`（SQLite；`ai_models.api_key` 落库用 Windows DPAPI `enc:v1:` 前缀，导出 JSON 仍为明文并有警告；勿打包进安装包）
- 侧栏分组：AI 工作台（Harness/模型配置/模板片段/工作暂存；模型配置页内含「配置列表 / 模型对比」与场景模型绑定、本机 Ollama/LM Studio 探测；智枢另有 Profile / 轻附件 / `/` 片段，用量台账在设置页；**提示词页已从侧栏与路由下线**，`PromptOptimizer.tsx` 与 `lib/promptOptimize.ts` 保留，恢复只需在 `App.tsx` 加回 Tab / `NAV_NODES` / 渲染与 `import`；智枢里的「优化提示词」不受影响）→ 账号资产（Cursor/账号配额；WorkBuddy 页已从侧栏与路由下线，`WorkBuddyManager.tsx` 及 `components/workbuddy/` 保留，恢复只需在 `App.tsx` 加回 Tab / `NAV_NODES` / 渲染三处；扫码加号、签到、自动签到设置都已在账号配额页）→ 工程管理（Git 管理/环境变量）→ 网络管理（网页工具/内网穿透/Hosts 管理）→ 系统工具（工具箱 / 快捷应用 `app-launcher`）→ 设置。快捷应用页面根节点带 `.devtools-tool`，与工具箱页共用尺寸来源（`App.tsx` 的 `NAV_NODES` 是唯一事实，文案以 `src/locales/*/navigation.json` 为准）。**该页有两个视图**（`.al-view-tabs` 切换，偏好存 localStorage `al-view`）：应用网格，以及「端口查杀」面板 `PortKillerPanel.tsx`（后端 `port_commands.rs` 的 `find_port_listeners` / `kill_port_listeners`）——工具箱页那 8 个仍然是纯前端、零后端命令，端口能力有意放在这一页

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
                      # runtime/tool/db/cancellation/app_launcher/port/update…）；共 218 条注册
                      # （计数口径：`generate_handler!` 列表里 `模块::命令,` 的行；含
                      # `detect_local_ai_endpoints` / `regenerate_test_report_ai_section`；
                      # 原 `devtools_commands.rs` 随 HTTP 客户端一起删除，该文件已不存在）
  纯函数层             # git_merge.rs：合并 / 变基状态、冲突列表、diff3 分段与冲突解决命令（冲突只交给用户）；
 # change_report.rs：Git 报告的模块 / 层级 / 风险 / 缺测 / API 判定 +
                      # AI 分块与幻觉校验；report_commands.rs 里的提示词构造函数
                      # （requirement_block / output_rules / build_ai_*）——可单测、不含 IO；
                      # agent_policy.rs：智枢四档权限（tool_gate / action_kind / 工具列表 / 策略提示词 / 审查结论解析）；
                      # agent_stream.rs：代理回合 SSE 切行与流式 <think> 过滤；
                      # agent_fs.rs（路径范围 / 受保护路径 / 文件工具 / diff）、agent_proc.rs（cmd / PowerShell /
                      # 后台进程）、agent_sys.rs（系统与工具箱能力）、agent_computer.rs（截屏 / 键鼠）
  cursor/ tray/       # Cursor 多身份、系统托盘与智枢窗
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

**主题应用**：`src/lib/theme.ts` 的 `applyDocumentTheme()` 写 `data-theme`；`system` 时移除属性。多窗口（智枢）通过 localStorage `workbench-settings` + `app-theme-changed` 事件同步。新主题色必须在 `tokens.css` 各主题块补齐全部 token，不能只改一处。

**Logo**：母版 `docs/brand/logo-silver.png`（冷银）/ `logo-ice.png`（冰蓝）；打包图标 `src-tauri/icons/`；侧栏 `src/assets/logo-silver.png`。

## 项目规范

### 代码组织

- **组件**：`src/components/` 单文件单组件（PascalCase `.tsx`）；页面级组件与 Modal 平铺；弹窗外壳统一用 `ModalTitleRow`（Esc/关闭/busy 规则）+ 确认框走 `ConfirmModal` 的 `useConfirm` / `useConfirmChoice`（不要手写 `window.confirm`）。
- **状态**：业务数据一律经 Zustand `store.ts` → `storage.ts`（`db_load`/`db_save`）持久化；仅 UI 偏好（settings）走 localStorage persist，键 `workbench-settings`。乐观更新用 `optimisticUpdate`，invoke 封装用 `tauriInvoke` / `withTable`。
- **IPC 透传**：前端调用优先经 `src/core/store/invocations.ts`（薄封装），不要在组件里散落裸 `invoke` 字符串重复定义。
- **Rust**：按域拆 `*_commands.rs`；注册三件套缺一不可（实现 → `lib.rs` `mod` + `invoke_handler` → 前端 invocations）。错误结构化用 `config.rs` 的 `ErrorCode` / `WorkbenchError`。
- **命名**：TS camelCase / 接口 PascalCase；Rust snake_case；DB 列 snake_case；Tauri 命令 snake_case 动词开头（`git_commit`、`run_test`）；事件 kebab-case（`generate-text-chunk`、`test-run-output`）。

### 交互与 UX

- 智枢/托盘/全局快捷键等多窗口行为以 `交接文档.md`「智枢窗要点」为准，不信任窗口缓存可见性。
- 长任务必须可取消（`CancelGuard`）并有实时进度事件；报告侧是 `test-report-collect-progress`（多仓采集）与 `test-report-ai-progress`（AI 分块）。
- 危险操作（删账号、覆盖 Hosts、批量推拉）必须二次确认并给出影响范围列表。
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
6. **取消机制**：AI 流式（智枢）用 `request_id`，报告 AI 用 `reportId` 作 token，共用 `CancelGuard`（`cancellation.rs`）。
7. **页面保活**：Git / DSH 面板用 `gitMounted` / `dshMounted` + `.page-panel.is-active`，切走隐藏不卸载。

## 样式硬约定（窗口 resize 性能）

主窗无边框 + 自定义标题栏，resize 时整篇 CSS 重栅格化：

- **禁止 `transition: all`**，禁止把布局属性（width/padding/margin/font-size/gap…）放进 transition。只用 `--transition-colors-fast` / `--transition-colors`（仅 color/border/shadow/opacity/transform）。
- 唯一例外：`.sidebar` 折叠宽度、`.dsh-progress-fill`；`App.tsx` resize 期间给 `body` 加 `.resizing` 临时关闭。
- **新增带 `backdrop-filter` 的界面必须加进 `src/styles/base.css` 的 `body.resizing :is(...)` 清单**。
- 拖拽区用 `data-tauri-drag-region`（不是 Electron 的 `-webkit-app-region`）。

## 页面尺寸与自适应约定（大小窗口都要好看）

- **页面内容宽度只有一个来源**：`tokens.css` 的 `:root { --page-max-width: 1440px }`，由 `.page-scrollable`（≥1400 视口的宽屏块）、`.snippets-page`、`.drafts-page`、`.runtime-page`、`.models-container`、`.git-manager-column`、工具页 `.devtools-tool` 共同引用。**新页面不要再写自己的 `max-width` 常量**（历史上 960 / 1080 / 1440 三套并存，导致各页占比忽高忽低）。卡片网格已定：快捷应用 260 / 模板片段与工作暂存 320 / 模型配置与环境变量 340。
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
- 智枢窗：**不能在命令线程建窗**（builder 挂住）；可见性统一走 `tray::bubble::vis::{really_visible, force_visible}`，不要信任 tao 缓存；前端隐藏走 `invoke("hide_ai_hub")`。
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
| `交接文档.md` | 历史状态、Cursor/智枢等踩坑记录（必读） |
| `ROADMAP.md` | 阶段规划 |
| `docs/cloudflared.md` | 内网穿透专题 |
