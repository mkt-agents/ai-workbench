import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AIModelConfig, QuickAskTurn } from "../../core/types";
import { buildPromptOptimizeMessages } from "../../lib/promptOptimize";
import { recordAiUsage } from "../../lib/aiUsage";
import { formatAttachmentsBlock, type AskAttachment } from "../../lib/askAttachments";
import { MAX_HISTORY_TURNS, MAX_SAVED_TURNS, MAX_TURN_CHARS, buildMeta, buildSystem } from "./config";
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

function clipTurn(text: string): string {
  return text.length > MAX_TURN_CHARS
    ? text.slice(0, MAX_TURN_CHARS) + "\n…(truncated)"
    : text;
}

/** Full transcript (displayed + persisted), bounded so the session row stays small. */
function appendTurn(prev: QaTurn[], question: string, answer: string): QaTurn[] {
  const next: QaTurn[] = [
    ...prev,
    { role: "user", content: question },
    { role: "assistant", content: answer },
  ];
  return next.slice(-MAX_SAVED_TURNS * 2);
}

/** Only the most recent rounds go to the model, each clipped. */
function requestHistory(turns: QaTurn[]): QaTurn[] {
  return turns.slice(-MAX_HISTORY_TURNS * 2).map((t) => ({ ...t, content: clipTurn(t.content) }));
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
};

/**
 * The streaming Q&A pipeline: send / stop / regenerate over
 * `generate_text_stream` + generate-text-chunk/-done/-error events.
 */
export function useAskStream(opts: AskStreamOptions) {
  const { t } = useTranslation("quickask");
  const {
    selectedModel, task, optimize, input, setInput, setSelectedText, getContext,
    routeOverride, onSent, persistTurns,
    profileSystemRole, profileTemperature, attachments = [],
  } = opts;

  const [answer, setAnswer] = useState("");
  const [source, setSource] = useState<AnswerSource | null>(null);
  /** Mode of the previous exchange, so short follow-ups stay on the project. */
  const lastModeRef = useRef<AskMode | null>(null);
  /** Task the last question was sent with, so regenerate replays it. */
  const lastTaskRef = useRef<TaskKind>("none");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Full transcript of this chat; the last MAX_HISTORY_TURNS pairs ride along as context. */
  const [history, setHistory] = useState<QaTurn[]>([]);
  /** Question currently being answered (shown above the streaming answer). */
  const [pending, setPending] = useState<{ question: string } | null>(null);
  /** Where the in-flight request is: collecting context → waiting for the first token → streaming. */
  const [phase, setPhase] = useState<AskPhase>("idle");
  /** Last request that failed (not stopped), so the error card can offer a retry. */
  const failedRef = useRef<{ question: string; history: QaTurn[]; task: TaskKind; mode: AskMode } | null>(null);
  const [failedMode, setFailedMode] = useState<AskMode | null>(null);
  /** Last question + the history snapshot it was sent with, for regenerate. */
  const lastQuestionRef = useRef<string>("");
  const lastHistoryRef = useRef<QaTurn[]>([]);
  const streamIdRef = useRef(0);
  /** Request id of the stream currently in flight, so it can be cancelled. */
  const activeReqRef = useRef<string | null>(null);

  /** Persist (or create) the active session after a completed exchange. */
  const persistRef = useRef(persistTurns);
  persistRef.current = persistTurns;

  /** `replay` (regenerate) pins the task and mode of the original question. */
  const send = async (
    rawInput?: string,
    historyOverride?: QaTurn[],
    replay?: { task: TaskKind; mode: AskMode },
  ) => {
    if (busy) return;
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
    // The context dump is per-request; history carries only the plain Q&A pairs.
    const baseHistory = historyOverride ?? history;
    // Regenerate replaces the last pair: hide it now, restore it if the retry fails.
    const shownBefore = history;
    if (historyOverride) setHistory(baseHistory);
    const reqId = `qa-${Date.now()}-${++streamIdRef.current}`;
    const myGen = streamIdRef.current;
    activeReqRef.current = reqId;
    setBusy(true);
    setError(null);
    setAnswer("");
    setSelectedText("");
    setPending({ question });
    setPhase("waiting");
    failedRef.current = null;
    setFailedMode(null);
    if (!rawInput) setInput("");
    let sentMode: AskMode = replay?.mode ?? "chat";
    const finishTurn = (text: string) => {
      const nextTurns = appendTurn(baseHistory, question, text);
      setHistory(nextTurns);
      setPending(null);
      setPhase("idle");
      setAnswer("");
      lastQuestionRef.current = question;
      lastHistoryRef.current = baseHistory;
      persistRef.current(nextTurns, question);
    };
    /** `failed` = a real error (offer retry); false = stopped / empty answer. */
    const failTurn = (failed: boolean) => {
      setPending(null);
      setPhase("idle");
      setAnswer("");
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
        const ctxResult = await getContext(route.mode);
        summary = ctxResult.summary;
        const hasProject = summary.parts.some((p) => p !== "clipboard");
        system = buildSystem(selectedModel, route.mode, sendTask, hasProject);
        if (profileSystemRole?.trim()) {
          system += `\n\n[Profile] ${profileSystemRole.trim()}`;
        }
        const ctx = ctxResult.text;
        user = ctx
          ? `${question}\n\n---\nContext from workbench:\n${ctx}`
          : question;
        user += formatAttachmentsBlock(attachments);
      }
      if (streamIdRef.current === myGen) setPhase("waiting");
      setSource({ mode: route.mode, summary, modelName: selectedModel.name, task: sendTask });
      lastModeRef.current = route.mode;
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
          setBusy(false);
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
          setBusy(false);
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

      const maxTokens = Math.max(selectedModel.maxTokens || 0, 1536);
      const temperature =
        typeof profileTemperature === "number" && Number.isFinite(profileTemperature)
          ? Math.min(2, Math.max(0, profileTemperature))
          : selectedModel.temperature;
      await invoke("generate_text_stream", {
        req: {
          config: { ...selectedModel, maxTokens, temperature },
          system,
          user,
          requestId: reqId,
          history: requestHistory(baseHistory),
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
        if (stopped && full.trim()) finishTurn(full);
        else failTurn(!stopped);
        setBusy(false);
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
  }, [setSelectedText]);

  /** Point the chat state at a restored session's turns. */
  const loadSession = useCallback((turns: QaTurn[]) => {
    setHistory(turns);
    setSource(null);
    lastModeRef.current = null;
    lastTaskRef.current = "none";
    const lastUser = [...turns].reverse().find((t) => t.role === "user");
    setAnswer("");
    setPending(null);
    setError(null);
    failedRef.current = null;
    setFailedMode(null);
    setSelectedText("");
    // Point regenerate at the final exchange of this session.
    lastQuestionRef.current = lastUser?.content ?? "";
    lastHistoryRef.current = turns.length >= 2 ? turns.slice(0, -2) : [];
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
    regenerate,
    stopStream,
    resetChat,
    loadSession,
  };
}
