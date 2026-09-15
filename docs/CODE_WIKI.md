# AI Workbench — Code Wiki

> 完整的项目代码知识百科，覆盖架构、模块、关键类函数、依赖关系与运行方式。

---

## 目录

- [1. 项目概述](#1-项目概述)
- [2. 技术栈](#2-技术栈)
- [3. 目录结构](#3-目录结构)
- [4. 整体架构](#4-整体架构)
- [5. 启动流程](#5-启动流程)
- [6. 前端核心模块](#6-前端核心模块)
- [7. 前端组件层](#7-前端组件层)
- [8. Rust 后端核心模块](#8-rust-后端核心模块)
- [9. 数据库设计](#9-数据库设计)
- [10. Tauri IPC 命令完整清单](#10-tauri-ipc-命令完整清单)
- [11. 依赖关系图](#11-依赖关系图)
- [12. 运行与调试](#12-运行与调试)
- [13. 打包与发布](#13-打包与发布)
- [14. 扩展指南](#14-扩展指南)

---

## 1. 项目概述

**AI Workbench** 是一个基于 **Tauri v2** 的 Windows 桌面应用，定位为 **AI 驱动的开发工具箱**。它将以下能力整合到一个统一界面中：

| 能力域 | 功能 |
|--------|------|
| **AI 工作台** | DeepSeek Harness 本地服务托管、多厂商模型配置、AI 快问（含流式 + 多轮）、提示词优化、代码片段管理 |
| **账号与 Git** | Cursor 多身份一键切换（独立 profile 目录）、Git 仓库批量管理 / 扫描 / 提交 |
| **开发环境** | Node.js / JDK 版本切换（修改注册表 PATH） |
| **系统工具** | 系统 Hosts 读写 + 自动备份恢复、Cloudflare Tunnel（临时 / 命名隧道）、网页工具快捷入口 |

产品标识：
- **identifier**: `com.ai-workbench.app`
- **当前版本**: `0.1.0`
- **默认 DeepSeek 端口**: `3080`

---

## 2. 技术栈

### 前端（React + TypeScript）

| 技术 | 版本 | 用途 |
|------|------|------|
| React | 18.3 | UI 框架 |
| TypeScript | 5.5 | 类型安全 |
| Vite | 5.4 | 构建工具 + 开发服务器 |
| Zustand | 5.0 | 状态管理（`persist` + JSONStorage） |
| i18next | 26.4 | 国际化（zh-CN / en-US） |
| lucide-react | 1.33 | 图标库 |
| @tauri-apps/api | 2.0 | Tauri 前端绑定 |
| @tauri-apps/plugin-store | 2.0 | 前端 kv 存储（仅 settings） |
| @tauri-apps/plugin-global-shortcut | 2.3 | 全局快捷键（快问 `Ctrl+Alt+K`） |

### 后端（Rust + Tauri）

| crate | 版本 | 用途 |
|-------|------|------|
| tauri | 2.x | 桌面框架（启用 `tray-icon`、`image-png`） |
| tauri-plugin-shell | 2 | Shell 命令执行 |
| tauri-plugin-store | 2 | Store 插件 |
| rusqlite | 0.33 (bundled) | SQLite 数据库 |
| serde / serde_json | 1 | 序列化 |
| serde_yaml | 0.9 | YAML 读写（DSH settings / Cloudflared config） |
| reqwest | 0.12 | HTTP 客户端（AI 模型请求、npm 查询） |
| tokio | 1 (full) | 异步运行时 |
| chrono | 0.4 | 时间戳格式化 |
| base64 / url / arboard / rand / encoding_rs | — | 基础工具 |
| windows | 0.61 | Windows API（`ReplaceFileW`、`CreateFileW` 等） |

---

## 3. 目录结构

```
ai-workbench/
│
├── src/                          # React 前端
│   ├── App.tsx                   # 主容器 + 侧边栏导航 + 标题栏
│   ├── main.tsx                  # 入口（hash 路由分发：主窗口 / 快问 / 快问气泡）
│   ├── styles.css                # 全局样式（data-theme 驱动的皮肤系统）
│   ├── tauri.d.ts / vite-env.d.ts
│   │
│   ├── core/                     # 核心业务层
│   │   ├── index.ts              # 统一导出
│   │   ├── boot.ts               # 启动引导（bootApp）
│   │   ├── store.ts              # Zustand 全局状态（最大文件 ~1000 行）
│   │   ├── store/helpers.ts      # optimisticUpdate / withTable / tauriInvoke
│   │   ├── store/invocations.ts  # 纯透传的 Tauri invoke 包装集合
│   │   ├── types.ts              # 所有实体 TypeScript 接口定义
│   │   ├── sqlite.ts             # DB 初始化占位（Rust 端完成真实初始化）
│   │   ├── storage.ts            # SQLite 适配层（db_load / db_save）
│   │   ├── constants.ts          # 前端魔法值集中地
│   │   ├── pathUtils.ts          # 路径规范化 / 比较 / 查找 workspace
│   │   └── cursorMatch.ts        # Cursor 账号匹配（email+name 对比）
│   │
│   ├── components/               # UI 组件层（全部无类型拆分，单文件单组件）
│   │   ├── AIAssistant.tsx       # 模型配置页（CRUD + 连接测试 + 批量操作 + 分组）
│   │   ├── DeepSeekHarness.tsx   # DSH iframe 托管 + 启动/停止/端口配置
│   │   ├── PromptOptimizer.tsx   # AI 提示词改写（调用 generate_text）
│   │   ├── SnippetsManager.tsx   # 代码片段 CRUD + JSON 导入导出
│   │   ├── QuickAskApp.tsx       # 快问主窗口（hash 路由 #/quick-ask）
│   │   ├── QuickAskBubble.tsx    # 快问桌面悬浮气泡（#/quick-ask-bubble）
│   │   ├── GitManager.tsx        # Git 管理主页面（仓库总览）
│   │   ├── GitCommitPanel.tsx    # Git 提交面板（多仓 diff + AI 生成说明）
│   │   ├── GitReposPage.tsx      # Git 仓库列表页
│   │   ├── CommitChangelist.tsx  # 文件变更列表组件
│   │   ├── CursorManager.tsx     # Cursor 多账号管理（切换 / 初始化 / 诊断）
│   │   ├── VersionSwitcher.tsx   # Node.js / JDK 版本切换
│   │   ├── CloudflaredManager.tsx# 内网穿透（临时 / 命名隧道 + 日志流）
│   │   ├── HostsManager.tsx      # 系统 Hosts 读写 + 备份恢复
│   │   ├── PluginBrowser.tsx     # 网页工具（WebView 打开 http(s) 链接）
│   │   ├── Settings.tsx          # 设置页（主题 / 语言 / 导出导入）
│   │   ├── ErrorBoundary.tsx     # React 错误边界
│   │   ├── ConfirmModal.tsx      # 确认对话框 + Provider（Context）
│   │   ├── AccountManagerModal.tsx     # Git 账号弹窗
│   │   ├── BatchIdentityModal.tsx      # 批量身份弹窗
│   │   ├── RepoBindingModal.tsx        # 仓库绑定弹窗
│   │   ├── ScanReposModal.tsx          # 扫描 Git 仓库弹窗
│   │   ├── RuntimeInstallModal.tsx      # 运行时安装弹窗
│   │   └── AppLogoMark.tsx             # Logo 组件
│   │
│   ├── hooks/                     # 自定义 React Hooks
│   │   ├── useTauriEvent.ts      # Tauri 事件订阅封装
│   │   └── useInvokeError.ts     # invoke 错误处理
│   │
│   ├── lib/                       # 业务工具库
│   │   ├── aiProviders.ts       # AI 厂商 provider 列表 + 显示元数据
│   │   ├── browser.ts            # 打开浏览器窗口（webview）
│   │   ├── floatingMenu.ts       # 悬浮菜单定位计算
│   │   ├── promptOptimize.ts     # 提示词构造器
│   │   ├── quickAskShortcut.ts   # 全局快捷键注册
│   │   ├── snippets.ts           # 片段 {{param}} 替换引擎
│   │   ├── theme.ts              # 主题应用（documentTheme + CSS 类切换）
│   │   └── version.ts            # 版本号语义比较
│   │
│   ├── i18n/                      # i18next 配置
│   │   ├── config.ts
│   │   └── locales/index.ts
│   │
│   └── locales/                   # 翻译文件
│       ├── zh-CN/*.json          # 中文
│       └── en-US/*.json          # 英文
│
├── src-tauri/                      # Rust 后端
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── icons/                     # 多平台图标
│   ├── capabilities/default.json
│   ├── src/
│   │   ├── main.rs                # 二进制入口（thin，调用 lib::run）
│   │   ├── lib.rs                 # Tauri Builder + invoke_handler + setup
│   │   │
│   │   ├── db_commands.rs         # SQLite db_load / db_save
│   │   ├── git_commands.rs        # 全部 Git 相关命令（status/commit/push/pull/diff/scan…）
│   │   ├── ai_commands.rs         # AI 模型连接测试 / 列表 / generate_text / generate_text_stream
│   │   ├── dsh_commands.rs        # DeepSeek Harness 生命周期（start/stop/install/update + auth patch）
│   │   ├── cloudflared_commands.rs# Cloudflare Tunnel（快速 / 命名 + 日志 + config.yml）
│   │   ├── hosts_commands.rs      # 系统 Hosts（读 / 写 / 备份 / 还原 / 原子替换）
│   │   ├── runtime_commands.rs    # Node.js / JDK 版本切换（修改注册表 PATH）
│   │   ├── plugin_commands.rs     # 网页工具（WebView navigate）
│   │   ├── tool_commands.rs       # 工具类：auto-start / 剪贴板 / 数据导入导出 / 文件对话框
│   │   ├── cancellation.rs        # AI 流式请求取消（CancelGuard）
│   │   ├── cancellation_commands.rs# cancel_request Tauri 命令
│   │   │
│   │   ├── cursor/                # Cursor 多身份模块
│   │   │   ├── mod.rs             # 所有 cursor_* Tauri 命令 + 核心备份/切换逻辑
│   │   │
│   │   ├── tray/                  # 系统托盘
│   │   │   ├── mod.rs             # 托盘图标菜单注册 + 快问窗口 + 气泡
│   │   │   └── bubble.rs          # 气泡 ready 事件
│   │   │
│   │   ├── config.rs              # Rust 侧常量 + ErrorCode 枚举 + WorkbenchError
│   │   └── bin/
│   │       └── cursor_switch_test.rs  # Cursor 切换集成测试
│   │
│   └── rules/                     # 代码检查规则
│       ├── default.golangci.yml
│       ├── default.luacheckrc
│       ├── default_checkstyle.xml
│
├── docs/                           # 项目文档
│   ├── brand/                     # Logo 母版
│   ├── cloudflared.md             # 内网穿透专题文档
│   ├── git-multi-repo-scan-plan.md
│   ├── modal-unify-plan.md
│   ├── workbench-upgrade-plan.md
│   └── CODE_WIKI.md               # 本文件
│
├── public/                         # 静态资源
├── scripts/                        # 工具脚本
├── docs/screenshot.png
├── README.md
├── LICENSE
└── package.json
```

---

## 4. 整体架构

### 4.1 分层模型

```
┌─────────────────────────────────────────────────────┐
│              用户操作层（Windows Desktop）            │
├─────────────────────────────────────────────────────┤
│                    ┌──────────┐                      │
│                    │   标题栏  │                      │
│                    │ (自定义)  │                      │
│          ┌─────────┴─────────┴──────────┐           │
│          │                               │           │
│    ┌─────┴──────┐              ┌─────────┴─────┐     │
│    │  侧边栏导航  │              │   主内容区      │     │
│    │ (折叠/分组)  │              │  (Tab 渲染)     │     │
│    └────────────┘              └───────────────┘     │
├─────────────────────────────────────────────────────┤
│                  React + Vite 前端                   │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────┐  │
│  │ Zustand   │→│  Store    │→│   storage.ts      │  │
│  │ 全局状态  │  │ (CRUD)    │  │ (db_load / save) │  │
│  └──────────┘  └──────────┘  └────────┬─────────┘  │
│                                        │              │
│                   Tauri `invoke` IPC ←─┘              │
├─────────────────────────────────────────────────────┤
│                 Tauri v2 Runtime                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────┐  │
│  │ 托盘图标  │  │ 全局快捷键│  │ 多窗口管理        │  │
│  └──────────┘  └──────────┘  └────────┬─────────┘  │
├─────────────────────────────────────────────────────┤
│                   Rust 后端                           │
│  ┌─────────────────────────────────────────────────┐ │
│  │  commands/*.rs  ──  Git │ AI │ DSH │ Cloudflared│ │
│  │                  Hosts │ Runtime │ Cursor │ Tray │ │
│  └───────────────────┬─────────────────────────────┘ │
│                      │                                │
│  ┌───────────────────┴─────────────────────────────┐ │
│  │ SQLite (rusqlite)   │  外部进程 (git / node /    │ │
│  │ %APPDATA%/.../     │          cloudflared /      │ │
│  │   ai-workbench.db  │          cursor.exe)        │ │
│  └───────────────────┴─────────────────────────────┘ │
├─────────────────────────────────────────────────────┤
│                 Windows 系统层                         │
│  Registry PATH │ System32/drivers/etc/hosts │ 进程管理 │
└─────────────────────────────────────────────────────┘
```

### 4.2 数据流（以 Cursor 切换为例）

```
用户点击「切换账号」
    │
    ▼
React: CursorManager.onSwitchAccount(id)
    │
    ▼
Store: switchCursorAccount(id) → invokeSwitchCursorAccount(...)
    │                                    │
    ▼                                    ▼
  optimisticUpdate              tauri.invoke('switch_cursor_account', {...})
    │                                    │
    │                          ┌─────────┴─────────┐
    │                          │  Rust  spawn_blocking│
    │                          │  1. 备份当前 Cookie  │
    │                          │  2. 恢复目标 Cookie   │
    │                          │  3. 重启 Cursor       │
    │                          └─────────┬─────────┘
    │                                    │
    ▼                                    ▼
 UI 立即更新                       持久化成功，乐观确认
```

---

## 5. 启动流程

```
App 启动
  │
  ├─1. Rust setup() hook (lib.rs)
  │     ├─ 创建 data_dir: %APPDATA%\com.ai-workbench.app\
  │     ├─ 打开 SQLite: ai-workbench.db
  │     ├─ DDL CREATE TABLE IF NOT EXISTS ...
  │     ├─ ALTER TABLE 字段迁移（增量）
  │     ├─ 后台 tokio::spawn: cursor shared-workspace migration
  │     ├─ global_shortcut 插件注册
  │     ├─ tray::init_tray()（托盘 + 快问窗口）
  │     └─ tray::ensure_quick_ask_bubble()
  │
  ├─2. Vite dev server / 静态资源 → Tauri 窗口加载 index.html
  │
  ├─3. React main.tsx 渲染
  │     ├─ ErrorBoundary 包裹
  │     ├─ hash 路由检测：
  │     │   #/quick-ask-bubble → QuickAskBubble
  │     │   #/quick-ask       → QuickAskApp
  │     │   默认               → App（主窗口）
  │     └─ bootApp() ← boot.ts
  │
  ├─4. bootApp() （StrictMode-safe，全局 memoize）
  │     ├─ initializeDatabase()  ← 目前是 no-op（Rust 端已完成）
  │     └─ useGlobalStore.getState().initialize()
  │           └─ Promise.all([loadAccounts, loadRepoConfigs, ...]) 共 10 项并行加载
  │
  ├─5. App.tsx useEffect 完成
  │     ├─ setIsReady(true)（显示主界面）
  │     ├─ registerQuickAskShortcut(settings.quickAskShortcut)
  │     ├─ applyDocumentTheme(settings.theme)
  │     ├─ i18n.changeLanguage(settings.language)
  │     └─ sync 快问气泡可见状态到 Rust
  │
  └─ 渲染侧边栏 + 主内容区
```

---

## 6. 前端核心模块

### 6.1 `core/types.ts` — 实体定义

所有核心实体接口集中在这一个文件中：

| 接口 | 用途 | 关键字段 |
|------|------|----------|
| `AppSettings` | 用户偏好 | theme, language, quickAskShortcut, quickAskChips, sidebarCollapsed |
| `GitAccount` | Git 身份 | name, email, color, note |
| `GitRepoConfig` | 仓库身份绑定 | path, userName, email, accountId |
| `GitWorkspace` | Workspace 分组 | path, name |
| `GitRepoSummary` | 仓库概览 | branch, dirtyCount, ahead, behind, hasUpstream |
| `GitStatusEntry` | 单文件状态 | path, indexStatus, workTreeStatus, group |
| `CursorAccount` | Cursor 账号 | email, backupPath, profileDir, profileInitialized, gitUserName |
| `AIModelConfig` | AI 模型配置 | provider, apiKey, baseUrl, model, temperature, maxTokens, isDefault |
| `CloudflaredNamedProfile` | 命名隧道 | hostname, localUrl, authMode, token/configPath |
| `Snippet` | 代码片段 | name, content, params, tags, useCount |
| `HostProfile` | Hosts 配置 | name, content |
| `WebPlugin` | 网页工具 | name, url, group, order, hotkey, isPreset |

**Provider 类型枚举**（`AIModelConfig.provider`）:
```typescript
'openai' | 'anthropic' | 'deepseek' | 'ollama' | 'longcat' | 'agnes'
| 'openrouter' | 'google' | 'groq' | 'mistral' | 'xai' | 'moonshot'
| 'mimo' | 'zhipu' | 'qwen' | 'siliconflow' | 'together' | 'custom'
```

### 6.2 `core/store.ts` — 全局状态中心

**文件长度**: ~1000 行，整个前端状态的单一真实来源。

**架构**:
```
zustand.create()
  ├─ persist 中间件 → localStorage 存 settings（key: "workbench-settings"）
  └─ 全部业务数据 → 通过 storage.ts → SQLite（Rust）
```

**State 结构**:
```typescript
GlobalState = {
  settings: AppSettings,
  git: { accounts: GitAccount[], repoConfigs: GitRepoConfig[] },
  recentProjects: RecentProject[],
  gitWorkspaces: GitWorkspace[],
  webPlugins: WebPlugin[],
  hostProfiles: HostProfile[],
  cursorAccounts: CursorAccount[],
  aiModels: AIModelConfig[],
  cloudflaredProfiles: CloudflaredNamedProfile[],
  snippets: Snippet[],
}
```

**关键方法分组**:

| 分组 | 方法 | 特点 |
|------|------|------|
| Settings | `setSettings` | merge 模式 |
| Git 身份 | `loadAccounts`, `addAccount`, `updateAccount`, `deleteAccount` | optimisticUpdate |
| Git 仓库 | `loadRepoConfigs`, `addRepoConfig`, `upsertRepoConfigs`, `updateRepoConfig`, `deleteRepoConfig` | pathKey 去重 |
| Cursor | `loadCursorAccounts`, `addCursorAccount`, `switchCursorAccount`, `deleteCursorAccount` | 调 Rust cursor 命令 |
| AI 模型 | `loadAIModels`, `addAIModel`, `updateAIModel`, `deleteAIModel`, `setDefaultAIModel` | 首项自动 default |
| Cloudflared | `loadCloudflaredProfiles`, `addCloudflaredProfile`, ... | — |
| Snippets | `loadSnippets`, `addSnippet`, `deleteSnippet`, `bumpSnippetUse` | 首次加载注入种子片段 |
| 持久化 | `initialize()` | Promise.all 并行加载 10 类数据 |
| DSH 状态 | `loadDshStatus`, `refreshDshStatus` | 缓存 nodejs_installed / dshVersion / hasUpdate |
| Cursor invoke | `invoke*` 系列 | 纯透传到 `store/invocations.ts` |

**辅助函数**:
- `withTable(table, fn)` — 锁表日志
- `optimisticUpdate(old, next, set, persist)` — 乐观更新模式
- `tauriInvoke<T>(cmd, args)` — 封装 tauri invoke + 错误处理
- `buildSeedSnippets(now)` — 内置 6 个种子片段

### 6.3 `core/storage.ts` — SQLite 适配层

```
storage.<domain>.load()  →  invoke('db_load', { table })  →  返回 Typed[]
storage.<domain>.save()  →  invoke('db_save', { table, rows })  →  void
```

**Table 类型**:
```typescript
type DbTable = 'git_accounts' | 'git_repo_configs' | 'host_profiles'
  | 'web_plugins' | 'plugin_states' | 'recent_projects'
  | 'git_workspaces' | 'cursor_accounts' | 'ai_models'
  | 'cloudflared_profiles' | 'snippets'
```

**命名约定**: TypeScript 用 camelCase (`createdAt`)，SQLite 用 snake_case (`created_at`)。该层完成双向映射。

### 6.4 `core/boot.ts` — 启动引导

```typescript
bootApp(): Promise<void>
```

**设计要点**:
- **StrictMode-safe**: React 双次 mount 不会重复初始化
- **多窗口安全**: 主窗口和快问窗口共享同一个 bootPromise
- **实际工作**: `initializeDatabase()` 目前是空函数（Rust setup 已完成建表），真正工作在 `store.initialize()`

### 6.5 `core/constants.ts` — 魔法值集中地

| 常量 | 值 | 用途 |
|------|-----|------|
| `DSH_DEFAULT_PORT` | 3080 | DeepSeek 默认端口 |
| `QUICKASK_CONTEXT_LIMIT` | 2000 | 快问上下文截断上限 |
| `QUICKASK_DIRTY_REPO_LIMIT` | 5 | 扫描脏仓库数 |
| `QUICKASK_CLIPBOARD_LIMIT` | 800 | 剪贴板截断 |
| `AI_DEFAULT_MAX_TOKENS` | 4096 | AI 默认 token |
| `RECENT_PROJECTS_LIMIT` | 200 | 最近项目上限 |

### 6.6 `core/pathUtils.ts` — 路径工具

| 函数 | 用途 |
|------|------|
| `normalizePath(path)` | 统一分隔符为 `/`，去尾部斜杠 |
| `pathKey(path)` | 用于比较的 key（normalize + lowercase） |
| `isPathUnder(child, parent)` | 严格的路径包含判断 |
| `findWorkspaceForRepo(repoPath, workspaces)` | 找最长匹配的 workspace |
| `projectNameFromPath(path)` | 从路径取最后一段 |

### 6.7 `lib/` — 业务工具库

| 模块 | 导出 | 功能 |
|------|------|------|
| `theme.ts` | `applyDocumentTheme(theme)` | 根据 AppTheme 设置 CSS 类（支持 glass/ice/silver/light/dark/system） |
| `quickAskShortcut.ts` | `registerQuickAskShortcut(shortcut)` | 调用 `registerShortcut` + navigate |
| `aiProviders.ts` | `AI_PROVIDERS: ProviderMeta[]` | provider 显示名 + 默认 baseUrl |
| `snippets.ts` | `renderSnippet(template, params)` | `{{param}}` 替换引擎 |
| `version.ts` | `compareVersions(a, b)` | 语义版本比较（含 prerelease） |
| `promptOptimize.ts` | — | 提示词构造器 |
| `browser.ts` | — | 打开 webview 窗口 |

### 6.8 `hooks/useTauriEvent.ts`

```typescript
useTauriEvent<T>(eventName, handler, deps)
```

封装 `listen<T>()` + 自动卸载。

### 6.9 `hooks/useInvokeError.ts`

invoke 错误的统一处理（提取 `ErrorCode` + 显示用户友好消息）。

---

## 7. 前端组件层

### 7.1 App.tsx — 主容器

**核心职责**:
- 侧边栏导航渲染（分组折叠 + 折叠模式自动合并）
- Tab 切换（Git / DeepSeek / 模型配置 / 提示词 / 片段 / Cursor / 版本 / Hosts / Cloudflared / 插件 / 设置）
- 窗口标题栏（最小化 / 最大化 / 关闭隐藏到托盘）
- 响应式自动折叠（< 720px 自动折叠，> 900px 自动展开）
- 两个懒加载面板（Git / DSH 首次进入后常驻 DOM）

**关键状态**:
```typescript
activeTab: Tab                // 当前激活页面
gitMounted: boolean           // Git 面板是否挂载
dshMounted: boolean           // DSH iframe 是否挂载
collapsed: boolean            // 侧边栏折叠
openGroups: Set<string>       // 分组展开状态
```

**特殊设计**:
- **关闭不退出**: `handleClose()` → `window.hide()` + emit tray-minimized
- **导航**: 通过 Tauri emit 注册 `navigate-tab` 事件，托盘菜单可跳转

### 7.2 核心业务组件

| 组件 | 功能 | 关键交互 |
|------|------|----------|
| `DeepSeekHarness` | iframe 托管 DSH | 启动/停止/端口/认证补丁状态 |
| `AIAssistant` | AI 模型配置 | CRUD + 连接测试 + 同步 DSH + 批量操作 + 分组视图 |
| `PromptOptimizer` | 提示词改写 | 调用 `generate_text` + system prompt 模板 |
| `SnippetsManager` | 代码片段管理 | `{{param}}` 编辑 + JSON 导入导出 |
| `QuickAskApp` | 快问主窗口（`#/quick-ask`） | 流式 + 多轮 + 上下文 chip + 片段选择器 |
| `QuickAskBubble` | 桌面气泡（`#/quick-ask-bubble`） | 快捷唤起，全局置顶 |
| `CursorManager` | Cursor 多账号 | 初始化 profile + 切换 + 诊断导出 |
| `GitManager` | Git 管理 | 仓库总览 / 扫描 / 身份 |
| `GitCommitPanel` | Git 提交 | 多仓 diff + AI 生成 commit message |
| `CloudflaredManager` | 内网穿透 | 临时隧道 / 命名隧道 / 日志流 / config.yml 编辑 |
| `HostsManager` | Hosts 管理 | 读写 + 原子替换 + 备份恢复 |
| `VersionSwitcher` | 运行时切换 | Node.js / JDK PATH 写入注册表 |
| `PluginBrowser` | 网页工具 | 自定义 WebView 窗口打开 http(s) |
| `Settings` | 设置 | 主题 / 语言 / 自启 / 快捷键 / 数据导出导入 |

### 7.3 弹窗组件

| 组件 | 用途 |
|------|------|
| `ConfirmModal` | Context Provider + useConfirm hook |
| `AccountManagerModal` | Git 账号弹窗（增删改） |
| `BatchIdentityModal` | 批量设置仓库身份 |
| `RepoBindingModal` | 仓库绑定 Git 账号 |
| `ScanReposModal` | 扫描目录找 Git 仓库 |
| `RuntimeInstallModal` | 安装新版本 Node/JDK |
| `AccountManagerModal` | Cursor 账号编辑弹窗 |

---

## 8. Rust 后端核心模块

### 8.1 `lib.rs` — 应用入口

**职责**: Tauri Builder 组装 + setup hook + invoke_handler 注册。

**State 管理**:
```rust
DbState { conn: Mutex<Connection> }        // SQLite 连接
DshState { instances: Mutex<Vec<DshInstance>> }  // DSH 实例追踪
CloudflaredState { sessions: Mutex<HashMap<String, TunnelSession>> }  // 隧道会话
TrayTunnelUrls { ... }                     // 托盘隧道 URL
```

**setup 流程**:
1. `app.path().app_data_dir()` → `create_dir_all`
2. `Connection::open(&db_path)` → 内联 `CREATE TABLE`（10 张表）
3. 逐行 `ALTER TABLE` 迁移（增量 schema）
4. `Mutex::new(conn)` → state 注入
5. 后台 `cursor::ensure_shared_workspace_migration()`
6. 注册 `global_shortcut` 插件
7. `tray::init_tray()` → 托盘图标菜单
8. `tray::ensure_quick_ask_window()`
9. `tray::set_quick_ask_bubble_visible_inner()`

**invoke_handler**: 约 **70+ 条命令**，完整清单见第 10 节。

### 8.2 `config.rs` — 常量与错误码

**核心常量**:
```rust
DSH_DEFAULT_PORT: u16 = 3080
TRAY_POLL_INTERVAL_SECS: u64 = 5
MODEL_TEST_TIMEOUT_SECS: u64 = 15
GENERATE_TEXT_TIMEOUT_SECS: u64 = 60
GENERATE_STREAM_TIMEOUT_SECS: u64 = 120
FILE_COPY_MAX_RETRIES: u32 = 8
CURSOR_SOFT_QUIT_TIMEOUT_MS: u64 = 8_000
```

**ErrorCode 枚举**（14 种）:
```rust
InvalidAccountId, CursorNotFound, CursorDbTooLarge, BackupIncomplete, AuthMissing,
AiModelNotConfigured, AiEmptyPrompt, AiEmptyResponse, AiRequestCancelled,
DshNotInstalled, DshStartFailed, DshInstallFailed, InvalidInput, InternalError
```

**WorkbenchError**: 结构化错误，`code: &'static str` + `detail: String`。

### 8.3 `db_commands.rs` — 通用 SQLite 层

**接口**:
```rust
db_load(state: DbState, table: DbTable) -> Result<Vec<SqlRow>, String>
db_save(state: DbState, table: DbTable, rows: Vec<serde_json::Value>) -> Result<(), String>
```

**设计**:
- **整表全量写**: `DELETE FROM table` → 逐行 INSERT（由前端决定写入顺序）
- **事务安全**: 使用 `unchecked_transaction`，中途失败自动 rollback
- **SQL 模板**: `load_sql(table)` 返回静态 `SELECT ... ORDER BY`
- **类型转换**: `row_to_map` 处理 Integer/Real/Text/Blob/Null → serde_json::Value

### 8.4 `git_commands.rs` — Git 操作

**全部基于 `Command::new("git")` + `creation_flags(CREATE_NO_WINDOW)`**。

**关键实现**:

| 函数 | 说明 |
|------|------|
| `git_repo_summary` | `rev-parse HEAD` + `status --porcelain` + `rev-list @{upstream}` 聚合 |
| `git_status` | porcelain 解析，拆 staged / unstaged / untracked 三组 |
| `git_diff` | staged → `diff --cached`; untracked → `diff --no-index /dev/null`; 截断 200KB |
| `git_commit` | 先 `has_staged_changes` 检查，友好化错误文案 |
| `git_push/pull` | 检测 upstream，自动 `-u origin HEAD` 首次推送 |
| `git_scan_repos` | 递归扫描 + 跳过 node_modules/target/.hidden + max_depth 限制 |
| `set_git_config` | 全局/仓库级 user.name / user.email |

**Porcelain 解析** `parse_porcelain_line`:
- 处理引号路径 + octal escape（Git 对 unicode 文件名的处理）
- 区分 staged-only / unstaged-only / both（如 `MM` 生成两条）

### 8.5 `ai_commands.rs` — AI 模型调用

**支持两种协议**: OpenAI Chat Completions + Anthropic Messages。

| 命令 | 特点 |
|------|------|
| `test_model_connection` | 发 `Hello` 测通，返回 HTTP 状态 |
| `list_provider_models` | `GET {base}/models` 解析 data 数组 |
| `generate_text` | 单次请求，60s 超时 |
| `generate_text_stream` | SSE 流式，emit chunk/done/error 事件；支持 CancelGuard |

**流式事件**:
```rust
app.emit("generate-text-chunk", { id, text })
app.emit("generate-text-done",   { id })
app.emit("generate-text-error",  { id, error })
```

**认证**:
- OpenAI-compatible: `Authorization: Bearer {api_key}`
- Anthropic: `x-api-key` + `anthropic-version`
- Mimo: `api-key` header

### 8.6 `dsh_commands.rs` — DeepSeek Harness

这是**最复杂**的模块之一。

**关键数据结构**:
```rust
DshInstance { pid, port, auth_url?, auth_patch_warning? }
DshState { instances: Mutex<Vec<DshInstance>> }
```

**启动流程** `start_dsh(port)`:
1. `prune_dead_instances` — 清理已死实例
2. 端口占用检查 → 如已被 DSH 服务接管则 adopt
3. 定位 DSH 安装（`npm root -g` / `node_modules` / 直接 node 启动入口）
4. `spawn_dsh_server` — 传 `--trusted-host 127.0.0.1`
5. `wait_for_port` — 30s 超时轮询
6. HTTP 探测 → `401` → 应用认证补丁 → 重启 → 再探测

**认证补丁** `patch_dsh_auth`:
- 目标: `dsh-client-connection/lib/index.js`
- 备份: 原文件 → `.js.ai-workbench-backup`
- 替换: `browserAuth.isAuthenticated` → `return void 0`
- Sentinel: `/* ai-workbench:dsh-auth-bypass */` 标记已补丁
- 幂等: 多次调用安全

**停止** `stop_dsh(port)`:
- 先从追踪列表移除
- `netstat -ano | find port` 找 PID → `taskkill /T /F /PID`
- `wait_for_port_release` 确保端口释放

### 8.7 `cloudflared_commands.rs` — 内网穿透

**State**:
```rust
CloudflaredState { sessions: Mutex<HashMap<String, TunnelSession>> }
TunnelSession { id, pid, mode, local_url, public_url?, profile_id?, child }
```

**隧道模式**:
- **Quick** (`cloudflared tunnel --url local_url`) — 临时，自动分配 `.trycloudflare.com`
- **Named** (`cloudflared tunnel --config run`) — 命名，需要 `--url` 或 config.yml ingress

**关键安全设计**:
- **Token 不进命令行**: 写入临时 YAML config → `--config` 传路径（避免 Windows 任务管理器泄露）
- **日志 reader**: `stdout`/`stderr` 独立线程读取 → emit `cloudflared-log` 事件 + 解析 `.trycloudflare.com` URL
- **进程存活**: `try_wait()` 定期 reap

**config.yml 编辑**: `patch_ingress_text` 纯文本 patch，保留注释格式，Insert catch-all 之前。

### 8.8 `cursor/mod.rs` — Cursor 多身份

**核心机制**:
- 每个账号独立 `--user-data-dir`（profile 目录）
- 切换时: 备份当前 profile → 恢复目标 profile → 重启 Cursor
- workspace / history / extensions / agent data 共享（复制一次）

**备份策略**: SQLite vscdb + auth.json + cookies → 打包目录

**命令**: 约 **20 条** Cursor 相关命令（见第 10 节）。

### 8.9 `hosts_commands.rs` — 系统 Hosts

**文件路径**: `%WINDIR%\System32\drivers\etc\hosts`

**安全设计**:
- `is_admin()` 实际测写权限（不只是 admin 组成员）
- 每次写前自动备份 → `%APPDATA%\...\hosts-backups\hosts-{timestamp}.bak`
- **原子替换**: 写 `.tmp` → `ReplaceFileW` Windows API（失败不丢原文件）
- `ipconfig /flushdns` 刷新 DNS
- **编码处理**: UTF-8 / UTF-16LE / UTF-16BE / GBK 自动识别
- 备份目录路径白名单校验

### 8.10 `runtime_commands.rs` — 版本切换

**核心**: 修改注册表 `HKLM/SOFTWARE/Microsoft/Windows/CurrentVersion/Path` + `HKCU/...` → 重启当前进程继承。

**加载快照**: 读注册表得到 Machine+User PATH，拼接得到完整 PATH。

### 8.11 `tool_commands.rs` — 工具类

| 命令 | 功能 |
|------|------|
| `set_auto_start` / `get_auto_start` | 开机自启（注册表 Run key） |
| `copy_to_clipboard` / `read_clipboard` | 剪贴板（arboard crate） |
| `export_data` / `import_data` | JSON 导出全部表 / 导入覆盖 |
| `save_text_file` / `pick_text_file` | 文件对话框（rfd） |

### 8.12 `tray/mod.rs` — 系统托盘

**初始化** `tray::init_tray()`:
- 注册 TrayIcon（silver logo）
- 菜单:
  - 打开主窗口
  - 快问
  - 当前隧道状态（动态）
  - 退出

**快问窗口**: 独立 WebView，加载同一 React 应用的 `#/quick-ask` 路由。

**气泡**: 独立窗口 `#/quick-ask-bubble`，物理屏幕坐标定位（通过 Rust `SetWindowPos`）。

### 8.13 `cancellation.rs` — 请求取消

```rust
CancelGuard::new(request_id)  // RAII，drop 时自动清理
is_cancelled(request_id)      // 每 chunk 检查
cancel_request(request_id)    // 前端调用取消
```

---

## 9. 数据库设计

### 9.1 物理位置

```
%APPDATA%\com.ai-workbench.app\ai-workbench.db
```

使用 `rusqlite` bundled 模式，无需外部 sqlite3.dll。

### 9.2 表结构

#### git_accounts
| 列 | 类型 | 约束 |
|----|------|------|
| id | TEXT | PRIMARY KEY (uuid) |
| name | TEXT | NOT NULL |
| email | TEXT | NOT NULL |
| color | TEXT | NOT NULL |
| note | TEXT | NULL |
| created_at | TEXT | NOT NULL |
| updated_at | TEXT | NOT NULL |

#### git_repo_configs
| 列 | 类型 | 约束 |
|----|------|------|
| path | TEXT | PRIMARY KEY |
| name | TEXT | NOT NULL |
| user_name | TEXT | NOT NULL |
| email | TEXT | NOT NULL |
| account_id | TEXT | NULL (→ git_accounts.id) |
| created_at | TEXT | NOT NULL |
| updated_at | TEXT | NOT NULL |

#### cursor_accounts
| 列 | 类型 | 约束 |
|----|------|------|
| id | TEXT | PRIMARY KEY (uuid) |
| name | TEXT | NOT NULL |
| email | TEXT | NOT NULL |
| color | TEXT | NOT NULL |
| backup_path | TEXT | NOT NULL |
| profile_dir | TEXT | NULL |
| profile_initialized | INTEGER | DEFAULT 0 |
| git_user_name | TEXT | NULL |
| git_email | TEXT | NULL |
| password | TEXT | NULL |
| notes | TEXT | NULL |
| is_logged_in | INTEGER | DEFAULT 0 |
| created_at | TEXT | NOT NULL |
| updated_at | TEXT | NOT NULL |

#### ai_models
| 列 | 类型 | 约束 |
|----|------|------|
| id | TEXT | PRIMARY KEY (uuid) |
| name | TEXT | NOT NULL |
| provider | TEXT | NOT NULL |
| api_key | TEXT | NOT NULL (明文) |
| auth_type | TEXT | DEFAULT 'api' |
| base_url | TEXT | NOT NULL |
| model | TEXT | NOT NULL |
| temperature | REAL | DEFAULT 0.7 |
| max_tokens | INTEGER | DEFAULT 4096 |
| is_default | INTEGER | DEFAULT 0 |
| created_at | TEXT | NOT NULL |
| updated_at | TEXT | NOT NULL |

#### cloudflared_profiles
| 列 | 类型 | 约束 |
|----|------|------|
| id | TEXT | PRIMARY KEY (uuid) |
| name | TEXT | NOT NULL |
| hostname | TEXT | NOT NULL |
| local_url | TEXT | NOT NULL |
| token | TEXT | NULL (auth_mode=token) |
| auth_mode | TEXT | DEFAULT 'token' |
| config_path | TEXT | NULL (auth_mode=config) |
| created_at | TEXT | NOT NULL |
| updated_at | TEXT | NOT NULL |

#### snippets
| 列 | 类型 | 约束 |
|----|------|------|
| id | TEXT | PRIMARY KEY (snip-xxx) |
| name | TEXT | NOT NULL |
| content | TEXT | NOT NULL |
| tags | TEXT | DEFAULT '' |
| params | TEXT | DEFAULT '' |
| use_count | INTEGER | DEFAULT 0 |
| created_at | TEXT | NOT NULL |
| updated_at | TEXT | NOT NULL |

#### web_plugins
| 列 | 类型 | 约束 |
|----|------|------|
| id | TEXT | PRIMARY KEY |
| name | TEXT | NOT NULL |
| url | TEXT | NOT NULL |
| group | TEXT | DEFAULT '' |
| tags | TEXT | DEFAULT '' |
| order | INTEGER | DEFAULT 0 |
| hotkey | TEXT | DEFAULT '' |
| is_preset | INTEGER | DEFAULT 0 |
| last_opened_at | TEXT | NULL |
| open_count | INTEGER | DEFAULT 0 |
| added_at | TEXT | NOT NULL |

#### 其他表

| 表 | 主键 | 用途 |
|----|------|------|
| recent_projects | INTEGER AUTO | 最近项目（限 200） |
| git_workspaces | INTEGER AUTO | Workspace 分组 |
| host_profiles | TEXT (uuid) | Hosts 配置快照 |
| web_plugin_history | INTEGER AUTO | 插件打开历史（限 100） |
| plugin_states | TEXT (plugin_id) | 插件启用/配置 |

### 9.3 Schema 迁移

在 `lib.rs` setup 中使用 `ALTER TABLE ADD COLUMN` 行内迁移（幂等，静默忽略失败）：
```rust
let _ = conn.execute_batch("ALTER TABLE cursor_accounts ADD COLUMN notes TEXT;");
let _ = conn.execute_batch("ALTER TABLE web_plugins ADD COLUMN \"group\" TEXT NOT NULL DEFAULT '';");
// ... 共 ~10 条增量迁移
```

---

## 10. Tauri IPC 命令完整清单

### 10.1 Git Commands（16 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `git_repo_summary` | path: String | GitRepoSummary |
| `git_status` | path: String | Vec\<GitStatusEntry\> |
| `git_stage` | path, files: Vec\<String\> | String |
| `git_unstage` | path, files: Vec\<String\> | String |
| `git_commit` | path, message: String | String |
| `git_push` | path: String | String |
| `git_pull` | path: String | String |
| `git_diff` | path, file_path, staged: bool | String |
| `git_discard` | path, file_path, untracked, staged: bool | String |
| `git_undo_last_commit` | path: String | String |
| `git_commit_context` | path: String | GitCommitContext |
| `git_is_repo` | path: String | bool |
| `git_scan_repos` | path, max_depth?: u32 | Vec\<GitScannedRepo\> |
| `set_git_config` | scope, name, email | String |
| `set_repo_git_config` | repo_path, name, email | String |
| `get_repo_git_config` | repo_path | (String, String) |
| `get_git_config` | scope?: String | (String, String) |
| `pick_directory` | — | Option\<String\> |

### 10.2 AI Commands（4 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `test_model_connection` | AIModelConfig | ModelConnectionResult |
| `list_provider_models` | provider, api_key, base_url | ListModelsResult |
| `generate_text` | config, system, user | String |
| `generate_text_stream` | config, system, user, request_id, history? | — (emit 事件) |

### 10.3 DSH Commands（12 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `start_dsh` | port?: u16 | DshInstance |
| `stop_dsh` | port: u16 | String |
| `list_dsh` | — | Vec\<DshInstance\> |
| `check_dsh_port` | port: u16 | bool |
| `check_dsh_http` | port: u16 | bool |
| `check_nodejs_installed` | — | bool |
| `get_dsh_version` | — | String |
| `get_dsh_latest_version` | — | String |
| `get_dsh_versions` | — | Vec\<String\> |
| `install_dsh` | version?: String | String (emit 进度) |
| `update_dsh` | version?: String | String (emit 进度) |
| `sync_model_to_dsh` | name, provider, api_key, base_url, model, max_tokens | String |
| `restore_dsh_auth` | — | String |

### 10.4 Cloudflared Commands（14 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `cloudflared_status` | — | CloudflaredStatus |
| `cloudflared_install` | — | String |
| `cloudflared_open_download` | — | — |
| `cloudflared_pick_binary` | — | Option\<String\> |
| `cloudflared_set_binary_path` | path: String | String |
| `cloudflared_clear_binary_path` | — | String |
| `cloudflared_pick_config` | — | Option\<String\> |
| `cloudflared_start_quick_tunnel` | local_url: String | TunnelStatus |
| `cloudflared_start_named_tunnel` | profile_id, hostname, token?, config_path?, local_url? | TunnelStatus |
| `cloudflared_stop_tunnel` | id: String | String |
| `cloudflared_stop_all_tunnels` | — | String |
| `cloudflared_tunnel_status` | — | Vec\<TunnelStatus\> |
| `cloudflared_setup_new_domain` | config_path, hostname, local_url | String |
| `open_in_browser` | url: String | — |

### 10.5 Hosts Commands（6 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `read_system_hosts` | — | String |
| `is_admin` | — | bool |
| `write_system_hosts` | content: String | String |
| `list_host_backups` | — | Vec\<HostBackup\> |
| `restore_host_backup` | path: String | String |
| `backup_hosts_now` | — | String |

### 10.6 Cursor Commands（20+ 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `inspect_cursor_backup` | account_id | BackupInspect |
| `get_cursor_login_status` | — | {email, name, isLoggedIn} |
| `is_cursor_running` | — | bool |
| `init_account_profile` | account_id | String |
| `finish_account_profile` | account_id, relaunch? | String |
| `launch_cursor` | account_id? | String |
| `quit_cursor` | — | String |
| `get_cursor_profile_dir` | account_id | String |
| `switch_cursor_account` | target, current?, relaunch? | String |
| `delete_cursor_backup` | account_id | — |
| `get_cursor_disk_usage` | — | DiskUsage |
| `cleanup_cursor_full_backups` | — | CleanupResult |
| `read_cursor_diagnostics` | account_id? | String |
| `list_cursor_backups` | — | Vec\<BackupInfo\> |
| `get_cursor_orphan_profiles` | — | OrphanReport |
| `cleanup_cursor_orphan_profiles` | — | CleanupResult |

### 10.7 DB Commands（2 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `db_load` | table: DbTable | Vec\<SqlRow\> |
| `db_save` | table, rows: Vec\<JSON\> | — |

### 10.8 Runtime Commands（9 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `list_runtime_versions` | — | Vec\<RuntimeVersion\> |
| `get_active_runtime` | kind: String | RuntimeVersion |
| `add_custom_runtime` | kind, path, name? | RuntimeVersion |
| `remove_custom_runtime` | kind, path | — |
| `switch_runtime` | kind, path, user_confirm | RuntimeSwitchResult |
| `plan_runtime_switch` | kind, path | RuntimeSwitchPlan |
| `open_runtime_folder` | path | — |
| `open_runtime_terminal` | path | — |
| `list_installable_runtimes` | kind: String | Vec\<InstallableVersion\> |
| `install_runtime` | kind, version | String |
| `uninstall_runtime` | kind, path | String |

### 10.9 Tray Commands（5 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `open_quick_ask_with_text` | text? | — |
| `tray_toggle_quick_ask` | — | — |
| `open_main_deepseek` | — | — |
| `set_quick_ask_bubble_visible` | visible, x?, y? | — |
| `quick_ask_bubble_ready` | — | — |

### 10.10 Tool Commands（8 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `set_auto_start` | enabled: bool | — |
| `get_auto_start` | — | bool |
| `copy_to_clipboard` | text: String | — |
| `read_clipboard` | — | String |
| `export_data` | — | String (JSON) |
| `import_data` | json: String | String |
| `save_text_file` | default_name?, content | String |
| `pick_text_file` | — | Option\<String\> |

### 10.11 Cancellation Commands（1 条）

| 命令 | 参数 | 返回 |
|------|------|------|
| `cancel_request` | request_id: String | — |

---

## 11. 依赖关系图

### 11.1 前端内部依赖

```
App.tsx
  ├─ core/boot.ts  → core/sqlite.ts, core/store.ts
  ├─ core/store.ts → core/storage.ts, core/types.ts, core/cursorMatch.ts, core/pathUtils.ts
  ├─ core/storage.ts → (Tauri invoke db_load/db_save)
  ├─ core/store/invocations.ts → (Tauri invoke × 20+)
  ├─ hooks/useTauriEvent.ts → (Tauri listen)
  ├─ lib/theme.ts
  ├─ lib/quickAskShortcut.ts → (Tauri registerShortcut)
  └─ components/*
       ├─ AIAssistant → aiProviders.ts, (Tauri ai_commands)
       ├─ QuickAskApp → snippets.ts, (Tauri generate_text_stream)
       ├─ CursorManager → (Tauri cursor commands)
       ├─ DeepSeekHarness → (Tauri dsh_commands)
       ├─ CloudflaredManager → (Tauri cloudflared_commands)
       └─ Settings → (Tauri tool_commands, getVersion)
```

### 11.2 Rust 后端内部依赖

```
lib.rs (Builder)
  ├─ db_commands.rs → DbState
  ├─ git_commands.rs (Command::new("git"))
  ├─ ai_commands.rs → reqwest
  ├─ dsh_commands.rs → DshState, config.rs
  ├─ cloudflared_commands.rs → CloudflaredState, config.rs
  ├─ hosts_commands.rs → chrono, windows (ReplaceFileW)
  ├─ runtime_commands.rs → windows (Registry)
  ├─ cursor/mod.rs → DbState, config.rs
  ├─ tray/mod.rs → TEA (TrayEvent), cursor
  ├─ tool_commands.rs → arboard, rfd
  ├─ cancellation.rs (stateful static HashMap)
  └─ config.rs (全局常量 + ErrorCode)
```

### 11.3 外部进程依赖

| 进程 | 用途 | 调用方式 |
|------|------|----------|
| `git` | Git 操作 | `Command::new("git").args(...)` |
| `cmd /c npm` | DSH 安装/查询版本 | `Command::new("cmd").args(["/c", "npm", ...])` |
| `node.exe` | DSH 服务 | 找到全局 node → `Command::new(node_path).arg(entry)` |
| `cloudflared.exe` | 内网穿透 | `resolve_cloudflared()` 找到路径 → spawn |
| `cursor.exe` | Cursor IDE | `launch_cursor` / `quit_cursor` |
| `winget` | cloudflared 安装 | `cmd /c winget install Cloudflare.cloudflared` |
| `ipconfig /flushdns` | DNS 刷新 | hosts 写入后执行 |
| `netstat -ano` | 端口 → PID | 查 DSH / cloudflared 占用的进程 |
| `taskkill /T /F /PID` | 杀进程 | DSH / cloudflared 停止 |
| `ReplaceFileW` (Win32) | 原子替换 | hosts / cloudflared config.yml 写入 |

---

## 12. 运行与调试

### 12.1 环境前置

```
Node.js ≥ 18
Rust ≥ 1.70
Windows 10/11 (WebView2 通常已预装)
可选: Git, Cursor, Node.js (for DSH), cloudflared
```

### 12.2 开发启动

```bash
# 1. 安装前端依赖
npm install

# 2. 仅前端（无桌面壳）
npm run dev
# 访问 http://localhost:1420

# 3. 完整 Tauri 开发模式
npm run tauri -- dev
```

### 12.3 快捷调试

#### 调试主窗口
```powershell
npm run tauri -- dev
# DevTools: Ctrl+Shift+I
```

#### 调试快问窗口
在快问窗口中按 `Ctrl+Shift+I` 打开 DevTools。

#### 调试托盘后台线程
托盘运行在独立窗口，可以通过 Tauri `emit` 日志或在 Rust 代码中 `eprintln!` 输出到 `cargo run` 终端。

### 12.4 Rust 后端日志

`eprintln!` 输出到 Tauri Dev Server 的终端窗口。
推荐格式: `eprintln!("[cursor] 切换失败: {e}");`

### 12.5 前端错误

React ErrorBoundary 捕获后渲染 fallback UI，控制台可见堆栈。

---

## 13. 打包与发布

### 13.1 版本同步（三处）

| 文件 | 字段 |
|------|------|
| `package.json` | `"version": "0.1.0"` |
| `tauri.conf.json` | `"version": "0.1.0"` |
| `Cargo.toml` | `version = "0.1.0"` |

### 13.2 构建

```bash
# 安装依赖
npm install

# 构建
npm run tauri -- build

# 调试构建
npm run tauri -- build -- --debug
```

### 13.3 产物

| 类型 | 路径 |
|------|------|
| NSIS 安装包 | `src-tauri/target/release/bundle/nsis/ai_workbench_0.1.0_x64-setup.exe` |
| 便携 exe | `src-tauri/target/release/ai_workbench.exe` |
| MSI | `src-tauri/target/release/bundle/msi/` |

### 13.4 发布检查清单

- [ ] 三处版本号已同步
- [ ] DSH 认证补丁已测试（0.1.5-rc.1 / rc.2 等版本兼容）
- [ ] Cloudflared 临时 / 命名隧道均测试
- [ ] Hosts 读写 + 备份恢复 + atomic replace
- [ ] Cursor 账号初始化 + 切换 + 启动
- [ ] 导入/导出 JSON（含密钥明文警告）
- [ ] 关闭到托盘 → 托盘菜单可见
- [ ] 快问快捷键 `Ctrl+Alt+K`（需要 admin 可能有权限限制）

---

## 14. 扩展指南

### 14.1 新增一个 Setting（用户偏好持久化到 localStorage）

**Step 1**: 在 `types.ts` 的 `AppSettings` 添加字段
```typescript
interface AppSettings {
  // ...existing
  myNewFeature: boolean;
}
```

**Step 2**: 在 `store.ts` 的 `DEFAULT_SETTINGS` 添加默认值

**完成** — persist 中间件自动合并。

### 14.2 新增一个 SQLite 表实体

**Step 1**: 前端 `core/types.ts` 添加 TypeScript 接口

**Step 2**: 前端 `core/storage.ts` 添加 table enum + load/save 方法

**Step 3**: 前端 `core/store.ts` 添加 load/add/update/delete 方法（使用 `withTable` + `optimisticUpdate`）

**Step 4**: 后端 `lib.rs` setup 中 `CREATE TABLE IF NOT EXISTS ...`

**Step 5**: 后端 `db_commands.rs` 添加表类型 + load_sql + insert_row

**完成** — 自动走 db_load/db_save 通道。

### 14.3 新增一个 Tauri IPC 命令（仅 Rust）

**Step 1**: 在对应模块 `src-tauri/src/xxx_commands.rs` 添加 `#[tauri::command]` 函数

```rust
#[tauri::command]
pub async fn my_new_command(arg1: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        // blocking work
        Ok("result".to_string())
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?
}
```

**Step 2**: 在 `lib.rs` 的 `invoke_handler` 中注册：
```rust
.my_new_command
```

**Step 3**: 前端调用：
```typescript
const { invoke } = await import('@tauri-apps/api/core');
const result = await invoke<string>('my_new_command', { arg1: 'hello' });
```

### 14.4 新增一个导航 Tab

**Step 1**: 在 `App.tsx` 的 `Tab` type 联合添加新类型

**Step 2**: 在 `NAV_NODES` 数组中添加导航项

**Step 3**: 在 JSX `main-body` 中添加条件渲染

**Step 4**: 创建新组件 `NewFeaturePage.tsx`

### 14.5 新增 i18n key

在对应 locale 文件（`locales/zh-CN/xxx.json` 和 `en-US/xxx.json`）添加 key-value，然后在组件中：
```typescript
const { t } = useTranslation('xxx');
t('myKey')
```

---

## 附录 A：事件（Emitter）清单

### Rust → Frontend 事件

| 事件名 | Payload | 触发时机 |
|--------|---------|----------|
| `generate-text-chunk` | `{ id, text }` | AI 流式每 chunk |
| `generate-text-done` | `{ id }` | AI 流结束 |
| `generate-text-error` | `{ id, error }` | AI 流错误 |
| `dsh:install_progress` | `{ stage, percent, message }` | DSH 安装进度 |
| `dsh:update_progress` | `{ stage, percent, message }` | DSH 更新进度 |
| `cloudflared-log` | `{ id, tag, line }` | cloudflared 日志行 |
| `cloudflared-url` | `{ id, url }` | 隧道拿到公网 URL |
| `tray-minimized` | — | 用户关闭窗口到托盘 |
| `quick-ask-bubble-enabled` | `boolean` | 气泡显隐切换 |
| `navigate-tab` | `Tab` | 托盘菜单跳转 |

### Frontend → Rust Event

| 事件名 | 触发 |
|--------|------|
| `tray-minimized` | App.tsx handleClose() |

---

## 附录 B：种子片段（内置 6 个）

`store/helpers.ts` `buildSeedSnippets()`:

| ID | 名称 | 用途 |
|----|------|------|
| snip-seed-1 | Code Review Prompt | 代码审查提示词模板 |
| snip-seed-2 | Git Commit Message | AI 生成 commit message 模板 |
| snip-seed-3 | Refactoring | 重构建议模板 |
| snip-seed-4 | Bug Analysis | Bug 分析模板 |
| snip-seed-5 | Test Generation | 测试生成模板 |
| snip-seed-6 | Documentation | 文档生成模板 |

---

## 附录 C：Provider 元数据

`lib/aiProviders.ts` 包含每个 provider 的：
- displayName
- defaultBaseUrl
- apiKeyHint
- authType（api / token_plan）
- 特殊行为（如 mimo 用 `api-key` header）

---

*Generated from source analysis on 2026-09-15*
