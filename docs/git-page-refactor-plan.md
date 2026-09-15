# Git 管理页改造计划

> 状态：✅ 已实施（2025-09-10）
> 决策：预设不一致仅提示+一键应用（不自动写）/ 提交防呆软提示 / 账号管理独立弹窗 / 不保留旧 tab
> 结果：6 视图 → 2 视图（仓库/提交）+ 2 弹窗（账号管理/绑定身份）；后端零改动；`tsc`、`npm run build`、`cargo check` 全部通过

---

## 一、现状问题

### 1.1 导航层级过深（3 层 6 视图）

```
身份管理 → Git 管理
├── 工作台 (GitWorkbench)
├── 提交 (GitCommitPanel)
└── 身份 (GitIdentity)
    ├── 快速切换 (QuickSwitch)      ← 孙页 1
    ├── 账号管理 (AccountManager)    ← 孙页 2
    └── 仓库配置 (RepoConfig)        ← 孙页 3
```

完成一个完整工作流（选仓库 → 看状态 → 核对身份 → 提交）需要在 3 层导航间来回跳。

### 1.2 身份功能三处重叠

| 能力 | QuickSwitch | AccountManager | RepoConfig |
|------|:---:|:---:|:---:|
| 账号列表展示 | ✅（卡片，点击应用） | ✅（卡片，仅查看） | ❌ |
| 账号增删改 | ✅（自动建档，不能改/删） | ✅（完整 CRUD） | ❌ |
| 切全局身份 | ✅ | ❌ | ❌ |
| 切仓库身份 | ✅（手动选路径） | ❌ | ✅（手动点应用） |
| 仓库↔身份预设 | ❌ | ❌ | ✅（仅存储） |
| 手动输入身份 | ✅ | ❌ | ❌ |

**同一个动作"给仓库 X 应用身份 Y"有两条互不相通的路径**（QuickSwitch 手选路径 / RepoConfig 应用按钮），且都要用户主动想起来去哪一页操作。

### 1.3 预设身份与实际身份脱节（断链）

- `RepoConfig` 保存的 `git_repo_configs` 只是数据库记录，**不会写 git config**
- 切换到某仓库时，没有提醒"该仓库有预设身份未应用"
- 用错误身份提交后才发现，只能 reset 重做——这是整个页面最痛的点

### 1.4 提交页强依赖工作台

`GitCommitPanel` 只读 `settings.currentGitRepo`，没有仓库就无法工作；想换仓库必须跳回工作台点"设为当前"。

---

## 二、改造方案：2 个平级视图 + 复用弹窗

### 2.1 新结构

```
身份管理 → Git 管理
├── 仓库 (GitReposPage)   ← 合并 Workbench + RepoConfig
└── 提交 (GitCommitPanel)  ← 增强：内置仓库选择器 + 身份条
     弹窗（全局复用）：
     ├── 账号管理弹窗 (AccountManagerModal)  ← 原 AccountManager 改造
     └── 绑定身份弹窗 (RepoBindingModal)     ← 原 RepoConfig 表单
```

### 2.2 「仓库」页设计（GitReposPage）

保留 Workbench 卡片式仓库总览，每张卡片增加**身份区**：

```
┌────────────────────────────────────────────┐
│ ai-workbench   ● 当前    分支: main  ↑2 ↓0   │
│ 脏文件: 3                                   │
│ ─────────────────────────────────────────  │
│ 身份: 张三 <z@a.com>          [绑定] [管理]  │
│ ⚠ 与预设不一致（预设: 李四 <l@b.com>）[应用] │  ← 仅在不一致时显示
│ ─────────────────────────────────────────  │
│ [设为当前] [提交] [打开] [拉取] [推送] [移除] │
└────────────────────────────────────────────┘
```

要点：
- **身份行**：显示该仓库实际生效的 `user.name/email`（复用 `get_repo_git_config`）
- **绑定预设**：`[绑定]` 打开弹窗，从账号下拉选择或手动输入（替代原 RepoConfig 的添加表单）；保存到 `git_repo_configs`
- **不一致提醒**：实际身份 ≠ 预设身份时显示黄色警告条 + `[应用预设]` 一键按钮（`invokeSetRepoGitConfig`）
- 原 Workbench 的拉取/推送/设为当前/移除/打开全部保留

### 2.3 「提交」页增强（GitCommitPanel）

在现有文件列表 + diff + AI 生成消息之上，顶部新增两条：

**① 仓库选择器（下拉）**
- 直接列出 `recentProjects`，选择即 `setCurrentGitRepo`
- 不再强制跳工作台；无仓库时引导按钮保留

**② 身份快速条**
- 显示当前生效身份 + `[切换]` 按钮
- 点击展开账号选择器（点击账号 = 仓库级 `invokeSetRepoGitConfig`，即 QuickSwitch 的核心能力内联化）
- 附 `[管理账号]` 链接打开账号管理弹窗
- **提交前防呆**：`runCommit` 前比对实际身份与该仓库预设——不一致弹确认「用身份 X 提交？预设为 Y [改用预设提交]」——把断链彻底焊死

### 2.4 账号管理弹窗（AccountManagerModal）

- 原 `AccountManager` 整体从"页面"改为"Modal 组件"，CRUD 逻辑不动
- 入口收敛为两处：仓库页卡片 `[管理]`、提交页身份条 `[管理账号]`
- QuickSwitch 的"自动建档"能力保留在身份快速条里（应用未知身份时按 email 去重建档）

### 2.5 删除清单

| 组件 | 处置 |
|------|------|
| `GitIdentity.tsx` | 删除（三层导航取消） |
| `QuickSwitch.tsx` | 删除（能力拆分进提交页身份条 + 仓库页应用按钮） |
| `RepoConfig.tsx` | 列表页删除；表单改造为 `RepoBindingModal` |
| `AccountManager.tsx` | 改造为 `AccountManagerModal.tsx`（逻辑复用） |

### 2.6 数据与后端

- **Rust 后端零改动**：所需命令（`get_repo_git_config` / `set_repo_git_config` / `git_repo_summary` 等）全部已存在
- **DB 表不动**：`git_accounts` / `git_repo_configs` / `recent_projects` 结构与用途不变
- 一致性比对、防呆确认全部在前端完成

---

## 三、实施阶段

### Phase 1 — 导航收敛 + 仓库页合并（核心）
1. `GitManager` 三 tab 改两 tab（仓库 / 提交）
2. 新建 `GitReposPage`：Workbench 卡片 + 身份行 + 绑定按钮 + 不一致警告
3. 新建 `RepoBindingModal`（从 RepoConfig 表单抽出）
4. `AccountManagerModal` 改造
5. 删除 `GitIdentity` / `RepoConfig` 页面引用

### Phase 2 — 提交页增强
6. 仓库选择器下拉
7. 身份快速条 + 账号选择弹层 + 自动建档
8. 删除 `QuickSwitch`（能力已内联）

### Phase 3 — 防呆闭环
9. `runCommit` / `runStageAllAndCommit` 前身份一致性校验 + 确认弹窗
10. 仓库页"设为当前"时若存在未应用预设，顺手提示

### Phase 4 — 收尾
11. i18n key 增删（zh-CN / en-US 同步）
12. 旧组件文件清理、无引用翻译键清理
13. README / 交接文档侧边栏结构描述更新

---

## 四、开放决策点

| # | 决策 | 推荐 |
|---|------|------|
| 1 | 预设身份不一致时，自动应用还是仅提示？ | **仅提示 + 一键应用**（自动写 git config 意外感强，可能覆盖用户临时身份） |
| 2 | 提交前防呆是硬阻断（必须选择）还是软提示（可忽略）？ | **软提示**：弹确认但允许"仍用当前身份提交" |
| 3 | 账号管理保留独立弹窗，还是完全并入身份条？ | **独立弹窗**（低频 CRUD 不该挤占高频界面） |
| 4 | 旧的三 tab 是否保留兼容入口？ | **不保留**，一次切换干净利落（数据无损失，仅 UI 变化） |

---

## 五、验收标准

- [x] 任何高频操作 ≤ 2 次点击可达：换仓库、切身份、暂存+提交、推送
- [x] 仓库预设身份不一致时在仓库页与提交页都有可见提醒
- [x] 提交前若身份与预设冲突，必定出现确认（不会静默用错身份提交）— 多仓 changelist 的 `runCommit` 对每个待提交仓比对 preset，汇总弹窗（仍用当前 / 改用预设）
- [x] 账号 CRUD、仓库绑定、手动输入身份、AI 生成消息等既有能力全部保留
- [x] `git_accounts` / `git_repo_configs` / `recent_projects` 数据无损，无需迁移
- [x] TypeScript / Rust 编译零错误，无未使用组件与死翻译键

## 六、实施记录（2025-09-10）

### 变更文件

| 文件 | 变更 |
|------|------|
| `src/components/GitManager.tsx` | 三 tab → 两 tab（仓库 / 提交） |
| `src/components/GitReposPage.tsx` | **新建**：仓库卡片 + 身份行 + 绑定/解绑 + 不一致警告一键应用 + 账号管理入口 |
| `src/components/RepoBindingModal.tsx` | **新建**：绑定弹窗，从账号带入或手输，默认「保存并立即应用」写 git config |
| `src/components/AccountManagerModal.tsx` | **新建**：原 AccountManager 页面改造为弹窗，CRUD 逻辑不变 |
| `src/components/GitCommitPanel.tsx` | 身份快速条（chips 点击即切+自动建档）+ 不一致警告横幅 |
| `src/components/GitIdentity.tsx` `QuickSwitch.tsx` `RepoConfig.tsx` `AccountManager.tsx` `GitWorkbench.tsx` | **删除**（能力已合并） |
| `src/locales/{zh-CN,en-US}/git.json` | 新增 `repos.*` 与 commit 身份相关 key；删除 `identity` / `quickSwitch` / `repoConfig` 死段 |
| `src/styles.css` | 新增 `.repos-identity-row` `.repos-mismatch` `.commit-identity-quick/chip` `.repos-modal-*` 等 |
| `README.md` `ROADMAP.md` | 功能描述与组件清单同步 |

### 关键实现决策

1. **绑定即应用**：`RepoBindingModal` 保存预设时默认同时写仓库级 git config（勾选框可关），从源头消灭"存了但没生效"
2. **防呆是软提示**：`runCommit` 前比对 preset；不一致弹确认，提供「仍用当前身份提交」和「改用预设提交」两个出口，不硬阻断（多仓汇总一条）
3. **自动建档**：提交页身份条应用账号后按 email 去重，未建档的自动补建（沿用 QuickSwitch 行为）
4. **后端零改动**：所有数据流走既有 Rust 命令与 SQLite 表（后续演进已扩展命令，见第七节）

---

## 七、后续演进（2026-09，相对上文）

在「2 视图」基础上继续打磨，文档以 [README.md](../README.md) / [交接文档.md](../交接文档.md) 为准：

| 能力 | 说明 |
|------|------|
| 页保活 | `App.tsx` `gitMounted` + `.page-panel`，切侧栏不卸载 Git |
| 工作区 | 表 `git_workspaces`；仓库页分组折叠；提交页筛选全部/工作区/其它 |
| 扫描子仓 | `git_scan_repos` + `ScanReposModal` |
| 批量身份 | `BatchIdentityModal` |
| 变更列表 | `CommitChangelist`：多仓勾选、左右 diff 高亮、AI `generate_text` |
| 筛选漏检 | 当前 dirty 仓被 filter 挡住时自动扩筛选；空态区分筛选内外 |
| 后端扩展 | `git_status` / stage / commit / push / pull / diff / discard / commit_context 等（已非「后端零改动」） |

### 流程复核修复（2026-09-12）

对照多仓 changelist 现状补齐防呆与跨页一致性：

| 项 | 处理 |
|----|------|
| 提交前身份确认 | `CommitChangelist.runCommit` 恢复软提示（多仓汇总；改用预设先写 config） |
| Commit 页 Pull | 对 scoped 且 `behind>0 && hasUpstream` 的仓批量拉取；behind 提示条 |
| 推送前 behind | 提交并推送 / 仅推送时二次确认 |
| 批量 push/pull | 与单卡相同 ahead/behind/upstream 过滤 |
| 多仓同 message | 提交栏提示「将对 N 个仓使用同一说明」 |
| 部分提交 unstage | 确认；可选提交后重新暂存未勾选文件 |
| 自动切当前仓 | 去掉脏仓自动 `setCurrentGitRepo`；仅用户点击或首次未设置时写入 |
| porcelain 路径 | `unquote_porcelain_path` 处理引号与八进制转义 |
| i18n | 清理旧单仓 stage/unstage 死键；补齐新文案 |

历史决策（2 视图 + 弹窗）仍然有效；上表为增量能力索引。
