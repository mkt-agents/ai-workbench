import { useCallback, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../../core/store";
import type { GitRepoSummary, GitStatusEntry, QuickAskChips } from "../../core/types";
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

/** Max concurrent git processes to avoid saturating the thread pool. */
const GIT_CONCURRENCY = 4;

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

/**
 * Workbench context collection (workspace / git identity / dirty diffs / clipboard)
 * with a TTL cache so a burst of sends doesn't re-spawn git for every question.
 *
 * `focusedRepos` are the repos given FULL diff context (multi-select in the
 * toolbar, optionally populated by scanning a folder). When empty it falls back
 * to the current repo. Every other dirty recent project is summarised with only
 * status + a couple of small diffs so the 12k budget isn't blown.
 */
export function useAskContext(chips: QuickAskChips, focusedRepos: string[]) {
  const currentGitRepo = useGlobalStore((s) => s.settings.currentGitRepo);
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const invokeGitRepoSummary = useGlobalStore((s) => s.invokeGitRepoSummary);
  const invokeGitStatus = useGlobalStore((s) => s.invokeGitStatus);
  const invokeGitDiff = useGlobalStore((s) => s.invokeGitDiff);
  const invokeGitLog = useGlobalStore((s) => s.invokeGitLog);
  const invokeGetGitConfig = useGlobalStore((s) => s.invokeGetGitConfig);

  const [ctxPreview, setCtxPreview] = useState("");
  const [ctxTruncated, setCtxTruncated] = useState(false);
  const [ctxLoading, setCtxLoading] = useState(false);
  /** Context cache: avoid re-fetching git data on every send. */
  const cachedContextRef = useRef<{ text: string; truncated: boolean; at: number } | null>(null);
  /** Chips signature to detect changes that invalidate cache. */
  const chipsSigRef = useRef<string>("");

  const refreshContext = useCallback(async (): Promise<{ text: string; truncated: boolean }> => {
    const parts: string[] = [];
    let truncatedFlag = false;

    // Focus set drives which repos get full diffs; empty = auto (current repo).
    const focusList = focusedRepos.length
      ? focusedRepos
      : currentGitRepo
        ? [currentGitRepo]
        : [];
    const focusSet = new Set(focusList);
    // The "primary" for the workspace line + recent commits is the first focus repo.
    const primaryRepo = focusList[0] || null;

    if (chips.workspace) {
      const ws = gitWorkspaces[0];
      if (ws) parts.push(`Workspace: ${ws.path}`);
      if (focusList.length === 1 && primaryRepo) parts.push(`Current repo: ${primaryRepo}`);
      else if (focusList.length > 1)
        parts.push(`Focused repos: ${focusList.map((p) => p.split(/[\\/]/).pop() || p).join(", ")}`);
    }

    // Git identity: fetch config + recent commits for the primary repo only.
    if (chips.git) {
      try {
        const [name, email] = await invokeGetGitConfig("global");
        if (name || email) parts.push(`Git identity: ${name} <${email}>`);
        if (primaryRepo) {
          try {
            const commits = await invokeGitLog(primaryRepo, 5);
            if (commits.length > 0) {
              parts.push(
                "Recent commits:\n" +
                  commits.map((c) => `  ${c[0]} ${c[1]} (${c[2]})`).join("\n")
              );
            }
          } catch {
            /* ignore */
          }
        }
      } catch {
        /* ignore */
      }
    }

    // Dirty repos: focus set gets full diffs, others get status only.
    if (chips.dirty) {
      // Collect candidate paths: focus repos first, then recent projects.
      const candidates: string[] = [];
      for (const p of focusList) candidates.push(p);
      for (const p of (recentProjects || []).slice(0, MAX_SECONDARY_REPOS)) {
        if (!focusSet.has(p.path)) candidates.push(p.path);
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

      const focusEntries = valid.filter((r) => focusSet.has(r.path));
      const secondaryEntries = valid.filter(
        (r) => !focusSet.has(r.path) && r.summary.isGit && r.summary.dirtyCount > 0
      );

      // Full-diff block for every focused repo (concurrency-capped across repos).
      const focusLines = await withConcurrency(
        focusEntries.map(({ path: repo, summary: s }) => async (): Promise<string | null> => {
          if (!s.isGit) return null;
          if (s.dirtyCount === 0) {
            return `[FOCUS] ${s.name || repo} (${s.branch}): clean`;
          }
          const lines: string[] = [`[FOCUS] ${s.name || repo} (${s.branch}): ${s.dirtyCount} changes`];
          try {
            const entries = await invokeGitStatus(repo);
            const shown = entries.slice(0, DIRTY_FILE_LIMIT);
            for (const e of shown) lines.push(`  ${statusMark(e)} ${e.path}`);
            if (entries.length > DIRTY_FILE_LIMIT) {
              lines.push(`  …(+${entries.length - DIRTY_FILE_LIMIT} more)`);
            }
            const diffFiles = entries.filter((e) => e.group !== "untracked").slice(0, MAX_DIFF_FILES_PRIMARY);
            if (diffFiles.length > 0) {
              const diffs = await withConcurrency(
                diffFiles.map((e) => async () => {
                  try {
                    const diff = await invokeGitDiff(repo, e.path, false);
                    if (!diff || diff === "（无差异）") return null;
                    const clipped = diff.length > MAX_DIFF_PER_FILE
                      ? diff.slice(0, MAX_DIFF_PER_FILE) + "\n  …(truncated)"
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
      if (focusBlocks.length > 0) parts.push(focusBlocks.join("\n\n"));

      // Secondary repos — status marks + a couple of small diffs.
      if (secondaryEntries.length > 0) {
        const secondaryResults = await withConcurrency(
          secondaryEntries.map(({ path: repo, summary: s }) => async () => {
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
          parts.push("Other dirty repos:\n" + validResults.slice(0, 4).join("\n\n"));
        }
      }
    }

    if (chips.clipboard) {
      try {
        const clip = await invoke<string>("read_clipboard");
        if (clip.trim()) {
          const tclip = truncate(clip.trim(), 800);
          parts.push("Clipboard:\n" + tclip.text);
          if (tclip.truncated) truncatedFlag = true;
        }
      } catch {
        /* ignore */
      }
    }

    const joined = parts.join("\n\n");
    const enabledChips: string[] = [];
    if (chips.workspace) enabledChips.push("workspace");
    if (chips.git) enabledChips.push("git");
    if (chips.dirty) enabledChips.push("dirty");
    if (chips.clipboard) enabledChips.push("clipboard");
    const header = `Active context: ${enabledChips.join(", ") || "none"}`;
    const result = truncate(header + "\n\n" + joined, CONTEXT_LIMIT);
    const out = { text: result.text, truncated: truncatedFlag || result.truncated };
    setCtxPreview(out.text);
    setCtxTruncated(out.truncated);
    return out;
  }, [
    chips.workspace,
    chips.git,
    chips.dirty,
    chips.clipboard,
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

  /** Get context: use cache if fresh, otherwise rebuild. */
  const getContext = useCallback((): Promise<{ text: string; truncated: boolean }> => {
    const sig = `${chips.workspace}|${chips.git}|${chips.dirty}|${chips.clipboard}|${currentGitRepo}|${focusedRepos.join(",")}`;
    const cached = cachedContextRef.current;
    const now = Date.now();
    if (cached && chipsSigRef.current === sig && now - cached.at < CONTEXT_TTL_MS) {
      return Promise.resolve({ text: cached.text, truncated: cached.truncated });
    }
    // Rebuild and cache
    chipsSigRef.current = sig;
    setCtxLoading(true);
    return refreshContext()
      .then((out) => {
        cachedContextRef.current = { text: out.text, truncated: out.truncated, at: Date.now() };
        return out;
      })
      .finally(() => setCtxLoading(false));
  }, [chips, currentGitRepo, focusedRepos, refreshContext]);

  /** Force the next getContext() to rebuild (e.g. after the window is re-shown). */
  const invalidateContext = useCallback(() => {
    chipsSigRef.current = "";
  }, []);

  return { ctxPreview, ctxTruncated, ctxLoading, getContext, invalidateContext };
}
