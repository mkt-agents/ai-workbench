# AI Workbench (AI 工作台)

基于 **Tauri v2 + React + Rust/SQLite** 的 Windows 桌面应用，定位为 **AI 驱动的开发工具箱**。核心是 **DeepSeek 本地服务**，辅以模型配置、提示词、Git / Cursor 多身份与账号配额，以及内网穿透、网页工具、Hosts 管理等能力。所有数据留在本机，不上传密钥。

## 效果图

![AI Workbench 效果图](docs/screenshot.png)

## 功能特性

### AI 工作台

- **天玑（AI 助手）**：本机 Harness——全局快捷键唤起的多轮流式对话，**Code / Work 双轨**（代码与文档办公）+ 任务模式钉住。四档权限：只读 → 编辑 → 审查托管 → 完全权限；文件编辑、命令执行均有 diff 审批与安全边界；支持截屏与键鼠（完全权限）、`/` 命令面板、附件、日报与用量台账。**同一会话可逐轮切换模型**（历史不清空，答案气泡标注「由 X 回答」，切到不看图模型时历史图片自动降级为文字占位）；**图文对话**：粘贴 / 拖放 / 选择图片即可提问，当前模型不看图时自动切到会话里的看图模型或退化文字占位，不再硬拦；**文档附件**：docx / pdf / xlsx / csv 等拖放或选择后自动抽取文本层注入上下文（PDF 无 OCR，扫描件会提示）；**图片识别文字**：附件上的「识别文字」用 Tesseract 做本机 OCR，把图里的字抽成文本（纯文本模型也能读图）；**可写子代理**：开启后编辑档及以上可把能自成一体的小改动整体交给子代理，复用主回合的审批、安全边界与撤销
- **企微智能机器人**：设置 → 联网扩展；WebSocket 长连接（Bot ID + Secret），支持私聊配对与群聊 @ 触发，复用天玑 gent_run（默认只读权限）。两档引擎（天玑内置 / DeepSeek Harness），图片在看图模型上真正进模型、审批转聊天审批码。**面向逆向文档的三件套**：① **文档优先问答**（模式可配：自动（默认，按问题意图）/ 文档优先 / 文档+代码回退 / 仅读代码——文档优先模式下能用 `doc/` 逆向文档回答就不读业务代码，用户说「读代码」时放行）；② **逆向文档每日自动更新**（到点拉 release 分支 → 按新提交让 AI 更新 `doc/` 下文档 → 企微群 webhook 推送变动；脏工作区跳过、无变更不空推、**硬边界只允许改文档目录**）；③ **SkyWalking 线上排障**：天玑可调用查询工具；DSH 由 Rust 按 RFC3339/中文起止时间、关键词采集入口日志 → trace → 关联服务日志，再把证据交给模型核对代码，不是 DSH 自主调用 SkyWalking MCP。日志源先经最多两次能力探测（基础地址或完整 `/graphql`）；失败/未执行直接说明，不由模型改写。单故障时刻先查±15分钟，近似时刻成功查空可补±60分钟；业务ID分别查询。源就绪后允许最多2轮受本地校验的DSH代码JSON补查，必须保留原业务ID或使用证据确认的traceId，`code_ref`须匹配工作区真实源码行；补查证据按`[R1L1]`等编号。全过程共享120秒累计采集时间、24次HTTP，DSH整轮600秒；单次采集最多300条日志、3条trace、6个服务/实例补查目标。设置页按当前后端实例显示PID/build、实际请求、采集证据/展示日志、采集WARN、截断/待查页和未解释决策；缺字段显示「未提供」，累计补查可含重复。结论分清结果、直接原因与具体触发原因：过滤为0仅是失败出口，normal trace不排除业务异常；短窗候选不等于同一请求，错误、截断或预算耗尽返回部分覆盖（`partial`）。OAP 时区可选固定 UTC 偏移或跟本机；追问条件缓存仅在内存保留 30 分钟、不存日志正文（不等于 DSH 会话/问答旁证不留内容）。详见 [DSH 排障边界](<docs/wecom-dsh-engine.md>)。
- **Harness（侧栏）**：管理本机 DeepSeek 本地服务（DSH），内嵌 Web UI——与天玑是两条入口：天玑管本机代理与治理，DSH 管深度对话引擎
- **模型管理**：多厂商 + OpenAI 兼容接口，连接测试、按场景绑定、探测本机 Ollama / LM Studio；**添加模型时自动识别类型**（文本 / 图文多模态，按模型名规则库推断并预填，可手动改成自动 / 文本 / 图文三态，`/models` 列表也标注类型）；页内「提示词」Tab 管理本地提示词库（`{{param}}`、导入导出、天玑 `/` 插入）；API Key 落库使用 Windows DPAPI 加密
- **工作暂存**：临时存放提示词 / 脚本 / 备忘，可一键转为提示词

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
├── 工作暂存
├── 模型管理（列表 / 对比 / 提示词）
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
2. 天玑是本机 Harness（四档权限 + Code/Work 双轨 + 工具代理）；侧栏 DeepSeek Harness 仍是独立的 DSH 深度对话入口
3. DeepSeek / cloudflared 依赖本机环境
4. Git 提交页基于收藏仓库扫描

## 许可

MIT
