/**
 * AI Hub workspace / service selection. Pure.
 *
 * The persisted selection (`settings.aiHubFocusRepos`) mixes two entry kinds:
 * `ws:<workspaceId>` = a whole workspace, resolved to its services every time
 * (so services added later are picked up), and plain paths = single services /
 * empty workspaces.
 *
 * Every entry is a workspace: the Git page's saved workspaces, folders added
 * from the picker this session, and one-service workspaces for anything not
 * claimed by a workspace folder. A workspace may hold many services, one, or
 * none at all (a plain folder added from the picker).
 */
import { findWorkspaceForRepo, isPathUnder, pathKey, projectNameFromPath } from "../../core/pathUtils";
import type { GitScannedRepo, GitWorkspace, RecentProject } from "../../core/types";

export type RepoOption = { path: string; name: string };

/** A folder added from the picker (session-only, not written to the Git page). */
export type ExtraFolder = { path: string; name: string; repos: GitScannedRepo[] };

export type PickerGroup = {
  key: string;
  /** `ws:<id>` for a saved workspace; null for session folders and one-service workspaces. */
  token: string | null;
  /** Workspace name. */
  name: string;
  /** Workspace folder; null only as a defensive fallback. */
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
  extraFolders: ExtraFolder[],
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
  for (const f of extraFolders) for (const r of f.repos) add(r.path, r.name);
  for (const s of selection) if (!isGroupToken(s)) add(s);

  // Session folders ride along as workspaces with negative ids (never persisted).
  const workspaces: GitWorkspace[] = [
    ...gitWorkspaces,
    ...extraFolders
      .filter((f) => !gitWorkspaces.some((w) => pathKey(w.path) === pathKey(f.path)))
      .map((f, i) => ({ id: -(i + 1), path: f.path, name: f.name, createdAt: "" })),
  ];

  // Which workspace claims each repo (strictly inside the workspace folder).
  const wsOf = new Map<string, GitWorkspace>();
  for (const p of all) {
    const ws = findWorkspaceForRepo(p.path, workspaces);
    if (ws) wsOf.set(pathKey(p.path), ws);
  }

  const groups: PickerGroup[] = workspaces.map((ws) => ({
    key: ws.id > 0 ? `ws-${ws.id}` : `session-${pathKey(ws.path)}`,
    token: ws.id > 0 ? `${WS_PREFIX}${ws.id}` : null,
    name: ws.name || projectNameFromPath(ws.path),
    path: ws.path,
    repos: all
      .filter((p) => wsOf.get(pathKey(p.path))?.id === ws.id)
      .map((p) => ({ path: p.path, name: p.name || projectNameFromPath(p.path) }))
      .sort((a, b) => a.name.localeCompare(b.name)),
  }));

  // Anything not claimed by a workspace folder is a one-service workspace of its
  // own — except paths that ARE a workspace folder (e.g. a persisted selection
  // entry pointing at an empty workspace): those already have their group.
  const claimed = new Set(groups.flatMap((g) => g.repos.map((r) => pathKey(r.path))));
  const wsFolders = new Set(workspaces.map((w) => pathKey(w.path)));
  for (const p of all) {
    const k = pathKey(p.path);
    if (claimed.has(k) || wsFolders.has(k)) continue;
    groups.push({
      key: `repo-${k}`,
      token: null,
      name: p.name || projectNameFromPath(p.path),
      path: p.path,
      repos: [{ path: p.path, name: p.name || projectNameFromPath(p.path) }],
    });
  }
  return groups;
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
  // A selection entry naming the workspace folder itself selects the whole
  // workspace (the only way to select an empty one).
  if (group.path && hasPath(selection, group.path)) return "all";
  if (group.repos.length === 0) return "none";
  const n = group.repos.filter((r) => hasPath(selection, r.path)).length;
  if (n === 0) return "none";
  return n === group.repos.length ? "all" : "some";
}

function withoutGroup(selection: string[], group: PickerGroup): string[] {
  const keys = new Set(group.repos.map((r) => pathKey(r.path)));
  const self = group.path ? pathKey(group.path) : null;
  return selection.filter((s) => {
    if (s === group.token) return false;
    if (self && !isGroupToken(s) && pathKey(s) === self) return false;
    return isGroupToken(s) || !keys.has(pathKey(s));
  });
}

/** Fully selected → clear the group; otherwise → select the whole group. */
export function toggleGroup(selection: string[], group: PickerGroup): string[] {
  const rest = withoutGroup(selection, group);
  if (groupState(group, selection) === "all") return rest;
  if (group.token) return [...rest, group.token];
  if (group.repos.length > 0) return [...rest, ...group.repos.map((r) => r.path)];
  // An empty workspace selects its folder itself.
  return group.path ? [...rest, group.path] : rest;
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
      if (!g) continue;
      // An empty workspace resolves to its folder itself.
      if (g.repos.length === 0 && g.path) push(g.path);
      else g.repos.forEach((r) => push(r.path));
    } else {
      // A path naming a workspace folder selects the whole workspace.
      const asFolder = groups.find((x) => x.path && pathKey(x.path) === pathKey(s));
      if (asFolder) {
        if (asFolder.repos.length === 0) push(s);
        else asFolder.repos.forEach((r) => push(r.path));
      } else {
        push(s);
      }
    }
  }
  return out;
}

/** Where an entry lives: its workspace group (folder paths name their group), or the repo itself. */
function placeOf(entry: string, groups: PickerGroup[]): string {
  if (isGroupToken(entry)) return groups.find((g) => g.token === entry)?.key ?? entry;
  const k = pathKey(entry);
  const g = groups.find(
    (x) => x.path && (pathKey(x.path) === k || x.repos.some((r) => pathKey(r.path) === k)),
  );
  return g?.key ?? `repo:${k}`;
}

/**
 * Edit levels work in one place: keep only the entries in the same workspace
 * (or the same standalone repo) as `touched`, by default the first entry.
 */
export function keepOnePlace(selection: string[], groups: PickerGroup[], touched?: string): string[] {
  if (selection.length === 0) return selection;
  const keep = placeOf(touched ?? selection[0], groups);
  return selection.filter((s) => placeOf(s, groups) === keep);
}

/** The selection spans more than one workspace / standalone repo. */
export function spansPlaces(selection: string[], groups: PickerGroup[]): boolean {
  return new Set(selection.map((s) => placeOf(s, groups))).size > 1;
}

/**
 * Directory the agent works in. One service → that repo; several services of
 * one workspace → the workspace folder, with the picked services listed.
 * Anything past the first workspace only reaches the prompt as a summary.
 */
export function agentTarget(
  selection: string[],
  groups: PickerGroup[],
  currentRepo: string | null | undefined,
): { root: string; services: string[] } {
  const paths = resolveFocus(selection, groups);
  if (paths.length === 0) return { root: currentRepo || "", services: [] };
  const first = pathKey(paths[0]);
  const ws = groups.find((g) => g.path && g.repos.some((r) => pathKey(r.path) === first));
  const services = ws?.path ? paths.filter((p) => isPathUnder(p, ws.path as string)) : [];
  if (!ws?.path || services.length < 2) return { root: paths[0], services: [] };
  return { root: ws.path, services };
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
    // An empty workspace works in the folder itself.
    if (g.repos.length === 0) return g.path ? g.name : fmt("ctx.otherGroup");
    // A one-service workspace reads as just the service.
    if (picked.length === 1 && state === "all") return picked[0].name;
    if (!g.path) {
      return fmt("ctx.groupSome", { name: fmt("ctx.otherGroup"), n: picked.length });
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
        if (!x.g.path || picked.length === 1) return picked.length === 1 ? picked[0].name : short(x);
        return x.g.name;
      })
      .join("、");
  }
  return fmt("ctx.nRepos", { n: active.length });
}
