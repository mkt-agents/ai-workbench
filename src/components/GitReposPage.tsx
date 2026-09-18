import { startTransition, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
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
import { findWorkspaceForRepo, projectNameFromPath } from "../core/pathUtils";
import { resolveRepoAccount } from "../core/gitIdentity";
import { useConfirm } from "./ConfirmModal";
import AccountManagerModal from "./AccountManagerModal";
import BatchIdentityModal from "./BatchIdentityModal";
import RepoBindingModal from "./RepoBindingModal";
import ScanReposModal from "./ScanReposModal";
import type { GitAccount, GitRepoSummary, GitWorkspace, RecentProject } from "../core/types";

type Props = {
  active?: boolean;
  onOpenCommit?: () => void;
};

const REFRESH_TTL_MS = 30_000;
const SUMMARY_CONCURRENCY = 4;
const REPOS_COLLAPSED_KEY = "workbench-git-collapsed-groups";

async function mapPool<T>(
  items: T[],
  concurrency: number,
  fn: (item: T) => Promise<void>
): Promise<void> {
  if (items.length === 0) return;
  let next = 0;
  const workers = Array.from({ length: Math.min(concurrency, items.length) }, async () => {
    while (true) {
      const i = next++;
      if (i >= items.length) break;
      await fn(items[i]);
    }
  });
  await Promise.all(workers);
}

async function mapPoolCounted<T>(
  items: T[],
  concurrency: number,
  fn: (item: T) => Promise<void>,
  onProgress?: (done: number, total: number) => void
): Promise<{ ok: number; fail: number }> {
  let ok = 0;
  let fail = 0;
  let done = 0;
  const total = items.length;
  await mapPool(items, concurrency, async (item) => {
    try {
      await fn(item);
      ok += 1;
    } catch {
      fail += 1;
    } finally {
      done += 1;
      onProgress?.(done, total);
    }
  });
  return { ok, fail };
}

function identityMatches(
  actual: { name: string; email: string },
  preset: { userName: string; email: string }
): boolean {
  return (
    actual.name.trim().toLowerCase() === preset.userName.trim().toLowerCase() &&
    actual.email.trim().toLowerCase() === preset.email.trim().toLowerCase()
  );
}

function emptySummary(path: string, name: string, error: string): GitRepoSummary {
  return {
    path,
    name: name || projectNameFromPath(path),
    branch: "",
    dirtyCount: 0,
    ahead: 0,
    behind: 0,
    hasUpstream: false,
    userName: "",
    userEmail: "",
    isGit: false,
    error,
  };
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
  const invokeGitRepoSummary = useGlobalStore((s) => s.invokeGitRepoSummary);
  const invokeGitIsRepo = useGlobalStore((s) => s.invokeGitIsRepo);
  const invokeGitScanRepos = useGlobalStore((s) => s.invokeGitScanRepos);
  const invokeGitPush = useGlobalStore((s) => s.invokeGitPush);
  const invokeGitPull = useGlobalStore((s) => s.invokeGitPull);
  const invokeSetRepoGitConfig = useGlobalStore((s) => s.invokeSetRepoGitConfig);
  const invokeOpenRuntimeFolder = useGlobalStore((s) => s.invokeOpenRuntimeFolder);
  const invokeGitRemoteUrl = useGlobalStore((s) => s.invokeGitRemoteUrl);

  const [summaries, setSummaries] = useState<Record<string, GitRepoSummary>>({});
  const [identities, setIdentities] = useState<Record<string, { name: string; email: string }>>({});
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
  const [collapsed, setCollapsed] = useState<Set<string>>(() => {
    try {
      const raw = localStorage.getItem(REPOS_COLLAPSED_KEY);
      const parsed = raw ? (JSON.parse(raw) as unknown) : null;
      return Array.isArray(parsed) ? new Set(parsed as string[]) : new Set();
    } catch {
      return new Set();
    }
  });
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
    async (paths: { path: string; name?: string }[]) => {
      await mapPool(paths, SUMMARY_CONCURRENCY, async (p) => {
        try {
          const s = await invokeGitRepoSummary(p.path);
          startTransition(() => {
            setSummaries((prev) => ({ ...prev, [p.path]: s }));
            if (s.isGit) {
              setIdentities((prev) => ({
                ...prev,
                [p.path]: { name: s.userName || "", email: s.userEmail || "" },
              }));
            }
          });
        } catch (e) {
          const fallback = emptySummary(p.path, p.name || "", String(e));
          startTransition(() => {
            setSummaries((prev) => ({ ...prev, [p.path]: fallback }));
          });
        }
      });
    },
    [invokeGitRepoSummary]
  );

  /**
   * For repos without a path-level binding, check if their origin remote
   * matches a host config and auto-apply that account's identity.
   * Silent — never blocks the UI, failures are ignored.
   */
  const autoApplyHostIdentities = useCallback(
    async (paths: string[]) => {
      const repoConfigs = useGlobalStore.getState().git.repoConfigs;
      const hostConfigs = useGlobalStore.getState().git.hostConfigs;
      const accounts = useGlobalStore.getState().git.accounts;
      if (hostConfigs.length === 0) return;

      for (const path of paths) {
        // Skip if a path-level binding already exists
        if (repoConfigs.some((c) => c.path === path)) continue;
        try {
          const remoteUrl = await invokeGitRemoteUrl(path);
          if (!remoteUrl) continue;
          const account = resolveRepoAccount({
            repoPath: path,
            remoteUrl,
            repoConfigs,
            hostConfigs,
            accounts,
          });
          if (account) {
            await invokeSetRepoGitConfig(path, account.name, account.email);
          }
        } catch {
          /* ignore — non-fatal */
        }
      }
    },
    [invokeGitRemoteUrl, invokeSetRepoGitConfig]
  );

  const refresh = useCallback(
    async (opts?: { force?: boolean }) => {
      const force = opts?.force ?? false;
      const now = Date.now();
      if (!force && hasLoadedRef.current && now - lastRefreshAtRef.current < REFRESH_TTL_MS) {
        return;
      }
      setLoading(true);
      try {
        await Promise.all([loadRecentProjects(), loadRepoConfigs(), loadHostConfigs(), loadWorkspaces(), loadAccounts()]);
        const projects = useGlobalStore.getState().recentProjects;
        const paths = projects.map((p) => p.path);
        await refreshPaths(projects);
        // After summaries load, auto-apply host-based identities (silent)
        void autoApplyHostIdentities(paths).then(() => {
          // Re-fetch summaries so the UI reflects applied identities
          void refreshPaths(projects);
        });
        lastRefreshAtRef.current = Date.now();
        hasLoadedRef.current = true;
      } finally {
        setLoading(false);
      }
    },
    [loadAccounts, loadRecentProjects, loadRepoConfigs, loadHostConfigs, loadWorkspaces, refreshPaths, autoApplyHostIdentities]
  );

  useEffect(() => {
    if (active) void refresh();
  }, [active, refresh]);

  // Keep collapsed groups across page switches / app restarts.
  useEffect(() => {
    try {
      localStorage.setItem(REPOS_COLLAPSED_KEY, JSON.stringify([...collapsed]));
    } catch {
      /* ignore */
    }
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
    setBatchBusy(true);
    setBatchProgress({ done: 0, total: paths.length });
    try {
      const { ok, fail } = await mapPoolCounted(
        paths,
        SUMMARY_CONCURRENCY,
        fn,
        (done, total) => setBatchProgress({ done, total })
      );
      showMsg(fail > 0 ? "error" : "success", t("batch.done", { ok, fail }));
      await refreshPaths(
        paths.map((path) => {
          const p = recentProjects.find((x) => x.path === path);
          return { path, name: p?.name };
        })
      );
    } finally {
      setBatchBusy(false);
      setBatchProgress(null);
    }
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

  const handleBatchPush = () => {
    const paths = [...selected].filter((path) => {
      const s = summaries[path];
      return !!s?.isGit && (s.ahead > 0 || !s.hasUpstream);
    });
    if (paths.length === 0) {
      showMsg("error", t("batch.noneToPush"));
      return;
    }
    void runBatch(paths, async (path) => {
      void (await invokeGitPush(path));
    });
  };

  const handleBatchRefresh = () => {
    const paths = [...selected];
    void runBatch(paths, async (path) => {
      const p = recentProjects.find((x) => x.path === path);
      const s = await invokeGitRepoSummary(path);
      startTransition(() => {
        setSummaries((prev) => ({ ...prev, [path]: s }));
        if (s.isGit) {
          setIdentities((prev) => ({
            ...prev,
            [path]: { name: s.userName || "", email: s.userEmail || "" },
          }));
        }
      });
      void p;
    });
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
      setSelected(new Set());
      showMsg("success", t("batch.removed", { count: paths.length }));
      await refresh({ force: true });
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleAdd = async () => {
    try {
      const dir = await invokePickDirectory();
      if (!dir) return;
      const ok = await invokeGitIsRepo(dir);
      if (ok) {
        await addRecentProject({ path: dir, name: projectNameFromPath(dir) });
        if (!currentGitRepo) setCurrentGitRepo(dir);
        showMsg("success", t("workbench.added"));
        await refresh({ force: true });
        return;
      }
      const found = await invokeGitScanRepos(dir, 1);
      if (found.length === 0) {
        showMsg("error", t("workbench.notGitRepo"));
        return;
      }
      setScanState({ rootPath: dir, repos: found });
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleRescanWorkspace = async (ws: GitWorkspace) => {
    try {
      const found = await invokeGitScanRepos(ws.path, 1);
      if (found.length === 0) {
        showMsg("error", t("workbench.notGitRepo"));
        return;
      }
      setScanState({ rootPath: ws.path, repos: found });
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleRemoveWorkspace = async (ws: GitWorkspace) => {
    const ok = await confirm({
      title: t("workspace.removeTitle"),
      message: t("workspace.removeConfirm", { name: ws.name }),
      confirmText: t("workspace.remove"),
      icon: "warning",
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
      icon: "warning",
    });
    if (!ok) return;
    await removeRecentProject(id);
    setSelected((prev) => {
      const next = new Set(prev);
      next.delete(path);
      return next;
    });
    if (currentGitRepo === path) setCurrentGitRepo(undefined);
    showMsg("success", t("workbench.removed"));
    await refresh({ force: true });
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

  const handlePush = async (path: string) => {
    if (syncingPath) return;
    setSyncingPath(path);
    try {
      await invokeGitPush(path);
      showMsg("success", t("workbench.pushOk"));
      await refresh({ force: true });
    } catch (e) {
      showMsg("error", String(e));
      await refresh({ force: true });
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
      await refresh({ force: true });
    } catch (e) {
      showMsg("error", String(e));
      await refresh({ force: true });
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
      await refresh({ force: true });
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
      await refresh({ force: true });
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const currentProject = currentGitRepo
    ? recentProjects.find((p) => p.path === currentGitRepo)
    : undefined;
  const currentRepoName = currentProject
    ? currentProject.name || projectNameFromPath(currentProject.path)
    : currentGitRepo
      ? projectNameFromPath(currentGitRepo)
      : null;

  const renderCard = (p: RecentProject) => {
    const s = summaries[p.path];
    const isCurrent = currentGitRepo === p.path;
    const actual = identities[p.path];
    const preset = repoConfigs.find((c) => c.path === p.path);
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
                  disabled={applyingPath === p.path}
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
                disabled={syncingPath === p.path}
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
                disabled={syncingPath === p.path}
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
              onClick={handleAdd}
              disabled={batchBusy}
              title={t("workbench.addRepo")}
            >
              <FolderPlus size={14} />
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
              onClick={handleBatchPush}
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

      {recentProjects.length === 0 && gitWorkspaces.length === 0 ? (
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
                        disabled={batchBusy}
                        title={t("workspace.rescan")}
                        aria-label={t("workspace.rescan")}
                        onClick={() => void handleRescanWorkspace(g.workspace!)}
                      >
                        <RefreshCw size={13} />
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
