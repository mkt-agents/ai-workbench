# 企微机器人对接 — 开发计划

> 目标：补齐企微通道与桌面端天玑体验的差距。当前已实现「文本/语音(转写)入站 → 策略闸门 → 天玑 agent → 纯文本流式出站 + 配对/白名单/脱敏/并发」。
> 本计划按用户价值与实现成本排序，分四期。每期末尾给验收口径。

---

## P0 — 进站富媒体（让用户「能像在桌面端一样发东西」）

**痛点**：`wecom_orchestrator.rs:151` 只放行 `text`/`voice`；`wecom_proto.rs:183` 的 `extract_text` 对 `mixed` 丢图片、对 `image`/`file`/`link` 返回空。

### 1.1 图片/文件入站（`wecom_proto.rs` + `wecom_orchestrator.rs` + `wecom_ws.rs`）
- 在 `extract_text` 之外新增 `extract_media(body, msgtype) -> Vec<MediaRef>`，识别 `image`(`image_url`/临时`media_id`)、`file`(`file_url`/`media_id`)、`mixed` 中的图片项。
- `MsgCallback` 增加 `media: Vec<MediaRef>` 字段；`wecom_ws.rs` 增加「拉取临时素材」能力（`aibot_get_media` 或企微临时下载 URL），把媒体落盘到 `%APPDATA%\com.ai-workbench.app\wecom-media\<msgid>\`（限时清理）。
- `wecom_orchestrator.rs:135` 起：当 agent 配置 `vision=true` 且存在图片媒体时，把图片作为附件随 `AgentRunRequest`（复用天玑 `@`/`attachments` 通道）一并提交；无 vision 时回「此模型不支持看图」。
- 文件类：作为附件（路径）交给 agent，与桌面端「附加文件」同一套限额/删除逻辑。

### 1.2 语音真正可用
- `extract_text` 的 `voice` 分支：优先 `recognition`；为空时回「暂不支持该消息类型，请发文字」**而不是静默丢弃**（改 `wecom_orchestrator.rs:164` 的空文本处理，对 voice 显式提示）。

**验收**：企微发图 → 天玑带图推理；发文件 → 作为附件；发空语音 → 明确提示而非无反应。

---

## P1 — 出站富媒体 + 媒体上传

**痛点**：`respond_stream`（`wecom_proto.rs:68`）写死 `msgtype:"stream"` 纯文本，无 `media_id` 上传。

### 2.1 出站媒体上传（`wecom_ws.rs` + `wecom_proto.rs`）
- `WecomSocket` 增加 `upload_media(path) -> Result<media_id>`，按企微智能机器人素材接口上传，返回 `media_id`。
- `wecom_proto.rs` 新增 `respond_image(req_id, media_id)` / `respond_file(req_id, media_id, filename)` / 可选 `respond_markdown(req_id, content)`（若平台支持 markdown 卡片；否则降级为纯文本）。

### 2.2 结果回写分流（`wecom_orchestrator.rs` 的 pump）
- `AgentBridgeEvent` 中检测 agent 产出的图片/文件/代码块：
  - 图片/文件 → 走 2.1 上传后 `respond_image`/`respond_file`；
  - 长代码/表格 → 优先 `respond_markdown`，超限再切文件下发。
- 保留现有 400ms 节流纯文本流作为默认路径。

**验收**：agent 生成图片/文件 → 企微收到对应图片/文件消息；长代码 → 渲染为富文本或文件，不再堆在纯文本里。

---

## P2 — 企微内审批闭环

**痛点**：`wecom_orchestrator.rs:369` 的 `ApprovalNeeded` 只推提示，无法在企微确认。

### 3.1 审批事件下发（`wecom_orchestrator.rs` + `wecom_proto.rs`）
- 把 `ApprovalNeeded` 的 `{action, detail, kind}` 序列化成企微**交互卡片 / 文本 + 指令提示**（如「回复 1 允许本次 / 2 拒绝」或 `aibot` 交互按钮）。
- 监听企微回传的用户选择（复用 `on_msg` 解析「审批回复」，按会话锁定归属当前 pending 审批）。
- 经 `agent` bridge 把决定回灌（调用现有审批回执通道 `emit_approval` 的应答接口）。

### 3.2 策略对齐
- 仅对「只读/编辑」档启用企微内审批；「审查托管/完全权限」仍回桌面端（避免远程越权）。
- 审批 5 分钟超时逻辑保持不变，超时仍自动拒绝。

**验收**：在企微里对需要确认的写文件/命令操作点「允许/拒绝」，天玑继续或中止，无需切桌面。

---

## P3 — 持久化与可观测性

**痛点**：`wecom_session.rs:90` 的 `SESSIONS` 是纯内存，重启清空；旁证 JSONL 不回读；无可视化。

### 4.1 会话历史持久化
- `session_history()` 启动时从 `%APPDATA%\...\wecom-sessions\` 回读 JSONL 重建内存；`append_turn` 保持现写盘逻辑。
- pending 配对码 `WecomState::applied` 重启即丢的问题：配对结果已落 `settings.wecomPairedUsers`（apply 时合并），无需额外改；但**待配对码 TTL 内重启即失效**，可加注提示「重启后需重新配对」。

### 4.2 运维 UI（`WecomSettingsCard.tsx`）
- 新增「企微会话日志」折叠区：读 `%APPDATA%\...\wecom-sessions\` 最近 N 条 user/assistant，只读展示，便于排查。
- 通道状态区增加「上次错误详情」「已连接时长」。

### 4.3 群聊 @ 校验（可选，低成本）
- `wecom_orchestrator.rs:171` 的 `let _ = mentioned;` 改为真实解析 body 里的 `mentioned`/`is_mentioned` 字段；当 `group_policy=Open` 且 `require_mention` 时按真实 @ 状态闸门，使「未 @ 也回复」场景可用。

**验收**：进程重启后多轮对话不丢失；设置页可看企微对话日志与连接详情。

---

## 范围外（本期不做）
- 主动推送 / 定时任务结果主动发企微（需引入调度与离线消息队列）。
- 消息撤回、已读回执、`aibot_get_msg` 拉历史。
- 跨平台（本项目 Windows-only，DPAPI 加密保持）。

## 依赖与风险
- 企微「智能机器人」素材上传 / 交互卡片的具体接口字段需对照官方文档校准（`wecom_proto.rs` 现有 `cmd` 字段如 `aibot_respond_msg` 为参考）。
- P2 审批回灌需确认 `agent.rs` 现有审批应答通道是否暴露给外部事件源，必要时新增 `agent_approve_external` 命令。
- 媒体落盘需加清理策略（按 mtime 7 天回收），避免无限增长。

## 已落地（2026-10-28 批次）

> 来源：联网扩展·企微机器人的完善度排查（结合文档/代码/日志、关联日志能力与调用链）。

### 排障能力补齐（`wecom_skywalking.rs` + `agent.rs` 工具 schema + `wecom_orchestrator.rs`）
- **调用链按 refs 重建树**：`QUERY_TRACE` 补 `refs{traceId parentSegmentId parentSpanId type}`，`build_span_tree` 用 `segmentId:spanId` 索引 + refs/parentSpanId 还原父子层级，前序缩进输出，出错 span 带 `status_code`/tags/logs，环引用有保护。
- **指标诊断 `kind=metrics`**：`listMetrics` 探测 → 端点 `endpoint_resp_time`/`endpoint_cpm` TopN（`sortMetrics`）+ 实例清单（`listInstances`），给相关性证据（不表述为因果）。
- **日志过滤增强**：`exclude_keyword`、`level`（tag）、`page`/`page_size`（封顶 100）。
- **多 header**：`parse_headers` 按换行/分号拆多条 `Key: Value`，支持鉴权版 OAP。
- **工具描述**：`skywalking_query` 说明三模式可多轮串联（日志→trace→指标）。
- **DSH 档预取关键词**：`pick_log_keyword` 按中英文分隔符切词，不再把整句塞进 content 匹配。
- **日志结构化 + 排障视图（2026-10-28）**：`LogRow` + `format_logs`——顶部「错误级条数 + 可追查 traceId（错误优先）」摘要，每行 `[LEVEL] 本地时间 svc·ep·@logger·thread·trace=…`（`fmt_ts` epoch ms→本地时间，`is_error_row`/`summarize_logs` 判错去重）。
- **DSH 档「日志⇄调用链闭环」（2026-10-28）**：`maybe_prefetch_skywalking` 无 traceId 先查日志、再按日志里第一个（错误优先）traceId 自动查调用链；有 traceId 查调用链并补该 trace 相关日志，一起注入 prompt（`fetch_logs_rows`/`first_trace_id`/`format_rows_brief`，各 20s 超时、失败静默降级）。

### 可观测性（排查 Bot 自身）
- 命令 `wecom_log_tail`（通道日志尾）、`wecom_session_recent`（最近会话旁证），设置页「排障」区两个折叠面板（通道日志 / 会话日志，按需加载）。
- `WecomStatus` 增 `lastError` / `connectedMs`（已连接时长），设置页状态区展示。

### 引擎对齐
- **DSH 档 MCP 透传**：`session/new`/`resume` 的 `mcpServers` 改为下发企微通道配置的 MCP（总开关开时），不再写死 `[]`。
- `docs/wecom-dsh-engine.md` 增「两档能力对照表」，显式标注 DSH 档不提供企微业务工具/出站富媒体。

### 正确性
- **群聊 @ 真值校验**：`wecom_proto` 解析回调里的 `mentioned`/`is_mentioned`（bool 或 0/1）为 `body_mentioned`；`on_msg` 优先用它做群闸门，缺失才回落「平台已过滤」假设并记日志。
- **问答模式新增 `auto`（默认）**：`answer_mode_rules` 按本条消息意图分流——排障（`wants_troubleshooting`）走 SkyWalking+代码、要代码（`user_text_mentions_code`）走代码、其余文档优先；修掉「固定模式下排障被文档优先挡住」的不准。`normalize_answer_mode` 空/未知回落 `auto`，设置页下拉首项「自动（按问题判断）」。
- **auto 模式重写为意图分类 + 槽位抽取（2026-10-28，新模块 `wecom_intent.rs`）**：`analyze(text)` 打分分类（Troubleshoot/ReadCode/DocLookup）+ 抽排障槽位（service / keyword 错误特征优先 / minutes 时间窗 / level / traceId）。排障口径把线索写进 SOP、DSH 预取与 tianji system 都用这份槽位组精准查询；规避「日志/模块/500 行」等误判（`has_http_error_code` 计量词不计 + doc 语境压制弱信号）。旧扁平子串判定 `wants_troubleshooting`/`user_text_mentions_code`/`pick_log_keyword`/`extract_trace_id` 收敛进新模块。
- **跨服务排障强化（2026-10-28）**：① 意图识别增 `TROUBLE_SYMPTOM` 业务症状词（慢/卡/失败/没反应/用不了…）+「问题/故障×服务/接口/线上」组合提权，让大白话症状也触发排障；② 预取默认**不限定服务、跨全部服务按错误特征查**（只有问句点名服务才传 service），窗口 30→240 分钟，顺 traceId 拉跨服务调用链看哪个 span 报错；③ 查空不静默（注入「已跨服务查无匹配，建议换词/放宽窗/给 traceId」），④ DSH 档 SOP 改「据上文已附的跨服务日志/调用链定位，不调拿不到的工具」，tianji 档维持用工具；⑤ `maybe_prefetch_skywalking` 全程写 `wecom.log` 的 `prefetch` 行（触发/跳过/命中N条/查空/失败），设置页「排障→通道日志」可查「为什么没去调日志」。
- **按关键信息搜日志（2026-10-28）**：① `wecom_intent::extract_identifiers` 不依赖空格按字符类抽业务标识符——手机号 `1\d{10}`、长数字串（订单号/用户ID，≥6 位）、字母数字混合码（ORD20241028）、带连字符/下划线 ID（req-123-abc）、长 hex；排除 HTTP 状态码与已识别 traceId。② `QaAnalysis.keywords`（有序：标识符在前、错误特征在后，封顶 3）替换旧单 `keyword`。③ `wecom_skywalking::build_log_condition` 的 `keywordsOfContent` 填整个数组（AND 收窄「订单号+error」），`parse_keywords` 兼容 `keywords` 数组与 `keyword` 逗号串。④ 预取用多关键词查；AND 查空自动退化为单（首个）关键词再查一次。⑤ `skywalking_query` 工具描述与两档 SOP 都提示「优先用订单号/用户ID/手机号/requestId 等关键信息搜日志」。
- **全局并发可配**：`Orchestrator.max_jobs`（原子量，`set_max_jobs` 1–8、越界回落默认 2），设置页「谁能用」区下拉；`settings.wecomMaxJobs` → apply 的 `maxJobs`。
- **配对码重启提示**：有待配对项时设置页「配对」区显示「重启失效、需重新发消息」提示（`pairingRestartHint`）。
- **「停止」指令真取消（2026-10-28）**：此前「停止」只被当普通消息答一句话，上一轮仍在跑、气泡一直「正在思考…」。现在 `Orchestrator.running` 按会话登记在跑回合的取消句柄（`CancelHandle::Tianji{request_id}` / `Dsh{responder}`），`on_msg` 在拿会话锁**之前**用 `is_stop_command` 识别纯停止指令（停/停止/别查了/取消/stop/cancel…，含实质内容的排障问句如「停止服务一直报错」不拦），命中即真正取消（天玑 `cancel_request` / DSH `session/cancel`）并给卡住的 stream 补一帧 finish 收尾气泡；无在跑任务回「当前没有正在进行的任务」。回合结束 `unregister_running`（仅当 stream_id 一致才移除）。

## 仍待办（需真机/产品决策）
- **出站富媒体字段校准**（D2）：`respond_image`/`respond_file`/`upload_media` 仍标「待联调验证」，需对照企微智能机器人官方素材接口核对后去标（唯一需真机联调、无法本地验证的一项）。

## 已落地（2026-10-31 批次，DSH 为主）

- **普通问答 Markdown 真流式**：DSH 非 `protected` 回合对 `agent_message_chunk` 按 350ms 节流推累计 `respond_stream`；排障仍全缓冲 + 报告校验。
- **入站富媒体**：`MediaRef` 支持仅 `media_id`；`wecom_media::materialize_inbound` 落盘 `wecom-media/<msgid>/`（7 天回收），图片内联 / 文本附件读入 / 二进制给路径。
- **DSH 出站**：回合登记 `out_targets`；回复中 `WECOM_ATTACH:<path>` 由宿主剥离并 `send_image`/`send_file`。
- **设置页**：会话指令清单、连接排查清单、docgen 启用 checklist（中英 i18n）。
