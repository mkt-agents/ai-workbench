import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  Check,
  Copy,
  FileText,
  Pin,
  PinOff,
  Pencil,
  Plus,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import type { Draft } from "../core/types";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";

function firstLine(text: string): string {
  const line = text.split(/\r?\n/).find((l) => l.trim())?.trim() ?? "";
  return line;
}

function displayTitle(draft: Draft, untitled: string): string {
  const t = draft.title.trim();
  if (t) return t;
  const line = firstLine(draft.content);
  return line || untitled;
}

function formatUpdatedAt(
  iso: string,
  t: (key: string, opts?: Record<string, unknown>) => string
): string {
  const ms = Date.parse(iso);
  if (!Number.isFinite(ms)) return "";
  const minutes = Math.max(0, Math.round((Date.now() - ms) / 60000));
  if (minutes < 1) return t("justNow");
  if (minutes < 60) return t("minutesAgo", { count: minutes });
  const hours = Math.round(minutes / 60);
  if (hours < 48) return t("hoursAgo", { count: hours });
  return t("daysAgo", { count: Math.round(hours / 24) });
}

export default function DraftsManager() {
  const { t } = useTranslation("drafts");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();
  const drafts = useGlobalStore((s) => s.drafts);
  const addDraft = useGlobalStore((s) => s.addDraft);
  const updateDraft = useGlobalStore((s) => s.updateDraft);
  const deleteDraft = useGlobalStore((s) => s.deleteDraft);
  const addSnippet = useGlobalStore((s) => s.addSnippet);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [composeTitle, setComposeTitle] = useState("");
  const [composeContent, setComposeContent] = useState("");
  const [saving, setSaving] = useState(false);
  const [editing, setEditing] = useState<Draft | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(
    null
  );
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const showMsg = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    window.setTimeout(() => setMessage(null), 3500);
  };

  const sorted = useMemo(() => {
    return [...drafts].sort((a, b) => {
      if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
      return b.updatedAt.localeCompare(a.updatedAt);
    });
  }, [drafts]);

  const canCompose = composeContent.trim().length > 0;

  const handleComposeSave = async () => {
    if (!canCompose || saving) return;
    setSaving(true);
    try {
      await addDraft({
        title: composeTitle.trim(),
        content: composeContent,
        pinned: false,
      });
      setComposeTitle("");
      setComposeContent("");
    } catch (e) {
      showMsg("error", String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleCopy = async (draft: Draft) => {
    try {
      await invokeCopyToClipboard(draft.content);
      setCopiedId(draft.id);
      window.setTimeout(() => setCopiedId((cur) => (cur === draft.id ? null : cur)), 1500);
    } catch (e) {
      showMsg("error", t("copyFailed", { error: String(e) }));
    }
  };

  const handleInsertQuickAsk = async (draft: Draft) => {
    try {
      await invoke("open_quick_ask_with_text", { text: draft.content });
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleTogglePin = async (draft: Draft) => {
    try {
      await updateDraft(draft.id, { pinned: !draft.pinned });
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleSaveEdit = async () => {
    if (!editing || !editing.content.trim()) return;
    try {
      await updateDraft(editing.id, {
        title: editing.title.trim(),
        content: editing.content,
      });
      setEditing(null);
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleToSnippet = async (draft: Draft) => {
    const name = displayTitle(draft, t("untitled")).slice(0, 80);
    try {
      await addSnippet({
        name,
        content: draft.content,
        tags: "",
        params: "",
        kind: "text",
      });
      showMsg("success", t("toSnippetDone", { name }));
    } catch (e) {
      showMsg("error", t("toSnippetFailed", { error: String(e) }));
    }
  };

  const handleDelete = async (draft: Draft) => {
    const name = displayTitle(draft, t("untitled"));
    const ok = await confirm({
      title: t("deleteTitle"),
      message: t("deleteConfirm", { name }),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await deleteDraft(draft.id);
      showMsg("success", t("deleted"));
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  return (
    <div className="drafts-page">
      <section className="drafts-compose card">
        <div className="input-group">
          <label className="input-label">{t("composeTitle")}</label>
          <input
            className="input-field"
            value={composeTitle}
            onChange={(e) => setComposeTitle(e.target.value)}
            placeholder={t("composeTitlePlaceholder")}
          />
        </div>
        <div className="input-group drafts-compose-body">
          <label className="input-label">{t("composeContent")}</label>
          <textarea
            className="input-field drafts-compose-editor"
            value={composeContent}
            onChange={(e) => setComposeContent(e.target.value)}
            placeholder={t("composePlaceholder")}
            rows={6}
            spellCheck={false}
          />
        </div>
        <div className="drafts-compose-actions">
          <span className="drafts-count">{t("countLabel", { count: drafts.length })}</span>
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => void handleComposeSave()}
            disabled={!canCompose || saving}
          >
            <Plus size={14} />
            {t("saveDraft")}
          </button>
        </div>
      </section>

      <div className="drafts-list">
        {sorted.length === 0 ? (
          <div className="drafts-empty">
            <FileText size={28} />
            <p>{t("empty")}</p>
            <span className="setting-hint">{t("emptyHint")}</span>
          </div>
        ) : (
          sorted.map((draft) => {
            const title = displayTitle(draft, t("untitled"));
            const preview = firstLine(draft.content);
            return (
              <article
                key={draft.id}
                className={`draft-card${draft.pinned ? " is-pinned" : ""}`}
              >
                <div className="draft-card-head">
                  <div className="draft-card-titles">
                    <h3 className="draft-card-title">{title}</h3>
                    <span className="draft-card-time">
                      {formatUpdatedAt(draft.updatedAt, t)}
                    </span>
                  </div>
                  {draft.pinned && (
                    <span className="draft-pin-badge" title={t("unpin")}>
                      <Pin size={12} />
                    </span>
                  )}
                </div>
                {preview && preview !== title && (
                  <p className="draft-card-preview">{preview}</p>
                )}
                <div className="draft-card-actions">
                  <button
                    type="button"
                    className="snippet-icon-btn"
                    title={t("copy")}
                    aria-label={t("copy")}
                    onClick={() => void handleCopy(draft)}
                  >
                    {copiedId === draft.id ? <Check size={14} /> : <Copy size={14} />}
                  </button>
                  <button
                    type="button"
                    className="snippet-icon-btn"
                    title={t("insertQuickAsk")}
                    aria-label={t("insertQuickAsk")}
                    onClick={() => void handleInsertQuickAsk(draft)}
                  >
                    <Sparkles size={14} />
                  </button>
                  <button
                    type="button"
                    className="snippet-icon-btn"
                    title={draft.pinned ? t("unpin") : t("pin")}
                    aria-label={draft.pinned ? t("unpin") : t("pin")}
                    onClick={() => void handleTogglePin(draft)}
                  >
                    {draft.pinned ? <PinOff size={14} /> : <Pin size={14} />}
                  </button>
                  <button
                    type="button"
                    className="snippet-icon-btn"
                    title={t("edit")}
                    aria-label={t("edit")}
                    onClick={() => setEditing({ ...draft })}
                  >
                    <Pencil size={14} />
                  </button>
                  <button
                    type="button"
                    className="qa-mini-btn primary"
                    title={t("toSnippet")}
                    onClick={() => void handleToSnippet(draft)}
                  >
                    {t("toSnippet")}
                  </button>
                  <button
                    type="button"
                    className="snippet-icon-btn danger"
                    title={t("delete")}
                    aria-label={t("delete")}
                    onClick={() => void handleDelete(draft)}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              </article>
            );
          })
        )}
      </div>

      {editing && (
        <div className="modal-overlay">
          <div className="modal drafts-modal">
            <ModalTitleRow title={t("editTitle")} onClose={() => setEditing(null)} />
            <div className="input-group">
              <label className="input-label">{t("title")}</label>
              <input
                className="input-field"
                value={editing.title}
                onChange={(e) => setEditing({ ...editing, title: e.target.value })}
                autoFocus
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("content")}</label>
              <textarea
                className="input-field drafts-compose-editor"
                rows={10}
                value={editing.content}
                onChange={(e) => setEditing({ ...editing, content: e.target.value })}
                spellCheck={false}
              />
            </div>
            <div className="modal-actions">
              <button type="button" className="btn btn-secondary" onClick={() => setEditing(null)}>
                {t("cancel")}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => void handleSaveEdit()}
                disabled={!editing.content.trim()}
              >
                {t("save")}
              </button>
            </div>
          </div>
        </div>
      )}

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <X size={16} />}
          </span>
          {message.text}
        </div>
      )}
    </div>
  );
}
