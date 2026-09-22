import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AIModelConfig, QuickAskTurn } from "../../core/types";
import { buildPromptOptimizeMessages } from "../../lib/promptOptimize";
import { MAX_HISTORY_TURNS, MAX_TURN_CHARS, TASK_SYSTEM } from "./config";
import type { OptimizeConfig, TaskKind } from "./config";

type QaTurn = QuickAskTurn;

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

export type AskStreamOptions = {
  selectedModel: AIModelConfig | undefined;
  task: TaskKind;
  /** Scenario/goals for the `optimize` task (ignored otherwise). */
  optimize: OptimizeConfig;
  input: string;
  setInput: (updater: string | ((prev: string) => string)) => void;
  setSelectedText: (text: string) => void;
  getContext: () => Promise<{ text: string; truncated: boolean }>;
  /** Persist the completed exchange as a session (wired to useAskSessions). */
  persistTurns: (turns: QaTurn[], firstQuestion: string) => void;
};

/**
 * The streaming Q&A pipeline: send / stop / regenerate over
 * `generate_text_stream` + generate-text-chunk/-done/-error events.
 */
export function useAskStream(opts: AskStreamOptions) {
  const { t } = useTranslation("quickask");
  const { selectedModel, task, optimize, input, setInput, setSelectedText, getContext, persistTurns } = opts;

  const [answer, setAnswer] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Previous Q&A pairs sent along as multi-turn context (follow-up questions). */
  const [history, setHistory] = useState<QaTurn[]>([]);
  /** Last question + the history snapshot it was sent with, for regenerate. */
  const lastQuestionRef = useRef<string>("");
  const lastHistoryRef = useRef<QaTurn[]>([]);
  const streamIdRef = useRef(0);
  /** Request id of the stream currently in flight, so it can be cancelled. */
  const activeReqRef = useRef<string | null>(null);

  /** Persist (or create) the active session after a completed exchange. */
  const persistRef = useRef(persistTurns);
  persistRef.current = persistTurns;

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
      let system: string;
      let user: string;
      if (task === "optimize") {
        // Prompt rewriting is self-contained: the draft IS the request, no
        // workbench context needed.
        const built = buildPromptOptimizeMessages(optimize.scenario, optimize.goals, question);
        system = built.system;
        user = built.user;
      } else {
        const ctxResult = await getContext();
        system = TASK_SYSTEM[task];
        const ctx = ctxResult.text;
        user = ctx
          ? `${question}\n\n---\nContext from workbench:\n${ctx}`
          : question;
      }

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
            persistRef.current(nextTurns, question);
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
            persistRef.current(nextTurns, question);
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
    setAnswer("");
    setError(null);
    setSelectedText("");
    lastQuestionRef.current = "";
    lastHistoryRef.current = [];
  }, [setSelectedText]);

  /** Point the chat state at a restored session's turns. */
  const loadSession = useCallback((turns: QaTurn[]) => {
    setHistory(turns);
    const lastAnswer = [...turns].reverse().find((t) => t.role === "assistant");
    const lastUser = [...turns].reverse().find((t) => t.role === "user");
    setAnswer(lastAnswer?.content ?? "");
    setError(null);
    setSelectedText("");
    // Point regenerate at the final exchange of this session.
    lastQuestionRef.current = lastUser?.content ?? "";
    lastHistoryRef.current = turns.length >= 2 ? turns.slice(0, -2) : [];
  }, [setSelectedText]);

  return {
    answer,
    busy,
    error,
    setError,
    history,
    lastQuestionRef,
    lastHistoryRef,
    send,
    stopStream,
    resetChat,
    loadSession,
  };
}
