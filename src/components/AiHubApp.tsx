import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useGlobalStore, MAX_AI_HUB_SESSIONS } from "../core/store";
import type { AIModelConfig, AiHubSession, AiHubTurn } from "../core/types";
import { bootApp } from "../core/boot";
import { writeStoredString } from "../core/localState";
import { applyDocumentTheme } from "../lib/theme";
import { resolveFallbackModel, resolveSceneModel } from "../lib/aiSceneModels";
import { DEVTOOLS_TOOL_KEY } from "./DevTools";
import {
  ASK_ATTACH_MAX_FILES,
  attachmentNameFromPath,
  canAddAttachment,
  isTextAttachmentPath,
  makeAttachment,
  type AskAttachment,
} from "../lib/askAttachments";
import AskPanel from "./aiHub/AskPanel";
import AiHubHeader, { type AiHubMode } from "./aiHub/AiHubHeader";
import AttachmentBar from "./aiHub/AttachmentBar";
import ContextPicker from "./aiHub/ContextPicker";
import OptimizeBar from "./aiHub/OptimizeBar";
import ParamModal from "./aiHub/ParamModal";
import ProfileModal from "./aiHub/ProfileModal";
import ProfilePicker from "./aiHub/ProfilePicker";
import QuickSelect from "./aiHub/QuickSelect";
import SessionHistory from "./aiHub/SessionHistory";
import SnippetPicker from "./aiHub/SnippetPicker";
import ToolsPanel from "./aiHub/ToolsPanel";
import { useNextSuggestion } from "./aiHub/useNextSuggestion";
import { DEFAULT_CHIPS, DEFAULT_OPTIMIZE, encodeTask, parseTask } from "./aiHub/config";
import type { OptimizeConfig, TaskKind } from "./aiHub/config";
import { routeQuestion } from "./aiHub/route";
import { findProfile, mergeProfiles } from "../lib/aiHubProfiles";
import { openSuggestions, type InstalledApp, type OpenSuggestion } from "./aiHub/localActions";
import { GIT_SUBTAB_KEY } from "./GitManager";
import type { RouteOverride } from "./aiHub/route";
import { useAskContext } from "./aiHub/useAskContext";
import { useAskEvents } from "./aiHub/useAskEvents";
import { useAskSessions } from "./aiHub/useAskSessions";
import { useAskSnippets } from "./aiHub/useAskSnippets";
import { useHarnessComposer, type HarnessCommand, type HarnessSkillItem } from "./aiHub/useHarnessComposer";
import ModePicker from "./aiHub/ModePicker";
import { HARNESS_MODES, modeRoute, type HarnessMode } from "./aiHub/harness";
import { AGENT_PERMISSIONS, useAskStream, type AgentPermission } from "./aiHub/useAskStream";
import { useConfirm, useConfirmChoice } from "./ConfirmModal";
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
/** 任务模式（对话 / 代码 / 文档 / 数据 / 调研 / 运维），与界面 tab 的 `mode` 无关。 */
const TASK_MODE_KEY = "qa-task-mode";

function readTaskMode(): HarnessMode {
  const raw = localStorage.getItem(TASK_MODE_KEY) as HarnessMode | null;
  return raw && HARNESS_MODES.includes(raw) ? raw : "code";
}
/** Set once the user stopped wanting to be asked before hiding a running round. */
/**
 * 一个仓库最多记住几个分支的「以后都允许」：来回切过的旧分支不该一直堆在设置里。
 * 只按写入顺序回收（对象键序），不额外存时间戳。
 */
const MAX_REMEMBERED_BRANCHES = 5;

/**
 * 分支层只留最近用过的 N 个分支，当前分支排在最后（＝最近一次）。
 * 键序没变时返回原对象，避免无谓的重渲染。
 */
function pruneRememberedBranches(
  perRepo: Record<string, string[]>,
  current: string,
): Record<string, string[]> {
  const names = Object.keys(perRepo);
  const kept = names.filter((name) => name !== current).slice(-(MAX_REMEMBERED_BRANCHES - 1));
  const order = [...kept, current];
  if (order.length === names.length && order.every((name, i) => names[i] === name)) return perRepo;
  const next: Record<string, string[]> = {};
  for (const name of kept) next[name] = perRepo[name];
  next[current] = perRepo[current];
  return next;
}

const CLOSE_ACK_KEY = "qa-close-ack";
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
  const mcpServers = useGlobalStore((s) => s.mcpServers);
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
  const invokePickSourceFile = useGlobalStore((s) => s.invokePickSourceFile);
  const invokeReadTextFile = useGlobalStore((s) => s.invokeReadTextFile);
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
  /** Drop events arrive from the window level, where `mode` is not in scope. */
  const modeRef = useRef<AiHubMode>("ask");
  modeRef.current = mode;
  const [input, setInput] = useState("");
  const [task, setTask] = useState<TaskKind>("none");
  const [optimize, setOptimize] = useState<OptimizeConfig>(DEFAULT_OPTIMIZE);
  const [modelId, setModelId] = useState<string>("");
  const [attachments, setAttachments] = useState<AskAttachment[]>([]);
  const [taskMode, setTaskMode] = useState<HarnessMode>(readTaskMode);
  useEffect(() => {
    localStorage.setItem(TASK_MODE_KEY, taskMode);
  }, [taskMode]);
  /** `@` 引用进来的文件；追问沿用同一批，换文件重新 @ 即可。 */
  const [refs, setRefs] = useState<string[]>([]);
  /** 项目 + 全局技能（只取名字与说明做补全；正文由后端按 `/skill:name` 注入）。 */
  const [harnessSkills, setHarnessSkills] = useState<HarnessSkillItem[]>([]);
  /** Files are being dragged over the window (ask tab only). */
  const [dropHint, setDropHint] = useState(false);
  const chips = settings.aiHubChips ?? DEFAULT_CHIPS;
  /** Which popover is open — only one at a time (mutual exclusion). */
  const [openPop, setOpenPop] = useState<
    null | "model" | "focus" | "permission" | "daily" | "profile" | "mode"
  >(null);
  /** Profile editor (custom personas + temperature). */
  const [profileModalOpen, setProfileModalOpen] = useState(false);
  const profiles = useMemo(
    () => mergeProfiles(settings.aiHubProfiles),
    [settings.aiHubProfiles],
  );
  const activeProfile = useMemo(
    () => findProfile(settings.aiHubProfileId, settings.aiHubProfiles),
    [settings.aiHubProfileId, settings.aiHubProfiles],
  );
  /** Composer pill override for the next send only; "auto" = routeQuestion decides. */
  const [routeOverride, setRouteOverride] = useState<RouteOverride>("auto");
  /** Write-capable levels always work on the project; read-only follows the auto route. */
  // 非「代码 / 对话」模式必须带项目上下文（文档、表格、调研都要读工作区），
  // 不再交给正则猜是不是代码问题。
  const effectiveOverride: RouteOverride =
    permission === "read" ? (taskMode === "code" ? routeOverride : modeRoute(taskMode)) : "project";
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
  const confirmChoice = useConfirmChoice();
  /** Esc dismissed the open-suggestion list; typing brings it back. */
  const [openDismissed, setOpenDismissed] = useState(false);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // `stream` is declared below; the error callback only runs later, from a click.
  const repo = useRepoFocus({
    single: permission !== "read",
    onError: (msg) => stream.setError(msg),
    onNotice: showHint,
  });

  const ctx = useAskContext(chips, repo.resolvedFocus);

  const models = aiModels as AIModelConfig[];
  const selectedModel = useMemo(() => {
    const scene = task === "optimize" ? "optimize" : "aiHub";
    return resolveSceneModel(scene, models, settings.aiSceneModels, modelId || null) ?? undefined;
  }, [models, modelId, task, settings.aiSceneModels]);
  // 主模型整轮失败（连不上 / 4xx / 5xx / 空回答）时的备胎：同协议的另一个可用模型
  const fallbackModel = useMemo(
    () => resolveFallbackModel(selectedModel, models),
    [models, selectedModel]
  );

  // 审批记忆 / diff 确认按仓库覆盖：选了仓库就优先用该仓库的值，否则用全局。
  const agentRoot = repo.agentRoot?.trim() || "";
  // 审批记忆再细到分支：选了仓库就问一次当前分支（非 git 目录 / detached HEAD 当没有分支）
  const invokeGitCurrentBranch = useGlobalStore((s) => s.invokeGitCurrentBranch);
  const [branchByRepo, setBranchByRepo] = useState<Record<string, string>>({});
  /** 上一次真正问分支的时间：焦点来回切会连着触发，1.5 秒内只问一次。 */
  const branchRefreshAt = useRef(0);
  const refreshAgentBranch = useCallback(
    (force = false) => {
      if (!agentRoot) return;
      const now = Date.now();
      // 轮次结束那条要 force：下一问必须带上最新分支，不能被节流吞掉。
      if (!force && now - branchRefreshAt.current < 1500) return;
      branchRefreshAt.current = now;
      void invokeGitCurrentBranch(agentRoot)
        .then((branch) => {
          const name = branch && branch !== "HEAD" ? branch : "";
          setBranchByRepo((prev) => (prev[agentRoot] === name ? prev : { ...prev, [agentRoot]: name }));
        })
        // 非 git 目录 / detached HEAD：清掉这个仓库的分支名，别沿用切走前的那个。
        .catch(() => setBranchByRepo((prev) => (prev[agentRoot] === "" ? prev : { ...prev, [agentRoot]: "" })));
    },
    [agentRoot, invokeGitCurrentBranch],
  );
  useEffect(() => {
    refreshAgentBranch();
    // 切分支一般发生在这个窗口外面（Git 页 / 终端 / 代理自己 checkout）：
    // 窗口重新拿到焦点时重问一次，别让分支级记忆沿用上一个分支。
    const onFocus = () => refreshAgentBranch();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshAgentBranch]);
  const agentBranch = agentRoot ? branchByRepo[agentRoot] || "" : "";
  // 同一类可能同时出现在全局与分支层，去重后再下发（后端只做 contains，重复没有意义）。
  const agentAlwaysAllow = [
    ...new Set([
      ...(settings.agentAlwaysAllow || []),
      ...(agentRoot ? settings.agentAlwaysAllowByRepo?.[agentRoot] || [] : []),
      ...(agentRoot && agentBranch ? settings.agentAlwaysAllowByBranch?.[agentRoot]?.[agentBranch] || [] : []),
    ]),
  ];
  const agentDiffConfirm = agentRoot
    ? (settings.agentDiffConfirmByRepo?.[agentRoot] ?? settings.agentDiffConfirm === true)
    : settings.agentDiffConfirm === true;

  // stream <-> sessions reference each other's callbacks; bridge via ref.
  const persistRef = useRef<(turns: QaTurn[], firstQuestion: string) => Promise<void>>(
    async () => {},
  );
  const stream = useAskStream({
    selectedModel,
    fallbackModel,
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
    onInvalidateContext: ctx.invalidateContext,
    // 用户点了「以后都允许」：把这一类 action_kind 记进设置，下一次提问直接带上。
    // 记在最细的已知范围：有分支 → 仓库+分支；只有仓库 → 仓库；都没有 → 全局。
    onRememberKind: (kind, repoPath) => {
      if (!kind) return;
      const key = repoPath?.trim();
      const branch = key ? branchByRepo[key] || "" : "";
      if (key && branch) {
        const byBranch = settings.agentAlwaysAllowByBranch || {};
        const perRepo = byBranch[key] || {};
        const list = perRepo[branch] || [];
        if (list.includes(kind)) return;
        setSettings({
          agentAlwaysAllowByBranch: {
            ...byBranch,
            [key]: pruneRememberedBranches({ ...perRepo, [branch]: [...list, kind].slice(-20) }, branch),
          },
        });
      } else if (key) {
        const byRepo = settings.agentAlwaysAllowByRepo || {};
        const list = byRepo[key] || [];
        if (list.includes(kind)) return;
        setSettings({ agentAlwaysAllowByRepo: { ...byRepo, [key]: [...list, kind].slice(-20) } });
      } else {
        const list = settings.agentAlwaysAllow || [];
        if (list.includes(kind)) return;
        setSettings({ agentAlwaysAllow: [...list, kind].slice(-20) });
      }
    },
    persistTurns: (turns, q) => {
      void persistRef.current(turns, q);
    },
    attachments,
    profileSystemRole: activeProfile?.systemRole,
    profileTemperature: activeProfile?.temperature,
    agent: {
      permission,
      vision: Boolean(selectedModel?.vision),
      diffConfirm: agentDiffConfirm,
      alwaysAllow: agentAlwaysAllow,
      webAllow: settings.agentWebAllow || [],
      subagents: settings.agentSubagents === true,
      parallelReads: settings.agentParallelReads !== false,
      retries: settings.agentRetries !== false,
      grepIgnore: settings.agentGrepIgnore !== false,
      planFirst: settings.agentPlanFirst === true,
      contextTokens: settings.agentContextTokens,
      resultCap: settings.agentResultCap,
      anthropicStream: settings.agentAnthropicStream !== false,
      mcpEnabled: settings.agentMcpEnabled !== false,
      repoPath: repo.agentRoot,
      services: repo.agentServices,
      apps: quickAppLaunchers.map((app) => ({
        name: app.name,
        path: app.path,
        args: app.args || "",
      })),
      // 只送启用的、填全了的：后端每轮会去列一次工具，烂配置只会浪费一轮的等待
      mcp: mcpServers
        .filter((s) => s.enabled && s.name.trim() && s.command.trim())
        .map((s) => ({ name: s.name.trim(), command: s.command.trim(), args: s.args || "" })),
      mode: taskMode,
      refs,
    },
  });
  const invokeHarnessConfig = useGlobalStore((s) => s.invokeHarnessConfig);
  useEffect(() => {
    const root = repo.agentRoot;
    if (!root) {
      setHarnessSkills([]);
      return;
    }
    void invokeHarnessConfig(root)
      .then((config) => setHarnessSkills(config.skills))
      .catch(() => setHarnessSkills([]));
  }, [repo.agentRoot, invokeHarnessConfig]);
  // 代理自己 `git checkout` 完，这一轮就结束了而窗口焦点没变；跑完再问一次分支，
  // 免得下一问还带着旧分支的「以后都允许」。
  const agentWasBusy = useRef(false);
  useEffect(() => {
    if (agentWasBusy.current && !stream.busy) refreshAgentBranch(true);
    agentWasBusy.current = stream.busy;
  }, [stream.busy, refreshAgentBranch]);
  const sessions = useAskSessions({
    task: encodeTask(task, optimize),
    activeSessionId,
    setActiveSessionId,
    setError: stream.setError,
    onNotice: showHint,
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

  const snippets = useAskSnippets({ setInput, inputRef });
  const setComposerInput = (updater: string | ((prev: string) => string)) => {
    setInput((prev) => {
      const next = typeof updater === "function" ? updater(prev) : updater;
      // Defer so slash sync sees the committed value after this setState.
      queueMicrotask(() => snippets.syncSlashFromInput(next));
      return next;
    });
  };
  /** 内置命令：只放真能执行的动作，别让面板里出现点了没反应的条目。 */
  const harnessCommands = useMemo<HarnessCommand[]>(
    () => [
      { id: "clear", label: t("palette.clear"), hint: t("palette.clearHint"), run: () => stream.resetChat() },
      {
        id: "plan",
        label: t("palette.plan"),
        hint: t("palette.planHint"),
        run: () => setSettings({ agentPlanFirst: settings.agentPlanFirst !== true }),
      },
    ],
    [t, stream, settings.agentPlanFirst],
  );
  const palette = useHarnessComposer({
    input,
    setInput: setComposerInput,
    root: repo.agentRoot,
    skills: harnessSkills,
    commands: harnessCommands,
    snippets: snippets.snippetChoices,
    slashOpen: snippets.snippetOpen && snippets.slashMode,
    slashQueryText: snippets.snippetQuery,
    onPickSnippet: snippets.pickSnippet,
    onRef: (path) => setRefs((prev) => (prev.includes(path) ? prev : [...prev, path])),
  });

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
    enabled: settings.aiHubSuggest !== false,
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
        lastMode: stream.lastModeRef.current,
        override: effectiveOverride,
      }),
    // lastModeRef changes only on send, which also re-renders via `answer`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [deferredInput, effectiveOverride, stream.answer, stream.history.length],
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

  const profileLabel = (id: string) => {
    const p = profiles.find((x) => x.id === id);
    return p ? t(p.nameKey || "", { defaultValue: p.name }) : t("profile.none");
  };

  /** Profile for the next message; the running round keeps the one it started with. */
  const changeProfile = (id: string) => {
    setSettings({ aiHubProfileId: id || undefined });
    if (stream.busy) showHint(t("profile.nextAs", { name: profileLabel(id) }));
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
    requestAnimationFrame(() => inputRef.current?.focus());
  };

  /** Fold picked/dropped files into the attachment list, saying what did not fit. */
  const attachFiles = (items: { name: string; text: string }[]) => {
    const next = [...attachments];
    let cut = 0;
    let overBudget: "count" | "size" | null = null;
    const rejected: string[] = [];
    for (const item of items) {
      const probe = makeAttachment(item.name, item.text);
      const check = canAddAttachment(next, { name: item.name, content: item.text });
      if (!check.ok) {
        overBudget = check.reason;
        rejected.push(item.name);
        continue;
      }
      if (probe.truncated) cut += 1;
      next.push(probe);
    }
    setAttachments(next);
    if (cut > 0) showHint(t("attach.truncated"));
    if (overBudget) {
      stream.setError(
        `${t(overBudget === "count" ? "attach.tooMany" : "attach.tooLarge")}（${rejected.join(", ")}）`,
      );
    }
  };

  /** Real filename, no silent truncation: pick → read by path instead of the content-only picker. */
  const addAttachmentFromFile = async () => {
    try {
      const path = await invokePickSourceFile(t("attach.pickTitle"));
      const text = await invokeReadTextFile(path);
      if (!text.trim()) {
        showHint(t("attach.empty"));
        return;
      }
      attachFiles([{ name: attachmentNameFromPath(path), text }]);
    } catch (e) {
      const msg = String(e);
      if (msg.includes("已取消") || msg.toLowerCase().includes("cancel")) return;
      stream.setError(msg);
    }
  };

  const attachDroppedFiles = async (paths: string[]) => {
    const picked: { name: string; text: string }[] = [];
    const unreadable: string[] = [];
    for (const path of paths.slice(0, ASK_ATTACH_MAX_FILES)) {
      const name = attachmentNameFromPath(path);
      if (!isTextAttachmentPath(path)) {
        unreadable.push(name);
        continue;
      }
      try {
        const text = await invokeReadTextFile(path);
        if (text.trim()) picked.push({ name, text });
      } catch {
        unreadable.push(name);
      }
    }
    if (picked.length) attachFiles(picked);
    if (unreadable.length) {
      stream.setError(t("attach.notText", { name: unreadable.join(", ") }));
    }
  };

  // Native drag-drop only reports paths through Tauri, so the listener lives at the window level.
  const attachDroppedRef = useRef<(paths: string[]) => void>(() => {});
  attachDroppedRef.current = attachDroppedFiles;
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      try {
        const { getCurrentWebview } = await import("@tauri-apps/api/webview");
        const off = await getCurrentWebview().onDragDropEvent((event) => {
          const payload = event.payload;
          if (payload.type === "enter" || payload.type === "over") {
            if (modeRef.current === "ask") setDropHint(true);
          } else if (payload.type === "leave") {
            setDropHint(false);
          } else {
            setDropHint(false);
            if (modeRef.current === "ask") attachDroppedRef.current(payload.paths);
          }
        });
        if (cancelled) off();
        else unlisten = off;
      } catch {
        /* plain-browser dev: no webview, so drops are not reported */
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

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

  /**
   * Hide the window. A round still running — or worse, still waiting on the user —
   * has no taskbar button to come back to, so point at the tray / orb / Ctrl+Alt+K.
   */
  const closeWindow = async () => {
    const pendingApproval = Boolean(stream.agentApproval);
    if ((stream.busy || pendingApproval) && localStorage.getItem(CLOSE_ACK_KEY) !== "1") {
      const choice = await confirmChoice({
        title: t(pendingApproval ? "hideApprovalTitle" : "hideBusyTitle"),
        message: t(pendingApproval ? "hideApprovalBody" : "hideBusyBody"),
        warning: t("hideWarn"),
        confirmText: t("hideNow"),
        altConfirmText: t("hideAlways"),
        cancelText: t("hideStay"),
        icon: "warning",
      });
      if (choice === "cancel") return;
      if (choice === "alt") localStorage.setItem(CLOSE_ACK_KEY, "1");
    }
    void hide();
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
      // Layered escape: dismiss the topmost layer first, and only stop the round
      // once nothing is in the way (Esc used to kill a running turn while a popover
      // was open, because 停流 sat at the head of the chain).
      if (snippets.paramTarget) {
        snippets.setParamTarget(null);
        return;
      }
      if (profileModalOpen) {
        setProfileModalOpen(false);
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
      if (stream.busy) {
        void stream.stopStream();
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
      void closeWindow();
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
  }, [snippets.paramTarget, profileModalOpen, snippets.snippetOpen, visibleOpenList, mode, openPop, historyOpen, ctxOpen, task, input, stream.busy, stream.agentApproval, permission]);

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

  /** Deleting a conversation is the only irreversible thing this window does. */
  const removeSessionConfirmed = async (s: AiHubSession) => {
    if (stream.busy && s.id === activeSessionId) return;
    const ok = await confirm({
      title: t("deleteSession"),
      message: t("deleteSessionBody", {
        title: s.title || t("newChat"),
        n: Math.ceil((s.turns?.length ?? 0) / 2),
      }),
      warning: t("deleteSessionWarn"),
      confirmText: t("deleteConfirm"),
      icon: "danger",
    });
    if (ok) void sessions.removeSession(s.id);
  };

  /** 批量删除：一条确认框报总数与总轮数；正在出答案的那一条先跳过（它会被收尾逻辑写回去）。 */
  const removeSessionsConfirmed = async (list: AiHubSession[]) => {
    const skipped = list.filter((s) => stream.busy && s.id === activeSessionId);
    const targets = list.filter((s) => !(stream.busy && s.id === activeSessionId));
    if (targets.length === 0) return;
    const turns = targets.reduce((n, s) => n + Math.ceil((s.turns?.length ?? 0) / 2), 0);
    const ok = await confirm({
      title: t("deleteManyTitle"),
      message: t("deleteManyBody", { n: targets.length, turns }),
      warning: t("deleteSessionWarn"),
      confirmText: t("deleteManyConfirm", { n: targets.length }),
      icon: "danger",
    });
    if (!ok) return;
    await sessions.removeSessions(targets.map((s) => s.id));
    showHint(
      skipped.length
        ? t("deleteManySkipped", { n: targets.length })
        : t("deleteManyDone", { n: targets.length })
    );
  };

  const editQuestion = (index: number) => {
    const text = stream.history[index]?.content || "";
    if (!text) return;
    setComposerInput(text);
    requestAnimationFrame(() => {
      const el = inputRef.current;
      if (el) el.setSelectionRange(el.value.length, el.value.length);
      el?.focus();
    });
  };

  /** Rewind the transcript to before this question, then put it back in the composer. */
  const restartFrom = (index: number) => {
    if (stream.busy) return;
    const text = stream.history[index]?.content || "";
    stream.truncateTo(index);
    setComposerInput(text);
    requestAnimationFrame(() => inputRef.current?.focus());
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
        onClose={() => void closeWindow()}
      />
      {computerActive && (
        <div className="qa-computer-banner" role="status">
          {t("computer.banner")}
        </div>
      )}

      {mode === "tools" ? (
        <ToolsPanel />
      ) : (
        <>
          <AttachmentBar
            attachments={attachments}
            onRemove={(id) => setAttachments((prev) => prev.filter((x) => x.id !== id))}
          />

          {dropHint && (
            <div className="qa-drop-hint" role="status">
              {t("attach.dropHint")}
            </div>
          )}

          {ctx.ctxTruncated && ctxOpen && <div className="ai-hub-hint">{t("truncated")}</div>}
          {ctxOpen && (
            <pre className="ai-hub-ctx-preview">{ctx.ctxPreview || t("ctx.previewEmpty")}</pre>
          )}

          {/* 命令面板接管 `/` 的候选：有命令条目时不再叠一层片段浮层。
              手动打开（工具栏按钮）时 palette 为空，片段列表照常出现。 */}
          {snippets.snippetOpen && !palette.items && (
            <SnippetPicker
              snippetQuery={snippets.snippetQuery}
              setSnippetQuery={snippets.setSnippetQuery}
              snippetChoices={snippets.snippetChoices}
              onPick={snippets.pickSnippet}
              slashMode={snippets.slashMode}
              active={snippets.active}
              onMoveActive={snippets.moveActive}
              setActiveIndex={snippets.setActiveIndex}
              onPickActive={snippets.pickActive}
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
            onExample={(text) => {
              setComposerInput(text);
              requestAnimationFrame(() => inputRef.current?.focus());
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
            agentPlan={stream.agentPlan}
            agentApproval={stream.agentApproval}
            stopping={stream.stopping}
            onDecideAgent={(decision) => void stream.decideAgent(decision)}
            onContinue={() => void stream.send(t("agent.continuePrompt"))}
            hasSnippets={(snippets.snippets?.length ?? 0) > 0}
            snippetOpen={snippets.snippetOpen}
            snippetNav={
              snippets.snippetOpen
                ? {
                    active: snippets.snippetChoices.length > 0,
                    onMove: snippets.moveActive,
                    onPick: snippets.pickActive,
                  }
                : null
            }
            onToggleSnippets={() => snippets.openManual()}
            onPasteClipboard={() => void fillClipboard()}
            onAttachFile={() => void addAttachmentFromFile()}
            onSend={() => void sendAsk()}
            palette={{
              items: palette.items,
              active: palette.active,
              onMove: palette.move,
              onHover: palette.setActive,
              onPick: palette.pick,
              onPickActive: palette.pickActive,
            }}
            modeControl={
              <ModePicker
                mode={taskMode}
                onMode={setTaskMode}
                open={openPop === "mode"}
                onToggle={() => togglePop("mode")}
              />
            }
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
                removableKeys={repo.removableKeys}
                onRemoveGroup={repo.removeExtra}
                single={permission !== "read"}
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
            profileControl={
              <ProfilePicker
                profiles={profiles}
                value={settings.aiHubProfileId || ""}
                onChange={changeProfile}
                onManage={() => {
                  setOpenPop(null);
                  setProfileModalOpen(true);
                }}
                open={openPop === "profile"}
                onToggle={() => togglePop("profile")}
              />
            }
            permission={permission}
            onPermission={(next) => void changePermission(next)}
            diffConfirm={agentDiffConfirm}
            onDiffConfirm={(on) => {
              if (agentRoot) {
                const byRepo = settings.agentDiffConfirmByRepo || {};
                setSettings({ agentDiffConfirmByRepo: { ...byRepo, [agentRoot]: on } });
              } else {
                setSettings({ agentDiffConfirm: on });
              }
            }}
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
            onEditQuestion={editQuestion}
            onRestartFrom={restartFrom}
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
                  onRemove={(s) => void removeSessionConfirmed(s)}
                  onRemoveMany={(list) => void removeSessionsConfirmed(list)}
                  onExportAll={() => void sessions.exportAllSessions()}
                  maxSessions={MAX_AI_HUB_SESSIONS}
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

          {profileModalOpen && (
            <ProfileModal
              custom={settings.aiHubProfiles || []}
              onChange={(next) => setSettings({ aiHubProfiles: next })}
              onClose={() => setProfileModalOpen(false)}
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
