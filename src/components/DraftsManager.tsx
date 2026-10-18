import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  BookmarkPlus,
  Brush,
  Check,
  Copy,
  FileText,
  Undo2,
  Pin,
  PinOff,
  Pencil,
  PenLine,
  Plus,
  StickyNote,
  Search,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import type { Draft } from "../core/types";
import { readStoredString, writeStoredString } from "../core/localState";
import { useConfirm, useConfirmChoice } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";

// The page unmounts when another tab is opened; keep a half-written note alive.
const COMPOSE_TITLE_KEY = "drafts-compose-title";
const COMPOSE_CONTENT_KEY = "drafts-compose-content";
const UNDO_MS = 8000;
const STALE_DAYS = 30;
const STALE_PREVIEW_NAMES = 5;

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

/** Body shown on the card; drops the first line when it already serves as the title. */
function previewText(draft: Draft): string {
  const body = draft.content.replace(/^\s*\n/, "").trimEnd();
  if (draft.title.trim()) return body;
  const lines = body.split(/\r?\n/);
  const idx = lines.findIndex((l) => l.trim());
  return idx < 0 ? "" : lines.slice(idx + 1).join("\n").replace(/^\s*\n/, "");
}

function isSaveShortcut(e: React.KeyboardEvent): boolean {
  return e.key === "Enter" && (e.ctrlKey || e.metaKey);
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
  const days = Math.round(hours / 24);
  if (days <= 7) return t("daysAgo", { count: days });
  return new Date(ms).toLocaleDateString();
}

function formatAbsolute(iso: string): string {
  const ms = Date.parse(iso);
  return Number.isFinite(ms) ? new Date(ms).toLocaleString() : "";
}

export default function DraftsManager() {
  const { t } = useTranslation("drafts");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();
  const confirmChoice = useConfirmChoice();
  const drafts = useGlobalStore((s) => s.drafts);
  const addDraft = useGlobalStore((s) => s.addDraft);
  const updateDraft = useGlobalStore((s) => s.updateDraft);
  const deleteDraft = useGlobalStore((s) => s.deleteDraft);
  const deleteDrafts = useGlobalStore((s) => s.deleteDrafts);
  const restoreDrafts = useGlobalStore((s) => s.restoreDrafts);
  const addSnippet = useGlobalStore((s) => s.addSnippet);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [composeTitle, setComposeTitle] = useState(() => readStoredString(COMPOSE_TITLE_KEY));
  const [composeContent, setComposeContent] = useState(() =>
    readStoredString(COMPOSE_CONTENT_KEY)
  );
  const [saving, setSaving] = useState(false);
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<Draft | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(
    null
  );
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [undo, setUndo] = useState<{ items: Draft[]; text: string } | null>(null);
  const [, setTick] = useState(0);
  const searchRef = useRef<HTMLInputElement>(null);
  const composeRef = useRef<HTMLTextAreaElement>(null);
  const msgTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const undoTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => writeStoredString(COMPOSE_TITLE_KEY, composeTitle), [composeTitle]);
  useEffect(() => writeStoredString(COMPOSE_CONTENT_KEY, composeContent), [composeContent]);

  // Relative times ("3 分钟前") go stale while the page stays open.
  useEffect(() => {
    const id = window.setInterval(() => setTick((n) => n + 1), 60_000);
    return () => {
      window.clearInterval(id);
      if (msgTimer.current) clearTimeout(msgTimer.current);
      if (undoTimer.current) clearTimeout(undoTimer.current);
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "f") return;
      if (document.querySelector(".modal-overlay") || !searchRef.current) return;
      e.preventDefault();
      searchRef.current.focus();
      searchRef.current.select();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const showMsg = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    if (msgTimer.current) clearTimeout(msgTimer.current);
    msgTimer.current = setTimeout(() => setMessage(null), 3500);
  };

  const offerUndo = (items: Draft[], text: string) => {
    setMessage(null);
    setUndo({ items, text });
    if (undoTimer.current) clearTimeout(undoTimer.current);
    undoTimer.current = setTimeout(() => setUndo(null), UNDO_MS);
  };

  const handleUndo = async () => {
    if (!undo) return;
    if (undoTimer.current) clearTimeout(undoTimer.current);
    const { items } = undo;
    setUndo(null);
    try {
      await restoreDrafts(items);
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const sorted = useMemo(() => {
    return [...drafts].sort((a, b) => {
      if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
      return b.updatedAt.localeCompare(a.updatedAt);
    });
  }, [drafts]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return sorted;
    return sorted.filter(
      (d) => d.title.toLowerCase().includes(q) || d.content.toLowerCase().includes(q)
    );
  }, [sorted, query]);

  const stale = useMemo(() => {
    const cutoff = Date.now() - STALE_DAYS * 86_400_000;
    return drafts.filter((d) => !d.pinned && Date.parse(d.updatedAt) < cutoff);
  }, [drafts]);
  const staleIds = useMemo(() => new Set(stale.map((d) => d.id)), [stale]);

  const canCompose = composeContent.trim().length > 0;
  const original = editing ? drafts.find((d) => d.id === editing.id) : undefined;
  const editDirty =
    !!editing &&
    !!original &&
    (editing.title !== original.title || editing.content !== original.content);

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
      composeRef.current?.focus();
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

  const handleInsertAiHub = async (draft: Draft) => {
    try {
      await invoke("open_ai_hub_with_text", { text: draft.content });
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
    if (!editDirty) {
      setEditing(null);
      return;
    }
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

  const requestCloseEdit = async () => {
    if (editDirty) {
      const ok = await confirm({
        title: t("discardTitle"),
        message: t("discardConfirm"),
        confirmText: t("discard"),
        icon: "warning",
      });
      if (!ok) return;
    }
    setEditing(null);
  };

  const handleToSnippet = async (draft: Draft) => {
    const name = displayTitle(draft, t("untitled")).slice(0, 80);
    const choice = await confirmChoice({
      title: t("toSnippetTitle"),
      message: t("toSnippetConfirm", { name }),
      confirmText: t("toSnippetAndDelete"),
      altConfirmText: t("toSnippetKeep"),
      icon: "question",
    });
    if (choice === "cancel") return;
    try {
      await addSnippet({
        name,
        content: draft.content,
        tags: "",
        params: "",
        kind: "text",
      });
    } catch (e) {
      showMsg("error", t("toSnippetFailed", { error: String(e) }));
      return;
    }
    if (choice === "confirm") {
      try {
        await deleteDraft(draft.id);
        offerUndo([draft], t("toSnippetDoneDeleted", { name }));
      } catch (e) {
        showMsg("error", String(e));
      }
      return;
    }
    showMsg("success", t("toSnippetDone", { name }));
  };

  const handleDelete = async (draft: Draft) => {
    const name = displayTitle(draft, t("untitled"));
    try {
      await deleteDraft(draft.id);
      offerUndo([draft], t("deletedToast", { name }));
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleCleanStale = async () => {
    if (!stale.length) return;
    const names = stale
      .slice(0, STALE_PREVIEW_NAMES)
      .map((d) => t("quotedName", { name: displayTitle(d, t("untitled")) }))
      .join(t("nameSeparator"));
    const ok = await confirm({
      title: t("cleanTitle"),
      message: t("cleanConfirm", { count: stale.length, days: STALE_DAYS }),
      warning:
        stale.length > STALE_PREVIEW_NAMES
          ? t("cleanListMore", { names, count: stale.length })
          : t("cleanList", { names }),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    const items = [...stale];
    try {
      await deleteDrafts(items.map((d) => d.id));
      offerUndo(items, t("cleanedToast", { count: items.length }));
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const composeKeyDown = (e: React.KeyboardEvent) => {
    if (!isSaveShortcut(e)) return;
    e.preventDefault();
    void handleComposeSave();
  };

  return (
    <div className="drafts-page">
      <section className="drafts-compose card">
        <div className="drafts-compose-head">
          <span className="drafts-compose-icon">
            <PenLine size={15} />
          </span>
          <div className="drafts-compose-heading">
            <strong>{t("composeHeading")}</strong>
            <span>{t("composeSubheading")}</span>
          </div>
        </div>
        <div className="drafts-editor">
          <input
            className="drafts-editor-title"
            value={composeTitle}
            onChange={(e) => setComposeTitle(e.target.value)}
            onKeyDown={composeKeyDown}
            placeholder={t("composeTitlePlaceholder")}
            aria-label={t("composeTitle")}
          />
          <textarea
            ref={composeRef}
            className="drafts-editor-body"
            value={composeContent}
            onChange={(e) => setComposeContent(e.target.value)}
            onKeyDown={composeKeyDown}
            placeholder={t("composePlaceholder")}
            aria-label={t("composeContent")}
            rows={6}
            spellCheck={false}
          />
        </div>
        <div className="drafts-compose-actions">
          <span className="drafts-kbd-hint">
            <kbd>Ctrl</kbd>+<kbd>Enter</kbd>
            <span>{t("composeShortcut")}</span>
          </span>
          <div className="drafts-compose-buttons">
            {(composeTitle || composeContent) && (
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => {
                  setComposeTitle("");
                  setComposeContent("");
                  composeRef.current?.focus();
                }}
                disabled={saving}
              >
                {t("clearCompose")}
              </button>
            )}
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
        </div>
      </section>

      {drafts.length > 0 && (
        <div className="drafts-toolbar">
          <div className="snippets-search">
            <Search size={13} className="snippets-search-icon" />
            <input
              ref={searchRef}
              className="input-field"
              placeholder={t("search")}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape" && query) {
                  e.stopPropagation();
                  setQuery("");
                }
              }}
              aria-label={t("search")}
            />
            {query && (
              <button
                type="button"
                className="snippets-search-clear"
                onClick={() => {
                  setQuery("");
                  searchRef.current?.focus();
                }}
                aria-label={t("clearSearch")}
                title={t("clearSearch")}
              >
                <X size={11} />
              </button>
            )}
          </div>
          <div className="drafts-toolbar-actions">
            <span className="drafts-count">
              {query.trim()
                ? t("countFiltered", { shown: visible.length, count: drafts.length })
                : t("countLabel", { count: drafts.length })}
            </span>
            {stale.length > 0 && (
              <button
                type="button"
                className="btn btn-secondary btn-small"
                title={t("cleanHint", { days: STALE_DAYS })}
                onClick={() => void handleCleanStale()}
              >
                <Brush size={12} />
                {t("cleanButton", { count: stale.length })}
              </button>
            )}
          </div>
        </div>
      )}

      <div className="drafts-list">
        {drafts.length === 0 ? (
          <div className="drafts-empty">
            <span className="drafts-empty-icon">
              <FileText size={22} />
            </span>
            <p>{t("empty")}</p>
            <span className="setting-hint">{t("emptyHint")}</span>
          </div>
        ) : visible.length === 0 ? (
          <div className="drafts-empty">
            <span className="drafts-empty-icon">
              <Search size={22} />
            </span>
            <p>{t("noMatch")}</p>
            <button type="button" className="btn btn-secondary" onClick={() => setQuery("")}>
              {t("clearSearch")}
            </button>
          </div>
        ) : (
          visible.map((draft) => {
            const title = displayTitle(draft, t("untitled"));
            const preview = previewText(draft);
            const lineCount = draft.content.split(/\r?\n/).length;
            return (
              <article
                key={draft.id}
                className={`draft-card${draft.pinned ? " is-pinned" : ""}`}
              >
                <div className="draft-card-head">
                  <span className="draft-card-icon" title={draft.pinned ? t("pinned") : undefined}>
                    {draft.pinned ? <Pin size={14} /> : <StickyNote size={14} />}
                  </span>
                  <div className="draft-card-titles">
                    <h3 className="draft-card-title" title={title}>
                      {title}
                    </h3>
                    <span
                      className={`draft-card-time${staleIds.has(draft.id) ? " is-stale" : ""}`}
                      title={
                        staleIds.has(draft.id)
                          ? `${formatAbsolute(draft.updatedAt)} · ${t("staleHint", { days: STALE_DAYS })}`
                          : formatAbsolute(draft.updatedAt)
                      }
                    >
                      {formatUpdatedAt(draft.updatedAt, t)}
                      {" · "}
                      {t("sizeLabel", { lines: lineCount, chars: draft.content.length })}
                    </span>
                  </div>
                </div>
                <button
                  type="button"
                  className="draft-card-preview"
                  title={t("clickToEdit")}
                  onClick={() => setEditing({ ...draft })}
                >
                  <span className="draft-card-preview-text">
                    {preview || <span className="draft-card-preview-empty">{t("noBody")}</span>}
                  </span>
                </button>
                <div className="draft-card-actions">
                  <div className="draft-card-tools">
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
                      className="snippet-icon-btn"
                      title={`${t("toSnippet")} · ${t("toSnippetHint")}`}
                      aria-label={t("toSnippet")}
                      onClick={() => void handleToSnippet(draft)}
                    >
                      <BookmarkPlus size={14} />
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
                  <button
                    type="button"
                    className="snippet-icon-btn draft-card-ask"
                    title={t("insertAiHub")}
                    aria-label={t("insertAiHub")}
                    onClick={() => void handleInsertAiHub(draft)}
                  >
                    <Sparkles size={14} />
                  </button>
                  <button
                    type="button"
                    className={`snippet-use-btn${copiedId === draft.id ? " is-done" : ""}`}
                    onClick={() => void handleCopy(draft)}
                  >
                    {copiedId === draft.id ? <Check size={13} /> : <Copy size={13} />}
                    {copiedId === draft.id ? t("copied") : t("copy")}
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
            <ModalTitleRow title={t("editTitle")} onClose={() => void requestCloseEdit()} />
            <div className="drafts-editor">
              <input
                className="drafts-editor-title"
                value={editing.title}
                onChange={(e) => setEditing({ ...editing, title: e.target.value })}
                onKeyDown={(e) => {
                  if (!isSaveShortcut(e)) return;
                  e.preventDefault();
                  void handleSaveEdit();
                }}
                placeholder={t("composeTitlePlaceholder")}
                aria-label={t("title")}
              />
              <textarea
                className="drafts-editor-body"
                rows={12}
                value={editing.content}
                onChange={(e) => setEditing({ ...editing, content: e.target.value })}
                onKeyDown={(e) => {
                  if (!isSaveShortcut(e)) return;
                  e.preventDefault();
                  void handleSaveEdit();
                }}
                aria-label={t("content")}
                spellCheck={false}
                autoFocus
              />
            </div>
            <div className="modal-actions">
              <span className="drafts-kbd-hint drafts-modal-hint">
                <kbd>Ctrl</kbd>+<kbd>Enter</kbd>
                <span>{t("saveShortcut")}</span>
              </span>
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => void requestCloseEdit()}
              >
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

      {undo && (
        <div className="toast toast-warning" role="status">
          <span className="toast-icon">
            <Trash2 size={16} />
          </span>
          <span className="toast-text">{undo.text}</span>
          <button
            type="button"
            className="btn btn-secondary btn-small toast-action"
            onClick={() => void handleUndo()}
          >
            <Undo2 size={12} /> {t("undo")}
          </button>
        </div>
      )}

      {!undo && message && (
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
