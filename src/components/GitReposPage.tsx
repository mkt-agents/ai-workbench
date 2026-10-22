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
  X,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import { GIT_CONCURRENCY, mapPoolCounted } from "../core/asyncPool";
import { describePushTargets, isPushable } from "../core/gitPushScope";
import { parseInvokeError } from "../hooks/useInvokeError";
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
import BatchBranchModal, {
  type BranchLanding,
  type BranchModalMode,
  type BranchOptions,
} from "./BatchBranchModal";
import BatchReport, { type BatchRow } from "./git/BatchReport";
import RepoBindingModal from "./RepoBindingModal";
import ScanReposModal from "./ScanReposModal";
import type { ReportTarget } from "./testReportTypes";
import type { GitAccount, GitWorkspace, RecentProject } from "../core/types";

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

/** What a finished batch run was, so 只重试失败项 can re-run exactly that. */
/** 建分支的完整方案：从哪个基点起、要不要先取远端、落在远端还是本地。 */
type BranchPlan = {
  mode: BranchModalMode;
  name: string;
  switchExisting: boolean;
  base: string | null;
  fetchBase: boolean;
  landing: BranchLanding;
};

type BatchAction =
  | { kind: "refresh" }
  | { kind: "pull" }
  | { kind: "push" }
  | { kind: "bind"; accountId: string }
  | { kind: "branch"; plan: BranchPlan };

/** One repo's input to a run: `skip` pre-marks it (with the reason) without touching git. */
type BatchItem = { path: string; skip?: string };

/** A run's per-repo work; returning `{ skip }` means the repo was handled but the step was not. */
/** `skip` = 没碰 git 的原因；`note` = 成功了但值得写明（例如基点落在哪个提交）。 */
type BatchFn = (path: string) => Promise<void | { skip: string } | { note: string }>;

/** The result list: what was run, one row per repo, and how to re-run the failures. */
type BatchReportData = { title: string; rows: BatchRow[]; action: BatchAction };

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
  const invokeGitFetchBranch = useGlobalStore((s) => s.invokeGitFetchBranch);
  const invokeGitPublishBranch = useGlobalStore((s) => s.invokeGitPublishBranch);
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
  /** Cancel is cooperative: queued repos never reach git, running ones finish. */
  const batchCancel = useRef(false);
  const [batchCanceling, setBatchCanceling] = useState(false);
  const [batchProgress, setBatchProgress] = useState<{
    done: number;
    total: number;
    indeterminate?: boolean;
  } | null>(null);
  /** 上一次批量的逐仓库结果，留到用户关掉（toast 只活 8 秒）。 */
  const [batchReport, setBatchReport] = useState<BatchReportData | null>(null);
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

  /** 跨组筛选时逐个点组头太费；一键选中当前筛选出来的全部仓库。 */
  const allVisibleSelected = visiblePaths.size > 0 && [...visiblePaths].every((path) => selected.has(path));

  const toggleVisibleSelection = () => {
    const add = !allVisibleSelected;
    setSelected((prev) => {
      const next = new Set(prev);
      for (const path of visiblePaths) {
        if (add) next.add(path);
        else next.delete(path);
      }
      return next;
    });
  };

  /** 选区名单：点一行把它移出选区；被筛选挡在列表外的仓库也标出来，免得计数看不出所以然。 */
  const roster = useMemo(
    () =>
      [...selected]
        .map((path) => ({
          path,
          label: recentProjects.find((p) => p.path === path)?.name || projectNameFromPath(path),
          branch: summaries[path]?.branch ?? "",
          hidden: !visiblePaths.has(path),
        }))
        .sort((a, b) => a.label.localeCompare(b.label) || a.path.localeCompare(b.path)),
    [selected, recentProjects, summaries, visiblePaths]
  );

  const repoLabel = (path: string) =>
    recentProjects.find((p) => p.path === path)?.name || projectNameFromPath(path);

  /** Structured invoke errors read as a Chinese sentence, not `[GIT_CONFLICT] …`. */
  const errorText = (e: unknown) => {
    const raw = String(e instanceof Error ? e.message : e ?? "").trim();
    return parseInvokeError(raw).message || raw;
  };

  /**
   * Same step over several repos, one row per repo kept for the result list:
   * the 8-second toast could only show three errors of a 19-repo run, and a
   * pre-marked `skip` never reaches git at all.
   */
  const runBatch = async (
    items: BatchItem[],
    title: string,
    action: BatchAction,
    fn: BatchFn
  ): Promise<BatchRow[] | null> => {
    if (batchLock.current || items.length === 0) return null;
    batchLock.current = true;
    batchCancel.current = false;
    setBatchCanceling(false);
    const rowOf = new Map<string, BatchRow>(
      items.map((i) => [
        i.path,
        { path: i.path, label: repoLabel(i.path), status: i.skip ? "skip" : "ok", detail: i.skip },
      ])
    );
    const run = items.filter((i) => !i.skip).map((i) => i.path);
    setBatchBusy(true);
    setBatchProgress({ done: 0, total: run.length });
    if (run.length > 0) {
      // Hold the cache so a background refresh cannot race the writes below.
      holdRepos(run);
      try {
        await mapPoolCounted(run, GIT_CONCURRENCY, async (path) => {
          // Cancel means "never start this one": git is not touched at all, so a
          // half-run batch leaves no mystery writes behind.
          if (batchCancel.current) {
            rowOf.set(path, {
              path,
              label: repoLabel(path),
              status: "cancel",
              detail: t("batch.cancelNotRun"),
            });
            return;
          }
          try {
            const out = await fn(path);
            rowOf.set(
              path,
              out && "skip" in out
                ? { path, label: repoLabel(path), status: "skip", detail: out.skip }
                : out && "note" in out
                  ? { path, label: repoLabel(path), status: "ok", detail: out.note }
                  : { path, label: repoLabel(path), status: "ok" }
            );
          } catch (e) {
            rowOf.set(path, { path, label: repoLabel(path), status: "fail", detail: errorText(e) });
            throw e;
          }
        }, { onProgress: (done, total) => setBatchProgress({ done, total }) });
      } finally {
        releaseRepos(run);
        batchLock.current = false;
        setBatchBusy(false);
        setBatchProgress(null);
      }
      // The writes landed after these paths were last read, so force a re-read —
      // only for the repos git actually touched.
      const touched = run.filter((path) => rowOf.get(path)?.status !== "cancel");
      await refreshPaths(
        touched.map((path) => {
          const p = recentProjects.find((x) => x.path === path);
          return { path, name: p?.name };
        }),
        { force: true }
      );
    } else {
      batchLock.current = false;
      setBatchBusy(false);
      setBatchProgress(null);
    }
    const rows = items.map((i) => rowOf.get(i.path) ?? { path: i.path, label: repoLabel(i.path), status: "ok" as const });
    setBatchCanceling(false);
    setBatchReport({ title, rows, action });
    return rows;
  };

  /** 取消 = 不再把剩下的仓库交给 git；已经在跑的那几个仍然跑完，结果照样进清单。 */
  const requestBatchCancel = () => {
    batchCancel.current = true;
    setBatchCanceling(true);
  };

  const doBatchBind = async (account: GitAccount, paths: string[]) => {
    const rows = await runBatch(
      paths.map((path) => ({ path })),
      t("batch.bind"),
      { kind: "bind", accountId: account.id },
      async (path) => {
        void (await invokeSetRepoGitConfig(path, account.name, account.email));
      }
    );
    const bound = (rows ?? []).filter((r) => r.status === "ok").map((r) => r.path);
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
      showMsg("error", errorText(e));
    }
  };

  const handleBatchBind = async (account: GitAccount) => {
    const paths = [...batchPaths];
    setShowBatchBind(false);
    await doBatchBind(account, paths);
  };

  const withSkip = (paths: string[], skipOf: (path: string) => string | undefined): BatchItem[] =>
    paths.map((path) => {
      const skip = skipOf(path);
      return skip ? { path, skip } : { path };
    });

  const pullSkip = (path: string): string | undefined => {
    const s = summaries[path];
    if (!s?.isGit) return t("batch.skipNotGit");
    if (!s.hasUpstream) return t("batch.skipNoUpstream");
    if (s.behind === 0) return t("batch.skipUpToDate");
    return undefined;
  };

  const doBatchPull = (paths: string[]) => {
    const items = withSkip(paths, pullSkip);
    if (items.every((i) => i.skip)) {
      showMsg("error", t("batch.noneToPull"));
      return;
    }
    void runBatch(items, t("batch.pull"), { kind: "pull" }, async (path) => {
      void (await invokeGitPull(path));
    });
  };

  const handleBatchPull = () => doBatchPull([...batchPaths]);

  const pushSkip = (path: string): string | undefined => {
    const s = summaries[path];
    if (!s?.isGit) return t("batch.skipNotGit");
    if (!s.hasCommits) return t("batch.skipNoCommits");
    if (!s.originUrl) return t("batch.skipNoRemote");
    if (!isPushable(s)) return t("batch.skipNothingAhead");
    return undefined;
  };

  const doBatchPush = async (paths: string[]) => {
    const targets = paths
      .filter((path) => !pushSkip(path))
      .map((path) => ({ path, label: repoLabel(path), summary: summaries[path]! }));
    if (targets.length === 0) {
      showMsg("error", t("batch.noneToPush"));
      return;
    }
    const { message, warning } = describePushTargets(targets, t);
    const ok = await confirm({
      title: t("batch.pushTitle"),
      message,
      warning,
      confirmText: t("batch.push"),
      icon: warning ? "warning" : "info",
    });
    if (!ok) return;
    void runBatch(withSkip(paths, pushSkip), t("batch.push"), { kind: "push" }, async (path) => {
      void (await invokeGitPush(path));
    });
  };

  const handleBatchPush = () => void doBatchPush([...batchPaths]);

  /**
   * Ensure `<name>` exists — on origin only, or locally and checked out. The base
   * is resolved per repo by git (remote-tracking ref before the local branch), so
   * a repo that cannot see the base fails with a plain sentence instead of
   * silently forking off whatever tip it happened to hold.
   */
  const doBatchBranch = async (paths: string[], plan: BranchPlan) => {
    const { mode, name, switchExisting, base, fetchBase, landing } = plan;
    const remoteOnly = mode === "create" && landing === "remote";
    const branchSkip = (path: string): string | undefined => {
      const s = summaries[path];
      if (!s?.isGit) return t("batch.skipNotGit");
      // 没指定基点就是从各仓 HEAD 起，HEAD 没提交就没得起；指定基点时 unborn HEAD 不影响。
      if (mode === "create" && !base && !s.hasCommits) return t("batch.skipNoCommits");
      if (remoteOnly && !s.originUrl) return t("batch.skipNoRemotePublish");
      return undefined;
    };
    const runnable = paths.filter((path) => !branchSkip(path));
    if (runnable.length === 0) {
      showMsg("error", t("batch.noneToBranch"));
      return;
    }
    const label = (path: string) => summaries[path]?.name || repoLabel(path);
    const lines = runnable.map((path) => `• ${label(path)} — ${summaries[path]?.branch ?? ""}`);
    const baseNote = base ? t("batch.baseNote", { name: base }) : t("batch.baseHeadNote");
    const noRemote = paths.filter(
      (path) => summaries[path]?.isGit && !summaries[path]?.originUrl
    ).length;
    const warnings: string[] = [];
    if (mode === "create") {
      if (base && fetchBase) warnings.push(t("batch.fetchBaseWarning", { name: base }));
      if (remoteOnly) {
        if (noRemote > 0) warnings.push(t("batch.noRemotePublishWarning", { count: noRemote }));
        const heads = [
          ...new Set(runnable.map((path) => summaries[path]?.branch ?? "").filter(Boolean)),
        ];
        if (!base && heads.length > 1) {
          warnings.push(t("batch.divergentBaseWarning", { names: heads.slice(0, 4).join("、") }));
        }
      } else {
        const unborn = base
          ? []
          : paths.filter((path) => summaries[path]?.isGit && !summaries[path]?.hasCommits);
        if (unborn.length > 0) {
          warnings.push(t("batch.branchNoCommitsWarning", { names: unborn.map(label).join(", ") }));
        }
        if (noRemote > 0) warnings.push(t("batch.branchNoRemoteWarning", { count: noRemote }));
        if (switchExisting) warnings.push(t("batch.branchSwitchExistingNote", { name }));
      }
    }
    if (mode === "switch") {
      const dirty = runnable.filter((path) => (summaries[path]?.dirtyCount ?? 0) > 0).length;
      if (dirty > 0) warnings.push(t("batch.branchDirtyWarning", { count: dirty }));
    }
    const ok = await confirm({
      title: remoteOnly
        ? t("batch.branchPublishTitle")
        : mode === "switch"
          ? t("batch.branchSwitchTitle")
          : t("batch.branchCreateTitle"),
      message: `${
        mode === "switch"
          ? t("batch.branchSwitchConfirmMessage", { count: runnable.length, name })
          : remoteOnly
            ? t("batch.branchPublishConfirmMessage", { count: runnable.length, name, base: baseNote })
            : t("batch.branchCreateConfirmMessage", { count: runnable.length, name, base: baseNote })
      }\n${lines.join("\n")}`,
      warning: warnings.length > 0 ? warnings.join("\n") : undefined,
      confirmText: remoteOnly
        ? t("batch.publishBranch")
        : mode === "create"
          ? t("batch.createBranch")
          : t("batch.switchBranch"),
      icon: warnings.length > 0 ? "warning" : "info",
    });
    if (!ok) return;
    void runBatch(
      withSkip(paths, branchSkip),
      remoteOnly
        ? t("batch.publishBranch")
        : mode === "create"
          ? t("batch.createBranch")
          : t("batch.switchBranch"),
      { kind: "branch", plan },
      async (path) => {
        if (mode === "switch") {
          // Decide per repo from that repo's own refs: the modal only offers the
          // union, so a branch can be missing in some of the selected repos.
          const list = await invokeGitListBranches(path);
          if (list.current === name) return { skip: t("batch.skipOnBranch", { name }) };
          if (!list.local.includes(name) && !list.remote.includes(name)) {
            throw new Error(t("batch.branchMissing", { name }));
          }
          await invokeGitSwitchBranch(path, name);
          return;
        }
        // 「先从远端取基点」只在指定了基点时有意义 —— HEAD 本来就在工作区里。
        if (base && fetchBase) await invokeGitFetchBranch(path, base);
        if (remoteOnly) return { note: await invokeGitPublishBranch(path, name, base) };
        const list = await invokeGitListBranches(path);
        if (list.local.includes(name)) {
          if (!switchExisting) throw new Error(t("batch.branchExistsLocal", { name }));
          await invokeGitSwitchBranch(path, name);
        } else if (list.remote.includes(name)) {
          // The remote already has this name. Checkout that tip instead of
          // forking a new branch from the chosen base and pushing over it.
          await invokeGitSwitchBranch(path, name);
        } else {
          await invokeGitCreateBranch(path, name, base);
        }
        if (!summaries[path]?.originUrl) return { skip: t("batch.skipNoRemote") };
        void (await invokeGitPush(path));
      }
    );
  };
  const handleBranchConfirm = (name: string, options: BranchOptions) => {
    const mode = branchModal;
    setBranchModal(null);
    if (!mode) return;
    void doBatchBranch([...batchPaths], { mode, name, ...options });
  };

  /** 只重试失败项：按原动作在同一批仓库上重跑一次（该确认的还是会确认）。 */
  const retryBatch = (paths: string[], action: BatchAction) => {
    switch (action.kind) {
      case "refresh":
        void doBatchRefresh(paths);
        break;
      case "pull":
        doBatchPull(paths);
        break;
      case "push":
        void doBatchPush(paths);
        break;
      case "bind": {
        const account = accounts.find((a) => a.id === action.accountId);
        if (!account) {
          showMsg("error", t("batch.noAccounts"));
          return;
        }
        void doBatchBind(account, paths);
        break;
      }
      case "branch":
        void doBatchBranch(paths, action.plan);
        break;
    }
  };

  const doBatchRefresh = async (paths: string[]) => {
    if (batchLock.current || paths.length === 0) return;
    batchLock.current = true;
    setBatchBusy(true);
    // One summarize call for the whole selection, so there is no per-repo fraction to advance.
    setBatchProgress({ done: 0, total: paths.length, indeterminate: true });
    try {
      invalidateRepos(paths);
      await refreshRepos(paths);
      const snap = getRepoSnapshot();
      const rows = paths.map((path): BatchRow => {
        const err = snap[path]?.error;
        return { path, label: repoLabel(path), status: err ? "fail" : "ok", detail: err ? errorText(err) : undefined };
      });
      setBatchReport({ title: t("batch.refresh"), rows, action: { kind: "refresh" } });
    } catch (e) {
      showMsg("error", errorText(e));
    } finally {
      batchLock.current = false;
      setBatchBusy(false);
      setBatchProgress(null);
    }
  };

  const handleBatchRefresh = () => void doBatchRefresh([...batchPaths]);

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
        confirmText: t("batch.removeConfirm"),
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
          {selected.size === 0 ? (
            <span className="repos-batch-count">{t("batch.selected", { count: 0 })}</span>
          ) : (
            <DropdownMenu
              label={t("batch.rosterTitle")}
              triggerClassName="repos-batch-count"
              items={roster.map((row) => ({
                label: `${row.label}${row.branch ? ` — ${row.branch}` : ""}${
                  row.hidden ? ` · ${t("batch.rosterHidden")}` : ""
                }`,
                icon: <X size={12} aria-hidden />,
                onClick: () => toggleSelected(row.path),
              }))}
            >
              {t("batch.selected", { count: batchPaths.length })}
              <ChevronDown size={12} aria-hidden />
            </DropdownMenu>
          )}
          {visiblePaths.size > 0 && (
            <button
              type="button"
              className="repos-batch-clear"
              disabled={batchBusy}
              onClick={toggleVisibleSelection}
              title={allVisibleSelected ? t("batch.clearVisibleTip") : t("batch.selectAllVisibleTip")}
            >
              {allVisibleSelected ? t("batch.clearVisible") : t("batch.selectAllVisible", { count: visiblePaths.size })}
            </button>
          )}
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
                  {batchCanceling
                    ? t("batch.canceling", { count: batchProgress.total - batchProgress.done })
                    : batchProgress.indeterminate
                      ? t("batch.refreshing", { count: batchProgress.total })
                      : t("batch.running", { done: batchProgress.done, total: batchProgress.total })}
                </span>
              )}
              {batchProgress && !batchProgress.indeterminate && (
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={requestBatchCancel}
                  disabled={batchCanceling}
                  title={t("batch.cancelTip")}
                >
                  {t("batch.cancel")}
                </button>
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
                  title={t("batch.createBranchTip")}
                >
                  <GitBranchPlus size={14} />
                  <span>{t("batch.createBranch")}</span>
                </button>
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={() => setBranchModal("switch")}
                  title={t("batch.switchTip")}
                >
                  <GitBranch size={14} />
                  <span>{t("batch.switchBranch")}</span>
                </button>
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={handleBatchPull}
                  title={t("batch.pullTip")}
                >
                  <Download size={14} />
                  <span>{t("batch.pull")}</span>
                </button>
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={handleBatchPush}
                  title={t("batch.pushTip")}
                >
                  <Upload size={14} />
                  <span>{t("batch.push")}</span>
                </button>
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled={batchBusy || batchPaths.length === 0}
                  onClick={handleBatchRefresh}
                  title={t("batch.refreshTip")}
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
                  title={t("batch.removeTip")}
                >
                  <Trash2 size={14} />
                  <span>{t("batch.remove")}</span>
                </button>
              </div>
            </>
          )}
        </div>
      )}

      {batchReport && (
        <BatchReport
          title={batchReport.title}
          rows={batchReport.rows}
          busy={batchBusy}
          onClose={() => setBatchReport(null)}
          onRetry={(paths) => retryBatch(paths, batchReport.action)}
        />
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
