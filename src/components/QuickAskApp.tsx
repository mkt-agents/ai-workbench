import { useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { FilePlus2, History, MessageSquarePlus, Pin, PinOff, Sparkles, Terminal, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useGlobalStore } from "../core/store";
import type { AIModelConfig, GitScannedRepo, QuickAskSession, QuickAskTurn, SnippetTaskBind } from "../core/types";
import { bootApp } from "../core/boot";
import { projectNameFromPath } from "../core/pathUtils";
import { writeStoredString } from "../core/localState";
import { applyDocumentTheme } from "../lib/theme";
import { resolveSceneModel } from "../lib/aiSceneModels";
import { DEVTOOLS_TOOL_KEY } from "./DevTools";
import {
  canAddAttachment,
  makeAttachment,
  type AskAttachment,
} from "../lib/askAttachments";
import { findProfile, mergeProfiles } from "../lib/quickAskProfiles";
import AskPanel from "./quickAsk/AskPanel";
import ContextPicker from "./quickAsk/ContextPicker";
import {
  buildPickerGroups,
  describeSelection,
  resolveFocus,
  toggleGroup,
  toggleService,
} from "./quickAsk/repoSelection";
import type { PickerGroup } from "./quickAsk/repoSelection";
import OptimizeBar from "./quickAsk/OptimizeBar";
import ParamModal from "./quickAsk/ParamModal";
import QuickSelect from "./quickAsk/QuickSelect";
import SessionHistory from "./quickAsk/SessionHistory";
import SnippetPicker from "./quickAsk/SnippetPicker";
import ToolsPanel from "./quickAsk/ToolsPanel";
import {
  DEFAULT_CHIPS,
  DEFAULT_OPTIMIZE,
  encodeTask,
  makeTitle,
  parseTask,
} from "./quickAsk/config";
import type { OptimizeConfig, TaskKind } from "./quickAsk/config";
import { routeQuestion } from "./quickAsk/route";
import type { RouteOverride } from "./quickAsk/route";
import { useAskContext } from "./quickAsk/useAskContext";
import { useAskEvents } from "./quickAsk/useAskEvents";
import { useAskSessions } from "./quickAsk/useAskSessions";
import { useAskSnippets } from "./quickAsk/useAskSnippets";
import { useAskStream } from "./quickAsk/useAskStream";

type QaTurn = QuickAskTurn;

const PINNED_KEY = "qa-pinned";

function QuickAskApp() {
  const { t, i18n } = useTranslation("quickask");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const addSnippet = useGlobalStore((s) => s.addSnippet);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeGitScanRepos = useGlobalStore((s) => s.invokeGitScanRepos);
  const invokeGitIsRepo = useGlobalStore((s) => s.invokeGitIsRepo);
  const invokePickDirectory = useGlobalStore((s) => s.invokePickDirectory);
  const invokePickTextFile = useGlobalStore((s) => s.invokePickTextFile);
  const invokeSetQuickAskPinned = useGlobalStore((s) => s.invokeSetQuickAskPinned);

  const [ready, setReady] = useState(false);
  const [pinned, setPinned] = useState(() => localStorage.getItem(PINNED_KEY) === "1");
  useEffect(() => {
    localStorage.setItem(PINNED_KEY, pinned ? "1" : "0");
    void invokeSetQuickAskPinned(pinned).catch(() => {});
  }, [pinned, invokeSetQuickAskPinned]);
  const togglePinned = () => setPinned((v) => !v);
  const [mode, setMode] = useState<"ask" | "tools">("ask");
  const [input, setInput] = useState("");
  const [task, setTask] = useState<TaskKind>("none");
  const [optimize, setOptimize] = useState<OptimizeConfig>(DEFAULT_OPTIMIZE);
  const [modelId, setModelId] = useState<string>("");
  const [attachments, setAttachments] = useState<AskAttachment[]>([]);
  const chips = settings.quickAskChips ?? DEFAULT_CHIPS;
  /** Workspace tokens (`ws:<id>`) and/or service paths; empty = auto (current repo). */
  const [focusedRepos, setFocusedRepos] = useState<string[]>(settings.quickAskFocusRepos ?? []);
  /** Which popover is open — only one at a time (mutual exclusion). */
  const [openPop, setOpenPop] = useState<null | "model" | "focus" | "route" | "profile">(null);
  /** Composer pill override for the next send only; "auto" = routeQuestion decides. */
  const [routeOverride, setRouteOverride] = useState<RouteOverride>("auto");
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
  /** Transcript index of the assistant message that was just copied / saved. */
  const [copied, setCopied] = useState<number | null>(null);
  const [snippetSaved, setSnippetSaved] = useState<number | null>(null);
  const [exportDone, setExportDone] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  const repoGroups = useMemo(
    () =>
      buildPickerGroups(gitWorkspaces, recentProjects, extraRepos, settings.currentGitRepo, focusedRepos),
    [gitWorkspaces, recentProjects, extraRepos, settings.currentGitRepo, focusedRepos],
  );
  const resolvedFocus = useMemo(() => resolveFocus(focusedRepos, repoGroups), [focusedRepos, repoGroups]);

  const ctx = useAskContext(chips, resolvedFocus);

  const models = aiModels as AIModelConfig[];
  const profiles = useMemo(
    () => mergeProfiles(settings.quickAskProfiles),
    [settings.quickAskProfiles]
  );
  const activeProfile = useMemo(
    () => findProfile(settings.quickAskProfileId, settings.quickAskProfiles),
    [settings.quickAskProfileId, settings.quickAskProfiles]
  );
  const selectedModel = useMemo(() => {
    const scene = task === "optimize" ? "optimize" : "quickAsk";
    const override = modelId || activeProfile?.modelId || null;
    return resolveSceneModel(scene, models, settings.aiSceneModels, override) ?? undefined;
  }, [models, modelId, task, settings.aiSceneModels, activeProfile?.modelId]);

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
    routeOverride,
    onSent: () => {
      // Quick actions and the pill override are one-shot.
      setTask("none");
      setRouteOverride("auto");
      setAttachments([]);
    },
    persistTurns: (turns, q) => {
      void persistRef.current(turns, q);
    },
    profileSystemRole: activeProfile?.systemRole,
    profileTemperature: activeProfile?.temperature,
    attachments,
  });
  const sessions = useAskSessions({
    task: encodeTask(task, optimize),
    activeSessionId,
    setActiveSessionId,
    setError: stream.setError,
    onRemovedActive: stream.resetChat,
  });
  persistRef.current = sessions.persistTurns;

  const snippets = useAskSnippets({ setInput, inputRef, input });
  const setComposerInput = (updater: string | ((prev: string) => string)) => {
    setInput((prev) => {
      const next = typeof updater === "function" ? updater(prev) : updater;
      // Defer so slash sync sees the committed value after this setState.
      queueMicrotask(() => snippets.syncSlashFromInput(next));
      return next;
    });
  };

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
    // An external prefill carrying a task arms it as a one-shot quick action.
    onPrefillTask: (raw) => {
      const parsed = parseTask(raw);
      setMode("ask");
      setTask(parsed.task);
      setOptimize(parsed.optimize);
    },
  });

  const deferredInput = useDeferredValue(input);
  const routePreview = useMemo(
    () =>
      routeQuestion({
        question: deferredInput,
        task,
        lastMode: stream.lastModeRef.current,
        override: routeOverride,
      }),
    // lastModeRef changes only on send, which also re-renders via `answer`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [deferredInput, task, routeOverride, stream.answer, stream.history.length],
  );

  const setChip = (key: keyof typeof chips, value: boolean) => {
    setSettings({
      quickAskChips: { ...chips, [key]: value },
    });
  };

  const commitFocus = (next: string[]) => {
    setFocusedRepos(next);
    setSettings({ quickAskFocusRepos: next });
  };

  const onToggleGroup = (group: PickerGroup) => commitFocus(toggleGroup(focusedRepos, group));
  const onToggleService = (group: PickerGroup, path: string) =>
    commitFocus(toggleService(focusedRepos, group, path));

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

  // Only the optimize scenario/goals are remembered; the task itself is one-shot
  // and always starts as plain Q&A.
  const optimizeRestoredRef = useRef(false);
  useEffect(() => {
    if (!ready || optimizeRestoredRef.current || !settings.quickAskLastTask) return;
    optimizeRestoredRef.current = true;
    const parsed = parseTask(settings.quickAskLastTask);
    if (parsed.task === "optimize") setOptimize(parsed.optimize);
  }, [ready, settings.quickAskLastTask]);

  const changeTask = (next: TaskKind) => {
    setTask(next);
    // If a text snippet is bound to this task and the composer is empty, insert it.
    const bindable: SnippetTaskBind[] = ["debug", "explain", "polish", "translate", "summarize"];
    if (bindable.includes(next as SnippetTaskBind) && !input.trim()) {
      const bound = (snippets.snippets || []).find(
        (s) => s.kind !== "prompt" && s.taskBind === next
      );
      if (bound?.content) {
        setComposerInput(bound.content);
        void useGlobalStore.getState().bumpSnippetUse(bound.id).catch(() => {});
      }
    }
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  const applyProfile = (id: string) => {
    if (!id) {
      setSettings({ quickAskProfileId: undefined });
      return;
    }
    setSettings({ quickAskProfileId: id });
    const p = findProfile(id, settings.quickAskProfiles);
    if (p?.modelId && models.some((m) => m.id === p.modelId)) {
      setModelId(p.modelId);
    }
  };

  const addAttachmentFromFile = async () => {
    try {
      const text = await invokePickTextFile(t("attach.pickTitle"));
      if (!text?.trim()) return;
      const name = `file-${attachments.length + 1}.txt`;
      const check = canAddAttachment(attachments, { name, content: text });
      if (!check.ok) {
        stream.setError(t(check.reason === "count" ? "attach.tooMany" : "attach.tooLarge"));
        return;
      }
      setAttachments((prev) => [...prev, makeAttachment(name, text)]);
    } catch (e) {
      if (String(e).includes("已取消") || String(e).toLowerCase().includes("cancel")) return;
      stream.setError(String(e));
    }
  };

  const addAttachmentFromInput = () => {
    const text = input.trim();
    if (!text) return;
    const name = `paste-${attachments.length + 1}.txt`;
    const check = canAddAttachment(attachments, { name, content: text });
    if (!check.ok) {
      stream.setError(t(check.reason === "count" ? "attach.tooMany" : "attach.tooLarge"));
      return;
    }
    setAttachments((prev) => [...prev, makeAttachment(name, text)]);
  };

  const changeOptimize = (next: OptimizeConfig) => {
    setOptimize(next);
    setSettings({ quickAskLastTask: encodeTask("optimize", next) });
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
    setTask("none");
    setRouteOverride("auto");
    setAttachments([]);
    setActiveSessionId(null);
    setHistoryOpen(false);
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  const sendAsk = () => {
    setHistoryOpen(false);
    void stream.send();
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      // Layered escape: stop generating > modal > snippet panel > session panel >
      // context panel > clear input > hide window.
      if (stream.busy) {
        void stream.stopStream();
        return;
      }
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
      if (task !== "none") {
        setTask("none");
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
  }, [snippets.paramTarget, snippets.snippetOpen, openPop, historyOpen, ctxOpen, task, input, stream.busy]);

  const togglePop = (id: NonNullable<typeof openPop>) => {
    setOpenPop((cur) => (cur === id ? null : id));
  };

  // Switching modes unmounts ask chrome; drop popovers / history drawer.
  useEffect(() => {
    setOpenPop(null);
    setHistoryOpen(false);
  }, [mode]);

  // Re-showing the window: drop popovers/history and park focus in the composer.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen("quick-ask-shown", () => {
      setOpenPop(null);
      setHistoryOpen(false);
      requestAnimationFrame(() => inputRef.current?.focus());
    }).then((fn) => {
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
    setTask("none");
    setRouteOverride("auto");
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

  /** `index` points at an assistant turn in the transcript. */
  const saveAnswerAsSnippet = async (index: number) => {
    const text = stream.history[index]?.content;
    if (!text) return;
    const question = stream.history[index - 1]?.content;
    const name = makeTitle(question || text, t("saveSnippet"));
    try {
      await addSnippet({ name, content: text, tags: "quickask,ai", params: "" });
      setSnippetSaved(index);
      window.setTimeout(() => setSnippetSaved((cur) => (cur === index ? null : cur)), 1500);
    } catch (e) {
      stream.setError(String(e));
    }
  };

  const copyAnswer = async (index: number) => {
    const text = stream.history[index]?.content;
    if (!text) return;
    try {
      await invokeCopyToClipboard(text);
      setCopied(index);
      window.setTimeout(() => setCopied((cur) => (cur === index ? null : cur)), 1500);
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

  /** Editor-class tools live in the main window: select the tool (shared localStorage +
      storage 事件让已挂载的 DevTools 跟着切), switch tab and get out of the way. */
  const jumpToDevtools = async (toolId?: string) => {
    try {
      if (toolId) writeStoredString(DEVTOOLS_TOOL_KEY, toolId);
      await invoke("open_main_tab", { tab: "devtools" });
      await hide();
    } catch {
      /* window jump is best-effort */
    }
  };

  const autoRepoName = settings.currentGitRepo ? projectNameFromPath(settings.currentGitRepo) : null;
  /** Selection label for the composer pill in project mode; null = nothing to analyze. */
  const projectLabel = useMemo(
    () => describeSelection(focusedRepos, repoGroups, autoRepoName, (k, p) => t(k, p)),
    [focusedRepos, repoGroups, autoRepoName, t],
  );
  const pickerLabel =
    focusedRepos.length === 0 || !projectLabel
      ? autoRepoName
        ? t("ctx.autoRepo", { name: autoRepoName })
        : t("ctx.noRepo")
      : projectLabel;

  const previewContext = async () => {
    if (ctxOpen) {
      setCtxOpen(false);
      return;
    }
    await ctx.getContext("project");
    setCtxOpen(true);
    setOpenPop(null);
  };

  if (!ready) {
    return <div className="quick-ask-root loading">{t("loading")}</div>;
  }

  return (
    <div className="quick-ask-root">
      <header className="quick-ask-header" data-tauri-drag-region>
        <div className="quick-ask-title" data-tauri-drag-region>
          <Sparkles size={15} strokeWidth={1.75} />
          <span>{t("title")}</span>
        </div>
        <div className="quick-ask-header-actions">
          <div className="qa-mode-tabs" role="tablist" aria-label={t("title")}>
            <button
              type="button"
              role="tab"
              aria-selected={mode === "ask"}
              className={`qa-mode-btn${mode === "ask" ? " active" : ""}`}
              onClick={() => setMode("ask")}
              title={`${t("modeAsk")} (Alt+1)`}
            >
              <Sparkles size={11} /> {t("modeAsk")}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={mode === "tools"}
              className={`qa-mode-btn${mode === "tools" ? " active" : ""}`}
              onClick={() => setMode("tools")}
              title={`${t("modeTools")} (Alt+2)`}
            >
              <Terminal size={11} /> {t("modeTools")}
            </button>
          </div>
          {mode === "ask" && (
            <>
              <button
                type="button"
                className="btn-icon"
                onClick={startNewChat}
                disabled={stream.busy}
                title={`${t("newChat")} (Ctrl+N)`}
              >
                <MessageSquarePlus size={15} />
              </button>
              <button
                type="button"
                className={`btn-icon qa-history-btn${historyOpen ? " on" : ""}`}
                onClick={() => setHistoryOpen((v) => !v)}
                title={t("history")}
                aria-expanded={historyOpen}
              >
                <History size={15} />
              </button>
            </>
          )}
          <button
            type="button"
            className={`btn-icon qa-pin-btn${pinned ? " is-active" : ""}`}
            onClick={togglePinned}
            title={pinned ? t("unpin") : t("pin")}
            aria-pressed={pinned}
          >
            {pinned ? <Pin size={14} /> : <PinOff size={14} />}
          </button>
          <button type="button" className="btn-icon" onClick={() => void hide()} title={t("close")}>
            <X size={16} />
          </button>
        </div>
      </header>

      {mode === "tools" ? (
        <ToolsPanel onJump={(toolId) => void jumpToDevtools(toolId)} />
      ) : (
        <>
          <div className="quick-ask-toolbar">
            <QuickSelect
              value={settings.quickAskProfileId || ""}
              options={[
                { value: "", label: t("profile.none") },
                ...profiles.map((p) => ({
                  value: p.id,
                  label: p.nameKey ? t(p.nameKey) : p.name,
                })),
              ]}
              onChange={applyProfile}
              placeholder={t("profile.none")}
              alignLeft
              open={openPop === "profile"}
              onToggle={() => togglePop("profile")}
            />
            <QuickSelect
              value={selectedModel?.id || ""}
              options={models.map((m) => ({ value: m.id, label: `${m.name} (${m.model})` }))}
              onChange={setModelId}
              placeholder={t("noModel")}
              alignLeft
              open={openPop === "model"}
              onToggle={() => togglePop("model")}
            />
            <ContextPicker
              open={openPop === "focus"}
              onToggleOpen={() => togglePop("focus")}
              groups={repoGroups}
              selection={focusedRepos}
              label={pickerLabel}
              currentRepo={settings.currentGitRepo || null}
              onToggleGroup={onToggleGroup}
              onToggleService={onToggleService}
              onClear={() => commitFocus([])}
              onScanFolder={() => void scanFolder()}
              scanning={scanning}
              chips={chips}
              setChip={setChip}
              onPreview={() => void previewContext()}
              previewLoading={ctx.ctxLoading}
            />
          </div>

          <div className="qa-attach-bar">
            <button
              type="button"
              className="qa-mini-btn"
              onClick={() => void addAttachmentFromFile()}
              title={t("attach.file")}
              disabled={stream.busy}
            >
              <FilePlus2 size={13} /> {t("attach.file")}
            </button>
            <button
              type="button"
              className="qa-mini-btn"
              onClick={addAttachmentFromInput}
              title={t("attach.fromInput")}
              disabled={stream.busy || !input.trim()}
            >
              {t("attach.fromInput")}
            </button>
            {attachments.map((a) => (
              <span key={a.id} className="qa-attach-chip" title={`${a.name} · ${a.content.length} chars`}>
                {a.name}
                <button
                  type="button"
                  className="qa-attach-chip-x"
                  onClick={() => setAttachments((prev) => prev.filter((x) => x.id !== a.id))}
                  aria-label={t("attach.remove")}
                >
                  <X size={11} />
                </button>
              </span>
            ))}
          </div>

          {ctx.ctxTruncated && ctxOpen && <div className="quick-ask-hint">{t("truncated")}</div>}
          {ctxOpen && (
            <pre className="quick-ask-ctx-preview">{ctx.ctxPreview || t("ctx.previewEmpty")}</pre>
          )}

          {snippets.snippetOpen && (
            <SnippetPicker
              snippetQuery={snippets.snippetQuery}
              setSnippetQuery={snippets.setSnippetQuery}
              snippetChoices={snippets.snippetChoices}
              onPick={snippets.pickSnippet}
            />
          )}

          {task === "optimize" && <OptimizeBar value={optimize} onChange={changeOptimize} />}

          <AskPanel
            input={input}
            setInput={setComposerInput}
            inputRef={inputRef}
            busy={stream.busy}
            phase={stream.phase}
            modelName={selectedModel?.name || ""}
            error={stream.error}
            failedMode={stream.failedMode}
            onRetry={(asChat) => stream.retry(asChat)}
            onDismissError={stream.dismissError}
            lastQuestion={stream.lastQuestionRef.current}
            onExample={(text, exampleTask) => {
              setComposerInput(text);
              if (exampleTask) changeTask(exampleTask);
              else requestAnimationFrame(() => inputRef.current?.focus());
            }}
            answer={stream.answer}
            turns={stream.history}
            pending={stream.pending}
            source={stream.source}
            canRegenerate={Boolean(stream.lastQuestionRef.current)}
            copied={copied}
            snippetSaved={snippetSaved}
            exportDone={exportDone}
            selectedText={selectedText}
            setSelectedText={setSelectedText}
            route={routePreview}
            routeOverride={routeOverride}
            onRouteOverride={setRouteOverride}
            routeMenuOpen={openPop === "route"}
            onToggleRouteMenu={() => togglePop("route")}
            projectLabel={projectLabel}
            task={task}
            onTask={changeTask}
            hasSnippets={(snippets.snippets?.length ?? 0) > 0}
            snippetOpen={snippets.snippetOpen}
            onToggleSnippets={() => snippets.openManual()}
            onPasteClipboard={() => void fillClipboard()}
            onSend={sendAsk}
            onStop={() => void stream.stopStream()}
            onRegenerate={() => void stream.regenerate()}
            onCopyAnswer={(i) => void copyAnswer(i)}
            onSaveSnippet={(i) => void saveAnswerAsSnippet(i)}
            onOpenDeepSeek={() => void openDeepSeek()}
            onExport={() => void handleExport()}
            onQuote={quoteSelection}
            onCopySelection={() => void copySelection()}
          />

          {historyOpen && (
            <div
              className="qa-history-overlay"
              role="dialog"
              aria-label={t("history")}
              onMouseDown={(e) => {
                if (e.target === e.currentTarget) setHistoryOpen(false);
              }}
            >
              <div className="qa-history-panel">
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
              </div>
            </div>
          )}

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
