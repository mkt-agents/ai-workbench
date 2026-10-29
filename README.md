# AI Workbench (AI 工作台)

基于 **Tauri v2 + React + Rust/SQLite** 的 Windows 桌面应用，定位为 **AI 驱动的开发工具箱**。核心是 **DeepSeek 本地服务**，辅以模型配置、提示词、Git / Cursor 多身份与账号配额，以及内网穿透、网页工具、Hosts 管理等能力。所有数据留在本机，不上传密钥。

## 效果图

![AI Workbench 效果图](docs/screenshot.png)

## 功能特性

### AI 工作台

- **天玑（AI 助手）**：本机 Harness——全局快捷键唤起的多轮流式对话，**Code / Work 双轨**（代码与文档办公）+ 任务模式钉住。四档权限：只读 → 编辑 → 审查托管 → 完全权限；文件编辑、命令执行均有 diff 审批与安全边界；支持截屏与键鼠（完全权限）、`/` 命令面板、附件、日报与用量台账。**同一会话可逐轮切换模型**（历史不清空，答案气泡标注「由 X 回答」，切到不看图模型时历史图片自动降级为文字占位）；**图文对话**：粘贴 / 拖放 / 选择图片即可提问，当前模型不看图时自动切到会话里的看图模型或退化文字占位，不再硬拦；**文档附件**：docx / pdf / xlsx / csv 等拖放或选择后自动抽取文本层注入上下文（PDF 无 OCR，扫描件会提示）；**图片识别文字**：附件上的「识别文字」用 Tesseract 做本机 OCR，把图里的字抽成文本（纯文本模型也能读图）；**可写子代理**：开启后编辑档及以上可把能自成一体的小改动整体交给子代理，复用主回合的审批、安全边界与撤销
- **企微智能机器人**：设置 → IM机器人 → 企业微信；WebSocket 长连接（Bot ID + Secret），支持私聊配对与群聊 @ 触发，复用天玑 gent_run（默认只读权限）。两档引擎（天玑内置 / DeepSeek Harness），图片在看图模型上真正进模型、审批转聊天审批码。**面向逆向文档的三件套**：① **文档优先问答**（模式可配：自动（默认，按问题意图）/ 文档优先 / 文档+代码回退 / 仅读代码——文档优先模式下能用 `doc/` 逆向文档回答就不读业务代码，用户说「读代码」时放行）；② **逆向文档每日自动更新**（到点拉 release 分支 → 按新提交让 AI 更新 `doc/` 下文档 → 企微群 webhook 推送变动；脏工作区跳过、无变更不空推、**硬边界只允许改文档目录**）；③ **SkyWalking 线上排障**：天玑可调用查询工具；DSH 由 Rust 按 RFC3339/中文起止时间、关键词采集入口日志 → trace → 关联服务日志，再把证据交给模型核对代码，不是 DSH 自主调用 SkyWalking MCP。日志源先经最多两次能力探测（基础地址或完整 `/graphql`）；失败/未执行直接说明，不由模型改写。单故障时刻先查±15分钟，近似时刻成功查空可补±60分钟；业务ID分别查询。源就绪后允许最多2轮受本地校验的DSH代码JSON补查，必须保留原业务ID或使用证据确认的traceId，`code_ref`须匹配工作区真实源码行；补查证据按`[R1L1]`等编号。全过程共享120秒累计采集时间、24次HTTP，DSH整轮600秒；单次采集最多300条日志、3条trace、6个服务/实例补查目标。设置页按当前后端实例显示PID/build、实际请求、采集证据/展示日志、采集WARN、截断/待查页和未解释决策；缺字段显示「未提供」，累计补查可含重复。结论分清结果、直接原因与具体触发原因：过滤为0仅是失败出口，normal trace不排除业务异常；短窗候选不等于同一请求，错误、截断或预算耗尽返回部分覆盖（`partial`）。OAP 时区可选固定 UTC 偏移或跟本机；追问条件缓存仅在内存保留 30 分钟、不存日志正文（不等于 DSH 会话/问答旁证不留内容）。详见 [DSH 排障边界](<docs/wecom-dsh-engine.md>)。**性能实现待同条件实测（2026-10-29）**：保持同模型与 reasoning，不加大24HTTP/120秒预算；请求内连接/成功响应复用与singleflight、每OAP全局并发2、剩余预算≥20且无service的独立双ID组并发（分页及trace/decision仍串行）、256KiB retained摘录索引/delta，以及合格`diagnostic_report`直接渲染（旧`final_answer`经引用/源码行校验后映射到同一renderer，未形成合格报告才走final_model）。不跨用户缓存业务证据，不自动降低深度；运行诊断保留旧阶段快照，并可展开同 request 的 summary 阶段表（raw 标签/次数/耗时/首事件/字符/工具事件/校验失败/会话动作）；不重复累加父子阶段，total为执行耗时、queue单列，与含排队的聊天尾注分口径。优化前真实基线为461.7秒（collect31.6秒）与255秒（collect49.2秒），不是已验证提速结果。
  - **本轮性能调整待验证（2026-10-29）**：完整指令「新会话」显式reset，活动ACP会话追问reuse而非重复resume；带gaps且层级已按覆盖降级的合格final直接渲染、保留缺口，省final_model。首次查询18次、保留6次补查，parent24HTTP/120秒不增；首轮低于旧并发阈值20而保守串行，未实现首两页新并发；time_window_mismatch不当empty；source_hints读stack实际源码定位行±8，不是日志或跨用户cache。真实日志对照：群total355.161秒/acquire60.964秒/planning166.935+44.373秒/final82.538秒，私聊190.361/74.074/69.207/46.493秒；差值主要在规划与总结，未证明群平台慢或新版本已提速。见 [本轮性能观测](<docs/wecom-dsh-engine.md>)。
  - **统一排障报告与耗时（2026-10-29，格式/内容回归待验证）**：旧 direct 的summary/claims+6k摘录与fallback自由Markdown两条展示路径，现收敛到 [DiagnosticReport version 1](<src-tauri/src/wecom_report.rs>)。最新按图一恢复「结果 / 直接原因 / 具体触发原因 / 下游伴随 / 未完成」纵向排障格式，替代宽表；timeline最多32条校验；按证据ID全局去重，共享8块展示/6条短引文预算，后续claim只列refs、code补日志refs。实际message仅去严格识别的框架前缀、服务/环境/实例包装及excerpt元数据，保留topic，未知原文不删。时间、服务和原文由宿主证据提供。缺代码就明示未核对，阻断缺口与扩展取证限制分别保留，引用/源码行匹配不等于因果或线上版本已验证。新报告与旧final映射共用renderer；protected普通Markdown或校验失败转同结构部分报告，不泄漏内部JSON，不新增polish模型调用。旧回归模型已查出stock，fallback另由裸报告/wrapper、subject字符串、10条timeline超旧上限8及C未与trigger L join等契约问题触发；本轮支持strict bare、省略queries的报告envelope，仅在已核代码claim与trigger的L引用有交集时机械补C关联，不改level/结论text，也不绕过无效代码检查。失败按request记录稳定code。恢复fixture是实际47/55/63形状脱敏材料加synthetic evidence/toy source，不是完整原OAP。build标记`report-v2-readable-concurrency32`，构建与测试结果待主流程通知。统一结构不保证同提示每次逐字或语义相同；非排障显式JSON不变。DSH排障最终正文仍全缓冲至Done，进度与审批照常显示；两档引擎最终答复保留宿主 `⏱ 本次耗时：…`，从消息入口含排队计至final准备，不代表送达时延；重试与历史沿用冻结正文。本轮文档核对未运行测试、构建或真实回放，历史通过数不转记为本轮结果。详见 [报告契约与回复边界](<docs/wecom-dsh-engine.md>)。
  - **会话与共享任务（2026-10-29，集成验收待主流程收口）**：私聊按机器人+用户、普通群聊按机器人+群+用户隔离；同群通过「创建共享任务 NAME / 加入任务 CODE / 任务状态 / 退出任务」显式共享上下文，仅创建者可「结束任务 CODE」。任务元数据入本机数据库，24小时无活动过期，全库最多200个有效任务、每任务32名成员。群内所有回复仍对全群可见，本地历史隔离不等于私密；旧群历史与机器人归属未知的旧私聊不自动迁入，新会话空白开始。全局活动回合支持1–32，默认2，保留3/6等已存合法值；设置页展示后端实际活动、生效上限、同会话等待与有效共享任务数。32不是HTTP并发，每OAP最多2在途约束保持。详见 [会话与并发边界](<docs/wecom-dsh-engine.md>)。
  - **请求级诊断增强（2026-10-29，新增实现 / 集成待验收）**：运行诊断新增按 requestId、phase、status、时间筛选及单请求查看，支持复制已加载事件摘要与原生保存对话框导出 JSON。诊断只留 typed 结构元数据，不含问答、业务日志或报告全文；JSONL 按单文件 5 MiB、总共 5 份、7 天 lazy 保留。后端每页最多扫描 1 MiB / 返回 200 事件，普通列表每页 50、最多 10 页 / 500 事件；单请求导出也是有界查询，保留 `partial/readErrors/health`，不冒充完整链路。可粘贴旧原始 requestId 查询：`wecom-preflight-…` 归一到 `wecom-…` 后哈希；旧历史缺 ID 不猜关联。游标遇追加、覆盖或过期变更提示 `DIAGNOSTICS_CURSOR_STALE`，需刷新；纯 rename 轮转且原身份未变可续读。静默消息 reply 登记 RAII 释放、配置代际/队列失效、DSH 启动锁与 WS 旧代发布保护已修；DSH MCP 改为独立顶层配置，无天玑 model 快照也可传，显式模型/effort 配置失败或 20 秒配置超时会中止本轮模型任务。Unicode 脱敏及启动/ACP stderr 截断前脱敏已补，日志读取错误不再显示为空。ingress/policy/排队/阶段结束/模型选择与完成/采集及query/报告校验/最终发送与平台ACK事件已接入；统一business terminal语义及真实企微/OAP验收仍待收口；配置 ACK 不证明实际 provider 使用，平台 ACK 不代表用户已读。第三批首批并行暂未启用，首轮18预算低于20阈值的公共入口回归保留串行事实，不声称提速。详见 [诊断使用与边界](<docs/wecom-dsh-engine.md#请求级诊断增强2026-10-29>)、[代码审查与剩余建议](<docs/wecom-diagnostics-review.md>)。
  - **取消回复脱敏与证据去重（2026-10-29，本地通过、真实待验收）**：按用户明确批准，移除 `redact_reply` 及编排/proto/report display全部回复脱敏，保留Markdown/HTML/control转义与内部diagnostic脱敏。完整业务ID、时间、地址、路径及必要字段保留，凭据/token、手机号也不再自动遮盖，**群内回复全群可见**；不主动搜集无关秘密。两档引擎仍完整缓冲至Done，排障保留报告/引用/源码校验，不恢复未经验证token流；进度/审批、异常/取消收尾、首次final冻结、宿主耗时、同正文重试/history边界不变。报告按证据ID全局每块只展示一次，共享6短引文/8块预算；后续claim只列refs，code补日志refs。严格识别后才去框架前缀及服务/环境/实例包装，保留topic，未知原文不删；纵向结构与高亮保留，不增polish。本轮 `cargo test wecom_` 393通过/0失败/2忽略，最终全量954通过/0失败/14忽略，diff检查通过，保留既有unused/dead_code warnings；移除旧回复脱敏测试，因此不是历史957。957、397/0/2及前端build保留为上一轮记录。本轮未改前端、未跑npm build、未重启/发版，真实企微待验收。详见 [最新回复边界](<docs/wecom-dsh-engine.md#取消回复脱敏与证据去重2026-10-29当前增量>)。
  - **企微阅读排版**：简短结论标题、长业务ID逐行、可读时间范围与明确时区；摘要突出、原因正文克制加粗，原日志引用独立展示。字体与字号由企微客户端决定，不注入HTML/CSS，不增加模型润色调用。
  - **日志日期查询**：支持“10月26号/10月26日”、完整年月日、昨天/前天自然日及“近三天”滚动72小时。明确日期解析失败或冲突先澄清，不再悄悄回退当前4小时；首次查询提示实际范围，OAP与报告使用同一范围。见[日期语义](<docs/wecom-dsh-engine.md#明确日期与相对日期查询2026-10-29>)。
  - **配置自我介绍**：回答设置可填写完整介绍；“你好 / 你是谁 / hi”等短句直接回配置正文，不走模型。“新对话，你好”先安全重置当前会话再介绍；带实际业务问题的问候不会被吞。统一2000个Unicode字符，空配置使用内置默认介绍。详见[自我介绍与新对话](<docs/wecom-dsh-engine.md#配置自我介绍与新对话问候2026-10-29>)。
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
