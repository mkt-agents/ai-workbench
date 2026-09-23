/**
 * Shared workspace-grouping for the repo list and the regression-report tree.
 * Pure: folds recent projects into groups keyed by their owning workspace, with
 * an "other" catch-all for anything unassigned.
 */
import { findWorkspaceForRepo } from "./pathUtils";
import type { GitWorkspace, RecentProject } from "./types";

export type RepoGroup = {
  key: string;
  workspace: GitWorkspace | null;
  projects: RecentProject[];
};

/** localStorage key for collapsed group keys — shared by the repos page and the report tree. */
export const REPOS_COLLAPSED_KEY = "workbench-git-collapsed-groups";

export function buildRepoGroups(
  gitWorkspaces: GitWorkspace[],
  recentProjects: RecentProject[],
  repoQuery = ""
): RepoGroup[] {
  const q = repoQuery.trim().toLowerCase();
  const byWs = new Map<number, RecentProject[]>();
  const others: RecentProject[] = [];
  for (const p of recentProjects) {
    if (q && !`${p.name} ${p.path}`.toLowerCase().includes(q)) continue;
    const ws = findWorkspaceForRepo(p.path, gitWorkspaces);
    if (ws) {
      const list = byWs.get(ws.id) || [];
      list.push(p);
      byWs.set(ws.id, list);
    } else {
      others.push(p);
    }
  }
  const result: RepoGroup[] = [];
  for (const ws of gitWorkspaces) {
    result.push({
      key: `ws-${ws.id}`,
      workspace: ws,
      projects: byWs.get(ws.id) || [],
    });
  }
  if (others.length > 0 || gitWorkspaces.length === 0) {
    result.push({ key: "other", workspace: null, projects: others });
  }
  // While searching, groups without matches are just noise.
  return q ? result.filter((g) => g.projects.length > 0) : result;
}
