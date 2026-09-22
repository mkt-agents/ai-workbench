import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalStore } from "../../core/store";
import type { QuickAskSession, QuickAskTurn } from "../../core/types";
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

/** Persistence of quick-ask chat sessions (SQLite `quick_ask_sessions`). */
export function useAskSessions(opts: AskSessionsOptions) {
  const { t } = useTranslation("quickask");
  const { task, activeSessionId, setActiveSessionId, setError, onRemovedActive } = opts;
  const quickAskSessions = useGlobalStore((s) => s.quickAskSessions);
  const upsertQuickAskSession = useGlobalStore((s) => s.upsertQuickAskSession);
  const deleteQuickAskSession = useGlobalStore((s) => s.deleteQuickAskSession);
  const loadQuickAskSessions = useGlobalStore((s) => s.loadQuickAskSessions);
  const invokeSaveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);

  /** Persist (or create) the active session after a completed exchange. */
  const persistTurns = useCallback(async (turns: QuickAskTurn[], firstQuestion: string) => {
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
  }, [task, activeSessionId, setActiveSessionId, setError, upsertQuickAskSession, t]);

  const removeSession = useCallback(async (id: string) => {
    try {
      await deleteQuickAskSession(id);
      if (activeSessionId === id) {
        setActiveSessionId(null);
        onRemovedActive();
      }
    } catch (e) {
      setError(String(e));
    }
  }, [activeSessionId, deleteQuickAskSession, setActiveSessionId, onRemovedActive, setError]);

  /** Persist a new title for a session. Returns whether the rename was stored. */
  const renameSession = useCallback(async (id: string, title: string): Promise<boolean> => {
    const trimmed = title.trim();
    if (!trimmed) return false;
    const target = quickAskSessions.find((s) => s.id === id);
    if (!target) return false;
    try {
      const now = new Date().toISOString();
      await upsertQuickAskSession({ ...target, title: trimmed, updatedAt: now });
      await loadQuickAskSessions();
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, [quickAskSessions, upsertQuickAskSession, loadQuickAskSessions, setError]);

  /** Export the active session as a Markdown file. */
  const exportActiveSession = useCallback(async (history: QuickAskTurn[]) => {
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
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, [activeSessionId, quickAskSessions, invokeSaveTextFile, setError, t]);

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

  return { quickAskSessions, persistTurns, removeSession, renameSession, exportActiveSession, relativeTime };
}
