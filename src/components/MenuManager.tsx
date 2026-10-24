/**
 * 系统工具 → 菜单管理：侧栏分组、排序、改名。
 *
 * - 纯逻辑在 `lib/navCustomize.ts`，权威结构在 `lib/navStructure.tsx`；
 * - 自定义只覆盖显示层，全部落在 settings（`navCustomization`），改完侧栏即时生效；
 * - 「恢复默认」清空全部自定义，不动任何功能页本身。
 */
import { useEffect, useRef, useState } from "react";
import type { DragEvent as ReactDragEvent, KeyboardEvent as ReactKeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { GripVertical, Pencil, Plus, RotateCcw, Trash2, X } from "lucide-react";
import { useGlobalStore } from "../core/store";
import type { NavCustomization } from "../core/types";
import { NAV_NODES } from "../lib/navStructure";
import type { EffectiveNavGroup } from "../lib/navCustomize";
import {
  GROUP_RENAME_PREFIX,
  applyNavCustomization,
  normalizeNavCustomization,
  withGroupDroppedOn,
  withGroupRemoved,
  withItemDroppedOn,
  withItemGroupChanged,
  withNewGroup,
  withRename,
} from "../lib/navCustomize";
import { useConfirm } from "./ConfirmModal";

/** 默认分组名 / 导航项名的 i18n key：判断「改名成默认文案」时用来回退覆盖项。 */
const DEFAULT_GROUP_LABEL_KEYS = new Map<string, string>();
const DEFAULT_ITEM_LABEL_KEYS = new Map<string, string>();
for (const node of NAV_NODES) {
  if ("group" in node) {
    DEFAULT_GROUP_LABEL_KEYS.set(node.group, node.labelKey);
    for (const child of node.children) DEFAULT_ITEM_LABEL_KEYS.set(child.id, child.labelKey);
  }
}

/** 正在被拖的东西。 */
type DragPayload = { kind: "item"; id: string } | { kind: "group"; id: string };

/** 落点：统一用「插到谁前面」表达，`null` 表示组末 / 列表末尾。 */
type DropHint =
  | { kind: "item"; groupId: string; beforeId: string | null }
  | { kind: "group"; beforeGroupId: string | null }
  | null;

const sameHint = (a: DropHint, b: DropHint): boolean => {
  if (a === b) return true;
  if (!a || !b || a.kind !== b.kind) return false;
  if (a.kind === "item" && b.kind === "item") {
    return a.groupId === b.groupId && a.beforeId === b.beforeId;
  }
  if (a.kind === "group" && b.kind === "group") return a.beforeGroupId === b.beforeGroupId;
  return false;
};

export default function MenuManager() {
  const { t } = useTranslation("navigation");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const confirm = useConfirm();

  const custom = normalizeNavCustomization(settings.navCustomization);
  const groups = applyNavCustomization(NAV_NODES, custom);

  const commit = (next: NavCustomization) => setSettings({ navCustomization: next });

  const groupLabel = (g: EffectiveNavGroup) =>
    custom.renames[`${GROUP_RENAME_PREFIX}${g.id}`] ||
    (g.labelKey ? t(g.labelKey) : t("menuTool.defaultName"));
  const itemLabel = (id: string, labelKey: string) => custom.renames[id] || t(labelKey);

  /** 改名 key（`group:<id>` 或页签 id）对应的默认文案；自定义分组回落到「新分组」。 */
  const defaultLabelOf = (key: string): string | null => {
    if (key.startsWith(GROUP_RENAME_PREFIX)) {
      const gid = key.slice(GROUP_RENAME_PREFIX.length);
      if (custom.customGroups.includes(gid)) return t("menuTool.defaultName");
      const labelKey = DEFAULT_GROUP_LABEL_KEYS.get(gid);
      return labelKey ? t(labelKey) : null;
    }
    const labelKey = DEFAULT_ITEM_LABEL_KEYS.get(key);
    return labelKey ? t(labelKey) : null;
  };

  // 行内改名：editingKey 就是 renames 的键——导航项用页签 id、分组用 `group:<gid>`，
  // Enter 提交 / Esc 取消。
  const [editingKey, setEditingKey] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const editInputRef = useRef<HTMLInputElement>(null);
  /** Esc 之后输入框会被卸载，某些环境仍可能补发 blur；用它挡住误提交。 */
  const cancelEditRef = useRef(false);
  useEffect(() => {
    if (editingKey) editInputRef.current?.select();
  }, [editingKey]);
  const startEdit = (key: string, current: string) => {
    cancelEditRef.current = false;
    setEditingKey(key);
    setDraft(current);
  };
  const commitEdit = () => {
    if (cancelEditRef.current) {
      cancelEditRef.current = false;
      return;
    }
    const key = editingKey;
    if (!key) return;
    const fallback = defaultLabelOf(key);
    // 改成和默认文案一样等于没改：清掉覆盖项，切语言 / 升级后继续跟随默认文案。
    const nextName = fallback !== null && draft.trim() === fallback ? "" : draft;
    commit(withRename(custom, key, nextName));
    setEditingKey(null);
  };
  const cancelEdit = () => {
    cancelEditRef.current = true;
    setEditingKey(null);
  };
  const onRenameKeyDown = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    if (e.key === "Enter") commitEdit();
    if (e.key === "Escape") cancelEdit();
  };

  const resetAll = async () => {
    const ok = await confirm({
      title: t("menuTool.resetTitle"),
      message: t("menuTool.resetMessage"),
      icon: "danger",
    });
    if (ok) setSettings({ navCustomization: undefined });
  };

  // 新建分组后把该组滚进视野（只做一次）。
  const rootRef = useRef<HTMLDivElement>(null);
  const [scrollTarget, setScrollTarget] = useState<string | null>(null);
  useEffect(() => {
    if (!scrollTarget) return;
    const el = rootRef.current?.querySelector<HTMLElement>(`[data-mm-group="${scrollTarget}"]`);
    el?.scrollIntoView({ block: "nearest" });
    setScrollTarget(null);
  }, [scrollTarget]);

  const addGroup = () => {
    const { next, id } = withNewGroup(custom, t("menuTool.defaultName"));
    commit(next);
    // 新组落在列表末尾，列表比窗口长时滚过去，否则看起来像「点了没反应」。
    setScrollTarget(id);
  };

  /** 删除自定义分组：组内菜单会回到各自的默认分组，先让用户确认。 */
  const removeGroup = async (group: EffectiveNavGroup) => {
    const ok = await confirm({
      title: t("menuTool.deleteTitle"),
      message: t("menuTool.deleteMessage", {
        name: groupLabel(group),
        items: group.children.length,
      }),
      warning: group.children.length > 0 ? t("menuTool.deleteWarning") : undefined,
      confirmText: t("menuTool.deleteConfirm"),
      icon: "danger",
    });
    if (ok) commit(withGroupRemoved(custom, group.id));
  };

  // ── 拖拽排序 ──
  // 拖分组头改组序；拖菜单行在组内重排或换组。落点统一表达成「插到某个兄弟之前」，
  // 与 withGroupDroppedOn / withItemDroppedOn 的参数一一对应，不用另算索引。
  const [drag, setDrag] = useState<DragPayload | null>(null);
  const [dropHint, setDropHint] = useState<DropHint>(null);
  /** 落点同时记一份 ref：drop 必须读它——state 里 null 既可能是「还没算」，
   *  也可能是「这里落下等于不动」，用 state 会误判成「落到末尾」。 */
  const dropHintRef = useRef<DropHint>(null);

  const endDrag = () => {
    dropHintRef.current = null;
    setDrag(null);
    setDropHint(null);
  };
  // dragover 每帧都来，落点没变就不要重渲染
  const hintOrKeep = (next: DropHint) => {
    dropHintRef.current = next;
    setDropHint((prev) => (sameHint(prev, next) ? prev : next));
  };

  /** 从按钮 / 输入框 / 下拉上起手时不算拖拽，否则点一下就可能把整行拖走。 */
  const dragStartedOnControl = (event: ReactDragEvent) =>
    Boolean((event.target as HTMLElement).closest("button, input, select, textarea, a"));

  const startItemDrag = (event: ReactDragEvent, itemId: string) => {
    if (dragStartedOnControl(event)) {
      event.preventDefault();
      return;
    }
    event.stopPropagation();
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", itemId);
    setDrag({ kind: "item", id: itemId });
  };

  const startGroupDrag = (event: ReactDragEvent, groupId: string) => {
    if (dragStartedOnControl(event)) {
      event.preventDefault();
      return;
    }
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", groupId);
    setDrag({ kind: "group", id: groupId });
  };

  /** 行内 dragover：指针在行的上半区就插到本行前，下半区插到下一行前（末行 = 组末）。 */
  const onItemDragOver = (
    event: ReactDragEvent<HTMLDivElement>,
    groupId: string,
    itemId: string,
    index: number,
    siblings: { id: string }[],
  ) => {
    if (drag?.kind !== "item") return; // 拖分组时落点由卡片自己算
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "move";
    // 落在自己身上、或自己正下方那道缝，都是原地不动：不画落点线
    if (itemId === drag.id) {
      hintOrKeep(null);
      return;
    }
    const rect = event.currentTarget.getBoundingClientRect();
    const belowHalf = event.clientY > rect.top + rect.height / 2;
    const beforeId = belowHalf ? siblings[index + 1]?.id ?? null : itemId;
    hintOrKeep(beforeId === drag.id ? null : { kind: "item", groupId, beforeId });
  };

  const onItemDrop = (event: ReactDragEvent<HTMLDivElement>, groupId: string) => {
    if (drag?.kind !== "item") return;
    event.preventDefault();
    event.stopPropagation();
    const hint = dropHintRef.current;
    if (hint?.kind === "item" && hint.groupId === groupId) {
      commit(withItemDroppedOn(custom, NAV_NODES, drag.id, groupId, hint.beforeId));
    }
    endDrag();
  };

  /** 卡片级 dragover：拖菜单进来 = 追加组末；拖分组 = 插到本卡之前 / 之后。 */
  const onGroupDragOver = (event: ReactDragEvent<HTMLElement>, groupId: string, index: number) => {
    if (!drag) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    if (drag.kind === "item") {
      hintOrKeep({ kind: "item", groupId, beforeId: null });
      return;
    }
    const rect = event.currentTarget.getBoundingClientRect();
    const belowHalf = event.clientY > rect.top + rect.height / 2;
    const beforeGroupId = belowHalf ? groups[index + 1]?.id ?? null : groupId;
    // 拖到自己头上、或自己正下方那道缝，都是原地不动
    const selfIndex = groups.findIndex((g) => g.id === drag.id);
    const nextOfSelf = selfIndex >= 0 ? groups[selfIndex + 1]?.id ?? null : null;
    hintOrKeep(
      beforeGroupId === drag.id || beforeGroupId === nextOfSelf
        ? null
        : { kind: "group", beforeGroupId },
    );
  };

  const onGroupDrop = (event: ReactDragEvent<HTMLElement>, group: EffectiveNavGroup) => {
    if (!drag) return;
    event.preventDefault();
    const hint = dropHintRef.current;
    if (drag.kind === "group") {
      if (hint?.kind === "group") {
        commit(withGroupDroppedOn(custom, NAV_NODES, drag.id, hint.beforeGroupId));
      }
    } else if (hint?.kind === "item" && hint.groupId === group.id) {
      commit(withItemDroppedOn(custom, NAV_NODES, drag.id, group.id, hint.beforeId));
    }
    endDrag();
  };

  return (
    <div className="menu-manager" ref={rootRef}>
      <div className="mm-toolbar">
        <p className="mm-hint">{t("menuTool.hint")}</p>
        <div className="mm-toolbar-actions">
          <button type="button" className="mm-btn mm-btn-accent" onClick={addGroup}>
            <Plus size={14} />
            {t("menuTool.newGroup")}
          </button>
          {(custom.renames && Object.keys(custom.renames).length > 0) ||
          custom.itemOrder.length > 0 ||
          custom.groupOrder.length > 0 ||
          custom.customGroups.length > 0 ||
          Object.keys(custom.itemGroups).length > 0 ? (
            <button type="button" className="mm-btn mm-btn-danger" onClick={() => void resetAll()}>
              <RotateCcw size={14} />
              {t("menuTool.resetAll")}
            </button>
          ) : null}
        </div>
      </div>

      {groups.map((group, gi) => {
        const gKey = `${GROUP_RENAME_PREFIX}${group.id}`;
        const gName = groupLabel(group);
        const gRenamed = Boolean(custom.renames[gKey]);
        const groupDragging = drag?.kind === "group" && drag.id === group.id;
        const groupDropBefore = dropHint?.kind === "group" && dropHint.beforeGroupId === group.id;
        // 组末落点画在卡片底边：拖菜单进组末，或拖分组到整个列表最后。
        const dropEnd =
          (dropHint?.kind === "item" && dropHint.groupId === group.id && dropHint.beforeId === null) ||
          (dropHint?.kind === "group" && dropHint.beforeGroupId === null && gi === groups.length - 1);
        return (
          <section
            key={group.id}
            className={[
              "mm-group",
              groupDragging ? "is-dragging" : "",
              groupDropBefore ? "is-drop-before" : "",
              dropEnd ? "is-drop-end" : "",
            ]
              .filter(Boolean)
              .join(" ")}
            data-mm-group={group.id}
            onDragOver={(e) => onGroupDragOver(e, group.id, gi)}
            onDrop={(e) => onGroupDrop(e, group)}
          >
            <div
              className="mm-group-header"
              draggable={editingKey !== gKey}
              onDragStart={(e) => startGroupDrag(e, group.id)}
              onDragEnd={endDrag}
            >
              <span className="mm-grip" title={t("menuTool.dragGroup")} aria-hidden="true">
                <GripVertical size={12} />
              </span>
              {editingKey === gKey ? (
                <input
                  ref={editInputRef}
                  className="input-field mm-rename-input"
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onBlur={commitEdit}
                  onKeyDown={onRenameKeyDown}
                  aria-label={t("menuTool.rename")}
                />
              ) : (
                <>
                  <span className="mm-group-name" title={gName}>
                    {gName}
                  </span>
                  <span className="mm-count">{group.children.length}</span>
                  {group.custom && <span className="mm-badge">{t("menuTool.customGroup")}</span>}
                  <button
                    type="button"
                    className="mm-icon-btn"
                    title={t("menuTool.rename")}
                    aria-label={t("menuTool.rename")}
                    onClick={() => startEdit(gKey, gName)}
                  >
                    <Pencil size={13} />
                  </button>
                  {gRenamed && (
                    <button
                      type="button"
                      className="mm-icon-btn"
                      title={t("menuTool.resetName")}
                      aria-label={t("menuTool.resetName")}
                      onClick={() => commit(withRename(custom, gKey, ""))}
                    >
                      <X size={13} />
                    </button>
                  )}
                </>
              )}
              <span className="mm-flex" />
              {group.custom && (
                <button
                  type="button"
                  className="mm-icon-btn mm-icon-danger"
                  title={t("menuTool.deleteGroup")}
                  aria-label={t("menuTool.deleteGroup")}
                  onClick={() => void removeGroup(group)}
                >
                  <Trash2 size={13} />
                </button>
              )}
            </div>

            {group.children.length === 0 && (
              <p className="mm-empty">{t("menuTool.emptyGroup")}</p>
            )}
            {group.children.map((leaf, ii) => {
              const name = itemLabel(leaf.id, leaf.labelKey);
              const renamed = Boolean(custom.renames[leaf.id]);
              // 改名用的键 = 页签 id 本身（分组才带 `group:` 前缀）。editingKey 必须与
              // renames 的键一致，否则写进去的是 `item:<id>`、读的是 `<id>`，改名无声丢失。
              const itemKey = leaf.id;
              const itemDragging = drag?.kind === "item" && drag.id === leaf.id;
              const itemDropBefore =
                dropHint?.kind === "item" &&
                dropHint.groupId === group.id &&
                dropHint.beforeId === leaf.id;
              return (
                <div
                  key={leaf.id}
                  className={[
                    "mm-item",
                    itemDragging ? "is-dragging" : "",
                    itemDropBefore ? "is-drop-before" : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  draggable={editingKey !== itemKey}
                  onDragStart={(e) => startItemDrag(e, leaf.id)}
                  onDragEnd={endDrag}
                  onDragOver={(e) => onItemDragOver(e, group.id, leaf.id, ii, group.children)}
                  onDrop={(e) => onItemDrop(e, group.id)}
                >
                  <span className="mm-grip" title={t("menuTool.dragItem")} aria-hidden="true">
                    <GripVertical size={12} />
                  </span>
                  <span className="nav-icon">{leaf.icon}</span>
                  {editingKey === itemKey ? (
                    <input
                      ref={editInputRef}
                      className="input-field mm-rename-input"
                      value={draft}
                      onChange={(e) => setDraft(e.target.value)}
                      onBlur={commitEdit}
                      onKeyDown={onRenameKeyDown}
                      aria-label={t("menuTool.rename")}
                    />
                  ) : (
                    <>
                      <span className="mm-item-name" title={name}>
                        {name}
                      </span>
                      {renamed && (
                        <button
                          type="button"
                          className="mm-icon-btn"
                          title={t("menuTool.resetName")}
                          aria-label={t("menuTool.resetName")}
                          onClick={() => commit(withRename(custom, leaf.id, ""))}
                        >
                          <X size={13} />
                        </button>
                      )}
                      <button
                        type="button"
                        className="mm-icon-btn"
                        title={t("menuTool.rename")}
                        aria-label={t("menuTool.rename")}
                        onClick={() => startEdit(itemKey, name)}
                      >
                        <Pencil size={13} />
                      </button>
                    </>
                  )}
                  <span className="mm-flex" />
                  <select
                    className="input-field mm-move-select"
                    value={group.id}
                    title={gName}
                    aria-label={t("menuTool.moveTo")}
                    onChange={(e) =>
                      commit(withItemGroupChanged(custom, NAV_NODES, leaf.id, e.target.value))
                    }
                  >
                    {groups.map((g) => (
                      <option key={g.id} value={g.id}>
                        {groupLabel(g)}
                      </option>
                    ))}
                  </select>
                </div>
              );
            })}
          </section>
        );
      })}
    </div>
  );
}
