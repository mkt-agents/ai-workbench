import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  Archive,
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Download,
  GitBranch,
  GitMerge,
  History,
  Loader2,
  RefreshCw,
  RotateCcw,
  Sparkles,
  Trash2,
  Upload,
  Undo2,
  X,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import { GIT_CONCURRENCY, mapPoolCounted } from "../core/asyncPool";
import { readStoredArray, readStoredString, writeStoredArray, writeStoredString } from "../core/localState";
import {
  getRepoSnapshot,
  holdRepos,
  invalidateRepos,
  refreshRepos,
  releaseRepos,
  reposLoaded,
} from "../core/gitCache";
import { findWorkspaceForRepo, pathKey, projectNameFromPath } from "../core/pathUtils";
import { describePushTargets, isPushable } from "../core/gitPushScope";
import { recordAiUsage } from "../lib/aiUsage";
import { useConfirm, useConfirmChoice } from "./ConfirmModal";
import { looksLikeTempFile } from "../lib/tempFiles";
import { parseInvokeError } from "../hooks/useInvokeError";
import MergeResolver from "./git/MergeResolver";
import DiffViewer from "./git/DiffViewer";
import DiffModal, { type DiffFile } from "./git/DiffModal";
import type {
  AIModelConfig,
  GitStatusEntry,
  RecentProject,
  RepoBatchItem,
} from "../core/types";


function identityMatches(
  actual: { name: string; email: string },
  preset: { userName: string; email: string }
): boolean {
  return (
    actual.name.trim().toLowerCase() === preset.userName.trim().toLowerCase() &&
    actual.email.trim().toLowerCase() === preset.email.trim().toLowerCase()
  );
}

/** Favorites ∪ current repo — commit page must see the active path even if not favorited. */
function projectsForScan(
  favorites: RecentProject[],
  currentGitRepo?: string
): RecentProject[] {
  const list = [...favorites];
  const cur = currentGitRepo?.trim();
  if (!cur) return list;
  const key = pathKey(cur);
  if (list.some((p) => pathKey(p.path) === key)) return list;
  list.unshift({
    id: -1,
    path: cur,
    name: projectNameFromPath(cur),
    lastOpenedAt: new Date().toISOString(),
  });
  return list;
}

function findProjectByPath(projects: RecentProject[], path?: string): RecentProject | undefined {
  if (!path) return undefined;
  const key = pathKey(path);
  return projects.find((p) => pathKey(p.path) === key);
}

const AI_CONTEXT_MAX = 12_000;

const CL_FILTER_KEY = "workbench-commit-filter";
const CL_COLLAPSED_KEY = "workbench-commit-collapsed-repos";
const CL_DRAFT_KEY = "workbench-commit-draft";
const CL_RECENT_KEY = "workbench-commit-recent-messages";
const CL_IGNORE_WS_KEY = "cl-diff-ignore-ws";
const RECENT_LOG = 12;
const RECENT_LOCAL = 10;

const COMMIT_MSG_SYSTEM =
  "你是资深工程师。根据 git 变更写一条提交说明。" +
  "要求：只输出一行中文说明；可用 conventional 前缀（feat/fix/docs/refactor/chore）；" +
  "不要 markdown、不要引号、不要正文、不要解释。";

function cleanGeneratedMessage(raw: string): string {
  let s = raw.trim();
  if (s.startsWith("```")) {
    s = s.replace(/^```[a-zA-Z]*\n?/, "").replace(/\n?```$/, "").trim();
  }
  s = s.replace(/^["'「『]|["'」』]$/g, "").trim();
  const firstLine = s.split(/\r?\n/).map((l) => l.trim()).find((l) => l.length > 0) || s;
  return firstLine.slice(0, 200);
}

function truncateContext(text: string, max = AI_CONTEXT_MAX): string {
  if (text.length <= max) return text;
  return `${text.slice(0, max)}\n...(truncated)`;
}

function isRepoDirty(
  path: string,
  summaries: Record<string, RepoBatchItem>,
  statuses: Record<string, GitStatusEntry[]>
): boolean {
  const key = pathKey(path);
  const s =
    summaries[path] ||
    Object.values(summaries).find((x) => pathKey(x.path) === key);
  const statusEntries =
    statuses[path] ||
    Object.entries(statuses).find(([p]) => pathKey(p) === key)?.[1];
  return !!s?.isGit && ((s.dirtyCount ?? 0) > 0 || (statusEntries?.length ?? 0) > 0);
}

/** A workspace id, or "other". Null when there are no groups yet — the list stays unscoped. */
function scopeFilterForRepo<T extends { id: number; path: string }>(
  repoPath: string | undefined,
  workspaces: T[]
): string | null {
  if (workspaces.length === 0) return null;
  if (repoPath) {
    const ws = findWorkspaceForRepo(repoPath, workspaces);
    if (ws) return `ws-${ws.id}`;
    return "other";
  }
  return `ws-${workspaces[0].id}`;
}

function projectsInFilter<T extends { id: number; path: string }>(
  filter: string,
  projects: RecentProject[],
  workspaces: T[]
): RecentProject[] {
  if (filter === "all") return projects;
  if (filter === "other") {
    return projects.filter((p) => !findWorkspaceForRepo(p.path, workspaces));
  }
  const wsId = Number(filter.replace(/^ws-/, ""));
  const ws = workspaces.find((w) => w.id === wsId);
  if (!ws) return projects;
  return projects.filter((p) => findWorkspaceForRepo(p.path, [ws])?.id === ws.id);
}

/** Keep the focused repo when it already sits in the scope; otherwise prefer a dirty one. */
function pickRepoInScope(
  scope: RecentProject[],
  current: string | undefined,
  summaries: Record<string, RepoBatchItem>,
  statuses: Record<string, GitStatusEntry[]>
): string | undefined {
  if (current && scope.some((p) => pathKey(p.path) === pathKey(current))) return current;
  const dirty = scope.find((p) => isRepoDirty(p.path, summaries, statuses));
  return (dirty ?? scope[0])?.path;
}

function statusCode(entry: GitStatusEntry): string {
  if (entry.group === "conflicted") return "U";
  if (entry.group === "untracked") return "?";
  if (entry.group === "staged") return (entry.indexStatus || "M").trim() || "M";
  return (entry.workTreeStatus || "M").trim() || "M";
}

function statusClass(code: string): string {
  const c = code.toUpperCase();
  if (c === "A") return "git-status-a";
  if (c === "D") return "git-status-d";
  if (c === "R" || c === "C") return "git-status-r";
  if (c === "?" || c === "!") return "git-status-u";
  if (c === "U") return "git-status-conflict";
  return "git-status-m";
}

function rawInvokeError(err: unknown): string {
  return (err instanceof Error ? err.message : String(err ?? "")).trim();
}

/** The message without the `[GIT_CONFLICT]`-style code prefix. */
function formatInvokeError(err: unknown): string {
  return parseInvokeError(rawInvokeError(err)).message.trim() || "操作失败";
}

function invokeErrorCode(err: unknown): string | null {
  return parseInvokeError(rawInvokeError(err)).code;
}

/** Git is mid-merge/rebase in this repo, or has unmerged paths. */
function repoInGitOperation(s: RepoBatchItem | undefined): boolean {
  return !!s?.isGit && (!!s.operation || (s.conflictCount ?? 0) > 0);
}

function truncateError(err: string, max = 360): string {
  const s = err.trim();
  if (s.length <= max) return s;
  return `${s.slice(0, max)}…`;
}

function selectionKey(entry: { path: string; group: string }): string {
  return `${entry.group}\0${entry.path}`;
}

/** git reports repo-relative `/` paths; tooltips show the full Windows path. */
function absoluteFilePath(repoPath: string, relPath: string): string {
  return `${repoPath.replace(/[\\/]+$/, "")}\\${relPath.replace(/\//g, "\\")}`;
}

/**
 * Rebuild the checked set after a refresh without overriding the user.
 *
 * A plain "select all" here silently re-ticks files the user had just
 * unticked, and the next commit then includes them — so previously seen
 * entries keep their tick state, entries that appeared since default to
 * checked, and vanished entries are dropped.
 */
function mergeSelection(
  prev: Map<string, Set<string>>,
  known: Map<string, Set<string>>,
  next: Record<string, GitStatusEntry[]>
): Map<string, Set<string>> {
  const out = new Map<string, Set<string>>();
  for (const [path, entries] of Object.entries(next)) {
    const checked = prev.get(path);
    const seen = known.get(path);
    const kept = new Set<string>();
    for (const entry of entries) {
      // Conflicts are resolved in the merge panel, never committed by a tick.
      if (entry.group === "conflicted") continue;
      const key = selectionKey(entry);
      if (!seen || !seen.has(key)) {
        kept.add(key);
      } else if (!checked || checked.has(key)) {
        kept.add(key);
      }
    }
    out.set(path, kept);
  }
  return out;
}

function fileKey(repoPath: string, filePath: string, group?: string): string {
  return group ? `${repoPath}\0${group}\0${filePath}` : `${repoPath}\0${filePath}`;
}

type DiffTarget = {
  repoPath: string;
  filePath: string;
  staged: boolean;
  wholeFile: boolean;
};

/** Which write is running; the matching button alone shows a spinner. */
type BusyAction = "commit" | "commitPush" | "push" | "pull" | "rollback" | "stash" | "hunk" | "merge";

type Props = {
  active: boolean;
  onToast: (type: "success" | "error", text: string) => void;
  refreshNonce?: number;
  onUndoLastCommit?: () => void;
  undoDisabled?: boolean;
  undoTitle?: string;
  undoing?: boolean;
  /** Pre-selected AI model for commit-message generation (controlled by parent toolbar). */
  selectedModel: AIModelConfig | null;
  /** Currently selected model id in the parent toolbar. */
  selectedModelId: string;
  /** Notifies parent when the user picks a different model. */
  onModelChange: (id: string) => void;
  /** Workspace name while a group filter is active; null means "follow the repo". */
  onScopeLabel?: (label: string | null) => void;
};

function IndeterminateCheckbox({
  checked,
  indeterminate,
  disabled,
  onChange,
}: {
  checked: boolean;
  indeterminate: boolean;
  disabled?: boolean;
  onChange: () => void;
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
      onClick={(e) => e.stopPropagation()}
    />
  );
}

type ModelSelectorProps = {
  models: AIModelConfig[];
  selectedId: string;
  onChange: (id: string) => void;
  disabled?: boolean;
};

export function ModelSelector({ models, selectedId, onChange, disabled }: ModelSelectorProps) {
  const { t } = useTranslation("git");
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  const active = models.find((m) => m.id === selectedId) ?? models[0];

  useEffect(() => {
    if (!open) return;
    const onOutside = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onOutside);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onOutside);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="prompt-model-wrap" ref={ref}>
      <button
        type="button"
        className="prompt-model-trigger"
        disabled={disabled}
        aria-expanded={open}
        aria-label={t("commit.aiModel")}
        title={active ? `${active.name} — ${active.model}` : t("commit.aiModel")}
        onClick={() => setOpen((v) => !v)}
      >
        <Bot size={12} className="prompt-model-icon" aria-hidden />
        <span className="prompt-model-trigger-label">
          {active?.name || t("commit.aiModel")}
        </span>
        <ChevronDown size={12} />
      </button>

      {open && (
        <div className="prompt-model-menu" role="listbox">
          <div className="prompt-model-menu-list">
            {models.map((m) => {
              const isActive = m.id === selectedId;
              return (
                <button
                  key={m.id}
                  type="button"
                  className={`prompt-model-item${isActive ? " is-active" : ""}`}
                  onClick={() => {
                    onChange(m.id);
                    setOpen(false);
                  }}
                  role="option"
                  aria-selected={isActive}
                  title={`${m.name} — ${m.model}`}
                >
                  <span className="prompt-model-item-name">{m.name}</span>
                  <span className="prompt-model-item-id">{m.model}</span>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

function CommitChangelist({
  active,
  onToast,
  refreshNonce = 0,
  onUndoLastCommit,
  undoDisabled,
  undoTitle,
  undoing,
  selectedModel,
  selectedModelId,
  onModelChange,
  onScopeLabel,
}: Props) {
  const { t } = useTranslation("git");
  const confirm = useConfirm();
  const confirmChoice = useConfirmChoice();

  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const repoConfigs = useGlobalStore((s) => s.git.repoConfigs);
  const currentGitRepo = useGlobalStore((s) => s.settings.currentGitRepo);
  const loadRecentProjects = useGlobalStore((s) => s.loadRecentProjects);
  const loadWorkspaces = useGlobalStore((s) => s.loadWorkspaces);
  const setCurrentGitRepo = useGlobalStore((s) => s.setCurrentGitRepo);
  const invokeGitStage = useGlobalStore((s) => s.invokeGitStage);
  const invokeGitUnstage = useGlobalStore((s) => s.invokeGitUnstage);
  const invokeGitCommit = useGlobalStore((s) => s.invokeGitCommit);
  const invokeGitPush = useGlobalStore((s) => s.invokeGitPush);
  const invokeGitPull = useGlobalStore((s) => s.invokeGitPull);
  const invokeGitMergeState = useGlobalStore((s) => s.invokeGitMergeState);
  const invokeGitMergeAbort = useGlobalStore((s) => s.invokeGitMergeAbort);
  const invokeGitDiff = useGlobalStore((s) => s.invokeGitDiff);
  const invokeGitApplyCached = useGlobalStore((s) => s.invokeGitApplyCached);
  const invokeGitLog = useGlobalStore((s) => s.invokeGitLog);
  const invokeGitStashPush = useGlobalStore((s) => s.invokeGitStashPush);
  const invokeGitStashList = useGlobalStore((s) => s.invokeGitStashList);
  const invokeGitStashPop = useGlobalStore((s) => s.invokeGitStashPop);
  const invokeGitStashDrop = useGlobalStore((s) => s.invokeGitStashDrop);
  const invokeGitDiscard = useGlobalStore((s) => s.invokeGitDiscard);
  const invokeGitCommitContext = useGlobalStore((s) => s.invokeGitCommitContext);
  const invokeSetRepoGitConfig = useGlobalStore((s) => s.invokeSetRepoGitConfig);
  const invokeGenerateText = useGlobalStore((s) => s.invokeGenerateText);
  const aiModels = useGlobalStore((s) => s.aiModels);

  const [filter, setFilter] = useState(() => readStoredString(CL_FILTER_KEY, "all"));
  const [loading, setLoading] = useState(false);
  const [busyAction, setBusyAction] = useState<BusyAction | null>(null);
  /** Any write running: every action stays disabled, only the running one spins. */
  const busy = busyAction !== null;
  const [generating, setGenerating] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number; label?: string } | null>(null);
  const [message, setMessage] = useState(() => readStoredString(CL_DRAFT_KEY));
  const [amend, setAmend] = useState(false);
  const [recentOpen, setRecentOpen] = useState(false);
  const [recentSubjects, setRecentSubjects] = useState<string[]>([]);
  const [stashes, setStashes] = useState<Array<{ index: number; message: string }>>([]);
  const [stashOpen, setStashOpen] = useState(false);
  const [summaries, setSummaries] = useState<Record<string, RepoBatchItem>>({});
  const [statuses, setStatuses] = useState<Record<string, GitStatusEntry[]>>({});
  const [selected, setSelected] = useState<Map<string, Set<string>>>(() => new Map());
  const [collapsed, setCollapsed] = useState<Set<string>>(
    () => new Set(readStoredArray<string>(CL_COLLAPSED_KEY))
  );
  /** Failed repos of the last batch: stays until dismissed (a toast is gone in 8s). */
  const [report, setReport] = useState<{ title: string; lines: string[] } | null>(null);
  /** Conflict resolver for one repo, starting at `file`. */
  const [resolver, setResolver] = useState<{ repoPath: string; file?: string } | null>(null);
  /** Large diff dialog over the changelist. */
  const [diffModal, setDiffModal] = useState<{ files: DiffFile[]; index: number } | null>(null);
  /** Repos whose merge pull stopped on conflicts during the running batch. */
  const conflictHitRef = useRef<string[]>([]);
  const [diffTarget, setDiffTarget] = useState<DiffTarget | null>(null);
  const [diffText, setDiffText] = useState("");
  const [diffLoading, setDiffLoading] = useState(false);
  const [ignoreWs, setIgnoreWs] = useState(() => readStoredString(CL_IGNORE_WS_KEY) === "1");
  const refreshGen = useRef(0);
  const diffGen = useRef(0);
  const recentRef = useRef<HTMLDivElement>(null);
  const recentGen = useRef(0);
  const stashRef = useRef<HTMLDivElement>(null);
  /** Until the first read lands, an empty changelist proves nothing. */
  const loadedOnce = useRef(false);
  /** Keys each repo showed on the previous refresh, to tell "new" from "still there". */
  const knownKeysRef = useRef<Map<string, Set<string>>>(new Map());
  const diffTargetRef = useRef<DiffTarget | null>(null);

  useEffect(() => {
    diffTargetRef.current = diffTarget;
  }, [diffTarget]);

  // View choices and the draft survive page switches (and app restarts).
  useEffect(() => {
    writeStoredString(CL_FILTER_KEY, filter);
  }, [filter]);

  useEffect(() => {
    writeStoredArray(CL_COLLAPSED_KEY, [...collapsed]);
  }, [collapsed]);

  useEffect(() => {
    writeStoredString(CL_DRAFT_KEY, message);
  }, [message]);

  useEffect(() => {
    if (!recentOpen && !stashOpen) return;
    const onOutside = (e: Event) => {
      const node = e.target;
      if (!(node instanceof Node)) return;
      if (recentOpen && recentRef.current && !recentRef.current.contains(node)) {
        recentGen.current += 1;
        setRecentOpen(false);
      }
      if (stashOpen && stashRef.current && !stashRef.current.contains(node)) {
        setStashOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (document.querySelector(".modal-overlay")) return;
      e.stopPropagation();
      recentGen.current += 1;
      setRecentOpen(false);
      setStashOpen(false);
    };
    window.addEventListener("pointerdown", onOutside, true);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("pointerdown", onOutside, true);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [recentOpen, stashOpen]);

  const closeDiff = useCallback(() => {
    ++diffGen.current; // drop an in-flight diff response
    setDiffTarget(null);
    setDiffText("");
  }, []);

  useEffect(() => {
    if (!active || !diffTarget) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // A dialog on top owns Escape; the diff pane only closes on its own.
      if (document.querySelector(".modal-overlay")) return;
      e.stopPropagation();
      closeDiff();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, diffTarget, closeDiff]);

  const scanProjects = useMemo(
    () => projectsForScan(recentProjects, currentGitRepo),
    [recentProjects, currentGitRepo]
  );

  const defaultModel = useMemo(
    () => aiModels.find((m) => m.isDefault) || aiModels[0] || null,
    [aiModels]
  );

  const scopedProjects = useMemo(
    () => projectsInFilter(filter, scanProjects, gitWorkspaces),
    [filter, gitWorkspaces, scanProjects]
  );

  const filterIsGroup =
    filter === "other" || gitWorkspaces.some((w) => filter === `ws-${w.id}`);

  // "全部仓库" is gone. A saved "all" (or a deleted group) lands on the current repo's group.
  useEffect(() => {
    if (gitWorkspaces.length === 0 || filterIsGroup) return;
    const next = scopeFilterForRepo(currentGitRepo, gitWorkspaces);
    if (next) setFilter(next);
  }, [filterIsGroup, gitWorkspaces, currentGitRepo]);

  // The dropdown is a workspace scope. The header "当前" and the identity row
  // follow it, so picking car-trade stops showing a repo that lives elsewhere.
  useEffect(() => {
    if (filter === "all") {
      onScopeLabel?.(null);
      return;
    }
    const label =
      filter === "other"
        ? t("workspace.other")
        : gitWorkspaces.find((w) => w.id === Number(filter.replace(/^ws-/, "")))?.name ?? null;
    onScopeLabel?.(label);
    if (scopedProjects.length === 0) return;
    const picked = pickRepoInScope(scopedProjects, currentGitRepo, summaries, statuses);
    if (picked && pathKey(picked) !== pathKey(currentGitRepo || "")) {
      setCurrentGitRepo(picked);
    }
  }, [
    filter,
    scopedProjects,
    gitWorkspaces,
    currentGitRepo,
    summaries,
    statuses,
    setCurrentGitRepo,
    onScopeLabel,
    t,
  ]);

  const refresh = useCallback(async () => {
    const gen = ++refreshGen.current;
    // Cache-first: keep showing the last known changelist while re-reading.
    if (!reposLoaded()) setLoading(true);
    try {
      await Promise.all([loadRecentProjects().catch(() => {}), loadWorkspaces().catch(() => {})]);
      if (gen !== refreshGen.current) return;
      const state = useGlobalStore.getState();
      const projects = projectsForScan(state.recentProjects, state.settings.currentGitRepo);
      // One batched read covers summaries *and* file lists: no second
      // `git_status` pass per dirty repo any more.
      await refreshRepos(projects.map((p) => p.path), { withStatus: true }).catch(() => {});
      if (gen !== refreshGen.current) return;

      const snapshot = getRepoSnapshot();
      const nextSummaries: Record<string, RepoBatchItem> = {};
      const nextStatuses: Record<string, GitStatusEntry[]> = {};
      const statusErrors: string[] = [];
      for (const p of projects) {
        const item = snapshot[p.path];
        if (!item) continue;
        nextSummaries[p.path] = item;
        if (item.error) {
          statusErrors.push(`${p.name || projectNameFromPath(p.path)}: ${item.error}`);
          continue;
        }
        if (item.isGit && item.dirtyCount > 0) nextStatuses[p.path] = item.status;
      }

      setStatuses(nextStatuses);
      setSummaries(nextSummaries);
      setSelected((prev) => mergeSelection(prev, knownKeysRef.current, nextStatuses));
      knownKeysRef.current = new Map(
        Object.entries(nextStatuses).map(([path, entries]) => [
          path,
          new Set(entries.map(selectionKey)),
        ])
      );
      // Keep an open diff unless its file left the changelist.
      const target = diffTargetRef.current;
      if (target) {
        const alive = (nextStatuses[target.repoPath] || []).some(
          (e) =>
            e.path === target.filePath &&
            (target.staged ? e.group === "staged" : e.group !== "staged")
        );
        if (!alive) closeDiff();
      }
      if (statusErrors.length > 0) {
        onToast(
          "error",
          t("commit.changelistStatusFetchFail", {
            detail: truncateError(statusErrors.slice(0, 2).join("；"), 160),
          })
        );
      }
      const dirtyFirst = projects.find((p) => nextSummaries[p.path]?.isGit && (nextSummaries[p.path]?.dirtyCount ?? 0) > 0);
      if (!state.settings.currentGitRepo && dirtyFirst) {
        setCurrentGitRepo(dirtyFirst.path);
      }
    } finally {
      loadedOnce.current = true;
      if (gen === refreshGen.current) setLoading(false);
    }
  }, [loadRecentProjects, loadWorkspaces, onToast, setCurrentGitRepo, t]);

  /**
   * Refresh after a write. The shared cache is TTL-based, so without an
   * explicit invalidate the next read would happily return the pre-write
   * snapshot for up to a few seconds.
   */
  const refreshAfterWrite = useCallback(
    async (paths: string[]) => {
      invalidateRepos(paths);
      await refresh();
    },
    [refresh]
  );

  useEffect(() => {
    if (active) void refresh();
  }, [active, refresh, refreshNonce]);

  const dirtyRepos = useMemo(() => {
    return scopedProjects.filter((p) => isRepoDirty(p.path, summaries, statuses));
  }, [scopedProjects, statuses, summaries]);

  const allDirtyRepos = useMemo(() => {
    return scanProjects.filter((p) => isRepoDirty(p.path, summaries, statuses));
  }, [scanProjects, statuses, summaries]);

  const labelPath = useCallback(
    (path: string) =>
      recentProjects.find((p) => pathKey(p.path) === pathKey(path))?.name || projectNameFromPath(path),
    [recentProjects]
  );

  const summaryForPath = useCallback(
    (path?: string) => {
      if (!path) return undefined;
      const direct = summaries[path];
      if (direct) return direct;
      const key = pathKey(path);
      return Object.values(summaries).find((s) => pathKey(s.path) === key);
    },
    [summaries]
  );

  // The filter is the user's choice (and is restored between visits): a clean
  // workspace must stay selectable. The empty state offers a one-click jump to
  // the repos that do have changes instead of widening the filter here.

  const scopedPathKeys = useMemo(
    () => new Set(scopedProjects.map((p) => pathKey(p.path))),
    [scopedProjects]
  );

  const selectedCount = useMemo(() => {
    let n = 0;
    for (const [repoPath, set] of selected) {
      if (!scopedPathKeys.has(pathKey(repoPath))) continue;
      n += set.size;
    }
    return n;
  }, [selected, scopedPathKeys]);

  // A repo mid-merge/rebase or with conflicts is never pushed or pulled from here.
  const pushableRepos = useMemo(() => {
    return scopedProjects.filter((p) => isPushable(summaries[p.path]) && !repoInGitOperation(summaries[p.path]));
  }, [scopedProjects, summaries]);

  /** Scoped repos where git is in the middle of something. */
  const operationRepos = useMemo(
    () => scopedProjects.filter((p) => repoInGitOperation(summaries[p.path])),
    [scopedProjects, summaries]
  );
  /** Merges with every conflict resolved: committing concludes them. */
  const mergeCommitTargets = useMemo(
    () =>
      operationRepos
        .filter((p) => summaries[p.path]?.operation === "merge" && (summaries[p.path]?.conflictCount ?? 0) === 0)
        .map((p) => p.path),
    [operationRepos, summaries]
  );
  /** Repos that block commit: unresolved conflicts, or rebase / cherry-pick / revert. */
  const blockedRepoPaths = useMemo(
    () => operationRepos.map((p) => p.path).filter((path) => !mergeCommitTargets.includes(path)),
    [operationRepos, mergeCommitTargets]
  );

  const pushableCount = pushableRepos.length;

  /** Local branches the batch push would publish for the first time. */
  const firstPushRepos = useMemo(
    () => pushableRepos.filter((p) => !summaries[p.path]?.hasUpstream),
    [pushableRepos, summaries]
  );

  const pullableRepos = useMemo(() => {
    return scopedProjects.filter((p) => {
      const s = summaries[p.path];
      return !!s?.isGit && s.behind > 0 && s.hasUpstream && !repoInGitOperation(s);
    });
  }, [scopedProjects, summaries]);

  const pullableCount = pullableRepos.length;
  const behindTotal = pullableRepos.reduce((n, p) => n + (summaries[p.path]?.behind ?? 0), 0);

  const selectedRepoCount = useMemo(() => {
    let n = 0;
    for (const [repoPath, files] of selected) {
      if (!scopedPathKeys.has(pathKey(repoPath))) continue;
      if (files.size > 0) n += 1;
    }
    return n;
  }, [selected, scopedPathKeys]);

  const currentSummary = summaryForPath(currentGitRepo);
  const currentIsFavorited = !!findProjectByPath(recentProjects, currentGitRepo);

  const runningLabel = (action: BusyAction) =>
    progress && progress.total > 1
      ? `${t(`commit.running.${action}`)} ${progress.done}/${progress.total}`
      : t(`commit.running.${action}`);

  /** Selected repos that git won't let us commit in right now. */
  const selectedBlocked = useMemo(
    () =>
      [...selected.entries()]
        .filter(([path, files]) => files.size > 0 && blockedRepoPaths.some((b) => pathKey(b) === pathKey(path)))
        .map(([path]) => path),
    [selected, blockedRepoPaths]
  );
  const amendDuringMerge = amend && mergeCommitTargets.length > 0;
  const amendBlocked = amend && selectedRepoCount > 1;
  const canCommit =
    !!message.trim() &&
    !busy &&
    !generating &&
    !amendBlocked &&
    !amendDuringMerge &&
    selectedBlocked.length === 0 &&
    (amend
      ? selectedRepoCount <= 1 && (selectedRepoCount === 1 || !!currentGitRepo)
      : selectedCount > 0 || mergeCommitTargets.length > 0);
  const commitBlockReason = selectedBlocked.length > 0
    ? t("commit.conflictBlocked", { name: labelPath(selectedBlocked[0]) })
    : amendDuringMerge
      ? t("commit.amendDuringMerge")
      : amendBlocked
    ? t("commit.amendOneRepo")
    : !message.trim() && (selectedCount > 0 || amend || mergeCommitTargets.length > 0)
      ? t("commit.changelistNeedMessage")
      : amend && selectedRepoCount === 0 && !currentGitRepo
        ? t("commit.changelistPickRepo")
        : !amend && selectedCount === 0 && dirtyRepos.length > 0
          ? t("commit.changelistNeedFiles")
          : !amend && selectedCount === 0 && dirtyRepos.length === 0 && !!message.trim()
            ? t("commit.changelistNothingToCommit")
            : null;

  /**
   * Show what a batch push will do before doing it: publishing a branch that
   * has no upstream yet is not reversible from here.
   */
  const confirmBatchPush = async (paths: string[], addedCommits = 0) => {
    const rows = paths
      .map((path) => ({
        label:
          recentProjects.find((p) => p.path === path)?.name || projectNameFromPath(path),
        summary: summaries[path],
      }))
      .filter((row): row is { label: string; summary: RepoBatchItem } => !!row.summary);
    const { message, warning } = describePushTargets(rows, t, { addedCommits });
    return await confirm({
      title: t("batch.pushTitle"),
      message,
      warning,
      confirmText: t("batch.push"),
      icon: warning ? "warning" : "info",
    });
  };

  /** One repo at a time, so a credential window is not covered by the next push. */
  const noteRepo = (repoPath: string) => {
    setProgress((prev) =>
      prev ? { ...prev, label: labelPath(repoPath) } : { done: 0, total: 1, label: labelPath(repoPath) }
    );
  };

  /** Merge pull; a conflict stops here and is remembered for the resolver. */
  const mergePull = async (repoPath: string) => {
    try {
      await invokeGitPull(repoPath, true);
    } catch (e) {
      if (invokeErrorCode(e) === "GIT_CONFLICT") conflictHitRef.current.push(repoPath);
      throw e;
    }
  };

  const pushWithUpdate = async (repoPath: string) => {
    try {
      await invokeGitPush(repoPath);
    } catch (e) {
      if (invokeErrorCode(e) !== "GIT_PUSH_REJECTED") throw e;
      const ok = await confirm({
        title: t("commit.pushRejectedTitle"),
        message: t("commit.pushRejectedMessage", { name: labelPath(repoPath) }),
        confirmText: t("commit.pushRejectedMerge"),
        icon: "warning",
      });
      if (!ok) throw e;
      await mergePull(repoPath);
      await invokeGitPush(repoPath);
    }
  };

  /** After a batch: failures stay listed; a merge that stopped on conflicts opens the resolver. */
  const finishBatch = async (
    title: string,
    result: { ok: number; fail: number; errors: string[] },
    okText: string,
    paths: string[]
  ) => {
    const hits = conflictHitRef.current;
    conflictHitRef.current = [];
    if (result.fail > 0) {
      onToast("error", title);
      setReport({ title, lines: result.errors });
    } else {
      setReport(null);
      onToast("success", okText);
    }
    await refreshAfterWrite(paths);
    if (hits.length > 0) setResolver({ repoPath: hits[0] });
  };

  /** Abort lists what goes away: every changed file's merge result. */
  const runMergeAbort = async (repoPath: string) => {
    const files = (statuses[repoPath] || []).map((e) => e.path);
    const unique = [...new Set(files)];
    const list =
      unique
        .slice(0, 10)
        .map((f) => `• ${f}`)
        .join("\n") +
      (unique.length > 10 ? `\n${t("commit.rollbackCheckedMore", { count: unique.length - 10 })}` : "");
    const ok = await confirm({
      title: t("commit.abortMergeTitle"),
      message: `${t("commit.abortMergeConfirm", { name: labelPath(repoPath), count: unique.length })}${list ? `\n${list}` : ""}`,
      warning: t("commit.abortMergeWarn"),
      confirmText: t("commit.abortMerge"),
      icon: "danger",
    });
    if (!ok) return;
    setBusyAction("merge");
    try {
      onToast("success", await invokeGitMergeAbort(repoPath));
    } catch (e) {
      onToast("error", formatInvokeError(e));
    } finally {
      setBusyAction(null);
    }
    await refreshAfterWrite([repoPath]);
  };

  const fillMergeMessage = async (repoPath: string) => {
    try {
      const state = await invokeGitMergeState(repoPath);
      if (state.message) setMessage(state.message);
    } catch {
      /* the user can still type one */
    }
  };

  // A merge that just became committable gets git's merge message once, if the box is empty.
  const mergeMessageFilled = useRef<Set<string>>(new Set());
  useEffect(() => {
    if (!active || amend) return;
    const next = mergeCommitTargets.find((p) => !mergeMessageFilled.current.has(pathKey(p)));
    if (!next) return;
    mergeMessageFilled.current.add(pathKey(next));
    if (!message.trim()) void fillMergeMessage(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, amend, mergeCommitTargets]);

  const toggleCollapsed = (repoPath: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(repoPath)) next.delete(repoPath);
      else next.add(repoPath);
      return next;
    });
  };

  const setRepoSelection = (repoPath: string, files: string[] | null) => {
    setSelected((prev) => {
      const next = new Map(prev);
      if (!files || files.length === 0) next.delete(repoPath);
      else next.set(repoPath, new Set(files));
      return next;
    });
  };

  const toggleRepo = (repoPath: string, allEntries: GitStatusEntry[]) => {
    setCurrentGitRepo(repoPath);
    const entries = allEntries.filter((e) => e.group !== "conflicted");
    const cur = selected.get(repoPath);
    const allSelected =
      cur && cur.size === entries.length && entries.every((e) => cur.has(selectionKey(e)));
    if (allSelected) setRepoSelection(repoPath, null);
    else setRepoSelection(repoPath, entries.map((e) => selectionKey(e)));
  };

  // Ticking a file must not move the "current repo" focus — only acting on the
  // repo itself (title, whole-repo checkbox) does.
  const toggleFile = (repoPath: string, entry: GitStatusEntry) => {
    const key = selectionKey(entry);
    setSelected((prev) => {
      const next = new Map(prev);
      const set = new Set(next.get(repoPath) || []);
      if (set.has(key)) set.delete(key);
      else set.add(key);
      if (set.size === 0) next.delete(repoPath);
      else next.set(repoPath, set);
      return next;
    });
  };

  const reloadDiff = async (target: DiffTarget, ignoreWhitespace = ignoreWs) => {
    const gen = ++diffGen.current;
    setDiffLoading(true);
    try {
      const text = await invokeGitDiff(target.repoPath, target.filePath, target.staged, { ignoreWhitespace });
      if (gen !== diffGen.current) return;
      setDiffText(text);
    } catch (e) {
      if (gen !== diffGen.current) return;
      setDiffText(formatInvokeError(e));
    } finally {
      if (gen === diffGen.current) setDiffLoading(false);
    }
  };

  const loadDiff = async (repoPath: string, entry: GitStatusEntry) => {
    const target: DiffTarget = {
      repoPath,
      filePath: entry.path,
      staged: entry.group === "staged",
      wholeFile: entry.group === "untracked",
    };
    setDiffTarget(target);
    await reloadDiff(target);
  };

  /** The changelist row the diff shows (for its status badge). */
  const diffEntry = diffTarget
    ? (statuses[diffTarget.repoPath] || []).find(
        (e) => e.path === diffTarget.filePath && (e.group === "staged") === diffTarget.staged
      )
    : undefined;

  /** Stage / unstage hunks of `target` (the panel's file, or the dialog's). */
  const applyPatch = async (target: DiffTarget, patch: string) => {
    if (busy) return;
    setBusyAction("hunk");
    try {
      const text = await invokeGitApplyCached(target.repoPath, patch, target.staged);
      onToast("success", text);
      await refreshAfterWrite([target.repoPath]);
      const shown = diffTargetRef.current;
      if (shown && shown.repoPath === target.repoPath && shown.filePath === target.filePath) await reloadDiff(shown);
    } catch (e) {
      onToast("error", formatInvokeError(e));
    } finally {
      setBusyAction(null);
    }
  };

  const applyHunks = async (patch: string) => {
    if (diffTarget) await applyPatch(diffTarget, patch);
  };

  /** Every non-conflicted file of the scoped repos, in tree order: the dialog's ← / →. */
  const diffFiles = (): DiffFile[] =>
    dirtyRepos.flatMap((p) =>
      (statuses[p.path] || [])
        .filter((e) => e.group !== "conflicted")
        .map((e) => ({
          repoPath: p.path,
          filePath: e.path,
          staged: e.group === "staged",
          wholeFile: e.group === "untracked",
          statusLabel: statusLabel(statusCode(e)),
          statusClass: statusClass(statusCode(e)),
        }))
    );

  const openDiffModal = (target: DiffTarget | null) => {
    if (!target) return;
    const files = diffFiles();
    const at = files.findIndex(
      (f) => f.repoPath === target.repoPath && f.filePath === target.filePath && f.staged === target.staged
    );
    setDiffModal({ files, index: Math.max(0, at) });
  };

  const loadStashes = useCallback(async () => {
    if (!currentGitRepo) {
      setStashes([]);
      return;
    }
    try {
      setStashes(await invokeGitStashList(currentGitRepo));
    } catch {
      setStashes([]);
    }
  }, [currentGitRepo, invokeGitStashList]);

  useEffect(() => {
    if (!active) return;
    void loadStashes();
  }, [active, loadStashes, refreshNonce]);

  const openRecent = async () => {
    if (recentOpen) {
      recentGen.current += 1;
      setRecentOpen(false);
      return;
    }
    const gen = ++recentGen.current;
    setStashOpen(false);
    const local = readStoredArray<string>(CL_RECENT_KEY).slice(0, RECENT_LOCAL);
    let fromLog: string[] = [];
    if (currentGitRepo) {
      try {
        const rows = await invokeGitLog(currentGitRepo, RECENT_LOG);
        fromLog = rows.map((row) => row[1] || "").filter(Boolean);
      } catch {
        fromLog = [];
      }
    }
    if (gen !== recentGen.current) return;
    const merged: string[] = [];
    for (const item of [...local, ...fromLog]) {
      if (!merged.includes(item)) merged.push(item);
    }
    setRecentSubjects(merged);
    setRecentOpen(true);
  };

  const runStashPush = async () => {
    if (!currentGitRepo) {
      onToast("error", t("commit.changelistPickRepo"));
      return;
    }
    setBusyAction("stash");
    try {
      const text = await invokeGitStashPush(currentGitRepo, message.trim() || undefined);
      onToast("success", text);
      setStashOpen(true);
      await refreshAfterWrite([currentGitRepo]);
      await loadStashes();
    } catch (e) {
      onToast("error", formatInvokeError(e));
    } finally {
      setBusyAction(null);
    }
  };

  const runStashPop = async (index: number) => {
    if (!currentGitRepo) return;
    setBusyAction("stash");
    try {
      const text = await invokeGitStashPop(currentGitRepo, index);
      onToast("success", text);
      await refreshAfterWrite([currentGitRepo]);
      await loadStashes();
    } catch (e) {
      onToast("error", formatInvokeError(e));
    } finally {
      setBusyAction(null);
    }
  };

  const runStashDrop = async (index: number, label: string) => {
    if (!currentGitRepo) return;
    const ok = await confirm({
      title: t("commit.stashDropTitle"),
      message: t("commit.stashDropConfirm", { message: label || t("commit.stashUntitled") }),
      confirmText: t("commit.stashDrop"),
      icon: "danger",
    });
    if (!ok) return;
    setBusyAction("stash");
    try {
      const text = await invokeGitStashDrop(currentGitRepo, index);
      onToast("success", text);
      await loadStashes();
    } catch (e) {
      onToast("error", formatInvokeError(e));
    } finally {
      setBusyAction(null);
    }
  };

  const runDiscard = async (repoPath: string, entry: GitStatusEntry) => {
    const isUntracked = entry.group === "untracked";
    const isStaged = entry.group === "staged";
    const ok = await confirm({
      title: t("commit.discardTitle"),
      message: t("commit.discardConfirm", { path: entry.path }),
      warning: isUntracked
        ? t("commit.discardUntrackedWarn")
        : isStaged
          ? t("commit.discardStagedWarn")
          : t("commit.discardWarn"),
      confirmText: t("commit.discard"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      void (await invokeGitDiscard(repoPath, entry.path, isUntracked, isStaged));
      onToast("success", t("commit.discardOk"));
      if (diffTarget?.repoPath === repoPath && diffTarget.filePath === entry.path) closeDiff();
      await refreshAfterWrite([repoPath]);
    } catch (e) {
      onToast("error", formatInvokeError(e));
    }
  };

  /** IDEA 式回滚：把勾选文件的未提交改动恢复到上次提交的状态（未跟踪文件删除）。 */
  const runRollbackChecked = async () => {
    const pairs: { repoPath: string; repoName: string; entry: GitStatusEntry }[] = [];
    for (const [repoPath, fileSet] of selected) {
      if (fileSet.size === 0) continue;
      if (!scopedPathKeys.has(pathKey(repoPath))) continue;
      const repoName =
        recentProjects.find((p) => p.path === repoPath)?.name || projectNameFromPath(repoPath);
      for (const entry of statuses[repoPath] || []) {
        if (fileSet.has(selectionKey(entry))) pairs.push({ repoPath, repoName, entry });
      }
    }
    if (pairs.length === 0) return;

    const labels = pairs.map(({ repoName, entry }) => `${repoName}/${entry.path}`);
    const listText =
      labels.slice(0, 10).join("\n") +
      (labels.length > 10 ? `\n${t("commit.rollbackCheckedMore", { count: labels.length - 10 })}` : "");
    const ok = await confirm({
      title: t("commit.rollbackCheckedTitle"),
      message: `${t("commit.rollbackCheckedConfirm", { count: labels.length })}\n${listText}`,
      warning: t("commit.rollbackCheckedWarn"),
      confirmText: t("commit.rollbackChecked"),
      icon: "danger",
    });
    if (!ok) return;

    holdRepos([...new Set(pairs.map(({ repoPath }) => repoPath))]);
    setBusyAction("rollback");
    setProgress({ done: 0, total: pairs.length });
    let okCount = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        pairs,
        GIT_CONCURRENCY,
        async ({ repoPath, entry }) => {
          await invokeGitDiscard(
            repoPath,
            entry.path,
            entry.group === "untracked",
            entry.group === "staged"
          );
        },
        {
          onProgress: (done, total) => setProgress((prev) => ({ done, total, label: prev?.label })),
          describeError: (pair, e) =>
            `${pair.repoName}/${pair.entry.path}：${formatInvokeError(e)}`,
        }
      );
      okCount = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusyAction(null);
      setProgress(null);
      releaseRepos([...new Set(pairs.map(({ repoPath }) => repoPath))]);
    }

    // 正在查看的 diff 若刚被回滚，关掉以免展示已失效内容
    const target = diffTargetRef.current;
    if (target && pairs.some(({ repoPath, entry }) => repoPath === target.repoPath && entry.path === target.filePath)) {
      closeDiff();
    }

    if (fail > 0) {
      const detail = errors[0] ? `: ${truncateError(errors[0])}` : "";
      onToast("error", `${t("commit.rollbackCheckedDone", { ok: okCount, fail })}${detail}`);
      setReport({ title: t("commit.rollbackCheckedDone", { ok: okCount, fail }), lines: errors });
    } else {
      onToast("success", t("commit.rollbackCheckedDone", { ok: okCount, fail: 0 }));
    }
    void refreshAfterWrite([...new Set(pairs.map(({ repoPath }) => repoPath))]);
  };

  const runGenerateMessage = async () => {
    if (busy || generating) return;
    if (selectedCount === 0) {
      onToast("error", t("commit.changelistNeedFiles"));
      return;
    }
    if (!selectedModel) {
      onToast("error", t("commit.generateNeedModel"));
      return;
    }
    if (message.trim()) {
      const ok = await confirm({
        title: t("commit.generate"),
        message: t("commit.generateOverwrite"),
        confirmText: t("commit.generate"),
        icon: "warning",
      });
      if (!ok) return;
    }

    setGenerating(true);
    try {
      const fileLabels: string[] = [];
      const diffParts: string[] = [];
      let source = "selected";

      const selectedPairs: { repoPath: string; repoName: string; entry: GitStatusEntry }[] = [];
      for (const [repoPath, fileSet] of selected) {
        if (fileSet.size === 0) continue;
        if (!scopedPathKeys.has(pathKey(repoPath))) continue;
        const repoName =
          recentProjects.find((p) => p.path === repoPath)?.name || projectNameFromPath(repoPath);
        for (const entry of statuses[repoPath] || []) {
          if (fileSet.has(selectionKey(entry))) {
            selectedPairs.push({ repoPath, repoName, entry });
          }
        }
      }

      // One chunk per file/repo, filled in parallel but joined in order, so the
      // prompt stays stable while the diff reads run 4 at a time.
      type Chunk = { labels: string[]; text: string } | null;
      const chunks: Chunk[] = [];

      if (selectedPairs.length > 0) {
        setProgress({ done: 0, total: selectedPairs.length });
        chunks.length = selectedPairs.length;
        await mapPoolCounted(
          selectedPairs.map((pair, index) => ({ ...pair, index })),
          GIT_CONCURRENCY,
          async ({ repoPath, repoName, entry, index }) => {
            const label = `${repoName}/${entry.path}`;
            let diff = "";
            try {
              diff = await invokeGitDiff(repoPath, entry.path, entry.group === "staged");
            } catch {
              diff = entry.group === "untracked" ? "(untracked file)" : "";
            }
            chunks[index] = { labels: [label], text: `--- ${label} ---\n${diff || "(no diff)"}` };
          },
          { onProgress: (done, total) => setProgress({ done, total }) }
        );
      } else {
        source = "workspace";
        setProgress({ done: 0, total: dirtyRepos.length });
        chunks.length = dirtyRepos.length;
        await mapPoolCounted(
          dirtyRepos.map((p, index) => ({ p, index })),
          GIT_CONCURRENCY,
          async ({ p, index }) => {
            try {
              const ctx = await invokeGitCommitContext(p.path);
              const repoName = p.name || projectNameFromPath(p.path);
              chunks[index] = {
                labels: ctx.files.map((f) => `${repoName}/${f}`),
                text: `=== ${repoName} (${ctx.source}) ===\n${ctx.diff || "(no diff)"}`,
              };
            } catch {
              chunks[index] = null; // repo unreadable — skip it
            }
          },
          { onProgress: (done, total) => setProgress({ done, total }) }
        );
      }
      for (const chunk of chunks) {
        if (!chunk) continue;
        fileLabels.push(...chunk.labels);
        diffParts.push(chunk.text);
      }
      setProgress(null);

      if (fileLabels.length === 0 && diffParts.length === 0) {
        onToast("error", t("commit.changelistEmpty"));
        return;
      }

      const user = [
        `来源: ${source === "selected" ? "勾选文件" : "工作区变更"}`,
        `文件 (${fileLabels.length}):`,
        fileLabels.slice(0, 60).join("\n"),
        "",
        "Diff:",
        truncateContext(diffParts.join("\n\n") || "(无 diff 文本)"),
      ].join("\n");

      const raw = await invokeGenerateText({
        config: selectedModel as AIModelConfig,
        system: COMMIT_MSG_SYSTEM,
        user,
      });
      const cleaned = cleanGeneratedMessage(raw);
      if (!cleaned) {
        onToast("error", t("commit.generateFailed"));
        return;
      }
      recordAiUsage({
        modelId: selectedModel.id,
        modelName: selectedModel.name,
        estimatedTokens: Math.max(1, Math.round((user.length + cleaned.length) / 4)),
      });
      setMessage(cleaned);
      onToast("success", t("commit.generateOk"));
    } catch (e) {
      onToast("error", `${t("commit.generateFailed")}: ${formatInvokeError(e)}`);
    } finally {
      setGenerating(false);
    }
  };

  const runCommit = async (withPush: boolean) => {
    const msg = message.trim();
    if (!msg) {
      onToast("error", t("commit.changelistNeedMessage"));
      return;
    }
    let repos = [...selected.entries()].filter(
      ([repoPath, files]) => files.size > 0 && scopedPathKeys.has(pathKey(repoPath))
    );
    if (selectedBlocked.length > 0) {
      onToast("error", t("commit.conflictBlocked", { name: labelPath(selectedBlocked[0]) }));
      return;
    }
    if (amend && mergeCommitTargets.length > 0) {
      onToast("error", t("commit.amendDuringMerge"));
      return;
    }
    // A resolved merge is concluded by committing, even with nothing ticked.
    if (!amend) {
      for (const path of mergeCommitTargets) {
        if (!repos.some(([p]) => pathKey(p) === pathKey(path))) repos.push([path, new Set<string>()]);
      }
    }
    const isMerging = (repoPath: string) => mergeCommitTargets.some((p) => pathKey(p) === pathKey(repoPath));
    if (amend && repos.length > 1) {
      onToast("error", t("commit.amendOneRepo"));
      return;
    }
    if (amend && repos.length === 0) {
      if (!currentGitRepo) {
        onToast("error", t("commit.changelistPickRepo"));
        return;
      }
      repos = [[currentGitRepo, new Set<string>()]];
    } else if (repos.length === 0) {
      onToast("error", t("commit.changelistNeedFiles"));
      return;
    }

    // 疑似临时/非项目文件：列出来让用户决定——仍要提交 / 排除后提交 / 取消
    const tempPicks: { repoPath: string; keys: string[]; paths: string[] }[] = [];
    for (const [repoPath, fileSet] of repos) {
      const entries = statuses[repoPath] || [];
      const keys: string[] = [];
      const paths: string[] = [];
      for (const e of entries) {
        if (fileSet.has(selectionKey(e)) && looksLikeTempFile(e.path)) {
          keys.push(selectionKey(e));
          paths.push(e.path);
        }
      }
      if (keys.length > 0) tempPicks.push({ repoPath, keys, paths });
    }
    if (tempPicks.length > 0) {
      const list = tempPicks.flatMap((x) => x.paths);
      const shown = list
        .slice(0, 8)
        .map((p) => `• ${p}`)
        .join("\n");
      const more = list.length > 8 ? `\n${t("commit.tempMore", { count: list.length - 8 })}` : "";
      const choice = await confirmChoice({
        title: t("commit.tempTitle"),
        message: `${t("commit.tempConfirm", { count: list.length })}\n${shown}${more}`,
        warning: t("commit.tempWarn"),
        confirmText: t("commit.tempProceed"),
        altConfirmText: t("commit.tempExclude"),
        icon: "warning",
      });
      if (choice === "cancel") return;
      if (choice === "alt") {
        const exclude = new Map(tempPicks.map((x) => [x.repoPath, new Set(x.keys)]));
        setSelected((prev) => {
          const next = new Map(prev);
          for (const [repoPath, keys] of exclude) {
            const set = new Set(next.get(repoPath) || []);
            for (const k of keys) set.delete(k);
            if (set.size === 0) next.delete(repoPath);
            else next.set(repoPath, set);
          }
          return next;
        });
        repos = repos
          .map(([repoPath, fileSet]): [string, Set<string>] => {
            const keys = exclude.get(repoPath);
            if (!keys) return [repoPath, fileSet];
            const next = new Set(fileSet);
            for (const k of keys) next.delete(k);
            return [repoPath, next];
          })
          .filter(([, fileSet]) => fileSet.size > 0);
        if (repos.length === 0) {
          onToast("error", t("commit.changelistNeedFiles"));
          return;
        }
      }
    }

    if (amend) {
      const summary = summaryForPath(repos[0][0]);
      if (summary?.hasUpstream && summary.ahead === 0) {
        const ok = await confirm({
          title: t("commit.amendPublishedTitle"),
          message: t("commit.amendPublishedMessage"),
          warning: t("commit.amendPublishedWarn"),
          confirmText: t("commit.amend"),
          icon: "warning",
        });
        if (!ok) return;
      }
    }

    // Identity soft-guard: preset ≠ actual on any selected repo
    const mismatches: {
      path: string;
      name: string;
      actual: string;
      presetName: string;
      presetEmail: string;
      presetLabel: string;
    }[] = [];
    for (const [repoPath] of repos) {
      const preset = repoConfigs.find((c) => pathKey(c.path) === pathKey(repoPath));
      if (!preset) continue;
      const summary = summaries[repoPath];
      const actualName = (summary?.userName || "").trim();
      const actualEmail = (summary?.userEmail || "").trim();
      if (!actualName && !actualEmail) continue;
      if (identityMatches({ name: actualName, email: actualEmail }, preset)) continue;
      mismatches.push({
        path: repoPath,
        name: recentProjects.find((p) => p.path === repoPath)?.name || projectNameFromPath(repoPath),
        actual: `${actualName} <${actualEmail}>`,
        presetName: preset.userName,
        presetEmail: preset.email,
        presetLabel: `${preset.userName} <${preset.email}>`,
      });
    }
    if (mismatches.length > 0) {
      const detail = mismatches
        .map((m) => `• ${m.name}\n  ${t("commit.identityMismatchLine", { actual: m.actual, preset: m.presetLabel })}`)
        .join("\n");
      const choice = await confirmChoice({
        title: t("commit.identityMismatchTitle"),
        message:
          mismatches.length === 1
            ? t("commit.identityMismatchMsg", {
                actual: mismatches[0].actual,
                preset: mismatches[0].presetLabel,
              })
            : `${t("commit.identityMismatchMulti", { count: mismatches.length })}\n${detail}`,
        warning: t("commit.identityMismatchWarn"),
        confirmText: t("commit.commitWithCurrent"),
        altConfirmText: t("commit.usePresetFirst"),
        icon: "warning",
      });
      if (choice === "cancel") return;
      if (choice === "alt") {
        try {
          for (const m of mismatches) {
            await invokeSetRepoGitConfig(m.path, m.presetName, m.presetEmail);
          }
        } catch (e) {
          onToast("error", formatInvokeError(e));
          return;
        }
      }
    }

    // Partial commit will unstage unchecked staged files — warn once
    const partialUnstage: { path: string; files: string[] }[] = [];
    for (const [repoPath, fileSet] of repos) {
      // Inside a merge the index *is* the resolution: nothing gets unstaged there.
      if (fileSet.size === 0 || isMerging(repoPath)) continue;
      const entries = statuses[repoPath] || [];
      const selectedSet = new Set(fileSet);
      const toUnstage = entries
        .filter((e) => e.group === "staged" && !selectedSet.has(selectionKey(e)))
        .map((e) => e.path);
      if (toUnstage.length > 0) {
        partialUnstage.push({ path: repoPath, files: toUnstage });
      }
    }
    let restageAfter = false;
    if (partialUnstage.length > 0) {
      const count = partialUnstage.reduce((n, x) => n + x.files.length, 0);
      const choice = await confirmChoice({
        title: t("commit.unstagePartialTitle"),
        message: t("commit.unstagePartialConfirm", { count }),
        warning: t("commit.unstagePartialWarn"),
        confirmText: t("commit.unstagePartialProceed"),
        altConfirmText: t("commit.unstagePartialKeep"),
        icon: "warning",
      });
      if (choice === "cancel") return;
      restageAfter = choice === "alt";
    }

    const targetPaths = repos.map(([path]) => path);

    // Commit & push: show what each push will do before touching the remote.
    if (withPush) {
      const ok = await confirmBatchPush(targetPaths, 1);
      if (!ok) return;
    }

    // Pause background refresh for these repos until the batch settles.
    holdRepos(targetPaths);
    setBusyAction(withPush ? "commitPush" : "commit");
    setProgress({ done: 0, total: targetPaths.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        targetPaths,
        withPush ? 1 : GIT_CONCURRENCY,
        async (repoPath) => {
          if (withPush) noteRepo(repoPath);
          const files = selected.get(repoPath) || new Set<string>();
          const merging = isMerging(repoPath);
          if (files.size === 0 && !amend && !merging) return;
          const entries = statuses[repoPath] || [];
          const selectedEntries = entries.filter((e) => files.has(selectionKey(e)));
          const toUnstage = merging
            ? []
            : entries
                .filter((e) => e.group === "staged" && !files.has(selectionKey(e)))
                .map((e) => e.path);
          // Stage only unstaged/untracked rows. Staged-only rows stay as the
          // index version so unstaged hunks are not pulled in by `git add`.
          const toStage = [
            ...new Set(
              selectedEntries
                .filter((e) => e.group === "unstaged" || e.group === "untracked")
                .map((e) => e.path)
            ),
          ];
          if (files.size > 0 && toUnstage.length > 0) {
            await invokeGitUnstage(repoPath, toUnstage);
          }
          if (files.size > 0 && toStage.length > 0) {
            await invokeGitStage(repoPath, toStage);
          }
          await invokeGitCommit(repoPath, msg, amend);
          if (files.size > 0 && restageAfter && toUnstage.length > 0) {
            try {
              await invokeGitStage(repoPath, toUnstage);
            } catch {
              /* best-effort restore */
            }
          }
          if (withPush) {
            try {
              await pushWithUpdate(repoPath);
            } catch (e) {
              throw new Error(
                t("commit.changelistCommitOkPushFail", {
                  error: formatInvokeError(e),
                })
              );
            }
          }
        },
        {
          onProgress: (done, total) => setProgress((prev) => ({ done, total, label: prev?.label })),
          describeError: (repoPath, e) => `${labelPath(repoPath)}：${formatInvokeError(e)}`,
        }
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusyAction(null);
      setProgress(null);
      releaseRepos(targetPaths);
    }

    if (fail === 0) {
      const prev = readStoredArray<string>(CL_RECENT_KEY).filter((item) => item !== msg);
      writeStoredArray(CL_RECENT_KEY, [msg, ...prev].slice(0, RECENT_LOCAL));
      setMessage("");
      setAmend(false);
    }
    void finishBatch(
      t("commit.changelistDone", { ok, fail }),
      { ok, fail, errors },
      t("commit.changelistDone", { ok, fail }),
      targetPaths
    );
  };

  const actionsRef = useRef({ runCommit, canCommit, busy, generating });
  actionsRef.current = { runCommit, canCommit, busy, generating };
  const openModalRef = useRef(() => {});
  openModalRef.current = () => openDiffModal(diffTargetRef.current);

  // Ctrl+D: the selected file in the large diff dialog (IDEA's "Show Diff").
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "d") return;
      if (document.querySelector(".modal-overlay")) return;
      if ((e.target as HTMLElement | null)?.closest("input, textarea, select, [contenteditable='true']")) return;
      if (!diffTargetRef.current) return;
      e.preventDefault();
      openModalRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active]);

  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key !== "Enter" || e.altKey) return;
      if (document.querySelector(".modal-overlay")) return;
      const target = e.target as HTMLElement | null;
      if (target?.closest("input, select, [contenteditable='true']")) return;
      if (target?.tagName === "TEXTAREA" && !target.classList.contains("cl-commit-msg")) return;
      const actions = actionsRef.current;
      if (actions.busy || actions.generating || !actions.canCommit) return;
      e.preventDefault();
      void actions.runCommit(e.shiftKey);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active]);

  const runPush = async () => {
    if (pushableRepos.length === 0) {
      onToast("error", t("commit.changelistPushNeedAhead"));
      return;
    }
    const targetPaths = pushableRepos.map((p) => p.path);
    if (!(await confirmBatchPush(targetPaths))) return;
    holdRepos(targetPaths);
    setBusyAction("push");
    setProgress({ done: 0, total: targetPaths.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        targetPaths,
        1,
        async (repoPath) => {
          noteRepo(repoPath);
          await pushWithUpdate(repoPath);
        },
        {
          onProgress: (done, total) => setProgress((prev) => ({ done, total, label: prev?.label })),
          describeError: (repoPath, e) => `${labelPath(repoPath)}：${formatInvokeError(e)}`,
        }
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusyAction(null);
      setProgress(null);
      releaseRepos(targetPaths);
    }

    void finishBatch(t("commit.changelistDone", { ok, fail }), { ok, fail, errors }, t("commit.pushOk"), targetPaths);
  };

  const runPull = async () => {
    if (pullableRepos.length === 0) {
      onToast("error", t("commit.changelistPullNeedBehind"));
      return;
    }
    const targetPaths = pullableRepos.map((p) => p.path);
    holdRepos(targetPaths);
    setBusyAction("pull");
    setProgress({ done: 0, total: targetPaths.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    const diverged: string[] = [];
    try {
      const result = await mapPoolCounted(
        targetPaths,
        1,
        async (repoPath) => {
          noteRepo(repoPath);
          try {
            await invokeGitPull(repoPath);
          } catch (e) {
            if (invokeErrorCode(e) === "GIT_DIVERGED") diverged.push(repoPath);
            throw e;
          }
        },
        {
          onProgress: (done, total) => setProgress((prev) => ({ done, total, label: prev?.label })),
          describeError: (repoPath, e) => `${labelPath(repoPath)}：${formatInvokeError(e)}`,
        }
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
      // Fast-forward was impossible: merging is offered, never done unasked.
      if (diverged.length > 0) {
        setBusyAction(null);
        const merge = await confirm({
          title: t("commit.pullDivergedTitle"),
          message: t("commit.pullDivergedMessage", { names: diverged.map(labelPath).join("、") }),
          confirmText: t("commit.pullDivergedMerge"),
          icon: "warning",
        });
        if (merge) {
          setBusyAction("pull");
          setProgress({ done: 0, total: diverged.length });
          const merged = await mapPoolCounted(
            diverged,
            1,
            async (repoPath) => {
              noteRepo(repoPath);
              await mergePull(repoPath);
            },
            {
              onProgress: (done, total) => setProgress((prev) => ({ done, total, label: prev?.label })),
              describeError: (repoPath, e) => `${labelPath(repoPath)}：${formatInvokeError(e)}`,
            }
          );
          ok += merged.ok;
          fail = fail - diverged.length + merged.fail;
          const divergedLabels = new Set(diverged.map((p) => `${labelPath(p)}：`));
          errors = [...errors.filter((line) => ![...divergedLabels].some((l) => line.startsWith(l))), ...merged.errors];
        }
      }
    } finally {
      setBusyAction(null);
      setProgress(null);
      releaseRepos(targetPaths);
    }

    void finishBatch(t("commit.changelistDone", { ok, fail }), { ok, fail, errors }, t("commit.pullOk"), targetPaths);
  };

  const statusLabel = (code: string) => {
    const c = code.toUpperCase();
    if (c === "A") return t("commit.statusAdded");
    if (c === "D") return t("commit.statusDeleted");
    if (c === "R") return t("commit.statusRenamed");
    if (c === "C") return t("commit.statusCopied");
    if (c === "?" || c === "!") return t("commit.statusUntracked");
    if (c === "U") return t("commit.statusConflict");
    return t("commit.statusModified");
  };

  const renderRepo = (p: RecentProject) => {
    const entries = statuses[p.path] || [];
    // Dirty summary with zero parsed entries (e.g. porcelain parse miss) — still show the repo.
    if (entries.length === 0) {
      const summary = summaries[p.path];
      if (!summary?.isGit || (summary.dirtyCount ?? 0) <= 0) return null;
      return (
        <div key={p.path} className={`cl-repo ${currentGitRepo === p.path ? "is-current" : ""}`}>
          <div className="cl-repo-head">
            <span className="cl-repo-title" title={p.path}>
              {p.name || projectNameFromPath(p.path)}
            </span>
            {summary.branch && (
              <span className="commit-branch-badge" title={summary.branch}>
                <GitBranch size={12} />
                <span className="commit-branch-text">{summary.branch}</span>
              </span>
            )}
            <span className="repos-status-pill">{summary.dirtyCount}</span>
          </div>
          <div className="runtime-muted" style={{ padding: "6px 12px 10px" }}>
            {t("commit.changelistStatusParseEmpty")}
          </div>
        </div>
      );
    }
    const summary = summaries[p.path];
    const isCollapsed = collapsed.has(p.path);
    const sel = selected.get(p.path) || new Set<string>();
    const selectable = entries.filter((e) => e.group !== "conflicted");
    const allSelected = selectable.length > 0 && selectable.every((e) => sel.has(selectionKey(e)));
    const someSelected = selectable.some((e) => sel.has(selectionKey(e)));
    // Conflicts first: they are what blocks the repo.
    const ordered = [...entries].sort(
      (a, b) => Number(b.group === "conflicted") - Number(a.group === "conflicted")
    );
    const Chevron = isCollapsed ? ChevronRight : ChevronDown;
    const isCurrent = currentGitRepo === p.path;

    return (
      <div key={p.path} className={`cl-repo ${isCurrent ? "is-current" : ""}`}>
        <div className="cl-repo-head">
          <button
            type="button"
            className="cl-repo-toggle"
            onClick={() => toggleCollapsed(p.path)}
            aria-expanded={!isCollapsed}
          >
            <Chevron size={14} />
          </button>
          <IndeterminateCheckbox
            checked={allSelected}
            indeterminate={someSelected && !allSelected}
            disabled={busy}
            onChange={() => toggleRepo(p.path, entries)}
          />
          <button
            type="button"
            className="cl-repo-title"
            title={p.path}
            onClick={() => setCurrentGitRepo(p.path)}
          >
            {p.name || projectNameFromPath(p.path)}
          </button>
          {summary?.branch && (
            <span className="commit-branch-badge" title={summary.branch}>
              <GitBranch size={12} />
              <span className="commit-branch-text">{summary.branch}</span>
            </span>
          )}
          <span className="repos-status-pill">{entries.length}</span>
        </div>
        {!isCollapsed && (
          <ul className="cl-file-list">
            {ordered.map((e) => {
              const code = statusCode(e);
              const conflicted = e.group === "conflicted";
              const checked = !conflicted && sel.has(selectionKey(e));
              const isDiff =
                diffTarget?.repoPath === p.path && diffTarget.filePath === e.path;
              return (
                <li
                  key={fileKey(p.path, e.path, e.group)}
                  className={`cl-file ${isDiff ? "is-selected" : ""}${conflicted ? " is-conflict" : ""}`}
                  title={conflicted ? t("commit.conflictFileTip") : undefined}
                  onClick={() => {
                    if (conflicted) {
                      setResolver({ repoPath: p.path, file: e.path });
                      return;
                    }
                    if (!checked) toggleFile(p.path, e);
                    void loadDiff(p.path, e);
                  }}
                  onDoubleClick={() => {
                    if (conflicted) return;
                    openDiffModal({
                      repoPath: p.path,
                      filePath: e.path,
                      staged: e.group === "staged",
                      wholeFile: e.group === "untracked",
                    });
                  }}
                >
                  <IndeterminateCheckbox
                    checked={checked}
                    indeterminate={false}
                    disabled={busy || conflicted}
                    onChange={() => toggleFile(p.path, e)}
                  />
                  <span className={`git-file-badge ${statusClass(code)}`}>{statusLabel(code)}</span>
                  {looksLikeTempFile(e.path) && (
                    <span className="cl-file-temp" title={t("commit.tempBadgeTip")}>
                      {t("commit.tempBadge")}
                    </span>
                  )}
                  {e.group === "staged" && (
                    <span className="cl-file-staged">{t("commit.statusStaged")}</span>
                  )}
                  <span className="cl-file-path" title={absoluteFilePath(p.path, e.path)}>
                    <span className="cl-file-name">{e.path.slice(e.path.lastIndexOf("/") + 1)}</span>
                    {e.path.includes("/") && (
                      <span className="cl-file-dir">
                        {e.path.slice(0, e.path.lastIndexOf("/"))}
                      </span>
                    )}
                  </span>
                  <button
                    type="button"
                    className="btn commit-icon-btn cl-file-discard"
                    hidden={conflicted}
                    disabled={busy}
                    title={t("commit.discard")}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      void runDiscard(p.path, e);
                    }}
                  >
                    <Undo2 size={12} />
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    );
  };

  return (
    <div className="commit-changelist">
      <div className="cl-toolbar">
        <select
          className="input-field commit-batch-filter"
          value={filterIsGroup ? filter : scopeFilterForRepo(currentGitRepo, gitWorkspaces) ?? "other"}
          onChange={(e) => setFilter(e.target.value)}
          disabled={busy || gitWorkspaces.length === 0}
          hidden={gitWorkspaces.length === 0}
        >
          {gitWorkspaces.map((ws) => (
            <option key={ws.id} value={`ws-${ws.id}`}>
              {ws.name}
            </option>
          ))}
          {gitWorkspaces.length > 0 && (
            <option value="other">{t("workspace.other")}</option>
          )}
        </select>
        <button
          type="button"
          className="btn commit-icon-btn"
          disabled={loading || busy}
          onClick={() => void refresh()}
          title={t("commit.refresh")}
        >
          {loading ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
          <span>{t("commit.refresh")}</span>
        </button>
        <span className="repos-batch-count">
          {t("commit.changelistSelected", { count: selectedCount })}
        </span>
        {progress && busyAction && (
          <span className="runtime-muted">
            {t(`commit.running.${busyAction}`)}{" "}
            {t("commit.changelistRunning", {
              done: progress.done,
              total: progress.total,
              name: progress.label ? ` · ${progress.label}` : "",
            })}
          </span>
        )}
        <div
          className="cl-toolbar-model"
          title={aiModels.length === 0 ? t("commit.generateNeedModel") : undefined}
        >
          <ModelSelector
            models={aiModels}
            selectedId={selectedModelId || defaultModel?.id || ""}
            onChange={onModelChange}
            disabled={busy || generating || aiModels.length === 0}
          />
        </div>
      </div>

      {operationRepos.length > 0 && (
        <div className="cl-op-banners">
          {operationRepos.map((p) => {
            const s = summaries[p.path];
            const count = s?.conflictCount ?? 0;
            const name = labelPath(p.path);
            const ready = mergeCommitTargets.includes(p.path);
            const text = ready
              ? t("commit.mergeReadyBanner", { name })
              : s?.operation === "merge"
                ? t("commit.mergeBanner", { name, count })
                : s?.operation
                  ? t("commit.opBanner", { name, op: t(`commit.op.${s.operation}`) })
                  : t("commit.conflictOnlyBanner", { name, count });
            return (
              <div key={p.path} className={`cl-op-banner${ready ? " is-ready" : ""}`} role="status">
                <GitMerge size={14} aria-hidden />
                <span className="cl-op-text">{text}</span>
                {count > 0 && (
                  <button
                    type="button"
                    className="btn btn-primary btn-small"
                    disabled={busy}
                    onClick={() => setResolver({ repoPath: p.path })}
                  >
                    {t("commit.resolveConflicts")}
                  </button>
                )}
                {ready && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    disabled={busy || generating}
                    onClick={() => void fillMergeMessage(p.path)}
                  >
                    {t("commit.useMergeMessage")}
                  </button>
                )}
                {s?.operation === "merge" && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    disabled={busy}
                    onClick={() => void runMergeAbort(p.path)}
                  >
                    {busyAction === "merge" && <Loader2 size={13} className="spin" />}
                    {t("commit.abortMerge")}
                  </button>
                )}
              </div>
            );
          })}
        </div>
      )}

      <div className="cl-split">
        <div className="cl-split-tree">
          {(loading || !loadedOnce.current) && dirtyRepos.length === 0 ? (
            <div className="cl-panel cl-empty">
              <div className="cl-empty-body">
                <Loader2 size={18} className="spin" />
                <div className="cl-empty-title">{t("commit.changelistLoading")}</div>
              </div>
            </div>
          ) : dirtyRepos.length === 0 ? (
            <div className="cl-panel cl-empty">
              <div className="cl-empty-body">
                {filter !== "all" && allDirtyRepos.length > 0 ? (
                  <>
                    <div className="cl-empty-title">{t("commit.changelistEmptyFiltered")}</div>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => {
                        const next = scopeFilterForRepo(allDirtyRepos[0]?.path, gitWorkspaces);
                        if (next) setFilter(next);
                      }}
                    >
                      {t("commit.changelistShowAllDirty", { count: allDirtyRepos.length })}
                    </button>
                  </>
                ) : (
                  <>
                    <div className="cl-empty-title-row">
                      <div className="cl-empty-title">
                        {currentGitRepo
                          ? t("commit.changelistEmptyTitle")
                          : t("commit.changelistEmpty")}
                      </div>
                      {currentGitRepo && currentSummary?.branch && (
                        <span className="commit-branch-badge" title={currentSummary.branch}>
                          <GitBranch size={12} />
                          <span className="commit-branch-text">{currentSummary.branch}</span>
                        </span>
                      )}
                    </div>
                    {currentGitRepo && (
                      <div
                        className="cl-empty-path runtime-muted"
                        title={currentGitRepo}
                      >
                        {projectNameFromPath(currentGitRepo)}
                      </div>
                    )}
                    {currentGitRepo && !currentIsFavorited && (
                      <div className="cl-empty-note runtime-muted">
                        {t("commit.changelistEmptyUnfavorited")}
                      </div>
                    )}
                    {currentSummary?.isGit && currentSummary.behind > 0 && (
                      <div className="cl-empty-note cl-commit-hint-warn">
                        {t("commit.behindHint", { count: currentSummary.behind })}
                      </div>
                    )}
                    {currentSummary?.isGit && currentSummary.behind === 0 && currentSummary.ahead > 0 && (
                      <div className="cl-empty-note">
                        {t("commit.aheadHint", { count: currentSummary.ahead })}
                      </div>
                    )}
                    {currentSummary?.isGit &&
                      currentSummary.hasUpstream &&
                      currentSummary.ahead === 0 &&
                      currentSummary.behind === 0 && (
                        <div className="cl-empty-note runtime-muted">
                          {t("commit.changelistInSync")}
                        </div>
                      )}
                    <button
                      type="button"
                      className="btn btn-secondary btn-small cl-empty-action"
                      disabled={loading || busy}
                      onClick={() => void refresh()}
                    >
                      {loading ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
                      {t("commit.refresh")}
                    </button>
                    <div className="cl-empty-hint runtime-muted">
                      {t("commit.changelistEmptyHint")}
                    </div>
                  </>
                )}
              </div>
            </div>
          ) : (
            <div className="cl-panel cl-tree">{dirtyRepos.map(renderRepo)}</div>
          )}
        </div>
        <DiffViewer
          active={active}
          target={diffTarget}
          text={diffText}
          loading={diffLoading}
          statusLabel={diffEntry ? statusLabel(statusCode(diffEntry)) : undefined}
          statusClass={diffEntry ? statusClass(statusCode(diffEntry)) : undefined}
          busy={busy}
          ignoreWhitespace={ignoreWs}
          onIgnoreWhitespace={(on) => {
            setIgnoreWs(on);
            writeStoredString(CL_IGNORE_WS_KEY, on ? "1" : "0");
            if (diffTarget) void reloadDiff(diffTarget, on);
          }}
          onApply={(patch) => void applyHunks(patch)}
          onExpand={() => openDiffModal(diffTarget)}
          onClose={closeDiff}
        />
      </div>

      {report && (
        <div className="cl-report" role="alert">
          <div className="cl-report-head">
            <AlertTriangle size={14} aria-hidden />
            <span className="cl-report-title">{report.title}</span>
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={() => setReport(null)}
              title={t("commit.reportClose")}
              aria-label={t("commit.reportClose")}
            >
              <X size={14} />
            </button>
          </div>
          <ul className="cl-report-list">
            {report.lines.map((line, i) => (
              <li key={i}>{line}</li>
            ))}
          </ul>
        </div>
      )}

      <div className="cl-commit-bar">
        <div className="cl-composer">
          <textarea
            className="git-commit-message cl-commit-msg"
            rows={2}
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            placeholder={t("commit.messagePlaceholder")}
            disabled={busy || generating}
          />
          <div className="cl-composer-foot">
            <div className="cl-recent" ref={recentRef}>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={busy || generating}
                title={t("commit.recentMessagesTip")}
                onClick={() => void openRecent()}
              >
                <History size={13} />
                {t("commit.recentMessages")}
              </button>
              {recentOpen && (
                <div className="cl-recent-menu">
                  <div className="cl-menu-note runtime-muted">{t("commit.recentMessagesTip")}</div>
                  {recentSubjects.length === 0 ? (
                    <div className="cl-recent-empty runtime-muted">{t("commit.recentEmpty")}</div>
                  ) : (
                    recentSubjects.map((subject) => (
                      <button
                        key={subject}
                        type="button"
                        className="cl-recent-item"
                        onClick={() => {
                          setMessage(subject);
                          setRecentOpen(false);
                        }}
                      >
                        {subject}
                      </button>
                    ))
                  )}
                </div>
              )}
            </div>
            <div className="cl-mode" role="radiogroup" aria-label={t("commit.amendMode")}>
              <button
                type="button"
                role="radio"
                aria-checked={!amend}
                className={!amend ? "is-on" : ""}
                disabled={busy || generating}
                onClick={() => setAmend(false)}
              >
                {t("commit.commitNew")}
              </button>
              <button
                type="button"
                role="radio"
                aria-checked={amend}
                className={amend ? "is-on is-amend" : ""}
                disabled={busy || generating}
                onClick={() => setAmend(true)}
              >
                {t("commit.commitAmend")}
              </button>
            </div>
            <p className={`cl-mode-hint${amend ? " is-amend" : ""}`}>
              {amend ? t("commit.amendHint") : t("commit.commitNewHint")}
            </p>
            <div className="cl-composer-side">
              <div className="cl-stash" ref={stashRef}>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  disabled={busy || generating || !currentGitRepo}
                  title={t("commit.stashPushTip")}
                  onClick={() => {
                    setRecentOpen(false);
                    void runStashPush();
                  }}
                >
                  <Archive size={13} />
                  {t("commit.stashPush")}
                </button>
                <button
                  type="button"
                  className={`btn btn-secondary btn-small cl-stash-count${stashOpen ? " is-on" : ""}`}
                  disabled={!currentGitRepo}
                  title={t("commit.stashListTip")}
                  aria-expanded={stashOpen}
                  aria-label={t("commit.stashList", { count: stashes.length })}
                  onClick={() => {
                    setRecentOpen(false);
                    setStashOpen((open) => !open);
                  }}
                >
                  {stashes.length}
                </button>
                {stashOpen && (
                  <div className="cl-recent-menu cl-stash-menu">
                    <div className="cl-menu-note runtime-muted">{t("commit.stashPushTip")}</div>
                    {stashes.length === 0 ? (
                      <div className="cl-recent-empty runtime-muted">{t("commit.stashEmpty")}</div>
                    ) : (
                      stashes.map((entry) => (
                        <div key={entry.index} className="cl-stash-row">
                          <span className="cl-stash-msg">{entry.message || t("commit.stashUntitled")}</span>
                          <button
                            type="button"
                            className="btn btn-secondary btn-small"
                            disabled={busy}
                            onClick={() => void runStashPop(entry.index)}
                          >
                            {t("commit.stashPop")}
                          </button>
                          <button
                            type="button"
                            className="btn btn-secondary btn-small"
                            disabled={busy}
                            onClick={() => void runStashDrop(entry.index, entry.message)}
                            aria-label={t("commit.stashDrop")}
                          >
                            <Trash2 size={13} />
                          </button>
                        </div>
                      ))
                    )}
                  </div>
                )}
              </div>
            </div>
          </div>
        </div>
        <div className="cl-commit-foot">
          <div className="cl-commit-actions-secondary">
            <button
              type="button"
              className="btn btn-secondary cl-commit-ai"
              disabled={busy || generating || selectedCount === 0}
              onClick={() => void runGenerateMessage()}
              title={selectedCount === 0 ? t("commit.changelistNeedFiles") : t("commit.generate")}
            >
              {generating ? <Loader2 size={14} className="spin" /> : <Sparkles size={14} />}
              {generating ? t("commit.generating") : t("commit.generate")}
            </button>
            <button
              type="button"
              className="btn btn-secondary cl-commit-quiet"
              disabled={busy || generating || selectedCount === 0}
              onClick={() => void runRollbackChecked()}
              title={selectedCount > 0 ? t("commit.rollbackCheckedTitle") : t("commit.rollbackCheckedNeedFiles")}
            >
              {busyAction === "rollback" ? <Loader2 size={14} className="spin" /> : <Undo2 size={14} />}
              {busyAction === "rollback" ? runningLabel("rollback") : t("commit.rollbackChecked")}
            </button>
            {onUndoLastCommit ? (
              <button
                type="button"
                className="btn btn-secondary cl-commit-quiet"
                disabled={busy || generating || undoing || undoDisabled}
                onClick={() => onUndoLastCommit()}
                title={undoTitle || t("commit.undoLast")}
              >
                {undoing ? <Loader2 size={14} className="spin" /> : <RotateCcw size={14} />}
                {t("commit.undoLast")}
              </button>
            ) : null}
          </div>
          <div className="cl-commit-status">
            {commitBlockReason && <div className="cl-commit-hint">{commitBlockReason}</div>}
            {selectedRepoCount > 1 && (
              <div className="cl-commit-hint runtime-muted">
                {t("commit.changelistSameMessageHint", { count: selectedRepoCount })}
              </div>
            )}
            {pullableCount > 0 && (
              <div className="cl-commit-hint cl-commit-hint-warn">
                {t("commit.behindHint", { count: behindTotal })}
              </div>
            )}
            {pushableCount > 0 && pullableCount === 0 && (
              <div className="cl-commit-hint">
                {t("commit.aheadHint", {
                  count: pushableRepos.reduce((n, p) => n + (summaries[p.path]?.ahead ?? 0), 0),
                })}
              </div>
            )}
          </div>
          <div className="cl-commit-actions-primary">
            <div className="cl-commit-sync">
            <button
              type="button"
              className="btn btn-secondary"
              disabled={busy || generating || pullableCount === 0}
              onClick={() => void runPull()}
              title={
                pullableCount > 0
                  ? `${t("commit.behindHint", { count: behindTotal })} · ${t("commit.scopeFilter")}`
                  : t("commit.changelistPullNeedBehind")
              }
            >
              {busyAction === "pull" ? <Loader2 size={14} className="spin" /> : <Download size={14} />}
              {busyAction === "pull" ? runningLabel("pull") : t("commit.changelistPull")}
            </button>
            <button
              type="button"
              className="btn btn-secondary"
              disabled={!canCommit}
              title={commitBlockReason ?? t("commit.changelistShortcutCommit")}
              onClick={() => void runCommit(false)}
            >
              {busyAction === "commit" ? <Loader2 size={14} className="spin" /> : <Check size={14} />}
              {busyAction === "commit"
                ? runningLabel("commit")
                : amend
                  ? t("commit.commitAmendAction")
                  : mergeCommitTargets.length > 0
                    ? t("commit.commitMerge")
                    : t("commit.changelistCommit")}
            </button>
            <button
              type="button"
              className="btn btn-secondary"
              disabled={busy || generating || pushableCount === 0}
              onClick={() => void runPush()}
              title={
                pushableCount > 0
                  ? `${t("commit.aheadHint", {
                      count: pushableRepos.reduce((n, p) => n + (summaries[p.path]?.ahead ?? 0), 0),
                    })}${
                      firstPushRepos.length > 0
                        ? ` · ${t("commit.pushFirstCount", { count: firstPushRepos.length })}`
                        : ""
                    } · ${t("commit.scopeFilter")}`
                  : t("commit.changelistPushNeedAhead")
              }
            >
              {busyAction === "push" ? <Loader2 size={14} className="spin" /> : <Upload size={14} />}
              {busyAction === "push" ? runningLabel("push") : t("commit.changelistPush")}
            </button>
            </div>
            <button
              type="button"
              className="btn btn-primary"
              disabled={!canCommit}
              title={commitBlockReason ?? t("commit.changelistShortcutCommitPush")}
              onClick={() => void runCommit(true)}
            >
              {busyAction === "commitPush" ? <Loader2 size={14} className="spin" /> : <Upload size={14} />}
              {busyAction === "commitPush"
                ? runningLabel("commitPush")
                : amend
                  ? t("commit.commitAmendPush")
                  : mergeCommitTargets.length > 0
                    ? t("commit.commitMergePush")
                    : t("commit.changelistCommitPush")}
            </button>
          </div>
        </div>
      </div>
      {diffModal && (
        <DiffModal
          files={diffModal.files}
          startIndex={diffModal.index}
          busy={busy}
          onApply={(file, patch) => applyPatch(file, patch)}
          onClose={() => setDiffModal(null)}
        />
      )}
      {resolver && (
        <MergeResolver
          repoPath={resolver.repoPath}
          repoName={labelPath(resolver.repoPath)}
          initialFile={resolver.file}
          onToast={onToast}
          onClose={() => {
            const repoPath = resolver.repoPath;
            setResolver(null);
            void refreshAfterWrite([repoPath]);
          }}
          onResolved={() => void refreshAfterWrite([resolver.repoPath])}
        />
      )}
    </div>
  );
}

export default CommitChangelist;
