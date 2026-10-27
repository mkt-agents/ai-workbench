import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AIModelConfig, AiHubPlanStep, AiHubTurn } from "../../core/types";
import { recordAiUsage } from "../../lib/aiUsage";
import {
  formatAttachmentsBlock,
  imageAttachmentsOf,
  type AskAttachment,
} from "../../lib/askAttachments";
import { buildMeta, buildSystem, stepsMutatedWorkspace } from "./config";
import { compactSession, requestHistory, withSessionDigest } from "./sessionCompact";
import { buildUserMessage, stepsToBlocks, turnsToMessages } from "./harness";
import { cleanDailyReport, dailyReportSystem, isDailyReport, isDailyRevision, type DailyStyle } from "./dailyReport";
import { routeQuestion } from "./route";
import type { AskMode, RouteOverride } from "./route";
import { stripThinking } from "./thinking";
import type { ContextResult, ContextSummary } from "./useAskContext";

/** What the latest answer was based on — rendered as the answer source line. */
export type AnswerSource = {
  mode: AskMode;
  summary: ContextSummary;
  modelName: string;
  /** Wall-clock ms for the completed stream (when known). */
  elapsedMs?: number;
  promptTokens?: number;
  completionTokens?: number;
  /** Fallback estimate (~chars/4) when the provider omitted usage. */
  estimatedTokens?: number;
  /** The workbench context block was clipped to its budget. */
  contextTruncated?: boolean;
  /** Oldest rounds left out of the request to stay inside the char budget. */
  droppedTurns?: number;
  /** Characters this round actually carried (system + conversation). */
  contextChars?: number;
  /** Estimated share of the backend context budget (0..1). */
  budgetRatio?: number;
  /** Rounds in which the backend had to compact something. */
  trimmedRounds?: number;
  /** Input tokens served from the provider's prompt cache. */
  cacheHitTokens?: number;
};

type QaTurn = AiHubTurn;

export type AskPhase = "idle" | "context" | "waiting" | "streaming";

export type AgentStep = {
  tool: string;
  status: "running" | "done" | "denied" | "error" | string;
  summary: string;
  detail: string;
  /** Call argument from the backend, so the row can render in the UI language. */
  arg?: string;
};

/** The model's checklist for the round in flight (`update_plan`). */
export type AgentPlanStep = AiHubPlanStep;

export type AgentApprovalPatch = {
  callId: string;
  tool: string;
  summary: string;
  preview: string;
  /** `diff`: preview is a unified diff of a file / hosts change. */
  previewKind: "diff" | "text";
  /** `agent_policy::action_kind`, e.g. `cmd:npm`. */
  actionKind: string;
};

export type AgentApproval = {
  callId: string;
  tool: string;
  summary: string;
  preview: string;
  /** `diff`: preview is a unified diff of a file / hosts change. */
  previewKind: "diff" | "text";
  /** Epoch ms when the backend auto-denies this call; 0 = unknown. */
  deadlineAt: number;
  /** `agent_policy::action_kind` of the call, e.g. `cmd:npm` — what「以后都允许」stores. */
  actionKind: string;
  /** The backend offers cross-turn memory (Ask gate, not 完全权限). */
  canRemember: boolean;
  /** Merged approval: one card for several changes; present only when ≥ 2 patches. */
  patches?: AgentApprovalPatch[];
  /** All call ids answered together (merged card). */
  callIds?: string[];
  /** `approval` (default) | `question` (`ask_user`) | `plan` (`exit_plan_mode`). */
  kind?: "approval" | "question" | "plan";
  options?: string[];
  allowFreeText?: boolean;
  multi?: boolean;
};

/**
 * Approval answer: allow just this call, the same kind for the rest of the turn,
 * or the same kind from now on (remembered in settings).
 */
export type AgentDecision = "once" | "turn" | "always" | "deny";

/** Step output kept with a saved turn (the live one is capped by the backend too). */
const STEP_DETAIL_KEEP = 2000;

/**
 * Hub-local pref (same convention as `qa-pinned` / `qa-permission`): raise the
 * window when a call is waiting for the user instead of letting it time out.
 */
export const APPROVAL_FOCUS_KEY = "qa-approval-focus";

/** Ordered from least to most open; mirrors Rust `agent_policy::Permission`. */
export type AgentPermission = "read" | "workspace" | "review" | "full";
export const AGENT_PERMISSIONS: AgentPermission[] = ["read", "workspace", "review", "full"];

export type AgentRunOptions = {
  permission: AgentPermission;
  /** Agent working directory: one repo, or a workspace folder when `services` is set. */
  repoPath: string;
  /** Service repos picked inside a workspace folder `repoPath`. */
  services: string[];
  apps: { name: string; path: string; args: string }[];
  /** The selected model accepts images (unlocks operating the computer at 完全权限). */
  vision: boolean;
  /** 编辑档的写入也先看 diff（设置项）。 */
  diffConfirm: boolean;
  /** 跨轮审批记忆（设置项）：`action_kind` 列表。 */
  alwaysAllow: string[];
  /** `web_fetch` 的域名白名单（设置项）；空 = 不下发这个工具。 */
  webAllow: string[];
  /** 联网策略：off / allowlist / open。 */
  webTier: string;
  /** 搜索服务商：brave / tavily；空 = 不下发 web_search。 */
  webSearchProvider: string;
  /** 搜索 API key。 */
  webSearchKey: string;
  /** 允许派只读子代理（设置项，默认关）。 */
  subagents: boolean;
  /** 子代理可写（write_file / edit_file / run_command；设置项，默认关；仅编辑档及以上生效）。 */
  subagentWrite: boolean;
  /** 只读调用并发（设置项，默认开）。 */
  parallelReads: boolean;
  /** 429 / 5xx 自动重试（设置项，默认开）。 */
  retries: boolean;
  /** 搜索时仍屏蔽 .gitignore（设置项，默认开）。 */
  grepIgnore: boolean;
  /** 长任务先 update_plan（设置项，默认关）。 */
  planFirst: boolean;
  /** 每轮上下文预算（token，设置项；缺省由后端用 48000）。 */
  contextTokens?: number;
  /** 单条工具结果回灌上限（字符，设置项；缺省由后端用 12000）。 */
  resultCap?: number;
  /** Anthropic 走流式（设置项，默认开）。 */
  anthropicStream: boolean;
  /** MCP 总开关（设置项，默认开）。 */
  mcpEnabled: boolean;
  /** 任务模式（对话 / 代码 / 文档 / 数据 / 调研 / 运维）：后端按它收窄工具子集。 */
  mode?: string;
  /** 启用中的 MCP 服务；后端每轮列一次工具，名字是 `mcp_<server>_<tool>`。 */
  mcp: { name: string; command: string; args: string }[];
};

/** Full transcript (displayed + persisted). Past the verbatim window, older rounds fold into a digest. */
function appendTurn(
  prev: QaTurn[],
  question: string,
  answer: string,
  keepWhole = false,
  steps?: AgentStep[],
  limited = false,
  usage?: {
    elapsedMs?: number;
    promptTokens?: number;
    completionTokens?: number;
    contextChars?: number;
    budgetRatio?: number;
    trimmedRounds?: number;
    cacheHitTokens?: number;
    undoToken?: string;
    /** 回答这一轮的模型（同一会话逐轮可切换；fallback 时为备用模型）。 */
    modelId?: string;
    modelName?: string;
  },
  plan?: AgentPlanStep[],
): QaTurn[] {
  const kept = steps?.filter((s) => s.summary).map((s) => ({
    tool: s.tool,
    status: s.status,
    summary: s.summary,
    ...(s.arg ? { arg: s.arg } : {}),
    ...(s.detail ? { detail: s.detail.slice(0, STEP_DETAIL_KEEP) } : {}),
  }));
  // 工具块进会话：下一轮 messages 能看见上一轮调过什么、拿到了什么。
  const blocks = stepsToBlocks(answer, kept);
  const next: QaTurn[] = [
    ...prev,
    {
      role: "user",
      content: question,
      blocks: [{ kind: "text", text: question }],
      ...(usage?.modelId ? { modelId: usage.modelId } : {}),
      ...(usage?.modelName ? { modelName: usage.modelName } : {}),
    },
    {
      role: "assistant",
      content: answer,
      ...(keepWhole ? { keep: "whole" as const } : {}),
      ...(kept && kept.length > 0 ? { steps: kept } : {}),
      ...(blocks ? { blocks } : {}),
      ...(plan && plan.length > 0 ? { plan } : {}),
      ...(limited ? { limited: true } : {}),
      ...(usage?.elapsedMs != null ? { elapsedMs: usage.elapsedMs } : {}),
      ...(usage?.promptTokens != null ? { promptTokens: usage.promptTokens } : {}),
      ...(usage?.completionTokens != null ? { completionTokens: usage.completionTokens } : {}),
      ...(usage?.contextChars != null ? { contextChars: usage.contextChars } : {}),
      ...(usage?.budgetRatio != null ? { budgetRatio: usage.budgetRatio } : {}),
      ...(usage?.trimmedRounds ? { trimmedRounds: usage.trimmedRounds } : {}),
      ...(usage?.cacheHitTokens ? { cacheHitTokens: usage.cacheHitTokens } : {}),
      ...(usage?.undoToken ? { undoToken: usage.undoToken } : {}),
      ...(usage?.modelId ? { modelId: usage.modelId } : {}),
      ...(usage?.modelName ? { modelName: usage.modelName } : {}),
    },
  ];
  return compactSession(next);
}

export type AskStreamOptions = {
  selectedModel: AIModelConfig | undefined;
  /** 备用模型：主模型这一轮彻底失败时后端换它重来（见 `resolveFallbackModel`）。 */
  fallbackModel?: AIModelConfig | null;
  input: string;
  setInput: (updater: string | ((prev: string) => string)) => void;
  setSelectedText: (text: string) => void;
  getContext: (mode: AskMode) => Promise<ContextResult>;
  /** Today's commit notes, used instead of the raw diff dump for a daily report. */
  collectDailyBrief: () => Promise<{ text: string; repos: string[] }>;
  /** User override from the composer pill; "auto" = let routeQuestion decide. */
  routeOverride: RouteOverride;
  /** Called once a request is dispatched: one-shot override reset. */
  onSent: () => void;
  /** Drop the workbench context cache: this round changed the repo it describes. */
  onInvalidateContext: () => void;
  /** 「以后都允许」：把这一类的 `action_kind` 写进设置，下一轮直接放行。带仓库路径好按仓记忆。 */
  onRememberKind?: (kind: string, repoPath: string) => void;
  /** Persist the completed exchange as a session (wired to useAskSessions). */
  persistTurns: (turns: QaTurn[], firstQuestion: string) => void;
  /** Active profile: extra system role + optional temperature override. */
  profileSystemRole?: string;
  profileTemperature?: number;
  /** Light code/file attachments appended to the user message. */
  attachments?: AskAttachment[];
  /** Permission level + repo for the tool loop every question runs through. */
  agent: AgentRunOptions;
  /** Format of the daily report (日报). */
  dailyStyle?: DailyStyle;
};

/**
 * The streaming Q&A pipeline: send / stop / regenerate over `agent_run`
 * (generate-text-chunk/-done/-error + agent-step / agent-approval events).
 */
export function useAskStream(opts: AskStreamOptions) {
  const { t } = useTranslation("aihub");
  const {
    selectedModel, fallbackModel, input, setInput, setSelectedText, getContext, collectDailyBrief,
    routeOverride, onSent, onInvalidateContext, onRememberKind, persistTurns,
    profileSystemRole, profileTemperature, attachments = [], agent, dailyStyle = "chat",
  } = opts;

  const [answer, setAnswer] = useState("");
  const [source, setSource] = useState<AnswerSource | null>(null);
  /** Mode of the previous exchange, so short follow-ups stay on the project. */
  const lastModeRef = useRef<AskMode | null>(null);
  /** Notes from the last daily report, so a short rewrite stays on the same facts. */
  const lastDailyBriefRef = useRef("");
  const lastDailyReposRef = useRef<string[]>([]);
  const [busy, setBusy] = useState(false);
  /** busy 的同步镜像：send 的重入守卫必须读 ref——state 在同帧双发时都是旧值，
      双击/双 Enter 会并发两个 agent_run 同写一个仓库。 */
  const busyRef = useRef(false);
  const setBusySync = (value: boolean) => {
    busyRef.current = value;
    setBusy(value);
  };
  const [error, setError] = useState<string | null>(null);
  /** Full transcript. The last 50 rounds go verbatim; older rounds ride along as a digest. */
  const [history, setHistory] = useState<QaTurn[]>([]);
  /** Question currently being answered (shown above the streaming answer). */
  const [pending, setPending] = useState<{ question: string } | null>(null);
  /** Where the in-flight request is: collecting context → waiting for the first token → streaming. */
  const [phase, setPhase] = useState<AskPhase>("idle");
  const [agentSteps, setAgentSteps] = useState<AgentStep[]>([]);
  const agentStepsRef = useRef<AgentStep[]>([]);
  const rememberSteps = (next: AgentStep[] | ((prev: AgentStep[]) => AgentStep[])) => {
    // 同步写 ref：`finishTurn` 在同一个 tick 里读它，而 setState 的 updater 要等渲染才跑，
    // 否则最后一条 `agent-step`（done）会丢，`stepsMutatedWorkspace` 也判不出来。
    const value = typeof next === "function" ? next(agentStepsRef.current) : next;
    agentStepsRef.current = value;
    setAgentSteps(value);
  };
  /** Checklist published by `update_plan`; empty until the model writes one. */
  const [agentPlan, setAgentPlan] = useState<AgentPlanStep[]>([]);
  const agentPlanRef = useRef<AgentPlanStep[]>([]);
  const rememberPlan = useCallback((next: AgentPlanStep[]) => {
    agentPlanRef.current = next;
    setAgentPlan(next);
  }, []);
  const [agentApproval, setAgentApproval] = useState<AgentApproval | null>(null);
  const [stopping, setStopping] = useState(false);
  /** Last request that failed (not stopped), so the error card can offer a retry. */
  const failedRef = useRef<{
    question: string; history: QaTurn[]; mode: AskMode; attachments: AskAttachment[];
  } | null>(null);
  const [failedMode, setFailedMode] = useState<AskMode | null>(null);
  /** Last question + the history snapshot it was sent with, for regenerate. */
  const lastQuestionRef = useRef<string>("");
  const lastHistoryRef = useRef<QaTurn[]>([]);
  const streamIdRef = useRef(0);
  /** Request id of the stream currently in flight, so it can be cancelled. */
  const activeReqRef = useRef<string | null>(null);
  /** Set when the user hits stop, including before the backend command is active. */
  const stopRequestedRef = useRef(false);
  /** 当前这一轮的就地收尾函数（send 里定义）：停止看门狗超时后调用。 */
  const forceStopRef = useRef<(() => void) | null>(null);
  /** 在飞的 Tauri 监听。放在 ref 里，`send` 结束时和组件卸载时都能注销。 */
  const unlistenersRef = useRef<Array<() => void>>([]);
  const dropListeners = useCallback(() => {
    unlistenersRef.current.forEach((fn) => fn());
    unlistenersRef.current.length = 0;
  }, []);

  /** Persist (or create) the active session after a completed exchange. */
  const persistRef = useRef(persistTurns);
  persistRef.current = persistTurns;

  /** Attachments of the last dispatched round, so 重新生成 replays the same files. */
  const lastAttachmentsRef = useRef<AskAttachment[]>([]);

  /** `replay` (regenerate / retry) pins the mode and files of the original question. */
  const send = async (
    rawInput?: string,
    historyOverride?: QaTurn[],
    replay?: { mode: AskMode; attachments?: AskAttachment[] },
  ) => {
    if (busyRef.current) return;
    if (!selectedModel) {
      setError(t("noModel"));
      return;
    }
    const question = (rawInput ?? input).trim();
    if (!question) {
      setError(t("emptyInput"));
      return;
    }
    // 只读权限没有仓库也能问；会改东西的三档必须落在一个工作区或仓库里
    if (agent.permission !== "read" && !agent.repoPath.trim()) {
      setError(t("agent.noRepo"));
      return;
    }
    // The context dump is per-request; history carries only the plain Q&A pairs.
    const baseHistory = historyOverride ?? history;
    /** Char-budgeted transcript for this request; oldest rounds fold out first. */
    const budget = requestHistory(baseHistory);
    /** The previous round's checklist belongs to that round, not this one. */
    rememberPlan([]);
    /** The composer clears attachments on send: this round carries its own copy, so a
        retry / regenerate replays with the files it was answered from. */
    const sentAttachments = replay?.attachments ?? attachments;
    lastAttachmentsRef.current = sentAttachments;
    // Regenerate replaces the last pair: hide it now, restore it if the retry fails.
    const shownBefore = history;
    if (historyOverride) setHistory(baseHistory);
    // Stream events are broadcast to every window; time + per-window counter alone can
    // collide across windows, so add a random part.
    const reqId = `qa-${Date.now()}-${++streamIdRef.current}-${crypto.randomUUID().slice(0, 8)}`;
    const myGen = streamIdRef.current;
    activeReqRef.current = reqId;
    stopRequestedRef.current = false;
    // 上一轮的就地收尾如果还挂着，别让它落进这一轮
    forceStopRef.current = null;
    setBusySync(true);
    setError(null);
    setAnswer("");
    rememberSteps([]);
    setAgentApproval(null);
    setStopping(false);
    setSelectedText("");
    setPending({ question });
    setPhase("waiting");
    failedRef.current = null;
    setFailedMode(null);
    if (!rawInput) setInput("");
    let sentMode: AskMode = replay?.mode ?? "chat";
    let keepWhole = false;
    /** Daily report round: no tools, and the answer is cleaned before it lands. */
    let isDaily = false;
    const landText = (text: string) => (isDaily ? cleanDailyReport(text) : text);
    const finishTurn = (
      text: string,
      limited = false,
      usage?: {
        elapsedMs?: number;
        promptTokens?: number;
        completionTokens?: number;
        contextChars?: number;
        budgetRatio?: number;
        trimmedRounds?: number;
        cacheHitTokens?: number;
        undoToken?: string;
        modelId?: string;
        modelName?: string;
      },
    ) => {
      const nextTurns = appendTurn(
        baseHistory, question, text, keepWhole, agentStepsRef.current, limited,
        // 没有显式报告回答模型时，默认用本轮所选模型（轮级记忆的锚点）。
        { modelId: selectedModel.id, modelName: selectedModel.name, ...usage },
        agentPlanRef.current,
      );
      setHistory(nextTurns);
      setPending(null);
      setPhase("idle");
      setAnswer("");
      setStopping(false);
      lastQuestionRef.current = question;
      lastHistoryRef.current = baseHistory;
      persistRef.current(nextTurns, question);
      // The round changed the repo, so the cached workbench context now describes
      // files as they were before the agent touched them.
      if (stepsMutatedWorkspace(agentStepsRef.current)) onInvalidateContext();
    };
    /** `failed` = a real error (offer retry); false = stopped / empty answer. */
    const failTurn = (failed: boolean) => {
      setPending(null);
      setPhase("idle");
      setAnswer("");
      setStopping(false);
      if (historyOverride) setHistory(shownBefore);
      else setInput((prev) => prev || question);
      if (failed) {
        failedRef.current = {
          question, history: baseHistory, mode: sentMode, attachments: sentAttachments,
        };
        setFailedMode(sentMode);
      }
    };

    let settled = false;
    let full = "";
    /** Stopped / failed mid-stream still consumed tokens: estimate what arrived. */
    const recordPartialUsage = () => {
      if (!full.trim()) return;
      recordAiUsage({
        modelId: selectedModel.id,
        modelName: selectedModel.name,
        estimatedTokens: Math.max(1, Math.round((question.length + full.length) / 4)),
      });
    };
    /** 一帧内到多个 chunk 只重渲一次：SSE 常常几毫秒推一个 delta，逐个 setState 会把
        输入区、字幕、审批卡整棵树重渲几十次。 */
    let answerFrame: number | null = null;
    let answerPending = "";
    const cancelAnswerFrame = () => {
      if (answerFrame !== null) window.cancelAnimationFrame(answerFrame);
      answerFrame = null;
    };
    const teardown = () => {
      cancelAnswerFrame();
      dropListeners();
    };
    // 上一轮若没收干净（异常路径 / 窗口切换），这里先清掉，避免同一事件被两个 handler 处理
    dropListeners();
    const stoppedEarly = () => {
      if (settled) return true;
      if (!stopRequestedRef.current && streamIdRef.current === myGen) return false;
      settled = true;
      failTurn(false);
      setBusySync(false);
      setStopping(false);
      activeReqRef.current = null;
      teardown();
      return true;
    };

    /** 停止看门狗的就地收尾：和「收到取消错误」完全同一套落地动作。
        后端卡在不可打断的一步时，停止键也不能失灵。 */
    const forceStop = () => {
      if (settled) return;
      settled = true;
      // 只做本地收尾会把后端留在原地：它还在跑这一轮的工具，而 busy 已经放开，
      // 用户再问一句就有两个 `agent_run` 同时动同一个仓库。先发取消（不等它返回）。
      const activeId = activeReqRef.current;
      if (activeId) void invoke("cancel_request", { id: activeId }).catch(() => {});
      // 让迟到的 done / error 失效
      streamIdRef.current++;
      recordPartialUsage();
      const partial = landText(stripThinking(full));
      if (partial.trim()) finishTurn(partial);
      else failTurn(false);
      setBusySync(false);
      setStopping(false);
      activeReqRef.current = null;
      teardown();
    };
    forceStopRef.current = forceStop;

    try {
      let system: string;
      let user: string;
      const route = routeQuestion({
        question,
        lastMode: lastModeRef.current,
        override: replay?.mode ?? routeOverride,
      });
      sentMode = route.mode;
      let summary: ContextSummary = { repos: [], parts: [] };
      /** Did the context block get clipped? Surfaced on the answer source line. */
      let ctxTruncated = false;
      if (route.mode === "project") setPhase("context");
      const reviseDaily = isDailyRevision(question) && Boolean(lastDailyBriefRef.current);
      if (isDailyReport(question) || reviseDaily) {
        setPhase("context");
        const brief = isDailyReport(question)
          ? await collectDailyBrief()
          : { text: lastDailyBriefRef.current, repos: lastDailyReposRef.current };
        if (isDailyReport(question)) {
          lastDailyBriefRef.current = brief.text;
          lastDailyReposRef.current = brief.repos;
        }
        summary = { repos: brief.repos, parts: ["git", "dirty"] };
        system = buildMeta(selectedModel) + dailyReportSystem(dailyStyle);
        isDaily = true;
        if (reviseDaily) {
          system +=
            " The user is revising the report you just wrote. Keep the same facts from the notes and apply only their instruction.";
        }
        if (profileSystemRole?.trim()) {
          system += `\n\n[Profile] ${profileSystemRole.trim()}`;
        }
        user =
          `${question}\n\n---\nToday's notes (source only — write the report, do not copy this structure):\n${brief.text}`;
        user += formatAttachmentsBlock(sentAttachments);
        sentMode = "project";
        keepWhole = true;
        if (stoppedEarly()) return;
      } else {
        lastDailyBriefRef.current = "";
        lastDailyReposRef.current = [];
        const ctxResult = await getContext(route.mode);
        summary = ctxResult.summary;
        ctxTruncated = ctxResult.truncated;
        const hasProject = summary.parts.length > 0;
        system = buildSystem(selectedModel, route.mode, hasProject);
        if (profileSystemRole?.trim()) {
          system += `\n\n[Profile] ${profileSystemRole.trim()}`;
        }
        const ctx = ctxResult.text;
        user = ctx
          ? `${question}\n\n---\nContext from workbench:\n${ctx}`
          : question;
        user += formatAttachmentsBlock(sentAttachments);
        if (stoppedEarly()) return;
      }
      system = withSessionDigest(system, baseHistory);
      if (streamIdRef.current === myGen) setPhase("waiting");
      setSource({
        mode: sentMode,
        summary,
        modelName: selectedModel.name,
        contextTruncated: ctxTruncated || undefined,
        droppedTurns: budget.dropped || undefined,
      });
      lastModeRef.current = sentMode;
      if (!replay) onSent();
      const streamStarted = performance.now();

      const onChunk = async (event: { payload?: { id: string; text: string } }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        full += payload.text || "";
        // `<` 都不出现时不可能有 <think>：跳过整段 toLowerCase + 三次扫描
        const visible = full.includes("<") ? stripThinking(full, true) : full;
        // Still inside <think>: stay on the waiting indicator instead of an empty answer.
        if (visible) setPhase("streaming");
        answerPending = visible;
        if (answerFrame === null) {
          answerFrame = window.requestAnimationFrame(() => {
            answerFrame = null;
            setAnswer(answerPending);
          });
        }
      };
      const onDone = async (event: {
        payload?: {
          id: string;
          elapsedMs?: number;
          promptTokens?: number;
          completionTokens?: number;
          /** The turn stopped at the round / action cap:「继续」resumes it. */
          limited?: boolean;
          /** Context / budget telemetry of the last round (see `TurnStats` in agent.rs). */
          contextChars?: number;
          budgetRatio?: number;
          trimmedRounds?: number;
          cacheHitTokens?: number;
          /** Present when the turn wrote files and left a snapshot to restore. */
          undoToken?: string;
          /** 降级到备用模型时带上其展示名（主模型成功则缺省）。 */
          usedModel?: string;
        };
      }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        if (!settled) {
          settled = true;
          setBusySync(false);
          setStopping(false);
          const elapsedMs =
            payload.elapsedMs ?? Math.round(performance.now() - streamStarted);
          const promptTokens = payload.promptTokens;
          const completionTokens = payload.completionTokens;
          const estimatedTokens =
            promptTokens != null || completionTokens != null
              ? undefined
              : Math.max(1, Math.round((question.length + full.length) / 4));
          const telemetry = {
            contextChars: payload.contextChars,
            budgetRatio: payload.budgetRatio,
            trimmedRounds: payload.trimmedRounds,
            cacheHitTokens: payload.cacheHitTokens,
          };
          setSource((prev) =>
            prev
              ? { ...prev, elapsedMs, promptTokens, completionTokens, estimatedTokens, ...telemetry }
              : prev
          );
          const finalText = landText(stripThinking(full));
          if (finalText.trim()) {
            // 降级时后端报告真正回答的模型，用量台账也按它记，分模型统计才准。
            const answered =
              payload.usedModel && fallbackModel?.name === payload.usedModel
                ? { id: fallbackModel.id, name: fallbackModel.name }
                : payload.usedModel
                ? { id: selectedModel.id, name: payload.usedModel }
                : { id: selectedModel.id, name: selectedModel.name };
            recordAiUsage({
              modelId: answered.id,
              modelName: answered.name,
              promptTokens,
              completionTokens,
              estimatedTokens,
            });
            finishTurn(finalText, payload.limited === true, {
              elapsedMs, promptTokens, completionTokens, ...telemetry, undoToken: payload.undoToken,
              modelId: answered.id, modelName: answered.name,
            });
          } else {
            failTurn(false);
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
          recordPartialUsage();
          // Stopped mid-stream: keep the partial answer as context too.
          const partial = landText(stripThinking(full));
          if (stopped && partial.trim()) finishTurn(partial);
          else failTurn(!stopped);
          setBusySync(false);
          setStopping(false);
        }
        activeReqRef.current = null;
        teardown();
      };

      unlistenersRef.current.push(await listen<{ id: string; text: string }>("generate-text-chunk", onChunk));
      unlistenersRef.current.push(
        await listen<{
          id: string;
          elapsedMs?: number;
          promptTokens?: number;
          completionTokens?: number;
        }>("generate-text-done", onDone)
      );
      unlistenersRef.current.push(await listen<{ id: string; error: string }>("generate-text-error", onError));
      unlistenersRef.current.push(
        await listen<{
          id: string;
          tool: string;
          status: string;
          summary: string;
          detail: string;
          arg?: string;
        }>("agent-step", (event) => {
          const payload = event.payload;
          if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
          if (payload.status !== "running") setAgentApproval(null);
          rememberSteps((prev) => {
            const last = prev[prev.length - 1];
            const step: AgentStep = {
              tool: payload.tool,
              status: payload.status,
              summary: payload.summary,
              detail: payload.detail || "",
              // The final status arrives as its own event: carry the argument over.
              arg: payload.arg || last?.arg,
            };
            if (last && last.tool === step.tool && last.status === "running" && last.summary === step.summary) {
              return [...prev.slice(0, -1), step];
            }
            return [...prev, step];
          });
        }),
      );
      unlistenersRef.current.push(
        await listen<{ id: string; steps: AgentPlanStep[] }>("agent-plan", (event) => {
          const payload = event.payload;
          if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
          // The backend already normalised titles / statuses; keep only what the card can draw.
          const steps = (Array.isArray(payload.steps) ? payload.steps : [])
            .filter((s) => s && typeof s.title === "string" && s.title.trim())
            .map((s) => ({
              title: s.title.trim(),
              status: s.status === "done" ? "done" as const : s.status === "in_progress" ? "in_progress" as const : "pending" as const,
            }));
          if (steps.length > 0) rememberPlan(steps);
        }),
      );
      unlistenersRef.current.push(
        await listen<{
          id: string;
          callId: string;
          tool: string;
          summary: string;
          preview: string;
          previewKind?: "diff" | "text";
          deadlineAt?: number;
          actionKind?: string;
          canRemember?: boolean;
          patches?: AgentApprovalPatch[];
          callIds?: string[];
          kind?: string;
          options?: string[];
          allowFreeText?: boolean;
          multi?: boolean;
        }>("agent-approval", (event) => {
          const payload = event.payload;
          if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
          const kind =
            payload.kind === "question" || payload.kind === "plan" ? payload.kind : "approval";
          setAgentApproval({
            callId: payload.callId,
            tool: payload.tool,
            summary: payload.summary,
            preview: payload.preview || "",
            previewKind: payload.previewKind === "diff" ? "diff" : "text",
            deadlineAt: payload.deadlineAt ?? 0,
            actionKind: payload.actionKind || "",
            canRemember: Boolean(payload.canRemember),
            patches: Array.isArray(payload.patches) ? payload.patches : undefined,
            callIds: Array.isArray(payload.callIds) ? payload.callIds : undefined,
            kind,
            options: Array.isArray(payload.options) ? payload.options.filter(Boolean) : undefined,
            allowFreeText: Boolean(payload.allowFreeText),
            multi: Boolean(payload.multi),
          });
          // The round is blocked on the user: an opt-in raise beats a silent
          // auto-deny five minutes later.
          if (localStorage.getItem(APPROVAL_FOCUS_KEY) === "1") {
            void invoke("show_ai_hub").catch(() => {});
          }
        }),
      );

      const maxTokens = Math.max(selectedModel.maxTokens || 0, 1536);
      const temperature =
        typeof profileTemperature === "number" && Number.isFinite(profileTemperature)
          ? // The backend clamps to 0..1 (agent.rs / ai_commands.rs); match it so the
            // number shown in the picker is the number that gets sent.
            Math.min(1, Math.max(0, profileTemperature))
          : selectedModel.temperature;
      const config = { ...selectedModel, maxTokens, temperature };
      if (stoppedEarly()) return;
      // A plain-chat round under 只读权限 has no business carrying the tool menu:
      // it invites a file/git round trip the question never asked for.
      const wantTools =
        !isDaily &&
        (sentMode === "project" || agent.permission !== "read");
      await invoke("agent_run", {
        req: {
          config,
          // 主模型整轮失败时后端换它重试一次；没配置第二个同协议模型就为空
          fallbackConfig: fallbackModel || null,
          system,
          user,
          requestId: reqId,
          repoPath: agent.repoPath,
          services: agent.services,
          // 只发内容块历史：后端优先用 `messages`，再带一份 `history` 等于把同一份
          // 上下文序列化两遍（长会话里这份重复是实打实的 IPC 成本）。
          // 图片附件作为 image 块跟在正文后面：文本附件已经拼进 `user`，不重复发
          messages: [
            ...turnsToMessages(budget.turns, { stripImages: !selectedModel.vision }),
            buildUserMessage(user, imageAttachmentsOf(sentAttachments)),
          ],
          apps: agent.apps,
          permission: agent.permission,
          // The daily notes are already collected: the report needs no repo tools.
          tools: wantTools,
          vision: agent.vision,
          diffConfirm: agent.diffConfirm,
          alwaysAllow: agent.alwaysAllow,
          webAllow: agent.webAllow,
          webTier: agent.webTier || "allowlist",
          webSearchProvider: agent.webSearchProvider || "",
          webSearchKey: agent.webSearchKey || "",
          subagents: agent.subagents,
          subagentWrite: agent.subagentWrite,
          parallelReads: agent.parallelReads,
          retries: agent.retries,
          grepIgnore: agent.grepIgnore,
          planFirst: agent.planFirst,
          contextTokens: agent.contextTokens,
          resultCap: agent.resultCap,
          anthropicStream: agent.anthropicStream,
          mcpEnabled: agent.mcpEnabled,
          mcp: agent.mcp,
          mode: agent.mode ?? "",
        },
      });
      if (streamIdRef.current === myGen) {
        lastQuestionRef.current = question;
        lastHistoryRef.current = baseHistory;
      }
    } catch (e) {
      if (streamIdRef.current === myGen && !settled) {
        settled = true;
        // Stopping rejects the command too — no error for a deliberate stop.
        const stopped = String(e).includes("AI_REQUEST_CANCELLED");
        if (!stopped) setError(String(e));
        recordPartialUsage();
        const partial = landText(stripThinking(full));
        if (stopped && partial.trim()) finishTurn(partial);
        else failTurn(!stopped);
        setBusySync(false);
        setStopping(false);
      }
      activeReqRef.current = null;
      teardown();
    }
  };

  /** 停止看门狗：`cancel_request` 发出后，后端所有可等待点都在轮询取消标记，
      正常几秒内就会收尾。真卡在不可打断的一步（某个 spawn_blocking、坏掉的连接）
      时，就地在本地结束这一轮——停止键不能看起来失灵。
      后端稍后才到的 done / error 事件会被 teardown 掉，不会二次落地。 */
  useEffect(() => {
    if (!stopping) return;
    const timer = window.setTimeout(() => {
      if (!busyRef.current) return;
      forceStopRef.current?.();
      setStopping(false);
    }, 4000);
    return () => window.clearTimeout(timer);
  }, [stopping]);

  /// Abort the in-flight stream. The backend answers with an
  /// AI_REQUEST_CANCELLED error event (handled as a normal stop) and the partial
  /// answer stays on screen.
  const stopStream = async () => {
    const id = activeReqRef.current;
    stopRequestedRef.current = true;
    if (!id) {
      // 还显示 busy 却没有在飞的请求 id：状态已经失真（结束事件丢了），
      // 就地收尾，别让停止键变成摆设。
      forceStopRef.current?.();
      setStopping(false);
      return;
    }
    setStopping(true);
    try {
      await invoke("cancel_request", { id });
    } catch (e) {
      // 取消没送到后端：撤销「已请求停止」，否则下一次 send 会被 stoppedEarly 当成已停止，
      // 问题被静默塞回输入框。
      stopRequestedRef.current = false;
      setStopping(false);
      setError(String(e));
    }
  };

  // Cleanup any live stream listeners if the window closes mid-stream.
  useEffect(() => {
    return () => {
      // Bump the generation counter so any in-flight callbacks become no-ops,
      // and drop the Tauri listeners themselves — they outlive the component otherwise.
      streamIdRef.current++;
      dropListeners();
    };
  }, [dropListeners]);

  /** Wipe the current chat (new-chat / deleting the active session). */
  const resetChat = useCallback(() => {
    setHistory([]);
    setPending(null);
    setAnswer("");
    setSource(null);
    setError(null);
    failedRef.current = null;
    setFailedMode(null);
    setSelectedText("");
    lastQuestionRef.current = "";
    lastHistoryRef.current = [];
    lastAttachmentsRef.current = [];
    lastModeRef.current = null;
    lastDailyBriefRef.current = "";
    lastDailyReposRef.current = [];
    rememberSteps([]);
    rememberPlan([]);
    setAgentApproval(null);
    setStopping(false);
  }, [setSelectedText, rememberPlan]);

  /** Point the chat state at a restored session's turns. */
  const loadSession = useCallback((turns: QaTurn[]) => {
    const compacted = compactSession(turns);
    setHistory(compacted);
    setSource(null);
    lastModeRef.current = null;
    lastDailyBriefRef.current = "";
    lastDailyReposRef.current = [];
    rememberSteps([]);
    rememberPlan([]);
    setAgentApproval(null);
    setStopping(false);
    const verbatim = compacted.filter((t) => !t.digest);
    const lastUser = [...verbatim].reverse().find((t) => t.role === "user");
    setAnswer("");
    setPending(null);
    setError(null);
    failedRef.current = null;
    setFailedMode(null);
    setSelectedText("");
    // Point regenerate at the final exchange of this session.
    lastQuestionRef.current = lastUser?.content ?? "";
    lastHistoryRef.current = verbatim.length >= 2
      ? compacted.slice(0, -2)
      : compacted.filter((t) => t.digest);
  }, [setSelectedText]);

  /**
   * Fold the transcript back to just before turn `index` (「从这里重新开始」). The
   * files the agent already changed stay changed — this only rewrites the conversation.
   * An empty result is not persisted: that would leave a blank session behind.
   */
  const truncateTo = useCallback(
    (index: number): QaTurn[] => {
      const compacted = compactSession(history.slice(0, Math.max(0, index)));
      setHistory(compacted);
      setSource(null);
      setAnswer("");
      setPending(null);
      setError(null);
      failedRef.current = null;
      setFailedMode(null);
      lastModeRef.current = null;
      const verbatim = compacted.filter((t) => !t.digest);
      const lastUser = [...verbatim].reverse().find((t) => t.role === "user");
      lastQuestionRef.current = lastUser?.content ?? "";
      lastHistoryRef.current =
        verbatim.length >= 2 ? compacted.slice(0, -2) : compacted.filter((t) => t.digest);
      if (compacted.length > 0) persistRef.current(compacted, lastUser?.content ?? compacted[0].content);
      return compacted;
    },
    [history],
  );

  /** Replay the last question with the same history snapshot and files. */
  const regenerate = () =>
    send(lastQuestionRef.current, lastHistoryRef.current, {
      mode: lastModeRef.current ?? "chat",
      attachments: lastAttachmentsRef.current,
    });

  /** Resend the request that failed; `asChat` drops the project context. */
  const retry = (asChat = false) => {
    const f = failedRef.current;
    if (!f) return;
    // A failed send put the question back into the input; the retry owns it now.
    setInput((prev) => (prev.trim() === f.question ? "" : prev));
    void send(f.question, f.history, {
      mode: asChat ? "chat" : f.mode,
      attachments: f.attachments,
    });
  };

  const dismissError = () => {
    setError(null);
    failedRef.current = null;
    setFailedMode(null);
  };

  /** Record a local action (open a page / launch an app) without calling the model. */
  const noteLocal = (question: string, answer: string) => {
    if (busyRef.current) return;
    const q = question.trim();
    if (!q || !answer.trim()) return;
    const nextTurns = appendTurn(history, q, answer.trim());
    setHistory(nextTurns);
    setInput("");
    setError(null);
    lastQuestionRef.current = q;
    persistRef.current(nextTurns, q);
  };

  const decideAgent = async (decision: AgentDecision, answer?: string) => {
    const id = activeReqRef.current;
    const ids = agentApproval?.callIds?.length
      ? agentApproval.callIds
      : agentApproval?.callId
        ? [agentApproval.callId]
        : [];
    if (!id || ids.length === 0) return;
    const kind = agentApproval?.actionKind || "";
    try {
      await invoke("agent_decide", {
        requestId: id,
        callIds: ids,
        allow: decision !== "deny",
        scope: decision === "always" ? "always" : decision === "turn" ? "turn" : "once",
        answer: answer?.trim() || null,
      });
      // 收下之后再撤卡片：调用失败时卡片要留着，用户才能重试（以前先撤，失败就没得点了）
      setAgentApproval(null);
      // 记住要等后端收下这次决定：被判定失效（回合已结束）就不该写进设置
      if (decision === "always" && kind) onRememberKind?.(kind, agent.repoPath);
    } catch (e) {
      const message = String(e);
      setError(message);
      // 这一轮已经结束 / 没有等待项：卡片留着也没人能答，收掉
      if (message.includes("AI_APPROVAL_STALE")) setAgentApproval(null);
    }
  };

  return {
    /** Streaming text of the in-flight answer (empty once it lands in `history`). */
    answer,
    pending,
    phase,
    source,
    busy,
    error,
    setError,
    /** Mode of the last failed request (null = nothing to retry). */
    failedMode,
    retry,
    dismissError,
    history,
    lastModeRef,
    lastQuestionRef,
    lastHistoryRef,
    send,
    noteLocal,
    regenerate,
    stopStream,
    agentSteps,
    agentPlan,
    agentApproval,
    stopping,
    decideAgent,
    resetChat,
    loadSession,
    truncateTo,
  };
}
