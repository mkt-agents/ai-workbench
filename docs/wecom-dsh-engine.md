# 企微机器人：AI 引擎切换（天玑 / DeepSeek Harness）

> 2026-10-27。给「联网扩展 → 企业微信机器人」加了一个**通道级**的引擎开关：
> 同一个企微机器人，回话的工作可以交给内置天玑，也可以交给本机安装的
> DeepSeek Harness（`dsh --profile acp`）。

## 怎么切

设置 → 联网扩展 → 企业微信机器人 → 连接 → **AI 引擎**：

| 选项 | 实际执行者 | 依赖 |
| --- | --- | --- |
| 天玑（内置，默认） | 本应用 `agent_run`（四档权限、工具、审批卡） | 本应用「模型配置」里至少一个模型 |
| DeepSeek Harness | 常驻 `dsh --profile acp` 子进程（ACP / JSON-RPC） | 已安装 `@deepseek-ai/dsh`；「工作区路径」必须是绝对路径 |

切换保存即生效并自动重连，私聊与群聊统一走所选引擎。DSH 未安装 / 工作区为空时
`apply` 直接返回可操作错误，**不会静默回落天玑**。

配置项：`settings.wecomEngine`（`tianji` | `dsh`，缺省天玑）。下发时另带
`permission` 与 `workspace` 两个顶层字段，后端 `wecom_channel_apply` 用它们
配置 DSH，不再要求天玑的模型快照。

## 模型从哪来（profile 镜像）

DSH 的每个 profile 是**独立组合**：`~/.dsh/profiles/<name>/package.json` 里的
`dsh.profile.bundles` 决定挂哪些插件，`cordis.patch.yml` 是这一档的用户层。
用户真正的模型 provider 往往在 **`web` profile**（第三方插件），而 `acp` 默认只有
`dsh-base` + `dsh-acp-app` —— 于是默认路由落到 `deepseek-official`，一跑就
`no API key for provider route "deepseek-official"（code -32603）`。

所以引擎启动前会先镜像一次 `~/.dsh/profiles/acp`：

| 项目 | 做法 |
| --- | --- |
| bundle 列表 | 照抄 web，`@deepseek-ai/dsh-web-app` 换成 `@deepseek-ai/dsh-acp-app`，丢掉 `@dsh-market/plugin`、`dshmarket`（纯 UI） |
| `node_modules` | 目录联接（`mklink /J`）指向 web 的，不重装、不联网；已有目录不删 |
| `cordis.patch.yml` | 照抄 web，滤掉 `ui-*` 与 market 条目（文本切块，`!!js` 标签原样保留） |
| `compatibility.json` | 一并复制（第三方插件的版本豁免） |
| 依赖 | 与 web 一致，保证 `node_modules` 解析 |

只写 `acp` 目录，绝不改 `profiles/web` 与 `.credentials.yaml`。在 DSH 里改了插件或
模型后，点设置页「重新同步」（`wecom_dsh_sync`），引擎会重启并重新拉模型目录。

### 模型选择

- 「DSH 模型」下拉来自 ACP 的 `configOptions`（`wecom_dsh_models` 用一条探针会话
  读出来，随后 `session/close` 关掉）；按 provider 分组，值是不透明的
  `["provider","model"]`。
- 「思考强度」对应 `reasoning_effort`，模型/路由不支持时整栏隐藏。
- 选择通过 `session/set_config_option` 在每轮开始时下发（值没变则跳过）。
- 选「自动」时：先用模型目录里**真有凭据**的第一项（或与 DSH 当前值一致的那项），
  避免 DSH 默认路由指向没 key 的 provider。

### 已知限制（DSH 架构导致）

- 只依赖 `llm` / `credentials` 的 provider（`deepseek-official`、`deepseek-account`、
  `llm-pi-ai` 里配好的第三方）能直接在 ACP 里用。实测 `deepseek-account`
  会列出 `deepseek-v4.1-flash` / `deepseek-v4-pro`，账号登录态是 home 层共享的。
- 依赖 web 端 `connection` 服务的插件 provider（如 `dsh-provider-qoder`、
  `dsh-purge/redteam/ui`）在 ACP 里会停在 `pending (waiting for service: connection)`，
  模型不会出现在下拉里 —— 那是浏览器侧 RPC 桥，stdio 进程没有。镜像已经把能带的都带上，
  这类 provider 只能等 DSH 侧提供非 web 的入口。
- 没有凭据的路由不会出现在模型列表里（DSH 的 `listModels` 会隐藏），所以「列表为空」
  基本等于「DSH 里还没配 key / 没登录」。

## DSH 引擎怎么工作

- **进程**：`node <pkg>/lib/bin.js --profile acp --patch <patch.yml>`，cwd = 工作区，
  stdin/stdout 走 ACP（换行分隔 JSON-RPC），stderr 只做诊断（保留尾巴进状态）。
  优先复用已安装的入口（比 `npx` 省 ~2.5 秒），找不到时回退 `npx @deepseek-ai/dsh`。
- **会话**：企微会话键（`wecom:dm:<userid>` / `wecom:group:<chatid>`）映射到一个
  ACP `sessionId`，落盘在 `%APPDATA%\com.ai-workbench.app\wecom-dsh\sessions.json`。
  下一条消息先 `session/resume`（工作区变了会失败，失败就 `session/new` 重开，上下文随之重置）。
  **多轮上下文由 DSH 自己维护**，不再把 `wecom_session` 的历史塞进 prompt；旁证
  JSONL 仍然照旧写（切回天玑时还能续上）。
- **流式**：`session/update` 的 `agent_message_chunk` 累积后按 400ms 节流回写企微
  流式消息，首 40 字立即推；`agent_thought_chunk` / `tool_call` / `usage_update`
  不逐步播报，避免刷屏。注意 ACP 的文本是「提交点」粒度，不是逐 token。
- **收尾**：`session/prompt` 的 `stopReason` 映射成一句提示（截断 / 工具步数上限 /
  拒绝 / 取消）；DSH 回合看门狗 600 秒，超时先 `session/cancel` 再收尾（日志采集另有 90 秒预算）。

## 权限档位 → DSH 沙箱

渠道权限（`settings.wecomPermission`）在启动时写进 `--patch` 覆盖层
（`%APPDATA%\com.ai-workbench.app\wecom-dsh\permission.patch.yml`），
**不动用户的 `~/.dsh` 配置**；档位或工作区变化即重建 patch 并重启进程。

| 渠道权限 | sandbox `mode` | approval `policy` | 说明 |
| --- | --- | --- | --- |
| 只读 `read` | `read-only` | `ask` | 不能改文件；需要授权的动作仍会问 |
| 编辑 `workspace` | `workspace-write` | `ask` | 只能改会话 cwd 之下 |
| 审查 `review` | `workspace-write` | `ask` | DSH 没有「AI 先审」，与编辑档相同（UI 已注明） |
| 完全 `full` | `danger-full-access` | `never` | 不限制、不询问 |

覆盖层同时关掉 `permission`（权限预设）整行：ACP 面没有模式切换入口，留着只会让
DSH 自己的 `defaultPreset` 反过来覆盖我们钉死的档位。`--patch` 是**追加覆盖**，
但单个条目是整体赋值，所以 `config` 必须写全（本文件由程序生成，勿手改）。

天玑专属的安全机制（冲突仓库拒绝、受保护路径、命令黑名单、删除进回收站、diff 审批卡）
**只在 天玑 引擎生效**；DSH 档由它自己的沙箱 + 审批策略兜底。

## 审批怎么走

DSH 请求授权时发 `session/request_permission`（选项固定为 `allow-once` /
`reject-once`）。编排层把它登记成 4 位审批码，在对话里发一条提示：

```
🔐 DeepSeek Harness 需要你确认：<工具摘要>
回复「AB12 1」同意；回复「AB12 2」拒绝（5 分钟内有效）。
```

- 用户回复「码 1 / 码 2」→ 立刻回执 ACP（`selected: allow-once` / `selected: reject-once`），
  不需要切桌面。
- **没有「3 本轮同类都允许」**：ACP 只有一次性授权；回 3 会得到一句说明。
- 超时 5 分钟、或回合结束仍挂着 → 统一回 `cancelled`，不让 DSH 的工具调用悬着。
- 与天玑档的审批码共用 `pending_approvals` 表与同一套编码（36⁴ 循环），
  区别只是载荷（天玑记 `call_ids`，DSH 记等待端）。

## 边界与已知差异

- **图片/文件**：ACP 图片提示需要 attachment store + 支持图像的 route，首期降级为
  文本线索（`（图片「名」：url）`），与「天玑非 vision 模型」同一处理。
- **出站富媒体**：`wecom_send_image` / `wecom_send_file` 是天玑档的工具（依赖回合的
  `last_target`），DSH 档不会下发，也不会被调用。
- **首次使用**：`~/.dsh/profiles/acp` 不存在时要初始化（可能要装依赖，几十秒到几分钟）。
  启动超过 3 秒会先单独发一条「正在启动 DeepSeek Harness…」的消息；失败原因进
  设置页的「DSH 引擎状态」。
- **模型与登录**：ACP 档继承 `dsh-base` 的 credentials / authorization /
  deepseek-account，沿用 DSH GUI 里已配好的模型与账号；本应用的模型配置只对
  天玑档生效。

## 两档能力对照（「原版引擎 100%」的边界）

天玑档走本应用 `agent_run`，能力取决于我们的工具注册表；DSH 档走外部 ACP 进程，
能力取决于 DSH 自己的工具集。两者**不是逐项等价**，刻意做了如下取舍：

| 能力 | 天玑档 | DSH 档 | 说明 |
| --- | --- | --- | --- |
| 读代码 / 改文件 / 跑命令 | ✅ 四档权限 + 审批卡 | ✅ 沙箱 + ACP 审批码 | 档位映射见上表 |
| 文档问答模式（auto/doc/doc_code/code） | ✅ 注入系统提示 | ✅ 注入 ACP prompt 首个 text block | `answer_mode_rules` 两档共用；`auto`（默认）按本条消息意图分流 |
| SkyWalking 排障 | ✅ `skywalking_query` 工具（日志 / trace / 指标） | ⚠️ Rust 有限采集 → 证据注入；支持条件追问后再次采集 | 入口日志 → 最多 3 条 trace → 关联服务日志补查；不是 DSH 自主调用 SkyWalking MCP，也不是任意多轮工具执行，预算与覆盖见下节 |
| MCP 服务 | ✅ 随 `agent_run` 下发 | ✅ 随 `session/new`\|`resume` 的 `mcpServers` 下发 | 2026-10-28 补齐：企微通道配的 MCP 两档都可用（DSH 需总开关开） |
| 企微业务（文档/日程/智能表格 8 工具） | ✅ `wecom_*` 工具 | ❌ 不下发 | DSH 用自己的工具集，没有这组企微 Open API 工具 |
| 出站富媒体（发图/文件回对话） | ✅ `wecom_send_image/file`（Sensitive 档） | ❌ 不下发 | 依赖回合 `last_target`，ACP 侧无对应通道 |
| 入站图片（vision） | ✅ 内联 image 块 | ✅ ACP 宣告 `promptCapabilities.image` 时内联 | 模型不支持时都降级为文本线索 |
| 审批粒度 | 一次 / 本轮同类 / 以后都允许 | 仅一次性（ACP 限制） | 企微里回「码 3」在 DSH 档会被明确告知只有一次性 |
| 天玑专属安全（冲突仓库拒绝 / 受保护路径 / 命令黑名单 / 删除进回收站 / diff 审批卡） | ✅ | ❌（由 DSH 自己的沙箱兜底） | 见上「权限档位 → DSH 沙箱」 |

> 结论：**DSH 档在「用 DSH 原生模型/工具跑通用任务」上与原版一致**；企微专属的业务工具与出站富媒体**只在天玑档**提供。要求「企微业务 + 发图回群」的场景请用天玑档。

## DSH 日志排障：有限采集与证据边界

SkyWalking 查询由 Rust 编排层在 ACP prompt 之前执行，再把证据交给 DSH 对照代码分析。
这是内置采集流程，不是向 DSH 注册了 SkyWalking MCP；通道的普通 MCP 配置是另一条能力。

- **查询条件**：确定性解析 RFC3339 起止时间、中文「今天10点到11点」「昨天」「最近两小时」，
  以及 `关键词=ORD123456`、引号短语、服务名、traceId。多关键词默认 AND；「任一 / OR」分开查询，
  最多覆盖前 3 个关键词。没有明确时间时默认最近 4 小时；未识别或反向时间范围先返回条件待确认，不静默扩大查询。
- **时间与时区**：设置页可选 OAP 固定 UTC 偏移或跟随本机；固定偏移不自动切换夏令时。
  无时区的中文时间按所选偏移解释，RFC3339 保留明确时刻；结果列出实际起止时间与偏移。
- **采集顺序**：入口日志按错误优先选最多 3 条 trace，读取调用链，再查同 trace 跨服务日志；
  对链路上尚缺日志的服务/实例做 span 前后 30 秒短窗补查，窗口不超出用户范围。
  下游查询不沿用入口关键词，避免下游缺少订单号时被过滤；明确属于其它 trace 的记录被排除。
- **性能问题**：用户提到慢、延迟、性能或吞吐时，在剩余预算至少 5 次请求的情况下补一个服务的端点延迟/吞吐 TopN 与实例清单；这是窗口聚合值，不是趋势曲线。
- **证据强度**：区分「入口条件匹配」「同 trace 精确关联」与「同服务短时间窗候选」。
  短窗命中不证明同一请求，报错 span 不直接证明根因；回答需列出证据编号、根因候选和未完成项，
  工作区版本未经核对时不宣称等于线上版本。
- **每轮预算**：90 秒、最多 16 次实际 HTTP 请求（失败及服务 ID 查询也计数）、300 条日志、
  3 条 trace、6 个关联服务/实例补查目标；入口最多 3 页、同 trace 最多 2 页，每页 40 条。
  注入正文另限 32,000 字符。错误、超时或采集预算耗尽标记 `partial` 并返回已有证据；正文截断也会说明覆盖不完整。
  空结果、查询失败和未覆盖分开表达，不把任何一种当成「线上没有异常」。
- **有限追问**：可在聊天中要求「扩大到8小时」「只看 payment 服务」「下一页」「继续查下游」。
  Rust 仅在内存中保存查询条件、trace/分页游标和已查服务，30 分钟过期、最多 200 个上下文，
  按机器人/会话/用户/工作区/OAP 配置隔离。该缓存不存日志正文、不落盘；这不表示 DSH 会话或现有问答旁证不留内容。
  再次追问仍受同样预算约束；「停止」可取消采集，停止后不启动该轮模型分析。
- **解析限制**：目前支持全局 AND/OR，不支持嵌套逻辑表达式；中文数字支持常见两位数。模糊扩大窗口需补长度；未覆盖的前天/上周等说法提示明确日期。复杂短语请加引号，多个字段用分号分隔。
- **诊断**：通道日志的 `investigation-step` 行记录查询类别、条件摘要哈希、页号、结果和耗时，`investigation` 行记录请求标识、结果、HTTP 数量、日志/trace 数量和耗时，
  不直接写入采集正文或鉴权头。需联调不同 OAP 版本与实际时区；本节不代表测试或线上验收已经通过。

### 本轮本地验证（2026-10-28）

- `cargo test --manifest-path src-tauri/Cargo.toml`：673 通过、0 失败、13 忽略；包含本地 mock OAP 的跨服务链路、第二页异常、短窗候选、鉴权错误、预算及取消测试。
- `npm run build`：类型检查及前端构建通过；保留现有打包体积与混合导入警告。
- 此结果不替代真实企微/OAP版本联调，未安装或重启桌面应用。

## 代码位置

| 文件 | 作用 |
| --- | --- |
| `src-tauri/src/wecom_dsh.rs` | DSH 引擎：进程生命周期、ACP 收发与 id 路由、会话映射、patch 生成、档位映射、模型目录与选择 |
| `src-tauri/src/wecom_dsh_profile.rs` | profile 镜像：bundle 映射、patch 过滤、`node_modules` 联接、镜像报告 |
| `src-tauri/src/wecom_channel.rs` | `engine`/`permission`/`workspace` 入参、DSH 校验与状态、按引擎启停、`PendingApproval` 枚举 |
| `src-tauri/src/wecom_orchestrator.rs` | `on_msg` 引擎分流、`run_dsh_turn`、审批码回灌（两种载荷） |
| `src-tauri/src/wecom_investigation.rs` | 有限采集、trace/服务补查、证据强度、预算与短期追问上下文 |
| `src-tauri/src/wecom_investigation_query.rs` | 无 IO 的查询条件、明确时间与追问解析 |
| `src-tauri/src/wecom_skywalking.rs` | OAP GraphQL、实际 HTTP 预算、分页与时间过滤 |
| `src-tauri/src/dsh_commands.rs` | `dsh_cli` / `dsh_installed_version` / `dsh_clear_stale_lock` 等复用入口 |
| `src/lib/wecomChannel.ts` / `components/WecomSettingsCard.tsx` | 引擎下拉、DSH 状态胶囊、档位映射说明 |
