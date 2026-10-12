/**
 * Quick-ask repo selection by workspace or by service. Pure.
 *
 * The persisted selection (`settings.quickAskFocusRepos`) mixes two entry kinds:
 * `ws:<workspaceId>` = a whole workspace, resolved to its services every time
 * (so services added later are picked up), and plain repo paths = single services.
 */
import { buildRepoGroups } from "../../core/repoGroups";
import { pathKey, projectNameFromPath } from "../../core/pathUtils";
import type { GitScannedRepo, GitWorkspace, RecentProject } from "../../core/types";

export type RepoOption = { path: string; name: string };

export type PickerGroup = {
  key: string;
  /** `ws:<id>` for a workspace; null for the "other repos" bucket (no dynamic token). */
  token: string | null;
  /** Workspace name; empty for the "other repos" bucket. */
  name: string;
  path: string | null;
  repos: RepoOption[];
};

export type GroupState = "all" | "some" | "none";

const WS_PREFIX = "ws:";

export function isGroupToken(entry: string): boolean {
  return entry.startsWith(WS_PREFIX);
}

export function buildPickerGroups(
  gitWorkspaces: GitWorkspace[],
  recentProjects: RecentProject[],
  extraRepos: GitScannedRepo[],
  currentRepo: string | null | undefined,
  selection: string[],
): PickerGroup[] {
  const seen = new Set<string>();
  const all: RecentProject[] = [];
  const add = (path: string | null | undefined, name?: string) => {
    if (!path) return;
    const k = pathKey(path);
    if (seen.has(k)) return;
    seen.add(k);
    all.push({ id: 0, path, name: name || projectNameFromPath(path), lastOpenedAt: "" });
  };
  add(currentRepo);
  for (const p of recentProjects) add(p.path, p.name);
  for (const r of extraRepos) add(r.path, r.name);
  for (const s of selection) if (!isGroupToken(s)) add(s);

  return buildRepoGroups(gitWorkspaces, all)
    .filter((g) => g.projects.length > 0)
    .map((g) => ({
      key: g.key,
      token: g.workspace ? `${WS_PREFIX}${g.workspace.id}` : null,
      name: g.workspace?.name || (g.workspace ? projectNameFromPath(g.workspace.path) : ""),
      path: g.workspace?.path ?? null,
      repos: g.projects
        .map((p) => ({ path: p.path, name: p.name || projectNameFromPath(p.path) }))
        .sort((a, b) => a.name.localeCompare(b.name)),
    }));
}

function hasPath(selection: string[], path: string): boolean {
  const k = pathKey(path);
  return selection.some((s) => !isGroupToken(s) && pathKey(s) === k);
}

export function isServiceSelected(selection: string[], group: PickerGroup, path: string): boolean {
  return (group.token !== null && selection.includes(group.token)) || hasPath(selection, path);
}

export function groupState(group: PickerGroup, selection: string[]): GroupState {
  if (group.token && selection.includes(group.token)) return "all";
  const n = group.repos.filter((r) => hasPath(selection, r.path)).length;
  if (n === 0) return "none";
  return n === group.repos.length ? "all" : "some";
}

function withoutGroup(selection: string[], group: PickerGroup): string[] {
  const keys = new Set(group.repos.map((r) => pathKey(r.path)));
  return selection.filter((s) => s !== group.token && (isGroupToken(s) || !keys.has(pathKey(s))));
}

/** Fully selected → clear the group; otherwise → select the whole group. */
export function toggleGroup(selection: string[], group: PickerGroup): string[] {
  const rest = withoutGroup(selection, group);
  if (groupState(group, selection) === "all") return rest;
  return group.token ? [...rest, group.token] : [...rest, ...group.repos.map((r) => r.path)];
}

/**
 * Unchecking one service of a whole-workspace selection keeps the others as
 * explicit paths; checking the last missing service collapses back to the token.
 */
export function toggleService(selection: string[], group: PickerGroup, path: string): string[] {
  const rest = withoutGroup(selection, group);
  const k = pathKey(path);
  const current = group.repos.filter((r) => isServiceSelected(selection, group, r.path)).map((r) => r.path);
  const next = current.some((p) => pathKey(p) === k)
    ? current.filter((p) => pathKey(p) !== k)
    : [...current, path];
  if (group.token && next.length === group.repos.length) return [...rest, group.token];
  return [...rest, ...next];
}

/** Expand workspace tokens into service paths (deduplicated, order kept). */
export function resolveFocus(selection: string[], groups: PickerGroup[]): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const push = (p: string) => {
    const k = pathKey(p);
    if (!seen.has(k)) {
      seen.add(k);
      out.push(p);
    }
  };
  for (const s of selection) {
    if (isGroupToken(s)) {
      const g = groups.find((x) => x.token === s);
      g?.repos.forEach((r) => push(r.path));
    } else {
      push(s);
    }
  }
  return out;
}

type Fmt = (key: string, params?: Record<string, string | number>) => string;

/** One label for the picker trigger and the composer pill. Null = nothing to analyze. */
export function describeSelection(
  selection: string[],
  groups: PickerGroup[],
  autoName: string | null,
  fmt: Fmt,
): string | null {
  if (selection.length === 0) return autoName;

  const active = groups
    .map((g) => ({ g, state: groupState(g, selection) }))
    .filter((x) => x.state !== "none");
  if (active.length === 0) return autoName;

  const short = ({ g, state }: { g: PickerGroup; state: GroupState }): string => {
    const picked = g.repos.filter((r) => isServiceSelected(selection, g, r.path));
    if (!g.token) {
      return picked.length === 1 ? picked[0].name : fmt("ctx.groupSome", { name: fmt("ctx.otherGroup"), n: picked.length });
    }
    if (state === "all") return fmt("ctx.groupAll", { name: g.name });
    if (picked.length === 1) return fmt("ctx.groupOne", { name: g.name, svc: picked[0].name });
    return fmt("ctx.groupSome", { name: g.name, n: picked.length });
  };

  if (active.length === 1) return short(active[0]);
  if (active.length === 2) {
    return active
      .map((x) => {
        const picked = x.g.repos.filter((r) => isServiceSelected(selection, x.g, r.path));
        if (!x.g.token || picked.length === 1) return picked.length === 1 ? picked[0].name : short(x);
        return x.g.name;
      })
      .join("、");
  }
  return fmt("ctx.nRepos", { n: active.length });
}
