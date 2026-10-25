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
