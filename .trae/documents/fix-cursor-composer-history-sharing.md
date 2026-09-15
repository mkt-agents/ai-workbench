# 修复 Cursor 多账号 Composer 会话历史共享

## Context

**项目背景**：d:/ai_project/ai-workbench 是 Tauri 桌面应用，实现 Cursor 多账号切换（因 Cursor 不支持多账号切换，每次登录麻烦）。用户期望：**多账号切换 + 工作空间和会话共享**。

**当前问题**：用户反馈"手动启动 Cursor（图1）"和"Cursor 页启动 Cursor（图2）"两个会话未共享。图1 有 Pinned/Today/Yesterday/Older 多个 Composer 会话（如"(2) 商品区间匹配"），图2 仅有 "Model inquiry / 什么模型" 一个会话。

**根因分析**（已在源码核实）：

项目已设计了"账号隔离 + 共享层"机制：profile 目录作为 `--user-data-dir`，`workspaceStorage/History/snippets/extensions/settings/globalStorage 子目录`通过 junction 链接到默认 Cursor 目录共享，`state.vscdb*` 按 profile 隔离以保持登录态独立。Composer 会话历史存储在 `state.vscdb` 的 `ItemTable` 中（如 `composer.composers` 等 key），通过"非认证 key 同步"机制理论上应该跨账号共享。

但有 3 个 bug 导致失效：

1. **`seed_shared_state_from_default`（[mod.rs:1527-1538](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1527-L1538)）是一次性的**：依赖 marker `.shared-state-seeded-v1`，第一次执行后写入 marker，之后默认目录新增的 Composer 会话永远不会流入 shared 层
2. **`sync_profile_state_to_shared`（[mod.rs:1541-1550](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1541-L1550)）只在切换账号路径（[mod.rs:2694](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L2694)）调用一次**，普通退出 Cursor 不触发反向同步
3. **同步语义是 last-write-wins**（`INSERT OR REPLACE`，[mod.rs:1368](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1368)）：跨账号切换会话列表会被后启动的账号覆盖，而非合并

## 解决方案（基于 Step 1 调研结果修正）

### Step 1 调研输出（已完成）

通过新增 `dump_state` 调试 binary 直接查询 state.vscdb 表结构，**根本原因与原假设完全不同**：

**Cursor 的 state.vscdb 有 3 张表**：
- `ItemTable` (616 rows) — VS Code 标准键值存储，**当前已被同步**（[mod.rs:1325](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1325)）
- `composerHeaders` (**863 rows**) — **Composer 会话列表核心存储，未同步**！
- `cursorDiskKV` (**285,392 rows**) — Cursor 磁盘 KV，**未同步**！

**composerHeaders 表结构**：
```
composerId(TEXT) workspaceId(TEXT) createdAt(INTEGER) lastUpdatedAt(INTEGER)
isArchived(INTEGER) isSubagent(INTEGER) recency(INTEGER) checkpointAt(INTEGER)
value(TEXT) subagentTypeName(TEXT)
```
- 主键: `composerId`（UUID）
- `workspaceId` 是工作区路径的 32 字符 hash（如 `91edef1dc56b88544ee3afecf9aaa390`）
- `value` 是 JSON 头数据（含 `name`、`composerId`、`lastUpdatedAt` 等）
- 该表对应侧边栏看到的会话列表

**cursorDiskKV 表中 Composer 相关 key 前缀**：
- `composerData:<UUID>` (884 rows) — Composer 会话完整数据
- `composer.content.<hash>` (多个) — Composer 内容片段
- `composerVirtualRowHeights` (60 rows) — UI 状态
- `codeBlockDiff:*`, `codeBlockPartialInlineDiffFates:*` — 代码差异块

**cursorDiskKV 表中应排除的 key 前缀**（可能含登录态/账号信息）：
- `agentKv:*` (162,498 rows) — subagent 状态（保守排除）
- `bubbleId:*`, `checkpointId:*`, `ofsContent:*` — 临时对话数据，体积大且非 Composer 列表必需
- `inlineDiff:*`, `inlineDiffs:*` — 临时编辑数据

**根因更新**：项目当前同步逻辑（[mod.rs:1317-1345](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1317-L1345) `SELECT key, value FROM ItemTable`）**只同步 ItemTable 表**，完全未同步 `composerHeaders` 和 `cursorDiskKV` 表。这导致：
1. 手动启动 Cursor 写入默认目录 composerHeaders 的会话列表，无法流到 profile
2. profile 内创建的新会话只写入 profile 的 composerHeaders，无法流回默认目录或共享层
3. 多个 profile 之间 composerHeaders 完全隔离

### 修改方案（简化）

**不需要 JSON 合并器**——composerHeaders 是按行存储（每行一个 composerId），通过 SQL `INSERT OR REPLACE` 按 `composerId` 主键合并即可天然实现 union 语义。

**新增 3 个同步函数**：

1. `export_composer_headers(src_db) -> Vec<ComposerHeaderRow>` — 导出 composerHeaders 全表
2. `import_composer_headers(dst_db, rows)` — INSERT OR REPLACE by composerId
3. `export_cursor_disk_kv(src_db, prefix_filter) -> HashMap<String, Value>` — 按 prefix 导出 cursorDiskKV（仅 `composerData:` 和 `composer.content.` 前缀）
4. `import_cursor_disk_kv(dst_db, map)` — INSERT OR REPLACE by key

**新增双向同步函数**（替换原 `sync_shared_state_to_profile`/`sync_profile_state_to_shared`，**保留 ItemTable 同步不变**）：

- `sync_default_composer_to_shared()` — 默认目录 composerHeaders + cursorDiskKV(composerData/composer.content) → shared
- `sync_profile_composer_to_shared(profile)` — profile → shared
- `sync_shared_composer_to_profile(profile)` — shared → profile

### 实施步骤（修订）

**Step 2 → 新增 composerHeaders + cursorDiskKV 同步函数**
- 在 `mod.rs` 新增 `export_composer_headers` / `import_composer_headers` / `export_cursor_disk_kv` / `import_cursor_disk_kv` 函数
- 关键文件：`src-tauri/src/cursor/mod.rs`，新增在 `import_non_auth_keys` ([mod.rs:1347-1382](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1347-L1382)) 附近

**Step 3 → 在 `prepare_profile_shared` 中加入 composer 同步链路**
- 新顺序：
```rust
fn prepare_profile_shared(profile: &Path) -> Result<(), String> {
    seed_shared_from_default()?;
    link_profile_to_shared(profile)?;
    seed_shared_state_from_default()?;               // 保留：ItemTable 一次性 seed（marker 控制）
    merge_recent_workspaces_from_default()?;         // 保留：单 key JSON union
    sync_shared_state_to_profile(profile)?;           // 保留：ItemTable shared→profile
    // 新增 Composer 同步链路（composerHeaders + cursorDiskKV）
    sync_default_composer_to_shared()?;
    sync_profile_composer_to_shared(profile)?;       // profile→shared（拉回项目内会话）
    sync_shared_composer_to_profile(profile)?;       // shared→profile（落地）
    Ok(())
}
```
- 关键文件：`src-tauri/src/cursor/mod.rs` [line 725-738](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L725-L738)

**Step 4 → Cursor 退出时补 profile→shared 同步**
- 抽取公共函数 `sync_running_profile_state_to_shared(account_id: &str)`
- 在切换账号路径 ([mod.rs:2694](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L2694)) 调用

**Step 5 → 删除调试代码**
- 删除 `dump_state` binary（[src-tauri/src/bin/dump_state.rs](file:///d:/ai_project/ai-workbench/src-tauri/src/bin/dump_state.rs)）
- 删除 `dump_state_keys` / `dump_state_value` Tauri 命令及注册
- 删除 `Cargo.toml` 中的 `dump_state` bin 配置

## 关键复用点

- [`open_sqlite_ro`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1553-L1564)：只读打开 SQLite，避免锁定 Cursor 正在使用的数据库
- [`export_non_auth_keys`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1317-L1345) / [`import_non_auth_keys`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1347-L1382)：已有的非认证 key 读写函数
- [`read_state_value`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1407-L1430) / [`write_state_value`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1432-L1440)：单 key 读写
- [`merge_recent_workspaces_json`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1461-L1500)：JSON 合并器范本，新合并器仿此实现
- [`is_auth_key`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1246-L1250)：认证 key 判断（保持不变，登录态隔离边界）

## 隔离边界（保持不变，不破坏登录态隔离）

- `GLOBAL_STORAGE_EXCLUDED`（[mod.rs:582-590](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L582-L590)）：`state.vscdb*`、`storage.json`、`backups` 仍按 profile 独立，不被 junction 共享
- `is_auth_key`（[mod.rs:1246-1250](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1246-L1250)）：`cursorAuth/*`、`glass.lastSignedInAuthId`、`adminSettings.cachedAuthId` 仍被排除，不进入共享层
- `cursor_profile_dir(account_id)`（[mod.rs:168-173](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L168-L173)）：每个账号仍使用独立 profile 目录作为 `--user-data-dir`

## 验证步骤

### 调研阶段验证（Step 1 后）
1. 运行 `dump_state_keys target=default` 命令
2. 确认输出包含疑似 Composer 会话 key（如 `composer.composers`）
3. 用 `dump_state_value target=default key=composer.composers` 查看具体 JSON 结构（数组？对象？会话 ID 字段名？）
4. 同样查询 `target=shared` 和 `target=profile`，对比是否一致

### 实施后端到端验证
1. 清空共享层 marker 和 shared state DB：
   - 删除 `app_data_dir/cursor-shared/.shared-state-seeded-v1`
   - 删除 `app_data_dir/cursor-shared/User/globalStorage/state.vscdb*`
2. 手动启动 Cursor，创建若干 Composer 会话（如"测试会话1"），关闭 Cursor
3. 用项目启动账号 A 的 Cursor：
   - 启动后立即检查侧边栏，应能看到"测试会话1"
   - 在账号 A 内创建新会话"账号A的会话"，关闭 Cursor
4. 切换到账号 B 启动：
   - 侧边栏应同时显示"测试会话1" + "账号A的会话"（合并语义生效）
   - 在账号 B 内创建"账号B的会话"，关闭 Cursor
5. 切换回账号 A：
   - 侧边栏应同时显示"测试会话1" + "账号A的会话" + "账号B的会话"
   - 检查账号 A 仍处于登录态（Settings → Account 显示账号 A 邮箱，未被污染）
6. 再次手动启动 Cursor（默认目录），新增"手动新增会话"
7. 用项目启动账号 A，确认侧边栏出现"手动新增会话"（默认目录增量同步生效）
8. 重复步骤 4-5 多轮，验证不会出现会话丢失或重复

### 隔离边界验证（必须确认未被破坏）
1. 启动账号 A 后，检查 `cursor-profiles/A/User/globalStorage/state.vscdb` 的 `cursorAuth/accessToken` 应是账号 A 的 token
2. 切换到账号 B，检查 `cursor-profiles/B/User/globalStorage/state.vscdb` 的 `cursorAuth/accessToken` 应是账号 B 的 token
3. 两个 token 应不同，确认登录态隔离未被破坏

## 实施顺序

1. Step 1 调研（先做，输出决定 Step 2/5 的 key 名和 JSON 结构）
2. Step 2 合并器
3. Step 3 改造 seed → merge
4. Step 5 改造 sync 使用合并语义
5. Step 4 调整 prepare_profile_shared 顺序
6. Step 6 Cursor 退出时补同步
7. 端到端验证

## 风险与回退

- **风险**：Composer 会话 JSON 结构可能不是简单的会话 ID 数组（如嵌套对象、按时间分桶），合并器需根据 Step 1 调研结果调整
- **风险**：state.vscdb 被 Cursor 长期占用，写入时可能 SQLITE_BUSY。已有 `PRAGMA busy_timeout=8000`（[mod.rs:1359](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1359)），但极端情况需重试
- **风险**：增量合并可能引入重复会话（如会话 ID 在两端不一致）。合并器需以会话 ID 严格去重
- **回退**：保留 marker 文件机制，如出问题可临时恢复 marker 早返回，回到"一次性 seed"行为（虽然不解决问题但不会引入新问题）
