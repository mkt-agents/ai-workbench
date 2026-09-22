import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { History, Sparkles, Terminal, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useGlobalStore } from "../core/store";
import type { AIModelConfig, GitScannedRepo, QuickAskSession, QuickAskTurn } from "../core/types";
import { bootApp } from "../core/boot";
import { projectNameFromPath } from "../core/pathUtils";
import { applyDocumentTheme } from "../lib/theme";
import AskPanel from "./quickAsk/AskPanel";
import ContextBar from "./quickAsk/ContextBar";
import FocusPicker from "./quickAsk/FocusPicker";
import type { RepoOption } from "./quickAsk/FocusPicker";
import OptimizeBar from "./quickAsk/OptimizeBar";
import ParamModal from "./quickAsk/ParamModal";
import QuickSelect from "./quickAsk/QuickSelect";
import SessionHistory from "./quickAsk/SessionHistory";
import SnippetPicker from "./quickAsk/SnippetPicker";
import ToolsPanel from "./quickAsk/ToolsPanel";
import {
  DEFAULT_CHIPS,
  DEFAULT_OPTIMIZE,
  TASK_KINDS,
  encodeTask,
  makeTitle,
  parseTask,
} from "./quickAsk/config";
import type { OptimizeConfig, TaskKind } from "./quickAsk/config";
import { useAskContext } from "./quickAsk/useAskContext";
import { useAskEvents } from "./quickAsk/useAskEvents";
import { useAskSessions } from "./quickAsk/useAskSessions";
import { useAskSnippets } from "./quickAsk/useAskSnippets";
import { useAskStream } from "./quickAsk/useAskStream";

type QaTurn = QuickAskTurn;

function QuickAskApp() {
  const { t, i18n } = useTranslation("quickask");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const addSnippet = useGlobalStore((s) => s.addSnippet);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeGitScanRepos = useGlobalStore((s) => s.invokeGitScanRepos);
  const invokeGitIsRepo = useGlobalStore((s) => s.invokeGitIsRepo);
  const invokePickDirectory = useGlobalStore((s) => s.invokePickDirectory);

  const [ready, setReady] = useState(false);
  const [mode, setMode] = useState<"ask" | "tools">("ask");
  const [input, setInput] = useState("");
  const [task, setTask] = useState<TaskKind>("none");
  const [optimize, setOptimize] = useState<OptimizeConfig>(DEFAULT_OPTIMIZE);
  const [modelId, setModelId] = useState<string>("");
  const chips = settings.quickAskChips ?? DEFAULT_CHIPS;
  /** Repos given full-diff context (multi-select); empty = auto (current repo). */
  const [focusedRepos, setFocusedRepos] = useState<string[]>(settings.quickAskFocusRepos ?? []);
  /** Which toolbar popover is open — only one at a time (mutual exclusion). */
  const [openPop, setOpenPop] = useState<null | "model" | "task" | "focus">(null);
  /** Extra candidates pulled in by a folder scan (session-only). */
  const [extraRepos, setExtraRepos] = useState<GitScannedRepo[]>([]);
  const [scanning, setScanning] = useState(false);
  const [ctxOpen, setCtxOpen] = useState(false);
  /** Persisted-session id the current chat belongs to (null = fresh chat). */
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  /** Inline rename state: which session + draft title. */
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  /** Text currently selected inside the answer area (for quote follow-up). */
  const [selectedText, setSelectedText] = useState("");
  const [copied, setCopied] = useState(false);
  const [snippetSaved, setSnippetSaved] = useState(false);
  const [exportDone, setExportDone] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  const ctx = useAskContext(chips, focusedRepos);

  const models = aiModels as AIModelConfig[];
  const selectedModel = useMemo(() => {
    return models.find((m) => m.id === modelId) || models.find((m) => m.isDefault) || models[0];
  }, [models, modelId]);

  // stream <-> sessions reference each other's callbacks; bridge via ref.
  const persistRef = useRef<(turns: QaTurn[], firstQuestion: string) => Promise<void>>(
    async () => {},
  );
  const stream = useAskStream({
    selectedModel,
    task,
    optimize,
    input,
    setInput,
    setSelectedText,
    getContext: ctx.getContext,
    persistTurns: (turns, q) => {
      void persistRef.current(turns, q);
    },
  });
  const sessions = useAskSessions({
    task: encodeTask(task, optimize),
    activeSessionId,
    setActiveSessionId,
    setError: stream.setError,
    onRemovedActive: stream.resetChat,
  });
  persistRef.current = sessions.persistTurns;

  const snippets = useAskSnippets({ setInput, inputRef });

  useAskEvents({
    setSettings,
    getContext: ctx.getContext,
    invalidateContext: ctx.invalidateContext,
    setInput,
    inputRef,
    closePopovers: () => {
      snippets.setSnippetOpen(false);
      snippets.setParamTarget(null);
      setOpenPop(null);
      // Text arriving from snippets/browser must land in the visible ask view,
      // never into the hidden input while the tools panel is up.
      setMode("ask");
    },
    onPrefillTask: (raw) => {
      const parsed = parseTask(raw);
      setMode("ask");
      setTask(parsed.task);
      setOptimize(parsed.optimize);
    },
  });

  const setChip = (key: keyof typeof chips, value: boolean) => {
    setSettings({
      quickAskChips: { ...chips, [key]: value },
    });
  };

  const commitFocus = (next: string[]) => {
    setFocusedRepos(next);
    setSettings({ quickAskFocusRepos: next });
  };

  const toggleFocus = (path: string) => {
    commitFocus(
      focusedRepos.includes(path)
        ? focusedRepos.filter((p) => p !== path)
        : [...focusedRepos, path]
    );
  };

  /** Pick a folder, scan it for git repos, add them as candidates and select all. */
  const scanFolder = async () => {
    if (scanning) return;
    setScanning(true);
    try {
      const dir = await invokePickDirectory();
      if (!dir) return;
      let found: GitScannedRepo[];
      if (await invokeGitIsRepo(dir)) {
        found = [{ path: dir, name: projectNameFromPath(dir) }];
      } else {
        found = await invokeGitScanRepos(dir, 2);
      }
      if (found.length === 0) {
        stream.setError(t("focusScanEmpty"));
        return;
      }
      setExtraRepos((prev) => {
        const seen = new Set(prev.map((r) => r.path));
        return [...prev, ...found.filter((r) => !seen.has(r.path))];
      });
      // Select every repo just discovered (union with existing selection).
      commitFocus(Array.from(new Set([...focusedRepos, ...found.map((r) => r.path)])));
    } catch (e) {
      stream.setError(String(e));
    } finally {
      setScanning(false);
    }
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

  useEffect(() => {
    if (!modelId && selectedModel) setModelId(selectedModel.id);
  }, [modelId, selectedModel]);

  // Restore the last used task once, after settings finish loading.
  const lastTaskRestoredRef = useRef(false);
  useEffect(() => {
    if (!ready || lastTaskRestoredRef.current || !settings.quickAskLastTask) return;
    lastTaskRestoredRef.current = true;
    const parsed = parseTask(settings.quickAskLastTask);
    setTask(parsed.task);
    setOptimize(parsed.optimize);
  }, [ready, settings.quickAskLastTask]);

  const changeTask = (next: TaskKind) => {
    setTask(next);
    setSettings({ quickAskLastTask: encodeTask(next, optimize) });
  };

  const changeOptimize = (next: OptimizeConfig) => {
    setOptimize(next);
    if (task === "optimize") {
      setSettings({ quickAskLastTask: encodeTask("optimize", next) });
    }
  };

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

  const startNewChat = () => {
    if (stream.busy) return;
    stream.resetChat();
    setActiveSessionId(null);
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      // Layered escape: modal > snippet panel > session panel > context panel >
      // clear input > hide window.
      if (snippets.paramTarget) {
        snippets.setParamTarget(null);
        return;
      }
      if (snippets.snippetOpen) {
        snippets.setSnippetOpen(false);
        return;
      }
      if (openPop) {
        setOpenPop(null);
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
        if (!stream.busy) startNewChat();
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [snippets.paramTarget, snippets.snippetOpen, openPop, historyOpen, ctxOpen, input, stream.busy]);

  const togglePop = (id: NonNullable<typeof openPop>) => {
    setOpenPop((cur) => (cur === id ? null : id));
  };

  // Switching modes unmounts the toolbar; drop any popover that was open.
  useEffect(() => {
    setOpenPop(null);
  }, [mode]);

  // Re-showing the window after it was hidden with a popover open: start clean.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen("quick-ask-shown", () => setOpenPop(null)).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  // Clicking anywhere outside the toolbar closes the open popover. Clicks on
  // another trigger are handled by togglePop (mutual exclusion) instead.
  useEffect(() => {
    if (!openPop) return;
    const onDown = (e: MouseEvent) => {
      if (!(e.target as Element | null)?.closest?.(".qa-focus")) setOpenPop(null);
    };
    document.addEventListener("mousedown", onDown, true);
    return () => document.removeEventListener("mousedown", onDown, true);
  }, [openPop]);

  // Alt+1 / Alt+2: switch between ask and tools modes. Shift is excluded so
  // layout combos like Alt+Shift+2 don't flip the view by accident.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
      if (e.key === "1") {
        e.preventDefault();
        setMode("ask");
      } else if (e.key === "2") {
        e.preventDefault();
        setMode("tools");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const fillClipboard = async () => {
    try {
      const clip = await invoke<string>("read_clipboard");
      if (clip.trim()) setInput((prev) => (prev ? prev : clip.trim()));
    } catch (e) {
      stream.setError(String(e));
    }
  };

  /** Restore a persisted session into the current chat. */
  const openSession = (s: QuickAskSession) => {
    if (stream.busy) return;
    const turns = s.turns || [];
    stream.loadSession(turns);
    const parsed = parseTask(s.task);
    setTask(parsed.task);
    setOptimize(parsed.optimize);
    setActiveSessionId(s.id);
    setHistoryOpen(false);
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  const commitRename = async (id: string, title: string) => {
    if (!title.trim()) {
      setRenamingId(null);
      return;
    }
    if (await sessions.renameSession(id, title)) {
      setRenamingId(null);
    }
  };

  const handleExport = async () => {
    if (await sessions.exportActiveSession(stream.history)) {
      setExportDone(true);
      setTimeout(() => setExportDone(false), 1500);
    }
  };

  const saveAnswerAsSnippet = async () => {
    if (!stream.answer) return;
    const name = makeTitle(stream.lastQuestionRef.current || stream.answer, t("saveSnippet"));
    try {
      await addSnippet({ name, content: stream.answer, tags: "quickask,ai", params: "" });
      setSnippetSaved(true);
      window.setTimeout(() => setSnippetSaved(false), 1500);
    } catch (e) {
      stream.setError(String(e));
    }
  };

  const copyAnswer = async () => {
    if (!stream.answer) return;
    try {
      await invokeCopyToClipboard(stream.answer);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (e) {
      stream.setError(String(e));
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
      stream.setError(String(e));
    }
  };

  const openDeepSeek = async () => {
    try {
      await invoke("open_main_deepseek");
      await hide();
    } catch (e) {
      stream.setError(String(e));
    }
  };

  /** Editor-class tools live in the main window: switch tab and get out of the way. */
  const jumpToDevtools = async () => {
    try {
      await invoke("open_main_tab", { tab: "devtools" });
      await hide();
    } catch {
      /* window jump is best-effort */
    }
  };

  // Deduplicated repo candidates for the focus picker: current + recent +
  // folder-scanned + anything already selected (so scanned repos stay visible).
  const availableRepos = useMemo<RepoOption[]>(() => {
    const byPath = new Map<string, string>();
    const add = (path: string, name?: string) => {
      if (path && !byPath.has(path)) byPath.set(path, name || projectNameFromPath(path));
    };
    if (settings.currentGitRepo) add(settings.currentGitRepo);
    for (const p of recentProjects) add(p.path, p.name);
    for (const r of extraRepos) add(r.path, r.name);
    for (const p of focusedRepos) add(p);
    return Array.from(byPath, ([path, name]) => ({ path, name }));
  }, [settings.currentGitRepo, recentProjects, extraRepos, focusedRepos]);

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
          <div className="qa-mode-tabs">
            <button
              type="button"
              className={`qa-mode-btn${mode === "ask" ? " active" : ""}`}
              onClick={() => setMode("ask")}
              title={`${t("modeAsk")} (Alt+1)`}
            >
              <Sparkles size={11} /> {t("modeAsk")}
            </button>
            <button
              type="button"
              className={`qa-mode-btn${mode === "tools" ? " active" : ""}`}
              onClick={() => setMode("tools")}
              title={`${t("modeTools")} (Alt+2)`}
            >
              <Terminal size={11} /> {t("modeTools")}
            </button>
          </div>
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

      {mode === "tools" ? (
        <ToolsPanel onJump={() => void jumpToDevtools()} />
      ) : (
        <>
      <div className="quick-ask-toolbar">
        <QuickSelect
          value={selectedModel?.id || ""}
          options={models.map((m) => ({ value: m.id, label: `${m.name} (${m.model})` }))}
          onChange={setModelId}
          placeholder={t("noModel")}
          alignLeft
          open={openPop === "model"}
          onToggle={() => togglePop("model")}
        />
        <QuickSelect
          value={task}
          options={TASK_KINDS.map((k) => ({ value: k, label: t(`task.${k}`) }))}
          onChange={(v) => changeTask(v as TaskKind)}
          alignLeft
          open={openPop === "task"}
          onToggle={() => togglePop("task")}
        />
        <FocusPicker
          open={openPop === "focus"}
          onToggleOpen={() => togglePop("focus")}
          candidates={availableRepos}
          selected={focusedRepos}
          onToggle={toggleFocus}
          onClear={() => commitFocus([])}
          onScanFolder={() => void scanFolder()}
          scanning={scanning}
        />
      </div>

      <ContextBar
        chips={chips}
        setChip={setChip}
        onFillClipboard={() => void fillClipboard()}
        hasSnippets={(snippets.snippets?.length ?? 0) > 0}
        snippetOpen={snippets.snippetOpen}
        onToggleSnippets={() => snippets.setSnippetOpen((v) => !v)}
        ctxOpen={ctxOpen}
        onToggleCtx={() => setCtxOpen((v) => !v)}
        ctxLoading={ctx.ctxLoading}
        ctxPreview={ctx.ctxPreview}
        ctxTruncated={ctx.ctxTruncated}
      />

      {snippets.snippetOpen && (
        <SnippetPicker
          snippetQuery={snippets.snippetQuery}
          setSnippetQuery={snippets.setSnippetQuery}
          snippetChoices={snippets.snippetChoices}
          onPick={snippets.pickSnippet}
        />
      )}

      {historyOpen && (
        <SessionHistory
          sessions={sessions.quickAskSessions}
          activeSessionId={activeSessionId}
          busy={stream.busy}
          renamingId={renamingId}
          renameDraft={renameDraft}
          setRenameDraft={setRenameDraft}
          onStartRename={(s) => {
            setRenamingId(s.id);
            setRenameDraft(s.title || "");
          }}
          onCommitRename={(id, title) => void commitRename(id, title)}
          onCancelRename={() => setRenamingId(null)}
          onOpen={openSession}
          onRemove={(id) => void sessions.removeSession(id)}
          onNewChat={startNewChat}
          relativeTime={sessions.relativeTime}
        />
      )}

      {task === "optimize" && <OptimizeBar value={optimize} onChange={changeOptimize} />}

      <AskPanel
        input={input}
        setInput={setInput}
        inputRef={inputRef}
        busy={stream.busy}
        error={stream.error}
        answer={stream.answer}
        historyCount={stream.history.length}
        canRegenerate={Boolean(stream.lastQuestionRef.current)}
        copied={copied}
        snippetSaved={snippetSaved}
        exportDone={exportDone}
        selectedText={selectedText}
        setSelectedText={setSelectedText}
        onSend={() => void stream.send()}
        onStop={() => void stream.stopStream()}
        onRegenerate={() =>
          void stream.send(stream.lastQuestionRef.current, stream.lastHistoryRef.current)
        }
        onCopyAnswer={() => void copyAnswer()}
        onSaveSnippet={() => void saveAnswerAsSnippet()}
        onOpenDeepSeek={() => void openDeepSeek()}
        onExport={() => void handleExport()}
        onNewChat={startNewChat}
        onQuote={quoteSelection}
        onCopySelection={() => void copySelection()}
      />

      {snippets.paramTarget && (
        <ParamModal
          paramValues={snippets.paramValues}
          setParamValues={snippets.setParamValues}
          onClose={() => snippets.setParamTarget(null)}
          onConfirm={() => void snippets.confirmSnippetParams()}
        />
      )}
        </>
      )}
    </div>
  );
}

export default QuickAskApp;
