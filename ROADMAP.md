# AI Workbench 路线图

> 产品定位（已确认）：**AI 驱动的开发工具箱**。以 **DeepSeek 本地服务** 为核心 AI 引擎，Git/Cursor 身份管理为基础能力，AI 模型配置与系统工具（Hosts / Cloudflare 等）为辅助。

---

## 产品愿景

一个本地优先的桌面 AI 助手，通过集成 DeepSeek 本地服务执行 AI 任务，支持多模型配置与提示词优化，并管理开发者的 Git/Cursor 多身份与本机隧道/Hosts 等上下文。所有数据留在本机。

---

## 功能分层

| 层级 | 功能 | 当前状态 | 处置 |
|---|---|---|---|
| **核心** | DeepSeek 本地服务（安装/启动/停止/iframe，侧栏保活） | ✅ 已实现 | 维护 |
| **核心** | 多模型配置 + 连接测试 | ✅ 已实现 | 维护 |
| **核心** | 提示词优化 | ✅ 已实现 | 维护 |
| **核心** | Cursor 管理（备份/恢复/切换） | ✅ 已实现 | 维护 |
| **基础** | Git 管理（工作区+多仓提交+身份快速条） | ✅ 已实现（持续打磨中） | 维护 |
| **基础** | 运行时切换 | ✅ 已实现 | 维护 |
| **辅助** | Hosts 管理 | ✅ 已实现 | 保留，低优先 |
| **辅助** | Cloudflare Tunnel（多隧道 / Token / config.yml） | ✅ 已实现 | 维护 |
| **辅助** | 插件浏览器 | ✅ 已实现 | 保留，低优先 |
| **待清理** | 遗留组件 / 死代码 / 未使用翻译 | ✅ 已处理 | 维护 |

### 明确不做

- ❌ 内置多模型流式对话（通过 DeepSeek 外部集成提供 AI 能力）
- ❌ AI 工具执行循环（shell/文件/Git/包管理）
- ❌ 多平台支持（当前仅 Windows）
- ❌ 插件浏览器深度开发

---

## 阶段规划

### 阶段 1：文档对齐（P0）✅ 已完成

- [x] 重写 `README.md`：同步真实架构、功能列表、项目结构
- [x] 重写 `交接文档.md`：同步真实架构、完整 Rust 命令清单
- [x] 同步 `MISSION.md` 中的约束描述
- [x] 同步 `ROADMAP.md` 中的功能分层

### 阶段 2：死代码清理（P1）✅ 已完成

- [x] 移除 `AIModelConfigModal.tsx`（已合并到 AIAssistant）
- [x] 移除 `MarkdownContent.tsx`（未被任何组件引用）
- [x] 清理未使用的翻译命名空间键
- [x] 清理 `navigation.json` 中未使用的键
- [x] 移除未使用的依赖（`react-markdown`、`remark-gfm`）
- [x] 移除未使用的 Rust crate（`hex`、`futures`、reqwest `stream`）

### 阶段 3：体验优化（P1）

- [x] 窗口缩放性能优化
- [x] 侧边栏导航结构优化
- [x] 默认落地 DeepSeek 页；System 组默认展开；取消 tab 强制 remount
- [x] Git 最近项目写入闭环
- [x] i18n 一致性：侧栏 + Git / 模型 / Cursor / DeepSeek 主流程
- [x] 设置页检查更新（打开 GitHub）
- [x] 开机自启已实现

### 阶段 4：安全与架构（P1–P2）✅ 已完成

- [x] SQLite IPC 改为 typed `db_load` / `db_save` 白名单
- [x] Cursor `account_id` 路径加固
- [x] 收敛未使用的 Cursor 对外 invoke 命令
- [x] DSH 认证补丁失败时 UI 提示

### 阶段 5：发布就绪（P2）✅ 已完成

- [x] 基础测试：git 邮箱校验、plugin 路径沙箱、cursor account_id、db 仓储
- [x] 构建产物清理：`dist/`、`target/` 已在 `.gitignore`
- [x] 版本号规划与安装包流程（README）
- [x] 设置页真实版本 + Releases 检查更新

### 阶段 6：系统工具增强（P1）✅ 已完成

- [x] Cloudflare：安装/选 exe、临时隧道、命名隧道（Token）
- [x] Cloudflare：config.yml 认证、多隧道并行、按隧道日志
- [x] Cloudflare：双栏布局与样式精修
- [x] 文档同步：README / 交接文档 / `docs/cloudflared.md`

### 阶段 7：Git / DeepSeek 体验（P1）✅ 已完成（2026-09）

- [x] Git 页保活 + 摘要 TTL / 并发限制
- [x] 工作区（`git_workspaces`）分组、扫描子仓、批量绑定身份
- [x] 提交页 `CommitChangelist`：多仓变更树、左右 diff 高亮、AI 生成说明
- [x] 工作区筛选联动当前仓；筛选漏检空态区分 + 自动扩筛选
- [x] DeepSeek 运行态顶栏贴合；侧栏保活避免 iframe 重刷；检测中文案去重
- [x] 提示词页下拉 Portal，避免小窗裁切
- [x] 文档同步：README / 交接文档 / ROADMAP / `docs/git-page-refactor-plan.md` 演进说明

### 阶段 8：AI 随手可及（P0）✅ 已完成

详见 [`docs/workbench-upgrade-plan.md`](docs/workbench-upgrade-plan.md)。

- [x] M1：系统托盘 + 关窗进托盘 + `Ctrl+Alt+K` 快问窗 + 上下文芯片
- [x] M2：托盘运行态（DSH / 隧道）+ JSON 导出/导入
- [x] M3：精简片段库 + 快问插入

---

### 阶段 9：Cursor 稳定性与工作区共享（P0–P1）✅ 已完成（2026-09）

- [x] 修复黑屏：共享状态同步把 `ItemTable` 值统一转 BLOB，导致 Cursor `JSON.parse` 失败（`GlassWorkbench startup failed`）
  - 同步改为保留原生 SQLite 类型；新增 `repair_blob_typed_state()` 把「文本型 BLOB」还原为 TEXT（启动迁移 + 启动前兜底）
- [x] 共享 Cursor 机器级数据：`globalStorage` 内的 `anysphere.*`、`conversation-search.db`、扩展存储跨账号共享（`state.vscdb` 仍按账号独立）
- [x] 启动加固：直接 `Command::spawn`（去掉 `cmd /c start`，修复空格路径与窗口站隐患）
- [x] 删除安全：`remove_tree_removing_links` / `remove_path_for_link` 只摘链不跟随，避免误删默认 Cursor 数据
- [x] 性能：运行态探针 TTL 缓存（WMI 2.5s / tasklist 500ms）、退出整定 2.5s→800ms、前端聚焦仅在登录标记变化时写库
- [x] 可维护性：无主 profile 自动清理、`read_cursor_diagnostics` 诊断导出、账号搜索/排序、Cursor 页 i18n 收口

**成功标准**：切号不再黑屏；AI 工作区/会话在账号间可见；切换与聚焦无明显卡顿。

---

### 阶段 11：功能页完善（P1–P2）✅ 已完成（2026-09）

- [x] **模型配置**：批量删除（含默认模型警示）、Esc 关闭弹窗、名称重复校验、Max Tokens 校验、分组/折叠视图持久化、测试结果带时间戳、并排分组卡片高度对齐
- [x] **片段库**：JSON 导出/导入（指纹去重）、克隆、名称重复校验与保存禁用态、使用弹窗实时预览替换结果、Esc 关闭、空态区分
- [x] **Hosts**：写入系统前二次确认、编辑器脏状态标记 + 启用/注释行统计、另存为配置、`backup_hosts_now` 手动备份、配置名称重复校验
- [x] **内网穿透**：`open_in_browser` 公网 URL 浏览器打开（运行列表/绑定卡/临时隧道三处）、日志关键字过滤 + 自动滚动开关 + 复制/导出、绑定域名重复校验、删除运行中绑定警示
- [x] **快问**：多轮追问（`generate_text_stream` 新增可选 `history`，最近 6 轮上下文）、重新生成、新对话、复制反馈、Esc 弹窗优先、参数弹窗 Enter 提交
- [x] **DeepSeek**：运行时浏览器打开（复用 `open_in_browser`）、自定义启动端口（localStorage 持久化）
- [x] **Git 仓库页**：仓库搜索过滤、批量移除所选（仅移出列表）、分组折叠持久化

**新增 Rust 命令**：`backup_hosts_now`、`open_in_browser`；`GenerateTextStreamRequest.history`。

---

## 成功标准

- 新开发者读 `README.md` 即可在 5 分钟内跑起来并理解真实架构
- 无死代码、无未使用依赖、无编译警告
- 文档与代码完全一致

### 阶段 10：备选规划（📋 待定）

> 详细方案见 [docs/workbench-upgrade-plan.md](docs/workbench-upgrade-plan.md)

- [x] **Phase 1（P0）** — 系统托盘常驻 + 全局快捷键 `Ctrl+Alt+K` + 迷你快问窗（已随阶段 8 完成）
- [x] **Phase 2（P1）** — 片段库（`{{param}}` 模板）已随阶段 8 完成；剪贴板历史未做
- [ ] **Phase 3（P1）** — 纯前端小工具集（JSON / 时间戳 / Base64 / URL / UUID / JWT / 正则 / Hash）+ 端口/进程查看与 kill
- [x] **Phase 4（P2）** — 托盘聚合 dsh/隧道运行态 + 数据导出/导入（已随阶段 8 完成）
- [ ] **Phase 5（P2，可选）** — 轻量 HTTP 客户端（复用 `reqwest`）

**成功标准**：任意界面 ≤1s 唤起 AI 快问窗；剪贴板日采集无感运行；后台服务状态托盘一目了然；数据可导出迁移。
