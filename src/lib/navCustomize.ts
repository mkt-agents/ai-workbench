/**
 * 侧栏菜单自定义（菜单管理页的纯逻辑层，不含 React / IO）：
 * - 改名：导航项按 id、分组按 `group:<id>` 覆盖 `navigation.json` 文案；
 * - 排序：`itemOrder` / `groupOrder` 是完整顺序，缺失的（新版本新增页签）
 *   按默认顺序补在后面，所以升级不会丢布局；
 * - 分组：`itemGroups` 覆盖默认分组，`customGroups` 是用户新建的组。
 *
 * 权威结构永远来自 `navStructure.NAV_NODES`；这里只做投影与编辑。
 */
import type { NavCustomization } from "../core/types";
import type { NavLeaf, NavNode } from "./navStructure";

export const GROUP_RENAME_PREFIX = "group:";

export interface EffectiveNavGroup {
  id: string;
  /** 默认分组的 i18n key；自定义分组为 null（名字存 renames）。 */
  labelKey: string | null;
  custom: boolean;
  children: NavLeaf[];
}

const asRecord = (v: unknown): Record<string, string> => {
  if (!v || typeof v !== "object") return {};
  const out: Record<string, string> = {};
  for (const [k, val] of Object.entries(v as Record<string, unknown>)) {
    if (typeof k === "string" && k && typeof val === "string") out[k] = val;
  }
  return out;
};

const asStringArray = (v: unknown): string[] => {
  if (!Array.isArray(v)) return [];
  const seen = new Set<string>();
  for (const item of v) if (typeof item === "string" && item && !seen.has(item)) seen.add(item);
  return [...seen];
};

/**
 * 丢弃旧版本写坏的导航项改名键：曾经的菜单管理页用 `item:<id>` 作编辑键，改名就写进了
 * `renames["item:<id>"]`，读取侧只认页签 id，于是永远不生效。这些幽灵键留在库里会让
 * 「恢复默认」按钮一直亮着，读入时直接清掉（没有任何地方读它们）。
 */
function dropLegacyItemRenameKeys(renames: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(renames)) {
    if (!key.startsWith("item:")) out[key] = value;
  }
  return out;
}

/** 清洗持久化数据：任何字段坏掉都退回默认布局的那部分，而不是崩掉。 */
export function normalizeNavCustomization(raw: unknown): NavCustomization {
  const v = (raw ?? {}) as Record<string, unknown>;
  return {
    renames: dropLegacyItemRenameKeys(asRecord(v.renames)),
    itemGroups: asRecord(v.itemGroups),
    itemOrder: asStringArray(v.itemOrder),
    groupOrder: asStringArray(v.groupOrder),
    customGroups: asStringArray(v.customGroups),
  };
}

function defaultGroups(nodes: NavNode[]) {
  return nodes.filter((n): n is Extract<NavNode, { group: string }> => "group" in n);
}

function leafIndex(nodes: NavNode[]): Map<string, NavLeaf> {
  const map = new Map<string, NavLeaf>();
  for (const n of nodes) {
    if ("group" in n) for (const c of n.children) map.set(c.id, c);
    else map.set(n.id, n);
  }
  return map;
}

/** 完整的导航项顺序：用户顺序 ∩ 现存项 → 其余按默认顺序补齐。 */
export function effectiveItemOrder(nodes: NavNode[], custom: NavCustomization): string[] {
  const defaults = defaultGroups(nodes);
  const known = leafIndex(nodes);
  const seen = new Set<string>();
  const order: string[] = [];
  for (const id of custom.itemOrder) {
    if (known.has(id) && !seen.has(id)) {
      seen.add(id);
      order.push(id);
    }
  }
  for (const g of defaults) {
    for (const c of g.children) {
      if (!seen.has(c.id)) {
        seen.add(c.id);
        order.push(c.id);
      }
    }
  }
  return order;
}

/** 完整的分组顺序：用户顺序 ∩ 现存组 → 其余默认组按默认顺序 → 自定义组。 */
export function effectiveGroupOrder(nodes: NavNode[], custom: NavCustomization): string[] {
  const defaults = defaultGroups(nodes);
  const all = [...defaults.map((g) => g.group), ...custom.customGroups];
  const seen = new Set<string>();
  const order: string[] = [];
  for (const gid of custom.groupOrder) {
    if (all.includes(gid) && !seen.has(gid)) {
      seen.add(gid);
      order.push(gid);
    }
  }
  for (const g of defaults) {
    if (!seen.has(g.group)) {
      seen.add(g.group);
      order.push(g.group);
    }
  }
  for (const gid of custom.customGroups) {
    if (!seen.has(gid)) {
      seen.add(gid);
      order.push(gid);
    }
  }
  return order;
}

/** 侧栏渲染用的分组投影：组序、成员、归属全部落定。 */
export function applyNavCustomization(nodes: NavNode[], custom: NavCustomization): EffectiveNavGroup[] {
  const defaults = defaultGroups(nodes);
  const leaves = leafIndex(nodes);
  const defaultGroupOf = new Map<string, string>();
  for (const g of defaults) for (const c of g.children) defaultGroupOf.set(c.id, g.group);

  const order = effectiveItemOrder(nodes, custom);
  const gOrder = effectiveGroupOrder(nodes, custom);
  const customSet = new Set(custom.customGroups);
  const labelKeyOf = new Map(defaults.map((g) => [g.group, g.labelKey]));

  const childrenOf = new Map<string, NavLeaf[]>();
  for (const gid of gOrder) childrenOf.set(gid, []);
  for (const id of order) {
    // itemGroups 指向已删除的组时回落默认组。
    const target = custom.itemGroups[id] ?? defaultGroupOf.get(id);
    const gid = target && childrenOf.has(target) ? target : defaultGroupOf.get(id);
    if (!gid || !childrenOf.has(gid)) continue;
    childrenOf.get(gid)!.push(leaves.get(id)!);
  }

  return gOrder.map((gid) => ({
    id: gid,
    labelKey: labelKeyOf.get(gid) ?? null,
    custom: customSet.has(gid),
    children: childrenOf.get(gid) ?? [],
  }));
}

function defaultGroupOfId(nodes: NavNode[], itemId: string): string | null {
  for (const n of nodes) {
    if ("group" in n && n.children.some((c) => c.id === itemId)) return n.group;
  }
  return null;
}

/** 移动到别的分组：落为目标组最后一项。 */
export function withItemGroupChanged(
  custom: NavCustomization,
  nodes: NavNode[],
  itemId: string,
  groupId: string,
): NavCustomization {
  // 目标组不存在（自定义组被删、或持久化数据异常）时不动，避免写出悬挂映射。
  if (!effectiveGroupOrder(nodes, custom).includes(groupId)) return custom;
  if (defaultGroupOfId(nodes, itemId) === groupId && !custom.itemGroups[itemId]) return custom;
  const itemGroups = { ...custom.itemGroups, [itemId]: groupId };
  if (defaultGroupOfId(nodes, itemId) === groupId) delete itemGroups[itemId];
  // 目标组内放到末尾：从旧位置摘除再追加（顺序过滤天然按组归位）。
  const itemOrder = [...effectiveItemOrder(nodes, { ...custom, itemGroups }).filter((id) => id !== itemId), itemId];
  return { ...custom, itemGroups, itemOrder };
}

/** 把 id 插到 beforeId 之前；beforeId 为 null（或已不在列表里）时追加到末尾。 */
function placeBefore(list: string[], id: string, beforeId: string | null): string[] {
  const rest = list.filter((entry) => entry !== id);
  const at = beforeId && rest.includes(beforeId) ? rest.indexOf(beforeId) : rest.length;
  rest.splice(at, 0, id);
  return rest;
}

/** 拖拽分组：落到 beforeGroupId 之前（null = 整个列表末尾）。 */
export function withGroupDroppedOn(
  custom: NavCustomization,
  nodes: NavNode[],
  groupId: string,
  beforeGroupId: string | null,
): NavCustomization {
  const order = effectiveGroupOrder(nodes, custom);
  if (!order.includes(groupId)) return custom;
  if (beforeGroupId === groupId) return custom; // 落在自己身上：不动
  return { ...custom, groupOrder: placeBefore(order, groupId, beforeGroupId) };
}

/**
 * 拖拽导航项：落到目标分组的 beforeItemId 之前（null = 组末）。
 * 组内重排与跨组移动共用一套落点语义，避免「拖到别的组」要另算索引。
 */
export function withItemDroppedOn(
  custom: NavCustomization,
  nodes: NavNode[],
  itemId: string,
  groupId: string,
  beforeItemId: string | null,
): NavCustomization {
  if (beforeItemId === itemId) return custom; // 落在自己身上：不动
  const validGroups = new Set(effectiveGroupOrder(nodes, custom));
  if (!validGroups.has(groupId)) return custom;
  if (!leafIndex(nodes).has(itemId)) return custom;

  const itemGroups = { ...custom.itemGroups };
  const isDefaultGroup = defaultGroupOfId(nodes, itemId) === groupId;
  if (isDefaultGroup) delete itemGroups[itemId];
  else itemGroups[itemId] = groupId;

  const next: NavCustomization = { ...custom, itemGroups };
  // 锚点必须还落在目标组里（跨组拖拽时锚点属于原组的话，按组末处理）。
  const anchorGroup =
    beforeItemId === null
      ? null
      : itemGroups[beforeItemId] ?? defaultGroupOfId(nodes, beforeItemId);
  const anchor = beforeItemId && anchorGroup === groupId ? beforeItemId : null;
  return { ...next, itemOrder: placeBefore(effectiveItemOrder(nodes, next), itemId, anchor) };
}

/** 改名：空串 = 恢复默认文案。 */
export function withRename(custom: NavCustomization, key: string, name: string): NavCustomization {
  const trimmed = name.trim();
  const renames = { ...custom.renames };
  if (!trimmed || trimmed === "\u200b") delete renames[key];
  else renames[key] = trimmed;
  return { ...custom, renames };
}

/** 新建自定义分组（label 由调用方按当前语言给）。 */
export function withNewGroup(custom: NavCustomization, label: string): { next: NavCustomization; id: string } {
  const id = `custom-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
  return {
    next: {
      ...custom,
      customGroups: [...custom.customGroups, id],
      // 追加到末尾；effectiveGroupOrder 对未知组本来就是补尾语义。
      groupOrder: [...custom.groupOrder, id],
      renames: { ...custom.renames, [`${GROUP_RENAME_PREFIX}${id}`]: label },
    },
    id,
  };
}

/** 删除自定义分组：成员回到默认组，组名与顺序一并清理。默认组不可删。 */
export function withGroupRemoved(custom: NavCustomization, groupId: string): NavCustomization {
  if (!custom.customGroups.includes(groupId)) return custom;
  const itemGroups: Record<string, string> = {};
  for (const [id, gid] of Object.entries(custom.itemGroups)) {
    if (gid !== groupId) itemGroups[id] = gid;
  }
  const renames = { ...custom.renames };
  delete renames[`${GROUP_RENAME_PREFIX}${groupId}`];
  return {
    ...custom,
    customGroups: custom.customGroups.filter((g) => g !== groupId),
    groupOrder: custom.groupOrder.filter((g) => g !== groupId),
    itemGroups,
    renames,
  };
}
