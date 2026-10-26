# 天玑（AI Hub）代码审计报告 · 2026-10-26

> 审计范围：前端对话交互链路（`src/components/aiHub/`）、Rust 回合主循环（`src-tauri/src/agent*.rs`）、权限与工具注册表。
> 结论：**架构健康，问题集中在命令执行层与前端时序**。下面每一条都标了文件行号，可逐条复核。

## 1. 能力全景

```
输入 / 附件 / 轨道 / 模式
   └─ useAskStream.send()
        ├─ routeQuestion()          对话 / 代码 / 文档 / 数据 / 调研 / 运维
        ├─ useAskContext.getContext() 120s TTL + workspaces fingerprint
        ├─ requestHistory()         char 预算裁老轮次
        └─ invoke("agent_run")
             ├─ agent_policy::offered_tools  按档位给工具
             ├─ agent_budget::fit_context    每轮压到 48k（工具结果 → 最老轮次 → 系统提示）
             ├─ model_turn()                 SSE 流式，<think> 过滤
             ├─ 工具派发                    预取 → 判定 gate → 审批 → execute
             └─ emit_done                   用量 / 预算 / undoToken
```

### 1.1 工具权限矩阵

注册表唯一事实：`src-tauri/src/agent_tools.rs` 的 `TOOLS`（47 项）。闸门换算：`agent_policy::tool_gate`。

| GateKind | 数量 | 只读 | 编辑 | 审查 | 完全 | 代表工具 |
| --- | --- | --- | --- | --- | --- | --- |
| `Look` | 19 | Allow | Allow | Allow | Allow | `read_file` `grep` `list_dir` `task` `process_output` |
| `Plan` | 1 | Allow | Allow | Allow | Allow | `update_plan`（不占动作步） |
| `Write` | 2 | Deny | Allow | Review | Allow | `write_file` `edit_file` |
| `Change` | 13 | Deny | Ask | Review | Allow | `delete_path` `kill_process` `remember` `wecom_doc_create` |
| `Command` | 2 | Deny（只读命令 Allow） | Ask | Review | Allow | `run_command` `start_process` |
| `Sensitive` | 2 | Ask | Ask | Review | Allow | `web_fetch` `clipboard_read` |
| `Workbench` | 3 | Deny（读动作 Allow） | Ask | Review | Allow | `hosts` `tunnel` `runtime` |
| `Computer` | 4 | Deny | Deny | Deny | Allow | `screenshot` `mouse` `keyboard` `wait` |
| `mcp_*` | 运行时 | Ask | Ask | Review | Allow | 任意 MCP 服务暴露的工具 |

补充规则：

- `agentAlwaysAllow*` 只让 `Gate::Ask` 免问，`Deny` / `Review` 不受影响（`agent.rs:3298`）。
- `needs_diff_confirm` 只在编辑档 + `write_file`/`edit_file` 时把 `Allow` 降成「先看 diff」。
- 任何档位都不放开：Git 冲突（`conflict_block`）、危险命令黑名单、受保护路径、删除进回收站。

### 1.2 上下文与安全设施

| 设施 | 位置 | 说明 |
| --- | --- | --- |
| 项目上下文 | `agent_context.rs` | 项目文档 / 记忆 / 技能索引，24k 字符上限 |
| 预算裁剪 | `agent_budget.rs` | 每轮三级裁剪到 48k |
| 新鲜度指纹 | `agent_freshness.rs` | git HEAD + status + dirty mtime |
| 写前快照 | `agent_undo.rs` | 20 轮 / 50MB，`undoToken` 撤销 |
| 会话旁证 | `agent_session_log.rs` | `%APPDATA%\...\agent-sessions\` JSONL |
| MCP | `mcp.rs` | stdio + 换行 JSON-RPC，一次调用一个进程 |
| Hooks | `agent_hooks.rs` | PreToolUse / PostToolUse / Stop，只读档不跑 |

### 1.3 已核对、确认不是问题的点

- `Permission::parse`（`agent_policy.rs:29-36`）精确匹配，未知值回落 `Read` → fail-closed。
- `agent_protocol.rs` 与前端 `harness.ts` / `types.ts` 的块协议字段一一对应，无漂移。
- 未发现 `Mutex` 跨 `.await` 持有；`patches[0]` / `confirming[0]` / `rounds` 索引都有前置保证。
- 上下文缓存 TTL 未进签名不算 bug（`contextTtlMs` 在调用时读取）。
- 附件不进历史回合是既定设计（每轮上下文独立），不是缺陷。

## 2. 问题清单

### P0 · 安全

**P0-1 未知工具闸门 fail-open** — `agent_policy.rs:114-115`

```rust
// Unknown tools fall through to the executor's "unknown tool" error.
None => Gate::Allow,
```

闸门表漏项（新增工具忘登记、`offered_tools` 手写了注册表没有的名字）即**全档位静默放行**。虽然 `execute_tool` 会报 unknown，但 `known_tools` 由 `offered_tools` 推导（`agent.rs:2932`），于是「未知」这一层检查也一起失效。

**P0-2 企微出站富媒体被标成只读** — `agent_tools.rs:114-115`

```rust
ToolDef { name: "wecom_send_image", group: ToolGroup::Doc, gate: GateKind::Look, ... },
ToolDef { name: "wecom_send_file",  group: ToolGroup::Doc, gate: GateKind::Look, ... },
```

`Look` = 任何档位直接放行。只读权限下模型可以把任意本地文件发进企微会话，比需要白名单 + Ask 的 `web_fetch` 宽松得多。

**P0-3 命令黑名单是 15 条子串** — `agent.rs:809-841`

`BLOCKED` 只做 `lower.contains(word)`，两个方向的失效：

| 绕过 | 说明 |
| --- | --- |
| `del /q /s x`、`rd /q /s x`、`rmdir /q /s x` | 表里只有 `"del /s"`、`"rd /s"`、`"rmdir /s"`，参数换序即不命中 |
| `powershell -EncodedCommand <b64>`、`-e`、`iex` | 命令体是 base64 / 动态执行，全部子串失效（工具自身就用这条通道，`agent_proc.rs:74-80`） |
| `ri -r -fo`、`rm`、`erase`、`[IO.File]::Delete()` | 只挡了 `remove-item` |
| `icacls` `takeown` `bcdedit` `vssadmin delete shadows` `reg delete` `sc delete` `wevtutil cl` `taskkill /im` | 表里没有 |
| `cmd /c %c%`（`set c=del`） | 变量间接 |

反向误伤也存在：`git log --grep=shutdown` 会被 `contains("shutdown")` 命中。

**P0-4 `run_command` 完全不看受保护路径** — `agent_proc.rs:132-139`

```rust
if command_blocked(&command) { return Err("这条命令被拒绝".into()); }
```

`Protected::check` 只被 `agent_fs.rs:603/630/646` 的写 / 删 / 移调用。因此全权限下 `copy a C:\Windows\x`、写启动项、改 `.git/config` 全部可行，与 `agent_policy.rs:424-426` 的策略提示词「Destructive commands and system folders are still refused」直接矛盾。

**P0-5 git 全局选项粘连击穿子命令判定** — `agent_policy.rs:157-175`

`-C` / `-c` 只匹配独立成词，粘连写法落到 `_ => break`，随后 `sub` 变成选项本身：

```
git -C../repo reset --hard   → sub = "-c../repo" → git_call_blocked → _ => false
git -C../repo push -f        → 不拦（--force 那层 token 判定兜不住 -f）
git -ccore.pager=less push -f → 不拦
```

同时 `action_kind`（`agent_policy.rs:238-241`）把这类命令归成 `cmd:git -c../repo`，用户点「以后都允许」等于授权这一整类。

### P1 · 正确性

| # | 问题 | 位置 | 影响 |
| --- | --- | --- | --- |
| P1-1 | 受保护路径是字面前缀比较 | `agent_fs.rs:181-202` | `\\?\C:\Windows\...`、8.3 短名、UNC、符号链接都不覆盖；`trees` 全来自环境变量 |
| P1-2 | 只读档可把 `Ask` 永久化 | `agent.rs:3278` / `3298` | MCP / `web_fetch` / `clipboard_read` 点一次「以后都允许」跨会话常开，且 `needs_confirm` 不复查档位 |
| P1-3 | 先 emit 审批再注册 waiter | `agent.rs:3320-3346` vs `1066-1069` | 决定早于注册到达 → `AiApprovalStale`，用户白等 5 分钟 |
| P1-4 | `wait_decision` 200ms 轮询 + `expect("map")` | `agent.rs:1066-1088` | 决定滞后 ≤200ms、空转唤醒；Mutex 中毒后每次审批 panic |
| P1-5 | 并发预取不可取消 | `agent.rs:2814-2860` | 大仓库 grep / 子代理期间点停止不生效 |
| P1-6 | `forceStop` 只做本地收尾 | `useAskStream.ts:439-450` | 后端旧 run 仍在跑，`busy=false` 后用户可再发一问 → 两个 `agent_run` 并发操作同一仓库 |
| P1-7 | `rememberSteps` 在 setState updater 内写 ref | `useAskStream.ts:266-272` vs `379/391` | updater 在渲染期执行，`finishTurn` 同步读 ref 会漏最后一帧 → 存进会话的 steps 停在 `running`，且 `stepsMutatedWorkspace` 判定失败 → 上下文缓存不失效 |
| P1-8 | 卸载不注销监听 | `useAskStream.ts:808-814` | 只 `streamIdRef.current++`，`unlisteners` 是 `send` 局部变量，外部取不到 |
| P1-9 | 审批卡先清后 await | `useAskStream.ts:939` | `agent_decide` 抛错时卡片已消失，只能等自动拒绝 |
| P1-10 | 预取漏 `await_cancellable` | `agent.rs:2814/2859` | 与 P1-5 同源 |
| P1-11 | `step_arg` 无长度上限 | `agent.rs:577-589` | `clipboard_write` 的整段文本、heredoc 命令全量进 IPC 事件，且同一调用重复 emit 2-3 次 |
| P1-12 | 只读档仍下发必被拒的工具 | `agent_policy.rs:312-347` | `hosts/tunnel/runtime/clipboard_read/wecom_*_create` 在只读档是 `Deny`，模型白试、白扣动作步 |

### P2 · 性能与结构

| # | 问题 | 位置 | 影响 |
| --- | --- | --- | --- |
| P2-1 | 每个 chunk 全树重渲 | `AskPanel.tsx:119/136` | `{...props}` 全摊 + 两个子组件无 `memo`，答案每 30ms 变化都重渲输入卡片、模型选择器、权限菜单 |
| P2-2 | 同一份历史序列化两遍 | `useAskStream.ts:726-728` | 同时下发 `history` 与 `messages`，后端只取 `messages` |
| P2-3 | 流式拼接 O(n²) | `useAskStream.ts:526-531` | 每 chunk `full += text` 后 `stripThinking(full, true)` 全量重扫 |
| P2-4 | 每轮重复 token 估算 | `agent.rs:3048/3085/3141-3143` | `serde_json::to_string(&tools)`、`messages.clone()`、全量 `estimate_tokens` 各跑一遍 |
| P2-5 | 结算收尾四份重复实现 | `useAskStream.ts:425/439/593/754` | 任一处漏改就出现「loading 不复位」类 bug |
| P2-6 | 巨型单元 | `agent.rs` 3888 行；`useAskStream.send` 单函数 ~465 行 | 难审、难测 |
| P2-7 | 记住的审批仓库键无上限 | `AiHubApp.tsx:301-327` | 每开一个新工作区新增一条，永不回收 |
| P2-8 | 无整轮墙钟超时 | `agent.rs` | 只有 48 轮 × 单命令 600s，极端情况可挂数小时 |

## 3. 修复路线

### 批次 A · 收口闸门（低风险）

1. `tool_gate` 的 `None` 分支改 `Gate::Deny`（未知 = 拒绝，执行器的 unknown 作为第二道）。
2. `offered_tools` 改为「按闸门推导」：`Deny` 的档位不下发，删掉手写的、实际必被拒的 8 项。
3. `wecom_send_image` / `wecom_send_file` 升 `GateKind::Sensitive`。
4. `can_remember` 在只读档关闭；`mcp_*` 不提供「以后都允许」。
5. 守门测试：`offered_tools` 返回的每个名字都必须在 `TOOLS` 里。

### 批次 B · 加固命令与路径（中风险，需防误伤）

1. 黑名单结构化：程序 + 子命令 + flag 集合；`del/rd/rmdir` 用 token 集合而非参数顺序；`powershell -EncodedCommand/-e/-c` 先 base64 解码再递归判一次；补 `icacls/takeown/bcdedit/vssadmin/reg/sc/wevtutil`。
2. `git_calls` 支持 `-C<dir>` / `-c<k=v>` 粘连，未知 `-` token 一律 fail-closed。
3. `Protected` 下沉到 `run_command` / `start_process`：抽取 `>` 重定向目标与命令里的绝对路径，命中受保护树即拒。
4. `Protected::check` 剥 `\\?\` / `\\.\`、展开 8.3 短名、拒绝 UNC、`USERPROFILE\.ssh/.aws/.npmrc` 入树。

### 批次 C · 正确性与性能

1. 审批改「先注册 waiter 再 emit」+ `select!` 等待 + 锁中毒降级。
2. 预取套 `await_cancellable`；`step_arg` 截断 200 字符。
3. 前端：`forceStop` 先发 `cancel_request`；`rememberSteps` 同步写 ref；监听收进 `useRef` 由 cleanup 注销；`decideAgent` 成功后再清卡片；`stopStream` 失败复位 `stopRequestedRef`。
4. 渲染：拆分 `AskThread` / `AskComposer` 的 props 并 `memo`，滚动跟随改用 deferred 文本；去掉重复的 `history` 载荷。

## 4. 验证

```powershell
cargo test --manifest-path src-tauri/Cargo.toml
$env:TEMP=(Resolve-Path .build-tmp).Path; $env:TMP=$env:TEMP; npm run build
```

新增单测：`git -C../repo reset --hard` 必须被拦；`del /q /s` 与 `del /s /q` 都必须被拦；`powershell -EncodedCommand <del 的 base64>` 必须被拦；`npm run build` / `git status` / `git log` 不得被误伤。

## 5. 落地情况（2026-10-26 当日）

验证结果：`cargo test` **547 passed / 0 failed**，`npm run build` 通过。

### 已修

| 编号 | 改动 | 位置 |
| --- | --- | --- |
| P0-1 | 未知工具闸门 `None => Deny`；新增守门测试「下发清单必须是注册表的子集」 | `agent_policy.rs:114`、测试 `offered_tools_are_all_registered` |
| P0-2 | `wecom_send_image/file` → `GateKind::Sensitive`（并计入动作步） | `agent_tools.rs:114-115` |
| P0-3 | 黑名单从 15 条子串改为「程序名 + 子命令 + 参数」；`del/rm/rd/Remove-Item` 整类拒（绕过回收站）；`-EncodedCommand` 先 base64（UTF-16LE）解码再递归判；`reg/sc/certutil` 按子命令；`cmd /c`、`powershell -c` 递归剥一层 | `agent.rs` `command_blocked` |
| P0-4 | shell 接上 `Protected`：`command_write_guard` 拦受保护目录、`%SystemRoot%` 等环境变量间接、UNC、写类命令的路径参数 | `agent_fs.rs`、`agent.rs` 的 `run_command` / `start_process` 分支 |
| P0-5 | `git_calls` 支持 `-C<dir>` / `-c<k=v>` 粘连，未知全局选项 `unknown_option` → fail-closed | `agent_policy.rs:148-200`、`git_call_blocked` |
| P1-1 | `Protected::check` 先剥 `\\?\` / `\\.\`、`canonicalize` 最长存在祖先（展开 8.3 / 符号链接）、UNC 先按字面拒（避免网络卡顿）、凭证目录入树 | `agent_fs.rs` `path_key` / `real_path` |
| P1-2 | 只读档不给「以后都允许」，`needs_confirm` 在只读档不认历史记忆，`mcp_*` 一律不记 | `agent.rs` 阶段 1 |
| P1-3 | 审批改为「先注册 waiter、再 emit 卡片」 | `register_waiter` / `await_waiter` |
| P1-4 | `wait_decision` 的 200ms 轮询改 `select!`（决定 / 超时 / 取消三出口）；两个静态表改 `unwrap_or_else(into_inner)` | `agent.rs:1064-1112` |
| P1-5 | `prefetch_readonly` / `prefetch_tasks` 套 `await_cancellable`，返回 `Result` | `agent.rs` |
| P1-6 | `forceStop` 先发 `cancel_request` 并递增 generation | `useAskStream.ts` |
| P1-7 | `rememberSteps` 同步写 ref（与 `rememberPlan` 一致） | `useAskStream.ts` |
| P1-8 | 监听收进 `unlistenersRef`，卸载时 `dropListeners()` 真正注销 | `useAskStream.ts` |
| P1-9 | `decideAgent` 成功后才撤审批卡；`AI_APPROVAL_STALE` 才丢弃 | `useAskStream.ts` |
| P1-11 | `step_arg` 截断 200 字符（按字符，不切半个中文） | `agent.rs` `clip_chars` |
| P1-12 | 只读档不再下发必被拒的企微写工具（下发改由 `writes` 决定）；新增守门测试 | `agent_policy.rs` `offered_tools` |
| P2-2 | 去掉重复的 `history` 载荷，只发 `messages` | `useAskStream.ts` |
| P2-3 | chunk 用 rAF 合并，一帧只 `setAnswer` 一次；无 `<` 时跳过 `stripThinking` | `useAskStream.ts` |
| P2-5 | `stopStream` 失败复位 `stopRequestedRef` | `useAskStream.ts` |
| 其他 | `running` 步骤只在真会执行时发（Ask 等审批不再显示"正在执行"） | `agent.rs` 阶段 1 |
| 其他 | `ai-hub-shown` 的 1.5s 上下文采集计时器可取消，不叠加 | `useAskEvents.ts` |
| 其他 | `MarkdownView` 加 `memo`；滚动跟随改依赖 `useDeferredValue` 后的文本；输入框补 `aria-label`，面板补 `id` / `aria-activedescendant` | `MarkdownView.tsx`、`AskThread.tsx`、`AskComposer.tsx`、`CommandPalette.tsx` |

### 未做（留作后续，风险/收益不划算）

- **`AskComposer` / `AskThread` 整体 `memo`**：`AiHubApp` 里 `repoControl` / `modelControl` / `modeChip` 以及十余个 `onXxx` 都是内联字面量，每次渲染都是新引用，memo 会全部失效。要让 memo 生效得先把这些节点与回调 `useMemo` / `useCallback` 化——那是 `AiHubApp.tsx` 的一次独立重构，不该和安全修复混在一批。当前用 `MarkdownView` 的 memo + rAF 合并已拿掉大部分每帧开销。
- **`agentAlwaysAllowByRepo` 的仓库键 LRU 上限**（P2-7）：只影响设置体积。
- **整轮墙钟超时**（P2-8）：需要产品上定一个上限值，暂不动。
- **`agent.rs` 拆文件**（P2-6）：纯结构收益，改动面太大。

## 6. 第二轮复审（同日，覆盖上一轮没深挖的模块）

范围：`agent_stream` / `agent_budget` / `agent_context` / `harness_config` / `mcp` / `agent_undo` / `agent_web` / `agent_docs` / `agent_table` / `agent_computer` / `agent_session_log` / `agent_memory` / `useAskContext` / `sessionCompact` / `repoSelection` / `AgentActivity`。

### 本轮已修

| 级别 | 问题 | 位置 |
| --- | --- | --- |
| **P0** | `read_document` / `read_table` / `table_query` 标了 `parallel: true`，但 `execute_readonly` 不认它们 → 一轮里出现 2 个以上只读调用时，并发预取直接返回「不是只读文件工具」，文档/表格工具**间歇性可用**。已把三者并入 `execute_readonly`，`execute_tool` 的同名分支删除，实现只剩一处 | `agent.rs` `execute_readonly` |
| **P0** | 写文件时**先拍快照、后做受保护校验**：`write_file ~/.ssh/id_rsa` 即使被拒，内容也已经被逐字节复制进 `%APPDATA%\...\agent-snapshots\` 保留 20 轮 | `agent.rs` `write_file`/`edit_file` 分支 |
| **P0** | `web_fetch` 的 2MB 上限形同虚设：`resp.bytes()` 先整段读进内存再比大小，chunked / 无 `Content-Length` 的服务器完全不受约束 | `agent_web.rs` `web_fetch` |
| **P1** | `read_rows` 没有体积检查（`read_document` 有）：GB 级 CSV 先全量读入再全量解析，最后才 truncate | `agent_docs.rs` `read_rows` |
| **P1** | docx 解压后无上限（zip 炸弹）：1MB 的 zip 可膨胀成几 GB `document.xml` | `agent_docs.rs` `docx_text`（`take(32MB)`） |
| **P1** | Anthropic 的 `tool_result` 带截图时 `content` 是**数组**，`content_text` 只认字符串 → 这条工具结果按 0 token 计，三级裁剪绕过它 | `agent_budget.rs` `parts_text` |
| **P1** | `.ai-workbench/` 里的 `context.md` / skills / `hooks.json` 可以是符号链接，指向 `~/.ssh/id_rsa` 时内容会直接进系统提示 | `harness_config.rs` `within()` |
| **P1** | `useAskContext` 先把 `chipsSig` 记账再采集：采集中途抛错后，新签名配上旧缓存，下一次提问读到**上一个仓库**的上下文 | `useAskContext.ts` `getContext` |
| **P2** | 上下文指纹采在采集**之后**（描述的是新树、内容是旧树），采集期间保存一次就会「新指纹 + 旧内容」缓存 120 秒 | 同上 |
| **P2** | secondary 仓库对最多 6 个各跑 status + 2 次 diff，最后才 `slice(0,4)`，三分之一的工作白做 | `useAskContext.ts` `SECONDARY_REPOS_SHOWN` |
| **P2** | `mouse` / `keyboard` 同步跑在 tokio worker 上（内含 150-200ms 等待与拖动），期间流式输出卡住、停止键失灵 | `agent.rs` 对应分支（改 `spawn_blocking` + `await_cancellable`） |

### 待办（按性价比排序，本轮未动）

1. **MCP 进程孤儿**（`mcp.rs`）：`Child::kill()` 只杀 `cmd.exe`，真正的 `npx`/`node` 孙进程继续存活并持有 stdout 管道 → 读取线程永久阻塞。每个坏服务每次超时泄漏「1 进程 + 1 线程 + 1 管道」。建议 Job Object（`KILL_ON_JOB_CLOSE`）或 `taskkill /T /F`，并给 reader 线程一个关闭出口。
2. **MCP 每轮起进程 + 串行探测**（`mcp.rs` / `agent.rs`）：服务之间 `for … .await` 顺序握手，每个最坏 40s，5 个坏服务首轮要 200s，且这段不可取消。建议 `join_all` + 按轮缓存 `tools/list`，或常驻会话。
3. **`SseLines` 没有 `flush()`、也不合并多行 `data:`**（`agent_stream.rs`）：连接在最后一个 `data:` 之后没有换行就断开时，最后一段 delta 静默丢失；多行 `data:` 会被拆成两个坏 JSON 各自丢弃。补 `finish()` + 空行事件边界即可。
4. **Anthropic 流式路径不过 `ThinkFilter`**（`agent.rs:2717-2722`）：思考内容进了 `text_delta` 时会原样进 UI / 会话日志。
5. **`fit_context` 压不下去也不报**（`agent_budget.rs`）：`room.max(2_000)` 会把已扣掉工具表的预算又抬回去；单轮长任务（1 个 user turn + 30 次调用）二级裁剪完全失效。建议返回 `overflowed` 并做硬切。
6. **`table_query` 没有执行代价上限**（`agent_table.rs`）：`limit` 只限制输出行，递归 CTE 能把调用线程钉死数分钟；建议 `progress_handler` + 时间预算，黑名单换白名单。
7. **`agent_undo` 的并发与原子性**：manifest 读改写无锁、`.bak` 名用 `entries.len()` 会互相覆盖、`fs::write` 非原子、配额只在「本轮第一条记录」时算一次。
8. **`harness_config` 的 YAML fail-open**：frontmatter 写错时整个 `SkillMeta` 变 default，`allowed_tools` 空 = 不收窄工具，本来只给 `read_file` 的技能反而全放开。
9. **`agent_session_log` 永不清理**（`%APPDATA%\...\agent-sessions\`，无轮转）、`agent_memory` 读改写非原子且单条无长度上限。
10. **`agent_hooks` 的临时日志在 spawn 失败路径泄漏**（`%TEMP%\aw-hook-*.log`）。
11. **`harness_list_files` 是同步 command**：Tauri 2 里跑在主线程，`@` 补全每敲一键就把工作区走一遍（上限 2 万条），大仓库会卡 UI。建议 `async` + 缓存索引。
12. **`AgentActivity` 的步骤列表没有 memo 边界**，与流式 chunk 同频重渲（同 §5 的 `AiHubApp` 内联回调问题，属同一次重构）。

