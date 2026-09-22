import { useTranslation } from "react-i18next";
import { Check, FileText, MessageSquarePlus, Trash2, X } from "lucide-react";
import type { QuickAskSession } from "../../core/types";

type Props = {
  sessions: QuickAskSession[];
  activeSessionId: string | null;
  busy: boolean;
  renamingId: string | null;
  renameDraft: string;
  setRenameDraft: (v: string) => void;
  onStartRename: (s: QuickAskSession) => void;
  onCommitRename: (id: string, title: string) => void;
  onCancelRename: () => void;
  onOpen: (s: QuickAskSession) => void;
  onRemove: (id: string) => void;
  onNewChat: () => void;
  relativeTime: (iso: string) => string;
};

/** Persisted-session panel with inline rename. */
export default function SessionHistory(props: Props) {
  const { t } = useTranslation("quickask");
  const {
    sessions, activeSessionId, busy, renamingId, renameDraft, setRenameDraft,
    onStartRename, onCommitRename, onCancelRename, onOpen, onRemove, onNewChat, relativeTime,
  } = props;
  return (
    <div className="quick-ask-sessions">
      <div className="qa-sessions-head">
        <span>{t("history")}</span>
        <button type="button" className="btn btn-secondary btn-small" onClick={onNewChat} disabled={busy}>
          <MessageSquarePlus size={13} /> {t("newChat")}
        </button>
      </div>
      <div className="qa-session-list">
        {sessions.map((s) => (
          <div
            key={s.id}
            className={`qa-session-item${s.id === activeSessionId ? " active" : ""}`}
          >
            {renamingId === s.id ? (
              <div className="qa-session-rename">
                <input
                  className="qa-rename-input"
                  value={renameDraft}
                  autoFocus
                  onChange={(e) => setRenameDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") onCommitRename(s.id, renameDraft);
                    if (e.key === "Escape") onCancelRename();
                  }}
                />
                <button
                  type="button"
                  className="btn-icon"
                  onClick={() => onCommitRename(s.id, renameDraft)}
                  title={t("renameSave")}
                >
                  <Check size={13} />
                </button>
                <button
                  type="button"
                  className="btn-icon"
                  onClick={onCancelRename}
                  title={t("renameCancel")}
                >
                  <X size={13} />
                </button>
              </div>
            ) : (
              <button type="button" className="qa-session-main" onClick={() => onOpen(s)}>
                <span className="qa-session-title">{s.title || t("newChat")}</span>
                <span className="qa-session-meta">
                  {t("turnsCount", { n: Math.ceil((s.turns?.length ?? 0) / 2) })} · {relativeTime(s.updatedAt)}
                </span>
              </button>
            )}
            {renamingId !== s.id && (
              <div className="qa-session-actions">
                <button
                  type="button"
                  className="btn-icon"
                  onClick={() => onStartRename(s)}
                  title={t("renameSession")}
                >
                  <FileText size={13} />
                </button>
                <button
                  type="button"
                  className="btn-icon"
                  onClick={() => onRemove(s.id)}
                  title={t("deleteSession")}
                >
                  <Trash2 size={13} />
                </button>
              </div>
            )}
          </div>
        ))}
        {sessions.length === 0 && (
          <span className="quick-ask-hint">{t("historyEmpty")}</span>
        )}
      </div>
    </div>
  );
}
