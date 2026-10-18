import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AIModelConfig, QuickAskTurn } from "../../core/types";
import { buildPromptOptimizeMessages } from "../../lib/promptOptimize";
import { recordAiUsage } from "../../lib/aiUsage";
import { formatAttachmentsBlock, type AskAttachment } from "../../lib/askAttachments";
import { buildMeta, buildSystem } from "./config";
import { compactSession, requestHistory, withSessionDigest } from "./sessionCompact";
import { DAILY_REPORT_SYSTEM, isDailyReport, isDailyRevision } from "./dailyReport";
import type { OptimizeConfig, TaskKind } from "./config";
import { routeQuestion } from "./route";
import type { AskMode, RouteOverride } from "./route";
import type { ContextResult, ContextSummary } from "./useAskContext";

/** What the latest answer was based on — rendered as the answer source line. */
export type AnswerSource = {
  mode: AskMode;
  summary: ContextSummary;
  modelName: string;
  task: TaskKind;
  /** Wall-clock ms for the completed stream (when known). */
  elapsedMs?: number;
  promptTokens?: number;
  completionTokens?: number;
  /** Fallback estimate (~chars/4) when the provider omitted usage. */
  estimatedTokens?: number;
};

type QaTurn = QuickAskTurn;

export type AskPhase = "idle" | "context" | "waiting" | "streaming";

export type AgentStep = {
  tool: string;
  status: "running" | "done" | "denied" | "error" | string;
  summary: string;
  detail: string;
};

export type AgentApproval = {
  callId: string;
  tool: string;
  summary: string;
  preview: string;
};

export type AgentRunOptions = {
  enabled: boolean;
  repoPath: string;
  apps: { name: string; path: string; args: string }[];
};

/** Full transcript (displayed + persisted). Past the verbatim window, older rounds fold into a digest. */
function appendTurn(
  prev: QaTurn[],
  question: string,
  answer: string,
  keepWhole = false,
  steps?: AgentStep[],
): QaTurn[] {
  const kept = steps?.filter((s) => s.summary).map((s) => ({
    tool: s.tool,
    status: s.status,
    summary: s.summary,
  }));
  const next: QaTurn[] = [
    ...prev,
    { role: "user", content: question },
    {
      role: "assistant",
      content: answer,
      ...(keepWhole ? { keep: "whole" as const } : {}),
      ...(kept && kept.length > 0 ? { steps: kept } : {}),
    },
  ];
  return compactSession(next);
}

export type AskStreamOptions = {
  selectedModel: AIModelConfig | undefined;
  task: TaskKind;
  /** Scenario/goals for the `optimize` task (ignored otherwise). */
  optimize: OptimizeConfig;
  input: string;
  setInput: (updater: string | ((prev: string) => string)) => void;
  setSelectedText: (text: string) => void;
  getContext: (mode: AskMode) => Promise<ContextResult>;
  /** Today's commit notes, used instead of the raw diff dump for a daily report. */
  collectDailyBrief: () => Promise<{ text: string; repos: string[] }>;
  /** User override from the composer pill; "auto" = let routeQuestion decide. */
  routeOverride: RouteOverride;
  /** Called once a request is dispatched: one-shot task + override reset. */
  onSent: () => void;
  /** Persist the completed exchange as a session (wired to useAskSessions). */
  persistTurns: (turns: QaTurn[], firstQuestion: string) => void;
  /** Active profile: extra system role + optional temperature override. */
  profileSystemRole?: string;
  profileTemperature?: number;
  /** Light code/file attachments appended to the user message. */
  attachments?: AskAttachment[];
  /** When enabled, this question runs the repo tool loop instead of one completion. */
  agent?: AgentRunOptions;
};

/**
 * The streaming Q&A pipeline: send / stop / regenerate over
 * `generate_text_stream` + generate-text-chunk/-done/-error events.
 */
export function useAskStream(opts: AskStreamOptions) {
  const { t } = useTranslation("quickask");
  const {
    selectedModel, task, optimize, input, setInput, setSelectedText, getContext, collectDailyBrief,
    routeOverride, onSent, persistTurns,
    profileSystemRole, profileTemperature, attachments = [], agent,
  } = opts;

  const [answer, setAnswer] = useState("");
  const [source, setSource] = useState<AnswerSource | null>(null);
  /** Mode of the previous exchange, so short follow-ups stay on the project. */
  const lastModeRef = useRef<AskMode | null>(null);
  /** Notes from the last daily report, so a short rewrite stays on the same facts. */
  const lastDailyBriefRef = useRef("");
  const lastDailyReposRef = useRef<string[]>([]);
  /** Task the last question was sent with, so regenerate replays it. */
  const lastTaskRef = useRef<TaskKind>("none");
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
    setAgentSteps((prev) => {
      const value = typeof next === "function" ? next(prev) : next;
      agentStepsRef.current = value;
      return value;
    });
  };
  const [agentApproval, setAgentApproval] = useState<AgentApproval | null>(null);
  const [stopping, setStopping] = useState(false);
  /** Last request that failed (not stopped), so the error card can offer a retry. */
  const failedRef = useRef<{ question: string; history: QaTurn[]; task: TaskKind; mode: AskMode } | null>(null);
  const [failedMode, setFailedMode] = useState<AskMode | null>(null);
  /** Last question + the history snapshot it was sent with, for regenerate. */
  const lastQuestionRef = useRef<string>("");
  const lastHistoryRef = useRef<QaTurn[]>([]);
  const streamIdRef = useRef(0);
  /** Request id of the stream currently in flight, so it can be cancelled. */
  const activeReqRef = useRef<string | null>(null);
  /** Set when the user hits stop, including before the backend command is active. */
  const stopRequestedRef = useRef(false);

  /** Persist (or create) the active session after a completed exchange. */
  const persistRef = useRef(persistTurns);
  persistRef.current = persistTurns;

  /** `replay` (regenerate) pins the task and mode of the original question. */
  const send = async (
    rawInput?: string,
    historyOverride?: QaTurn[],
    replay?: { task: TaskKind; mode: AskMode },
  ) => {
    if (busyRef.current) return;
    const sendTask = replay?.task ?? task;
    if (!selectedModel) {
      setError(t("noModel"));
      return;
    }
    const question = (rawInput ?? input).trim();
    if (!question) {
      setError(t("emptyInput"));
      return;
    }
    if (agent?.enabled && !agent.repoPath.trim()) {
      setError(t("agent.noRepo"));
      return;
    }
    // The context dump is per-request; history carries only the plain Q&A pairs.
    const baseHistory = historyOverride ?? history;
    // Regenerate replaces the last pair: hide it now, restore it if the retry fails.
    const shownBefore = history;
    if (historyOverride) setHistory(baseHistory);
    const reqId = `qa-${Date.now()}-${++streamIdRef.current}`;
    const myGen = streamIdRef.current;
    activeReqRef.current = reqId;
    stopRequestedRef.current = false;
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
    const finishTurn = (text: string) => {
      const nextTurns = appendTurn(baseHistory, question, text, keepWhole, agentStepsRef.current);
      setHistory(nextTurns);
      setPending(null);
      setPhase("idle");
      setAnswer("");
      setStopping(false);
      lastQuestionRef.current = question;
      lastHistoryRef.current = baseHistory;
      persistRef.current(nextTurns, question);
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
        failedRef.current = { question, history: baseHistory, task: sendTask, mode: sentMode };
        setFailedMode(sentMode);
      }
    };

    let settled = false;
    let full = "";
    const teardown = () => {
      unlisteners.forEach((fn) => fn());
      unlisteners.length = 0;
    };
    const unlisteners: Array<() => void> = [];
    const stoppedEarly = () => {
      if (settled) return true;
      if (!stopRequestedRef.current && streamIdRef.current === myGen) return false;
      settled = true;
      failTurn(false);
      setBusySync(false);
      activeReqRef.current = null;
      teardown();
      return true;
    };

    try {
      let system: string;
      let user: string;
      const route = routeQuestion({
        question,
        task: sendTask,
        lastMode: lastModeRef.current,
        override: replay?.mode ?? routeOverride,
      });
      sentMode = route.mode;
      let summary: ContextSummary = { repos: [], parts: [] };
      if (sendTask !== "optimize" && route.mode === "project") setPhase("context");
      if (sendTask === "optimize") {
        // Prompt rewriting is self-contained: the draft IS the request, no
        // workbench context needed.
        const built = buildPromptOptimizeMessages(optimize.scenario, optimize.goals, question);
        system = buildMeta(selectedModel) + built.system;
        user = built.user;
      } else {
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
          system = buildMeta(selectedModel) + DAILY_REPORT_SYSTEM;
          if (reviseDaily) {
            system +=
              " The user is revising the report you just wrote. Keep the same facts from the notes and apply only their instruction.";
          }
          if (profileSystemRole?.trim()) {
            system += `\n\n[Profile] ${profileSystemRole.trim()}`;
          }
          user =
            `${question}\n\n---\nToday's notes (source only — write the report, do not copy this structure):\n${brief.text}`;
          user += formatAttachmentsBlock(attachments);
          sentMode = "project";
          keepWhole = true;
          if (stoppedEarly()) return;
        } else {
          lastDailyBriefRef.current = "";
          lastDailyReposRef.current = [];
          const ctxResult = await getContext(route.mode);
          summary = ctxResult.summary;
          const hasProject = summary.parts.length > 0;
          system = buildSystem(selectedModel, route.mode, sendTask, hasProject);
          if (profileSystemRole?.trim()) {
            system += `\n\n[Profile] ${profileSystemRole.trim()}`;
          }
          const ctx = ctxResult.text;
          user = ctx
            ? `${question}\n\n---\nContext from workbench:\n${ctx}`
            : question;
          user += formatAttachmentsBlock(attachments);
          keepWhole =
            sendTask === "polish" || sendTask === "translate" || sendTask === "summarize";
          if (stoppedEarly()) return;
        }
      }
      system = withSessionDigest(system, baseHistory);
      if (streamIdRef.current === myGen) setPhase("waiting");
      setSource({ mode: sentMode, summary, modelName: selectedModel.name, task: sendTask });
      lastModeRef.current = sentMode;
      lastTaskRef.current = sendTask;
      if (!replay) onSent();
      const streamStarted = performance.now();

      const onChunk = async (event: { payload?: { id: string; text: string } }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        if (!full && payload.text) setPhase("streaming");
        full += payload.text || "";
        setAnswer((prev) => prev + (payload.text || ""));
      };
      const onDone = async (event: {
        payload?: {
          id: string;
          elapsedMs?: number;
          promptTokens?: number;
          completionTokens?: number;
        };
      }) => {
        const payload = event.payload;
        if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
        if (!settled) {
          settled = true;
          setBusySync(false);
          const elapsedMs =
            payload.elapsedMs ?? Math.round(performance.now() - streamStarted);
          const promptTokens = payload.promptTokens;
          const completionTokens = payload.completionTokens;
          const estimatedTokens =
            promptTokens != null || completionTokens != null
              ? undefined
              : Math.max(1, Math.round((question.length + full.length) / 4));
          setSource((prev) =>
            prev
              ? { ...prev, elapsedMs, promptTokens, completionTokens, estimatedTokens }
              : prev
          );
          if (full.trim()) {
            recordAiUsage({
              modelId: selectedModel.id,
              modelName: selectedModel.name,
              promptTokens,
              completionTokens,
              estimatedTokens,
            });
            finishTurn(full);
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
          // Stopped mid-stream: keep the partial answer as context too.
          if (stopped && full.trim()) finishTurn(full);
          else failTurn(!stopped);
          setBusySync(false);
        }
        activeReqRef.current = null;
        teardown();
      };

      unlisteners.push(await listen<{ id: string; text: string }>("generate-text-chunk", onChunk));
      unlisteners.push(
        await listen<{
          id: string;
          elapsedMs?: number;
          promptTokens?: number;
          completionTokens?: number;
        }>("generate-text-done", onDone)
      );
      unlisteners.push(await listen<{ id: string; error: string }>("generate-text-error", onError));
      if (agent?.enabled) {
        unlisteners.push(
          await listen<{
            id: string;
            tool: string;
            status: string;
            summary: string;
            detail: string;
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
              };
              if (last && last.tool === step.tool && last.status === "running" && last.summary === step.summary) {
                return [...prev.slice(0, -1), step];
              }
              return [...prev, step];
            });
          }),
        );
        unlisteners.push(
          await listen<{
            id: string;
            callId: string;
            tool: string;
            summary: string;
            preview: string;
          }>("agent-approval", (event) => {
            const payload = event.payload;
            if (!payload || payload.id !== reqId || streamIdRef.current !== myGen) return;
            setAgentApproval({
              callId: payload.callId,
              tool: payload.tool,
              summary: payload.summary,
              preview: payload.preview || "",
            });
          }),
        );
      }

      const maxTokens = Math.max(selectedModel.maxTokens || 0, 1536);
      const temperature =
        typeof profileTemperature === "number" && Number.isFinite(profileTemperature)
          ? Math.min(2, Math.max(0, profileTemperature))
          : selectedModel.temperature;
      const config = { ...selectedModel, maxTokens, temperature };
      if (stoppedEarly()) return;
      if (agent?.enabled) {
        await invoke("agent_run", {
          req: {
            config,
            system,
            user,
            requestId: reqId,
            repoPath: agent.repoPath,
            history: requestHistory(baseHistory),
            apps: agent.apps,
          },
        });
      } else {
        await invoke("generate_text_stream", {
          req: {
            config,
            system,
            user,
            requestId: reqId,
            history: requestHistory(baseHistory),
          },
        });
      }
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
        if (stopped && full.trim()) finishTurn(full);
        else failTurn(!stopped);
        setBusySync(false);
      }
      activeReqRef.current = null;
      teardown();
    }
  };

  /// Abort the in-flight stream. The backend answers with an
  /// AI_REQUEST_CANCELLED error event (handled as a normal stop) and the partial
  /// answer stays on screen.
  const stopStream = async () => {
    const id = activeReqRef.current;
    if (!id) return;
    stopRequestedRef.current = true;
    setStopping(true);
    try {
      await invoke("cancel_request", { id });
    } catch (e) {
      setStopping(false);
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
    lastModeRef.current = null;
    lastTaskRef.current = "none";
    lastDailyBriefRef.current = "";
    lastDailyReposRef.current = [];
    rememberSteps([]);
    setAgentApproval(null);
    setStopping(false);
  }, [setSelectedText]);

  /** Point the chat state at a restored session's turns. */
  const loadSession = useCallback((turns: QaTurn[]) => {
    const compacted = compactSession(turns);
    setHistory(compacted);
    setSource(null);
    lastModeRef.current = null;
    lastTaskRef.current = "none";
    lastDailyBriefRef.current = "";
    lastDailyReposRef.current = [];
    rememberSteps([]);
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

  /** Replay the last question with the same history snapshot and task. */
  const regenerate = () =>
    send(lastQuestionRef.current, lastHistoryRef.current, {
      task: lastTaskRef.current,
      mode: lastModeRef.current ?? "chat",
    });

  /** Resend the request that failed; `asChat` drops the project context. */
  const retry = (asChat = false) => {
    const f = failedRef.current;
    if (!f) return;
    // A failed send put the question back into the input; the retry owns it now.
    setInput((prev) => (prev.trim() === f.question ? "" : prev));
    void send(f.question, f.history, { task: f.task, mode: asChat ? "chat" : f.mode });
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

  const decideAgent = async (allow: boolean) => {
    const id = activeReqRef.current;
    const callId = agentApproval?.callId;
    if (!id || !callId) return;
    setAgentApproval(null);
    try {
      await invoke("agent_decide", { requestId: id, callId, allow });
    } catch (e) {
      setError(String(e));
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
    agentApproval,
    stopping,
    decideAgent,
    resetChat,
    loadSession,
  };
}
