import { useCallback, useRef, useState } from "react";
import { useGlobalStore } from "../../core/store";
import { findWorkspaceForRepo } from "../../core/pathUtils";
import type { GitRepoSummary, GitStatusEntry, AiHubChips } from "../../core/types";
import {
  CONTEXT_LIMIT,
  CONTEXT_TTL_MS,
  DIRTY_FILE_LIMIT,
  MAX_DIFF_FILES_PRIMARY,
  MAX_DIFF_FILES_SECONDARY,
  MAX_DIFF_PER_FILE,
  MAX_SECONDARY_REPOS,
  truncate,
} from "./config";
import { formatDailyNotes, localDayStart, type DailyRepoNote } from "./dailyReport";
import type { AskMode } from "./route";

/** Max concurrent git processes to avoid saturating the thread pool. */
const GIT_CONCURRENCY = 4;
/** Several focused services: commit logs from at most this many. */
const MULTI_LOG_REPOS = 4;
/** Several dirty focused services share this many full-diff files in total. */
const MULTI_DIFF_FILE_BUDGET = 12;
const MULTI_DIFF_PER_FILE = 1200;
/** Secondary repos actually quoted in the context; only this many are worth querying. */
const SECONDARY_REPOS_SHOWN = 4;

/** Run async tasks with a concurrency cap to avoid overwhelming the git backend. */
async function withConcurrency<T>(tasks: Array<() => Promise<T>>, limit: number): Promise<T[]> {
  const results: T[] = [];
  let idx = 0;
  async function worker(): Promise<void> {
    while (idx < tasks.length) {
      const i = idx++;
      results[i] = await tasks[i]();
    }
  }
  const workers = Array.from({ length: Math.min(limit, tasks.length) }, () => worker());
  await Promise.all(workers);
  return results;
}

function statusMark(e: GitStatusEntry): string {
  return e.group === "untracked" ? "??" : `${e.indexStatus || " "}${e.workTreeStatus || " "}`;
}

export type ContextPart = keyof AiHubChips;

/** What one request actually carried — shown in the answer source line. */
export type ContextSummary = { repos: string[]; parts: ContextPart[] };

export type ContextResult = { text: string; truncated: boolean; summary: ContextSummary };

const EMPTY_SUMMARY: ContextSummary = { repos: [], parts: [] };

function repoName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}

/**
 * Workbench context collection (workspace / git identity / dirty diffs).
 * with a TTL cache so a burst of sends doesn't re-spawn git for every question.
 *
 * `focusedRepos` are resolved service paths (workspace tokens already expanded
 * by `resolveFocus`). When empty it falls back to the current repo. With a
 * single focus repo, other dirty recent projects are summarised with status +
 * a couple of small diffs; with several, the diff budget is shared between the
 * dirty services and nothing outside the selection is added.
 */
export function useAskContext(chips: AiHubChips, focusedRepos: string[]) {
  const currentGitRepo = useGlobalStore((s) => s.settings.currentGitRepo);
  // 设置项「复用上下文缓存」：关掉时 TTL 归零，每问一次都重新采集
  const contextTtlMs = useGlobalStore((s) =>
    s.settings.agentFreshContext === false ? 0 : CONTEXT_TTL_MS
  );
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const invokeGitRepoSummary = useGlobalStore((s) => s.invokeGitRepoSummary);
  const invokeWorkspaceFingerprint = useGlobalStore((s) => s.invokeWorkspaceFingerprint);
  const invokeGitStatus = useGlobalStore((s) => s.invokeGitStatus);
  const invokeGitDiff = useGlobalStore((s) => s.invokeGitDiff);
  const invokeGitLog = useGlobalStore((s) => s.invokeGitLog);
  const invokeGitLogFiles = useGlobalStore((s) => s.invokeGitLogFiles);
  const invokeGetGitConfig = useGlobalStore((s) => s.invokeGetGitConfig);

  const [ctxPreview, setCtxPreview] = useState("");
  const [ctxTruncated, setCtxTruncated] = useState(false);
  const [ctxLoading, setCtxLoading] = useState(false);
  /** Context cache: avoid re-fetching git data on every send. `fp` is the workspace digest it describes. */
  const cachedContextRef = useRef<(ContextResult & { at: number; fp: string }) | null>(null);
  /** Chips signature to detect changes that invalidate cache. */
  const chipsSigRef = useRef<string>("");

  /** The folder the cache is about: the focused repo, else the current repo. */
  const watchTarget = focusedRepos[0] || currentGitRepo || "";

  /**
   * Digest of the watched workspace, or "" when there is nothing to watch / it cannot be read.
   * A save in the editor or a branch switch changes it; the agent's own edits do too.
   */
  const fingerprintNow = useCallback(async (): Promise<string> => {
    if (!watchTarget) return "";
    try {
      const out = await invokeWorkspaceFingerprint(watchTarget);
      return out.fingerprint;
    } catch {
      return "";
    }
  }, [watchTarget, invokeWorkspaceFingerprint]);

  /**
   * A hit inside the TTL is only good if the files still look the same: two minutes is
   * long enough for the user to edit, stage or switch branches outside the hub.
   * A failed check is not evidence of change — the cache stays.
   */
  const cacheStillFresh = useCallback(
    async (cached: { fp: string }): Promise<boolean> => {
      if (!watchTarget) return true;
      if (!cached.fp) return false;
      const now = await fingerprintNow();
      return !now || now === cached.fp;
    },
    [watchTarget, fingerprintNow]
  );

  /** Project parts (workspace / git / dirty) are only collected in project mode. */
  const refreshContext = useCallback(async (mode: AskMode): Promise<ContextResult> => {
    const parts: string[] = [];
    const included = new Set<ContextPart>();
    const project = mode === "project";

    // Focus set drives which repos get full diffs; empty = auto (current repo).
    const focusList = focusedRepos.length
      ? focusedRepos
      : currentGitRepo
        ? [currentGitRepo]
        : [];
    const focusSet = new Set(focusList);
    // The "primary" for the workspace line + recent commits is the first focus repo.
    const primaryRepo = focusList[0] || null;

    if (project && chips.workspace) {
      const before = parts.length;
      if (focusList.length === 1 && primaryRepo) {
        const ws = findWorkspaceForRepo(primaryRepo, gitWorkspaces);
        if (ws) parts.push(`Workspace: ${ws.path}`);
        parts.push(`Current repo: ${primaryRepo}`);
      } else if (focusList.length > 1) {
        // Group the focus set by owning workspace: "car-trade (a, b) / ai-workbench".
        const byWs = new Map<string, { label: string; services: string[] }>();
        const loose: string[] = [];
        for (const p of focusList) {
          const ws = findWorkspaceForRepo(p, gitWorkspaces);
          if (!ws) {
            loose.push(repoName(p));
            continue;
          }
          const entry = byWs.get(ws.path) || { label: ws.name || repoName(ws.path), services: [] };
          entry.services.push(repoName(p));
          byWs.set(ws.path, entry);
        }
        const segments = [
          ...Array.from(byWs, ([path, e]) => `${e.label} [${path}] (${e.services.join(", ")})`),
          ...loose,
        ];
        parts.push(`Focused: ${segments.join(" / ")}`);
      }
      if (parts.length > before) included.add("workspace");
    }

    // Git identity + recent commits: 5 for a single repo, 2 each for up to 4 repos.
    if (project && chips.git) {
      const before = parts.length;
      try {
        const [name, email] = await invokeGetGitConfig("global");
        if (name || email) parts.push(`Git identity: ${name} <${email}>`);
      } catch {
        /* ignore */
      }
      const logRepos = focusList.slice(0, focusList.length === 1 ? 1 : MULTI_LOG_REPOS);
      const perRepo = focusList.length === 1 ? 5 : 2;
      const logs = await withConcurrency(
        logRepos.map((repo) => async () => {
          try {
            const commits = await invokeGitLog(repo, perRepo);
            if (commits.length === 0) return null;
            const head = focusList.length === 1 ? "Recent commits:" : `Recent commits (${repoName(repo)}):`;
            return head + "\n" + commits.map((c) => `  ${c[0]} ${c[1]} (${c[2]})`).join("\n");
          } catch {
            return null;
          }
        }),
        GIT_CONCURRENCY
      );
      for (const l of logs) if (l) parts.push(l);
      if (parts.length > before) included.add("git");
    }

    const repoNames: string[] = [];

    // Dirty repos: focus set gets full diffs, others get status only.
    if (project && chips.dirty) {
      // Collect candidate paths: focus repos first, then recent projects.
      // Picking several services (e.g. a whole workspace) is already the scope;
      // padding it with unrelated dirty repos would only eat the budget.
      const candidates: string[] = [];
      for (const p of focusList) candidates.push(p);
      if (focusList.length <= 1) {
        for (const p of (recentProjects || []).slice(0, MAX_SECONDARY_REPOS)) {
          if (!focusSet.has(p.path)) candidates.push(p.path);
        }
      }

      const summaries = await withConcurrency(
        candidates.map((path) => async () => {
          try {
            const s: GitRepoSummary = await invokeGitRepoSummary(path);
            return { path, summary: s };
          } catch {
            return null;
          }
        }),
        GIT_CONCURRENCY
      );
      const valid = summaries.filter((r): r is { path: string; summary: GitRepoSummary } => r !== null);

      const focusEntries = valid.filter((r) => focusSet.has(r.path) && r.summary.isGit);
      const secondaryEntries = valid.filter(
        (r) => !focusSet.has(r.path) && r.summary.isGit && r.summary.dirtyCount > 0
      );
      // Dirty services first (most changes first); clean ones collapse into one line.
      const dirtyFocus = focusEntries
        .filter((r) => r.summary.dirtyCount > 0)
        .sort((a, b) => b.summary.dirtyCount - a.summary.dirtyCount);
      const cleanFocus = focusEntries.filter((r) => r.summary.dirtyCount === 0);
      // Share the diff budget across dirty services instead of 8 full files each.
      const diffFilesPerRepo =
        dirtyFocus.length <= 1
          ? MAX_DIFF_FILES_PRIMARY
          : Math.max(2, Math.floor(MULTI_DIFF_FILE_BUDGET / dirtyFocus.length));
      const diffCharsPerFile = dirtyFocus.length <= 1 ? MAX_DIFF_PER_FILE : MULTI_DIFF_PER_FILE;

      // Full-diff block for every dirty focused repo (concurrency-capped across repos).
      const focusLines = await withConcurrency(
        dirtyFocus.map(({ path: repo, summary: s }) => async (): Promise<string | null> => {
          const lines: string[] = [`[FOCUS] ${s.name || repo} (${s.branch}): ${s.dirtyCount} changes`];
          try {
            const entries = await invokeGitStatus(repo);
            const shown = entries.slice(0, DIRTY_FILE_LIMIT);
            for (const e of shown) lines.push(`  ${statusMark(e)} ${e.path}`);
            if (entries.length > DIRTY_FILE_LIMIT) {
              lines.push(`  …(+${entries.length - DIRTY_FILE_LIMIT} more)`);
            }
            const diffFiles = entries.filter((e) => e.group !== "untracked").slice(0, diffFilesPerRepo);
            if (diffFiles.length > 0) {
              const diffs = await withConcurrency(
                diffFiles.map((e) => async () => {
                  try {
                    const diff = await invokeGitDiff(repo, e.path, false);
                    if (!diff || diff === "（无差异）") return null;
                    const clipped = diff.length > diffCharsPerFile
                      ? diff.slice(0, diffCharsPerFile) + "\n  …(truncated)"
                      : diff;
                    return `  --- ${e.path} ---\n  ${clipped.split("\n").join("\n  ")}`;
                  } catch {
                    return null;
                  }
                }),
                GIT_CONCURRENCY
              );
              const validDiffs = diffs.filter((d): d is string => d !== null);
              if (validDiffs.length > 0) {
                lines.push("  Changes:");
                lines.push(...validDiffs);
              }
            }
          } catch {
            /* summary line alone is still useful */
          }
          return lines.join("\n");
        }),
        GIT_CONCURRENCY
      );
      const focusBlocks = focusLines.filter((b): b is string => !!b);
      if (cleanFocus.length > 0) {
        focusBlocks.push(
          `[FOCUS] Clean (no uncommitted changes): ${cleanFocus
            .map(({ path, summary: s }) => `${s.name || repoName(path)} (${s.branch})`)
            .join(", ")}`
        );
      }
      if (focusBlocks.length > 0) {
        parts.push(focusBlocks.join("\n\n"));
        included.add("dirty");
        for (const { path, summary: s } of focusEntries) repoNames.push(s.name || repoName(path));
      }

      // Secondary repos — status marks + a couple of small diffs.
      // 只对真正会写进上下文的这几个仓库跑 git：以前对最多 6 个各跑 status + 2 次 diff，
      // 最后才 slice(0, 4)，三分之一的工作和进程是白花的。
      const secondaryShown = secondaryEntries.slice(0, SECONDARY_REPOS_SHOWN);
      if (secondaryShown.length > 0) {
        const secondaryResults = await withConcurrency(
          secondaryShown.map(({ path: repo, summary: s }) => async () => {
            const lines: string[] = [`${s.name || repo} (${s.branch}): ${s.dirtyCount} changes`];
            try {
              const entries = await invokeGitStatus(repo);
              const shown = entries.slice(0, 10);
              for (const e of shown) lines.push(`  ${statusMark(e)} ${e.path}`);
              if (entries.length > 10) lines.push(`  …(+${entries.length - 10} more)`);
              const diffFiles = entries.filter((e) => e.group !== "untracked").slice(0, MAX_DIFF_FILES_SECONDARY);
              if (diffFiles.length > 0) {
                const diffs = await withConcurrency(
                  diffFiles.map((e) => async () => {
                    try {
                      const diff = await invokeGitDiff(repo, e.path, false);
                      if (!diff || diff === "（无差异）") return null;
                      const clipped = diff.length > 600 ? diff.slice(0, 600) + "\n  …(truncated)" : diff;
                      return `  --- ${e.path} ---\n  ${clipped.split("\n").join("\n  ")}`;
                    } catch {
                      return null;
                    }
                  }),
                  2
                );
                const validDiffs = diffs.filter((d): d is string => d !== null);
                if (validDiffs.length > 0) {
                  lines.push("  Changes:");
                  lines.push(...validDiffs);
                }
              }
            } catch {
              /* summary line alone is useful */
            }
            return lines.join("\n");
          }),
          GIT_CONCURRENCY
        );
        const validResults = secondaryResults.filter(Boolean);
        if (validResults.length > 0) {
          parts.push("Other dirty repos:\n" + validResults.join("\n\n"));
          included.add("dirty");
          for (const { path, summary: s } of secondaryShown) {
            repoNames.push(s.name || repoName(path));
          }
        }
      }
    }

    const joined = parts.join("\n\n");
    if (project && repoNames.length === 0 && included.size > 0) {
      repoNames.push(...focusList.map(repoName));
    }

    const result = joined ? truncate(joined, CONTEXT_LIMIT) : { text: "", truncated: false };
    const out: ContextResult = {
      text: result.text,
      truncated: result.truncated,
      summary: {
        repos: Array.from(new Set(repoNames)),
        parts: (["dirty", "git", "workspace"] as ContextPart[]).filter((p) => included.has(p)),
      },
    };
    setCtxPreview(out.text);
    setCtxTruncated(out.truncated);
    return out;
  }, [
    chips.workspace,
    chips.git,
    chips.dirty,
    gitWorkspaces,
    recentProjects,
    currentGitRepo,
    focusedRepos,
    invokeGetGitConfig,
    invokeGitRepoSummary,
    invokeGitStatus,
    invokeGitDiff,
    invokeGitLog,
  ]);

  /** Get context for a mode: use cache if still fresh, otherwise rebuild. */
  const getContext = useCallback(
    async (mode: AskMode = "project"): Promise<ContextResult> => {
      const sig = `${mode}|${chips.workspace}|${chips.git}|${chips.dirty}|${currentGitRepo}|${focusedRepos.join(",")}`;
      const cached = cachedContextRef.current;
      const now = Date.now();
      if (cached && chipsSigRef.current === sig && now - cached.at < contextTtlMs && (await cacheStillFresh(cached))) {
        setCtxPreview(cached.text);
        setCtxTruncated(cached.truncated);
        return { text: cached.text, truncated: cached.truncated, summary: cached.summary };
      }
      if (mode === "chat") {
        setCtxPreview("");
        setCtxTruncated(false);
        return { text: "", truncated: false, summary: EMPTY_SUMMARY };
      }
      setCtxLoading(true);
      try {
        // 指纹采在收集**之前**：它要描述的就是这份内容对应的树。采在之后的话，
        // 采集期间保存一次，就会把「新指纹 + 旧内容」缓存满 120 秒。
        const fp = await fingerprintNow();
        const out = await refreshContext(mode);
        cachedContextRef.current = { ...out, at: Date.now(), fp };
        // 签名和缓存同一时刻提交：以前先写 sig 后写缓存，收集中途抛错会让新签名
        // 配上旧内容，下一次提问读到的还是上一个仓库的上下文。
        chipsSigRef.current = sig;
        return out;
      } finally {
        setCtxLoading(false);
      }
    },
    [chips, currentGitRepo, focusedRepos, refreshContext, cacheStillFresh, fingerprintNow]
  );

  /** Force the next getContext() to rebuild (e.g. after the window is re-shown). */
  const invalidateContext = useCallback(() => {
    chipsSigRef.current = "";
  }, []);

  /**
   * Today's commits plus a coarse "still dirty" area list. No patches — a daily
   * report should talk about the work, not quote the diff.
   */
  const collectDailyBrief = useCallback(async (): Promise<{ text: string; repos: string[] }> => {
    const focusList = focusedRepos.length
      ? focusedRepos
      : currentGitRepo
        ? [currentGitRepo]
        : [];
    const since = localDayStart();
    const notes = await withConcurrency(
      focusList.slice(0, 6).map((repo) => async (): Promise<DailyRepoNote | null> => {
        try {
          const summary = await invokeGitRepoSummary(repo);
          if (!summary.isGit) return null;
          const commits = await invokeGitLogFiles(repo, 30, since);
          let dirtyPaths: string[] = [];
          if (summary.dirtyCount > 0) {
            try {
              const entries = await invokeGitStatus(repo);
              dirtyPaths = entries.slice(0, DIRTY_FILE_LIMIT).map((e) => e.path);
            } catch {
              dirtyPaths = [];
            }
          }
          return {
            name: summary.name || repoName(repo),
            commits: commits
              .map((c) => ({
                subject: (c.subject || "").trim(),
                when: c.when || "",
                files: c.files || [],
              }))
              .filter((c) => c.subject),
            dirtyPaths,
          };
        } catch {
          return {
            name: repoName(repo),
            commits: [],
            dirtyPaths: [],
            unreadable: true,
          };
        }
      }),
      GIT_CONCURRENCY,
    );
    const repos = notes.filter((n): n is DailyRepoNote => n !== null);
    return { text: formatDailyNotes(repos), repos: repos.map((r) => r.name) };
  }, [focusedRepos, currentGitRepo, invokeGitRepoSummary, invokeGitLogFiles, invokeGitStatus]);

  return { ctxPreview, ctxTruncated, ctxLoading, getContext, invalidateContext, collectDailyBrief };
}
