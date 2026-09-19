import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Download,
  GitBranch,
  Loader2,
  RefreshCw,
  RotateCcw,
  Sparkles,
  Trash2,
  Upload,
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
import { useConfirm, useConfirmChoice } from "./ConfirmModal";
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
/** Rendering more than this many diff lines as individual spans gets sluggish. */
const DIFF_RENDER_LINES = 2000;

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

function statusCode(entry: GitStatusEntry): string {
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
  return "git-status-m";
}

function formatInvokeError(err: unknown): string {
  if (err instanceof Error) {
    const m = err.message.trim();
    return m || "操作失败";
  }
  const text = String(err ?? "").trim();
  return text || "操作失败";
}

function truncateError(err: string, max = 180): string {
  const s = err.trim();
  if (s.length <= max) return s;
  return `${s.slice(0, max)}…`;
}

function selectionKey(entry: { path: string; group: string }): string {
  return `${entry.group}\0${entry.path}`;
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

function diffLineClass(line: string): string {
  if (line.startsWith("+++ ") || line.startsWith("--- ") || line.startsWith("diff --git") || line.startsWith("index ")) {
    return "diff-line diff-line-meta";
  }
  if (line.startsWith("@@")) return "diff-line diff-line-hunk";
  if (line.startsWith("+")) return "diff-line diff-line-add";
  if (line.startsWith("-")) return "diff-line diff-line-del";
  return "diff-line";
}

function DiffPreview({ text }: { text: string }) {
  const { t } = useTranslation("git");
  const [expanded, setExpanded] = useState(false);
  useEffect(() => {
    setExpanded(false);
  }, [text]);
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const hidden = !expanded && lines.length > DIFF_RENDER_LINES;
  const shown = hidden ? lines.slice(0, DIFF_RENDER_LINES) : lines;
  return (
    <>
      {hidden && (
        <div className="cl-diff-more">
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => setExpanded(true)}
          >
            {t("commit.diffShowAll", { count: lines.length - DIFF_RENDER_LINES })}
          </button>
        </div>
      )}
      <pre className="git-diff-pre">
        {shown.map((line, i) => (
          <span key={i} className={diffLineClass(line)}>
            {line || "\u00a0"}
          </span>
        ))}
      </pre>
    </>
  );
}

type DiffTarget = {
  repoPath: string;
  filePath: string;
  staged: boolean;
};

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
  const invokeGitDiff = useGlobalStore((s) => s.invokeGitDiff);
  const invokeGitDiscard = useGlobalStore((s) => s.invokeGitDiscard);
  const invokeGitCommitContext = useGlobalStore((s) => s.invokeGitCommitContext);
  const invokeSetRepoGitConfig = useGlobalStore((s) => s.invokeSetRepoGitConfig);
  const invokeGenerateText = useGlobalStore((s) => s.invokeGenerateText);
  const aiModels = useGlobalStore((s) => s.aiModels);

  const [filter, setFilter] = useState(() => readStoredString(CL_FILTER_KEY, "all"));
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [message, setMessage] = useState(() => readStoredString(CL_DRAFT_KEY));
  const [summaries, setSummaries] = useState<Record<string, RepoBatchItem>>({});
  const [statuses, setStatuses] = useState<Record<string, GitStatusEntry[]>>({});
  const [selected, setSelected] = useState<Map<string, Set<string>>>(() => new Map());
  const [collapsed, setCollapsed] = useState<Set<string>>(
    () => new Set(readStoredArray<string>(CL_COLLAPSED_KEY))
  );
  const [diffTarget, setDiffTarget] = useState<DiffTarget | null>(null);
  const [diffText, setDiffText] = useState("");
  const [diffLoading, setDiffLoading] = useState(false);
  const refreshGen = useRef(0);
  const diffGen = useRef(0);
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

  const scopedProjects = useMemo(() => {
    if (filter === "all") return scanProjects;
    if (filter === "other") {
      return scanProjects.filter((p) => !findWorkspaceForRepo(p.path, gitWorkspaces));
    }
    const wsId = Number(filter.replace(/^ws-/, ""));
    const ws = gitWorkspaces.find((w) => w.id === wsId);
    if (!ws) return scanProjects;
    return scanProjects.filter((p) => findWorkspaceForRepo(p.path, [ws])?.id === ws.id);
  }, [filter, gitWorkspaces, scanProjects]);

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
  // the repos that do have changes instead of overriding it here.

  // currentGitRepo only changes on user click (or first-time unset during refresh) — never steal it.

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

  const pushableRepos = useMemo(() => {
    return scopedProjects.filter((p) => isPushable(summaries[p.path]));
  }, [scopedProjects, summaries]);

  const pushableCount = pushableRepos.length;

  /** Local branches the batch push would publish for the first time. */
  const firstPushRepos = useMemo(
    () => pushableRepos.filter((p) => !summaries[p.path]?.hasUpstream),
    [pushableRepos, summaries]
  );

  const pullableRepos = useMemo(() => {
    return scopedProjects.filter((p) => {
      const s = summaries[p.path];
      return !!s?.isGit && s.behind > 0 && s.hasUpstream;
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

  const toggleRepo = (repoPath: string, entries: GitStatusEntry[]) => {
    setCurrentGitRepo(repoPath);
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

  const loadDiff = async (repoPath: string, entry: GitStatusEntry) => {
    const staged = entry.group === "staged";
    const gen = ++diffGen.current;
    setDiffTarget({ repoPath, filePath: entry.path, staged });
    setDiffLoading(true);
    try {
      const text = await invokeGitDiff(repoPath, entry.path, staged);
      if (gen !== diffGen.current) return;
      setDiffText(text);
    } catch (e) {
      if (gen !== diffGen.current) return;
      setDiffText(formatInvokeError(e));
    } finally {
      if (gen === diffGen.current) setDiffLoading(false);
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

  const runGenerateMessage = async () => {
    if (busy || generating) return;
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
    const repos = [...selected.entries()].filter(
      ([repoPath, files]) => files.size > 0 && scopedPathKeys.has(pathKey(repoPath))
    );
    if (repos.length === 0) {
      onToast("error", t("commit.changelistNeedFiles"));
      return;
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
    setBusy(true);
    setProgress({ done: 0, total: targetPaths.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        targetPaths,
        GIT_CONCURRENCY,
        async (repoPath) => {
          const files = selected.get(repoPath) || new Set<string>();
          if (files.size === 0) return;
          const entries = statuses[repoPath] || [];
          const selectedEntries = entries.filter((e) => files.has(selectionKey(e)));
          const toUnstage = entries
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
          if (toUnstage.length > 0) {
            await invokeGitUnstage(repoPath, toUnstage);
          }
          if (toStage.length > 0) {
            await invokeGitStage(repoPath, toStage);
          }
          await invokeGitCommit(repoPath, msg);
          if (restageAfter && toUnstage.length > 0) {
            try {
              await invokeGitStage(repoPath, toUnstage);
            } catch {
              /* best-effort restore */
            }
          }
          if (withPush) {
            try {
              await invokeGitPush(repoPath);
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
          onProgress: (done, total) => setProgress({ done, total }),
          describeError: (_path, e) => formatInvokeError(e),
        }
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusy(false);
      setProgress(null);
      releaseRepos(targetPaths);
    }

    if (fail > 0) {
      const detail = errors[0] ? `: ${truncateError(errors[0])}` : "";
      onToast("error", `${t("commit.changelistDone", { ok, fail })}${detail}`);
    } else {
      onToast("success", t("commit.changelistDone", { ok, fail }));
      setMessage("");
    }
    void refreshAfterWrite(targetPaths);
  };

  const runPush = async () => {
    if (pushableRepos.length === 0) {
      onToast("error", t("commit.changelistPushNeedAhead"));
      return;
    }
    const targetPaths = pushableRepos.map((p) => p.path);
    if (!(await confirmBatchPush(targetPaths))) return;
    holdRepos(targetPaths);
    setBusy(true);
    setProgress({ done: 0, total: targetPaths.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        targetPaths,
        GIT_CONCURRENCY,
        async (repoPath) => {
          await invokeGitPush(repoPath);
        },
        {
          onProgress: (done, total) => setProgress({ done, total }),
          describeError: (_path, e) => formatInvokeError(e),
        }
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusy(false);
      setProgress(null);
      releaseRepos(targetPaths);
    }

    if (fail > 0) {
      const detail = errors[0] ? `: ${truncateError(errors[0])}` : "";
      onToast("error", `${t("commit.changelistDone", { ok, fail })}${detail}`);
    } else {
      onToast("success", t("commit.pushOk"));
    }
    void refreshAfterWrite(targetPaths);
  };

  const runPull = async () => {
    if (pullableRepos.length === 0) {
      onToast("error", t("commit.changelistPullNeedBehind"));
      return;
    }
    const targetPaths = pullableRepos.map((p) => p.path);
    holdRepos(targetPaths);
    setBusy(true);
    setProgress({ done: 0, total: targetPaths.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        targetPaths,
        GIT_CONCURRENCY,
        async (repoPath) => {
          await invokeGitPull(repoPath);
        },
        {
          onProgress: (done, total) => setProgress({ done, total }),
          describeError: (_path, e) => formatInvokeError(e),
        }
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusy(false);
      setProgress(null);
      releaseRepos(targetPaths);
    }

    if (fail > 0) {
      const detail = errors[0] ? `: ${truncateError(errors[0])}` : "";
      onToast("error", `${t("commit.changelistDone", { ok, fail })}${detail}`);
    } else {
      onToast("success", t("commit.pullOk"));
    }
    void refreshAfterWrite(targetPaths);
  };

  const statusLabel = (code: string) => {
    const c = code.toUpperCase();
    if (c === "A") return t("commit.statusAdded");
    if (c === "D") return t("commit.statusDeleted");
    if (c === "R") return t("commit.statusRenamed");
    if (c === "C") return t("commit.statusCopied");
    if (c === "?" || c === "!") return t("commit.statusUntracked");
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
    const allSelected = entries.length > 0 && entries.every((e) => sel.has(selectionKey(e)));
    const someSelected = entries.some((e) => sel.has(selectionKey(e)));
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
            {entries.map((e) => {
              const code = statusCode(e);
              const checked = sel.has(selectionKey(e));
              const isDiff =
                diffTarget?.repoPath === p.path && diffTarget.filePath === e.path;
              return (
                <li
                  key={fileKey(p.path, e.path, e.group)}
                  className={`cl-file ${isDiff ? "is-selected" : ""}`}
                  onClick={() => void loadDiff(p.path, e)}
                >
                  <IndeterminateCheckbox
                    checked={checked}
                    indeterminate={false}
                    disabled={busy}
                    onChange={() => toggleFile(p.path, e)}
                  />
                  <span className={`git-file-badge ${statusClass(code)}`}>{statusLabel(code)}</span>
                  <span className="cl-file-path" title={e.path}>
                    {e.path}
                  </span>
                  <button
                    type="button"
                    className="btn commit-icon-btn cl-file-discard"
                    disabled={busy}
                    title={t("commit.discard")}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      void runDiscard(p.path, e);
                    }}
                  >
                    <Trash2 size={12} />
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
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          disabled={busy}
        >
          <option value="all">{t("commit.changelistFilterAll")}</option>
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
        {progress && (
          <span className="runtime-muted">
            {t("commit.changelistRunning", { done: progress.done, total: progress.total })}
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
                      onClick={() => setFilter("all")}
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
        <div className="cl-panel cl-split-diff git-diff-panel">
          <div className="cl-diff-head">
            <span className="cl-diff-head-title">
              {diffTarget
                ? t("commit.diffTitle", { path: diffTarget.filePath })
                : t("commit.diffEmpty")}
            </span>
            {diffTarget && (
              <button
                type="button"
                className="btn commit-icon-btn"
                onClick={closeDiff}
                title={t("commit.diffClose")}
                aria-label={t("commit.diffClose")}
              >
                <X size={14} />
              </button>
            )}
          </div>
          {diffLoading ? (
            <div className="cl-diff-empty">
              <Loader2 size={14} className="spin" /> {t("commit.diffLoading")}
            </div>
          ) : diffText ? (
            <DiffPreview text={diffText} />
          ) : (
            <div className="cl-diff-empty">
              <div className="cl-empty-title">{t("commit.changelistDiffIdle")}</div>
              <div className="cl-empty-hint runtime-muted">{t("commit.changelistDiffHint")}</div>
            </div>
          )}
        </div>
      </div>

      <div className="cl-commit-bar">
        <textarea
          className="git-commit-message cl-commit-msg"
          rows={2}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          placeholder={t("commit.messagePlaceholder")}
          disabled={busy || generating}
        />
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
        <div className="cl-commit-actions">
          <div className="cl-commit-actions-secondary">
            <button
              type="button"
              className="btn btn-secondary cl-commit-ai"
              disabled={busy || generating || (selectedCount === 0 && dirtyRepos.length === 0)}
              onClick={() => void runGenerateMessage()}
              title={t("commit.generate")}
            >
              {generating ? <Loader2 size={14} className="spin" /> : <Sparkles size={14} />}
              {generating ? t("commit.generating") : t("commit.generate")}
            </button>
            {onUndoLastCommit ? (
              <button
                type="button"
                className="btn btn-secondary"
                disabled={busy || generating || undoing || undoDisabled}
                onClick={() => onUndoLastCommit()}
                title={undoTitle || t("commit.undoLast")}
              >
                {undoing ? <Loader2 size={14} className="spin" /> : <RotateCcw size={14} />}
                {t("commit.undoLast")}
              </button>
            ) : null}
          </div>
          <div className="cl-commit-actions-primary">
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
              {busy ? <Loader2 size={14} className="spin" /> : <Download size={14} />}
              {t("commit.changelistPull")}
            </button>
            <button
              type="button"
              className="btn btn-secondary"
              disabled={busy || generating || selectedCount === 0}
              onClick={() => void runCommit(false)}
            >
              {busy ? <Loader2 size={14} className="spin" /> : <Check size={14} />}
              {t("commit.changelistCommit")}
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
              {busy ? <Loader2 size={14} className="spin" /> : <Upload size={14} />}
              {t("commit.changelistPush")}
            </button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || generating || selectedCount === 0}
              onClick={() => void runCommit(true)}
            >
              {busy ? <Loader2 size={14} className="spin" /> : <Upload size={14} />}
              {t("commit.changelistCommitPush")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

export default CommitChangelist;
