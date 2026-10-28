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
  拒绝 / 取消）；DSH 回合看门狗 600 秒，超时先 `session/cancel` 再收尾（日志采集与补查合计另限 120 秒累计采集耗时）。

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

日志取证 v2 先由 Rust 验证日志源、采集入口证据，再允许 DSH 定点读代码并提出 JSON 补查计划。
补查仍由 Rust 校验和执行，最多 2 轮；不是向 DSH 注册了 SkyWalking MCP，也不是自由执行任意查询。
通道的普通 MCP 配置是另一条能力。

- **日志源优先**：地址支持 OAP 基础地址或完整 `/graphql` 地址，不重复追加路径。首次使用该配置先做
  `__typename` 与一条 `queryLogs` 能力探测，共 2 次 HTTP（计入总预算，首步失败即停止）；成功结果缓存 5 分钟。
  探测查空也可表示日志源就绪，探测中的日志不算业务证据。错误保留脱敏端点、HTTP 状态与 content-type。
- **查询条件**：确定性解析 RFC3339 起止时间、中文「今天10点到11点」「昨天」「最近两小时」，
  以及 `关键词=ORD123456`、引号短语、服务名、traceId。订单号、支付号等业务 ID 分组分别查询，避免把不同字段 ID 强行 AND；
  普通多关键词默认 AND，「任一 / OR」分别查询前 3 个词。没有明确时间时默认最近 4 小时。
  单个故障时刻先查前后 15 分钟；仅带「大概 / 左右」等近似标记且该组成功查空时，可补查前后 60 分钟。
  明确起止范围及相对窗口不走该自动扩窗；未识别或反向时间范围先返回条件待确认。
- **时间与时区**：设置页可选 OAP 固定 UTC 偏移或跟随本机；固定偏移不自动切换夏令时。
  无时区的中文时间按所选偏移解释，RFC3339 保留明确时刻；结果列出实际起止时间与偏移。
- **采集顺序**：入口日志优先保留最早事件、跳过/退出等业务决策与末次事件，选最多 3 条 trace，再补错误线索；读取调用链，再查同 trace 跨服务日志；
  对链路上尚缺日志的服务/实例做 span 前后 30 秒短窗补查，窗口不超出用户范围。
  下游查询不沿用入口关键词，避免下游缺少订单号时被过滤；明确属于其它 trace 的记录被排除。
- **性能问题**：用户提到慢、延迟、性能或吞吐时，在剩余预算至少 5 次请求的情况下补一个服务的端点延迟/吞吐 TopN 与实例清单；这是窗口聚合值，不是趋势曲线。
- **WARN / 决策上下文**：WARN 与跳过、过滤为 0 等 decision 线索优先进入日志摘录；长正文保留 cause/异常重点，
  行头带原长度、SHA-256 摘要与截断标记。这里仍是有界摘录，不是任意展开完整线上原文的接口。
  基于已观察到的服务、实例、线程/日志器与时间戳，在原窗口内查前后 2 秒；没有新增匹配上下文时可放宽到前后 30 秒。
  同线程或短窗记录仍只是候选；不同非空 trace 不串联，无 trace 的 WARN 不因请求携带 trace 约束而被提前过滤。
- **证据强度与结论层级**：区分「入口条件匹配」「同 trace 精确关联」与「同服务/线程短时间窗候选」。
  结论分 `result`（结果）、`direct_cause`（直接原因）、`trigger_verified`（具体触发已核验），缺字段回落 `unknown`。
  过滤为 0 / `STATUS_FILTERED_EMPTY` 只是结果或失败出口，不唯一指向去重、库存、价格等具体规则；normal trace 仅未报告 APM 技术错误，仍可能有业务 WARN、被捕获异常或降级。
  JSON 计划要求最高层有实际证据引用且无自报缺口；引用本身不证明因果，模型仍须用实际规则、判定值及 WARN/异常证据支持结论。
  编排层遇未解释决策且模型拟停止时，可在预算内强制一次证据锚定回溯；有未完成页或截断时限制最高结论层级。
  工作区版本未经核对时不宣称等于线上版本。
- **执行状态**：`not_executed`（未查询）、`source_error`（日志源失败）、`empty`（未取得业务证据）、
  `evidence`（已有证据）、`partial`（部分覆盖）分别返回。排障请求的解析/配置失败和源失败由 Rust 直接确定性回复，
  不交模型改写成「查不到日志」。`empty` 且 `sourceReady=true` 时可进入下面的代码线索补查；
  补查后仍无真实证据，则直接说明未生成业务根因结论。
- **代码线索补查**：最多 2 轮 DSH JSON 计划，每轮最多 3 个查询；校验失败可修正一次，仍失败就停止补查。
  校验器只接受固定 schema，不接受 URL、shell 或任意 GraphQL，时间只能在原窗口内收窄。
  `code_ref` 必须指向工作区内真实普通 UTF-8 文件（≤512 KiB），禁止路径穿越和符号链接/重解析点；行号与单行 excerpt 要匹配。
  新 service/keyword 须见于真实证据或核验过的源码行，源码只能构成候选；traceId 必须见于返回的证据。
  原问题带业务 ID 时，无证据确认的 traceId 就必须保留至少一个原始 ID，不把源码模板变成全站泛查。
  补查引用使用 `[R1L1]`、`[R1T1]`、`[R2L1]` 等，校验引用记录内确实含对应实体。
- **共享预算**：首次采集和最多 2 轮补查合计 **24 次实际 HTTP、120,000 毫秒累计采集耗时**，
  探测、失败及服务 ID 查询都计请求；模型规划耗时不混入采集计时，但整个 DSH 回合仍受 **600 秒**看门狗约束。
  单次采集保留上限为 300 条日志、3 条 trace、6 个关联服务/实例补查目标；入口最多 3 页、同 trace 最多 2 页，每页 40 条。
  定点日志补查按实际请求的 `page_size` 判满（包括每页 30 条），满页在该查询最多读 3 页；达到上限、失败或预算不足保留未完成分页标记。
  单次注入正文限 32,000 字符，补查累计上下文限 48,000 字符；累计超限保留初始摘录与最新补查摘录，同时标记截断与 `partial`。
  超时或预算耗尽返回已有证据与 `partial`，
  任何截断均限制覆盖；空结果、失败和未覆盖不等同于「线上没有异常」。
- **有限追问**：可在聊天中要求「扩大到8小时」「只看 payment 服务」「下一页」「继续查下游」。
  Rust 仅在内存中保存查询条件、trace/分页游标和已查服务，30 分钟过期、最多 200 个上下文，
  按机器人/会话/用户/工作区/OAP 配置隔离。该缓存不存日志正文、不落盘；这不表示 DSH 会话或现有问答旁证不留内容。
  再次追问仍受同样预算约束；「停止」可取消采集，停止后不启动该轮模型分析。
- **解析限制**：目前支持全局 AND/OR，不支持嵌套逻辑表达式；中文数字支持常见两位数。模糊扩大窗口需补长度；未覆盖的前天/上周等说法提示明确日期。复杂短语请加引号，多个字段用分号分隔。
- **诊断**：设置页显示当前窗口所连后端的 PID、buildLabel、最近排障状态与 `requestId/detail`，
  以及实际 HTTP 请求、采集证据、展示日志、采集 WARN、截断、待查页和 `sourceReady`；原说明折叠区逐条显示 `decisionGaps`。
  数值直接使用该实例 `lastInvestigation` 字段，不从回复文案或另一实例推断；缺字段或 null 显示「未提供」，真实 0 保持 0。
  `decisionGaps=[]` 仅表示后端未报告未解释决策，不表示具体触发原因已经验证。安装版与开发版并存时先核对 PID/build。
  通道日志 `investigation-step` 记录阶段、条件摘要哈希、页号、结果和耗时，`investigation` 记录实例/请求标识和实际计数，
  不直接写入采集正文或鉴权头。需联调不同 OAP 版本与实际时区；本节不代表 v2 测试或线上验收已经通过。

### 诊断字段口径

| `lastInvestigation` 字段 | 本机展示口径 |
| --- | --- |
| `requestCount` | 共享预算内实际 HTTP 请求，含探测、失败与服务 ID 解析 |
| `evidenceCount` | 采集日志与 trace spans 合计，不代表全部已注入模型 |
| `shownCount` | 各次有界证据渲染中展示的日志条目数，不含 trace spans；累计上下文再次截断时，不作为最终可见日志的精确总数 |
| `warnCount` | 采集日志中 WARN/WARNING 条数，不推测其中展示了多少 |
| `truncatedCount` | 后端报告的日志截断/未展示条目与累计上下文截断标记；不是丢失字符数，也不是 `evidenceCount - shownCount` |
| `incompletePages` | 未完成分页标记的累计数/下限，不是 OAP 精确剩余页总数 |
| `decisionGaps` | 后端记录的采集上下文缺口与模型报告的未解释决策；按原文列出，不由 UI 判断根因 |

首次与补查累计计数可能含重复查询，不宣称跨轮唯一。缺字段与真实 0、空数组分别显示；这些字段不表示已验证具体业务根因。

### WARN 取证本轮前端验证（2026-10-28）

- `node node_modules/typescript/bin/tsc --noEmit`、`npm run build`：均通过（保留既有打包体积与混合导入警告）。
- Rust 全量测试：719 通过、0 失败、13 忽略；覆盖30条满页后的WARN、已有trace时的上下文回填、跨logger但同实例候选、长日志原因摘录与分层结论。
- 最后源码快照企微定向回归：159 通过、0 失败、1 忽略。
- 新版运行标识为 `investigation-v3-warn`，用于区分已运行旧实例；不自动重启通道。
- 本组件 Node SSR 定向 fixture：中英文共 10 次渲染，覆盖缺字段、真实 0、有值、null/非法数值、无排障记录；同时核对 `decisionGaps` 空数组与缺字段区分、原文 HTML 转义，全部断言通过。
- 这些检查验证类型与渲染数据分支，不是桌面视觉验收或真实 OAP/企微端到端验证；本轮后端定向验证结果单独记录。

### 既有 v2 本地验证（2026-10-28，WARN 取证修正前）

以下为前轮记录，不代表本轮 WARN 改动已完成全量构建或线上验收。

- 全量 `cargo test`：704 通过、0 失败、13 忽略；最新企微定向回归 145 通过、0 失败、1 忽略，包含截图原文集成回归。
- `cargo build --manifest-path src-tauri/Cargo.toml --bin ai_workbench` 已生成新版开发可执行文件；没有自动替换安装版或启动新通道。
- `npm run build` 类型检查和前端构建通过；保留原有体积/混合导入警告。
- 验证涵盖单故障时刻、分离业务 ID、405 与解析失败零业务查询、原窗口约束、源码候选校验、代码补查参数校验及扩窗后的关联服务日志。
- 真正业务根因仍须由实际配置的 OAP 回放确认；本地 mock 与源码回归不代表目标业务已定位。安装版与开发版可并存，联调前先在诊断页核对 PID 与 `investigation-v2` 构建标记。

### 既有本地验证记录（2026-10-28，v2 改动前）

以下保留既有记录，不作为日志取证 v2 的通过结论。

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
| `src-tauri/src/wecom_investigation_plan.rs` | DSH JSON 补查计划校验、真实源码行核验与证据引用边界 |
| `src-tauri/src/wecom_skywalking.rs` | OAP GraphQL、实际 HTTP 预算、分页与时间过滤 |
| `src-tauri/src/dsh_commands.rs` | `dsh_cli` / `dsh_installed_version` / `dsh_clear_stale_lock` 等复用入口 |
| `src/lib/wecomChannel.ts` / `components/WecomSettingsCard.tsx` | 引擎下拉、DSH 状态胶囊、档位映射说明 |
