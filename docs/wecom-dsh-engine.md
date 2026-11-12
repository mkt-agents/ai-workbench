# 企微机器人：AI 引擎切换（天玑 / DeepSeek Harness）

> 2026-10-27 引入通道级引擎开关；2026-10-28 起入口为「IM机器人 → 企业微信」：
> 同一个企微机器人，回话的工作可以交给内置天玑，也可以交给本机安装的
> DeepSeek Harness（`dsh --profile acp`）。

## 多 bot 运行实例（2026-11-11）

IM 机器人支持同一进程内挂多个企微 bot：每个 bot 独立长连接、`Orchestrator`、问答 DSH 与文档 DSH 子进程。DSH 落盘命名空间为 `bot-{sanitizedBotId}` / `docgen-{sanitizedBotId}`（`sessions-*.json`、`permission-*.patch.yml`），与旧版单实例空 tag 并存迁移。设置页 apply 下发 `{ bots: [...] }` 时后端按 bot 增删改 reconcile；通道状态聚合在线数 `botsOnline/botsTotal`。

## 企微回复阅读排版（2026-11-03 短约束）

官方 `stream.content` 仅「常见 Markdown」，**不渲染 Mermaid/PlantUML 等流程图 DSL**。每轮 `reply_output_rules()` 压成极短 IM 约束：**鼓励**关键结论/ID/枚举/路径加粗，可用 `##` / `###` 分段；禁 HTML、禁 DSL（漏网 mermaid 由 `prepare_wecom_visible_markdown` / `rewrite_diagram_fences` 转 ```text ASCII）；无业务示例。说话风格交给设置页「上下文增强」。客户端字体/字号仍由企微决定。排障报告侧：结论置顶、证据去重与短引文预算等既有渲染规则不变；自我介绍与耗时尾注不改写。

## 明确日期与相对日期查询（2026-10-29）

修复“时间是10月26号”漏识别后静默查询当前4小时的问题；18:56请求的实际OAP submittedStart/end曾错误落在10月29日14:56—18:56，非报告排版或UTC偏移问题。该次证据不能回答26日业务原因，需使用正确范围重新查询。

- `10月26号/10月26日`：省略年份取查询时区当前年；落在未来则请用户补年份，不擅自取去年。无时刻时查该日00:00至次日00:00。
- `2026年10月26日`、`2026-10-26`：明确日期全天；支持日期加`10点到11点`、`11:31:12`，单故障时刻沿用±15分钟。
- `昨天`：前一个自然日00:00至今天00:00；`前天`：前两个自然日00:00至昨天00:00。两者也支持明确时刻。
- `近三天/最近三天/过去三天/近3天`：从当前查询时刻往前72小时，不是三个自然日。例如29日18:56查询，范围26日18:56至29日18:56。
- 同一查询混合冲突日期/相对窗口、非法月日、缺月的`26号`、残缺时刻等应先澄清，零HTTP，不回退默认4小时。引号关键词及业务ID先屏蔽，避免把关键词日期/订单号误当查询时间。

首次采集进度显示实际查询范围。范围传给源探测、实际日志查询与补查，报告继续展示宿主范围，不仅修标题。未指定日期才允许默认4小时。跨年范围建议明确两端完整年份；暂不完整支持的中文时分秒组合明确拒绝而非丢掉秒。

本轮Rust全量965通过、0失败、14忽略；包含8个公共Mock OAP回归与日期纯函数测试。最初固定时钟用例已复现错误回退29日4小时，修复后实际HTTP queryDuration与fallback报告均断言26日全天；昨天/前天/近三天分别验证自然日和滚动72h。diff检查通过，保留已有编译警告；本轮无前端代码变更，未重启真实通道、未发送真实查询。

## 非标准日期字面量与未执行原因（2026-10-30）

修复 `时间：2026 10-30 10:12:13- 2026 10-30 16:12:13`（空格分隔年）整条查询被拒、回复裸「查询条件未解析」的问题（msgid `f496a366b32f764be824e5909fb91ad3`）。三层修复，详见 `docs/wecom-timeliteral-20261030.md`：

- **解析前归一化**：`2026 10-30`、`2026/10/30`、`2026.10.30`、无年 `10-30 16:12:13` 在 `parse_query` 入口统一归一为 `YYYY-MM-DD`；带引号字面量与 `key=value` 业务值（含 `/` `:`）不改写；无效月日（`2-30`）仍交给日历校验报错，未来无年日期仍要求补年份。
- **残留守卫边界化**：`4745BD58-16-C7-…` 这类渠道 key 内部的 `58-16` 不再被判成未识别日期；数字日期片段仅作为独立 token 时才触发澄清。
- **未执行原因贯穿**：`InvestigationOutcome.clarification` 承载 host 拼装的具体原因（含示例格式），经 `fallback_with_reason` 渲染进「证据范围：未执行查询。…」与未完成项首行，并通过 `last_diagnostic` 暴露给设置页诊断面板；不带澄清时旧渲染逐字节不变。

同时：新起会话语义补充 `新起会话/新起对话/重新起会话/开新会话` 等；粘贴 JSON 时引号键名不再混入关键词，关键词总量封顶 8 并记录 `keywordCount` 原始数量。

## 思考强度按模型降级（2026-10-30）

思考强度是全局设置，但 ACP 的 `reasoning_effort` 按**模型**校验：`mimo-v2.6-flash` 这类模型不吃 `high`，`session/set_config_option` 回 `-32602 unknown reasoning effort for <model>: <value>`，整轮在提交前被 fail-closed 掐死（耗时 0 秒）。兼容策略——**思考强度是质量旋钮，不是正确性旋钮**：

- `apply_selection` 里 effort RPC 失败且错误形如「应用思考强度失败…unknown reasoning effort…」时：记住 (模型, 档位) 组合到 `unsupported_efforts`，本轮回落 Provider 默认继续跑；模型 RPC 失败（路由错误）仍然 fail-closed，只有 `应用思考强度失败` 前缀的错误才参与降级判定。
- 记忆命中后下一轮直接跳过 effort RPC（省一次注定失败的往返）；诊断日志记一行 `reasoning_effort=<档> 被模型拒绝，已回落 provider 默认`。
- 性能面板的 effort 标签展示**实际生效值**（回落时为 unknown），不虚报用户设置。
- 换模型/重启进程后组合表仍按模型键控有效；跨进程重启重新学习一次（代价仅一轮降级）。

## 配置自我介绍与新对话问候（2026-10-29）

回答设置的 `wecomSelfIntro` 为两档引擎共用的热更新配置。完整问候（你好/您好/hi/hello等）或身份短句（你是谁/你能做什么等）在通过访问策略后直接发送配置文本，保留换行和emoji，并沿用宿主耗时尾注，不调用模型。空配置回退内置天玑介绍；**打开会话的 `enter_chat` 欢迎语与问候短路共用同一套文案**（`welcome_text`：已配置则用配置，否则 `DEFAULT_SELF_INTRO`）。

`新对话，你好` / `新会话，你是谁` 先识别开头的新会话控制子句，再重置当前作用域的短历史、ACP映射与实际查询缓存键，最后处理剩余问候。不能只把整句话加到身份关键词表；`你好，帮我查日志`、`新对话，帮我查订单`仍处理业务正文。纯`新对话`返回新会话确认。引号、说明、否定语境不当重置。

重置不跨机器人/用户/群作用域；共享任务仅创建者可重置。旧回合持有会话锁时先提示停止或等完成，不在执行过程中清历史；历史写入失败会明确报错，不谎称已重置。自我介绍输入、IPC和后端统一2000个Unicode字符（不是2000个UTF-8字节），避免中文及emoji被额外截断。

验证覆盖配置正文从解析到实际final帧、问候不占模型准入、混合业务文本继续下游。本轮当前源码Rust全量955通过、0失败、14忽略（原生退出码0），前端build与diff检查通过；历史测试数不迁移。真实客户端验收需要更新运行实例，不能把源码/测试通过当作正在运行的旧程序已更新；本轮未重启或向企微发送测试消息。

## 取消回复脱敏与证据去重（2026-10-29，当前增量）

**用户已明确批准去掉企微回复脱敏；本轮本地回归通过，真实企微待验收，未重启、未发版。** 此要求替代上一轮回复遮盖策略。历史Rust全量 **957通过、0失败、14忽略**、`npm run build`通过（工作区TEMP）与diff检查通过仅属上一轮快照；397/0/2是其中间scope，942/911等亦为历史，不作为本轮通过结论。

### 出站文本边界

- 移除 `redact_reply` 及[双引擎编排](<../src-tauri/src/wecom_orchestrator.rs>)、[proto](<../src-tauri/src/wecom_proto.rs>)、[报告display](<../src-tauri/src/wecom_report.rs>)中的全部回复脱敏；保留Markdown/HTML/control转义及[内部 `redact_diagnostic`](<../src-tauri/src/wecom_sanitize.rs>)，转义不是遮盖业务值。
- 完整业务ID、时间、地址、路径及必要字段保留，凭据/token、手机号等也不再自动遮盖。**群内回复对全群可见**；不主动搜集与任务无关的秘密，不把本地历史隔离当作群消息私密。
- 两档引擎普通正文继续完整缓冲至Done，DSH排障正文仍须报告/引用/源码校验后发送；不恢复未经验证的token流。进度与审批持续；异常/超时/意外结束不回显未完成acc，显式取消向原stream发取消final，首次冻结后迟到Done不覆盖。DSH cancelled不展示acc；内置Done的limited不泛化为所有模型stop都丢正文。
- `send_finish`仍冻结完整final及宿主耗时，重试与回复history复用同一份正文；不再加回复清洗。Markdown仍用原生 `msgtype=stream` 的 `stream.content`，welcome仍为 `msgtype=text`；不清理旧历史或改写DSH内部会话。

### 报告展示边界

[报告renderer](<../src-tauri/src/wecom_report.rs>)保留摘要先行、纵向原因层级、短核心高亮、WARN/ERROR与阻断缺口/未执行建议高亮，原文用blockquote，不用宽表、不新增polish调用。**按证据ID全局去重：每个证据块全报告只展示一次，共享6条短引文、8个证据块预算；后续claim只列refs，code核对补日志refs。** timeline仍最多32条参与校验，展示预算不削减校验范围。

短引文只严格识别并去掉框架前缀、服务/环境/实例包装和excerpt元数据；保留业务topic与必要原文，未知前缀不猜删。宿主先做Markdown/HTML/control转义再加高亮；内部原始证据、解析/校验、源码核验和因果/线上版本边界不变。

**本轮本地验证通过**：`cargo test wecom_` 为393通过、0失败、2忽略；最终 `cargo test` 全量954通过、0失败、14忽略，diff检查通过，保留既有unused/dead_code warnings。移除旧回复脱敏测试，因此全量不是上一轮957。本轮未改前端、未跑npm build、未重启/发版或真实企微验收；真实客户端呈现与送达仍待单独验收。

## 卡住回归：退役收尾与进度查询（2026-10-29）

**本次修复本地全量942通过、0失败、14忽略，前端构建通过；真实企微复测待进行。** 主代理核对真实日志：私聊`e6dad457`于16:32:10入站，16:33:42采集214 evidence、partial、18HTTP，16:34:03与群聊`238d0caa`同时`cancelled/channel_changed`；planning约19秒无输出，**不是600秒超时**。旧代际取消仅本地discard、未finish原stream，是上轮回归。16:39:21“还没好吗”`560959ac`又进入reuse会话的新模型任务，136.8秒/6工具事件/0正文后于16:41:38被用户stop。详情见 [真实事件与修复清单](<wecom-stuck-20261029.md>)。

### 当前修复契约（已完成本地回归）

- **退役先收尾**：[orch.retire](<../src-tauri/src/wecom_orchestrator.rs>)暂停ingress、冻结旧任务中断final，再cancel/invalidate；在旧socket上共用2秒预算尝试发送与flush，随后stop。已冻结正常final不改写；`retirement/socket_drained`仅表示writer flush完成，不是平台ACK或用户已读。失败/超时记delivery_unavailable，不保证每个final都实际送达。
- **不再每次apply都重连**：[有效配置](<../src-tauri/src/wecom_channel_config.rs>)先解密、归一再做字段摘要比较；等效配置noop，热字段更新保留活动回合/socket，冷变更才退役。手动restart/profile sync同样受控退役；诊断记录`source=apply/restart/sync`与changedFields，不记录敏感值或配置摘要值。旧描述“先stop/每次保存产生代际”由本节更新。
- **完整短句查状态**：“进度 / 查到哪了 / 还没好吗 / 还没好 / 好了吗 / 当前进度”经会话/策略判定后先走状态查询，不进入新模型任务和模型队列；只匹配完整短句，保留“任务状态”的共享任务管理语义。
- **有界进度**：[progress](<../src-tauri/src/wecom_progress.rs>)只存内存结构状态，最多256条，done后2小时TTL（不是活跃任务2小时即淘汰）。独立timer在无可见正文时最多每30秒发状态，tool活动不延后节奏；正文开始后不覆盖，running优先于同会话新queued，首次terminal冻结。
- **同Bot本机租约**：[bot_lease](<../src-tauri/src/wecom_bot_lease.rs>)按同Windows用户/Bot摘要取得share_mode(0)句柄锁；空文件存在不等于被占用，不记owner PID或凭据。**旧安装版未采用锁，仍可能与新实例冲突**，不是跨版本/跨机器的全局锁。
- **有界发送与ACK分层**：[WS Outbox](<../src-tauri/src/wecom_ws.rs>)最多64项、同stream待发progress合并、final和flush边界不被覆盖，200ms本地pacing。enqueued、socket_written、socket_drained只描述本地不同阶段；平台回执现在记`platform_ack`并带`final_frame_unknown`，只有req关联，不把累计stream的ACK归给final。

当时开发进程22584与安装进程26820共存，日志出现`6000 data conflict`；同Bot双连及它与取消的因果尚未证实。16:34:03具体由apply/restart/sync哪个入口触发，旧日志未记录，仍是unknown。真实进程未重启、用户会话未主动重发；本轮已完成Rust全量与前端构建，保留unused/dead_code及大包警告；真实修复验收另计，不声称提速或已恢复。

### 半天思考中：操作员三步（2026-11-11）

终稿模式（见下文「出站终稿」）下，准入后立刻发「收到，正在处理…」，**答案正文仍一次终稿**；进行中气泡会约每 30 秒换成短状态句（轮换动词 + 阶段/工具 hint + 已用时），工具有进展时可提前约 20 秒刷新。**hint 长时间不变但「已用时」仍在涨** = 工具（如 `grep`）慢，通道未断；无新 activity 超过约 90 秒时气泡会写「未断线；可发停止」。进度帧保持单行（换行会导致部分企微客户端不显示气泡）。若连短状态也不变、且 `进度`/`停止` 显示已结束 → **finish 未送达**（outbox / 通道退役）。

在**卡住的同一会话**发整句指令（不要夹其它字）：

1. **`进度`**（同义：`查到哪了` / `还没好吗` / `好了吗` / `当前进度`）——完整短句走 [`wecom_progress`](../src-tauri/src/wecom_progress.rs) 状态查询，不启新模型任务。看 phase、已耗时、空闲秒数；若写「已完成 / 失败 / 超时」但气泡仍是思考中 → **finish 未送达**（outbox / 通道退役）。
2. **`停止`**（同义：`停` / `取消` / `别查了` / `stop`…）——真取消在跑回合并给原 stream 补 finish。若回「当前没有正在进行的任务」但气泡仍卡着 → 进程侧回合已没了，问题在**投递或历史僵尸气泡**。
3. **应用内：IM 机器人 → 企业微信 → 运行诊断**——通道日志搜该轮 `ack` / `turn` / `dsh` / `cancelled` / `timeout`；请求诊断用 `wecom-<msgid>` 或 `wecom-preflight-<msgid>`；记下页顶 **PID / buildLabel / activeJobs**，确认不是双开旧进程。

本机旁证（`%APPDATA%\com.ai-workbench.app\`）：

| 路径 | 看什么 |
| --- | --- |
| `wecom.log`（及 `.1`） | 回合生命周期、stall、工具标题（终稿后仍写日志） |
| `diagnostics\` 下 `wecom.jsonl*` | 请求级事件（无正文）：admission / engine / timeout / channel_changed |
| `agent-sessions\wecom-<msgid>.jsonl` | **天玑档**工具与模型旁证 |
| `wecom-media\<msgid>\` | 入站图/文件是否落盘（怀疑卡在 materialize 时） |

按卡住时长对号入座：

| 卡住时长大致 | 优先怀疑 | 锚点 |
| --- | --- | --- |
| 数秒～几十秒，偶有「正在启动 DeepSeek Harness…」 | DSH 冷启动 / `ensure_ready` | `run_dsh_turn` 约 3s 提示 |
| 接近 **7 分钟**仍无终稿 | 天玑泵看门狗 **420s** 空闲超时未到或未触发 | `run_agent_and_stream` |
| 接近 **30 分钟** | DSH 整轮 **1800s**（`DSH_TURN_TIMEOUT`） | `run_dsh_turn` 外层 |
| 出现过审批码后沉默 | 审批 **5 分钟** 自动拒绝 | `APPROVAL_TTL` / `APPROVAL_WAIT` |
| **半天**仍是思考中、从未超时收尾 | 看门狗前死锁、通道已换代未 `ensure_stream_finished`、或 finish 写出失败 | 对照 [wecom-stuck-20261029](<wecom-stuck-20261029.md>)；查 `channel_changed` / retire |

排障类问句在终稿模式下**中间不会出现工具标题**；引擎若仍在查日志 / 读仓，聊天侧也会停在思考中——先用 `进度` 区分「还在跑」还是「已死」。未拿到进度 / 诊断证据前，不要假定是模型 TTFT 问题；也不要为此回退 350ms 正文流式或把内部 stall 默认推回气泡。

## 请求级诊断增强（2026-10-29）

**状态：新增实现与请求级事件接入已落地，最终Rust全量911通过、0失败、14忽略，前端构建通过；真实企微/OAP验收待进行。** 本节是本轮增量；后文各阶段旧测试/性能数字仍为历史快照，不因本节更新成为当前最终结果。代码审查和剩余建议见 [诊断审查](<wecom-diagnostics-review.md>)。

### 已修复的生命周期与配置问题

- [回复登记器](<../src-tauri/src/wecom_reply.rs>) 增加 `RegistrationGuard`：静默策略丢弃、提前返回与取消future同样释放未完成登记，不靠等待容量/过期回收来补救。
- [通道apply](<../src-tauri/src/wecom_channel.rs>) 成为配置代际边界：先退休旧socket、作废旧队列/回合，再在生命周期锁下配置DSH。DSH关键启停共用启动锁；[WS发布](<../src-tauri/src/wecom_ws.rs>) 保护sender/state，旧代或已停止连接的迟到完成不覆盖新代状态。
- DSH MCP通过独立顶层 `mcp/mcp_enabled` 下发；没有天玑model/agent快照也能传MCP。旧agent字段仅作兼容来源，显式顶层配置优先；有效MCP变化会退休旧ACP进程，避免活动会话继续用旧客户端集合。
- 显式模型/effort配置失败会中止本轮模型任务，配置阶段deadline为**20秒**；超时也不提交模型任务。配置ACK仅说明ACP返回的选项值，不证明后续实际provider/model使用；不要据此推导质量或性能差异。
- [Unicode脱敏](<../src-tauri/src/wecom_sanitize.rs>) 在原字符串上匹配，避开大小写折叠变长造成的字节偏移问题；[启动诊断](<../src-tauri/src/dsh_commands.rs>) 与 [ACP stderr](<../src-tauri/src/wecom_dsh.rs>) 先脱敏再截断，避免截断切断凭据识别模式。文本脱敏不等于业务全文匿名化，诊断事件仍使用字段白名单。

### 存储与查询契约

[wecom_diagnostics.rs](<../src-tauri/src/wecom_diagnostics.rs>) 单独记录typed元数据，位置为 `%APPDATA%\com.ai-workbench.app\diagnostics\wecom.jsonl` 及 `.1` 至 `.4`。它与既有会话问答旁证、DSH会话历史分开。

| 项目 | 当前实现与解释 |
| --- | --- |
| 事件 | `schemaVersion/timestamp/requestId/phase/status/code?/durationMs?/metadata`；metadata按键和类型白名单投影，不接受任意正文、URL、路径或错误全文 |
| 保留 | 单文件5 MiB、总5份（当前1份+归档4份），7天lazy保留；写入时轮转/清过期文件，读取排除过期文件/事件，不是后台定时清扫 |
| requestId | `wecom-preflight-X` → `wecom-X` → `sha256:…`；普通requestId同样哈希，已有合法摘要保持幂等。可以贴旧原ID查询，不必人工计算哈希 |
| 排序 | 追加顺序倒序（`reverseAppend`），不是按模型自报时间或另一次采样重排 |
| 单页预算 | 扫描最多1 MiB、返回最多200事件；稀疏筛选可能返回0事件但仍有下一页 |
| 覆盖 | `nextCursor/scannedBytes/partial/readErrors/health`；`partial`可表示尚有快照数据，也可表示读坏行/读失败等缺口。读完保留页不等于历史完整 |
| 写入健康 | `health.status=unknown/ok/degraded`、`writeFailures/lastError/lastSuccessAt`；只描述本进程诊断写入观测，不是OAP、DSH或企微业务健康 |

**游标不是持续订阅。** 它绑定筛选和文件身份/位置快照；追加、覆盖、删除、过期或进程重启等快照变化返回 `DIAGNOSTICS_CURSOR_STALE`，界面停止续页并提示刷新。仅文件rename的轮转、且原身份与内容快照仍在时可续读；若轮转同时淘汰了原文件，按失效处理。查询期间有新事件写入时，刷新重取第一页是预期操作，不是“没有更多记录”。

### 三项诊断IPC与UI操作

[诊断IPC](<../src-tauri/src/wecom_diagnostic_commands.rs>)、[前端封装](<../src/lib/wecomChannel.ts>) 与 [请求诊断面板](<../src/components/WecomRequestDiagnostics.tsx>) 已接通：

| IPC | 用途 | 覆盖边界 |
| --- | --- | --- |
| `wecom_diagnostics_query` | 按requestId、phase、status、from/to及cursor查询 | 每页1 MiB/200事件上限；普通UI请求50事件一页 |
| `wecom_diagnostic_request` | 单请求查看 | 单次有界页，最多200事件；有游标可通过query续页 |
| `wecom_diagnostic_export` | 返回单请求脱敏JSON字符串 | 独立有界单页，不自动导出所有保留页，也不拼接UI已加载页；含`coverage.partial/hasMore/readErrors`及health |

使用路径：**设置 → IM机器人 → 企业微信 → 运行诊断**。

1. 粘贴原始requestId或诊断摘要ID，按需填写phase/status与起止时间；UI本地时间转ISO时间下发。
2. 点击查询后按需加载更多。普通页50条，UI最多10页/500条；单请求入口首屏最多200条，所有入口仍受500条总上限。达到上限或游标失效都有独立提示。
3. 点击事件的单请求按钮，或从带requestId的新会话旁证下钻。旧会话缺requestId不显示猜测关联。
4. “复制摘要”只复制**当前已加载事件**的标识、阶段、状态、code、耗时，不复制任意metadata值；不是整条请求汇总成功证明。
5. 单请求导出拿到JSON字符串后调用既有 `save_text_file` **原生保存对话框**；保存/取消/失败分别显示。只含结构元数据，没有业务全文；先看导出coverage再判断是否缺页。

中文/英文文案均已补齐。通道日志读取错误直接显示错误；会话日志与新诊断区也分别显示error、partial和空态，不再把读取失败当成“没有记录”。

### 会话旁证：有界读取不等于清理历史

`wecom_session_recent_page`是额外的会话查询IPC，返回 `entries/partial/readErrors/scannedBytes`；[读取实现](<../src-tauri/src/wecom_session.rs>) 每文件只读尾部256 KiB，合计8 MiB，目录枚举最多512项（因此最多512个文件），结果限额默认30、最多200条。超过范围明确partial，不承诺覆盖目录里所有历史。

旧旁证仍保留，不因查询删除；诊断7天轮转也不作用于会话历史。新写入旁证带requestId，旧记录无ID就保持无关联。**旁证可含问答正文；“metadata-only”仅指新诊断事件及其导出，不代表所有本机存储都没有正文。**

### 已接入事件与结果解释

主流程已实现并核对调用点：ingress、policy、admission queued、phase_ended、model selection ACK、investigation、query账本、report validated与validation failed、model completion、final delivery prepared/enqueued/retry及retry_exhausted、platform delivery_ack。实现接入不等于每条退出路径或两档引擎的真实链路已验收。

- `phase_ended`是阶段guard结束，不等于success；request结束也不自动表示业务成功。统一terminal状态及取消/超时/错误/静默分支覆盖先于进一步美化。
- `final frame enqueue`仅证明本地发送队列接收；平台ACK仅保留平台回执事实，不表示用户已读。
- 同一累计stream多次更新若缺唯一frame对应证据，不把某条ACK标成“最终送达”；缺失对应保持未知，不由时间接近猜测。
- 引用存在、源码行匹配、模型配置ACK、平台ACK是不同层面的事实；它们都不自动补齐源码/线上版本对应或因果语义证明。

### 验证快照与下一步

- **最终本地验证**：完成观测点与重启竞态收尾后，`cargo test --manifest-path src-tauri/Cargo.toml --quiet`为911通过、0失败、14忽略；`npm run build`通过，首次系统Temp锁问题通过工作区临时目录解决。编译仍有unused/dead_code及大bundle/混合import警告；未改CSS、未提交、未替换运行中的程序。手动重启/profile同步先退役再开放通道，后台仅幂等启动；模型目录探针要求已启用DSH通道。阻塞工具启动后的物理中止不作保证。
- 第三批首批并行暂未启用。[公共入口回归](<../src-tauri/src/wecom_investigation_tests.rs#L432-L447>) 已钉住首次18预算/保留6以及低于20阈值时峰值1的串行现状；内部24预算基准不代表生产首轮已提速。
- 尚无真实企微/OAP验收；源码/线上版本对应与因果语义仍未知。优先统一terminal状态，再补source版本快照、拆开自检、固定条件做证据质量A/B；不增加排版模型轮次。

## 怎么切

设置 → IM机器人 → 企业微信 → 回答设置 → **AI 引擎**：

| 选项 | 实际执行者 | 依赖 |
| --- | --- | --- |
| 天玑（内置，默认） | 本应用 `agent_run`（四档权限、工具、审批卡） | 本应用「模型广场」里至少一个模型 |
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
- **会话**：企微逻辑会话键映射到一个 ACP `sessionId`，映射落盘于应用数据目录下的
  `wecom-dsh/sessions.json`。新版会话作用域为私聊 `bot + user`、普通群聊
  `bot + chat + user`、显式共享任务 `bot + chat + task`；键使用版本化元组摘要，不直接拼接用户标识。
  已在当前 ACP 进程内活动的会话，下一条普通消息直接 `reuse`；只有落盘映射尚未恢复到当前进程时才 `session/resume`；结构化 session-expired / session_not_found、进程重启后常见的裸 `-32603 Internal error`，以及 `-32602 session cwd does not match`（工作区已改、落盘映射仍指向旧 cwd 会话），会自动 `session/new`（记 `resume→new` 日志）；配置里变更 workspace 时也会清空 `sessions.json` 映射，下一问直接 new。`already_active` / 鉴权失败 / 「not found, expired or already_active」等含糊文案仍 fail-closed 并提示「新对话」或等待当前回合，避免在别人还占着会话时静默丢历史。完整指令「新会话」显式重置当前作用域，不把普通追问当作重置。重置、队列与共享任务边界待本轮主流程集成验证。
  **多轮上下文由 DSH 自己维护**，不再把本机短历史塞进 prompt；问答旁证仍单独写入。
  新个人/共享会话从空上下文开始，不自动继承旧群历史；旧私聊没有可靠机器人归属也不自动迁入。
  同一新版逻辑会话切回天玑时可读取该作用域自己的旁证，不跨作用域复制。
- **出站终稿 + 进行中短状态（2026-11-12）**：答案正文仍一次 `send_finish`（**不**恢复 350ms Markdown 流式）；准入先发「收到，正在处理…」，随后由 `bubble_pulse` 约每 30 秒替换同一气泡为**单行**（避免 stream 换行导致客户端不显示）：以「收到，正在处理…」开头，后接轮换动词（`PULSE_VERBS` 共 12 档）+ 阶段或工具 hint + 已用时并带省略号；工具/思考进展可用 `bubble_nudge`（≥20s）提前刷新。显式「进度」仍用长格式 `status_reply`。另可保留排队/冷启动一句、审批提示。**排障** `protected_answer` 仍全缓冲至 `Done` 校验后再发。桌面天玑窗本地流式不受影响。`WECOM_ATTACH:<path>` 仍由宿主剥离上传。操作员路径见上文「半天思考中：操作员三步」。
- **收尾**：`session/prompt` 的 `stopReason` 映射成一句提示（截断 / 工具步数上限 /
  拒绝 / 取消）；DSH 回合看门狗 **1800 秒（30 分钟）**，超时先 `session/cancel` 再收尾（日志采集与补查合计另限 120 秒累计采集耗时）。

## 会话隔离、共享任务与并发（2026-10-29）

本节会话与准入已接入两档引擎。Rust全量回归805通过、0失败、14忽略；最终企微定向回归246通过、0失败、2忽略，前端及开发程序构建通过。真实企微多人联调另计，既有测试数字不自动证明实际32个模型任务的吞吐能力。

### 默认会话与显式共享

- 私聊作用域为 **机器人 + 用户**；普通群聊为 **机器人 + 群 + 用户**。同群不同成员默认不共用模型历史，换机器人也不串会话。
- 只有明确创建或加入共享任务，才使用 **机器人 + 群 + 任务** 的公共任务上下文；任务码只在原机器人、原群内有效。
- **所有群内回复都对群成员可见**，包括普通个人会话、共享任务、状态回执与审批提示。本地历史隔离不等于群消息私密。
- 旧群历史不自动复制给任何新个人或共享会话；旧私聊缺机器人归属也不自动迁移。新键只加载自己的历史，因此首次使用为空；旧记录保留不等于被注入新会话。

| 群内完整指令 | 行为 |
| --- | --- |
| `创建共享任务 NAME` | 创建并加入；名称 1–80 字，返回任务码 |
| `加入任务 CODE` | 显式加入同机器人、同群任务；已有另一任务绑定时先退出 |
| `任务状态` | 查看自己当前任务；未加入时显示个人会话 |
| `退出任务` | 解除自己的成员绑定，后续恢复个人会话，不结束其他成员任务 |
| `结束任务 CODE` | 仅创建者可结束；解除所有成员绑定，集成层负责清理该任务上下文 |

[共享任务元数据](<../src-tauri/src/wecom_tasks.rs>) 存于本机 SQLite `shared_tasks` 表：任务、机器人、群、owner、成员、创建/活动/结束时间，不把问答正文或凭据写入此表。全库最多 **200 个有效任务**，每任务最多 **32 名成员**；连续 **24 小时无普通任务消息**后过期。查询状态、加入/退出等辅助指令不延长活动时间。过期检查在路由、成员复核和计数时执行，不是后台每秒清扫；metadata 过期不等于删除所有历史存储。显式结束在编排层标记关闭、取消当前轮、清待决审批、重置ACP映射并写历史清理标记；排队消息执行前重新检查成员身份，结束后不继续执行。停止/审批查绑定不刷新任务活动时间。任务历史文件保留记录，清理标记阻止继续恢复旧上下文。

### 全局活动回合与同会话等待

设置 → 回答设置（「引擎与模型」页签，2026-11-06 从访问策略迁来）→ **全局活动回合上限**：默认 **2**，合法值为任意整数 **1–32**；常用选项为 `1, 2, 4, 8, 12, 16, 24, 32`。保存过的 `3`、`6` 或其它合法值会临时补入选项并保留，不因打开设置或保存其它字段而跳成 32；缺省/非法值回落 2。

[准入控制](<../src-tauri/src/wecom_admission.rs>) 区分持有全局执行许可的活动回合与等待同会话锁的消息。每会话等待上限 8，拿到会话锁后才尝试全局准入；全局已满直接返回忙，不创建另一条全局执行队列。同会话串行，不同会话共享全局额度。调低上限不撤回已有许可，所以在旧回合收尾前活动数可暂时高于新上限。

设置页新增以下后端状态字段，始终展示当前实例报告的值，缺字段/null/非法数值显示「未提供」，真实 0 保持 0；前端不从保存的配置或最近 HTTP 数猜测运行数。

| 状态字段 | 含义 |
| --- | --- |
| `activeJobs` | 当前持有全局执行许可的活动回合数 |
| `waitingJobs` | 全部会话等待上一轮/会话锁的消息数；不含活动执行，首条消息拿锁前也可能短暂计入 |
| `maxJobs` | 当前后端实际生效的活动回合上限，不一定等于界面保存值 |
| `sharedTasks` | 当前机器人有效共享任务数，不等于正在执行的回合数 |

**32 是回合上限，不是 HTTP 并发。** SkyWalking 仍按 OAP 端点最多 2 个在途 HTTP；每次排障的 24 次实际 HTTP / 120 秒累计采集预算与 DSH **1800 秒**整轮约束不变。

前端定向验证：局部 strict TypeScript 检查通过；归一化覆盖 32 个合法整数与 9 个非法/缺省值；双语 Node SSR 共 86 次 fixture，覆盖每个合法保存值的 option/选中项、运行数独立于设置值、缺字段/null/0/非法数值与群可见性文案。未改 CSS、未运行 full build/cargo，未重启通道；桌面视觉与后端端到端结果另计。

## 排障回复保护、统一报告与每条消息耗时（2026-10-29）

**最新明确要求：以图一纵向排障格式替代宽表。** 同提示私聊/群聊仍共用 [报告 renderer](<../src-tauri/src/wecom_report.rs>)；内部 `DiagnosticReport version=1` 不因展示 build 升至 v2 而变更。不新增模型润色（polish），保留耗时尾注与内部 JSON 隔离。

**旧回归的直接发现**：实际模型已查出 stock 线索；最终 fallback 不等于没有查到业务原因。旧响应存在裸报告/外层 wrapper 形状、`subject` 字符串、10 个 timeline 引用超过旧校验上限 8，以及代码 C 引用未与触发项 L 引用 join 等契约问题。本轮分开修复解析兼容、机械关联和纵向展示，不据此自动升级结论。

**验证状态**：本轮 Rust 全量 **881通过、0失败、14忽略**；前端及开发程序构建通过。真实失败输出的三种脱敏形状经过逐字符 AnswerBuffer 回放，库存原因、商品和失败出口保留，不再因已知格式兼容问题落入空结论；其证据和源码使用明确标注的合成 fixture，不冒充完整线上重放。真实企微/OAP回放及手机端人工验收仍待进行。此前868/308、774/215属于历史快照。

### 同一报告契约与展示结构

[计划解析器](<../src-tauri/src/wecom_investigation_plan.rs>) 接受互斥的 `queries`、`read_evidence_ids`、`diagnostic_report` 或旧 `final_answer`；没有最终报告的旧停止计划仍可进入 `final_model`。报告 envelope 使用 key `diagnostic_report`（另支持下述严格裸报告）、报告 `version=1`，包含 `summary`、`timeline`、分层 `cause`、`code_checks`、`ruled_out`、`gaps`、`next_actions`，不接受模型自报 `verified` 或时间线原文元数据。

用户看到的是查询对象/时间/宿主筛选与纵向排障正文，按图一顺序组织：

1. **结果**：先说本次请求发生了什么，附已核对的证据引用。
2. **直接原因**：说明失败出口；过滤为 0 不直接充当具体触发原因。
3. **具体触发原因**：展示触发事实及对应代码核对；缺代码则明确未核对，不补造源码或规则。
4. **下游伴随**：单列下游错误、伴随现象与容易误判的线索；日志级别、时间相邻、同 trace 本身不等于根因。
5. **未完成**：保留阻断缺口、非阻断取证限制与下一步（标注未执行）。

证据链恢复纵向短条目，不再用宽表。时间、服务、实例、级别及短引文取自宿主保留记录，解释与原文分开；时间按查询偏移显示，缺失就显示未记录。查询对象来自原始查询，兼容 `subject` 字符串不替换宿主查询对象。timeline 最多 **32 条参与校验**；按证据ID全局去重，每块全报告只展示一次，共享 **8块展示、6条短引文**预算，后续claim只列refs、code补日志refs。校验上限与展示预算分离，不因10条有效引用超过展示预算就整份fallback。短引文取实际message，仅去严格识别的框架前缀、服务/环境/实例包装及excerpt元数据，保留topic与必要原文，未知前缀不删；digest/offset/full_length不作正文，不声称短摘录是完整线上日志。

**协议恢复边界**：[计划解析器](<../src-tauri/src/wecom_investigation_plan.rs>) 支持严格裸报告（strict bare）及省略 `queries` 的报告 envelope；兼容仅消除形状差异，不接收任意对象作为报告。已核对的 code claim 仅在其 L 引用与 trigger 的 L 引用有交集时，机械补齐对应 C join；不改 `level`，不重写结论 text，不绕过无效代码引用检查；旧 `final_answer` 的 summary 恢复保留。恢复范围限于已有证据与已核对关联，不从缺失证据自动猜结论。

**核对边界**：事实引用检查记录真实存在且可唯一定位；源码检查工作区路径、行号和单行 excerpt 匹配。两者不证明句子语义、因果关系或线上部署版本。`trigger_verified` 新报告要求结果/直接原因/触发条件、触发项引用已核对代码且无 blocking gaps；非阻断的扩展取证缺口（例如全局截断但局部记录已保留）不自动否定局部事实。外层未分类 `gaps` 保守合并为 `blocking=true`，总数超限报错，不静默丢项。

**新旧协议层级边界**：planning 的新报告在无 blocking 且有可分析证据时按报告层级直出；`final_model` 经 `finish_with_report_ceiling` 对新报告使用独立上限，有证据时最高允许 `trigger_verified`，但实际仍须通过报告结构、引用、代码与blocking检查，并非直接授予最高结论。旧final缺少分类缺口，继续使用 `permitted_conclusion_level` 保守计算，累计截断/待查页会将拟报 `trigger_verified` 限为 `direct_cause`。统一 renderer 不等于新旧协议拥有相同语义，更不保证相同提示每次语义、证据选择或逐字结果一致。

### 最终正文在什么时机显示

[AnswerBuffer](<../src-tauri/src/wecom_answer.rs>) 与[编排层](<../src-tauri/src/wecom_orchestrator.rs>) 共同控制 DSH 排障最终输出：

- **排障全缓冲**：`protected` 模型正文直到 `Done` 才处理；Markdown 前缀也不提前推送。采集、启动、排队、停滞进度和审批提示继续显示。
- **合格两路同渲染**：新 `diagnostic_report` 校验后交给 `wecom_report::render`；旧 `final_answer` 先经旧协议引用/源码校验，再由 `from_legacy` 映射到同一 renderer。旧事实保留为「旧版映射，未自动归类」，不猜哪句是直接原因或触发条件；沿用旧层级不代表补齐新协议语义。
- **fallback 不是自由文章**：`final_model` 也要求输出同一内部报告协议。protected 普通 Markdown、协议混合前文、不完整/非法 JSON、只有查询没有最终报告、无效引用或超出宿主层级均转同一纵向结构的部分报告，只显示宿主已保留证据，不回显未经核对的模型原文。非排障任务的 Markdown/显式 JSON 仍走原有正文行为。
- **有界正文**：protected 缓冲与渲染后正文上限为64 KiB UTF-8字节；当前外层 `parse_plan` 另有32 KiB输入限制。超限转部分报告，不释放截断前文。取消、错误、超时与事件流提前关闭保留各自收尾语义，不释放未经核对的 protected 正文。

格式/引用校验失败的部分报告以「尚未形成通过本地核对的诊断结论，以下仅展示已保留的证据」开头；这不代表已验证根因。合格 planning 报告直接渲染省去 `final_model`，未形成合格报告才走原有最终模型阶段，没有新增专门排版的模型往返。

### 耗时如何计算和显示

[回复登记器](<../src-tauri/src/wecom_reply.rs>) 在每条 `on_msg` 入口、拿会话锁之前记录单调时钟 `Instant`。重复登记不重置起点，因此等待上一轮、排队、采集、模型与审批等待都计入本条耗时。

- **覆盖所有最终答复**：天玑与 DSH 的成功、错误、无数据/无文本、取消、超时、忙状态均追加一次尾注。停止指令的回执与被停止的原回合分别使用各自消息起点。登记容量已满时走明确的忙提示与 `0秒` 回执。
- **只给最终答复加尾注**：进度、占位、排队提示和“请确认”的审批提示没有耗时尾注；审批选择消息本身的确认回执属于该消息最终答复。
- **整秒向下取整**：不足一分钟显示 `⏱ 本次耗时：59秒`；满一分钟显示 `⏱ 本次耗时：1分0秒`；满一小时显示 `⏱ 本次耗时：1时0分0秒`。不足一秒显示 `0秒`，不由模型估算。
- **截止到 final 准备**：第一次准备最终消息时冻结耗时与正文，不把后续发送、退避重试或企微客户端展示时间计入。该数字不是送达时延，发送成功也不等于已获得客户端展示确认。
- **同一 stream 首次收尾生效**：取消、超时与正常完成竞争同一个最终状态；后来完成的模型结果不覆盖已冻结答复。发送重试复用同一帧和同一正文，不重新计时、重复加尾注或生成新答案；已结束 stream 的后续正文更新被抑制。
- **历史与展示一致**：问答历史优先取登记器内已冻结的含耗时最终正文，而不是原始协议或迟到模型正文。快照已被回收且 stream 已结束时，历史不回退记录原始候选正文。历史记录本身不作为送达证明。

### 回复缓存保留边界

回复登记器是进程内有界状态，不是永久幂等数据库：

| 项目 | 边界 |
| --- | --- |
| 总状态 | 最多 4096 个，优先回收已完成项；活跃项不按 LRU 淘汰 |
| 已完成标记 | 最多 2048 个，按完成顺序回收；读取不延长保留 |
| 最终正文快照 | 合计最多 4 MiB UTF-8 字节；正文回收不重开 stream |
| 活跃状态期限 | 起点后 2 小时，在下一次状态变更时关闭为已完成标记；不是“所有正文保留 2 小时” |
| 超大正文 | 首次准备可返回完整正文，但超过快照预算时不缓存正文 |
| 幂等有效期 | 仅覆盖仍被保留的状态；标记被回收后不再承诺去重，消息 stream ID 应保持唯一 |

本轮 build 标记为 `report-v2-readable-concurrency32`，用于和前轮 `report-v1-unified-concurrency32` 及更早实例区分。核对运行诊断的 PID/build 后再验收；文档更新不代表已构建、重启或替换正在运行的通道。

[AnswerBuffer](<../src-tauri/src/wecom_answer.rs>) 的报告失败诊断按 request 作用域记录稳定 code，便于区分解析、引用与代码关联失败；不把跨请求的最后一次失败当成本次原因。

[恢复 fixture 目录](<../src-tauri/tests/fixtures/wecom-report-recovery/>) 保留实际 47/55/63 响应形状的脱敏材料，并配 synthetic evidence / toy source；它是协议与恢复复现材料，不是完整原 OAP 证据。后续先用 `code-review` 核对契约，再用 `diagnose` 复现；测试结果待主流程回报，本轮文档子任务不执行测试或修改源码。

待验证：逐字符与 Markdown 前缀后的协议 JSON、转义键、不完整围栏、无效证据/源码引用、正文超限、普通 Markdown 与非排障 JSON；两档引擎所有最终状态的耗时、排队起点、审批提示无尾注、停止与迟到完成竞争、相同正文重试、历史一致性，以及登记器容量/字节/过期边界。真实企微端到端结果另行记录。

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

- **图片/文件入站**：回调 url 或 `media_id` 经宿主落盘；ACP 宣告 `promptCapabilities.image`
  时图片内联为 image block，否则与文件一样给路径/文本预览。
- **出站富媒体**：天玑档用 `wecom_send_image` / `wecom_send_file` 工具；DSH 档用回复行
  `WECOM_ATTACH:<path>`（宿主 `out_targets` + `wecom_media`）。二进制上传仍依赖业务 API
  凭证，协议字段待真机联调。
- **首次使用**：`~/.dsh/profiles/acp` 不存在时要初始化（可能要装依赖，几十秒到几分钟）。
  启动超过 3 秒会先单独发一条「正在启动 DeepSeek Harness…」的消息；失败原因进
  设置页的「DSH 引擎状态」。
- **模型与登录**：ACP 档继承 `dsh-base` 的 credentials / authorization /
  deepseek-account，沿用 DSH GUI 里已配好的模型与账号；本应用的模型广场只对
  天玑档生效。
- **审查档**：渠道「审查」在 DSH 映射为与「编辑」相同的 `workspace-write` + `ask`，
  **没有**天玑「AI 先审」；设置页 `dshPermHintShort` / `dshPermNote` 已写明。
- **欢迎语**：`enter_chat` 与短问候共用 `wecomSelfIntro`（见上节）。
- **超时提示**：DSH 外层 **1800 秒**超时文案为中性「本轮达到 {秒} 秒期限…」（数值跟 `DSH_TURN_TIMEOUT_SECS`），不再写死「排障回合」。
- **设置入口文案**：配对 / 业务 API 提示统一为「设置 → IM 机器人 → 企业微信」（不再写「联网扩展」）。

### P1 已落地（2026-11-01）

- **investigate 门控 + 并行 ready**：`needs_investigation` 判定非排障时跳过采集；排障路径与
  `ensure_ready` 并行启动。关键词 / 意图 / follow-up 仍走调查（单测见
  `needs_investigation_gates_chat_vs_troubleshoot`）。
- **effort「Provider 默认」**：`configure` 从非空改为空时清空 ACP 会话映射并落盘，下一轮
  `acquire` 新开会话，避免 resume 沿用 high/medium；设置页 `dshModelsHint` 已说明。
- **天玑出站 stream_id**：`out_targets` 与活流统一为 `stream-{msgid}`（修发图/文件对不齐）。

## 引擎优先问答（2026-11-03）

企微问答默认把**用户原文 + 入站图片**交给所选引擎的多模态入口（天玑 vision / DSH image block），**不做宿主 OCR**。长连接回调的 `image.url` / `file.url` 下载体是密文，必须用同条消息的 `aeskey`（AES-256-CBC，IV=密钥前 16 字节）解密后再落盘；漏解密会把乱码当 `.jpg` 喂模型，表现为「图片没有识别到」。解密语义对齐 `@wecom/aibot-node-sdk` 的 `decryptFile` / dsh-im-connect（Base64 容忍无 `=` 填充，CBC + 手动 PKCS#7≤32）。宿主只做：

- 访问策略 / 排队 / 停止与状态 / 出站媒体
- **能力卡**（文档目录、代码工具、日志是否可用及如何请求）
- 工具与 OAP 的真正执行与预算上限
- 入站图片落盘后以内联 image 块直传引擎（无 vision 能力时降级为路径提示）

宿主**不再**默认：自然语言抽锚点后澄清收口、先打 OAP 再让模型填空壳报告、用「优先使用预取」压迫模型。

- 天玑：用 skywalking_query / 读文件等工具自行决定查什么
- DSH：自由 ACP 回合；需要日志时输出 WECOM_LOG_QUERY:{...}，宿主执行后回灌（最多 2 轮）

## 两档能力对照（「原版引擎 100%」的边界）

天玑档走本应用 `agent_run`，能力取决于我们的工具注册表；DSH 档走外部 ACP 进程，
能力取决于 DSH 自己的工具集。两者**不是逐项等价**，刻意做了如下取舍：

| 能力 | 天玑档 | DSH 档 | 说明 |
| --- | --- | --- | --- |
| 读代码 / 改文件 / 跑命令 | ✅ 四档权限 + 审批卡 | ✅ 沙箱 + ACP 审批码 | 档位映射见上表 |
| 文档问答模式（auto/doc/doc_code/code） | ✅ 能力卡偏好提示 | ✅ 能力卡偏好提示 | `capability_card` 两档共用；只声明能力与模式偏好，不指挥「必须先查日志」工序；原文直达引擎 |
| SkyWalking 排障 | ✅ `skywalking_query` 工具（日志 / trace / 指标） | ✅ 模型主动 `WECOM_LOG_QUERY:` → 宿主执行 OAP（最多 2 轮） | **引擎优先（2026-11-03）**：宿主不再默认 `investigate`/抽词澄清/空壳报告；天玑自行调工具，DSH 用标记协议请宿主查日志后回灌 |
| MCP 服务 | ✅ 随 `agent_run` 下发 | ✅ 随 `session/new`\|`resume` 的 `mcpServers` 下发 | 2026-10-28 补齐：企微通道配的 MCP 两档都可用（DSH 需总开关开） |
| 企微业务（文档/日程/智能表格 8 工具） | ✅ `wecom_*` 工具 | ❌ 不下发 | DSH 用自己的工具集，没有这组企微 Open API 工具 |
| 出站富媒体（发图/文件回对话） | ✅ `wecom_send_image/file`（Sensitive 档） | ✅ `WECOM_ATTACH:path` 宿主上传（同 `wecom_media`） | 两档均登记 `out_targets`；二进制上传需业务 API 凭证，字段待真机联调 |
| 入站图片（vision） | ✅ 落盘后内联 image 块 | ✅ ACP 宣告 `promptCapabilities.image` 时内联 | 多模态直传，宿主不做 OCR；长连接 `url` 下载后须用消息内 `aeskey` 做 AES-256-CBC 解密再落盘（缺密钥且无图片魔数则失败）；仅 `media_id` 时走 `media/get`（明文，需业务 API）；无 vision 时降级路径提示 |
| 企微出站动态 | ✅ 思考占位 + 进行中短状态 + Done 终稿（窗内桌面仍可本地流式） | ✅ 同左（答案不中间刷；约 30s/`bubble_nudge` 换短状态） | 排障仍校验后一次出报告；审批/冷启动提示可单独出现 |
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
  新报告最高层要求实际证据与源码引用且无阻断缺口，非阻断限制保留展示；旧协议仍保守处理未分类缺口。引用本身不证明因果，模型仍须用实际规则、判定值及 WARN/异常证据支持结论。
  编排层遇未解释决策且模型拟停止时，可在预算内强制一次证据锚定回溯；未完成页或截断说明覆盖边界，不抹掉已保留记录支持的局部事实，具体层级约束见统一报告章节。
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
  探测、失败及服务 ID 查询都计请求；模型规划耗时不混入采集计时，但整个 DSH 回合仍受 **1800 秒**看门狗约束。
  单次采集保留上限为 300 条日志、3 条 trace、6 个关联服务/实例补查目标；入口最多 3 页、同 trace 最多 2 页，每页 40 条。
  定点日志补查按实际请求的 `page_size` 判满（包括每页 30 条），满页在该查询最多读 3 页；达到上限、失败或预算不足保留未完成分页标记。
  单次采集摘录限 32,000 字符；本轮性能实现改为请求内 EvidenceStore 保留摘录、索引与增量注入，详见下节（替代旧版反复拼接 48,000 字符累计上下文的路径）。截断与 `partial` 覆盖约束保留。
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
| `cacheHits` | 本轮成功响应复用次数，含 singleflight 等待后复用；不是实际 HTTP 次数，不跨请求缓存业务响应 |
| `evidenceCount` | 采集日志与 trace spans 合计，不代表全部已注入模型 |
| `shownCount` | 各次有界证据渲染中展示的日志条目数，不含 trace spans；累计上下文再次截断时，不作为最终可见日志的精确总数 |
| `warnCount` | 采集日志中 WARN/WARNING 条数，不推测其中展示了多少 |
| `truncatedCount` | 后端报告的日志截断/未展示条目与累计上下文截断标记；不是丢失字符数，也不是 `evidenceCount - shownCount` |
| `incompletePages` | 未完成分页标记的累计数/下限，不是 OAP 精确剩余页总数 |
| `decisionGaps` | 后端记录的采集上下文缺口与模型报告的未解释决策；按原文列出，不由 UI 判断根因 |

首次与补查累计计数可能含重复查询，不宣称跨轮唯一。缺字段与真实 0、空数组分别显示；这些字段不表示已验证具体业务根因。

### 性能实现与待测口径（2026-10-29）

本节依据当前源码与本轮代理交接记录，描述实现及待测项，不宣称真实提速已经验收。
**保持同一模型、原 reasoning / 思考强度不变，不自动降低分析深度；查询预算仍为 24 次实际 HTTP / 120 秒累计采集耗时，整轮 1800 秒。**

- **请求内查询复用**：[QueryBudget](<../src-tauri/src/wecom_skywalking.rs>) 的 clone 共享本次排障的连接池、成功响应缓存和 singleflight。缓存键包含端点、鉴权摘要、查询与变量；无效/失败响应不作为成功缓存。命中只增 `cacheHits`，不重复扣实际 HTTP。新请求使用新 QueryBudget，不跨用户/跨请求复用业务响应；既有 5 分钟源能力就绪缓存不是日志正文缓存。
- **有界并发**：全局按 OAP 端点限制最多 2 个在途 HTTP（不因不同鉴权拆成多组配额）。无 service 条件、两个独立入口 ID 组且剩余预算至少 20 次时才 2 路并发；各组内 pagination 仍串行，预算紧张时串行，trace/decision 及关联服务依赖链保持串行（本轮未实现全链路并发）。待测预算临界、取消/超时保留已收证据与多会话共用 OAP 的上限。
- **EvidenceStore**：[请求内证据存储](<../src-tauri/src/wecom_evidence_store.rs>) 仅在内存保留最多 **256 KiB 的 retained excerpt 与元数据**，不是 OAP 原始全文，也不是跨用户缓存。存储分配 canonical L/T ID，来源 `[R1L1]` 等为 alias；索引摘要仅用于导航，校验读取保留摘录。首次模型输入限 16k 字符，含索引/WARN/决策摘录；后续优先发 delta，超过 20k 时回落 bounded_prompt(20k)，可能重发优先摘录，不承诺零重复。超限和未保留部分仍是覆盖缺口。`read_evidence_ids`（亦接受 `readEvidenceIds`）一次最多 6 个 canonical ID，本轮最多展开 1 次、12k 字符，仅展开已保留摘录，不请求 OAP。
- **合格最终答省一次总结**：[计划协议校验](<../src-tauri/src/wecom_investigation_plan.rs>) 优先接收 `diagnostic_report`，兼容旧 `final_answer`，与 queries / read_evidence_ids 互斥。合格报告直接走统一 renderer，省一次 `final_model`，不额外 polish；旧 final 映射保留通用事实，不自动补成完整因果分析。带 blocking 缺口时仅保留允许层级，非阻断扩展限制仍展示；无效引用、非法协议或未形成最终报告时走既有校验/fallback，fallback 最终输出同样要求报告协议。源码引用有效不等于线上因果成立。
- **首轮保留补查预算**：首次 QueryBudget 限 18 次，为后续补查保留 6 次；parent 仍为 24 次实际 HTTP / 120 秒累计采集，未增加总预算。规划时间不计入采集时间。首轮 18 次低于旧双 ID 并发阈值 20，因此本轮首轮 ID 查询保守串行，可能增加数秒，但避免首次采集耗光 24 次而无法补查；尚未实现首两页的新并发策略。此前模拟并发收益不迁移到本轮。
- **时间与源码缺口**：`time_window_mismatch` 单独保留，不解释成成功查空、不据此扩大时间窗。`source_hints` 从 stack 指向的工作区实际源码文件读取定位行 ±8 行，只做源码候选与后续引用导航，不把日志内容当源码，不建立跨用户业务缓存；未读到的源码保持缺口。源码 hints 优先注入上限 8k 字符，是新增输入预算，不承诺缩短模型输入或已减少 token。
- **同请求阶段汇总，不推算总量**：[性能记录](<../src-tauri/src/wecom_performance.rs>) 保留旧 `lastPerformance` 的 requestId/phase/elapsedMs/inputChars/outputChars/toolEvents/firstEventMs，新增可选 `summary`：`{requestId,totalMs?,queueMs?,elapsedWithQueueMs?,phases:[{phase,elapsedMs,calls,inputChars,outputChars,toolEvents,firstEventMs?}],sessionAction,model,effort,validationFailures:string[]}`。设置页仅在 summary 与外层 requestId 相同时展开阶段表，保留 raw phase 与双语标签（planning_initial/repair/followup/evidence_expand 等）；按后端原值显示，不把最近阶段再次加进 summary，也不把 planning 父阶段与子阶段重复求和。total 是 run_dsh 执行耗时，queue 单列，与消息入口开始且包含排队的回复尾注不同；缺 total 时不由阶段合成。可选 elapsedWithQueueMs 直接显示后端含排队耗时，前端不自行相加，也不把它当作客户端送达耗时。firstEventMs 不等同首 token，toolEvents 不等同唯一调用。模型/effort 为后端报告值，未观测占位不代表实际模型已确认。校验失败逐条显示。`lastInvestigation.cacheHits` 仍独立附其 requestId，不与其它请求混算；缺字段/null/非法数值显示「未提供」，真实 0 保持 0。UI 复用现有折叠/滚动样式，无 CSS 改动。

**真实 baseline（优化前，两次记录，非同条件 A/B 提速结论）**：

| 记录 | 整轮 | collect | 说明 |
| --- | --- | --- | --- |
| 首次真实 baseline | 461.7 秒 | 31.6 秒 | 来自本轮任务/代理提供的真实运行基线 |
| 第二次真实 baseline | 255 秒 | 49.2 秒 | 采集耗时单列，不拿最近 phase 冒充整轮 |

待测：固定同模型与 reasoning、同问题/时间窗/OAP/代码版本，重复采样 total、collect、planning、firstEventMs、输入/输出字符、工具事件及 cacheHits，并同时核对证据/结论质量。比较冷/热源探测、双 ID 与串行分页、重复查询 singleflight、旧协议 fallback、final_answer 静态引用校验与取消/超时；未完成同条件重放前不填提速比例。本轮前端验证：双语 settings JSON 与 12 个新增文案键检查通过；以两个变更 TS/TSX 文件为入口、沿依赖使用项目同等 strict 选项的局部 `tsc --noEmit` 退出 0；16 次双语 Node SSR fixture 通过（缺字段/阶段/total/真实0/null及非法数值/不同请求ID/HTML转义）。此子任务未执行 full build 或 cargo；后端测试由主代理另行报告，真实 OAP/企微与桌面视觉验收仍待进行。

### 既有私聊/群聊性能优化本地验证（2026-10-29，统一报告之前）

以下记录属于此前 `sessions-v3-perf-concurrency32`，不覆盖本轮统一报告变更。

- 全量 Rust 测试：844 通过、0 失败、14 忽略；包括活动会话复用、显式新会话、带缺口直出、预算保留、时间窗偏离和源码线索。
- 前端 `npm run build` 及开发程序构建通过；保留既有编译/打包警告。
- 构建标记为 `sessions-v3-perf-concurrency32`，未自动重启或替换安装版。
- 以上是代码与本地回归验证，不代表真实OAP/模型同条件回放已经提速。

### 本轮私聊/群聊观测与待验证项（2026-10-29）

下表由本轮任务提供的真实日志对照整理，不是本子任务新发起的运行，也不是同条件 A/B 提速验证：

| 路径 | 执行 total | acquire | planning 分段 | final_model |
| --- | --- | --- | --- | --- |
| 群聊 | 355.161 秒 | 60.964 秒 | 166.935 + 44.373 秒 | 82.538 秒 |
| 私聊 | 190.361 秒 | 74.074 秒 | 69.207 秒 | 46.493 秒 |

群聊记录的 acquire 反而较短，主要差值在 planning/final_model；这些观测不足以归因于“群平台慢”，也不证明本轮修改已提速。阶段可能有父子包含关系，UI 不累加它们；真实客户端送达时间仍未观测。

本轮新增验收：同 request summary 与旧 phase 共存、total/queue 分口径、raw 子阶段标签、重复 phase 不被前端再汇总、缺字段与真实 0；活动会话追问 reuse、显式「新会话」reset、重启后 resume；带 gaps lower-level final 直接渲染且保留缺口、过强层级仍校验失败；首轮 18/reserve 6 与 parent 24/120；time_window_mismatch 不走 empty 分支；stack 源码 ±8 行实际读取与请求隔离。本轮前端局部验证：以 WecomSettingsCard/wecomChannel 与 vite-env 为入口、项目同等 strict 选项的 `tsc --noEmit` 退出 0；双语 settings JSON 解析及诊断/阶段/会话动作键对齐检查通过；八个范围内文件 `git diff --check` 退出 0（仅 ROADMAP 的 LF/CRLF 提示）。无 CSS 改动；本轮未新做 SSR/桌面视觉或 OAP 重放。主流程 full build/Rust 与真实企微复测另行记录，历史通过数不转记为本轮结果。

### 既有性能验证结果（2026-10-29，回复修正之前）

以下记录属于 `investigation-v4-perf`，不作为 `investigation-v4-reply` 的测试通过结论。

- Rust全量测试：747通过、0失败、14忽略（包含手动延迟基准）；最终源码企微定向回归188通过、0失败、2忽略。
- 本地独立双ID查询、每请求固定延迟2秒：串行 **4.016秒**，受控并发 **2.031秒**，峰值并发2；采集阶段耗时降低约49.4%。这是模拟HTTP基准，不代表整个真实排障提速比例。
- `npm run build` 类型检查与前端构建通过，保留既有体积/混合导入警告。
- 该历史版本直接渲染合格结论时附最多6条、合计6k字符的保留证据摘录；现已由统一报告的关键短摘录替代，不作为当前展示契约。
- 新版标识 `investigation-v4-perf`；未自动切换模型、降低思考强度或重启业务通道，真实总耗时改善待同条件回放。

### WARN 取证既有前端验证（2026-10-28）

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

## 排障自然语言查询补全（2026-11-02）

DSH 排障查询采用三层责任：规则解析负责时间窗、traceId、订单号、`service=`/关键词等确定性条件；规则只能确认“这是排障”但无法形成可执行锚点时，宿主先做一次**不花模型往返的确定性兜底抽取**（`wecom_natural_query::infer_intent_from_text`），兜底拿不到锚点才使用当前 DSH 模型做**一次隐藏、无工具的结构化抽取**；宿主随后对 JSON 做安全校验。模型抽取不是扩大搜索范围的授权。

确定性兜底复用 `wecom_intent::extract_business_phrases` 的词法，再剥尾缀虚词（`孙武的` → `孙武`）、丢掉含泛词（服务/日志/报错/会话/很多…）的整句残片，只留 1–3 个字面量锚点；`answerTargets` 由「某字段 是啥/是什么/是多少」句式取。**宁可不出锚点也不拿整句当关键词**——错误锚点会让 OAP 必然零命中，比不查更糟。命中即跳过模型抽取（诊断记为 `skipped_local`）。

抽取协议只接受 `confidence`、`service`、`anchors` 和 `answerTargets`。形状容错：BOM、` ```json ` 围栏（含 `\r\n` 与未闭合）、围栏前后的人话、未知字段、`confidence` 大小写、`anchors` 超过 3 个（截断）都不再判失败；未知字段被忽略而不是采信。值的约束不变：置信度必须为 `high`，至少一个 `anchors` 必须存在，且 service、anchors、answerTargets 每个值都要逐字出现在用户原话中（ASCII 大小写不敏感，中文按原文匹配）；查询语句、URL、命令、超长/重复/非法值、工具调用、权限请求、超时仍拒绝。

`anchors` 才会进入受限日志检索；`answerTargets`（例如 `openId`）只传给最终 DSH 分析作为回答聚焦字段，不会进入 SkyWalking `keywordsOfContent`，因此不会把“孙武”和“openId”错误地做 AND 过滤。抽取失败**不再中止整轮**：已解析出的 service 与时间窗一定保留，并按下面的顺序降级——(1) 服务已知、窗口 ≤ 24 小时、且没有任何锚点时，收窄为 `服务 + 窗口 + level=ERROR` 的有界查询（诊断记 `error_scan`）；(2) 连服务都没有时给出宿主生成的澄清句，写明已识别什么、还缺什么，serde/抽取内部原因只进诊断库，绝不出现在企微回复里。成功查询仍沿用既有 24 次 HTTP、120 秒采集和 1800 秒整轮预算。

每轮只记录 `query_extraction` 的状态与结果码（`accepted` / `skipped_local` / `error_scan` / `clarify`，结果码为 `json_shape` / `low_confidence` / `missing_anchor` / `unsafe_value` / `unavailable`）、锚点/回答目标计数、耗时和是否执行，不记录用户原文、姓名、ID、候选关键词或模型原始 JSON。设置页的请求诊断面板按 `phase`/`status`/`code` 通用渲染，新增状态无需改前端。规则明确时不会产生额外模型抽取调用；普通追问和共享会话不会串用其它会话的回答目标。

## 大白话准入与实体切分（2026-11-02 第二轮）

上一轮修的是「抽取阶段把答对的 JSON 判死」，这一轮修它前一层：**得先认出用户在报障，才肯去查日志**。`孙武登录不上` 曾因 `登录不上` 不在任何词表而 `trouble=0`、被判文档查询，整条排障链路不启动。

**准入（`wecom_intent`）**：`TROUBLE_SYMPTOM` 补齐大白话症状（登录不上/进不去/下不了单/付不了款/白屏/转圈/加载不出来/没数据/闪退/掉线/收不到/重复…），并新增「目标 + 症状」组合提权 `+15`（`服务/接口/线上` 与任一症状词同时出现）。症状词仍是 `+25`、低于强信号的 `+40`，`doc >= 32` 的压制规则不动——`写日志模块怎么实现`、`接口文档在哪` 仍判文档查询（有反向单测）。词表由 `wecom_intent::symptom_vocabulary()` 单点暴露，切分与查询层都引用它，不再各留一份。

**切分（`wecom_natural_query`）**：没有标点的中文会被 `extract_business_phrases` 整句当一个候选，`孙武登录不上` 会变成锚点 → OAP 必然零命中。现在 `segment_anchor` 先把首尾的症状词剥掉（`孙武登录不上` → `孙武`），只接受剥完仍是 **2–4 个汉字**、且不含泛词的结果；剥不干净就返回 None 交给模型或澄清。单字词（`卡`/`慢`）只从尾部剥，否则 `卡布奇诺` 会变成 `布奇诺`。同时 `GENERIC_WORDS` 补了动作词（下单/登录/支付/加载…）与小标题标签（关键信息/详细信息/上下文…），后者是同源封闭集，防止整段粘贴对话时把章节名当关键词。

**查询层补位（`wecom_investigation_query`）**：`analyze` 的关键词只放行错误词，纯文本实体一向进不了查询。现在若没有显式 `keyword=` 字段、也没有旁证正文短语，且**已有服务或业务标识符可定位**，会从剩余文本里补 1 个实体关键词（最多补到 3 个）——`txmsg 服务 孙武登录不上` 因此能同时带上服务与 `孙武`。服务未知时不补；`wecom_investigation` 另有 `anchor_without_service` 兜底：**由自然语言推出来的锚点不能顶替服务名**，缺服务时只问一句「哪个服务」，绝不跨服务泛查（用户自己写 `keyword=timeout` 明确要求的不拦）。

**澄清话术**：`not_executed` 的三处提示改成口语提问，时间用可读形态（`2026-11-02 16:03:16` 而不是 RFC3339），不再列「请补充订单号、requestId、关键词或服务名」这类字段清单，也不再出现「请写成 2026-10-30 16:12:13」——用户原话就是「必须要固定格式」。

**诊断**：每轮在 `investigate` 入口记一条 `phase=intent`（`troubleshoot` / `not_troubleshoot` + `confidence` / `service` / `keywordCount`），词表要调时在设置页直接看「为什么这句没被判成排障」，不用再猜。


| 文件 | 作用 |
| --- | --- |
| `src-tauri/src/wecom_dsh.rs` | DSH 引擎：进程生命周期、ACP 收发与 id 路由、会话映射、patch 生成、档位映射、模型目录与选择 |
| `src-tauri/src/wecom_dsh_profile.rs` | profile 镜像：bundle 映射、patch 过滤、`node_modules` 联接、镜像报告 |
| `src-tauri/src/wecom_channel.rs` | `engine`/`permission`/`workspace` 入参、DSH 校验与状态、按引擎启停、`PendingApproval` 枚举 |
| [wecom_orchestrator.rs](<../src-tauri/src/wecom_orchestrator.rs>) | `on_msg` 入口计时、引擎分流、`run_dsh_turn`、审批码回灌、统一 final 重试与含耗时历史 |
| [wecom_answer.rs](<../src-tauri/src/wecom_answer.rs>) | 排障正文缓冲、协议候选识别、计划/报告校验与部分报告 fallback |
| [wecom_report.rs](<../src-tauri/src/wecom_report.rs>) | DiagnosticReport version 1、引用/源码行核对、旧 final 映射及纵向排障 renderer |
| [wecom_reply.rs](<../src-tauri/src/wecom_reply.rs>) | 单调时钟、耗时格式、首次 final 冻结与有界回复登记器 |
| `src-tauri/src/wecom_investigation.rs` | 有限采集、trace/服务补查、证据强度、预算与短期追问上下文 |
| `src-tauri/src/wecom_investigation_query.rs` | 无 IO 的查询条件、明确时间与追问解析 |
| `src-tauri/src/wecom_investigation_plan.rs` | DSH JSON 补查计划校验、真实源码行核验与证据引用边界 |
| `src-tauri/src/wecom_skywalking.rs` | OAP GraphQL、实际 HTTP 预算、分页与时间过滤 |
| `src-tauri/src/dsh_commands.rs` | `dsh_cli` / `dsh_installed_version` / `dsh_clear_stale_lock` 等复用入口 |
| `src/lib/wecomChannel.ts` / `components/WecomSettingsCard.tsx` | 引擎下拉、DSH 状态胶囊、档位映射说明 |
