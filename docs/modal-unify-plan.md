# Git 弹框布局统一改造计划

> 状态：✅ 已实施（2025-09-10）
> 范围：Git 管理页 3 个弹框（账号管理 / 账号表单 / 绑定身份）+ 全项目弹框标准统一
> 结果：新增共享组件 `ModalTitleRow`（标题行+X+Esc），7 个弹框全部接入；账号管理弹窗消灭嵌套遮罩改为同层视图切换；`tsc`/`npm run build` 通过

---

## 一、现状调研

### 1.1 项目现有弹框模式（5 个成熟弹框）

| 弹框 | 组件 | 结构 |
|------|------|------|
| Hosts 配置 | HostsManager | `.modal-title` 纯文本 → 表单 → `.modal-actions` 右对齐「取消+主按钮」 |
| 插件编辑 | PluginBrowser | 同上（头部带图标） |
| 模型配置 | AIAssistant | 同上（`.models-modal` 加宽） |
| 命名隧道 | CloudflaredManager | 同上 |
| Cursor 账号 | CursorManager | 同上 |

**共同点**（即项目事实标准）：
- 标题行：`.modal-title` 纯文本，带底部分隔线
- 底部：`.modal-actions` 右对齐，`取消` 在左、`主按钮` 在右
- 关闭方式：点「取消」或点遮罩，**均无 X 按钮**

### 1.2 Git 页新弹框的偏差

| 偏差 | 位置 | 问题 |
|------|------|------|
| ① X 按钮样式自创 | AccountManagerModal | 用了自创的 `.repos-modal-close`，且 X 按钮的 title 写的是"取消"，语义混乱 |
| ② 账号管理弹窗内容过载 | AccountManagerModal | 「管理」与「增删改」混在一层：账号卡片 + 底部添加按钮 + 内嵌表单弹窗三块挤在一起，还出现"弹窗套弹窗" |
| ③ 嵌套遮罩 | AccountManagerModal | 表单弹窗叠在管理弹窗之上，两层 `.modal-overlay` 叠加导致遮罩变黑加重、视觉杂乱 |
| ④ 标题区不一致 | AccountManagerModal | 标题行塞了标题+数量徽章+X 三样东西，与其他弹框只有纯文本不同 |

---

## 二、统一标准（结合用户要求）

以项目现有 `.modal` 惯例为基础，补上用户要求的 X 按钮，形成新标准：

```
┌──────────────────────────────────┐
│ 标题文本                    [X]  │  ← 标题行：flex，标题在左，X 在右，底部分隔线
├──────────────────────────────────┤
│  内容区（表单 / 列表）            │
├──────────────────────────────────┤
│              [取消] [主按钮]      │  ← .modal-actions 右对齐（不变）
└──────────────────────────────────┘
```

规则：
1. **所有弹框右上角加 X**（关闭 = 点 X / 点遮罩 / 按 Esc / 点取消，多通道等价）
2. X 用统一类名 `.modal-close`（全局加一次 CSS，所有弹框复用）
3. 表单弹框底部保留「取消+主按钮」右对齐——X 与取消并存不冲突：X 是视觉习惯，取消是明确语义
4. **消灭弹窗套弹窗**：账号管理弹窗里的内嵌表单改为「同层切换」（点击"添加/编辑"→ 管理列表视图切换为表单视图，表单底部加"返回列表"）
5. 管理弹窗（有列表无表单提交）只放 X + 列表 + 添加入口，不出现 `.modal-actions`

## 三、实施步骤

### Step 1 — 全局 CSS（styles.css）
- 新增 `.modal-title-row`（flex 布局容器：标题文本 + X）
- 新增 `.modal-close`（X 按钮：无边框、hover 变色、与标题垂直居中）
- 删除自创的 `.repos-modal-close`（迁到统一类）

### Step 2 — 全项目既有弹框加 X（5 处）
HostsManager / PluginBrowser / AIAssistant / CloudflaredManager / CursorManager：
- 把 `<div className="modal-title">文本</div>` 包进 `.modal-title-row`，右侧加 X 按钮
- X 的 onClick 与原遮罩点击/取消逻辑共用同一函数（如 `closeModal` / `setShowModal(false)`）
- busy 状态下 X 一并禁用（与遮罩行为一致，CursorManager 已有此模式）

### Step 3 — Git 页弹框重构
- **AccountManagerModal**：
  - 标题行改 `.modal-title-row` + X（title 用 `关闭` 而非 `取消`）
  - 移除"弹窗套弹窗"：内层表单改为同层视图切换（`view: "list" | "form"`），表单底部「取消」变为「返回列表」
  - 列表视图：账号卡片 + 「+ 添加账号」按钮；表单视图：表单 + 「返回 + 保存」
- **RepoBindingModal**：
  - 标题行加 X（与上统一）
  - 其余不动（布局已符合标准）

### Step 4 — i18n
- X 按钮 title 用已有 `common.actions.close`（"关闭"/"Close"），无需新增 key

### Step 5 — 验证
- `tsc --noEmit` + `npm run build`
- 启动应用逐个检查：Git 两个弹框 + 另 5 个弹框的 X 出现、关闭行为、遮罩点击、按钮右对齐

---

## 四、改动清单

| 文件 | 改动 |
|------|------|
| `src/styles.css` | + `.modal-title-row` / `.modal-close`；删 `.repos-modal-close` |
| `src/components/HostsManager.tsx` | 标题行 + X |
| `src/components/PluginBrowser.tsx` | 标题行 + X |
| `src/components/AIAssistant.tsx` | 标题行 + X |
| `src/components/CloudflaredManager.tsx` | 标题行 + X |
| `src/components/CursorManager.tsx` | 标题行 + X（busy 禁用） |
| `src/components/AccountManagerModal.tsx` | 标题行 + X；消灭嵌套弹窗（列表/表单同层切换） |
| `src/components/RepoBindingModal.tsx` | 标题行 + X |

## 五、验收标准

- [x] 全项目 7 个弹框右上角均有 X，点击关闭且与「取消/遮罩」行为一致
- [x] 表单弹框底部按钮仍右对齐「取消+主按钮」
- [x] AccountManagerModal 无嵌套遮罩，列表↔表单为同层切换
- [x] 所有 X 的 hover 反馈一致，busy 时禁用（CursorManager busy 场景已处理）
- [x] Esc 键可关闭弹框（ModalTitleRow 统一挂载；ConfirmModal Esc=取消；多层遮罩只有最上层响应）
- [x] `tsc` / `npm run build` 通过

## 六、实施记录（2025-09-10）

### 实际改动

| 文件 | 改动 |
|------|------|
| `src/components/ModalTitleRow.tsx` | **新建**：统一标题行（标题+徽章+X+Esc）；多层遮罩叠加时仅最上层响应 Esc |
| `src/styles.css` | + `.modal-title-row` `.modal-title-text` `.modal-close`；删 `.repos-modal-close` |
| `src/components/ConfirmModal.tsx` | + Esc=取消（确认框语义） |
| `src/components/HostsManager.tsx` `CloudflaredManager.tsx` `AIAssistant.tsx` | `.modal-title` → `ModalTitleRow` |
| `src/components/CursorManager.tsx` | 同上，busy 时禁用 X 与 Esc |
| `src/components/PluginBrowser.tsx` | 自定义头部内加 X（复用 `.modal-close`，已有 Esc 逻辑保留） |
| `src/components/AccountManagerModal.tsx` | 标题行 X 统一；**弹窗套弹窗 → 同层视图切换**（`view: list/form`，表单底部「返回列表」） |
| `src/components/RepoBindingModal.tsx` | 标题行 → `ModalTitleRow` |
| `src/locales/{zh-CN,en-US}/git.json` | + `accountManager.backToList`（唯一新增 key；X 的 title 用已有 `common.actions.close`） |

### 关键设计

1. **Esc 分层响应**：`ModalTitleRow` 通过 `document.querySelectorAll(".modal-overlay")` 判断自己是否最上层，避免删除确认框叠在管理弹窗上时按 Esc 误关底层
2. **X 与取消并存**：X 是视觉习惯（右上角），取消是表单语义（右下），两者等价触发同一关闭函数
3. **确认框优先**：ConfirmModal 的 Esc 挂载在 provider 层，永远跟随 state 存在，天然最上层
