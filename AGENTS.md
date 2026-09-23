# AGENTS.md

面向 AI 编码代理的项目指南。改代码前先读本文件；深入细节再看 `docs/CODE_WIKI.md` 与 `交接文档.md`。

## 项目定位

**已确认定位（见 `ROADMAP.md`）**：**AI 驱动的开发工具箱**——以 **DeepSeek 本地服务**为核心 AI 引擎，Git / Cursor 身份管理为基础能力，AI 模型配置与系统工具（Hosts / 内网穿透等）为辅助。

- **愿景**：本地优先的桌面 AI 助手；多模型配置、提示词优化，管理开发者的 Git/Cursor 多身份与本机隧道/Hosts 等上下文。**所有数据留在本机，不上传密钥**。
- **功能分层**：核心 = DSH / 模型配置 / 提示词 / Cursor；基础 = Git 多仓（仓库 / 提交 / 报告）/ 运行时切换；辅助 = Hosts / Cloudflared / 网页工具；系统工具 = 小工具（devtools）。
- **已移除的功能，不要再去找**：自动化测试执行 / 测试用例管理 / 覆盖率 / 漏洞扫描整套能力已删除，其历史文档（`test-assistant-*.md`）也已清理，相关数据表在 `lib.rs` setup 中被 `DROP TABLE`。Git 报告页是**静态分析 + 按需 AI**：只读 git diff，不执行测试、不产出覆盖率。若需查阅旧实现，`git log -- docs/test-assistant-summary.md` 仍可回溯（最后出现在 `4a03f74`）。
- **明确不做**：内置多模型流式对话（AI 走 DeepSeek 外部集成）、AI 工具执行循环（shell/文件/Git 自动操作）、多平台（仅 Windows）、插件浏览器深度开发。
- 平台：仅 Windows 10/11
- 标识：`com.ai-workbench.app`，当前版本 `0.1.8`
- 用户数据：`%APPDATA%\com.ai-workbench.app\ai-workbench.db`（SQLite，密钥明文，勿打包进安装包）
- 侧栏分组：AI 工作台（Harness/模型配置/AI 提示词/片段库）→ 账号管理（Cursor）→ 版本管理（Git 管理/环境变量）→ 网络管理（Hosts/内网穿透/网页工具）→ 系统工具（小工具）→ 设置（文案以 `src/locales/*/navigation.json` 为准）

## 技术栈与目录

| 层 | 技术 |
|---|---|
| 前端 | React 18 + TypeScript 5.5 + Vite 5 |
| 后端 | Tauri v2 + Rust（rusqlite bundled） |
| 状态 | Zustand 5（业务数据→SQLite，settings→localStorage key `workbench-settings`） |
| i18n | i18next（`src/locales/zh-CN` / `en-US`） |
| 样式 | 纯 CSS（`src/styles.css`，`data-theme` 驱动） |

```
src/                  # React 前端
  App.tsx             # 主容器 + 侧栏导航（gitMounted/dshMounted 保活）
  main.tsx            # hash 路由：默认主窗 / #/quick-ask / #/quick-ask-bubble
  core/               # store.ts（全局状态）、types.ts、storage.ts、boot.ts、constants.ts
  components/         # 单文件单组件，页面级组件 + 各类 Modal
  lib/                # 业务工具（theme、browser、webTools、pluginHotkeys…）
  hooks/              # useTauriEvent、useInvokeError
src-tauri/src/        # Rust 后端
  lib.rs              # Tauri Builder + invoke_handler 注册 + setup（建表/迁移/托盘）
  *_commands.rs       # 按域拆分的 IPC 命令（git/report/ai/dsh/hosts/cursor/cloudflared/plugin/
                      # devtools/runtime/tool/db/cancellation…）
  纯函数层             # change_report.rs：Git 报告的模块 / 层级 / 风险 / 缺测 / API 判定 +
                      # AI 分块与幻觉校验；report_commands.rs 里的提示词构造函数
                      # （requirement_block / output_rules / build_ai_*）——可单测、不含 IO
  cursor/ tray/       # Cursor 多身份、系统托盘与快问窗
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

设计系统集中在 `src/styles.css`（约 1.3 万行），主题由 `html[data-theme]` 驱动，类型 `AppTheme = 'light' | 'dark' | 'system' | 'glass' | 'ice' | 'silver'`（默认 `glass`）。

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
- 语义色（略降饱和）：`--green` / `--red` / `--yellow` 及对应 `*-dim`
- 背景层级：`--bg-0`（常为 transparent）/ `--bg-1` / `--bg-2` / `--bg-3` / `--bg-hover` / `--input-bg`
- 文字四级：`--text-0`（主）/ `--text-1` / `--text-2` / `--text-3`（最弱）
- 边框：`--border` / `--border-subtle`；圆角 `--radius-sm|md|lg|xl|full`；间距 `--space-1..8`
- 阴影：`--shadow-xs|sm|md|lg`、`--card-shadow`、`--chrome-bg` / `--chrome-line`（标题栏/侧栏 chrome）

**视觉语言**：磨砂/半透明分层（玻璃主题）、极轻阴影、发丝边框、`Inter`（正文，`--font-sans`，基础字号 12.5px，`tabular-nums`）+ `JetBrains Mono`（代码 `--font-mono`）；图标统一 `lucide-react`；玻璃主题下侧栏/标题栏/卡片走 `:is([data-theme="glass"], [data-theme="ice"], [data-theme="silver"])` 共享覆盖层（约 L392 起）。

**主题应用**：`src/lib/theme.ts` 的 `applyDocumentTheme()` 写 `data-theme`；`system` 时移除属性。多窗口（快问）通过 localStorage `workbench-settings` + `app-theme-changed` 事件同步。新主题色必须在 `styles.css` 各主题块补齐全部 token，不能只改一处。

**Logo**：母版 `docs/brand/logo-silver.png`（冷银）/ `logo-ice.png`（冰蓝）；打包图标 `src-tauri/icons/`；侧栏 `src/assets/logo-silver.png`。

## 项目规范

### 代码组织

- **组件**：`src/components/` 单文件单组件（PascalCase `.tsx`）；页面级组件与 Modal 平铺；弹窗外壳统一用 `ModalTitleRow`（Esc/关闭/busy 规则）+ 确认框走 `ConfirmModal` 的 `useConfirm` / `useConfirmChoice`（不要手写 `window.confirm`）。
- **状态**：业务数据一律经 Zustand `store.ts` → `storage.ts`（`db_load`/`db_save`）持久化；仅 UI 偏好（settings）走 localStorage persist，键 `workbench-settings`。乐观更新用 `optimisticUpdate`，invoke 封装用 `tauriInvoke` / `withTable`。
- **IPC 透传**：前端调用优先经 `src/core/store/invocations.ts`（薄封装），不要在组件里散落裸 `invoke` 字符串重复定义。
- **Rust**：按域拆 `*_commands.rs`；注册三件套缺一不可（实现 → `lib.rs` `mod` + `invoke_handler` → 前端 invocations）。错误结构化用 `config.rs` 的 `ErrorCode` / `WorkbenchError`。
- **命名**：TS camelCase / 接口 PascalCase；Rust snake_case；DB 列 snake_case；Tauri 命令 snake_case 动词开头（`git_commit`、`run_test`）；事件 kebab-case（`generate-text-chunk`、`test-run-output`）。

### 交互与 UX

- 快问/托盘/全局快捷键等多窗口行为以 `交接文档.md`「快问窗要点」为准，不信任窗口缓存可见性。
- 长任务必须可取消（`CancelGuard`）并有实时进度事件；报告侧是 `test-report-collect-progress`（多仓采集）与 `test-report-ai-progress`（AI 分块）。
- 危险操作（删账号、覆盖 Hosts、批量推拉）必须二次确认并给出影响范围列表。
- 空态、加载态、错误态要区分（如 Git 提交页区分「筛选内无改动」vs「真正无改动」）。
- 文案中文为主、通过 i18n 双语；数字/代码/路径用等宽字体展示。

### 质量门槛

- 前端：`npm run build`（tsc）零错误；无独立 ESLint 配置。
- 后端：`cargo test`（单测内联 `#[cfg(test)] mod tests`，纯函数层优先覆盖解析/判定逻辑）。
- 新增带 `backdrop-filter` 的 UI：同步加入 `body.resizing` 关闭清单。
- 提交信息风格：`feat:` / `fix:` / `refactor:` + 中文简述（见 `git log`）。

## 架构约定

1. **IPC 读写表只走 `db_load` / `db_save`**（Rust 侧表白名单，整表全量写）。Git 报告**本体不落库**：采集结果只存在 `report_commands.rs` 的进程内缓存（`REPORT_CACHE`，上限 16 条 FIFO），AI 步骤靠 `reportId` 取回同一份数据。唯一持久化的是 **AI 结论历史**（`report_history.rs` 的 `report_ai_history`，上限 50 条 FIFO）。这个区分是有意的：报告是可重算的投影，AI 结论要花一次模型往返，删了就得重跑。
2. **Schema 迁移**：`CREATE TABLE IF NOT EXISTS` 不会给已有库加列；加列必须走 `pragma_table_info` 判存在再 `ALTER`，或 `lib.rs` setup 里的幂等 `ALTER TABLE` 行。
3. **命名映射**：TypeScript 用 camelCase，SQLite 用 snake_case，`storage.ts` 负责双向转换。
4. **三层分层（报告类模块）**：纯函数（serde/标准库，`change_report.rs`）→ 命令层（tauri、起 git 进程、发事件，`report_commands.rs`）。新逻辑优先落在纯函数层以便 `cargo test`。
5. **AI 提示词格式只有一份实现**：section 列表与逐行格式由 `report_commands::output_rules()` 生成，前端 `src/lib/reportAi.ts` 的 `parseAiLine()` 负责解析回来——改用例行格式必须同时改这两处（后端单测 `output_rules_only_add_the_coverage_section_with_an_ask` 钉住了契约）。
6. **取消机制**：AI 流式（快问）用 `request_id`，报告 AI 用 `reportId` 作 token，共用 `CancelGuard`（`cancellation.rs`）。
7. **页面保活**：Git / DSH 面板用 `gitMounted` / `dshMounted` + `.page-panel.is-active`，切走隐藏不卸载。

## 样式硬约定（窗口 resize 性能）

主窗无边框 + 自定义标题栏，resize 时整篇 CSS 重栅格化：

- **禁止 `transition: all`**，禁止把布局属性（width/padding/margin/font-size/gap…）放进 transition。只用 `--transition-colors-fast` / `--transition-colors`（仅 color/border/shadow/opacity/transform）。
- 唯一例外：`.sidebar` 折叠宽度、`.dsh-progress-fill`；`App.tsx` resize 期间给 `body` 加 `.resizing` 临时关闭。
- **新增带 `backdrop-filter` 的界面必须加进 `styles.css` 的 `body.resizing :is(...)` 清单**。
- 拖拽区用 `data-tauri-drag-region`（不是 Electron 的 `-webkit-app-region`）。

## 发布红线

- **开发标识零泄漏**：`[DEV]` 标题/托盘前缀、前端 DEV 徽标必须是编译期常量（`cfg!(debug_assertions)` / `import.meta.env.DEV`），禁止改成运行时开关。
- **版本号三处一致**：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`（`Cargo.toml` 最容易漏，决定 exe 文件属性版本）。
- `src-tauri/src/bin/` 内部诊断工具由 `dev-tools` feature 门控，默认不进安装包。
- 不把 `%APPDATA%\com.ai-workbench.app\` 用户数据打进安装包。

## 危险操作提示

- 改 Cursor 逻辑：`%APPDATA%\Cursor` 是单一数据源，删除/替换链接时**只摘链、不跟随递归删**；SQLite 同步必须保留原生类型（BLOB 误写会导致 Cursor 黑屏，已有 `repair_blob_typed_state` 兜底）。
- 快问窗：**不能在命令线程建窗**（builder 挂住）；可见性统一走 `tray::bubble::vis::{really_visible, force_visible}`，不要信任 tao 缓存；前端隐藏走 `invoke("hide_quick_ask")`。
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
| `docs/CODE_WIKI.md` | 架构、DB 表结构、IPC 命令清单（部分版本号可能滞后，以代码为准） |
| `交接文档.md` | 历史状态、Cursor/快问等踩坑记录（必读） |
| `ROADMAP.md` | 阶段规划 |
| `docs/cloudflared.md` | 内网穿透专题 |
