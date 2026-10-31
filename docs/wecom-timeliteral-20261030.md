# IM 机器人排障解析优化 — 2026-10-30 时间字面量修复

## 背景

2026-10-30 17:30（msgid `f496a366b32f764be824e5909fb91ad3`，732 字）用户发来：

> 时间：2026 10-30 10:12:13- 2026 10-30 16:12:13 …

DSH 引擎确定性预解析返回「排障结论 / 尚未形成通过本地核对的诊断结论 / > 证据范围：查询条件未解析。」（耗时 0 秒，未发任何查询）。同一文本在 `tianji` 引擎下正常作答。**结论：不是模型/NL 能力问题，是 Rust 时间解析器对非标准分隔日期字面量（`2026 10-30`）的适配缺口。**

## 根因（三层，逐层修）

1. **归一化缺失**：`parse_query` 只认 `2026-10-30` / `2026年10月30日` / `2026/10/30` 等标准形态；`2026 10-30`（空格分隔年）无法归一，且日志文本里的渠道 key（如 `4745BD58-16-C7-…`）让残留守卫误判「时间条件未完整识别」。
2. **残留守卫误伤**：`parse_window` 的 `\d{1,4}[-/]\d{1,2}` 残留匹配没有 token 边界概念 —— UUID/渠道 key 内部片段 `58-16` 被当成「未识别的日期」直接拒绝整条查询。
3. **澄清文案丢失（latent bug）**：`wecom_report::fallback` 的 `reason` 参数只用于比对 `"cancelled"` / `"budget"` 类别，宿主精心拼装的「查询条件未解析：时间条件未完整识别…」从未渲染给用户，WeCom 只看到笼统的「查询条件未解析」。

## 修复内容

### P0-A 时间字面量归一化（`wecom_investigation_query.rs`）

- 新增 `normalize_dates(text, today)`：在 `parse_query` 最前端做**单次**归一化（字节偏移对下游全部一致）。
  - 带年：`(\d{4})(?:\s*[-/.年]\s*|\s+)(\d{1,2})\s*[-/.月]\s*(\d{1,2})\s*[日号]?` → `YYYY-MM-DD`；月/日越界的不改写，交给既有 `calendar_date` 校验（`2月30号` 仍报错）。
  - 无年：`M-D` + 时钟尾（`16:12` / `16点`）→ 补当前年；越界（`12-31` 在未来）跳过，让残留守卫按原语义拒绝并提示。
  - 跳过范围：双引号/单引号/弯引号字面量、`key=value` 赋值（值字符类含 `/` `:`，避免 `matchId=2026/10/28` 被改写）；`10月26号 静默查错4小时` 之类的旧负例语义不变。
- 残留守卫改 `residual_hides_time_literal`：数字日期片段仅在**独立 token**（前后都不是字母数字/`-`/`/`/`.`/`_`）时才算未识别时间；渠道 key 内部的 `58-16` 不再误伤。中文词守卫（年月号日/点分秒/今天…）不变。

### P0-B 未执行原因贯穿渲染（`wecom_investigation.rs` / `wecom_report.rs` / `wecom_orchestrator.rs`）

- `InvestigationOutcome` 新增 `clarification: Option<String>`（host 文本，永不携带 OAP 原文/URL/凭证，沿用 `wecom_sanitize` 纪律）。
- `not_executed` 增加第 4 参，四处调用点分别给出具体原因：
  - 解析失败 → `查询条件未解析：{e} 请写成 2026-10-30 16:12:13，或 2026-10-30 10:12 至 16:12。`
  - 未配置 OAP → 提示去设置页填写地址与请求头。
  - 有时间窗但缺业务标识 → 明确「未执行跨服务泛查」。
  - 意图闸门 → 不解释（保持安静）。
- `wecom_report::fallback_with_reason(original, evidence, reason, clarification)`：`clarification` 非空时 ① `query_header` 渲染 `> 证据范围：未执行查询。{原因}`，② 以 `blocking: true` 的 gap 插入未完成项首行；`fallback` 保持旧签名兼容。`reason` 参数仍是类别 token（`"not_executed"` / `"cancelled"` / `"budget"` / 其它）。
- 两个 fallback 调用点（source 失败、无证据）改走新 API；`last_diagnostic()` JSON 增加 `clarification` 字段供设置页展示。

### P1-A 新起会话语义扩充（`wecom_investigation_query.rs`）

`新起会话|新起对话|重新起会话|重新起对话|开新会话|开新对话` 加入 `starts_new_investigation` / `new_session_remainder` 词表（原有否定/引用防护不变：`不要新起会话`、`说明：新起会话`、引号内均不触发）。

### P1-B 关键词爆炸防护（`wecom_investigation_query.rs`）

- `quoted_values` 丢弃**后跟 `:` / `：`** 的引号值 —— 粘贴 JSON 时键名（`orderNo`、`matchId`）不再混进关键词 AND。
- 关键词总量封顶 `MAX_KEYWORDS_TOTAL = 8`，超出截断；`InvestigationQuery.keyword_count` 记录封顶前原始数量（诊断用）。

### P2 设置页展示

IM 机器人 → 诊断面板在 `detail` 下新增一行 `clarification`（`role="alert"`），`wecomChannel.ts` 类型同步。

## 测试

`wecom_investigation_query` 新增 4 个用例：

- `spaced_and_slash_dates_normalize_before_parsing`：`2026 10-30 10:12:13- 2026 10-30 16:12:13` → Range；单点锚 15 分钟上下文；`2026/10/30`、`2026.10.30`、无年 `10-30 16:12:13`、`2026 10-30 10:12 至 16:12` 全过。
- `date_normalization_keeps_values_and_guards`：`matchId=2026-10-28` 不被改写、引号字面量保留、未来无年日期仍报错、`2026 2-30` 交给日历校验、渠道 key 不误伤。
- `new_session_variants_reset_the_query_scope`：四种新词正例 + 否定/引用负例。
- `keyword_explosion_guard_caps_and_drops_json_keys`：JSON 键名被丢弃、11 个字面量截到 8 且 `keyword_count ≥ 11`。

`wecom_report` 新增 `unexecuted_clarification_replaces_the_bare_unknown_header`：澄清文案出现在证据范围行与首条 gap；无澄清时旧渲染字节不变。

## 已知问题（非本次引入）

`wecom_skywalking::protocol_tests` 的 `session_reuses_connection…` 与 `header_endpoint…` 在干净工作树同样失败（连接计数 4≠1，本机环境相关的连接复用断言），与本次改动无关，另行跟进。

## 验收对照

| 验收项 | 结果 |
|---|---|
| 732 字真实消息 → `requestCount > 0`、`parsed=true` | 解析层单测通过（归一化 + 守卫放行）；待重启企微通道后实测 |
| `2026 10-30 10:12:13- 2026 10-30 16:12:13` → Range | ✅ 单测 |
| 不可解析时给出具体原因而非裸「查询条件未解析」 | ✅ 单测（header + gap 双路径） |
| `cargo test`（除 2 个预存 skywalking 失败）、`npm run build` | ✅ 409 wecom 用例通过 / 前端构建零错误 |
