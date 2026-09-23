/**
 * The "报告" sub-tab: a workspace-grouped repo tree on the left, the regression
 * report panel on the right. Selecting a workspace node = a merged folder report;
 * selecting a repo node = a single-repo report. The panel instance is never
 * remounted on selection change — it resets itself via its target key, so a slow
 * collect/AI from the previous target can't bleed into the new one.
 */
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronRight, Folder, GitBranch } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { getRepoSnapshot, refreshRepos, subscribeRepos } from "../core/gitCache";
import { buildRepoGroups, REPOS_COLLAPSED_KEY } from "../core/repoGroups";
import { readStoredArray, writeStoredArray } from "../core/localState";
import TestReportPanel from "./TestReportPanel";
import type { ReportTarget } from "./testReportTypes";
import "./GitReportsPage.css";

type Props = {
  active?: boolean;
  target: ReportTarget | null;
  onTargetChange: (t: ReportTarget) => void;
};

function sameTarget(a: ReportTarget | null, b: ReportTarget): boolean {
  if (!a) return false;
  if (a.kind === "repo" && b.kind === "repo") return a.path === b.path;
  if (a.kind === "folder" && b.kind === "folder")
    return a.name === b.name && a.paths.length === b.paths.length && a.paths.every((p, i) => p === b.paths[i]);
  return false;
}

export default function GitReportsPage({ active = true, target, onTargetChange }: Props) {
  const { t } = useTranslation("git");

  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const loadRecentProjects = useGlobalStore((s) => s.loadRecentProjects);
  const loadWorkspaces = useGlobalStore((s) => s.loadWorkspaces);
  const summaries = useSyncExternalStore(subscribeRepos, getRepoSnapshot);

  const [collapsed, setCollapsed] = useState<Set<string>>(
    () => new Set(readStoredArray<string>(REPOS_COLLAPSED_KEY))
  );
  useEffect(() => {
    writeStoredArray(REPOS_COLLAPSED_KEY, [...collapsed]);
  }, [collapsed]);

  // Ensure the tree has data + git flags when opened. Cache-first; refreshRepos
  // has a short TTL so re-runs are cheap. No host-identity writes (that's the
  // repos page's job).
  useEffect(() => {
    if (!active) return;
    if (recentProjects.length === 0) void loadRecentProjects().catch(() => {});
    if (gitWorkspaces.length === 0) void loadWorkspaces().catch(() => {});
    const paths = recentProjects.map((p) => p.path);
    if (paths.length > 0) void refreshRepos(paths).catch(() => {});
  }, [active, recentProjects.length, gitWorkspaces.length, loadRecentProjects, loadWorkspaces]);

  const groups = useMemo(() => buildRepoGroups(gitWorkspaces, recentProjects), [gitWorkspaces, recentProjects]);

  const toggleCollapsed = (key: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  return (
    <div className="git-reports-page">
      <div className="tr-split">
        <aside className="tr-tree">
          {groups.length === 0 && <div className="tr-tree-empty">{t("workbench.empty")}</div>}
          {groups.map((g) => {
            const isCollapsed = collapsed.has(g.key);
            const name = g.workspace ? g.workspace.name : t("workspace.other");
            const gitPaths = g.projects.filter((p) => summaries[p.path]?.isGit).map((p) => p.path);
            const folderTarget: ReportTarget = { kind: "folder", name, paths: gitPaths };
            const folderSelected = sameTarget(target, folderTarget);
            const Chevron = isCollapsed ? ChevronRight : ChevronDown;
            return (
              <div key={g.key} className="tr-tree-group">
                <div className={`tr-tree-folder${folderSelected ? " is-selected" : ""}`}>
                  <button
                    type="button"
                    className="tr-tree-caret"
                    onClick={() => toggleCollapsed(g.key)}
                    aria-label={isCollapsed ? "expand" : "collapse"}
                  >
                    <Chevron size={12} />
                  </button>
                  <button
                    type="button"
                    className="tr-tree-folder-label"
                    disabled={gitPaths.length === 0}
                    title={gitPaths.length === 0 ? t("reports.noGitInFolder") : t("reports.folderNode")}
                    onClick={() => onTargetChange(folderTarget)}
                  >
                    <Folder size={12} />
                    <span className="tr-tree-name">{name}</span>
                    <span className="tr-tree-count">{gitPaths.length}</span>
                  </button>
                </div>
                {!isCollapsed &&
                  g.projects.map((p) => {
                    const isGit = summaries[p.path]?.isGit;
                    const selected = target?.kind === "repo" && target.path === p.path;
                    return (
                      <button
                        key={p.path}
                        type="button"
                        className={`tr-tree-repo${selected ? " is-selected" : ""}`}
                        disabled={isGit !== true}
                        title={isGit === false ? t("reports.notGit") : p.path}
                        onClick={() => onTargetChange({ kind: "repo", path: p.path })}
                      >
                        <GitBranch size={11} />
                        <span className="tr-tree-name">{p.name}</span>
                      </button>
                    );
                  })}
              </div>
            );
          })}
        </aside>

        <div className="tr-report-main">
          {target ? (
            target.kind === "repo" ? (
              <TestReportPanel repoPath={target.path} />
            ) : (
              <TestReportPanel folderName={target.name} repoPaths={target.paths} />
            )
          ) : (
            <div className="tr-panel tr-panel-empty">
              <div className="tr-empty-title">{t("reports.empty")}</div>
              <div className="runtime-muted">{t("reports.emptyHint")}</div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
