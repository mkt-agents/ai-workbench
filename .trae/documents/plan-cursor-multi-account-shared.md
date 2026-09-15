# Cursor 多账号切换 + 工作空间/会话共享 — 设计与实施计划

## 1. 产品背景与目标

**为什么做**：Cursor 官方不支持多账号切换，每次切换都要重新登录，工作流被打断。

**用户期望**：
- **多账号切换**：保留每个账号的登录态，一键切到目标账号无需重新登录
- **工作空间共享**：A 账号打开过的项目，切到 B 账号后侧边栏仍能看到，反之亦然
- **会话共享**：A 账号创建的 Composer/Chat 会话，切到 B 账号后侧边栏能看到并继续；手动启动 Cursor 创建的会话，项目启动的 Cursor 也能看到

**一句话总结**：登录态按账号隔离，业务态（工作空间 + 会话）跨账号共享。

## 2. 现状评估

项目已落地"账号隔离 + 共享层"机制，方向正确，但有 1 个核心 gap。

### 已落地（无需重构）
- 每个账号独立 `cursor-profiles/<account_id>/` 作为 `--user-data-dir`，登录态天然隔离
- `cursor-shared/` 作为跨账号共享层
- `workspaceStorage`、`History`、`extensions`、`settings.json` 等通过 junction 链接到默认 Cursor 目录或共享目录，天然共享
- `state.vscdb*` 按 profile 隔离，`is_auth_key` 过滤 `cursorAuth/*`、`glass.lastSignedInAuthId`、`adminSettings.cachedAuthId` 等登录 key
- `ItemTable` 非认证 key 通过 `export_non_auth_keys`/`import_non_auth_keys` 双向同步（已实现）
- `history.recentlyOpenedPathsList` 通过 `merge_recent_workspaces_json` 做 JSON union 合并（已实现）
- 切换账号路径已调用 `sync_profile_state_to_shared`（[mod.rs:2978](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L2978)）

### 核心 gap
**Composer 会话历史存在两张表里，但都未同步**：

| 表 | 行数 | 作用 | 当前是否同步 |
|---|---|---|---|
| `ItemTable` | 616 | VS Code 标准键值 | 已同步 |
| `composerHeaders` | 863 | **Composer 会话列表核心存储** | **未同步** |
| `cursorDiskKV` | 285,392 | Cursor 磁盘 KV | **未同步** |

`cursorDiskKV` 中 Composer 相关 key 前缀：
- `composerData:<UUID>`（884 行）— 会话完整数据
- `composer.content.<hash>` — 会话内容片段

应排除的 `cursorDiskKV` key 前缀（隔离/临时/性能边界）：
- `agentKv:*`（162,498 行）— subagent 状态，保守排除
- `bubbleId:*`、`checkpointId:*`、`ofsContent:*` — 临时对话数据，体积大
- `inlineDiff:*`、`inlineDiffs:*` — 临时编辑数据

**直接后果**：
1. 手动启动 Cursor 写入默认目录 `composerHeaders` 的会话，无法流到项目启动的 profile → 用户截图里的"两个会话"问题
2. profile 内创建的新会话只写入 profile 的 `composerHeaders`，无法流回默认目录或其他 profile → 多账号会话完全隔离
3. 工作空间共享已生效，但用户感知不到，因为会话历史没共享

## 3. 设计原则

| 原则 | 含义 |
|---|---|
| 账号隔离优先 | 登录态、认证 key、cursorAuth 永远不进共享层；违反即回滚 |
| 共享层是唯一真相 | 所有 profile/默认目录的 composer 数据最终都流向 `cursor-shared` 层 |
| 双向同步 | default ↔ shared、profile ↔ shared 都要覆盖，单向必然丢会话 |
| 启动合并、退出回流 | 启动前 shared→profile 落地；退出/切换前 profile→shared 回流 |
| 主键合并而非覆盖 | `composerHeaders` 按 `composerId` 主键 `INSERT OR REPLACE`；`cursorDiskKV` 按 `key` 主键合并，天然 union 语义 |
| last-write-wins | 同一 `composerId` 在两端都有更新时，以 `lastUpdatedAt` 更新者为准（实际 SQL 行为：后写覆盖，配合启动顺序足够） |
| 不写 Cursor 不在用的 DB | 用 `open_sqlite_ro` 读、`busy_timeout=8000` 写、`INSERT OR REPLACE` 单事务 |

## 4. 改造方案

### 4.1 已完成（Step 1–2）

**Step 1**：调研 Composer 存储（已通过 `dump_state` binary 确认表结构和 key 前缀）

**Step 2**：已实现 4 个同步原语 + 3 个组合函数（[mod.rs:1417-1670](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1417-L1670)）：
- `export_composer_headers(src_db) -> Vec<ComposerHeaderRow>`
- `import_composer_headers(dst_db, rows)` — INSERT OR REPLACE by composerId
- `export_cursor_disk_kv(src_db) -> HashMap<String, Value>` — 仅导出 `composerData:` 和 `composer.content.` 前缀
- `import_cursor_disk_kv(dst_db, map)` — INSERT OR REPLACE by key
- `sync_default_composer_to_shared()` — default → shared
- `sync_profile_composer_to_shared(profile)` — profile → shared
- `sync_shared_composer_to_profile(profile)` — shared → profile

### 4.2 待实施

#### Step 3：在 `prepare_profile_shared` 中接入 Composer 同步链路

**文件**：`src-tauri/src/cursor/mod.rs` [line 726-739](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L726-L739)

**改造**：
```rust
fn prepare_profile_shared(profile: &Path) -> Result<(), String> {
    seed_shared_from_default()?;
    link_profile_to_shared(profile)?;

    // ItemTable 同步（保留）
    if let Err(e) = seed_shared_state_from_default() {
        eprintln!("[cursor] shared state seed: {e}");
    }
    if let Err(e) = merge_recent_workspaces_from_default() {
        eprintln!("[cursor] recent workspace merge: {e}");
    }
    sync_shared_state_to_profile(profile)?;

    // 新增 Composer 双向同步链路
    // 1. default → shared：手动启动 Cursor 创建的会话先流入共享层
    sync_default_composer_to_shared()?;
    // 2. profile → shared：上次本 profile 内创建/更新的会话回流（保险）
    sync_profile_composer_to_shared(profile)?;
    // 3. shared → profile：把所有账号的会话落地到本 profile，供 Cursor 启动时读取
    sync_shared_composer_to_profile(profile)?;

    Ok(())
}
```

**顺序不能错**：default → shared 必须在 shared → profile 之前，否则本 profile 拿不到手动启动新增的会话；profile → shared 在中间作为保险，覆盖上次启动后未回流的情况。

#### Step 4：扩展退出/切换路径的 Composer 回流

**问题**：当前 `sync_profile_state_to_shared`（[mod.rs:1825](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1825)）只同步 ItemTable，未同步 Composer；且只在切换账号路径调用，手动关闭 Cursor 不触发。

**改造 A：扩展 `sync_profile_state_to_shared` 覆盖 Composer**

```rust
fn sync_profile_state_to_shared(profile: &Path) -> Result<(), String> {
    let profile_db = live_state_vscdb_in(profile);
    if !profile_db.exists() {
        return Ok(());
    }
    let shared_db = shared_state_vscdb()?;

    // ItemTable 非认证 key（保留）
    let keys = export_non_auth_keys(&profile_db)?;
    if !keys.is_empty() {
        import_non_auth_keys(&shared_db, &keys)?;
    }

    // 新增：Composer 数据回流（profile → shared）
    let headers = export_composer_headers(&profile_db)?;
    if !headers.is_empty() {
        import_composer_headers(&shared_db, &headers)?;
    }
    let kv = export_cursor_disk_kv(&profile_db)?;
    if !kv.is_empty() {
        import_cursor_disk_kv(&shared_db, &kv)?;
    }
    Ok(())
}
```

**改造 B：覆盖两条回流路径**

1. **切换账号路径**（已调用 `sync_profile_state_to_shared`，[mod.rs:2978](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L2978)）：扩展后自动覆盖 Composer，无需新增调用点
2. **手动关闭 Cursor 路径**：本期不实现进程退出监听（依赖 Cursor 退出后下次启动时的 `prepare_profile_shared` 中的 `sync_profile_composer_to_shared` 兜底回流，足够满足"下次启动看到所有会话"目标）

> 进程退出监听需要 Tauri 监听 child process 退出事件，复杂度高且非阻塞需求，留作后续优化。

#### Step 5：删除调试代码

- 删除 `src-tauri/src/bin/dump_state.rs`
- 删除 `Cargo.toml` 中 `[[bin]] name = "dump_state"` 配置（[Cargo.toml:49-50](file:///d:/ai_project/ai-workbench/src-tauri/Cargo.toml#L49-L50)）
- 删除 `dump_state_keys` / `dump_state_value` Tauri 命令及其在 `lib.rs` 的注册

> 保留 `cursor_switch_test` binary（已有自测入口）。

#### Step 6：编译 + 端到端验证

详见 §5。

## 5. 验证方案

### 5.1 隔离边界验证（必须先做，确认未破坏登录态）

1. 账号 A 启动后，检查 `cursor-profiles/A/User/globalStorage/state.vscdb` 中 `cursorAuth/accessToken` 是 A 的 token
2. 切换到账号 B 启动，检查同位置 token 是 B 的
3. 两个 token 必须不同

### 5.2 会话共享端到端验证

1. **清空共享层**（确保从零开始）：
   - 删除 `app_data_dir/cursor-shared/.shared-state-seeded-v1`
   - 删除 `app_data_dir/cursor-shared/User/globalStorage/state.vscdb*`

2. **手动启动 Cursor**（默认目录），创建会话 S1（命名醒目如"测试会话1"），关闭 Cursor

3. **项目启动账号 A**：
   - 启动后侧边栏应显示 S1
   - 在 A 内创建会话 S2"账号A的会话"
   - 关闭 Cursor

4. **切换到账号 B**：
   - 侧边栏应同时显示 S1 + S2
   - 在 B 内创建 S3"账号B的会话"
   - 关闭 Cursor

5. **切换回账号 A**：
   - 侧边栏应同时显示 S1 + S2 + S3
   - Settings → Account 仍显示账号 A 邮箱（登录态未被污染）

6. **再次手动启动 Cursor**：
   - 侧边栏应出现 S1 + S2 + S3（profile → shared 回流生效）

7. 多轮重复 3-5，确认不会出现会话丢失或重复

### 5.3 工作空间共享验证（已有功能，回归确认）

- 账号 A 打开项目 P1，关闭 Cursor
- 切换到账号 B，侧边栏 Recent 应显示 P1

## 6. 风险与回退

| 风险 | 缓解 |
|---|---|
| `cursorDiskKV` 写入量大导致启动慢 | 只同步 `composerData:` + `composer.content.` 前缀（约 884 + N 行），不碰 `agentKv:*`（162k 行） |
| state.vscdb 被 Cursor 占用，写入 SQLITE_BUSY | `open_sqlite_ro` 只读打开 + `PRAGMA busy_timeout=8000`；切换路径会先 `quit_cursor_sync` 再写 |
| 同一 `composerId` 在两端都被更新，last-write-wins 丢失旧版本 | 符合用户预期："最近编辑胜出"；多账号不会同时编辑同一会话 |
| 共享层损坏污染所有 profile | 隔离边界（`is_auth_key`）保证登录态不被污染；最坏可删 `cursor-shared/` 重新 seed |
| 退出时不回流，导致下次启动拿不到本 profile 新会话 | `prepare_profile_shared` 中 `sync_profile_composer_to_shared` 兜底回流（启动时回流上次的） |

**回退方案**：
- 删除 `cursor-shared/User/globalStorage/state.vscdb*`，下次启动自动重新 seed
- 不影响各 profile 的登录态（独立 DB 文件）
- 如方案整体不可用，回滚 Step 3 改造（去掉 `prepare_profile_shared` 中新增的 3 个 composer 同步调用），保留 Step 2 的同步原语以备后续使用

## 7. 实施顺序

1. ✅ Step 1 调研（已完成）
2. ✅ Step 2 同步原语 + 组合函数（已完成）
3. ⏳ Step 3 改造 `prepare_profile_shared`（接入双向同步链路）
4. ⏳ Step 4 扩展 `sync_profile_state_to_shared` 覆盖 Composer
5. ⏳ Step 5 删除调试代码
6. ⏳ Step 6 编译 + 端到端验证

## 8. 关键复用点

- [`open_sqlite_ro`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1837-L1849)：只读打开 SQLite，避免锁定 Cursor 正在使用的数据库
- [`export_non_auth_keys`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1317) / [`import_non_auth_keys`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1347)：已有的非认证 key 读写函数
- [`merge_recent_workspaces_json`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1745-L1784)：JSON union 合并范本
- [`is_auth_key`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L1246-L1250)：认证 key 判断（保持不变，登录态隔离边界）
- [`GLOBAL_STORAGE_EXCLUDED`](file:///d:/ai_project/ai-workbench/src-tauri/src/cursor/mod.rs#L582-L590)：`state.vscdb*`、`storage.json` 仍按 profile 隔离

## 9. 不做的事

- 不引入进程退出监听（复杂度高，启动时兜底回流已足够）
- 不做增量同步（按 `lastUpdatedAt > marker`）：当前规模下全量同步足够快，过早优化
- 不合并 `composerHeaders` 和 `cursorDiskKV` 同步为单函数：保持单一职责，便于排障
- 不重构现有 ItemTable 同步逻辑：保持稳定，新逻辑并行存在
- 不引入 JSON 合并器：`composerHeaders` 按 `composerId` 主键 `INSERT OR REPLACE` 已天然实现 union
