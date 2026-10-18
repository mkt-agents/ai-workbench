# AI Workbench (AI 工作台)

基于 **Tauri v2 + React + Rust/SQLite** 的 Windows 桌面应用，定位为 **AI 驱动的开发工具箱**。核心是 **DeepSeek 本地服务**，辅以模型配置、模板片段、Git / Cursor 多身份与账号配额，以及内网穿透、网页工具、Hosts 管理等能力。

## 效果图

![AI Workbench 效果图](docs/screenshot.png)

### 应用图标

- 设计母版：[`docs/brand/logo-silver.png`](docs/brand/logo-silver.png)（默认冷银）、[`docs/brand/logo-ice.png`](docs/brand/logo-ice.png)（冰蓝备选）
- 打包默认：`src-tauri/icons/icon.png` / `icon.ico`
- 侧栏 / 加载屏：`src/assets/logo-silver.png`（`AppLogoMark`）

重新生成 Tauri 图标（使用母版 PNG）：

```bash
npx @tauri-apps/cli icon docs/brand/logo-silver.png
npx @tauri-apps/cli icon docs/brand/logo-ice.png -o src-tauri/icons/ice
```

## 功能特性

### AI 工作台

- **Harness**：管理本机 DeepSeek 本地服务，内嵌 Web UI；显示上次同步到 DSH 的模型与时间。0.2 及以上用官方启动令牌登录（独立内嵌页，不改安装文件）；旧版本留下的认证补丁会在下次启动时自动还原。账号页的外链在系统浏览器打开
- **智枢**：全局快捷键唤起，流式问答；自动判断要不要带上项目上下文（输入框左上的仓库名亮表示带、暗表示不带，单击即可改为下一条不带 / 带上），带上时才附带所选仓库的工作区 / Git / 改动摘要；会话行不再单列；输入框右下是权限档位和模型，档位仿 DeepSeek Harness 分四档：只读权限（默认，只读文件、搜代码、查 git）/ 编辑权限（可改仓库内文件，跑命令、删除、结束进程等先问，可选「本轮同类都允许」）/ 审查托管 EXP（AI 先审，有风险再问）/ 完全权限（不再询问、可操作整台电脑，首次切换二次确认），`Ctrl+.` 在前三档之间切换，回答进行中也能换档、对下一条生效；代理能按行读文件、精确编辑（审批卡片显示 diff）、删除进回收站、移动、按名找文件、正则搜索，跑 cmd / PowerShell，后台运行 dev server 并读取输出，打开文件和网址、读写剪贴板、查看和结束进程、释放端口，还能读写 hosts、启停内网穿透、切换运行时版本；模型在模型配置里标了「支持看图」时，完全权限下还能截屏并操作鼠标键盘（把鼠标移到屏幕左上角紧急停止，智枢窗口不进截图）；危险命令、Windows / Program Files / 本应用数据目录在任何档位都拦截，其它档位不出当前仓库；每一步的命令输出、报错和改动都能在步骤里展开查看；回答边生成边显示，模型不支持工具调用时自动降级为只回答；输入框下方的诊断 / 解释 / 润色 / 翻译 / 总结 / 优化提示词为一次性任务，发送后自动复位，日报按今天的提交直接生成（日报旁的小箭头可选群聊式 / 三段式，生成后自动去掉代码块、表格、表情和多余说明）；输入 `/` 或工具条可插入模板片段，也可附加文本文件、填入剪贴板；回答底部标注本次依据（项目/对话、仓库、模型、耗时与 token）；模型知道自己的名称与当天日期；窗口默认不置顶，标题栏图钉可切换为始终在最上层（记住选择）；标题栏可放大 / 还原（双击标题栏同效），窗口大小和位置下次打开沿用，窄窗口下代码块和表格自动换行；输入「打开 …」时除了页面和快捷应用，还会列出开始菜单 / 桌面上的已安装程序，本地没有匹配时回车交给 AI，AI 启动应用时同样能找到已安装程序
- **模型配置**：多厂商与 OpenAI 兼容接口，连接测试与同步到 DSH；按场景绑定默认模型（智枢 / 报告 / 提交 / 提示词优化）；一键探测本机 Ollama / LM Studio；页内「模型对比」一次性并排生成；API Key 落库时用 Windows DPAPI 加密
- **模板片段**：本地文本 / 提示词模板，支持 `{{param}}` 参数与导入导出（v2 带应用版本号）；可绑定智枢任务（输入为空时点任务自动填入）；智枢内可用 `/` 搜索插入
- **工作暂存**：任务中途临时存放提示词 / 脚本 / 备忘；没存的内容切页面不丢，支持搜索、多行预览、Ctrl+Enter 存入；可复制、插入智枢、置顶，或一键转为模板片段（可选同时删掉草稿）

### 账号资产

- **Cursor**：多账号一键切换，工作区与扩展数据共享、登录态互不覆盖
- **账号配额**：Cursor / WorkBuddy / DeepSeek / Claude / Qoder / MiMo 等额度看板；支持签到、领积分与每日定点；右侧「服务商与账号」栏统一管理各服务商的账号添加、看板显隐与本机安装状态（含微软商店应用）

### 工程管理

- **Git 管理**：多仓库扫描、身份绑定、批量拉取/建分支/切分支；提交页查看 diff、按块暂存、stash、修改上一次提交，并在页头切换分支
- **Git 报告**：按 diff 静态分析改动与风险，可选 AI 补充覆盖与必测场景；支持按章节重新生成
- **环境变量**：本机开发运行时相关配置

### 网络管理

- **内网穿透**：基于 cloudflared 的临时 / 命名隧道（[文档](docs/cloudflared.md)）
- **网页工具**：常用站点书签与油猴式用户脚本，独立弹窗打开
- **Hosts 管理**：读写系统 hosts，多套配置切换与备份恢复（需管理员）

### 系统工具

- **工具箱**：JSON、编解码、时间戳、Cron、UUID、Hash、JWT、正则
- **快捷应用**：常用应用一键启动 / 关闭，附带端口查杀

### 设置

- 多主题、中英语言、开机自启、智枢快捷键
- 本机 AI 用量台账（调用次数 / 估算 token，可清空）
- 应用内检查更新与升级；数据导出 / 导入（含明文密钥警告）

## 侧边栏导航

```
AI 工作台
├── Harness
├── 模型配置
├── 模板片段
└── 工作暂存

账号资产
├── Cursor
└── 账号配额

工程管理
├── Git 管理
└── 环境变量

网络管理
├── 内网穿透
├── 网页工具
└── Hosts 管理

系统工具
├── 工具箱
└── 快捷应用

设置（固定在侧栏底部）
```

## 技术栈

| 层 | 技术 |
|---|---|
| 前端 | React 18 + TypeScript + Vite 5 |
| 后端 | Tauri v2 + Rust |
| 持久化 | SQLite（`rusqlite` bundled） |
| 状态 | Zustand 5 |
| 样式 | 纯 CSS（`data-theme`） |
| i18n | i18next |

## 快速开始

### 环境要求

- Node.js 18+
- Rust 1.70+
- Windows 10/11
- WebView2（Win10/11 通常已预装）

### 安装与开发

在仓库根目录执行（不要写死本机盘符路径）：

```bash
git clone <repo-url>
cd ai-workbench
npm install
npm run tauri -- dev
```

仅前端 Vite（无桌面壳）：

```bash
npm run dev
```

调试构建出的 exe（相对路径）：

```powershell
npm run tauri -- build -- --debug
Start-Process .\src-tauri\target\debug\ai_workbench.exe
```

## 打安装包（Windows exe）

发布前先**同步版本号**（三处保持一致）：

| 文件 | 字段 |
|------|------|
| `package.json` | `"version"` |
| `package-lock.json` | `"version"`（2 处，随 `npm install` 写入） |
| `src-tauri/tauri.conf.json` | `"version"` |
| `src-tauri/Cargo.toml` | `version`（`src-tauri/Cargo.lock` 未被 git 跟踪，构建时自动更新） |

当前版本：`0.2.16`（设置页通过 Tauri `getVersion()` 读取）。发布前务必核对上表四处一致——`src-tauri/Cargo.toml` 最容易漏（它决定 exe 文件属性里的版本号）。

### 构建

```bash
npm install
npm run tauri -- build
```

### 产物位置

| 类型 | 路径 |
|------|------|
| NSIS 安装包 | `src-tauri/target/release/bundle/nsis/*.exe` |
| MSI（若生成） | `src-tauri/target/release/bundle/msi/` |
| 便携 exe | `src-tauri/target/release/ai_workbench.exe` |

`tauri.conf.json` 中 `bundle.targets` 为 `all` 时会尝试生成常见 Windows 安装格式；日常分发优先使用 **NSIS 安装包**。

### 分发注意

- **开发标识零泄漏（打包红线）**：所有开发标识——主窗口标题 `[DEV]` 后缀（`src-tauri/src/lib.rs`，`cfg!(debug_assertions)`）、托盘提示 `[DEV]` 前缀（`src-tauri/src/tray/mod.rs`）、前端 DEV 徽标（`src/App.tsx`，`import.meta.env.DEV`）——必须是**编译期常量**，`npm run tauri -- build`（release）自动剥离；禁止改用运行时环境变量 / 配置文件 / 命令行开关控制开发标识，新增开发态 UI 一律沿用上述两处编译期机制，发版前抽查安装包运行无任何 `[DEV]` / DEV 徽标
- 需要目标机器具备 **WebView2**
- DeepSeek 功能仍依赖本机 **Node.js + npm**（全局 `@deepseek-ai/dsh`）
- 内网穿透依赖本机 `cloudflared`（应用内可安装或指定 exe）
- 修改 Hosts 需**管理员**运行
- **不要**把用户数据目录打包进安装包：`%APPDATA%\com.ai-workbench.app\`（含 SQLite 与明文密钥）
- 安装包本身不包含用户的 API Key / Token；用户数据在首次运行后写入本机

## 隐私说明

- API Key、Tunnel Token、Cursor 密码备忘、CodeBuddy 账号凭证等保存在本机 SQLite：`%APPDATA%\com.ai-workbench.app\ai-workbench.db`
- 不会上传密钥；请勿分享数据库或导出的含密钥 JSON
- 书签图标只向**目标站点自身**请求 `/favicon.ico`，不经过第三方图标服务，内网域名不会外泄
- 「网页工具 → 从网址导入」会由本机直接向你填写的脚本地址发起一次 HTTP(S) 请求（不经第三方中转）

## 免责声明

- 本项目为个人开发的本地工具，与 DeepSeek、Cloudflare、Cursor 等厂商**无任何隶属或合作关系**；相关名称与图标仅用于描述兼容对象，版权归原作者所有
- 当前版本启动 DSH 时不再修改安装目录。若更早的版本留下认证补丁，下次启动会自动还原原始文件
- Hosts 修改、内网穿透等功能仅供合法用途，使用者需遵守当地法律法规
- 使用本项目产生的任何直接或间接风险由使用者自行承担

## 项目结构（摘要）

```
ai-workbench/
├── src/                 # React 前端（App、components、core、locales）
├── src-tauri/           # Tauri + Rust 命令（见 src/lib.rs invoke_handler）
├── docs/                # 截图、品牌图、专题文档
├── package.json
└── README.md
```

后端命令按模块拆分于 `src-tauri/src/*_commands.rs`，完整列表以 `src-tauri/src/lib.rs` 注册为准。

`src-tauri/src/bin/` 下是内部诊断工具（`cursor_switch_test`）。它们由 `dev-tools` 特性门控，**默认不参与构建**，因此不会被打进安装包；需要时用 `cargo build --features dev-tools` 单独编译。

## 已知限制

1. 目前仅面向 **Windows**
2. 完整 AI 对话主要走 DeepSeek Harness；智枢为轻量单发
3. DeepSeek / cloudflared 依赖本机环境
4. 敏感字段本机明文存储（见隐私说明）
5. Git 提交页基于收藏仓库扫描

## 许可

MIT
