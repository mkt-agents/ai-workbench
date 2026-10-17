import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  ChevronsDownUp,
  ChevronsUpDown,
  ClipboardList,
  Copy,
  FolderOpen,
  FolderPlus,
  GitBranch,
  GitBranchPlus,
  Link2,
  Loader2,
  MoreHorizontal,
  Pin,
  RefreshCw,
  Search,
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
import { readStoredArray, writeStoredArray } from "../core/localState";
import { findWorkspaceForRepo, pathKey, projectNameFromPath } from "../core/pathUtils";
import { buildRepoGroups, REPOS_COLLAPSED_KEY, type RepoGroup } from "../core/repoGroups";
import { resolveRepoAccount } from "../core/gitIdentity";
import {
  countByFilter,
  findPreset,
  identityMatches,
  matchesFilter,
  REPO_FILTERS,
  repoFlags,
  sortProjects,
  summarizeGroup,
  type RepoFilter,
} from "../core/repoStatus";
import { useConfirm } from "./ConfirmModal";
import DropdownMenu from "./DropdownMenu";
import AccountManagerModal from "./AccountManagerModal";
import BatchIdentityModal from "./BatchIdentityModal";
import BatchBranchModal, { type BranchModalMode } from "./BatchBranchModal";
import RepoBindingModal from "./RepoBindingModal";
import ScanReposModal from "./ScanReposModal";
import type { ReportTarget } from "./testReportTypes";
import type { GitAccount, GitWorkspace, RecentProject, RepoBatchItem } from "../core/types";

type Props = {
  active?: boolean;
  onOpenCommit?: () => void;
  onOpenReport?: (target: ReportTarget) => void;
};

const REFRESH_TTL_MS = 30_000;
const REPO_FILTER_KEY = "workbench-git-repo-filter";

function readStoredFilter(): RepoFilter {
  try {
    const v = localStorage.getItem(REPO_FILTER_KEY);
    return REPO_FILTERS.includes(v as RepoFilter) ? (v as RepoFilter) : "all";
  } catch {
    return "all";
  }
}

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

function GitReposPage({ active = true, onOpenCommit, onOpenReport }: Props) {
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
  const invokeGitCreateBranch = useGlobalStore((s) => s.invokeGitCreateBranch);
  const invokeGitSwitchBranch = useGlobalStore((s) => s.invokeGitSwitchBranch);
  const invokeGitListBranches = useGlobalStore((s) => s.invokeGitListBranches);
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
  const batchLock = useRef(false);
  const [batchProgress, setBatchProgress] = useState<{
    done: number;
    total: number;
    indeterminate?: boolean;
  } | null>(null);
  const [branchModal, setBranchModal] = useState<BranchModalMode | null>(null);
  const [filter, setFilter] = useState<RepoFilter>(readStoredFilter);
  const [lastRefreshAt, setLastRefreshAt] = useState(0);
  const [now, setNow] = useState(() => Date.now());

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
        setLastRefreshAt(lastRefreshAtRef.current);
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

  useEffect(() => {
    try {
      localStorage.setItem(REPO_FILTER_KEY, filter);
    } catch {
      /* storage unavailable: the filter just won't persist */
    }
  }, [filter]);

  // Keeps the "refreshed N min ago" label moving while the page is visible.
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(id);
  }, [active, lastRefreshAt]);

  const filterCounts = useMemo(
    () => countByFilter(recentProjects, summaries, repoConfigs),
    [recentProjects, summaries, repoConfigs]
  );

  const groups: RepoGroup[] = useMemo(() => {
    const searched = buildRepoGroups(gitWorkspaces, recentProjects, repoQuery);
    const narrowing = filter !== "all";
    return searched
      .map((g) => ({
        ...g,
        projects: sortProjects(
          narrowing
            ? g.projects.filter((p) =>
                matchesFilter(repoFlags(summaries[p.path], findPreset(p.path, repoConfigs)), filter)
              )
            : g.projects,
          summaries,
          repoConfigs,
          currentGitRepo
        ),
      }))
      .filter((g) => !narrowing || g.projects.length > 0);
  }, [gitWorkspaces, recentProjects, repoQuery, filter, summaries, repoConfigs, currentGitRepo]);

  const narrowed = repoQuery.trim() !== "" || filter !== "all";
  const visiblePaths = useMemo(() => {
    const set = new Set<string>();
    for (const group of groups) {
      for (const project of group.projects) set.add(project.path);
    }
    return set;
  }, [groups]);
  /** Batch actions follow the list on screen. A filter must not silently include hidden repos. */
  const batchPaths = useMemo(
    () => [...selected].filter((path) => visiblePaths.has(path)),
    [selected, visiblePaths]
  );
  const hiddenSelected = selected.size - batchPaths.length;
  const allCollapsed = groups.length > 0 && groups.every((g) => collapsed.has(g.key));
  const toggleAllCollapsed = () =>
    setCollapsed(allCollapsed ? new Set() : new Set(groups.map((g) => g.key)));

  const clearNarrowing = () => {
    setRepoQuery("");
    setFilter("all");
  };

  const refreshedLabel = (() => {
    if (!lastRefreshAt) return null;
    const mins = Math.floor((now - lastRefreshAt) / 60_000);
    return mins < 1 ? t("repos.refreshedJustNow") : t("repos.refreshedAgo", { count: mins });
  })();

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

  const toggleGroupSelection = (key: string, projects: RecentProject[]) => {
    if (projects.length === 0) return;
    let selecting = false;
    setSelected((prev) => {
      const next = new Set(prev);
      const allSelected = projects.every((p) => next.has(p.path));
      selecting = !allSelected;
      if (allSelected) {
        for (const p of projects) next.delete(p.path);
      } else {
        for (const p of projects) next.add(p.path);
      }
      return next;
    });
    // A collapsed group only shows one checkbox. Open it so the count matches the cards.
    if (selecting) {
      setCollapsed((prev) => {
        if (!prev.has(key)) return prev;
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  };

  const clearSelected = () => setSelected(new Set());

  const repoLabel = (path: string) =>
    recentProjects.find((p) => p.path === path)?.name || projectNameFromPath(path);

  const reportBatch = (ok: number, fail: number, errors: string[], skipped = 0) => {
    const parts: string[] = [t("batch.done", { ok, fail })];
    if (skipped > 0) parts.push(t("batch.skipped", { count: skipped }));
    if (errors.length > 0) {
      const shown = errors
        .slice(0, 3)
        .map((e) => (e.length > 140 ? `${e.slice(0, 140)}…` : e));
      parts.push(shown.join(" ｜ "));
      if (errors.length > 3) parts.push(t("batch.moreErrors", { count: errors.length - 3 }));
    }
    showMsg(fail > 0 ? "error" : "success", parts.join(" ｜ "));
  };

  const runBatch = async (
    paths: string[],
    fn: (path: string) => Promise<void>,
    skipped = 0
  ) => {
    if (batchLock.current || paths.length === 0) return;
    batchLock.current = true;
    // Hold the cache so a background refresh cannot race the writes below.
    holdRepos(paths);
    setBatchBusy(true);
    setBatchProgress({ done: 0, total: paths.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(paths, GIT_CONCURRENCY, fn, {
        onProgress: (done, total) => setBatchProgress({ done, total }),
        describeError: (path, error) => `${repoLabel(path)}：${String(error)}`,
      });
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      releaseRepos(paths);
      batchLock.current = false;
      setBatchBusy(false);
      setBatchProgress(null);
    }
    reportBatch(ok, fail, errors, skipped);
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
    const paths = [...batchPaths];
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
    const pullable: string[] = [];
    let skipped = 0;
    for (const path of batchPaths) {
      const s = summaries[path];
      if (s?.isGit && s.behind > 0 && s.hasUpstream) pullable.push(path);
      else skipped += 1;
    }
    if (pullable.length === 0) {
      showMsg("error", t("batch.noneToPull"));
      return;
    }
    void runBatch(
      pullable,
      async (path) => {
        void (await invokeGitPull(path));
      },
      skipped
    );
  };

  /** Ensure `<name>` exists locally and on origin in every selected repo. */
  const runBranchCreate = async (name: string, switchExisting: boolean) => {
    const rows = batchPaths
      .map((path) => ({ path, summary: summaries[path] }))
      .filter((row): row is { path: string; summary: RepoBatchItem } => !!row.summary?.isGit);
    const noCommits = rows.filter((row) => !row.summary.hasCommits);
    const runnable = rows.filter((row) => row.summary.hasCommits);
    if (runnable.length === 0) {
      showMsg("error", t("batch.noneToBranch"));
      return;
    }
    const noRemote = runnable.filter((row) => !row.summary.originUrl);
    const lines = runnable.map((row) => `• ${row.summary.name} — ${row.summary.branch}`);
    const warnings: string[] = [];
    if (noCommits.length > 0) {
      warnings.push(
        t("batch.branchNoCommitsWarning", {
          names: noCommits.map((row) => row.summary.name).join(", "),
        })
      );
    }
    if (noRemote.length > 0) {
      warnings.push(t("batch.branchNoRemoteWarning", { count: noRemote.length }));
    }
    if (switchExisting) {
      warnings.push(t("batch.branchSwitchExistingNote", { name }));
    }
    const ok = await confirm({
      title: t("batch.branchCreateTitle"),
      message: `${t("batch.branchCreateConfirmMessage", { count: runnable.length, name })}\n${lines.join("\n")}`,
      warning: warnings.length > 0 ? warnings.join("\n") : undefined,
      confirmText: t("batch.createBranch"),
      icon: warnings.length > 0 ? "warning" : "info",
    });
    if (!ok) return;
    await runBatch(
      runnable.map((row) => row.path),
      async (path) => {
        // Decide per repo: an existing local branch either fails the item or —
        // with the opt-in — becomes a plain switch.
        const list = await invokeGitListBranches(path);
        if (list.local.includes(name)) {
          if (!switchExisting) throw new Error(t("batch.branchExistsLocal", { name }));
          await invokeGitSwitchBranch(path, name);
        } else if (list.remote.includes(name)) {
          // The remote already has this name. Checkout that tip instead of
          // forking a new branch from the current HEAD and pushing over it.
          await invokeGitSwitchBranch(path, name);
        } else {
          await invokeGitCreateBranch(path, name);
        }
        // A fresh branch has no upstream, so git_push takes its `-u origin HEAD` path.
        await invokeGitPush(path);
      }
    );
  };

  const runBranchSwitch = async (name: string) => {
    const rows = batchPaths.filter((path) => summaries[path]?.isGit);
    if (rows.length === 0) {
      showMsg("error", t("batch.noneToBranch"));
      return;
    }
    const dirtyCount = rows.filter((path) => (summaries[path]?.dirtyCount ?? 0) > 0).length;
    const lines = rows.map((path) => {
      const s = summaries[path]!;
      const label = recentProjects.find((p) => p.path === path)?.name || s.name;
      return `• ${label} — ${s.branch}`;
    });
    const ok = await confirm({
      title: t("batch.branchSwitchTitle"),
      message: `${t("batch.branchSwitchConfirmMessage", { count: rows.length, name })}\n${lines.join("\n")}`,
      warning: dirtyCount > 0 ? t("batch.branchDirtyWarning", { count: dirtyCount }) : undefined,
      confirmText: t("batch.switchBranch"),
      icon: dirtyCount > 0 ? "warning" : "info",
    });
    if (!ok) return;
    await runBatch(rows, async (path) => {
      await invokeGitSwitchBranch(path, name);
    });
  };

  const handleBranchConfirm = (name: string, options: { switchExisting: boolean }) => {
    const mode = branchModal;
    setBranchModal(null);
    if (mode === "create") void runBranchCreate(name, options.switchExisting);
    if (mode === "switch") void runBranchSwitch(name);
  };

  const handleBatchRefresh = () => {
    const paths = [...batchPaths];
    if (batchLock.current || paths.length === 0) return;
    batchLock.current = true;
    setBatchBusy(true);
    // One summarize call for the whole selection, so there is no per-repo fraction to advance.
    setBatchProgress({ done: 0, total: paths.length, indeterminate: true });
    void (async () => {
      try {
        invalidateRepos(paths);
        await refreshRepos(paths);
        const snap = getRepoSnapshot();
        const errors = paths.flatMap((path) => {
          const err = snap[path]?.error;
          return err ? [`${repoLabel(path)}：${err}`] : [];
        });
        reportBatch(paths.length - errors.length, errors.length, errors);
      } catch (e) {
        showMsg("error", String(e));
      } finally {
        batchLock.current = false;
        setBatchBusy(false);
        setBatchProgress(null);
      }
    })();
  };

  /** Remove the selected repos from the workbench list (never touches the disk). */
  const handleBatchRemove = async () => {
    const paths = [...batchPaths];
    if (batchLock.current || paths.length === 0) return;
    batchLock.current = true;
    setBatchBusy(true);
    const removed: string[] = [];
    try {
      const ok = await confirm({
        title: t("batch.removeTitle"),
        message: t("batch.removeMessage", { count: paths.length }),
        warning: t("batch.removeWarning"),
        confirmText: tc("actions.delete"),
        icon: "danger",
      });
      if (!ok) return;
      for (const path of paths) {
        const p = recentProjects.find((x) => x.path === path);
        if (p) await removeRecentProject(p.id);
        if (currentGitRepo === path) setCurrentGitRepo(undefined);
        removed.push(path);
      }
      setSelected(new Set());
      showMsg("success", t("batch.removed", { count: removed.length }));
    } catch (e) {
      setSelected((prev) => {
        const next = new Set(prev);
        for (const path of removed) next.delete(path);
        return next;
      });
      showMsg("error", String(e));
    } finally {
      if (removed.length > 0) dropRepos(removed);
      batchLock.current = false;
      setBatchBusy(false);
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

  const handleGoCommit = (path: string) => {
    if (!onOpenCommit) return;
    if (!currentGitRepo || pathKey(currentGitRepo) !== pathKey(path)) setCurrentGitRepo(path);
    onOpenCommit();
  };

  const handleCopyPath = async (path: string) => {
    try {
      await navigator.clipboard.writeText(path);
      showMsg("success", t("repos.pathCopied"));
    } catch (e) {
      showMsg("error", String(e));
    }
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
  const currentScopeLabel = currentGitRepo
    ? findWorkspaceForRepo(currentGitRepo, gitWorkspaces)?.name ??
      (gitWorkspaces.length > 0 ? t("workspace.other") : null)
    : null;

  const renderCard = (p: RecentProject) => {
    const s = summaries[p.path];
    const isCurrent = !!currentGitRepo && pathKey(currentGitRepo) === pathKey(p.path);
    const actual = identities[p.path];
    const preset = findPreset(p.path, repoConfigs);
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
              {s && s.isGit && s.dirtyCount === 0 && (
                <span className="repos-status-pill">{t("workbench.clean")}</span>
              )}
              {s && s.isGit && s.dirtyCount > 0 && (
                <button
                  type="button"
                  className="repos-status-pill is-warn is-action"
                  title={t("repos.goCommitHint")}
                  disabled={!onOpenCommit}
                  onClick={() => handleGoCommit(p.path)}
                >
                  {t("workbench.dirty", { count: s.dirtyCount })}
                </button>
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
                {s.branch && (
                  <span className="commit-branch-badge" title={s.branch}>
                    <GitBranch size={11} />
                    <span className="commit-branch-text">{s.branch}</span>
                  </span>
                )}
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
                      title={`${t("repos.bound", { name: preset.userName })} · ${t("repos.editBinding")}`}
                    >
                      {t("repos.boundShort")}
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
              title={isCurrent ? t("workbench.current") : t("workbench.use")}
              aria-label={t("workbench.use")}
              onClick={() => handleSelect(p.path)}
            >
              <Pin size={14} />
            </button>
            {s?.isGit && (
              <button
                type="button"
                className="btn commit-icon-btn"
                title={t("testReport.open")}
                aria-label={t("testReport.open")}
                onClick={() => onOpenReport?.({ kind: "repo", path: p.path })}
              >
                <ClipboardList size={14} />
              </button>
            )}
            <DropdownMenu
              label={t("repos.more")}
              triggerClassName="btn commit-icon-btn"
              items={[
                {
                  label: t("repos.copyPath"),
                  icon: <Copy size={13} />,
                  onClick: () => void handleCopyPath(p.path),
                },
                ...(s?.isGit
                  ? [
                      {
                        label: preset ? t("repos.editBinding") : t("repos.bind"),
                        icon: <Link2 size={13} />,
                        onClick: () => setBindingPath(p.path),
                      },
                    ]
                  : []),
                { divider: true as const },
                {
                  label: t("workbench.remove"),
                  icon: <Trash2 size={13} />,
                  danger: true,
                  onClick: () => void handleRemove(p.id, p.path),
                },
              ]}
            >
              <MoreHorizontal size={14} />
            </DropdownMenu>
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
                <div
                  className="commit-scope"
                  title={
                    currentScopeLabel
                      ? `${currentScopeLabel} · ${currentGitRepo}`
                      : currentGitRepo
                  }
                >
                  <span className="commit-scope-mark">{t("workbench.current")}</span>
                  <span className="commit-scope-name">{currentScopeLabel || currentRepoName}</span>
                  {currentScopeLabel && currentScopeLabel !== currentRepoName && (
                    <>
                      <span className="commit-scope-dot" aria-hidden />
                      <span className="commit-scope-repo">{currentRepoName}</span>
                    </>
                  )}
                </div>
              ) : (
                <span className="runtime-muted">{t("workbench.noCurrent")}</span>
              )}
            </div>
            <p className="repos-toolbar-subtitle">
              {t("repos.subtitle")}
              {refreshedLabel && (
                <span className="repos-refreshed" title={new Date(lastRefreshAt).toLocaleString()}>
                  · {refreshedLabel}
                </span>
              )}
            </p>
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
        {recentProjects.length > 0 && (
          <div className="repos-filter-row">
            <div className="repos-filter-seg" role="tablist" aria-label={t("repos.filterLabel")}>
              {REPO_FILTERS.map((f) => (
                <button
                  key={f}
                  type="button"
                  role="tab"
                  aria-selected={filter === f}
                  className={`repos-filter-btn${filter === f ? " is-active" : ""}`}
                  onClick={() => setFilter(f)}
                >
                  {t(`repos.filter.${f}`)}
                  <span className="repos-filter-count">{filterCounts[f]}</span>
                </button>
              ))}
            </div>
            <button
              type="button"
              className="btn commit-icon-btn repos-collapse-all"
              onClick={toggleAllCollapsed}
              disabled={narrowed || groups.length === 0}
              title={allCollapsed ? t("repos.expandAll") : t("repos.collapseAll")}
              aria-label={allCollapsed ? t("repos.expandAll") : t("repos.collapseAll")}
            >
              {allCollapsed ? <ChevronsUpDown size={14} /> : <ChevronsDownUp size={14} />}
              <span>{allCollapsed ? t("repos.expandAll") : t("repos.collapseAll")}</span>
            </button>
          </div>
        )}
      </div>

      {recentProjects.length > 0 && (
        <div className={`repos-batch-bar${selected.size > 0 ? "" : " is-empty"}`}>
          <span className="repos-batch-count">{t("batch.selected", { count: batchPaths.length })}</span>
          {selected.size === 0 ? (
            <span className="repos-batch-hint">{t("batch.hint")}</span>
          ) : (
            <>
              {hiddenSelected > 0 && (
                <span className="repos-batch-hint">
                  {t("batch.hiddenSelected", { count: hiddenSelected })}
                </span>
              )}
              {batchProgress && (
                <span className="runtime-muted">
                  {batchProgress.indeterminate
                    ? t("batch.refreshing", { count: batchProgress.total })
                    : t("batch.running", { done: batchProgress.done, total: batchProgress.total })}
                </span>
              )}
              <button
                type="button"
                className="repos-batch-clear"
                disabled={batchBusy}
                onClick={clearSelected}
              >
                {t("batch.clear")}
              </button>
              <div className="repos-batch-actions">
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || accounts.length === 0 || batchPaths.length === 0}
                  title={accounts.length === 0 ? t("batch.noAccounts") : t("batch.bindTitle", { count: batchPaths.length })}
                  onClick={() => setShowBatchBind(true)}
                >
                  <User size={14} />
                  <span>{t("batch.bind")}</span>
                </button>
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={() => setBranchModal("create")}
                  title={t("branchModal.createTitle")}
                >
                  <GitBranchPlus size={14} />
                  <span>{t("batch.createBranch")}</span>
                </button>
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={() => setBranchModal("switch")}
                  title={t("branchModal.switchTitle")}
                >
                  <GitBranch size={14} />
                  <span>{t("batch.switchBranch")}</span>
                </button>
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={handleBatchPull}
                  title={t("batch.pull")}
                >
                  <Download size={14} />
                  <span>{t("batch.pull")}</span>
                </button>
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={handleBatchRefresh}
                  title={t("batch.refresh")}
                >
                  <RefreshCw size={14} />
                  <span>{t("batch.refresh")}</span>
                </button>
                <span className="repos-batch-sep" aria-hidden="true" />
                <button
                  type="button"
                  className="btn commit-icon-btn repos-batch-remove"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={() => void handleBatchRemove()}
                  title={t("batch.removeWarning")}
                >
                  <Trash2 size={14} />
                  <span>{t("batch.remove")}</span>
                </button>
              </div>
            </>
          )}
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

      <div className="repos-scroll">
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
      ) : narrowed && groups.every((g) => g.projects.length === 0) ? (
        <div className="runtime-empty">
          <div>{t("repos.noMatch")}</div>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            style={{ marginTop: 10 }}
            onClick={clearNarrowing}
          >
            {filter !== "all" ? t("repos.clearFilter") : t("repos.clearSearch")}
          </button>
        </div>
      ) : (
        <div className="runtime-list repos-list">
          {groups.map((g) => {
            // Searching / filtering shows every hit; the saved collapse state is left untouched.
            const isCollapsed = collapsed.has(g.key) && !narrowed;
            const sum = summarizeGroup(g.projects, summaries, repoConfigs);
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
                      title={t("workspace.selectGroup", { count: g.projects.length })}
                      onClick={(e) => e.stopPropagation()}
                    >
                      <GroupSelectCheckbox
                        checked={groupAllSelected}
                        indeterminate={groupPartialSelected}
                        disabled={batchBusy}
                        onChange={() => toggleGroupSelection(g.key, g.projects)}
                        ariaLabel={t("workspace.selectGroup", { count: g.projects.length })}
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
                  {(sum.dirty > 0 || sum.behind > 0 || sum.ahead > 0 || sum.identity > 0 || (isCollapsed && selectedInGroup > 0)) && (
                    <span className="repos-group-summary">
                      {isCollapsed && selectedInGroup > 0 && (
                        <span className="repos-status-pill" title={t("workspace.selectedInGroupTitle")}>
                          {t("workspace.selectedInGroup", { count: selectedInGroup })}
                        </span>
                      )}
                      {sum.dirty > 0 && (
                        <span className="repos-status-pill is-warn" title={t("repos.sumDirtyTitle")}>
                          {t("repos.sumDirty", { count: sum.dirty })}
                        </span>
                      )}
                      {sum.behind > 0 && (
                        <span className="repos-status-pill" title={t("repos.sumBehindTitle")}>
                          ↓{sum.behind}
                        </span>
                      )}
                      {sum.ahead > 0 && (
                        <span className="repos-status-pill" title={t("repos.sumAheadTitle")}>
                          ↑{sum.ahead}
                        </span>
                      )}
                      {sum.identity > 0 && (
                        <span className="repos-status-pill is-danger" title={t("repos.sumIdentityTitle")}>
                          {t("repos.sumIdentity", { count: sum.identity })}
                        </span>
                      )}
                    </span>
                  )}
                  {gitRepoPaths.length > 0 && (
                    <button
                      type="button"
                      className="repos-group-admin-btn"
                      disabled={batchBusy}
                      title={t("testReport.openFolder", { count: gitRepoPaths.length })}
                      aria-label={t("testReport.openFolder", { count: gitRepoPaths.length })}
                      onClick={() => onOpenReport?.({ kind: "folder", name: title, paths: gitRepoPaths })}
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
      </div>

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
          selectedCount={batchPaths.length}
          onClose={() => setShowBatchBind(false)}
          onPick={handleBatchBind}
        />
      )}

      {branchModal && (
        <BatchBranchModal
          mode={branchModal}
          paths={batchPaths}
          onClose={() => setBranchModal(null)}
          onConfirm={handleBranchConfirm}
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
