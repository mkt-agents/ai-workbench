# AI Workbench (AI 工作台)

基于 **Tauri v2 + React + Rust/SQLite** 的 Windows 桌面应用，定位为 **AI 驱动的开发工具箱**。核心是 **DeepSeek 本地服务**，辅以模型配置、模板片段、Git / Cursor 多身份与账号配额，以及内网穿透、网页工具、Hosts 管理等能力。所有数据留在本机，不上传密钥。

## 效果图

![AI Workbench 效果图](docs/screenshot.png)

## 功能特性

### AI 工作台

- **天玑（AI 助手）**：全局快捷键唤起的多轮流式对话，自动携带项目上下文（仓库 / 分支 / 改动摘要）。四档权限：只读 → 编辑 → 审查托管 → 完全权限，文件编辑、命令执行均有 diff 审批与安全边界（危险命令与系统目录拦截、删除进回收站）；支持截屏与键鼠操作（完全权限）、模板片段 `/` 插入、附件、日报生成与用量台账
- **Harness**：管理本机 DeepSeek 本地服务（DSH），内嵌 Web UI
- **模型配置**：多厂商 + OpenAI 兼容接口，连接测试、按场景绑定默认模型、探测本机 Ollama / LM Studio；API Key 落库使用 Windows DPAPI 加密
- **模板片段**：本地提示词模板，支持 `{{param}}` 参数、导入导出与天玑 `/` 插入
- **工作暂存**：临时存放提示词 / 脚本 / 备忘，可一键转为模板片段

### 账号资产

- **Cursor**：多账号一键切换（登录态快照，同一时刻仅一个账号在线）
- **账号配额**：Cursor / WorkBuddy / DeepSeek / Claude 等额度看板与签到

### 工程管理

- **Git 管理**：多仓库扫描与身份绑定；批量拉取 / 推送 / 建分支 / 切分支（推送前预览提交清单，可取消、失败项单独重试）；提交页支持 IDEA 式 diff 查看与暂存；冲突一律交给人处理（整文件三栏合并）
- **Git 报告**：基于 diff 的静态分析，可选 AI 补充覆盖与必测场景
- **环境变量**：本机开发运行时配置

### 网络管理

- **内网穿透**：基于 cloudflared 的临时 / 命名隧道（[文档](docs/cloudflared.md)）
- **网页工具**：站点书签 + 油猴式用户脚本（内置 / 自定义模板，支持 JSON 分享导入导出），独立弹窗打开
- **Hosts 管理**：读写系统 hosts，多套配置切换与备份恢复（需管理员）

### 系统工具

- **工具箱**：JSON、编解码、时间戳、Cron、UUID、Hash、JWT、正则、计算器、端口查杀
- **快捷应用**：常用应用一键启动 / 关闭

### 设置

- 多主题（含玻璃质感）、中英双语、开机自启、天玑快捷键
- 本机 AI 用量台账、应用内检查更新、数据导出 / 导入

## 侧边栏导航

```
AI 工作台
├── Harness
├── 协同办公
├── 模型配置
├── 模板片段
├── 工作暂存
└── 快捷应用

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
└── 工具箱

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

## 打安装包（Windows exe）

发布前先**同步版本号**（四处保持一致）：`package.json`、`package-lock.json`（2 处）、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`（最容易漏，决定 exe 文件属性版本）。当前版本：`0.2.33`。

```bash
npm install
npm run tauri -- build
```

| 类型 | 路径 |
|------|------|
| NSIS 安装包 | `src-tauri/target/release/bundle/nsis/*.exe` |
| MSI（若生成） | `src-tauri/target/release/bundle/msi/` |
| 便携 exe | `src-tauri/target/release/ai_workbench.exe` |

### 分发注意

- 需要目标机器具备 **WebView2**
- DeepSeek 功能依赖本机 **Node.js + npm**（全局 `@deepseek-ai/dsh`）
- 内网穿透依赖本机 `cloudflared`（应用内可安装或指定 exe）
- 修改 Hosts 需**管理员**运行
- **不要**把用户数据目录（`%APPDATA%\com.ai-workbench.app\`）打包进安装包；安装包不含用户的 API Key / Token

## 隐私说明

- API Key、Tunnel Token、Cursor 密码备忘等保存在本机 SQLite：`%APPDATA%\com.ai-workbench.app\ai-workbench.db`
- 不会上传密钥；请勿分享数据库或导出的含密钥 JSON
- 书签图标只向**目标站点自身**请求 `/favicon.ico`，不经过第三方图标服务
- 「网页工具 → 从网址导入」由本机直接向你填写的脚本地址发起请求（不经第三方中转）

## 免责声明

- 本项目为个人开发的本地工具，与 DeepSeek、Cloudflare、Cursor 等厂商**无任何隶属或合作关系**；相关名称与图标仅用于描述兼容对象，版权归原作者所有
- Hosts 修改、内网穿透等功能仅供合法用途，使用者需遵守当地法律法规
- 使用本项目产生的任何直接或间接风险由使用者自行承担

## 项目结构（摘要）

```
ai-workbench/
├── src/                 # React 前端（App、components、core、locales）
├── src-tauri/           # Tauri + Rust 命令（见 src/lib.rs invoke_handler）
├── scripts/             # 构建检查脚本与内置预设脚本
├── docs/                # 截图、品牌图、专题文档
└── package.json
```

后端命令按模块拆分于 `src-tauri/src/*_commands.rs`，完整列表以 `src-tauri/src/lib.rs` 注册为准。

## 已知限制

1. 目前仅面向 **Windows**
2. 天玑是本机 Harness（四档权限 + 工具代理），深度对话仍走 DeepSeek Harness
3. DeepSeek / cloudflared 依赖本机环境
4. Git 提交页基于收藏仓库扫描

## 许可

MIT
