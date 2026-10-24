import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Download, ListChecks, Pencil, Search, Trash2, X } from "lucide-react";
import type { AiHubSession } from "../../core/types";

type Props = {
  sessions: AiHubSession[];
  activeSessionId: string | null;
  /** While a round is streaming: deleting the ACTIVE session is blocked (its
      in-flight finishTurn would re-persist the just-deleted chat as a new session). */
  busy: boolean;
  renamingId: string | null;
  renameDraft: string;
  setRenameDraft: (v: string) => void;
  onStartRename: (s: AiHubSession) => void;
  onCommitRename: (id: string, title: string) => void;
  onCancelRename: () => void;
  onOpen: (s: AiHubSession) => void;
  /** Whole session, so the shell can name it in the delete confirmation. */
  onRemove: (s: AiHubSession) => void;
  /** 批量删除：面板只负责挑，确认框与落库由外壳统一处理。 */
  onRemoveMany: (list: AiHubSession[]) => void;
  onExportAll: () => void;
  /** Hard cap kept by SQLite; the panel says so before a save silently drops one. */
  maxSessions: number;
  relativeTime: (iso: string) => string;
};

function turnCount(session: AiHubSession): number {
  return Math.ceil((session.turns?.length ?? 0) / 2);
}

/** Persisted-session panel with inline rename + batch delete. */
export default function SessionHistory(props: Props) {
  const { t } = useTranslation("aihub");
  const {
    sessions, activeSessionId, busy, renamingId, renameDraft, setRenameDraft,
    onStartRename, onCommitRename, onCancelRename, onOpen, onRemove, onRemoveMany,
    onExportAll, maxSessions, relativeTime,
  } = props;
  const [query, setQuery] = useState("");
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<string[]>([]);

  const q = query.trim().toLowerCase();
  const visible = useMemo(() => {
    if (!q) return sessions;
    return sessions.filter((s) => {
      if ((s.title || "").toLowerCase().includes(q)) return true;
      return (s.turns || []).some((turn) => turn.content.toLowerCase().includes(q));
    });
  }, [sessions, q]);

  // 只认当前列表里还看得见的选中项：搜索过滤掉的那些不该被顺手删掉
  const chosen = useMemo(() => visible.filter((s) => selected.includes(s.id)), [visible, selected]);
  const allChosen = visible.length > 0 && chosen.length === visible.length;

  const exitSelect = () => {
    setSelectMode(false);
    setSelected([]);
  };

  const toggleOne = (id: string) => {
    setSelected((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  };

  const toggleAll = () => setSelected(allChosen ? [] : visible.map((s) => s.id));

  return (
    <div className="ai-hub-sessions">
      <div className="qa-sessions-head">
        <span>
          {selectMode ? t("historyBatch") : t("history")}
          <span className="qa-sessions-count">{`${sessions.length}/${maxSessions}`}</span>
        </span>
        <div className="qa-sessions-head-actions">
          {!selectMode && (
            <button
              type="button"
              className="qa-mini-btn"
              onClick={onExportAll}
              disabled={sessions.length === 0}
              title={t("exportAllTip")}
            >
              <Download size={12} /> {t("exportAll")}
            </button>
          )}
          <button
            type="button"
            className="qa-mini-btn"
            onClick={() => (selectMode ? exitSelect() : setSelectMode(true))}
            disabled={sessions.length === 0}
            title={selectMode ? t("historyBatchExit") : t("historyBatchTip")}
          >
            {selectMode ? <X size={12} /> : <Trash2 size={12} />}
            {selectMode ? t("historyBatchExit") : t("historyBatch")}
          </button>
        </div>
      </div>
      {selectMode && (
        <div className="qa-batch-bar">
          <span className="qa-batch-count">{t("historySelected", { n: chosen.length })}</span>
          <button
            type="button"
            className="qa-mini-btn"
            onClick={toggleAll}
            disabled={visible.length === 0}
            title={allChosen ? t("historySelectNone") : t("historySelectAll")}
          >
            <ListChecks size={12} />
            {allChosen ? t("historySelectNone") : t("historySelectAll")}
          </button>
          <button
            type="button"
            className="qa-mini-btn is-danger"
            disabled={chosen.length === 0}
            onClick={() => onRemoveMany(chosen)}
            title={t("historyBatchTip")}
          >
            <Trash2 size={12} />
            {t("deleteConfirm")}
          </button>
        </div>
      )}
      <label className="qa-sessions-filter">
        <Search size={13} className="qa-filter-icon" />
        <input
          className="qa-filter-input"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.nativeEvent.isComposing || e.keyCode === 229) return;
            if (e.key === "Escape") {
              e.stopPropagation();
              if (selectMode) exitSelect();
              else setQuery("");
            }
          }}
          placeholder={t("historySearch")}
          autoFocus
        />
        {query && (
          <button
            type="button"
            className="qa-filter-clear"
            onClick={() => setQuery("")}
            title={t("historyClear")}
            aria-label={t("historyClear")}
          >
            <X size={11} />
          </button>
        )}
      </label>
      <div className="qa-session-list">
        {visible.map((s) => {
          const picked = selected.includes(s.id);
          return (
            <div
              key={s.id}
              className={`qa-session-item${s.id === activeSessionId ? " active" : ""}${
                picked ? " is-selected" : ""
              }`}
            >
              {selectMode && (
                <span className={`qa-session-check${picked ? " is-on" : ""}`} aria-hidden="true">
                  {picked && <Check size={10} />}
                </span>
              )}
              {renamingId === s.id && !selectMode ? (
                <div className="qa-session-rename">
                  <input
                    className="qa-rename-input"
                    value={renameDraft}
                    autoFocus
                    onChange={(e) => setRenameDraft(e.target.value)}
                    onKeyDown={(e) => {
                      // 输入法选词中的 Enter / ↑↓ 不算提交
                      if (e.nativeEvent.isComposing || e.keyCode === 229) return;
                      if (e.key === "Enter") onCommitRename(s.id, renameDraft);
                      // Esc 只退出重命名：不冒泡到全局链（那里会把整个历史面板关掉）
                      if (e.key === "Escape") {
                        e.stopPropagation();
                        onCancelRename();
                      }
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
                <button
                  type="button"
                  className="qa-session-main"
                  onClick={() => (selectMode ? toggleOne(s.id) : onOpen(s))}
                >
                  <span className="qa-session-title">{s.title || t("newChat")}</span>
                  <span className="qa-session-meta">
                    {t("turnsCount", { n: turnCount(s) })} · {relativeTime(s.updatedAt)}
                  </span>
                </button>
              )}
              {!selectMode && renamingId !== s.id && (
                <div className="qa-session-actions">
                  <button
                    type="button"
                    className="btn-icon"
                    onClick={() => onStartRename(s)}
                    title={t("renameSession")}
                  >
                    <Pencil size={13} />
                  </button>
                  <button
                    type="button"
                    className="btn-icon"
                    onClick={() => onRemove(s)}
                    disabled={busy && s.id === activeSessionId}
                    title={t("deleteSession")}
                  >
                    <Trash2 size={13} />
                  </button>
                </div>
              )}
            </div>
          );
        })}
        {sessions.length === 0 && <span className="ai-hub-hint">{t("historyEmpty")}</span>}
        {sessions.length > 0 && visible.length === 0 && (
          <span className="ai-hub-hint">{t("historyNoMatch")}</span>
        )}
        {sessions.length >= maxSessions && (
          <span className="ai-hub-hint">{t("historyAtCap", { n: maxSessions })}</span>
        )}
      </div>
    </div>
  );
}
