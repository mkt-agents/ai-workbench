import { editAiHubSession, pageAiHubSession } from "../../core/store/invocations";
import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalStore } from "../../core/store";
import type { AiHubTurn } from "../../core/types";
import { makeTitle } from "./config";

export type AskSessionsOptions = {
  activeSessionId: string | null;
  setActiveSessionId: (id: string | null) => void;
  setError: (msg: string | null) => void;
  /** Tell the user when something was dropped (cap eviction), never silently. */
  onNotice: (msg: string) => void;
  /** Called when the currently-open session is deleted. */
  onRemovedActive: () => void;
  flushTranscript: (id: string) => Promise<void>;
};

/** Persistence of ai-hub chat sessions (SQLite `ai_hub_sessions`). */
export function useAskSessions(opts: AskSessionsOptions) {
  const { t } = useTranslation("aihub");
  const { activeSessionId, setActiveSessionId, setError, onNotice, onRemovedActive, flushTranscript } = opts;
  const aiHubSessions = useGlobalStore((s) => s.aiHubSessions);
  const saveAiHubSession = useGlobalStore((s) => s.saveAiHubSession);
  const deleteAiHubSession = useGlobalStore((s) => s.deleteAiHubSession);
  const deleteAiHubSessions = useGlobalStore((s) => s.deleteAiHubSessions);
  const loadAiHubSessions = useGlobalStore((s) => s.loadAiHubSessions);
  const invokeSaveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);

  const ensureSession = useCallback(async (firstQuestion: string) => {
    const id = `qas-${crypto.randomUUID()}`;
    const evicted = await saveAiHubSession(id, makeTitle(firstQuestion, t("newChat")), true);
    if (evicted.length) onNotice(t("historyEvicted", { names: evicted.slice(0, 3).join("、"), n: evicted.length }));
    return id;
  }, [onNotice, t, saveAiHubSession]);

  const persistTurns = useCallback(async (id: string, revision: number, from: number, turns: AiHubTurn[]) => {
    const nextRevision = await editAiHubSession(id, revision, from, turns);
    // Reads use the current SQLite state, so a late refresh cannot write old transcripts back.
    void loadAiHubSessions().catch((error) => setError(String(error)));
    return nextRevision;
  }, [loadAiHubSessions, setError]);

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

  /** 批量删除：整表只写一次；当前打开的那个也在里面时一并关掉（否则它会被下一次提问重新落库）。 */
  const removeSessions = useCallback(async (ids: string[]) => {
    if (ids.length === 0) return;
    try {
      await deleteAiHubSessions(ids);
      if (activeSessionId && ids.includes(activeSessionId)) {
        setActiveSessionId(null);
        onRemovedActive();
      }
    } catch (e) {
      setError(String(e));
    }
  }, [activeSessionId, deleteAiHubSessions, setActiveSessionId, onRemovedActive, setError]);

  /** Persist a new title for a session. Returns whether the rename was stored. */
  const renameSession = useCallback(async (id: string, title: string): Promise<boolean> => {
    const trimmed = title.trim();
    if (!trimmed) return false;
    const target = aiHubSessions.find((s) => s.id === id);
    if (!target) return false;
    try {
      await saveAiHubSession(id, trimmed, false);
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  }, [aiHubSessions, saveAiHubSession, setError]);

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

  /** Read a consistent transcript by revision, including pages not mounted in the thread. */
  const exportTurns = async (id: string): Promise<AiHubTurn[]> => {
    await flushTranscript(id);
    const tail = await pageAiHubSession(id);
    const chunks = [tail.turns];
    let start = tail.start;
    while (start > 0) {
      const older = await pageAiHubSession(id, start);
      if (older.revision !== tail.revision) throw new Error(t("context.historyChanged"));
      chunks.push(older.turns);
      start = older.start;
    }
    const turns = chunks.reverse().flat();
    if (tail.legacy) turns.unshift({ role: "assistant", content: `${t("context.legacy")}\n\n${tail.legacy}`, digest: true });
    return turns;
  };

  const exportActiveSession = async (history: AiHubTurn[]) => {
    const title = aiHubSessions.find((s) => s.id === activeSessionId)?.title || "ai-hub";
    const filename = `${title.replace(/[\\/:*?"<>|]+/g, "_").trim() || "ai-hub"}.md`;
    try {
      const turns = activeSessionId ? await exportTurns(activeSessionId) : history;
      if (!turns.length) return false;
      await invokeSaveTextFile(turnsToMarkdown(title, turns), filename, t("exportSession"));
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  };

  const exportAllSessions = async () => {
    if (!aiHubSessions.length) return;
    try {
      const sections: string[] = [];
      for (const session of aiHubSessions) {
        sections.push(turnsToMarkdown(session.title || t("newChat"), await exportTurns(session.id)));
      }
      const day = new Date().toISOString().slice(0, 10);
      await invokeSaveTextFile(sections.join("\n\n---\n\n"), `ai-hub-${day}.md`, t("exportAll"));
    } catch (e) { setError(String(e)); }
  };

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

  return {
    aiHubSessions,
    ensureSession,
    persistTurns,
    removeSession,
    removeSessions,
    renameSession,
    exportActiveSession,
    exportAllSessions,
    relativeTime,
  };
}
