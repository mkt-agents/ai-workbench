import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
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
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import { findWorkspaceForRepo, pathKey, projectNameFromPath } from "../core/pathUtils";
import { useConfirm, useConfirmChoice } from "./ConfirmModal";
import type { AIModelConfig, GitRepoSummary, GitStatusEntry, RecentProject } from "../core/types";

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

const CONCURRENCY = 4;
const AI_CONTEXT_MAX = 12_000;

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

async function mapPoolCounted(
  paths: string[],
  fn: (path: string) => Promise<void>,
  onProgress?: (done: number, total: number) => void
): Promise<{ ok: number; fail: number; errors: string[] }> {
  let ok = 0;
  let fail = 0;
  let done = 0;
  const errors: string[] = [];
  const total = paths.length;
  await mapPool(paths, CONCURRENCY, async (path) => {
    try {
      await fn(path);
      ok += 1;
    } catch (e) {
      fail += 1;
      errors.push(formatInvokeError(e));
    } finally {
      done += 1;
      onProgress?.(done, total);
    }
  });
  return { ok, fail, errors };
}

function isRepoDirty(
  path: string,
  summaries: Record<string, GitRepoSummary>,
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

/** Workspace filter value that includes `repoPath`. */
function filterValueForRepo(
  repoPath: string,
  workspaces: { id: number; path: string }[]
): string {
  const ws = findWorkspaceForRepo(repoPath, workspaces);
  if (ws) return `ws-${ws.id}`;
  if (workspaces.length > 0) return "other";
  return "all";
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
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  return (
    <pre className="git-diff-pre">
      {lines.map((line, i) => (
        <span key={i} className={diffLineClass(line)}>
          {line || "\u00a0"}
        </span>
      ))}
    </pre>
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
  onDirtyScopeChange?: (paths: string[]) => void;
  /** Parent reloads header summary when changelist finishes a refresh. */
  onRefreshed?: () => void;
  refreshNonce?: number;
  onUndoLastCommit?: () => void;
  undoDisabled?: boolean;
  undoTitle?: string;
  undoing?: boolean;
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

function CommitChangelist({
  active,
  onToast,
  onDirtyScopeChange,
  onRefreshed,
  refreshNonce = 0,
  onUndoLastCommit,
  undoDisabled,
  undoTitle,
  undoing,
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
  const invokeGitRepoSummary = useGlobalStore((s) => s.invokeGitRepoSummary);
  const invokeGitStatus = useGlobalStore((s) => s.invokeGitStatus);
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
  const loadAIModels = useGlobalStore((s) => s.loadAIModels);

  const [filter, setFilter] = useState("all");
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [message, setMessage] = useState("");
  const [summaries, setSummaries] = useState<Record<string, GitRepoSummary>>({});
  const [statuses, setStatuses] = useState<Record<string, GitStatusEntry[]>>({});
  const [selected, setSelected] = useState<Map<string, Set<string>>>(() => new Map());
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [diffTarget, setDiffTarget] = useState<DiffTarget | null>(null);
  const [diffText, setDiffText] = useState("");
  const [diffLoading, setDiffLoading] = useState(false);
  const refreshGen = useRef(0);
  const diffGen = useRef(0);

  const scanProjects = useMemo(
    () => projectsForScan(recentProjects, currentGitRepo),
    [recentProjects, currentGitRepo]
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
    setLoading(true);
    try {
      await Promise.all([loadRecentProjects().catch(() => {}), loadWorkspaces().catch(() => {})]);
      if (gen !== refreshGen.current) return;
      const state = useGlobalStore.getState();
      const projects = projectsForScan(state.recentProjects, state.settings.currentGitRepo);
      const nextSummaries: Record<string, GitRepoSummary> = {};
      await mapPool(projects, CONCURRENCY, async (p) => {
        try {
          nextSummaries[p.path] = await invokeGitRepoSummary(p.path);
        } catch (e) {
          nextSummaries[p.path] = {
            path: p.path,
            name: p.name || projectNameFromPath(p.path),
            branch: "",
            dirtyCount: 0,
            ahead: 0,
            behind: 0,
            hasUpstream: false,
            userName: "",
            userEmail: "",
            isGit: false,
            error: String(e),
          };
        }
      });
      if (gen !== refreshGen.current) return;
      setSummaries(nextSummaries);

      const dirty = projects.filter(
        (p) => nextSummaries[p.path]?.isGit && (nextSummaries[p.path]?.dirtyCount ?? 0) > 0
      );
      const nextStatuses: Record<string, GitStatusEntry[]> = {};
      const nextSelected = new Map<string, Set<string>>();
      const statusErrors: string[] = [];
      await mapPool(dirty, CONCURRENCY, async (p) => {
        try {
          const entries = await invokeGitStatus(p.path);
          nextStatuses[p.path] = entries;
          nextSelected.set(p.path, new Set(entries.map((e) => selectionKey(e))));
        } catch (e) {
          nextStatuses[p.path] = [];
          statusErrors.push(`${p.name || projectNameFromPath(p.path)}: ${formatInvokeError(e)}`);
        }
      });
      if (gen !== refreshGen.current) return;
      setStatuses(nextStatuses);
      setSelected(nextSelected);
      setDiffTarget(null);
      setDiffText("");
      if (statusErrors.length > 0) {
        onToast(
          "error",
          t("commit.changelistStatusFetchFail", {
            detail: truncateError(statusErrors.slice(0, 2).join("；"), 160),
          })
        );
      }
      if (!state.settings.currentGitRepo && dirty[0]) {
        setCurrentGitRepo(dirty[0].path);
      }
      onRefreshed?.();
    } finally {
      if (gen === refreshGen.current) setLoading(false);
    }
  }, [
    invokeGitRepoSummary,
    invokeGitStatus,
    loadRecentProjects,
    loadWorkspaces,
    onRefreshed,
    onToast,
    setCurrentGitRepo,
    t,
  ]);

  useEffect(() => {
    if (active) void refresh();
  }, [active, refresh, refreshNonce]);

  useEffect(() => {
    if (active && aiModels.length === 0) void loadAIModels().catch(() => {});
  }, [active, aiModels.length, loadAIModels]);

  const defaultModel = useMemo(
    () => aiModels.find((m) => m.isDefault) || aiModels[0] || null,
    [aiModels]
  );

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

  // Wrong filter (e.g.「其它仓库」) hides dirty repos — jump to a filter that shows them.
  useEffect(() => {
    if (!active || loading) return;
    if (filter === "all") return;
    if (dirtyRepos.length > 0) return;
    if (allDirtyRepos.length === 0) return;
    const prefer =
      (currentGitRepo && findProjectByPath(allDirtyRepos, currentGitRepo)) || allDirtyRepos[0];
    if (!prefer) {
      setFilter("all");
      return;
    }
    setFilter(filterValueForRepo(prefer.path, gitWorkspaces));
  }, [
    active,
    loading,
    filter,
    dirtyRepos.length,
    allDirtyRepos,
    currentGitRepo,
    gitWorkspaces,
  ]);

  // currentGitRepo only changes on user click (or first-time unset during refresh) — never steal it.

  useEffect(() => {
    onDirtyScopeChange?.(dirtyRepos.map((p) => p.path));
  }, [dirtyRepos, onDirtyScopeChange]);

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
    return scopedProjects.filter((p) => (summaries[p.path]?.ahead ?? 0) > 0);
  }, [scopedProjects, summaries]);

  const pushableCount = pushableRepos.length;

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

  const toggleFile = (repoPath: string, entry: GitStatusEntry) => {
    setCurrentGitRepo(repoPath);
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
    setCurrentGitRepo(repoPath);
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
      if (diffTarget?.repoPath === repoPath && diffTarget.filePath === entry.path) {
        setDiffTarget(null);
        setDiffText("");
      }
      await refresh();
    } catch (e) {
      onToast("error", formatInvokeError(e));
    }
  };

  const runGenerateMessage = async () => {
    if (busy || generating) return;
    if (!defaultModel) {
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

      if (selectedPairs.length > 0) {
        for (const { repoPath, repoName, entry } of selectedPairs) {
          const label = `${repoName}/${entry.path}`;
          fileLabels.push(label);
          let diff = "";
          try {
            diff = await invokeGitDiff(repoPath, entry.path, entry.group === "staged");
          } catch {
            diff = entry.group === "untracked" ? "(untracked file)" : "";
          }
          diffParts.push(`--- ${label} ---\n${diff || "(no diff)"}`);
        }
      } else {
        source = "workspace";
        for (const p of dirtyRepos) {
          try {
            const ctx = await invokeGitCommitContext(p.path);
            const repoName = p.name || projectNameFromPath(p.path);
            for (const f of ctx.files) fileLabels.push(`${repoName}/${f}`);
            diffParts.push(
              `=== ${repoName} (${ctx.source}) ===\n${ctx.diff || "(no diff)"}`
            );
          } catch {
            /* skip repo */
          }
        }
      }

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
        config: defaultModel as AIModelConfig,
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

    // Commit & push while behind remote
    if (withPush) {
      const behindRepos = repos
        .map(([path]) => path)
        .filter((path) => (summaries[path]?.behind ?? 0) > 0);
      if (behindRepos.length > 0) {
        const behindCount = behindRepos.reduce(
          (n, path) => n + (summaries[path]?.behind ?? 0),
          0
        );
        const ok = await confirm({
          title: t("commit.behindBeforePushTitle"),
          message: t("commit.behindHint", { count: behindCount }),
          warning: t("commit.behindBeforePushWarn"),
          confirmText: t("commit.behindBeforePushContinue"),
          icon: "warning",
        });
        if (!ok) return;
      }
    }

    setBusy(true);
    setProgress({ done: 0, total: repos.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        repos.map(([path]) => path),
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
        (done, total) => setProgress({ done, total })
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusy(false);
      setProgress(null);
    }

    if (fail > 0) {
      const detail = errors[0] ? `: ${truncateError(errors[0])}` : "";
      onToast("error", `${t("commit.changelistDone", { ok, fail })}${detail}`);
    } else {
      onToast("success", t("commit.changelistDone", { ok, fail }));
      setMessage("");
    }
    void refresh();
  };

  const runPush = async () => {
    if (pushableRepos.length === 0) {
      onToast("error", t("commit.changelistPushNeedAhead"));
      return;
    }
    const behindAmong = pushableRepos.filter((p) => (summaries[p.path]?.behind ?? 0) > 0);
    if (behindAmong.length > 0) {
      const behindCount = behindAmong.reduce((n, p) => n + (summaries[p.path]?.behind ?? 0), 0);
      const ok = await confirm({
        title: t("commit.behindBeforePushTitle"),
        message: t("commit.behindHint", { count: behindCount }),
        warning: t("commit.behindBeforePushWarn"),
        confirmText: t("commit.behindBeforePushContinue"),
        icon: "warning",
      });
      if (!ok) return;
    }
    setBusy(true);
    setProgress({ done: 0, total: pushableRepos.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        pushableRepos.map((p) => p.path),
        async (repoPath) => {
          await invokeGitPush(repoPath);
        },
        (done, total) => setProgress({ done, total })
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusy(false);
      setProgress(null);
    }

    if (fail > 0) {
      const detail = errors[0] ? `: ${truncateError(errors[0])}` : "";
      onToast("error", `${t("commit.changelistDone", { ok, fail })}${detail}`);
    } else {
      onToast("success", t("commit.pushOk"));
    }
    void refresh();
  };

  const runPull = async () => {
    if (pullableRepos.length === 0) {
      onToast("error", t("commit.changelistPullNeedBehind"));
      return;
    }
    setBusy(true);
    setProgress({ done: 0, total: pullableRepos.length });
    let ok = 0;
    let fail = 0;
    let errors: string[] = [];
    try {
      const result = await mapPoolCounted(
        pullableRepos.map((p) => p.path),
        async (repoPath) => {
          await invokeGitPull(repoPath);
        },
        (done, total) => setProgress({ done, total })
      );
      ok = result.ok;
      fail = result.fail;
      errors = result.errors;
    } finally {
      setBusy(false);
      setProgress(null);
    }

    if (fail > 0) {
      const detail = errors[0] ? `: ${truncateError(errors[0])}` : "";
      onToast("error", `${t("commit.changelistDone", { ok, fail })}${detail}`);
    } else {
      onToast("success", t("commit.pullOk"));
    }
    void refresh();
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
      </div>

      <div className="cl-split">
        <div className="cl-split-tree">
          {loading && dirtyRepos.length === 0 ? (
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
            {diffTarget
              ? t("commit.diffTitle", { path: diffTarget.filePath })
              : t("commit.diffEmpty")}
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
                  ? t("commit.behindHint", { count: behindTotal })
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
                  ? t("commit.aheadHint", {
                      count: pushableRepos.reduce((n, p) => n + (summaries[p.path]?.ahead ?? 0), 0),
                    })
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
