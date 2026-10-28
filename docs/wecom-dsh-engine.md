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
  拒绝 / 取消）；看门狗 900 秒，超时先 `session/cancel` 再收尾。

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

## 代码位置

| 文件 | 作用 |
| --- | --- |
| `src-tauri/src/wecom_dsh.rs` | DSH 引擎：进程生命周期、ACP 收发与 id 路由、会话映射、patch 生成、档位映射、模型目录与选择 |
| `src-tauri/src/wecom_dsh_profile.rs` | profile 镜像：bundle 映射、patch 过滤、`node_modules` 联接、镜像报告 |
| `src-tauri/src/wecom_channel.rs` | `engine`/`permission`/`workspace` 入参、DSH 校验与状态、按引擎启停、`PendingApproval` 枚举 |
| `src-tauri/src/wecom_orchestrator.rs` | `on_msg` 引擎分流、`run_dsh_turn`、审批码回灌（两种载荷） |
| `src-tauri/src/dsh_commands.rs` | `dsh_cli` / `dsh_installed_version` / `dsh_clear_stale_lock` 等复用入口 |
| `src/lib/wecomChannel.ts` / `components/WecomSettingsCard.tsx` | 引擎下拉、DSH 状态胶囊、档位映射说明 |
