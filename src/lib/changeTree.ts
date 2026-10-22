import type { GitStatusEntry } from "../core/types";

/**
 * Pure helpers for the commit page's IDEA-style change list.
 *
 * No IO and no React: grouping, path-tree building, tri-state selection math
 * and search filtering all live here so the rendering components stay thin.
 *
 * The commit pipeline (toStage/toUnstage in CommitChangelist.runCommit) keys
 * off `selectionKey` = `${group}\0${path}`. A file can legitimately appear in
 * both the staged and the unstaged view of the merged "更改" group, so the
 * group dimension must stay part of the key — only the presentation changed.
 */

/** Composite identity of one tickable changelist row. */
export function selectionKey(entry: { path: string; group: string }): string {
  return `${entry.group}\0${entry.path}`;
}

/** React list key for a changelist row (repo-scoped). */
export function fileKey(repoPath: string, filePath: string, group?: string): string {
  return group ? `${repoPath}\0${group}\0${filePath}` : `${repoPath}\0${filePath}`;
}

/** Single-letter status badge for an entry ("M", "A", "?", "U", …). */
export function statusCode(entry: GitStatusEntry): string {
  if (entry.group === "conflicted") return "U";
  if (entry.group === "untracked") return "?";
  if (entry.group === "staged") return (entry.indexStatus || "M").trim() || "M";
  return (entry.workTreeStatus || "M").trim() || "M";
}

/** CSS modifier for the status badge color. */
export function statusClass(code: string): string {
  const c = code.toUpperCase();
  if (c === "A") return "git-status-a";
  if (c === "D") return "git-status-d";
  if (c === "R" || c === "C") return "git-status-r";
  if (c === "?" || c === "!") return "git-status-u";
  if (c === "U") return "git-status-conflict";
  return "git-status-m";
}

/** IDEA-style top-level groups: tracked changes vs never-committed files. */
export type ChangeGroupId = "changes" | "untracked";

export type ChangeLeafNode = {
  kind: "file";
  /** Directory the leaf sits under, "" when directly in the repo root. */
  dir: string;
  name: string;
  entry: GitStatusEntry;
  key: string;
};

export type ChangeDirNode = {
  kind: "dir";
  /** Full directory path relative to the repo root. */
  path: string;
  name: string;
  children: ChangeTreeNode[];
};

export type ChangeTreeNode = ChangeDirNode | ChangeLeafNode;

export type ChangeGroups = {
  changes: GitStatusEntry[];
  untracked: GitStatusEntry[];
};

const groupRank = (e: GitStatusEntry): number =>
  e.group === "conflicted" ? 0 : e.group === "staged" ? 1 : 2;

/**
 * Split a repo's status entries into the two display groups. Conflicted and
 * both staged/unstaged rows land in "更改" (conflicts sort first — they are
 * what blocks the repo); "untracked" forms its own group.
 */
export function splitChangeGroups(entries: GitStatusEntry[]): ChangeGroups {
  const changes: GitStatusEntry[] = [];
  const untracked: GitStatusEntry[] = [];
  for (const e of entries) {
    if (e.group === "untracked") untracked.push(e);
    else changes.push(e);
  }
  const byPath = (a: GitStatusEntry, b: GitStatusEntry) => a.path.localeCompare(b.path);
  changes.sort((a, b) => groupRank(a) - groupRank(b) || byPath(a, b));
  untracked.sort(byPath);
  return { changes, untracked };
}

/**
 * Build a multi-level directory tree from entries (git `/`-relative paths).
 * Same-path entries from different groups become sibling leaves so each keeps
 * its own tick state; siblings sort dirs-first, then files by name.
 */
export function buildChangeTree(entries: GitStatusEntry[]): ChangeTreeNode[] {
  const roots: ChangeTreeNode[] = [];
  const dirIndex = new Map<string, ChangeDirNode>();
  const childrenOf = (dir: string): ChangeTreeNode[] => {
    if (!dir) return roots;
    const existing = dirIndex.get(dir);
    if (existing) return existing.children;
    const cut = dir.lastIndexOf("/");
    const parent = cut === -1 ? "" : dir.slice(0, cut);
    const node: ChangeDirNode = {
      kind: "dir",
      path: dir,
      name: cut === -1 ? dir : dir.slice(cut + 1),
      children: [],
    };
    dirIndex.set(dir, node);
    childrenOf(parent).push(node);
    return node.children;
  };
  for (const e of entries) {
    const cut = e.path.lastIndexOf("/");
    const dir = cut === -1 ? "" : e.path.slice(0, cut);
    const name = cut === -1 ? e.path : e.path.slice(cut + 1);
    childrenOf(dir).push({ kind: "file", dir, name, entry: e, key: selectionKey(e) });
  }
  const sortNodes = (nodes: ChangeTreeNode[]) => {
    nodes.sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === "dir" ? -1 : 1;
      if (a.kind === "dir" && b.kind === "dir") return a.path.localeCompare(b.path);
      if (a.kind === "file" && b.kind === "file") {
        return (
          a.name.localeCompare(b.name) ||
          groupRank(a.entry) - groupRank(b.entry) ||
          a.entry.path.localeCompare(b.entry.path)
        );
      }
      return 0;
    });
    for (const n of nodes) if (n.kind === "dir") sortNodes(n.children);
  };
  sortNodes(roots);
  return roots;
}

export type SelectionState = "all" | "some" | "none";

/** Tickable leaves below a node — conflicted rows are resolved elsewhere. */
export function collectLeafKeys(node: ChangeTreeNode, out: string[] = []): string[] {
  if (node.kind === "file") {
    if (node.entry.group !== "conflicted") out.push(node.key);
    return out;
  }
  for (const child of node.children) collectLeafKeys(child, out);
  return out;
}

/** All leaves below a node regardless of tickability (badges/counts). */
export function countLeaves(node: ChangeTreeNode): number {
  if (node.kind === "file") return 1;
  let n = 0;
  for (const child of node.children) n += countLeaves(child);
  return n;
}

/** Tri-state of a node's checkbox against the repo's selected-key set. */
export function nodeSelectionState(node: ChangeTreeNode, selected: Set<string>): SelectionState {
  const keys = collectLeafKeys(node);
  if (keys.length === 0) return "none";
  let hits = 0;
  for (const k of keys) if (selected.has(k)) hits += 1;
  if (hits === 0) return "none";
  return hits === keys.length ? "all" : "some";
}

/** Directory paths of every dir node in the forest (for expand/collapse all). */
export function collectDirPaths(nodes: ChangeTreeNode[], out: string[] = []): string[] {
  for (const n of nodes) {
    if (n.kind !== "dir") continue;
    out.push(n.path);
    collectDirPaths(n.children, out);
  }
  return out;
}

const pathMatches = (path: string, query: string): boolean =>
  path.toLowerCase().includes(query);

/**
 * Keep only leaves whose full path contains `query`, plus the ancestor chain
 * of surviving leaves. While searching the caller should treat every dir as
 * expanded so matched paths are visible without manual unfolding.
 */
export function filterTree(nodes: ChangeTreeNode[], query: string): ChangeTreeNode[] {
  const q = query.trim().toLowerCase();
  if (!q) return nodes;
  const walk = (list: ChangeTreeNode[]): ChangeTreeNode[] => {
    const out: ChangeTreeNode[] = [];
    for (const n of list) {
      if (n.kind === "file") {
        if (pathMatches(n.entry.path, q)) out.push(n);
        continue;
      }
      const children = walk(n.children);
      const selfHit = pathMatches(n.path, q);
      if (selfHit || children.length > 0) out.push({ ...n, children: selfHit ? [...n.children] : children });
    }
    return out;
  };
  return walk(nodes);
}
