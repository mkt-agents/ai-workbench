import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  BookmarkPlus, Check, ClipboardPaste, Copy, Download, Eye, FileText, History,
  MessageSquarePlus, RefreshCw, Send, Sparkles, Square, Trash2, X, ExternalLink,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../core/store";
import type {
  AIModelConfig, AppTheme, GitRepoSummary, QuickAskChips, QuickAskSession,
  QuickAskTurn, Snippet,
} from "../core/types";
import { bootApp } from "../core/boot";
import { fillParams, parseParamNames } from "../lib/snippets";
import { projectNameFromPath } from "../core/pathUtils";
import {
  APP_THEME_CHANGED_EVENT,
  applyDocumentTheme,
  readPersistedTheme,
} from "../lib/theme";
import ModalTitleRow from "./ModalTitleRow";
import MarkdownView from "./MarkdownView";
import { DEFAULT_CHIPS, TASK_SYSTEM, makeTitle, truncate, validTask } from "./quickAskConfig";
import type { TaskKind } from "./quickAskConfig";

const CONTEXT_LIMIT = 12000;
const SNIPPET_PICK_LIMIT = 24;
const DIRTY_FILE_LIMIT = 30;
/** Follow-up context: keep the last N Q&A pairs, clipped per turn. */
const MAX_HISTORY_TURNS = 6;
const MAX_TURN_CHARS = 4000;
/** Max diff bytes per file when including dirty content in quick-ask context. */
const MAX_DIFF_PER_FILE = 2000;
/** Max number of files to fetch diff for the PRIMARY repo in quick-ask context. */
const MAX_DIFF_FILES_PRIMARY = 8;
/** Max number of files to fetch diff for SECONDARY repos (status only by default). */
const MAX_DIFF_FILES_SECONDARY = 2;
/** How long (ms) a cached context stays fresh before auto-refresh. */
const CONTEXT_TTL_MS = 120000;
/** Max concurrent git processes to avoid saturating the thread pool. */
const GIT_CONCURRENCY = 4;
/** Max repos to scan for dirty status (beyond the primary repo). */
const MAX_SECONDARY_REPOS = 6;

type QaTurn = QuickAskTurn;

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

function clipTurn(text: string): string {
  return text.length > MAX_TURN_CHARS
    ? text.slice(0, MAX_TURN_CHARS) + "\n…(truncated)"
    : text;
}

function appendTurn(prev: QaTurn[], question: string, answer: string): QaTurn[] {
  const next: QaTurn[] = [
    ...prev,
    { role: "user", content: clipTurn(question) },
    { role: "assistant", content: clipTurn(answer) },
  ];
  return next.slice(-MAX_HISTORY_TURNS * 2);
}

function QuickAskApp() {
  const { t, i18n } = useTranslation("quickask");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const snippets = useGlobalStore((s) => s.snippets);
  const bumpSnippetUse = useGlobalStore((s) => s.bumpSnippetUse);
  const addSnippet = useGlobalStore((s) => s.addSnippet);
  const quickAskSessions = useGlobalStore((s) => s.quickAskSessions);
  const upsertQuickAskSession = useGlobalStore((s) => s.upsertQuickAskSession);
  const deleteQuickAskSession = useGlobalStore((s) => s.deleteQuickAskSession);
  const loadQuickAskSessions = useGlobalStore((s) => s.loadQuickAskSessions);
  const invokeSaveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeGitRepoSummary = useGlobalStore((s) => s.invokeGitRepoSummary);
  const invokeGitStatus = useGlobalStore((s) => s.invokeGitStatus);
  const invokeGitDiff = useGlobalStore((s) => s.invokeGitDiff);
  const invokeGitLog = useGlobalStore((s) => s.invokeGitLog);
  const invokeGetGitConfig = useGlobalStore((s) => s.invokeGetGitConfig);

  const [ready, setReady] = useState(false);
  const [input, setInput] = useState("");
  const [answer, setAnswer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [task, setTask] = useState<TaskKind>("none");
  const [modelId, setModelId] = useState<string>("");
  const chips = settings.quickAskChips ?? DEFAULT_CHIPS;
  const [ctxPreview, setCtxPreview] = useState("");
  const [ctxTruncated, setCtxTruncated] = useState(false);
  const [ctxLoading, setCtxLoading] = useState(false);
  /** Which repo the user wants to focus on for full diff context. */
  const [focusedRepo, setFocusedRepo] = useState<string | null>(null);
  const [ctxOpen, setCtxOpen] = useState(false);
  const [snippetOpen, setSnippetOpen] = useState(false);
  const [snippetQuery, setSnippetQuery] = useState("");
  const [paramTarget, setParamTarget] = useState<Snippet | null>(null);
  const [paramValues, setParamValues] = useState<Record<string, string>>({});
  /** Previous Q&A pairs sent along as multi-turn context (follow-up questions). */
  const [history, setHistory] = useState<QaTurn[]>([]);
  const [copied, setCopied] = useState(false);
  /** Persisted-session id the current chat belongs to (null = fresh chat). */
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [snippetSaved, setSnippetSaved] = useState(false);
  const [exportDone, setExportDone] = useState(false);
  /** Inline rename state: which session + draft title. */
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  /** Text currently selected inside the answer area (for quote follow-up). */
  const [selectedText, setSelectedText] = useState("");
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const answerRef = useRef<HTMLDivElement | null>(null);
  /** Last question + the history snapshot it was sent with, for regenerate. */
  const lastQuestionRef = useRef<string>("");
  const lastHistoryRef = useRef<QaTurn[]>([]);
  const streamIdRef = useRef(0);
  /** Request id of the stream currently in flight, so it can be cancelled. */
  const activeReqRef = useRef<string | null>(null);
  /** Context cache: avoid re-fetching git data on every send. */
  const cachedContextRef = useRef<{ text: string; truncated: boolean; at: number } | null>(null);
  /** Chips signature to detect changes that invalidate cache. */
  const chipsSigRef = useRef<string>("");

  const setChip = (key: keyof QuickAskChips, value: boolean) => {
    setSettings({
      quickAskChips: { ...chips, [key]: value },
    });
  };

  useEffect(() => {
    let cancelled = false;
    bootApp().then(() => {
      if (cancelled) return;
      // Load repos + workspaces so dirty/workspace chips have data.
      void useGlobalStore.getState().loadRecentProjects();
      void useGlobalStore.getState().loadWorkspaces();
      setReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    applyDocumentTheme(settings.theme);
    if (settings.language && i18n.language !== settings.language) {
      void i18n.changeLanguage(settings.language);
    }
  }, [settings.theme, settings.language, i18n]);

  const models = aiModels as AIModelConfig[];
  const selectedModel = useMemo(() => {
    return models.find((m) => m.id === modelId) || models.find((m) => m.isDefault) || models[0];
  }, [models, modelId]);

  useEffect(() => {
    if (!modelId && selectedModel) setModelId(selectedModel.id);
  }, [modelId, selectedModel]);

  const refreshContext = useCallback(async (): Promise<{ text: string; truncated: boolean }> => {
    const parts: string[] = [];
    let truncated = false;

    // Determine the primary repo: explicit focus > currentGitRepo > first dirty repo.
    const primaryRepo = focusedRepo || settings.currentGitRepo || null;

    if (chips.workspace) {
      const ws = gitWorkspaces[0];
      if (ws) parts.push(`Workspace: ${ws.path}`);
      if (primaryRepo) parts.push(`Current repo: ${primaryRepo}`);
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

    // Dirty repos: primary repo gets full diffs, secondary repos get status only.
    if (chips.dirty) {
      // Collect candidate paths: primary first, then recent projects.
      const candidates: string[] = [];
      if (primaryRepo) candidates.push(primaryRepo);
      for (const p of (recentProjects || []).slice(0, MAX_SECONDARY_REPOS)) {
        if (p.path !== primaryRepo) candidates.push(p.path);
      }

      // Phase 1: fetch summaries in parallel (concurrency-limited).
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

      // Separate primary from secondary.
      const primaryEntry = valid.find((r) => r.path === primaryRepo) || valid[0];
      const secondaryEntries = valid.filter((r) => r !== primaryEntry && r.summary.isGit && r.summary.dirtyCount > 0);

      // Phase 2a: primary repo — full status + diffs.
      if (primaryEntry?.summary.isGit && primaryEntry.summary.dirtyCount > 0) {
        const repo = primaryEntry.path;
        const s = primaryEntry.summary;
        const lines: string[] = [`[PRIMARY] ${s.name || repo} (${s.branch}): ${s.dirtyCount} changes`];
        try {
          const entries = await invokeGitStatus(repo);
          const shown = entries.slice(0, DIRTY_FILE_LIMIT);
          for (const e of shown) {
            const mark = e.group === "untracked" ? "??" : `${e.indexStatus || " "}${e.workTreeStatus || " "}`;
            lines.push(`  ${mark} ${e.path}`);
          }
          if (entries.length > DIRTY_FILE_LIMIT) {
            lines.push(`  …(+${entries.length - DIRTY_FILE_LIMIT} more)`);
            truncated = true;
          }
          // Fetch diffs for modified files (parallel, capped).
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
        parts.push(lines.join("\n"));

        // Phase 2b: secondary repos — status marks + limited diffs (concurrency-capped).
        if (secondaryEntries.length > 0) {
          const secondaryResults = await withConcurrency(
            secondaryEntries.map(({ path: repo, summary: s }) => async () => {
              const lines: string[] = [`${s.name || repo} (${s.branch}): ${s.dirtyCount} changes`];
              try {
                const entries = await invokeGitStatus(repo);
                const shown = entries.slice(0, 10);
                for (const e of shown) {
                  const mark = e.group === "untracked" ? "??" : `${e.indexStatus || " "}${e.workTreeStatus || " "}`;
                  lines.push(`  ${mark} ${e.path}`);
                }
                if (entries.length > 10) {
                  lines.push(`  …(+${entries.length - 10} more)`);
                }
                // Only fetch a couple of diffs for secondary repos.
                const diffFiles = entries.filter((e) => e.group !== "untracked").slice(0, MAX_DIFF_FILES_SECONDARY);
                if (diffFiles.length > 0) {
                  const diffs = await withConcurrency(
                    diffFiles.map((e) => async () => {
                      try {
                        const diff = await invokeGitDiff(repo, e.path, false);
                        if (!diff || diff === "（无差异）") return null;
                        const clipped = diff.length > 600
                          ? diff.slice(0, 600) + "\n  …(truncated)"
                          : diff;
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
      } else if (primaryEntry) {
        // Primary repo exists but is clean — just note it.
        parts.push(`[PRIMARY] ${primaryEntry.summary.name || primaryEntry.path} (${primaryEntry.summary.branch}): clean`);
        // Still scan secondary repos for dirty status.
        if (secondaryEntries.length > 0) {
          const secondaryLines = secondaryEntries.map(
            ({ summary: s, path: p }) => `  ${s.name || p} (${s.branch}): ${s.dirtyCount} changes`
          );
          parts.push("Dirty repos:\n" + secondaryLines.join("\n"));
        }
      }
    }

    if (chips.clipboard) {
      try {
        const clip = await invoke<string>("read_clipboard");
        if (clip.trim()) {
          const tclip = truncate(clip.trim(), 800);
          parts.push("Clipboard:\n" + tclip.text);
          if (tclip.truncated) truncated = true;
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
    const out = { text: result.text, truncated: truncated || result.truncated };
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
    settings.currentGitRepo,
    focusedRepo,
    invokeGetGitConfig,
    invokeGitRepoSummary,
    invokeGitStatus,
    invokeGitDiff,
    invokeGitLog,
  ]);

  /** Get context: use cache if fresh, otherwise rebuild. */
  const getContext = useCallback((): Promise<{ text: string; truncated: boolean }> => {
    const sig = `${chips.workspace}|${chips.git}|${chips.dirty}|${chips.clipboard}|${settings.currentGitRepo}|${focusedRepo}`;
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
  }, [chips, settings.currentGitRepo, focusedRepo, refreshContext]);

  // Use ref to always have the latest getContext without re-registering listeners
  const getContextRef = useRef(getContext);
  getContextRef.current = getContext;
  // Deduplicate prefill events (both quick-ask windows receive the same event)
  const prefillIdRef = useRef<string | null>(null);



  useEffect(() => {
    let unlistenShown: (() => void) | undefined;
    let unlistenPrefill: (() => void) | undefined;
    let unlistenTheme: (() => void) | undefined;

    const syncThemeFromStorage = () => {
      const theme = readPersistedTheme();
      if (!theme) return;
      setSettings({ theme });
      applyDocumentTheme(theme);
    };

    void listen("quick-ask-shown", () => {
      syncThemeFromStorage();
      // Reload mutable data from SQLite — the main window may have added/removed
      // models, snippets, or sessions while this window was hidden. bootApp()
      // only runs once on mount (memoised promise), so without this the lists go stale.
      void useGlobalStore.getState().loadAIModels();
      void useGlobalStore.getState().loadSnippets();
      void useGlobalStore.getState().loadQuickAskSessions();
      void useGlobalStore.getState().loadRecentProjects();
      void useGlobalStore.getState().loadWorkspaces();
      // Defer context build by 1.5s so the window paints instantly; the first
      // send will build it immediately anyway if the user is fast.
      chipsSigRef.current = "";
      window.setTimeout(() => {
        void getContextRef.current();
      }, 1500);
    }).then((fn) => {
      unlistenShown = fn;
    });
    void listen<{ id?: string; text?: string }>("quick-ask-prefill", (event) => {
      const payload = event.payload;
      let text = "";
      let id: string | null = null;
      if (typeof payload === "string") {
        text = payload;
      } else if (payload && typeof payload === "object") {
        text = (payload.text as string) || "";
        id = (payload.id as string) || null;
      }
      if (!text) return;
      // Deduplicate: if this event was already processed, skip
      if (id && prefillIdRef.current === id) return;
      if (id) {
        prefillIdRef.current = id;
      }
      setInput((prev) => (prev ? `${prev}\n${text}` : text));
      setSnippetOpen(false);
      setParamTarget(null);
      requestAnimationFrame(() => inputRef.current?.focus());
    }).then((fn) => {
      unlistenPrefill = fn;
    });
    void listen<AppTheme>(APP_THEME_CHANGED_EVENT, (event) => {
      const theme = event.payload;
      if (!theme) return;
      setSettings({ theme });
      applyDocumentTheme(theme);
    }).then((fn) => {
      unlistenTheme = fn;
    });
    return () => {
      unlistenShown?.();
      unlistenPrefill?.();
      unlistenTheme?.();
    };
  }, [setSettings]);

  const hide = async () => {
    // Go through Rust: tao resolves hide() via its cached visibility flag, which can
    // desync from the real window (see tray/bubble.rs `vis`) and turn this into a
    // no-op. Fall back to the JS API if the command is unavailable.
    try {
      await invoke("hide_quick_ask");
    } catch {
      try {
        await getCurrentWindow().hide();
      } catch {
        /* ignore */
      }
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      // Layered escape: modal > snippet panel > session panel > context panel >
      // clear input > hide window.
      if (paramTarget) {
        setParamTarget(null);
        return;
      }
      if (snippetOpen) {
        setSnippetOpen(false);
        return;
      }
      if (historyOpen) {
        setHistoryOpen(false);
        return;
      }
      if (ctxOpen) {
        setCtxOpen(false);
        return;
      }
      if (input.trim()) {
        setInput("");
        return;
      }
      void hide();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      // Ctrl/Cmd+N: new chat
      if ((e.ctrlKey || e.metaKey) && e.key === "n") {
        e.preventDefault();
        if (!busy) startNewChat();
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [paramTarget, snippetOpen, historyOpen, ctxOpen, input, busy]);

  // Track text selected inside the answer area for the quote-follow-up bar.
  useEffect(() => {
    const onSelect = () => {
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed || !answerRef.current) {
        setSelectedText("");
        return;
      }
      const node = sel.getRangeAt(0).commonAncestorContainer;
      if (!answerRef.current.contains(node)) {
        setSelectedText("");
        return;
      }
      setSelectedText(sel.toString());
    };
    document.addEventListener("selectionchange", onSelect);
    return () => document.removeEventListener("selectionchange", onSelect);
  }, []);

  const fillClipboard = async () => {
    try {
      const clip = await invoke<string>("read_clipboard");
      if (clip.trim()) setInput((prev) => (prev ? prev : clip.trim()));
    } catch (e) {
      setError(String(e));
    }
  };

  const applySnippetText = async (text: string, snippetId: string) => {
    setInput((prev) => (prev ? `${prev}\n${text}` : text));
    setSnippetOpen(false);
    setParamTarget(null);
    setSnippetQuery("");
    await bumpSnippetUse(snippetId);
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  const pickSnippet = (s: Snippet) => {
    const names = parseParamNames(s.params, s.content);
    if (names.length === 0) {
      void applySnippetText(s.content, s.id);
      return;
    }
    const init: Record<string, string> = {};
    names.forEach((n) => {
      init[n] = "";
    });
    setParamValues(init);
    setParamTarget(s);
  };

  const confirmSnippetParams = async () => {
    if (!paramTarget) return;
    const text = fillParams(paramTarget.content, paramValues);
    await applySnippetText(text, paramTarget.id);
  };

  const snippetChoices = useMemo(() => {
    const q = snippetQuery.trim().toLowerCase();
    const list = [...(snippets || [])].sort((a, b) => b.useCount - a.useCount || b.updatedAt.localeCompare(a.updatedAt));
    const filtered = q
      ? list.filter(
          (s) =>
            s.name.toLowerCase().includes(q) ||
            s.tags.toLowerCase().includes(q) ||
            s.content.toLowerCase().includes(q)
        )
      : list;
    return filtered.slice(0, SNIPPET_PICK_LIMIT);
  }, [snippets, snippetQuery]);

  /** Persist (or create) the active session after a completed exchange. */
  const persistTurns = async (turns: QaTurn[], firstQuestion: string) => {
    const existing = activeSessionId
      ? useGlobalStore.getState().quickAskSessions.find((s) => s.id === activeSessionId)
      : undefined;
    const now = new Date().toISOString();
    const session: QuickAskSession = existing
      ? { ...existing, turns, task, updatedAt: now }
      : {
          id: `qas-${crypto.randomUUID()}`,
          title: makeTitle(firstQuestion, t("newChat")),
          task,
          turns,
          createdAt: now,
          updatedAt: now,
        };
    if (!existing) setActiveSessionId(session.id);
    try {
      await upsertQuickAskSession(session);
    } catch (e) {
      setError(String(e));
    }
  };

  const send = async (rawInput?: string, historyOverride?: QaTurn[]) => {
    if (busy) return;
    if (!selectedModel) {
      setError(t("noModel"));
      return;
    }
    const question = (rawInput ?? input).trim();
    if (!question) {
      setError(t("emptyInput"));
      return;
    }
    // The context dump is per-request; history carries only the plain Q&A pairs.
    const baseHistory = historyOverride ?? history;
    const reqId = `qa-${Date.now()}-${++streamIdRef.current}`;
    const myGen = streamIdRef.current;
    activeReqRef.current = reqId;
    setBusy(true);
    setError(null);
    setAnswer("");
    setSelectedText("");

    let settled = false;
    let full = "";
    const teardown = () => {
      unlisteners.forEach((fn) => fn());
      unlisteners.length = 0;
    };
    const unlisteners: Array<() => void> = [];

    try {
      const ctxResult = await getContext();
      const system = TASK_SYSTEM[task];
      const ctx = ctxResult.text;
      const user = ctx
        ? `${question}\n\n---\nContext from workbench:\n${ctx}`
        : question;

      const onChunk = async (event: { payload?: { id: string; text: string } }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        full += payload.text || "";
        setAnswer((prev) => prev + (payload.text || ""));
      };
      const onDone = async (event: { payload?: { id: string } }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        if (!settled) {
          settled = true;
          setBusy(false);
          // Completed exchange becomes follow-up context (plain text, no ctx dump).
          if (full.trim()) {
            const nextTurns = appendTurn(baseHistory, question, full);
            setHistory(nextTurns);
            lastQuestionRef.current = question;
            lastHistoryRef.current = baseHistory;
            void persistTurns(nextTurns, question);
          }
        }
        activeReqRef.current = null;
        teardown();
      };
      const onError = async (event: { payload?: { id: string; error: string } }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        if (!settled) {
          settled = true;
          // A user-triggered stop is not a failure — keep the partial answer.
          const stopped = (payload.error || "").includes("AI_REQUEST_CANCELLED");
          if (!stopped) setError(payload.error || t("streamFailed"));
          else if (full.trim()) {
            // Stopped mid-stream: keep the partial answer as context too.
            const nextTurns = appendTurn(baseHistory, question, full);
            setHistory(nextTurns);
            lastQuestionRef.current = question;
            lastHistoryRef.current = baseHistory;
            void persistTurns(nextTurns, question);
          }
          setBusy(false);
        }
        activeReqRef.current = null;
        teardown();
      };

      unlisteners.push(await listen<{ id: string; text: string }>("generate-text-chunk", onChunk));
      unlisteners.push(await listen<{ id: string }>("generate-text-done", onDone));
      unlisteners.push(await listen<{ id: string; error: string }>("generate-text-error", onError));

      const maxTokens = Math.max(selectedModel.maxTokens || 0, 1536);
      await invoke("generate_text_stream", {
        req: {
          config: { ...selectedModel, maxTokens },
          system,
          user,
          requestId: reqId,
          history: baseHistory,
        },
      });
      if (streamIdRef.current === myGen) {
        setInput("");
        lastQuestionRef.current = question;
        lastHistoryRef.current = baseHistory;
      }
    } catch (e) {
      if (streamIdRef.current === myGen && !settled) {
        settled = true;
        // Stopping rejects the command too — no error for a deliberate stop.
        if (!String(e).includes("AI_REQUEST_CANCELLED")) {
          setError(String(e));
        }
        setBusy(false);
      }
      activeReqRef.current = null;
      teardown();
    }
  };

  const startNewChat = () => {
    if (busy) return;
    setHistory([]);
    setAnswer("");
    setError(null);
    setActiveSessionId(null);
    setSelectedText("");
    lastQuestionRef.current = "";
    lastHistoryRef.current = [];
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  /** Restore a persisted session into the current chat. */
  const openSession = (s: QuickAskSession) => {
    if (busy) return;
    const turns = s.turns || [];
    setHistory(turns);
    setTask(validTask(s.task));
    const lastAnswer = [...turns].reverse().find((t) => t.role === "assistant");
    const lastUser = [...turns].reverse().find((t) => t.role === "user");
    setAnswer(lastAnswer?.content ?? "");
    setActiveSessionId(s.id);
    setError(null);
    setSelectedText("");
    // Point regenerate at the final exchange of this session.
    lastQuestionRef.current = lastUser?.content ?? "";
    lastHistoryRef.current = turns.length >= 2 ? turns.slice(0, -2) : [];
    setHistoryOpen(false);
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  const removeSession = async (id: string) => {
    try {
      await deleteQuickAskSession(id);
      if (activeSessionId === id) {
        setActiveSessionId(null);
        setHistory([]);
        setAnswer("");
        lastQuestionRef.current = "";
        lastHistoryRef.current = [];
      }
    } catch (e) {
      setError(String(e));
    }
  };

  /** Persist a new title for a session. */
  const renameSession = async (id: string, title: string) => {
    const trimmed = title.trim();
    if (!trimmed) {
      setRenamingId(null);
      return;
    }
    const target = quickAskSessions.find((s) => s.id === id);
    if (!target) return;
    try {
      const now = new Date().toISOString();
      await upsertQuickAskSession({ ...target, title: trimmed, updatedAt: now });
      await loadQuickAskSessions();
      setRenamingId(null);
    } catch (e) {
      setError(String(e));
    }
  };

  /** Export the active session as a Markdown file. */
  const exportActiveSession = async () => {
    if (history.length === 0) return;
    const title = activeSessionId
      ? quickAskSessions.find((s) => s.id === activeSessionId)?.title || "quick-ask"
      : "quick-ask";
    const lines = [`# ${title}`, ""];
    history.forEach((turn) => {
      lines.push(turn.role === "user" ? `## 🧑 You` : `## 🤖 Assistant`);
      lines.push("");
      lines.push(turn.content);
      lines.push("");
    });
    const filename = `${title.replace(/[\\/:*?"<>|]/g, "_") || "quick-ask"}.md`;
    try {
      await invokeSaveTextFile(lines.join("\n"), filename, t("exportSession"));
      setExportDone(true);
      setTimeout(() => setExportDone(false), 1500);
    } catch (e) {
      setError(String(e));
    }
  };

  /** Relative "n minutes ago" label for the session list. */
  const relativeTime = (iso: string): string => {
    const then = new Date(iso).getTime();
    if (!Number.isFinite(then)) return "";
    const minutes = Math.floor((Date.now() - then) / 60000);
    if (minutes < 1) return t("justNow");
    if (minutes < 60) return t("minutesAgo", { n: minutes });
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return t("hoursAgo", { n: hours });
    const days = Math.floor(hours / 24);
    if (days < 7) return t("daysAgo", { n: days });
    return new Date(then).toLocaleDateString();
  };

  const saveAnswerAsSnippet = async () => {
    if (!answer) return;
    const name = makeTitle(lastQuestionRef.current || answer, t("saveSnippet"));
    try {
      await addSnippet({ name, content: answer, tags: "quickask,ai", params: "" });
      setSnippetSaved(true);
      window.setTimeout(() => setSnippetSaved(false), 1500);
    } catch (e) {
      setError(String(e));
    }
  };

  /** Quote the selected answer text into the input as a follow-up prompt. */
  const quoteSelection = () => {
    const text = selectedText.trim().slice(0, 500);
    if (!text) return;
    const quoted = t("quotePrefix", { text });
    setInput((prev) => (prev ? `${prev}\n\n${quoted}` : quoted));
    setSelectedText("");
    window.getSelection()?.removeAllRanges();
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  const copySelection = async () => {
    if (!selectedText) return;
    try {
      await invokeCopyToClipboard(selectedText);
      setSelectedText("");
      window.getSelection()?.removeAllRanges();
    } catch (e) {
      setError(String(e));
    }
  };

  /// Abort the in-flight stream. The backend answers with an
  /// AI_REQUEST_CANCELLED error event (handled as a normal stop) and the partial
  /// answer stays on screen.
  const stopStream = async () => {
    const id = activeReqRef.current;
    if (!id) return;
    activeReqRef.current = null;
    try {
      await invoke("cancel_request", { id });
    } catch (e) {
      setError(String(e));
    }
  };

  // Cleanup any live stream listeners if the window closes mid-stream.
  useEffect(() => {
    return () => {
      // Bump the generation counter so any in-flight callbacks become no-ops.
      streamIdRef.current++;
    };
  }, []);

  const copyAnswer = async () => {
    if (!answer) return;
    try {
      await invokeCopyToClipboard(answer);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (e) {
      setError(String(e));
    }
  };

  const openDeepSeek = async () => {
    try {
      await invoke("open_main_deepseek");
      await hide();
    } catch (e) {
      setError(String(e));
    }
  };

  // Deduplicated list of repos for the focus selector.
  const availableRepos = useMemo(() => {
    const set = new Set<string>();
    if (settings.currentGitRepo) set.add(settings.currentGitRepo);
    for (const p of recentProjects) set.add(p.path);
    return Array.from(set);
  }, [settings.currentGitRepo, recentProjects]);

  if (!ready) {
    return <div className="quick-ask-root loading">{t("loading")}</div>;
  }

  return (
    <div className="quick-ask-root">
      <header className="quick-ask-header" data-tauri-drag-region>
        <div className="quick-ask-title" data-tauri-drag-region>
          <Sparkles size={16} />
          <span>{t("title")}</span>
        </div>
        <div className="quick-ask-header-actions">
          <button
            type="button"
            className={`btn-icon qa-history-btn${historyOpen ? " on" : ""}`}
            onClick={() => setHistoryOpen((v) => !v)}
            title={t("history")}
          >
            <History size={15} />
          </button>
          <button type="button" className="btn-icon" onClick={() => void hide()} title={t("close")}>
            <X size={16} />
          </button>
        </div>
      </header>

      <div className="quick-ask-toolbar">
        <select
          className="input-field"
          value={selectedModel?.id || ""}
          onChange={(e) => setModelId(e.target.value)}
        >
          {models.length === 0 && <option value="">{t("noModel")}</option>}
          {models.map((m) => (
            <option key={m.id} value={m.id}>
              {m.name} ({m.model})
            </option>
          ))}
        </select>
        <select
          className="input-field"
          value={task}
          onChange={(e) => setTask(e.target.value as TaskKind)}
        >
          <option value="none">{t("task.none")}</option>
          <option value="debug">{t("task.debug")}</option>
          <option value="explain">{t("task.explain")}</option>
          <option value="polish">{t("task.polish")}</option>
        </select>
        <select
          className="input-field"
          value={focusedRepo || settings.currentGitRepo || ""}
          onChange={(e) => setFocusedRepo(e.target.value || null)}
          title={t("focusRepo")}
        >
          <option value="">{t("focusRepoAuto")}</option>
          {availableRepos.map((p) => (
            <option key={p} value={p}>
              {projectNameFromPath(p)}
            </option>
          ))}
        </select>
      </div>

      <div className="quick-ask-chips">
        <div className="qa-chip-group">
          <label className={`qa-chip ${chips.workspace ? "on" : ""}`}>
            <input
              type="checkbox"
              checked={chips.workspace}
              onChange={(e) => setChip("workspace", e.target.checked)}
            />
            {t("chip.workspace")}
          </label>
          <label className={`qa-chip ${chips.git ? "on" : ""}`}>
            <input type="checkbox" checked={chips.git} onChange={(e) => setChip("git", e.target.checked)} />
            {t("chip.git")}
          </label>
          <label className={`qa-chip ${chips.dirty ? "on" : ""}`}>
            <input type="checkbox" checked={chips.dirty} onChange={(e) => setChip("dirty", e.target.checked)} />
            {t("chip.dirty")}
          </label>
          <label className={`qa-chip ${chips.clipboard ? "on" : ""}`}>
            <input
              type="checkbox"
              checked={chips.clipboard}
              onChange={(e) => setChip("clipboard", e.target.checked)}
            />
            {t("chip.clipboard")}
          </label>
        </div>
        <div className="qa-tool-group">
          <button
            type="button"
            className="qa-tool-btn"
            onClick={() => void fillClipboard()}
            title={t("pasteClipboard")}
          >
            <ClipboardPaste size={14} />
          </button>
          {(snippets?.length ?? 0) > 0 && (
            <button
              type="button"
              className={`qa-tool-btn${snippetOpen ? " on" : ""}`}
              onClick={() => setSnippetOpen((v) => !v)}
              title={t("snippets")}
            >
              <FileText size={14} />
            </button>
          )}
          <button
            type="button"
            className={`qa-tool-btn${ctxOpen ? " on" : ""}${ctxLoading ? " is-loading" : ""}`}
            onClick={() => setCtxOpen((v) => !v)}
            disabled={!ctxPreview && !ctxLoading}
            title={ctxOpen ? t("hideContext") : t("showContext")}
          >
            {ctxLoading ? <span className="qa-spinner" /> : <Eye size={14} />}
          </button>
        </div>
      </div>

      {ctxTruncated && <div className="quick-ask-hint">{t("truncated")}</div>}
      {ctxOpen && ctxPreview && <pre className="quick-ask-ctx-preview">{ctxPreview}</pre>}

      {snippetOpen && (
        <div className="quick-ask-snippets">
          <input
            className="input-field quick-ask-snippet-search"
            placeholder={t("snippetSearch")}
            value={snippetQuery}
            onChange={(e) => setSnippetQuery(e.target.value)}
            autoFocus
          />
          <div className="quick-ask-snippet-list">
            {snippetChoices.map((s) => (
              <button key={s.id} type="button" className="qa-snippet-item" onClick={() => pickSnippet(s)}>
                {s.name}
              </button>
            ))}
            {snippetChoices.length === 0 && <span className="quick-ask-hint">{t("snippetEmpty")}</span>}
          </div>
        </div>
      )}

      {historyOpen && (
        <div className="quick-ask-sessions">
          <div className="qa-sessions-head">
            <span>{t("history")}</span>
            <button type="button" className="btn btn-secondary btn-small" onClick={startNewChat} disabled={busy}>
              <MessageSquarePlus size={13} /> {t("newChat")}
            </button>
          </div>
          <div className="qa-session-list">
            {quickAskSessions.map((s) => (
              <div
                key={s.id}
                className={`qa-session-item${s.id === activeSessionId ? " active" : ""}`}
              >
                {renamingId === s.id ? (
                  <div className="qa-session-rename">
                    <input
                      className="qa-rename-input"
                      value={renameDraft}
                      autoFocus
                      onChange={(e) => setRenameDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void renameSession(s.id, renameDraft);
                        if (e.key === "Escape") setRenamingId(null);
                      }}
                    />
                    <button
                      type="button"
                      className="btn-icon"
                      onClick={() => void renameSession(s.id, renameDraft)}
                      title={t("renameSave")}
                    >
                      <Check size={13} />
                    </button>
                    <button
                      type="button"
                      className="btn-icon"
                      onClick={() => setRenamingId(null)}
                      title={t("renameCancel")}
                    >
                      <X size={13} />
                    </button>
                  </div>
                ) : (
                  <button type="button" className="qa-session-main" onClick={() => openSession(s)}>
                    <span className="qa-session-title">{s.title || t("newChat")}</span>
                    <span className="qa-session-meta">
                      {t("turnsCount", { n: Math.ceil((s.turns?.length ?? 0) / 2) })} · {relativeTime(s.updatedAt)}
                    </span>
                  </button>
                )}
                {renamingId !== s.id && (
                  <div className="qa-session-actions">
                    <button
                      type="button"
                      className="btn-icon"
                      onClick={() => {
                        setRenamingId(s.id);
                        setRenameDraft(s.title || "");
                      }}
                      title={t("renameSession")}
                    >
                      <FileText size={13} />
                    </button>
                    <button
                      type="button"
                      className="btn-icon"
                      onClick={() => void removeSession(s.id)}
                      title={t("deleteSession")}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                )}
              </div>
            ))}
            {quickAskSessions.length === 0 && (
              <span className="quick-ask-hint">{t("historyEmpty")}</span>
            )}
          </div>
        </div>
      )}

      <div className="qa-input-card">
        <textarea
          ref={inputRef}
          className="quick-ask-input"
          placeholder={t("placeholder")}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== "Enter") return;
            // Ctrl/Cmd+Enter always sends; plain Enter sends too, Shift+Enter = newline.
            if (e.shiftKey && !(e.ctrlKey || e.metaKey)) return;
            e.preventDefault();
            if (!busy) void send();
          }}
          rows={4}
        />
        <div className="qa-input-foot">
          {busy ? (
            <button
              type="button"
              className="qa-stop-btn"
              onClick={() => void stopStream()}
              title={t("stop")}
            >
              <Square size={13} />
              <span>{t("stop")}</span>
            </button>
          ) : (
            <button
              type="button"
              className="qa-send-btn"
              onClick={() => void send()}
              disabled={!input.trim()}
              title={t("send")}
            >
              <Send size={15} />
            </button>
          )}
        </div>
      </div>

      <div className="quick-ask-actions">
        <button
          type="button"
          className="qa-mini-btn"
          disabled={busy || !lastQuestionRef.current}
          onClick={() => void send(lastQuestionRef.current, lastHistoryRef.current)}
          title={t("regenerate")}
        >
          <RefreshCw size={13} /> {t("regenerate")}
        </button>
        <button type="button" className="qa-mini-btn" disabled={!answer} onClick={() => void copyAnswer()}>
          {copied ? <Check size={13} /> : <Copy size={13} />} {copied ? t("copied") : t("copy")}
        </button>
        <button
          type="button"
          className="qa-mini-btn"
          disabled={!answer}
          onClick={() => void saveAnswerAsSnippet()}
          title={t("saveSnippet")}
        >
          {snippetSaved ? <Check size={13} /> : <BookmarkPlus size={13} />}{" "}
          {snippetSaved ? t("savedSnippet") : t("saveSnippet")}
        </button>
        <button type="button" className="qa-mini-btn" onClick={() => void openDeepSeek()}>
          <ExternalLink size={13} /> {t("openDeepseek")}
        </button>
        {history.length > 0 && (
          <button
            type="button"
            className="qa-mini-btn"
            onClick={() => void exportActiveSession()}
            title={t("exportSession")}
          >
            {exportDone ? <Check size={13} /> : <Download size={13} />}{" "}
            {exportDone ? t("exported") : t("exportSession")}
          </button>
        )}
        {(history.length > 0 || answer) && (
          <button
            type="button"
            className="qa-mini-btn"
            disabled={busy}
            onClick={startNewChat}
            title={t("newChat")}
          >
            <MessageSquarePlus size={13} /> {t("newChat")}
          </button>
        )}
      </div>

      {error && <div className="quick-ask-error">{error}</div>}

      {(answer || busy) && (
        <div ref={answerRef} className={`quick-ask-answer${busy ? " is-streaming" : ""}`}>
          <MarkdownView text={answer} />
          {busy && <span className="qa-caret">▍</span>}
          {selectedText && !busy && (
            <div className="qa-selection-bar">
              <span className="qa-selection-text">{selectedText.trim().slice(0, 60)}</span>
              <button type="button" className="btn btn-secondary btn-small" onClick={quoteSelection}>
                <MessageSquarePlus size={13} /> {t("followUp")}
              </button>
              <button type="button" className="btn btn-secondary btn-small" onClick={() => void copySelection()}>
                <Copy size={13} /> {t("copySelection")}
              </button>
            </div>
          )}
        </div>
      )}

      {paramTarget && (
        <div className="modal-overlay" onClick={() => setParamTarget(null)}>
          <div className="modal snippets-modal" onClick={(e) => e.stopPropagation()}>
            <ModalTitleRow title={t("fillParams")} onClose={() => setParamTarget(null)} />
            {Object.keys(paramValues).map((key) => (
              <div className="input-group" key={key}>
                <label className="input-label">{key}</label>
                <input
                  className="input-field"
                  value={paramValues[key]}
                  onChange={(e) => setParamValues({ ...paramValues, [key]: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      void confirmSnippetParams();
                    }
                  }}
                  autoFocus={Object.keys(paramValues)[0] === key}
                />
              </div>
            ))}
            <div className="modal-actions">
              <button type="button" className="btn btn-secondary" onClick={() => setParamTarget(null)}>
                {t("cancel")}
              </button>
              <button type="button" className="btn btn-primary" onClick={() => void confirmSnippetParams()}>
                {t("insertSnippet")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default QuickAskApp;
