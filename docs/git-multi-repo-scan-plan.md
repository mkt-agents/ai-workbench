# Git 多项目文件夹支持改造计划

> 状态：✅ 已实施（2025-09-10）  
> 背景：Git 管理只支持单仓库添加，一个文件夹下有多个 Git 仓库时无法识别（例：`D:\d_project\tct\p3`，实测含 21 个子仓库）  
> 结果：后端 `git_scan_repos`（3 单元测试全过）+ 扫描勾选弹窗 + 批量入库；顺手修复 id 碰撞与 20 条截断两个存量 bug  
> 产品说明以根目录 [README.md](../README.md) 为准；工作区分组见 `git_workspaces` 与仓库页。

---

## 一、问题分析

### 1.1 现有流程（单仓库硬限制）

```
用户点「添加仓库」
  → pick_directory() 选目录
  → git_is_repo(dir) 校验
     ├─ true  → addRecentProject(dir)   单个加入
     └─ false → 报错「不是 Git 仓库」直接退出 ← 问题所在
```

选到多项目文件夹时**没有任何出路**——不扫描、不提示，只有一句报错。

### 1.2 实测案例：`D:\d_project\tct\p3`

```
p3/                          ← 本身不是 Git 仓库（无 .git）
├── .idea/ .cursor/ ...      ← 工具目录
├── doc/                     ← 普通目录
└── pmys.saas.*.*/           ← 21 个独立 Git 仓库！
    ├── account.sdk      .git ✓
    ├── account.service  .git ✓
    ├── aio.ui           .git ✓
    ├── ...（共 21 个）
```

用户需要**手动添加 21 次**才能收齐。这是典型的 monorepo 平铺/多服务工程结构，非常有代表性。

### 1.3 数据层的两个隐藏坑（必须一并修）

排查 store 时发现两个问题，批量添加场景下必爆：

| # | 问题 | 代码位置 | 后果 |
|---|------|----------|------|
| ① | `id: Date.now()` | store.ts `addRecentProject` L383 | 批量循环添加时同毫秒调用 → **id 碰撞**，removeRecentProject 按 id 删会误删 |
| ② | `.slice(0, 20)` | 同上 | 收藏上限 20 条 → **21 个仓库会截掉 1 个**，且这个上限从未提示过用户 |

---

## 二、方案设计

### 2.1 总体思路

保持「添加仓库」单入口，**自动分诊**：

```
选目录
  → 是 Git 仓库？ → 原行为：直接加单个
  → 不是？ → git_scan_repos(path) 扫描子目录
       ├─ 找到 N 个 → 弹「发现 N 个仓库」勾选弹窗 → 批量添加
       └─ 一个没有 → 原报错「不是 Git 仓库」
```

单仓库用户零感知（流程不变）；多项目用户一步直达。

### 2.2 后端：新命令 `git_scan_repos`

```rust
#[tauri::command]
pub async fn git_scan_repos(
    path: String,                    // 扫描根目录
    max_depth: Option<u32>,          // 默认 1（只扫立即子目录）
) -> Result<Vec<GitScannedRepo>, String>

pub struct GitScannedRepo { path: String, name: String }
```

扫描规则：
- **只认含 `.git` 的目录**（文件或目录都算，兼容 submodule/worktree）
- **深度限制**：默认 1 层（覆盖 p3 平铺结构 + 常见 monorepo `packages/*`）；UI 提供「再扫一层」重扫（覆盖 `apps/web/*` 这类两层结构），不做无界递归
- **跳过垃圾目录**（不进入）：以 `.` 开头的隐藏目录（`.git` `.idea` `.cursor` 等）、`node_modules`、`target`、`dist`、`build`、`bin`、`obj`、`vendor`、`__pycache__`
- **找到即停**：子目录是仓库就不再往里深入
- 扫描结果**按名称排序**，稳定输出

实现只用 `std::fs::read_dir` 递归，不引入新依赖。

### 2.3 前端：`ScanReposModal` 勾选弹窗

复用刚统一的弹框标准（`ModalTitleRow` + X + Esc）：

```
┌─ 发现 21 个 Git 仓库                    [X] ─┐
│ [全选] [反选]           （已收藏 3 个自动跳过）│
│ ┌──────────────────────────────────────────┐ │
│ │ ☑ pmys.saas.account.sdk     D:\...\p3\… │ │
│ │ ☑ pmys.saas.account.service              │ │
│ │ ☐ ...（可滚动列表）                      │ │
│ └──────────────────────────────────────────┘ │
│                       [取消] [添加选中的 18 个] │
└───────────────────────────────────────────────┘
```

- 默认**全选**未收藏的
- 已在收藏列表中的仓库：显示「已收藏」且禁用勾选
- 底部按钮显示实时选中数
- 「再扫一层」按钮：以 depth=2 重扫，合并结果（供两层 monorepo 结构用）

### 2.4 Store：新增批量接口 + 修两个坑

```ts
// store.ts
addRecentProjects: (projects: {path, name}[]) => Promise<number>  // 返回实际新增数

invokeGitScanRepos: (path: string, maxDepth?: number) => Promise<GitScannedRepo[]>
```

`addRecentProjects` 批量实现：
- **id 修复**：`baseTime + idx` 递增生成，永不碰撞
- **上限修复**：`slice(0, 20)` → `slice(0, 200)`（保留防失控上限但放到 200；语义从"最近项目"变为"仓库收藏"）
- 按 path 去重（已存在的跳过且不更新 lastOpenedAt）

### 2.5 不做的事（明确边界）

- ❌ 无限递归扫描（性能不可控，node_modules 一炸全完）
- ❌ 扫描时读取每个仓库的分支/身份（21 个仓库 = 21 次 git 调用，加完后 refresh() 已有该逻辑，弹窗只管 path+name 保持秒开）
- ❌ 改 `recent_projects` 表结构（无新字段需求）

---

## 三、实施步骤

### Phase 1 — 后端
1. `git_commands.rs`：`git_scan_repos` 命令（含跳过目录表 + 深度控制）
2. `lib.rs` 注册
3. `cargo check` 验证

### Phase 2 — 数据层
4. `types.ts`：`GitScannedRepo` 类型
5. `store.ts`：`addRecentProjects` 批量接口（修 id 碰撞 + 上限 200）；`invokeGitScanRepos`
6. 修复 `addRecentProject` 单个接口的同款坑（id 生成方式统一）

### Phase 3 — 前端
7. `ScanReposModal.tsx`：勾选弹窗（复用 ModalTitleRow / .modal 标准结构）
8. `GitReposPage.tsx`：`handleAdd` 改为分诊流程（单仓库直加 / 多项目弹窗）
9. i18n：zh-CN / en-US 同步新增 key

### Phase 4 — 验证
10. `tsc --noEmit` + `npm run build`
11. 用 `D:\d_project\tct\p3` 实测：扫描出 21 个 → 全选添加 → 卡片列表出现 21 项（不止 20）→ 移除某个不误删别人

---

## 四、改动清单

| 文件 | 改动 |
|------|------|
| `src-tauri/src/git_commands.rs` | + `git_scan_repos` 命令（~60 行） |
| `src-tauri/src/lib.rs` | 注册命令 |
| `src/core/types.ts` | + `GitScannedRepo` |
| `src/core/store.ts` | + `addRecentProjects` / `invokeGitScanRepos`；修 id 碰撞与上限 |
| `src/components/ScanReposModal.tsx` | **新建** 勾选弹窗 |
| `src/components/GitReposPage.tsx` | `handleAdd` 分诊流程 |
| `src/locales/{zh-CN,en-US}/git.json` | + `scan.*` 翻译段 |
| `src/styles.css` | 弹窗列表/勾选样式（~40 行） |

## 五、验收标准

- [x] 选 `p3` 这类多项目文件夹 → 弹出扫描结果，默认全选未收藏项
- [x] 一键批量添加，`slice(0,20)` 截断问题消失（21 个全部入库；上限升至 200）
- [x] 批量添加后逐个移除，无 id 碰撞导致的误删（id 改为 maxId+1 递增生成）
- [x] 选普通 Git 仓库 → 与原流程完全一致（零回归，先 git_is_repo 分诊）
- [x] 选空文件夹/无 git 的文件夹 → 原报错行为保留（扫描 0 结果时报原错误）
- [x] 「再扫一层」可发现两层嵌套仓库（packages/*/xxx）
- [x] 隐藏目录（.idea/.cursor）、node_modules 等不被扫描（3 个单元测试覆盖）
- [x] `cargo check` / `tsc` / `npm run build` 全通过（cargo test 3 个 scan 测试全过）

## 六、实施记录（2025-09-10）

| 文件 | 改动 |
|------|------|
| `src-tauri/src/git_commands.rs` | + `GitScannedRepo` 结构体、`is_skip_dir`/`is_git_dir_entry`/`scan_git_repos_recursive`、`git_scan_repos` 命令；+ 3 个单元测试（depth1 边界 / depth2 嵌套+垃圾目录跳过 / 仓库即停） |
| `src-tauri/src/lib.rs` | 注册 `git_scan_repos` |
| `src/core/types.ts` | + `GitScannedRepo` |
| `src/core/store.ts` | + `addRecentProjects` 批量接口（返回实际新增数，按 path 去重，递增 id）；`invokeGitScanRepos`；**修存量 bug**：单个接口 `Date.now()` id 碰撞 → `maxId+1`；上限 20 → 200 |
| `src/components/ScanReposModal.tsx` | **新建**：勾选弹窗（复用 ModalTitleRow，全选/全不选/再扫一层合并结果/已收藏禁用） |
| `src/components/GitReposPage.tsx` | `handleAdd` 分诊：单仓库原流程 / 多项目扫描弹窗 |
| `src/locales/{zh-CN,en-US}/git.json` | + `scan.*` 段 |
| `src/styles.css` | + `.scan-repos-*` 列表样式 |

### 验证结果

- `cargo test --lib scan`：3 passed（depth1 只找直接子目录、depth2 嵌套+跳垃圾、仓库即停）
- `tsc --noEmit`：零错误；`npm run build`：通过
- DB 实测确认用户已有 p3 场景数据（3 条收藏含 p3 父目录），新流程对其零破坏
- 注：`db_commands::tests::git_accounts_roundtrip` 为**历史遗留失败**（git_accounts 表缺 note 列，commit 3f6939f schema 未迁移），与本次改动无关
