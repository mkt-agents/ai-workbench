import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useGlobalStore } from "../core/store";
import type { AIModelConfig, AiHubSession, AiHubTurn, SnippetTaskBind } from "../core/types";
import { bootApp } from "../core/boot";
import { writeStoredString } from "../core/localState";
import { applyDocumentTheme } from "../lib/theme";
import { resolveSceneModel } from "../lib/aiSceneModels";
import { DEVTOOLS_TOOL_KEY } from "./DevTools";
import {
  canAddAttachment,
  makeAttachment,
  type AskAttachment,
} from "../lib/askAttachments";
import AskPanel from "./aiHub/AskPanel";
import AiHubHeader, { type AiHubMode } from "./aiHub/AiHubHeader";
import AttachmentBar from "./aiHub/AttachmentBar";
import ContextPicker from "./aiHub/ContextPicker";
import OptimizeBar from "./aiHub/OptimizeBar";
import ParamModal from "./aiHub/ParamModal";
import QuickSelect from "./aiHub/QuickSelect";
import SessionHistory from "./aiHub/SessionHistory";
import SnippetPicker from "./aiHub/SnippetPicker";
import ToolsPanel from "./aiHub/ToolsPanel";
import { useNextSuggestion } from "./aiHub/useNextSuggestion";
import { DEFAULT_CHIPS, DEFAULT_OPTIMIZE, encodeTask, parseTask } from "./aiHub/config";
import type { OptimizeConfig, TaskKind } from "./aiHub/config";
import { routeQuestion } from "./aiHub/route";
import { openSuggestions, type InstalledApp, type OpenSuggestion } from "./aiHub/localActions";
import { GIT_SUBTAB_KEY } from "./GitManager";
import type { RouteOverride } from "./aiHub/route";
import { useAskContext } from "./aiHub/useAskContext";
import { useAskEvents } from "./aiHub/useAskEvents";
import { useAskSessions } from "./aiHub/useAskSessions";
import { useAskSnippets } from "./aiHub/useAskSnippets";
import { AGENT_PERMISSIONS, useAskStream, type AgentPermission } from "./aiHub/useAskStream";
import { useConfirm } from "./ConfirmModal";
import { COMPUTER_TOOL_NAMES } from "./aiHub/AgentActivity";
import { DAILY_STYLES, type DailyStyle } from "./aiHub/dailyReport";
import { useRepoFocus } from "./aiHub/useRepoFocus";
import { useAnswerActions } from "./aiHub/useAnswerActions";
import {
  hideAiHub as hide, useModeHotkeys, useResizingBodyClass, useTimedFlag, useWindowBounds,
} from "./aiHub/useAiHubWindow";

type QaTurn = AiHubTurn;

const PINNED_KEY = "qa-pinned";
const PERMISSION_KEY = "qa-permission";
/** Set once the user confirmed what 完全权限 means; later switches don't ask again.
    v2: the level now reaches the whole machine, so earlier acks must see the new warning. */
const FULL_ACK_KEY = "qa-full-ack-v2";
/** Set once the user agreed to screenshots going to the model provider. */
const COMPUTER_ACK_KEY = "qa-computer-ack";
/** Ctrl+. cycles these; 完全权限 is only reachable from the menu. */
const HOTKEY_CYCLE: AgentPermission[] = ["read", "workspace", "review"];
const DAILY_STYLE_KEY = "qa-daily-style";
const INSTALLED_TTL_MS = 10 * 60 * 1000;

function readDailyStyle(): DailyStyle {
  const stored = localStorage.getItem(DAILY_STYLE_KEY) as DailyStyle | null;
  return stored && DAILY_STYLES.includes(stored) ? stored : "chat";
}

/** Stored level; the pre-levels agent switch (`qa-agent=1`) becomes 编辑权限. */
function readPermission(): AgentPermission {
  const stored = localStorage.getItem(PERMISSION_KEY) as AgentPermission | null;
  if (stored && AGENT_PERMISSIONS.includes(stored)) return stored;
  return localStorage.getItem("qa-agent") === "1" ? "workspace" : "read";
}

function AiHubApp() {
  const { t, i18n } = useTranslation("aihub");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const invokeGenerateText = useGlobalStore((s) => s.invokeGenerateText);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const quickAppLaunchers = useGlobalStore((s) => s.quickAppLaunchers);
  const invokeLaunchApp = useGlobalStore((s) => s.invokeLaunchApp);
  const invokeScanDesktopApps = useGlobalStore((s) => s.invokeScanDesktopApps);
  // Start Menu / Desktop programs for "打开 …"; rescanned on show once the list is stale.
  const [installedApps, setInstalledApps] = useState<InstalledApp[]>([]);
  const installedScanAt = useRef(0);
  const refreshInstalledApps = useCallback(() => {
    if (installedScanAt.current && Date.now() - installedScanAt.current < INSTALLED_TTL_MS) return;
    installedScanAt.current = Date.now();
    invokeScanDesktopApps()
      .then((list) => setInstalledApps(list.map((app) => ({ name: app.display_name, path: app.exe_path }))))
      .catch(() => {
        installedScanAt.current = 0;
      });
  }, [invokeScanDesktopApps]);
  useEffect(() => refreshInstalledApps(), [refreshInstalledApps]);
  const invokePickTextFile = useGlobalStore((s) => s.invokePickTextFile);
  const invokeSetAiHubPinned = useGlobalStore((s) => s.invokeSetAiHubPinned);

  const [ready, setReady] = useState(false);
  const [pinned, setPinned] = useState(() => localStorage.getItem(PINNED_KEY) === "1");
  const [permission, setPermission] = useState<AgentPermission>(readPermission);
  const [dailyStyle, setDailyStyleState] = useState<DailyStyle>(readDailyStyle);
  const setDailyStyle = (style: DailyStyle) => {
    setDailyStyleState(style);
    localStorage.setItem(DAILY_STYLE_KEY, style);
  };
  useEffect(() => {
    localStorage.setItem(PINNED_KEY, pinned ? "1" : "0");
    void invokeSetAiHubPinned(pinned).catch(() => {});
  }, [pinned, invokeSetAiHubPinned]);
  const togglePinned = () => setPinned((v) => !v);
  const [mode, setMode] = useState<AiHubMode>("ask");
  const [input, setInput] = useState("");
  const [task, setTask] = useState<TaskKind>("none");
  const [optimize, setOptimize] = useState<OptimizeConfig>(DEFAULT_OPTIMIZE);
  const [modelId, setModelId] = useState<string>("");
  const [attachments, setAttachments] = useState<AskAttachment[]>([]);
  const chips = settings.aiHubChips ?? DEFAULT_CHIPS;
  /** Which popover is open — only one at a time (mutual exclusion). */
  const [openPop, setOpenPop] = useState<null | "model" | "focus" | "permission" | "daily">(null);
  /** Composer pill override for the next send only; "auto" = routeQuestion decides. */
  const [routeOverride, setRouteOverride] = useState<RouteOverride>("auto");
  /** Write-capable levels always work on the project; read-only follows the auto route. */
  const effectiveOverride: RouteOverride = permission === "read" ? routeOverride : "project";
  const [ctxOpen, setCtxOpen] = useState(false);
  /** Persisted-session id the current chat belongs to (null = fresh chat). */
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  /** Inline rename state: which session + draft title. */
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  /** Text currently selected inside the answer area (for quote follow-up). */
  const [selectedText, setSelectedText] = useState("");
  /** Brief floating notice: an action refused mid-answer, or a level change for the next message. */
  const [hintOn, flashHint] = useTimedFlag(2500);
  const [hintText, setHintText] = useState("");
  const showHint = (text: string) => {
    setHintText(text);
    flashHint();
  };
  const flashBusyHint = () => showHint(t("busyHint"));
  const confirm = useConfirm();
  /** Esc dismissed the open-suggestion list; typing brings it back. */
  const [openDismissed, setOpenDismissed] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // `stream` is declared below; the error callback only runs later, from a click.
  const repo = useRepoFocus({ onError: (msg) => stream.setError(msg) });

  const ctx = useAskContext(chips, repo.resolvedFocus);

  const models = aiModels as AIModelConfig[];
  const selectedModel = useMemo(() => {
    const scene = task === "optimize" ? "optimize" : "aiHub";
    return resolveSceneModel(scene, models, settings.aiSceneModels, modelId || null) ?? undefined;
  }, [models, modelId, task, settings.aiSceneModels]);

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
    collectDailyBrief: ctx.collectDailyBrief,
    routeOverride: effectiveOverride,
    dailyStyle,
    onSent: () => {
      // Quick actions and the pill override are one-shot.
      setTask("none");
      setRouteOverride("auto");
      setAttachments([]);
    },
    persistTurns: (turns, q) => {
      void persistRef.current(turns, q);
    },
    attachments,
    agent: {
      permission,
      vision: Boolean(selectedModel?.vision),
      repoPath: repo.resolvedFocus[0] || settings.currentGitRepo || "",
      apps: quickAppLaunchers.map((app) => ({
        name: app.name,
        path: app.path,
        args: app.args || "",
      })),
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

  const answers = useAnswerActions({
    history: stream.history,
    setError: stream.setError,
    setInput,
    inputRef,
    selectedText,
    setSelectedText,
    exportSession: sessions.exportActiveSession,
  });

  const snippets = useAskSnippets({ setInput, inputRef, input });
  const setComposerInput = (updater: string | ((prev: string) => string)) => {
    setInput((prev) => {
      const next = typeof updater === "function" ? updater(prev) : updater;
      // Defer so slash sync sees the committed value after this setState.
      queueMicrotask(() => snippets.syncSlashFromInput(next));
      return next;
    });
  };

  // LLM 预测不合格/失败时的确定性候选：从上一问的关键词直接给高置信建议
  const buildFallbackSuggestion = useCallback((): string | null => {
    const q =
      [...stream.history].reverse().find((x) => x.role === "user" && !x.digest)?.content || "";
    if (/未提交|有什么改动|哪些改动|哪些文件|改了什么|提交了什么|什么变更|git status/i.test(q)) {
      return t("suggestDirty");
    }
    return null;
  }, [stream.history, t]);

  const { suggestion: nextSuggestion, dismiss: dismissSuggestion } = useNextSuggestion({
    model: selectedModel,
    turns: stream.history,
    busy: stream.busy,
    generate: invokeGenerateText,
    fallback: buildFallbackSuggestion,
  });

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
        override: effectiveOverride,
      }),
    // lastModeRef changes only on send, which also re-renders via `answer`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [deferredInput, task, effectiveOverride, stream.answer, stream.history.length],
  );
  const contextIncluded = routePreview.mode === "project";
  /** Repo name click: flip the auto decision for the next message, or go back to auto. */
  const toggleInclude = () => {
    if (permission !== "read") return;
    setRouteOverride((cur) => (cur !== "auto" ? "auto" : contextIncluded ? "chat" : "project"));
  };
  const includeTip = (() => {
    const name = repo.projectLabel || t("ctx.noRepo");
    if (permission !== "read") return t("ctx.includeLocked", { name });
    const state = contextIncluded ? t("ctx.includeOn", { name }) : t("ctx.includeOff");
    return `${state} · ${t(`route.reason.${routePreview.reason}`)}`;
  })();

  /** Level for the next message. Mid-answer is fine: the running round keeps its own. */
  const changePermission = async (next: AgentPermission) => {
    if (next === permission) return;
    if (next === "full" && localStorage.getItem(FULL_ACK_KEY) !== "1") {
      const ok = await confirm({
        title: t("permission.fullConfirmTitle"),
        message: t("permission.fullConfirmBody"),
        warning: t("permission.fullConfirmWarn"),
        confirmText: t("permission.fullConfirmOk"),
        icon: "warning",
      });
      if (!ok) return;
      localStorage.setItem(FULL_ACK_KEY, "1");
    }
    setPermission(next);
    localStorage.setItem(PERMISSION_KEY, next);
    if (stream.busy) showHint(t("permission.nextAs", { name: t(`permission.${next}`) }));
  };

  const setChip = (key: keyof typeof chips, value: boolean) => {
    setSettings({
      aiHubChips: { ...chips, [key]: value },
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

  useEffect(() => {
    if (!modelId && selectedModel) setModelId(selectedModel.id);
  }, [modelId, selectedModel]);

  // Only the optimize scenario/goals are remembered; the task itself is one-shot
  // and always starts as plain Q&A.
  const optimizeRestoredRef = useRef(false);
  useEffect(() => {
    if (!ready || optimizeRestoredRef.current || !settings.aiHubLastTask) return;
    optimizeRestoredRef.current = true;
    const parsed = parseTask(settings.aiHubLastTask);
    if (parsed.task === "optimize") setOptimize(parsed.optimize);
  }, [ready, settings.aiHubLastTask]);

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

  const changeOptimize = (next: OptimizeConfig) => {
    setOptimize(next);
    setSettings({ aiHubLastTask: encodeTask("optimize", next) });
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

  const openList = useMemo(
    () => openSuggestions(input, quickAppLaunchers, installedApps),
    [input, quickAppLaunchers, installedApps],
  );
  useEffect(() => setOpenDismissed(false), [input]);
  const visibleOpenList = openDismissed ? null : openList;

  const runOpen = (item: OpenSuggestion) => {
    if (stream.busy) {
      flashBusyHint();
      return;
    }
    const question = input.trim();
    setHistoryOpen(false);
    void (async () => {
      try {
        const action = item.action;
        if (action.kind === "tab") {
          if (action.gitSub) writeStoredString(GIT_SUBTAB_KEY, action.gitSub);
          if (action.toolId) writeStoredString(DEVTOOLS_TOOL_KEY, action.toolId);
          await invoke("open_main_tab", { tab: action.tab });
        } else {
          await invokeLaunchApp(action.path, action.args || undefined);
        }
        stream.noteLocal(question, t("actionDone", { name: action.label }));
        await hide();
      } catch (e) {
        stream.setError(String(e));
      }
    })();
  };

  const sendAsk = async () => {
    // Screenshots leave the machine: ask once before the first turn that could take them.
    if (permission === "full" && selectedModel?.vision && localStorage.getItem(COMPUTER_ACK_KEY) !== "1") {
      const ok = await confirm({
        title: t("computer.confirmTitle"),
        message: t("computer.confirmBody"),
        warning: t("computer.confirmWarn"),
        confirmText: t("computer.confirmOk"),
        icon: "warning",
      });
      if (!ok) return;
      localStorage.setItem(COMPUTER_ACK_KEY, "1");
    }
    setHistoryOpen(false);
    void stream.send();
  };
  /** A screen tool is running this turn: keep the emergency stop visible. */
  const computerActive =
    stream.busy && stream.agentSteps.some((s) => COMPUTER_TOOL_NAMES.includes(s.tool));

  const askDaily = () => {
    if (stream.busy) return;
    setHistoryOpen(false);
    setInput("");
    void stream.send(t("examples.daily"));
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
      if (mode === "ask" && visibleOpenList) {
        setOpenDismissed(true);
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
        if (stream.busy) flashBusyHint();
        else startNewChat();
      }
      // Ctrl/Cmd+. : next permission level (never lands on 完全权限)
      if ((e.ctrlKey || e.metaKey) && e.key === ".") {
        e.preventDefault();
        const at = HOTKEY_CYCLE.indexOf(permission);
        void changePermission(HOTKEY_CYCLE[(at + 1) % HOTKEY_CYCLE.length]);
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [snippets.paramTarget, snippets.snippetOpen, visibleOpenList, mode, openPop, historyOpen, ctxOpen, task, input, stream.busy, permission]);

  const togglePop = (id: NonNullable<typeof openPop>) => {
    setOpenPop((cur) => (cur === id ? null : id));
  };

  // Switching modes unmounts ask chrome; drop popovers / history drawer.
  useEffect(() => {
    setOpenPop(null);
    setHistoryOpen(false);
    snippets.setSnippetOpen(false);
    snippets.setParamTarget(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode]);

  useResizingBodyClass();
  const windowBounds = useWindowBounds();
  useModeHotkeys(setMode);

  // Re-showing the window: drop popovers/history and park focus in the composer.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen("ai-hub-shown", () => {
      setOpenPop(null);
      setHistoryOpen(false);
      refreshInstalledApps();
      requestAnimationFrame(() => inputRef.current?.focus());
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, [refreshInstalledApps]);

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

  const fillClipboard = async () => {
    try {
      const clip = (await invoke<string>("read_clipboard")).trim();
      if (clip) setInput((prev) => (prev ? `${prev}\n${clip}` : clip));
    } catch (e) {
      stream.setError(String(e));
    }
  };

  /** Restore a persisted session into the current chat. */
  const openSession = (s: AiHubSession) => {
    if (stream.busy) {
      flashBusyHint();
      return;
    }
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
    return <div className="ai-hub-root loading">{t("loading")}</div>;
  }

  return (
    <div className="ai-hub-root">
      <AiHubHeader
        mode={mode}
        onMode={setMode}
        busy={stream.busy}
        agentApproval={stream.agentApproval}
        onDecideAgent={(decision) => void stream.decideAgent(decision)}
        onStop={() => void stream.stopStream()}
        historyOpen={historyOpen}
        onToggleHistory={() => setHistoryOpen((v) => !v)}
        onNewChat={startNewChat}
        pinned={pinned}
        onTogglePinned={togglePinned}
        maximized={windowBounds.maximized}
        onToggleMaximized={windowBounds.toggleMaximized}
        onClose={() => void hide()}
      />
      {computerActive && (
        <div className="qa-computer-banner" role="status">
          {t("computer.banner")}
        </div>
      )}

      {mode === "tools" ? (
        <ToolsPanel onJump={(toolId) => void jumpToDevtools(toolId)} />
      ) : (
        <>
          <AttachmentBar
            attachments={attachments}
            onRemove={(id) => setAttachments((prev) => prev.filter((x) => x.id !== id))}
          />

          {ctx.ctxTruncated && ctxOpen && <div className="ai-hub-hint">{t("truncated")}</div>}
          {ctxOpen && (
            <pre className="ai-hub-ctx-preview">{ctx.ctxPreview || t("ctx.previewEmpty")}</pre>
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
            suggestion={nextSuggestion}
            onSuggestionPick={(text) => {
              setComposerInput(text);
              requestAnimationFrame(() => inputRef.current?.focus());
            }}
            onSuggestionDismiss={dismissSuggestion}
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
            copied={answers.copied}
            snippetSaved={answers.snippetSaved}
            exportDone={answers.exportDone}
            selectedText={selectedText}
            setSelectedText={setSelectedText}
            projectLabel={repo.projectLabel}
            task={task}
            onTask={changeTask}
            onDaily={askDaily}
            agentSteps={stream.agentSteps}
            agentApproval={stream.agentApproval}
            stopping={stream.stopping}
            onDecideAgent={(decision) => void stream.decideAgent(decision)}
            onContinue={() => void stream.send(t("agent.continuePrompt"))}
            hasSnippets={(snippets.snippets?.length ?? 0) > 0}
            snippetOpen={snippets.snippetOpen}
            onToggleSnippets={() => snippets.openManual()}
            onPasteClipboard={() => void fillClipboard()}
            onAttachFile={() => void addAttachmentFromFile()}
            onSend={() => void sendAsk()}
            repoControl={
              <ContextPicker
                open={openPop === "focus"}
                onToggleOpen={() => togglePop("focus")}
                groups={repo.repoGroups}
                selection={repo.focusedRepos}
                label={repo.pickerLabel}
                currentRepo={settings.currentGitRepo || null}
                onToggleGroup={repo.onToggleGroup}
                onToggleService={repo.onToggleService}
                onClear={repo.clearFocus}
                onScanFolder={() => void repo.scanFolder()}
                scanning={repo.scanning}
                chips={chips}
                setChip={setChip}
                onPreview={() => void previewContext()}
                previewLoading={ctx.ctxLoading}
                included={contextIncluded}
                pinned={permission === "read" && routeOverride !== "auto"}
                locked={permission !== "read"}
                includeTip={includeTip}
                onToggleInclude={toggleInclude}
              />
            }
            modelControl={
              <QuickSelect
                className="qa-model-pick"
                value={selectedModel?.id || ""}
                options={models.map((m) => ({
                  value: m.id,
                  label: `${m.name} (${m.model})`,
                  short: m.name,
                }))}
                onChange={setModelId}
                placeholder={t("noModel")}
                alignLeft
                dropUp
                ghost
                open={openPop === "model"}
                onToggle={() => togglePop("model")}
              />
            }
            permission={permission}
            onPermission={(next) => void changePermission(next)}
            permissionMenuOpen={openPop === "permission"}
            onTogglePermissionMenu={() => togglePop("permission")}
            dailyStyle={dailyStyle}
            onDailyStyle={setDailyStyle}
            dailyMenuOpen={openPop === "daily"}
            onToggleDailyMenu={() => togglePop("daily")}
            openList={visibleOpenList}
            onRunOpen={runOpen}
            onStop={() => void stream.stopStream()}
            onRegenerate={() => void stream.regenerate()}
            onCopyAnswer={(i) => void answers.copyAnswer(i)}
            onSaveSnippet={(i) => void answers.saveAnswerAsSnippet(i)}
            onExport={() => void answers.handleExport()}
            onQuote={answers.quoteSelection}
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
                  sessions={sessions.aiHubSessions}
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
      {hintOn && (
        <div className="qa-busy-hint" role="status">
          {hintText}
        </div>
      )}
    </div>
  );
}

export default AiHubApp;
