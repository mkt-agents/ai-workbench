/**
 * Status derivation for the repo list: filter chips, in-group ordering and the
 * per-group summary all read the same flags, so they can never disagree.
 * Pure — no store access, no IO.
 */
import { isPushable } from "./gitPushScope";
import { pathKey, projectNameFromPath } from "./pathUtils";
import type { GitRepoConfig, RecentProject, RepoBatchItem } from "./types";

export type RepoFilter = "all" | "dirty" | "sync" | "identity";

export const REPO_FILTERS: RepoFilter[] = ["all", "dirty", "sync", "identity"];

export type RepoFlags = {
  dirty: boolean;
  behind: boolean;
  ahead: boolean;
  identityIssue: boolean;
  invalid: boolean;
};

export function identityMatches(
  actual: { name: string; email: string },
  preset: { userName: string; email: string }
): boolean {
  return (
    actual.name.trim().toLowerCase() === preset.userName.trim().toLowerCase() &&
    actual.email.trim().toLowerCase() === preset.email.trim().toLowerCase()
  );
}

export function findPreset(
  path: string,
  presets: GitRepoConfig[]
): GitRepoConfig | undefined {
  const key = pathKey(path);
  return presets.find((c) => pathKey(c.path) === key);
}

export function repoFlags(
  s: RepoBatchItem | undefined,
  preset: GitRepoConfig | undefined
): RepoFlags {
  if (!s) return { dirty: false, behind: false, ahead: false, identityIssue: false, invalid: false };
  if (!s.isGit) return { dirty: false, behind: false, ahead: false, identityIssue: false, invalid: true };
  const actual = { name: s.userName || "", email: s.userEmail || "" };
  const noIdentity = !actual.name.trim() && !actual.email.trim();
  return {
    dirty: s.dirtyCount > 0,
    behind: s.behind > 0 && s.hasUpstream,
    ahead: isPushable(s),
    identityIssue: noIdentity || (!!preset && !identityMatches(actual, preset)),
    invalid: false,
  };
}

export function matchesFilter(flags: RepoFlags, filter: RepoFilter): boolean {
  switch (filter) {
    case "dirty":
      return flags.dirty;
    case "sync":
      return flags.behind || flags.ahead;
    case "identity":
      return flags.identityIssue;
    default:
      return true;
  }
}

function rank(flags: RepoFlags, isCurrent: boolean): number {
  if (isCurrent) return 0;
  if (flags.dirty) return 1;
  if (flags.behind || flags.ahead) return 2;
  return 3;
}

const displayName = (p: RecentProject) => p.name || projectNameFromPath(p.path);

/** Current repo → dirty → needs sync → the rest; ties by name. Returns a new array. */
export function sortProjects(
  projects: RecentProject[],
  summaries: Record<string, RepoBatchItem>,
  presets: GitRepoConfig[],
  currentPath: string | undefined
): RecentProject[] {
  const current = currentPath ? pathKey(currentPath) : null;
  const keyed = projects.map((p) => ({
    p,
    r: rank(repoFlags(summaries[p.path], findPreset(p.path, presets)), pathKey(p.path) === current),
    n: displayName(p).toLowerCase(),
  }));
  keyed.sort((a, b) => a.r - b.r || a.n.localeCompare(b.n));
  return keyed.map((k) => k.p);
}

export type GroupSummary = { dirty: number; behind: number; ahead: number; identity: number };

export function summarizeGroup(
  projects: RecentProject[],
  summaries: Record<string, RepoBatchItem>,
  presets: GitRepoConfig[]
): GroupSummary {
  const out: GroupSummary = { dirty: 0, behind: 0, ahead: 0, identity: 0 };
  for (const p of projects) {
    const f = repoFlags(summaries[p.path], findPreset(p.path, presets));
    if (f.dirty) out.dirty++;
    if (f.behind) out.behind++;
    if (f.ahead) out.ahead++;
    if (f.identityIssue) out.identity++;
  }
  return out;
}

/** Per-filter counts over the whole list (search does not narrow them). */
export function countByFilter(
  projects: RecentProject[],
  summaries: Record<string, RepoBatchItem>,
  presets: GitRepoConfig[]
): Record<RepoFilter, number> {
  const out: Record<RepoFilter, number> = { all: projects.length, dirty: 0, sync: 0, identity: 0 };
  for (const p of projects) {
    const f = repoFlags(summaries[p.path], findPreset(p.path, presets));
    if (matchesFilter(f, "dirty")) out.dirty++;
    if (matchesFilter(f, "sync")) out.sync++;
    if (matchesFilter(f, "identity")) out.identity++;
  }
  return out;
}
