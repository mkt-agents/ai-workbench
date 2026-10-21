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
  /** Tell the user when something was dropped (cap eviction), never silently. */
  onNotice: (msg: string) => void;
  /** Called when the currently-open session is deleted. */
  onRemovedActive: () => void;
};

/** Persistence of ai-hub chat sessions (SQLite `ai_hub_sessions`). */
export function useAskSessions(opts: AskSessionsOptions) {
  const { t } = useTranslation("aihub");
  const { task, activeSessionId, setActiveSessionId, setError, onNotice, onRemovedActive } = opts;
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
      const evicted = await upsertAiHubSession(session);
      if (evicted.length > 0) {
        onNotice(t("historyEvicted", { names: evicted.slice(0, 3).join("、"), n: evicted.length }));
      }
    } catch (e) {
      setError(String(e));
    }
  }, [task, activeSessionId, setActiveSessionId, setError, onNotice, upsertAiHubSession, t]);

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

  /** Transcript → Markdown: steps and cost belong in the archive, not just the prose. */
  const turnsToMarkdown = useCallback((title: string, turns: AiHubTurn[]): string => {
    const lines: string[] = [`# ${title}`, ""];
    turns.forEach((turn) => {
      if (turn.digest) {
        lines.push(`## ${t("digestLabel")}`, "", turn.content, "");
        return;
      }
      lines.push(turn.role === "user" ? `## 🧑 You` : `## 🤖 Assistant`, "");
      lines.push(turn.content);
      if (turn.steps?.length) {
        lines.push("", `**${t("agent.steps", { n: turn.steps.length })}**`);
        for (const s of turn.steps) {
          const mark = s.status === "error" ? " ⚠" : s.status === "denied" ? " ✕" : "";
          lines.push(`- \`${s.tool}\` ${s.summary}${mark}`);
        }
      }
      if (turn.role === "assistant") {
        const cost: string[] = [];
        if (turn.elapsedMs != null) {
          cost.push(
            turn.elapsedMs >= 1000
              ? t("source.elapsedSec", { n: (turn.elapsedMs / 1000).toFixed(1) })
              : t("source.elapsedMs", { n: turn.elapsedMs }),
          );
        }
        if (turn.promptTokens != null || turn.completionTokens != null) {
          const p = turn.promptTokens ?? 0;
          const c = turn.completionTokens ?? 0;
          cost.push(t("source.tokens", { n: p + c, in: p, out: c }));
        }
        if (cost.length) lines.push("", `> ${cost.join(" · ")}`);
      }
      lines.push("");
    });
    return lines.join("\n");
  }, [t]);

  /** Export the active session as a Markdown file. */
  const exportActiveSession = useCallback(async (history: AiHubTurn[]) => {
    if (history.length === 0) return;
    const title = activeSessionId
      ? aiHubSessions.find((s) => s.id === activeSessionId)?.title || "ai-hub"
      : "ai-hub";
    const filename = `${title.replace(/[\\/:*?"<>|]/g, "_") || "ai-hub"}.md`;
    try {
      await invokeSaveTextFile(turnsToMarkdown(title, history), filename, t("exportSession"));
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, [activeSessionId, aiHubSessions, invokeSaveTextFile, setError, t, turnsToMarkdown]);

  /** Export every stored conversation into one Markdown file, newest first. */
  const exportAllSessions = useCallback(async () => {
    if (aiHubSessions.length === 0) return;
    const doc = aiHubSessions
      .map((s) => turnsToMarkdown(s.title || t("newChat"), s.turns || []))
      .join("\n\n---\n\n");
    const day = new Date().toISOString().slice(0, 10);
    try {
      await invokeSaveTextFile(doc, `ai-hub-${day}.md`, t("exportAll"));
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, [aiHubSessions, invokeSaveTextFile, setError, t, turnsToMarkdown]);

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

  return { aiHubSessions, persistTurns, removeSession, renameSession, exportActiveSession, exportAllSessions, relativeTime };
}
