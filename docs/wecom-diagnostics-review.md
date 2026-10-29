# 企微 / DSH 诊断增强：代码核对与剩余建议

> 2026-10-29 源码复查与实现验收记录。请求级诊断、第一批可靠性修复及前端已落地；最终 Rust 全量 911 通过、0 失败、14 忽略，前端构建通过。真实企微/OAP与桌面视觉验收未执行；未重启通道、未发送真实测试消息、未提交代码。

## 后续真实卡住事件与回归修复（2026-10-29）

**本节晚于前一批911通过的验证快照；当前修复测试待主代理收口。** 详细时间线与核对边界见 [卡住事件记录](<wecom-stuck-20261029.md>)。真实日志由主代理核对，本子任务重读代码，不将摘要整理说成独立原始日志复核。

- **直接原因已收窄**：私聊`e6dad457`于16:32:10入站，16:33:42已采集214 evidence、partial、18HTTP；16:34:03与群聊`238d0caa`同时`cancelled/channel_changed`，planning仅约19秒无输出，不是600秒超时。上轮代际取消仅本地discard而未finish原stream是回归。
- **二次等待来自误路由**：16:39:21“还没好吗”`560959ac`进入新模型任务并reuse会话，136.8秒/6工具事件/0正文，16:41:38被用户stop；完整进度短句应先走状态而非模型。
- **因果缺口保持unknown**：22584开发版与26820安装版共存、出现`6000 data conflict`，不等于已证同Bot双连；旧日志缺`source`，具体apply/restart/sync触发未知。
- **退役顺序修正**：[orch.retire](<../src-tauri/src/wecom_orchestrator.rs>)暂停ingress，冻结旧任务final、cancel/invalidate，共用2秒尝试旧socket发送+flush后再stop。`socket_drained`不是ACK，已冻结final不覆盖；不再沿用“先stop后本地清理就算完成”的旧描述。
- **新增配置/进度/租约模块**：[channel_config](<../src-tauri/src/wecom_channel_config.rs>)按解密后的有效语义哈希比较，noop/热更新保留回合，冷变更才退休；记录source/changedFields而非敏感值。[progress](<../src-tauri/src/wecom_progress.rs>)最多256条、done后2小时TTL；完整状态短句先路由，“任务状态”仍是shared命令；30秒独立心跳不被tool活动延期，正文显示后不覆盖，running优先queued。[bot_lease](<../src-tauri/src/wecom_bot_lease.rs>)用同Windows用户同Bot的share_mode(0)句柄锁；旧安装版未采用锁，仍可能冲突。
- **有界传输**：[WS](<../src-tauri/src/wecom_ws.rs>)最多64项Outbox，同stream待发progress合并，final/flush边界保留，200ms本地pacing；新平台事件为`platform_ack/final_frame_unknown`，只有req关联，不归因final帧。

审查优先级：退役final冻结与flush顺序 → 完整短句零新增模型任务 → 配置noop/冷热语义 → progress容量/TTL/心跳与Outbox/租约 → 最终回归。实际进程未重启，用户会话未主动重发；真实修复验收仍待进行。前一批下表的`delivery_ack`是历史事件名，当前传输语义以本节为准。统一business terminal、自检拆分、版本快照和证据质量A/B建议仍保留，不新增排版模型轮次。

## 优先结论

1. **优先统一terminal语义，再完善展示。** 当前[入口](<../src-tauri/src/wecom_orchestrator.rs>)存在request `ended`、[阶段guard](<../src-tauri/src/wecom_performance.rs>)存在 `phase_ended`，它们表达生命周期结束，不是模型、业务或交付成功；主流程应收口所有退出路径后再宣称完整请求链路。
2. **导出不是全量请求证据包。** [request/export](<../src-tauri/src/wecom_diagnostics.rs>)只执行一个有界页（扫描1 MiB、最多200事件）；UI加载更多不会改变导出覆盖。应始终查看`coverage.partial/hasMore/readErrors`，复制摘要也只覆盖已加载事件。
3. **现有ACK不补足因果或送达证明。** 模型配置ACK不证明实际provider使用；本地frame入队不等于平台接收，平台ACK不等于用户已读；累计stream更新缺唯一对应时，不给最终送达结论。
4. **生产首轮仍串行。** [公共入口回归](<../src-tauri/src/wecom_investigation_tests.rs#L432-L447>)锁住首次18次预算、保留6次及峰值1；20阈值下的旧并发分支在该入口不启用。第三批首批并行暂未启用，不把内部24预算并行基准外推为生产提速。

## 已核对的新增实现

| 代码落点 | 本轮核对事实 | 仍需保持的解释边界 |
| --- | --- | --- |
| [wecom_reply.rs](<../src-tauri/src/wecom_reply.rs>) / [wecom_orchestrator.rs](<../src-tauri/src/wecom_orchestrator.rs>) | 登记后持有`RegistrationGuard`，静默策略返回/取消future也清理未完成登记 | RAII释放不是用户收到消息的证明 |
| [wecom_channel.rs](<../src-tauri/src/wecom_channel.rs>) / [wecom_dsh.rs](<../src-tauri/src/wecom_dsh.rs>) | 配置新代际退休socket、失效队列/回合，DSH生命周期/启动锁约束关键操作；MCP顶层配置独立于agent model | 保存配置改变执行代际，不代表真实新进程/新通道已验收 |
| [wecom_ws.rs](<../src-tauri/src/wecom_ws.rs>) | sender/state发布检查当前代际及stop，阻止旧连接迟到覆盖 | 连接状态和具体frame交付状态分开 |
| [wecom_orchestrator.rs](<../src-tauri/src/wecom_orchestrator.rs>) / [wecom_dsh.rs](<../src-tauri/src/wecom_dsh.rs>) | 显式model/effort配置失败中止本轮模型任务，配置deadline20秒 | 选项ACK不等于provider实际使用；采集可能已经发生，不写“整个请求从未执行” |
| [wecom_sanitize.rs](<../src-tauri/src/wecom_sanitize.rs>) / [dsh_commands.rs](<../src-tauri/src/dsh_commands.rs>) | Unicode大小写匹配使用原串；启动及ACP stderr先脱敏后截断 | 文本脱敏不是通用正文匿名化，metadata字段白名单仍必要 |
| [wecom_diagnostics.rs](<../src-tauri/src/wecom_diagnostics.rs>) | typed元数据JSONL；5 MiB×总5份，7天lazy保留；查询/导出重新投影磁盘字段 | 不包含业务全文，也不描述会话正文的保留政策；health只代表诊断写入观测 |
| [wecom_diagnostic_commands.rs](<../src-tauri/src/wecom_diagnostic_commands.rs>) / [lib.rs](<../src-tauri/src/lib.rs>) | query/request/export三IPC已注册，另有session recent page | 有接口不等于全链路事件接线覆盖 |
| [wecom_session.rs](<../src-tauri/src/wecom_session.rs>) | 每文件256 KiB，总8 MiB，目录枚举512项上限；新旁证带requestId | 只读保留旧历史，不删除；旧无ID不猜；超出候选集不承诺全局最近完整性 |
| [wecomChannel.ts](<../src/lib/wecomChannel.ts>) / [WecomRequestDiagnostics.tsx](<../src/components/WecomRequestDiagnostics.tsx>) | 按请求/phase/status/time筛选、单请求、摘要、导出；普通页50，10页/500封顶；调用既有原生保存对话框 | 摘要不带任意metadata值；导出有界单页，不等于已加载页集合 |
| [WecomSettingsCard.tsx](<../src/components/WecomSettingsCard.tsx>) / [中文文案](<../src/locales/zh-CN/settings.json>) / [英文文案](<../src/locales/en-US/settings.json>) | 新诊断入口双语、会话ID下钻、日志error不再显示空 | 本次未进行桌面视觉或真实企微验收 |

### requestId、页覆盖与轮转

- requestId先将`wecom-preflight-X`归一成`wecom-X`再SHA-256，查询入口执行同样处理；返回的合法`sha256:…`幂等，所以旧原始ID和新摘要ID都可查。会话/用户/群等标识另行哈希，不靠原标识明文做可分享关联。
- 查询按追加倒序而非时间排序；单页扫描最多1 MiB、最多200事件。稀疏过滤可出现空页和nextCursor；空页不等于没有匹配历史。
- `readErrors`记录读取缺口；`partial`覆盖还有快照数据或坏行/读失败；`health`报告本进程写入状态。三者不可互相代替，也不可代替业务成功状态。
- 游标绑定进程、筛选、文件身份和位置快照：追加、覆盖、删除、过期或重启变化返回`DIAGNOSTICS_CURSOR_STALE`并刷新。纯rename且原身份/快照仍在可继续；轮转伴随淘汰则失效。
- 5份包括当前文件，不是“5个归档+当前文件”。7天为lazy清理/读取排除，不承诺定时秒级删除。诊断轮转不会删除会话问答旁证。

## 已接入事件与剩余验收检查表

已核对主流程接入：ingress/policy/admission queued/phase_ended/model selection ACK/investigation/query/report validated与validation failed/model completion/final delivery prepared、enqueued、retry及retry_exhausted/platform delivery_ack。下表列接入后的验收重点，不把实现接入标成真实全链路通过：

| 阶段 | 需留下的事实 | 禁止的推断 |
| --- | --- | --- |
| ingress / policy | 收到、策略判定、静默退出原因code | 没答复就一定模型失败 |
| queued / admission | 排队与准入、配置代际取消 | 排队耗时等于模型耗时 |
| phase结束 | 阶段耗时与结束事件 | `phase_ended=success` |
| model selection | 请求配置、ACK或失败/超时、配置状态 | ACK值=实际provider使用 |
| collection / query账本 | 采集范围、实际次数、缓存、截断、partial/错误 | 查空或未查=业务不存在 |
| report validation | 校验通过/失败code、渲染/fallback分支 | 引用存在/代码行匹配=因果已证明 |
| final frame enqueue | 本地入队结果与可核对标识 | 本地入队=平台最终接收 |
| platform ACK | 平台errcode、已知关联或未知关联 | ACK=已读；模糊累计stream ACK=最终送达 |

## 剩余建议（不增加排版模型轮次）

### P0：统一语义terminal状态

先将生命周期、模型执行、报告质量、传输状态分开约定。建议采用相互独立的结果轴，而不是单个success总括一切：

- 生命周期：结束、取消、超时、策略静默、配置代际失效。
- 模型/报告：配置失败、模型错误、报告校验失败、部分报告、通过本地校验。
- 传输：本地入队、平台ACK/错误、关联未知；用户已读继续保持未知。

让每个退出路径都有明确终态和可追踪原因code，验证取消与正常完成竞争、迟到事件、重复finish以及队列失效；完成这些后才考虑UI汇总徽章。状态名称与最终实现需由主流程统一，不把此建议当作已存在的枚举。

### P1：source版本快照

保留当前源码身份（commit及dirty事实、参与核验文件摘要）、查询窗口/OAP源身份和观测时间；线上部署版本另需部署记录、构建ID或服务端版本证据。两边没有可靠映射时明确unknown，不用本机源码匹配替代线上代码确认。快照建议只保留可核查标识，避免把源码全文或业务数据加入诊断JSONL。

### P1：拆分自检

把通道连接、DSH安装/启动、模型配置ACK、MCP发现、OAP连通/schema/鉴权分别给状态与耗时；某项未执行明确not_run，不让一个“自检成功”遮住其他失败。自检不自动变成生产业务查询、不附加排版模型往返；实际provider是否使用与配置ACK继续分层。

### P1：证据质量A/B与性能一起验

固定问题、模型、effort、窗口、OAP与源码/部署版本，分别记录冷/热启动、queue、collect、planning、final、入队/ACK（可关联时）。同时比较有效证据数量、WARN/分页覆盖、缺口保留、原因层级、引用/代码匹配和错误归因，避免只看总耗时。保留24次HTTP/120秒采集、整轮600秒及原reasoning，不增加polish/排版模型轮次。首批并行若未来启用，先证明公共入口实际到达并发分支，再做同条件数据对照。

## 验证状态

| 证据来源 | 当前记录 | 口径 |
| --- | --- | --- |
| 最终Rust回归 | `cargo test --manifest-path src-tauri/Cargo.toml --quiet`：911通过、0失败、14忽略 | 包含WS逆序发布、独立MCP、诊断分页/脱敏/轮转、RPC取消清理与报告降级测试；仍有既有unused/dead_code警告 |
| 最终前端build | `npm run build`成功；系统Temp文件锁问题通过工作区临时目录解决 | 保留现有大bundle与混合静态/动态import警告，未改CSS |
| 差异检查 | `git diff --check`通过 | 未commit；新增文件需随本次改动一并保留 |
| 真实企微/OAP | 未验收 | 不宣称真实送达、已读、provider实际使用或提速 |
| 源码/线上版本与因果语义 | 未知 | 本地引用/源码行校验不补足该证明 |

手动重启与profile同步先停socket、更新代际并同步退役DSH，再开放入口；后台只做幂等启动，避免迟到restart误杀新回合。模型目录探针仅在已启用DSH通道内执行。已启动的阻塞系统工具不保证被物理中断，真实取消联调仍需覆盖。后续真实验收单独补录。用户说明见 [引擎诊断章节](<wecom-dsh-engine.md#请求级诊断增强2026-10-29>)；实现交接见 [交接文档](<../交接文档.md#企微--dsh-诊断增强交接2026-10-29本轮增量>)。
