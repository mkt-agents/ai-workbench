import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  ClipboardList,
  FolderOpen,
  FolderPlus,
  GitBranch,
  GitCommitHorizontal,
  Loader2,
  RefreshCw,
  Search,
  Star,
  Trash2,
  User,
  Download,
  Upload,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import { GIT_CONCURRENCY, mapPoolCounted } from "../core/asyncPool";
import {
  dropRepos,
  getRepoSnapshot,
  holdRepos,
  invalidateRepos,
  refreshRepos,
  releaseRepos,
  reposLoaded,
  subscribeRepos,
} from "../core/gitCache";
import { describePushTargets, isPushable } from "../core/gitPushScope";
import { readStoredArray, writeStoredArray } from "../core/localState";
import { findWorkspaceForRepo, pathKey, projectNameFromPath } from "../core/pathUtils";
import { resolveRepoAccount } from "../core/gitIdentity";
import { useConfirm } from "./ConfirmModal";
import AccountManagerModal from "./AccountManagerModal";
import BatchIdentityModal from "./BatchIdentityModal";
import RepoBindingModal from "./RepoBindingModal";
import ScanReposModal from "./ScanReposModal";
import TestReportModal from "./TestReportModal";
import type { GitAccount, GitWorkspace, RecentProject, RepoBatchItem } from "../core/types";

type Props = {
  active?: boolean;
  onOpenCommit?: () => void;
};

const REFRESH_TTL_MS = 30_000;
const REPOS_COLLAPSED_KEY = "workbench-git-collapsed-groups";

function identityMatches(
  actual: { name: string; email: string },
  preset: { userName: string; email: string }
): boolean {
  return (
    actual.name.trim().toLowerCase() === preset.userName.trim().toLowerCase() &&
    actual.email.trim().toLowerCase() === preset.email.trim().toLowerCase()
  );
}

type RepoGroup = {
  key: string;
  workspace: GitWorkspace | null;
  projects: RecentProject[];
};

function GroupSelectCheckbox({
  checked,
  indeterminate,
  disabled,
  onChange,
  ariaLabel,
}: {
  checked: boolean;
  indeterminate: boolean;
  disabled?: boolean;
  onChange: () => void;
  ariaLabel: string;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate && !checked;
  }, [indeterminate, checked]);
  return (
    <input
      ref={ref}
      type="checkbox"
      checked={checked}
      disabled={disabled}
      onChange={onChange}
      aria-label={ariaLabel}
    />
  );
}

function GitReposPage({ active = true, onOpenCommit }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();

  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const accounts = useGlobalStore((s) => s.git.accounts);
  const repoConfigs = useGlobalStore((s) => s.git.repoConfigs);
  const currentGitRepo = useGlobalStore((s) => s.settings.currentGitRepo);
  const loadRecentProjects = useGlobalStore((s) => s.loadRecentProjects);
  const loadWorkspaces = useGlobalStore((s) => s.loadWorkspaces);
  const addRecentProject = useGlobalStore((s) => s.addRecentProject);
  const addWorkspace = useGlobalStore((s) => s.addWorkspace);
  const removeWorkspace = useGlobalStore((s) => s.removeWorkspace);
  const removeRecentProject = useGlobalStore((s) => s.removeRecentProject);
  const loadRepoConfigs = useGlobalStore((s) => s.loadRepoConfigs);
  const loadHostConfigs = useGlobalStore((s) => s.loadHostConfigs);
  const loadAccounts = useGlobalStore((s) => s.loadAccounts);
  const upsertRepoConfigs = useGlobalStore((s) => s.upsertRepoConfigs);
  const deleteRepoConfig = useGlobalStore((s) => s.deleteRepoConfig);
  const setCurrentGitRepo = useGlobalStore((s) => s.setCurrentGitRepo);
  const invokePickDirectory = useGlobalStore((s) => s.invokePickDirectory);
  const invokeGitIsRepo = useGlobalStore((s) => s.invokeGitIsRepo);
  const invokeGitScanRepos = useGlobalStore((s) => s.invokeGitScanRepos);
  const invokeGitPush = useGlobalStore((s) => s.invokeGitPush);
  const invokeGitPull = useGlobalStore((s) => s.invokeGitPull);
  const invokeSetRepoGitConfig = useGlobalStore((s) => s.invokeSetRepoGitConfig);
  const invokeOpenRuntimeFolder = useGlobalStore((s) => s.invokeOpenRuntimeFolder);

  // Shared with the commit page: one read per repo, cache-first rendering.
  const summaries = useSyncExternalStore(subscribeRepos, getRepoSnapshot);
  const identities = useMemo(() => {
    const map: Record<string, { name: string; email: string }> = {};
    for (const s of Object.values(summaries)) {
      if (s.isGit) map[s.path] = { name: s.userName || "", email: s.userEmail || "" };
    }
    return map;
  }, [summaries]);
  const [repoQuery, setRepoQuery] = useState("");
  const [loading, setLoading] = useState(false);
  const [syncingPath, setSyncingPath] = useState<string | null>(null);
  const [applyingPath, setApplyingPath] = useState<string | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [bindingPath, setBindingPath] = useState<string | null>(null);
  const [testReportPath, setTestReportPath] = useState<string | null>(null);
  const [testReportFolder, setTestReportFolder] = useState<{ name: string; repoPaths: string[] } | null>(null);
  const [showAccounts, setShowAccounts] = useState(false);
  const [scanState, setScanState] = useState<{
    rootPath: string;
    repos: { path: string; name: string }[];
  } | null>(null);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [collapsed, setCollapsed] = useState<Set<string>>(
    () => new Set(readStoredArray<string>(REPOS_COLLAPSED_KEY))
  );
  const [scanningPath, setScanningPath] = useState<string | null>(null);
  const [showBatchBind, setShowBatchBind] = useState(false);
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchProgress, setBatchProgress] = useState<{ done: number; total: number } | null>(null);

  const lastRefreshAtRef = useRef(0);
  const hasLoadedRef = useRef(false);

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), type === "error" ? 8000 : 3000);
  }, []);

  const refreshPaths = useCallback(
    async (paths: { path: string; name?: string }[], options?: { force?: boolean }) => {
      const list = paths.map((p) => p.path);
      if (options?.force) invalidateRepos(list);
      // Errors arrive per item from the batch command; nothing to catch here
      // beyond a transport failure, which must not blank the whole list.
      await refreshRepos(list).catch(() => {});
    },
    []
  );

  /**
   * For repos without a path-level binding, check if their cached origin remote
   * matches a host config and auto-apply that account's identity.
   * Silent by design, but reported once so the write is not invisible.
   *
   * Reads the URL straight from the batch summary — this used to spawn one
   * `git remote get-url` per repo on every refresh.
   */
  const attemptedHostApplies = useRef<Set<string>>(new Set());

  const autoApplyHostIdentities = useCallback(async (): Promise<number> => {
    const { git } = useGlobalStore.getState();
    if (git.hostConfigs.length === 0) return 0;
    const snapshot = getRepoSnapshot();
    const changed: string[] = [];

    for (const [path, item] of Object.entries(snapshot)) {
      // Skip if a path-level binding already exists
      if (!item.isGit || !item.originUrl) continue;
      if (git.repoConfigs.some((c) => pathKey(c.path) === pathKey(path))) continue;
      const account = resolveRepoAccount({
        repoPath: path,
        remoteUrl: item.originUrl,
        repoConfigs: git.repoConfigs,
        hostConfigs: git.hostConfigs,
        accounts: git.accounts,
      });
      if (!account) continue;
      // Remember the attempt: a failed write must not be retried every refresh.
      const key = `${path}|${account.id}`;
      if (attemptedHostApplies.current.has(key)) continue;
      attemptedHostApplies.current.add(key);
      try {
        await invokeSetRepoGitConfig(path, account.name, account.email);
        invalidateRepos([path]);
        changed.push(path);
      } catch {
        attemptedHostApplies.current.delete(key);
      }
    }
    return changed.length;
  }, [invokeSetRepoGitConfig]);

  const refresh = useCallback(
    async (opts?: { force?: boolean }) => {
      const force = opts?.force ?? false;
      const now = Date.now();
      if (!force && hasLoadedRef.current && now - lastRefreshAtRef.current < REFRESH_TTL_MS) {
        return;
      }
      // Cache-first: the very first load has nothing to show, and an explicit
      // refresh still spins so the click is acknowledged.
      if (!reposLoaded() || force) setLoading(true);
      try {
        await Promise.all([loadRecentProjects(), loadRepoConfigs(), loadHostConfigs(), loadWorkspaces(), loadAccounts()]);
        const projects = useGlobalStore.getState().recentProjects;
        if (force) invalidateRepos(projects.map((p) => p.path));
        await refreshPaths(projects);
        lastRefreshAtRef.current = Date.now();
        hasLoadedRef.current = true;

        const applied = await autoApplyHostIdentities();
        if (applied > 0) {
          showMsg("success", t("repos.autoApplied", { count: applied }));
          // Only the rewritten repos are still marked stale here.
          await refreshPaths(projects);
        }
      } finally {
        setLoading(false);
      }
    },
    [loadAccounts, loadRecentProjects, loadRepoConfigs, loadHostConfigs, loadWorkspaces, refreshPaths, autoApplyHostIdentities, showMsg, t]
  );

  useEffect(() => {
    if (active) void refresh();
  }, [active, refresh]);

  // Keep collapsed groups across page switches / app restarts.
  useEffect(() => {
    writeStoredArray(REPOS_COLLAPSED_KEY, [...collapsed]);
  }, [collapsed]);

  const groups: RepoGroup[] = useMemo(() => {
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
  }, [gitWorkspaces, recentProjects, repoQuery]);

  const toggleCollapsed = (key: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const toggleSelected = (path: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const toggleGroupSelection = (projects: RecentProject[]) => {
    if (projects.length === 0) return;
    setSelected((prev) => {
      const next = new Set(prev);
      const allSelected = projects.every((p) => next.has(p.path));
      if (allSelected) {
        for (const p of projects) next.delete(p.path);
      } else {
        for (const p of projects) next.add(p.path);
      }
      return next;
    });
  };

  const clearSelected = () => setSelected(new Set());

  const runBatch = async (
    paths: string[],
    fn: (path: string) => Promise<void>
  ) => {
    if (batchBusy || paths.length === 0) return;
    // Hold the cache so a background refresh cannot race the writes below.
    holdRepos(paths);
    setBatchBusy(true);
    setBatchProgress({ done: 0, total: paths.length });
    let ok = 0;
    let fail = 0;
    try {
      const result = await mapPoolCounted(paths, GIT_CONCURRENCY, fn, {
        onProgress: (done, total) => setBatchProgress({ done, total }),
      });
      ok = result.ok;
      fail = result.fail;
    } finally {
      releaseRepos(paths);
      setBatchBusy(false);
      setBatchProgress(null);
    }
    showMsg(fail > 0 ? "error" : "success", t("batch.done", { ok, fail }));
    // The writes landed after these paths were last read, so force a re-read.
    await refreshPaths(
      paths.map((path) => {
        const p = recentProjects.find((x) => x.path === path);
        return { path, name: p?.name };
      }),
      { force: true }
    );
  };

  const handleBatchBind = async (account: GitAccount) => {
    const paths = [...selected];
    setShowBatchBind(false);
    const bound: string[] = [];
    await runBatch(paths, async (path) => {
      await invokeSetRepoGitConfig(path, account.name, account.email);
      bound.push(path);
    });
    if (bound.length === 0) return;
    try {
      await upsertRepoConfigs(
        bound.map((path) => ({
          path,
          name: projectNameFromPath(path),
          userName: account.name,
          email: account.email,
          accountId: account.id,
        }))
      );
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleBatchPull = () => {
    const paths = [...selected].filter((path) => {
      const s = summaries[path];
      return !!s?.isGit && s.behind > 0 && s.hasUpstream;
    });
    if (paths.length === 0) {
      showMsg("error", t("batch.noneToPull"));
      return;
    }
    void runBatch(paths, async (path) => {
      void (await invokeGitPull(path));
    });
  };

  const handleBatchPush = async () => {
    const paths = [...selected].filter((path) => isPushable(summaries[path]));
    if (paths.length === 0) {
      showMsg("error", t("batch.noneToPush"));
      return;
    }
    // A no-upstream repo gets `push -u origin HEAD`, i.e. a brand new remote
    // branch — never do that silently for a whole selection.
    const rows = paths
      .map((path) => ({
        label: recentProjects.find((p) => p.path === path)?.name || projectNameFromPath(path),
        summary: summaries[path],
      }))
      .filter((row): row is { label: string; summary: RepoBatchItem } => !!row.summary);
    const { message, warning } = describePushTargets(rows, t);
    const ok = await confirm({
      title: t("batch.pushTitle"),
      message,
      warning,
      confirmText: t("batch.push"),
      icon: warning ? "warning" : "info",
    });
    if (!ok) return;
    void runBatch(paths, async (path) => {
      void (await invokeGitPush(path));
    });
  };

  const handleBatchRefresh = () => {
    const paths = [...selected];
    if (batchBusy || paths.length === 0) return;
    setBatchBusy(true);
    setBatchProgress({ done: 0, total: paths.length });
    void (async () => {
      try {
        // One batched read for the whole selection, not one per repo.
        invalidateRepos(paths);
        await refreshRepos(paths);
        setBatchProgress({ done: paths.length, total: paths.length });
        showMsg("success", t("batch.done", { ok: paths.length, fail: 0 }));
      } finally {
        setBatchBusy(false);
      }
    })();
  };

  /** Remove the selected repos from the workbench list (never touches the disk). */
  const handleBatchRemove = async () => {
    const paths = [...selected];
    if (batchBusy || paths.length === 0) return;
    const ok = await confirm({
      title: t("batch.removeTitle"),
      message: t("batch.removeMessage", { count: paths.length }),
      warning: t("batch.removeWarning"),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      for (const path of paths) {
        const p = recentProjects.find((x) => x.path === path);
        if (p) await removeRecentProject(p.id);
        if (currentGitRepo === path) setCurrentGitRepo(undefined);
      }
      dropRepos(paths);
      setSelected(new Set());
      showMsg("success", t("batch.removed", { count: paths.length }));
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  /**
   * A directory scan can take a few seconds on a cold network drive, so the
   * trigger shows progress and cannot be pressed twice.
   */
  const startScan = async (source: "add" | string) => {
    if (scanningPath) return;
    setScanningPath(source === "add" ? "__add__" : source);
    try {
      if (source === "add") {
        const dir = await invokePickDirectory();
        if (!dir) return;
        if (await invokeGitIsRepo(dir)) {
          await addRecentProject({ path: dir, name: projectNameFromPath(dir) });
          if (!currentGitRepo) setCurrentGitRepo(dir);
          showMsg("success", t("workbench.added"));
          // Only the new entry needs a git read — the rest of the list is cached.
          await refreshOne(dir);
          return;
        }
        const found = await invokeGitScanRepos(dir, 1);
        if (found.length === 0) {
          showMsg("error", t("workbench.notGitRepo"));
          return;
        }
        setScanState({ rootPath: dir, repos: found });
        return;
      }
      const found = await invokeGitScanRepos(source, 1);
      if (found.length === 0) {
        showMsg("error", t("workbench.notGitRepo"));
        return;
      }
      setScanState({ rootPath: source, repos: found });
    } catch (e) {
      showMsg("error", String(e));
    } finally {
      setScanningPath(null);
    }
  };

  const handleRemoveWorkspace = async (ws: GitWorkspace) => {
    const ok = await confirm({
      title: t("workspace.removeTitle"),
      message: t("workspace.removeConfirm", { name: ws.name }),
      confirmText: t("workspace.remove"),
      icon: "danger",
    });
    if (!ok) return;
    await removeWorkspace(ws.id);
    showMsg("success", t("workspace.removed"));
  };

  const handleRemove = async (id: number, path: string) => {
    const ok = await confirm({
      title: t("workbench.removeTitle"),
      message: t("workbench.removeConfirm", { name: projectNameFromPath(path) }),
      confirmText: t("workbench.remove"),
      icon: "danger",
    });
    if (!ok) return;
    await removeRecentProject(id);
    dropRepos([path]);
    setSelected((prev) => {
      const next = new Set(prev);
      next.delete(path);
      return next;
    });
    if (currentGitRepo === path) setCurrentGitRepo(undefined);
    showMsg("success", t("workbench.removed"));
  };

  const handleSelect = (path: string) => {
    setCurrentGitRepo(path);
    showMsg("success", t("workbench.selected"));
  };

  const handleOpenFolder = async (path: string) => {
    try {
      await invokeOpenRuntimeFolder(path);
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  /** Re-read one card after a write instead of hammering every repo again. */
  const refreshOne = useCallback(async (path: string) => {
    invalidateRepos([path]);
    await refreshRepos([path]).catch(() => {});
  }, []);

  const handlePush = async (path: string) => {
    if (syncingPath) return;
    setSyncingPath(path);
    try {
      await invokeGitPush(path);
      showMsg("success", t("workbench.pushOk"));
      await refreshOne(path);
    } catch (e) {
      showMsg("error", String(e));
      await refreshOne(path);
    } finally {
      setSyncingPath(null);
    }
  };

  const handlePull = async (path: string) => {
    if (syncingPath) return;
    setSyncingPath(path);
    try {
      await invokeGitPull(path);
      showMsg("success", t("workbench.pullOk"));
      await refreshOne(path);
    } catch (e) {
      showMsg("error", String(e));
      await refreshOne(path);
    } finally {
      setSyncingPath(null);
    }
  };

  const handleApplyPreset = async (path: string, userName: string, email: string) => {
    if (applyingPath) return;
    setApplyingPath(path);
    try {
      await invokeSetRepoGitConfig(path, userName, email);
      showMsg("success", t("repos.applied"));
      await refreshOne(path);
    } catch (e) {
      showMsg("error", t("repos.applyFailed", { error: String(e) }));
    } finally {
      setApplyingPath(null);
    }
  };

  const handleUnbind = async (path: string, name: string) => {
    const ok = await confirm({
      title: t("repos.unbindTitle"),
      message: t("repos.unbindConfirm", { name }),
      confirmText: tc("actions.delete"),
      icon: "warning",
    });
    if (!ok) return;
    try {
      await deleteRepoConfig(path);
      showMsg("success", t("repos.unbound"));
      // Only the binding record changed; the repo's git state is untouched.
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const currentProject = currentGitRepo
    ? recentProjects.find((p) => pathKey(p.path) === pathKey(currentGitRepo))
    : undefined;
  const currentRepoName = currentProject
    ? currentProject.name || projectNameFromPath(currentProject.path)
    : currentGitRepo
      ? projectNameFromPath(currentGitRepo)
      : null;

  const renderCard = (p: RecentProject) => {
    const s = summaries[p.path];
    const isCurrent = !!currentGitRepo && pathKey(currentGitRepo) === pathKey(p.path);
    const actual = identities[p.path];
    const preset = repoConfigs.find((c) => pathKey(c.path) === pathKey(p.path));
    const mismatch = !!preset && !!actual && s?.isGit && !identityMatches(actual, preset);
    const isChecked = selected.has(p.path);

    return (
      <div key={p.path} className={`repos-card ${isCurrent ? "is-active" : ""}`} title={p.path}>
        <div className="repos-card-main">
          <label className="repos-card-check">
            <input
              type="checkbox"
              checked={isChecked}
              onChange={() => toggleSelected(p.path)}
              disabled={batchBusy}
            />
          </label>
          <div className="repos-card-info">
            <div className="repos-card-title">
              <span className="runtime-ver">{p.name || projectNameFromPath(p.path)}</span>
              {isCurrent && <span className="runtime-badge active">{t("workbench.current")}</span>}
              {s?.isGit && s.branch && (
                <span className="commit-branch-badge" title={s.branch}>
                  <GitBranch size={12} />
                  <span className="commit-branch-text">{s.branch}</span>
                </span>
              )}
              {s && s.isGit && (
                <span className="repos-status-pill">
                  {s.dirtyCount === 0
                    ? t("workbench.clean")
                    : t("workbench.dirty", { count: s.dirtyCount })}
                </span>
              )}
              {s && s.isGit && (s.ahead > 0 || s.behind > 0) && (
                <span className="commit-sync-badge">
                  ↑{s.ahead} ↓{s.behind}
                </span>
              )}
              {s && !s.isGit && (
                <span className="runtime-badge machine">{t("workbench.invalid")}</span>
              )}
            </div>
            {s?.isGit && (
              <div className="repos-identity-row">
                <div className="repos-identity-info">
                  <User size={12} />
                  {actual && (actual.name || actual.email) ? (
                    <span className="repos-identity-text" title={`${actual.name} <${actual.email}>`}>
                      {actual.name}
                      {actual.email ? (
                        <span className="runtime-muted"> &lt;{actual.email}&gt;</span>
                      ) : null}
                    </span>
                  ) : (
                    <span className="runtime-muted">{t("repos.noIdentity")}</span>
                  )}
                </div>
                {preset ? (
                  <>
                    <button
                      type="button"
                      className="repos-identity-link"
                      onClick={() => setBindingPath(p.path)}
                      title={t("repos.editBinding")}
                    >
                      {t("repos.bound", { name: preset.userName })}
                    </button>
                    <button
                      type="button"
                      className="repos-identity-link is-danger"
                      onClick={() => handleUnbind(p.path, preset.name)}
                      title={t("repos.unbind")}
                    >
                      {t("repos.unbind")}
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    className="repos-identity-link"
                    onClick={() => setBindingPath(p.path)}
                  >
                    {t("repos.bind")}
                  </button>
                )}
              </div>
            )}
            {s?.error && <div className="runtime-muted">{s.error}</div>}
            {mismatch && preset && (
              <div className="repos-mismatch">
                <AlertTriangle size={12} />
                <span className="repos-mismatch-text">
                  {t("repos.mismatch", {
                    preset: `${preset.userName} <${preset.email}>`,
                  })}
                </span>
                <button
                  type="button"
                  className="btn btn-primary btn-small"
                  disabled={!!applyingPath || batchBusy}
                  onClick={() => handleApplyPreset(p.path, preset.userName, preset.email)}
                >
                  {applyingPath === p.path ? (
                    <Loader2 size={12} className="spin" />
                  ) : (
                    t("repos.applyPreset")
                  )}
                </button>
              </div>
            )}
          </div>
          <div className="repos-card-actions">
            <button
              type="button"
              className="btn commit-icon-btn"
              title={t("workbench.openFolder")}
              aria-label={t("workbench.openFolder")}
              onClick={() => handleOpenFolder(p.path)}
            >
              <FolderOpen size={14} />
            </button>
            {s && s.isGit && s.behind > 0 && s.hasUpstream && (
              <button
                type="button"
                className="btn commit-icon-btn"
                disabled={!!syncingPath || batchBusy}
                title={t("workbench.pullTitle", { count: s.behind })}
                onClick={() => handlePull(p.path)}
              >
                {syncingPath === p.path ? <Loader2 size={14} className="spin" /> : <Download size={14} />}
              </button>
            )}
            {s && s.isGit && (s.ahead > 0 || !s.hasUpstream) && (
              <button
                type="button"
                className="btn commit-icon-btn"
                disabled={!!syncingPath || batchBusy}
                title={
                  s.ahead > 0
                    ? t("workbench.pushTitle", { count: s.ahead })
                    : t("workbench.pushNoUpstream")
                }
                onClick={() => handlePush(p.path)}
              >
                {syncingPath === p.path ? <Loader2 size={14} className="spin" /> : <Upload size={14} />}
              </button>
            )}
            <button
              type="button"
              className="btn commit-icon-btn commit-icon-btn-primary"
              disabled={isCurrent || (s ? !s.isGit : false)}
              title={t("workbench.use")}
              onClick={() => handleSelect(p.path)}
            >
              <Star size={14} />
            </button>
            {isCurrent && onOpenCommit && (
              <button
                type="button"
                className="btn commit-icon-btn"
                title={t("workbench.goCommit")}
                aria-label={t("workbench.goCommit")}
                onClick={onOpenCommit}
              >
                <GitCommitHorizontal size={14} />
              </button>
            )}
            {s?.isGit && (
              <button
                type="button"
                className="btn commit-icon-btn"
                title={t("testReport.open")}
                aria-label={t("testReport.open")}
                onClick={() => setTestReportPath(p.path)}
              >
                <ClipboardList size={14} />
              </button>
            )}
            <button
              type="button"
              className="btn commit-icon-btn"
              title={t("workbench.remove")}
              onClick={() => handleRemove(p.id, p.path)}
            >
              <Trash2 size={14} />
            </button>
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className="page-scrollable git-workbench">
      <div className="repos-toolbar">
        <div className="repos-toolbar-primary">
          <div className="repos-toolbar-context">
            <div className="repos-toolbar-context-row">
              {currentGitRepo && currentRepoName ? (
                <>
                  <span className="runtime-badge active">{t("workbench.current")}</span>
                  <span className="repos-toolbar-name" title={currentGitRepo}>
                    {currentRepoName}
                  </span>
                </>
              ) : (
                <span className="runtime-muted">{t("workbench.noCurrent")}</span>
              )}
            </div>
            <p className="repos-toolbar-subtitle">{t("repos.subtitle")}</p>
          </div>
          <div className="repos-toolbar-actions">
            <div className="repos-search">
              <Search size={12} aria-hidden />
              <input
                type="search"
                value={repoQuery}
                onChange={(e) => setRepoQuery(e.target.value)}
                placeholder={t("repos.searchRepos")}
                aria-label={t("repos.searchRepos")}
              />
            </div>
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={() => void refresh({ force: true })}
              disabled={loading || batchBusy}
              title={t("workbench.refresh")}
            >
              {loading ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
              <span>{t("workbench.refresh")}</span>
            </button>
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={() => setShowAccounts(true)}
              title={t("repos.manageAccounts")}
            >
              <User size={14} />
              <span>{t("repos.manageAccounts")}</span>
            </button>
            <button
              type="button"
              className="btn commit-icon-btn commit-icon-btn-primary"
              onClick={() => void startScan("add")}
              disabled={batchBusy || scanningPath !== null}
              title={t("workbench.addRepo")}
            >
              {scanningPath === "__add__" ? (
                <Loader2 size={14} className="spin" />
              ) : (
                <FolderPlus size={14} />
              )}
              <span>{t("workbench.addRepo")}</span>
            </button>
          </div>
        </div>
      </div>

      {selected.size > 0 && (
        <div className="repos-batch-bar">
          <span className="repos-batch-count">{t("batch.selected", { count: selected.size })}</span>
          {batchProgress && (
            <span className="runtime-muted">
              {t("batch.running", { done: batchProgress.done, total: batchProgress.total })}
            </span>
          )}
          <div className="repos-batch-actions">
            <button
              type="button"
              className="btn commit-icon-btn"
              disabled={batchBusy || accounts.length === 0}
              onClick={() => setShowBatchBind(true)}
            >
              <User size={14} />
              <span>{t("batch.bind")}</span>
            </button>
            <button
              type="button"
              className="btn commit-icon-btn"
              disabled={batchBusy}
              onClick={handleBatchPull}
            >
              <Download size={14} />
              <span>{t("batch.pull")}</span>
            </button>
            <button
              type="button"
              className="btn commit-icon-btn"
              disabled={batchBusy}
              onClick={() => void handleBatchPush()}
            >
              <Upload size={14} />
              <span>{t("batch.push")}</span>
            </button>
            <button
              type="button"
              className="btn commit-icon-btn"
              disabled={batchBusy}
              onClick={handleBatchRefresh}
            >
              <RefreshCw size={14} />
              <span>{t("batch.refresh")}</span>
            </button>
            <button
              type="button"
              className="btn commit-icon-btn"
              disabled={batchBusy}
              onClick={() => void handleBatchRemove()}
              title={t("batch.removeTitle")}
            >
              <Trash2 size={14} />
              <span>{t("batch.remove")}</span>
            </button>
            <button
              type="button"
              className="btn repos-batch-clear"
              disabled={batchBusy}
              onClick={clearSelected}
              title={t("batch.clear")}
            >
              <XCircle size={14} />
              <span>{t("batch.clear")}</span>
            </button>
          </div>
        </div>
      )}

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={14} /> : <XCircle size={14} />}
          </span>
          <span className="toast-text">{message.text}</span>
        </div>
      )}

      {!hasLoadedRef.current ? (
        <div className="runtime-empty">
          <Loader2 size={18} className="spin" />
          <div className="runtime-muted">{tc("status.loading")}</div>
        </div>
      ) : recentProjects.length === 0 && gitWorkspaces.length === 0 ? (
        <div className="runtime-empty">
          <div>{t("workbench.empty")}</div>
          <div className="runtime-muted">{t("workbench.emptyHint")}</div>
        </div>
      ) : groups.every((g) => g.projects.length === 0) ? (
        <div className="runtime-empty">
          <div>{t("repos.noMatch")}</div>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            style={{ marginTop: 10 }}
            onClick={() => setRepoQuery("")}
          >
            {t("repos.clearSearch")}
          </button>
        </div>
      ) : (
        <div className="runtime-list repos-list">
          {groups.map((g) => {
            const isCollapsed = collapsed.has(g.key);
            const title = g.workspace ? g.workspace.name : t("workspace.other");
            const Chevron = isCollapsed ? ChevronRight : ChevronDown;
            const selectedInGroup = g.projects.filter((p) => selected.has(p.path)).length;
            const groupAllSelected =
              g.projects.length > 0 && selectedInGroup === g.projects.length;
            const groupPartialSelected =
              selectedInGroup > 0 && selectedInGroup < g.projects.length;
            const gitRepoPaths = g.projects.filter((p) => summaries[p.path]?.isGit).map((p) => p.path);
            return (
              <div key={g.key} className="repos-group">
                <div className="repos-group-header">
                  {g.projects.length > 0 && (
                    <label
                      className="repos-group-check"
                      title={t("workspace.selectGroup")}
                      onClick={(e) => e.stopPropagation()}
                    >
                      <GroupSelectCheckbox
                        checked={groupAllSelected}
                        indeterminate={groupPartialSelected}
                        disabled={batchBusy}
                        onChange={() => toggleGroupSelection(g.projects)}
                        ariaLabel={t("workspace.selectGroup")}
                      />
                    </label>
                  )}
                  <button
                    type="button"
                    className="repos-group-toggle"
                    onClick={() => toggleCollapsed(g.key)}
                    title={title}
                  >
                    <Chevron size={14} />
                    <span className="repos-group-title">{title}</span>
                    <span className="runtime-muted">({g.projects.length})</span>
                  </button>
                  {gitRepoPaths.length > 0 && (
                    <button
                      type="button"
                      className="repos-group-admin-btn"
                      disabled={batchBusy}
                      title={t("testReport.openFolder", { count: gitRepoPaths.length })}
                      aria-label={t("testReport.openFolder", { count: gitRepoPaths.length })}
                      onClick={() => setTestReportFolder({ name: title, repoPaths: gitRepoPaths })}
                    >
                      <ClipboardList size={13} />
                    </button>
                  )}
                  {g.workspace && (
                    <span className="repos-group-path" title={g.workspace.path}>
                      {g.workspace.path}
                    </span>
                  )}
                  {g.workspace && (
                    <div className="repos-group-admin">
                      <button
                        type="button"
                        className="repos-group-admin-btn"
                        disabled={batchBusy || scanningPath !== null}
                        title={t("workspace.rescan")}
                        aria-label={t("workspace.rescan")}
                        onClick={() => void startScan(g.workspace!.path)}
                      >
                        {scanningPath === g.workspace.path ? (
                          <Loader2 size={13} className="spin" />
                        ) : (
                          <RefreshCw size={13} />
                        )}
                      </button>
                      <button
                        type="button"
                        className="repos-group-admin-btn is-danger"
                        disabled={batchBusy}
                        title={t("workspace.remove")}
                        aria-label={t("workspace.remove")}
                        onClick={() => void handleRemoveWorkspace(g.workspace!)}
                      >
                        <Trash2 size={13} />
                      </button>
                    </div>
                  )}
                </div>
                {!isCollapsed && (
                  <div className="repos-group-body">
                    {g.projects.length === 0 ? (
                      <div className="runtime-muted repos-group-empty">{t("workbench.empty")}</div>
                    ) : (
                      g.projects.map(renderCard)
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      {bindingPath !== null && (
        <RepoBindingModal
          repoPath={bindingPath}
          onClose={() => setBindingPath(null)}
          onSaved={() => {
            setBindingPath(null);
            void refresh({ force: true });
          }}
        />
      )}

      {testReportPath !== null && (
        <TestReportModal repoPath={testReportPath} onClose={() => setTestReportPath(null)} />
      )}

      {testReportFolder !== null && (
        <TestReportModal
          folderName={testReportFolder.name}
          repoPaths={testReportFolder.repoPaths}
          onClose={() => setTestReportFolder(null)}
        />
      )}

      {showAccounts && (
        <AccountManagerModal
          onClose={() => {
            setShowAccounts(false);
            void refresh({ force: true });
          }}
        />
      )}

      {showBatchBind && (
        <BatchIdentityModal
          selectedCount={selected.size}
          onClose={() => setShowBatchBind(false)}
          onPick={handleBatchBind}
        />
      )}

      {scanState && (
        <ScanReposModal
          rootPath={scanState.rootPath}
          initialRepos={scanState.repos}
          onClose={() => setScanState(null)}
          onAdded={(addedCount) => {
            const root = scanState.rootPath;
            setScanState(null);
            void addWorkspace(root).then(() => {
              showMsg("success", t("scan.addedN", { count: addedCount }));
              void refresh({ force: true });
            });
          }}
        />
      )}
    </div>
  );
}

export default GitReposPage;
