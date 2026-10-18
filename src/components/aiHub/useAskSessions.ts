import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalStore } from "../../core/store";
import type { AiHubSession, AiHubTurn } from "../../core/types";
import { makeTitle } from "./config";

export type AskSessionsOptions = {
  /** Encoded task string (see config.encodeTask), stored verbatim per session. */
  task: string;
  activeSessionId: string | null;
  setActiveSessionId: (id: string | null) => void;
  setError: (msg: string | null) => void;
  /** Called when the currently-open session is deleted. */
  onRemovedActive: () => void;
};

/** Persistence of ai-hub chat sessions (SQLite `ai_hub_sessions`). */
export function useAskSessions(opts: AskSessionsOptions) {
  const { t } = useTranslation("aihub");
  const { task, activeSessionId, setActiveSessionId, setError, onRemovedActive } = opts;
  const aiHubSessions = useGlobalStore((s) => s.aiHubSessions);
  const upsertAiHubSession = useGlobalStore((s) => s.upsertAiHubSession);
  const deleteAiHubSession = useGlobalStore((s) => s.deleteAiHubSession);
  const loadAiHubSessions = useGlobalStore((s) => s.loadAiHubSessions);
  const invokeSaveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);

  /** Persist (or create) the active session after a completed exchange. */
  const persistTurns = useCallback(async (turns: AiHubTurn[], firstQuestion: string) => {
    const existing = activeSessionId
      ? useGlobalStore.getState().aiHubSessions.find((s) => s.id === activeSessionId)
      : undefined;
    const now = new Date().toISOString();
    const session: AiHubSession = existing
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
      await upsertAiHubSession(session);
    } catch (e) {
      setError(String(e));
    }
  }, [task, activeSessionId, setActiveSessionId, setError, upsertAiHubSession, t]);

  const removeSession = useCallback(async (id: string) => {
    try {
      await deleteAiHubSession(id);
      if (activeSessionId === id) {
        setActiveSessionId(null);
        onRemovedActive();
      }
    } catch (e) {
      setError(String(e));
    }
  }, [activeSessionId, deleteAiHubSession, setActiveSessionId, onRemovedActive, setError]);

  /** Persist a new title for a session. Returns whether the rename was stored. */
  const renameSession = useCallback(async (id: string, title: string): Promise<boolean> => {
    const trimmed = title.trim();
    if (!trimmed) return false;
    const target = aiHubSessions.find((s) => s.id === id);
    if (!target) return false;
    try {
      const now = new Date().toISOString();
      await upsertAiHubSession({ ...target, title: trimmed, updatedAt: now });
      await loadAiHubSessions();
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, [aiHubSessions, upsertAiHubSession, loadAiHubSessions, setError]);

  /** Export the active session as a Markdown file. */
  const exportActiveSession = useCallback(async (history: AiHubTurn[]) => {
    if (history.length === 0) return;
    const title = activeSessionId
      ? aiHubSessions.find((s) => s.id === activeSessionId)?.title || "ai-hub"
      : "ai-hub";
    const lines = [`# ${title}`, ""];
    history.forEach((turn) => {
      if (turn.digest) {
        lines.push(`## ${t("digestLabel")}`);
        lines.push("");
        lines.push(turn.content);
        lines.push("");
        return;
      }
      lines.push(turn.role === "user" ? `## 🧑 You` : `## 🤖 Assistant`);
      lines.push("");
      lines.push(turn.content);
      lines.push("");
    });
    const filename = `${title.replace(/[\\/:*?"<>|]/g, "_") || "ai-hub"}.md`;
    try {
      await invokeSaveTextFile(lines.join("\n"), filename, t("exportSession"));
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, [activeSessionId, aiHubSessions, invokeSaveTextFile, setError, t]);

  /** Relative "n minutes ago" label for the session list. */
  const relativeTime = useCallback((iso: string): string => {
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
  }, [t]);

  return { aiHubSessions, persistTurns, removeSession, renameSession, exportActiveSession, relativeTime };
}
