import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Plus, Trash2, Copy, Pencil, Check, XCircle, Sparkles, X, Download, Upload, Loader2, FileText } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../core/store";
import type { Snippet } from "../core/types";
import ModalTitleRow from "./ModalTitleRow";
import { useConfirm } from "./ConfirmModal";
import { fillParams, parseParamNames, splitTags } from "../lib/snippets";

const UNDO_MS = 8000;

type SortMode = "useCount" | "updated";

export default function SnippetsManager() {
  const { t } = useTranslation("snippets");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();
  const snippets = useGlobalStore((s) => s.snippets);
  const loadSnippets = useGlobalStore((s) => s.loadSnippets);
  const addSnippet = useGlobalStore((s) => s.addSnippet);
  const updateSnippet = useGlobalStore((s) => s.updateSnippet);
  const deleteSnippet = useGlobalStore((s) => s.deleteSnippet);
  const restoreSnippet = useGlobalStore((s) => s.restoreSnippet);
  const bumpSnippetUse = useGlobalStore((s) => s.bumpSnippetUse);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeSaveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);
  const invokePickTextFile = useGlobalStore((s) => s.invokePickTextFile);

  const [query, setQuery] = useState("");
  const [tagFilter, setTagFilter] = useState<string>("");
  const [sortMode, setSortMode] = useState<SortMode>("useCount");
  const [editing, setEditing] = useState<Partial<Snippet> | null>(null);
  const [useTarget, setUseTarget] = useState<Snippet | null>(null);
  const [paramValues, setParamValues] = useState<Record<string, string>>({});
  const [undoItem, setUndoItem] = useState<Snippet | null>(null);
  const [transferBusy, setTransferBusy] = useState<"export" | "import" | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const undoTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const showMsg = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 4000);
  };

  useEffect(() => {
    void loadSnippets();
    return () => {
      if (undoTimer.current) clearTimeout(undoTimer.current);
    };
  }, [loadSnippets]);

  // Esc closes the topmost modal, matching the overlay-click affordance.
  useEffect(() => {
    if (!editing && !useTarget) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (useTarget) setUseTarget(null);
      else setEditing(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [editing, useTarget]);

  const allTags = useMemo(() => {
    const set = new Set<string>();
    for (const s of snippets) {
      for (const tag of splitTags(s.tags)) set.add(tag);
    }
    return [...set].sort((a, b) => a.localeCompare(b));
  }, [snippets]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    let list = snippets.filter((s) => {
      if (tagFilter) {
        const tags = splitTags(s.tags).map((x) => x.toLowerCase());
        if (!tags.includes(tagFilter.toLowerCase())) return false;
      }
      if (!q) return true;
      return (
        s.name.toLowerCase().includes(q) ||
        s.tags.toLowerCase().includes(q) ||
        s.content.toLowerCase().includes(q)
      );
    });
    list = [...list].sort((a, b) => {
      if (sortMode === "useCount") {
        return b.useCount - a.useCount || b.updatedAt.localeCompare(a.updatedAt);
      }
      return b.updatedAt.localeCompare(a.updatedAt);
    });
    return list;
  }, [snippets, query, tagFilter, sortMode]);

  const openCreate = () => {
    setEditing({ name: "", content: "", tags: "", params: "" });
  };

  const saveEdit = async () => {
    if (!editing?.name?.trim() || !editing.content?.trim()) return;
    // Duplicate names make the tag/search lists confusing; block them like the models page.
    const nameNorm = editing.name.trim().toLowerCase();
    if (snippets.some((s) => s.id !== editing.id && s.name.trim().toLowerCase() === nameNorm)) {
      showMsg("error", t("nameDuplicate"));
      return;
    }
    try {
      if (editing.id) {
        await updateSnippet(editing.id, {
          name: editing.name.trim(),
          content: editing.content,
          tags: editing.tags || "",
          params: editing.params || "",
        });
      } else {
        await addSnippet({
          name: editing.name.trim(),
          content: editing.content,
          tags: editing.tags || "",
          params: editing.params || "",
        });
      }
      setEditing(null);
    } catch (e) {
      showMsg("error", `${tc("status.error")}: ${e}`);
    }
  };

  const handleClone = async (s: Snippet) => {
    try {
      await addSnippet({
        name: `${s.name}${t("cloneSuffix")}`,
        content: s.content,
        tags: s.tags,
        params: s.params,
      });
      showMsg("success", t("cloned", { name: s.name }));
    } catch (e) {
      showMsg("error", `${tc("status.error")}: ${e}`);
    }
  };

  /** Backup / transfer. Snippets are plaintext; the JSON should still be treated carefully. */
  const handleExport = async () => {
    if (snippets.length === 0 || transferBusy) return;
    setTransferBusy("export");
    try {
      const payload = {
        app: "ai-workbench",
        kind: "snippets",
        version: 1,
        exportedAt: new Date().toISOString(),
        snippets: snippets.map((s) => ({
          name: s.name,
          content: s.content,
          tags: s.tags,
          params: s.params,
        })),
      };
      const path = await invokeSaveTextFile(
        JSON.stringify(payload, null, 2),
        `snippets-${new Date().toISOString().slice(0, 10)}.json`,
        t("export")
      );
      showMsg("success", t("exportDone", { path }));
    } catch (error) {
      const message = String(error);
      // The native dialog reports a cancelled save as an error; that is not a failure.
      if (message.includes("已取消")) return;
      showMsg("error", t("exportFailed", { error: message }));
    } finally {
      setTransferBusy(null);
    }
  };

  const handleImport = async () => {
    if (transferBusy) return;
    setTransferBusy("import");
    try {
      const raw = await invokePickTextFile(t("import"));
      let parsed: unknown;
      try {
        parsed = JSON.parse(raw);
      } catch {
        showMsg("error", t("importInvalid"));
        return;
      }
      const list = Array.isArray(parsed)
        ? parsed
        : ((parsed as { snippets?: unknown }).snippets ?? []);
      if (!Array.isArray(list) || list.length === 0) {
        showMsg("error", t("importInvalid"));
        return;
      }
      const fingerprint = (s: { name?: string; content?: string }) =>
        `${s.name ?? ""}|${s.content ?? ""}`.trim();
      const existing = new Set(snippets.map(fingerprint));
      let added = 0;
      let skipped = 0;
      for (const item of list as Array<Record<string, unknown>>) {
        const entry = {
          name: String(item?.name ?? "").trim(),
          content: String(item?.content ?? ""),
        };
        if (!entry.name || !entry.content || existing.has(fingerprint(entry))) {
          skipped += 1;
          continue;
        }
        existing.add(fingerprint(entry));
        await addSnippet({
          name: entry.name,
          content: entry.content,
          tags: String(item?.tags ?? ""),
          params: String(item?.params ?? ""),
        });
        added += 1;
      }
      showMsg(
        added > 0 ? "success" : "error",
        t("importDone", { added, skipped })
      );
    } catch (error) {
      const message = String(error);
      if (message.includes("已取消")) return;
      showMsg("error", t("importFailed", { error: message }));
    } finally {
      setTransferBusy(null);
    }
  };

  const handleDelete = async (s: Snippet) => {
    const ok = await confirm({
      title: t("deleteTitle"),
      message: t("deleteConfirm", { name: s.name }),
      warning: t("deleteWarning"),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    await deleteSnippet(s.id);
    setUndoItem(s);
    if (undoTimer.current) clearTimeout(undoTimer.current);
    undoTimer.current = setTimeout(() => setUndoItem(null), UNDO_MS);
  };

  const handleUndo = async () => {
    if (!undoItem) return;
    await restoreSnippet(undoItem);
    setUndoItem(null);
    if (undoTimer.current) clearTimeout(undoTimer.current);
  };

  const resolvedText = (s: Snippet, values: Record<string, string>) =>
    Object.keys(values).length ? fillParams(s.content, values) : s.content;

  const startUse = (s: Snippet) => {
    const names = parseParamNames(s.params, s.content);
    if (names.length === 0) {
      setParamValues({});
      setUseTarget(s);
      return;
    }
    const init: Record<string, string> = {};
    names.forEach((n) => {
      init[n] = "";
    });
    setParamValues(init);
    setUseTarget(s);
  };

  const confirmCopy = async () => {
    if (!useTarget) return;
    const text = resolvedText(useTarget, paramValues);
    await invokeCopyToClipboard(text);
    await bumpSnippetUse(useTarget.id);
    setUseTarget(null);
  };

  const confirmInsertQuickAsk = async () => {
    if (!useTarget) return;
    const text = resolvedText(useTarget, paramValues);
    await bumpSnippetUse(useTarget.id);
    setUseTarget(null);
    try {
      await invoke("open_quick_ask_with_text", { text });
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="snippets-page">
      <div className="snippets-toolbar">
        <input
          className="input-field"
          placeholder={t("search")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <select
          className="input-field snippets-sort"
          value={sortMode}
          onChange={(e) => setSortMode(e.target.value as SortMode)}
          aria-label={t("sortLabel")}
        >
          <option value="useCount">{t("sortUseCount")}</option>
          <option value="updated">{t("sortUpdated")}</option>
        </select>
        <button
          type="button"
          className="btn btn-secondary"
          onClick={() => void handleImport()}
          disabled={transferBusy !== null}
          title={t("import")}
          aria-label={t("import")}
        >
          {transferBusy === "import" ? (
            <Loader2 size={14} className="spin" />
          ) : (
            <Upload size={14} />
          )}
        </button>
        <button
          type="button"
          className="btn btn-secondary"
          onClick={() => void handleExport()}
          disabled={transferBusy !== null || snippets.length === 0}
          title={t("export")}
          aria-label={t("export")}
        >
          {transferBusy === "export" ? (
            <Loader2 size={14} className="spin" />
          ) : (
            <Download size={14} />
          )}
        </button>
        <button type="button" className="btn btn-primary" onClick={openCreate}>
          <Plus size={14} /> {t("add")}
        </button>
      </div>

      {allTags.length > 0 && (
        <div className="snippets-tags">
          <button
            type="button"
            className={`snippets-tag-chip${!tagFilter ? " on" : ""}`}
            onClick={() => setTagFilter("")}
          >
            {t("tagAll")}
          </button>
          {allTags.map((tag) => (
            <button
              key={tag}
              type="button"
              className={`snippets-tag-chip${tagFilter === tag ? " on" : ""}`}
              onClick={() => setTagFilter(tagFilter === tag ? "" : tag)}
            >
              {tag}
            </button>
          ))}
        </div>
      )}

      <div className="snippets-list">
        {filtered.map((s) => {
          const tags = splitTags(s.tags);
          return (
            <div key={s.id} className="snippet-card">
              <div className="snippet-card-head">
                <strong className="snippet-name" title={s.name}>{s.name}</strong>
                {s.useCount > 0 && (
                  <span className="snippet-badge" title={t("used", { count: s.useCount })}>
                    {s.useCount}×
                  </span>
                )}
              </div>
              {tags.length > 0 && (
                <div className="snippet-card-tags">
                  {tags.map((tag) => (
                    <button
                      key={tag}
                      type="button"
                      className="snippet-tag-chip"
                      onClick={() => setTagFilter(tagFilter === tag ? "" : tag)}
                      title={tagFilter === tag ? t("tagAll") : tag}
                    >
                      {tag}
                    </button>
                  ))}
                </div>
              )}
              <pre className="snippet-preview">{s.content}</pre>
              <div className="snippet-actions">
                <button type="button" className="qa-mini-btn primary" onClick={() => startUse(s)}>
                  <Copy size={13} /> {t("use")}
                </button>
                <button
                  type="button"
                  className="qa-mini-btn"
                  onClick={() => setEditing({ ...s })}
                >
                  <Pencil size={13} /> {t("edit")}
                </button>
                <button
                  type="button"
                  className="qa-mini-btn"
                  onClick={() => void handleClone(s)}
                  title={t("clone")}
                >
                  <Copy size={13} /> {t("clone")}
                </button>
                <button
                  type="button"
                  className="qa-mini-btn danger"
                  onClick={() => void handleDelete(s)}
                >
                  <Trash2 size={13} /> {t("delete")}
                </button>
              </div>
            </div>
          );
        })}
        {filtered.length === 0 && (
          <div className="snippets-empty">
            <FileText size={28} />
            <p>{snippets.length === 0 ? t("empty") : t("noMatch")}</p>
            {snippets.length === 0 ? (
              <button type="button" className="btn btn-primary" onClick={openCreate}>
                <Plus size={14} /> {t("add")}
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => {
                  setQuery("");
                  setTagFilter("");
                }}
              >
                {t("clearSearch")}
              </button>
            )}
          </div>
        )}
      </div>

      {editing && (
        <div className="modal-overlay" onClick={() => setEditing(null)}>
          <div className="modal snippets-modal" onClick={(e) => e.stopPropagation()}>
            <ModalTitleRow title={editing.id ? t("editTitle") : t("addTitle")} onClose={() => setEditing(null)} />
            <div className="input-group">
              <label className="input-label">{t("name")}</label>
              <input
                className="input-field"
                value={editing.name || ""}
                onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                autoFocus
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("tags")}</label>
              <input
                className="input-field"
                value={editing.tags || ""}
                onChange={(e) => setEditing({ ...editing, tags: e.target.value })}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("params")}</label>
              <input
                className="input-field"
                value={editing.params || ""}
                onChange={(e) => setEditing({ ...editing, params: e.target.value })}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("content")}</label>
              <textarea
                className="input-field snippets-content-editor"
                rows={8}
                value={editing.content || ""}
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
                onClick={() => void saveEdit()}
                disabled={!editing.name?.trim() || !editing.content?.trim()}
              >
                {t("save")}
              </button>
            </div>
          </div>
        </div>
      )}

      {useTarget && (
        <div className="modal-overlay" onClick={() => setUseTarget(null)}>
          <div className="modal snippets-modal" onClick={(e) => e.stopPropagation()}>
            <ModalTitleRow
              title={Object.keys(paramValues).length ? t("fillParams") : t("useTitle")}
              onClose={() => setUseTarget(null)}
            />
            {Object.keys(paramValues).length === 0 ? (
              <p className="setting-hint" style={{ marginBottom: 12 }}>
                {t("useHint")}
              </p>
            ) : (
              Object.keys(paramValues).map((key) => (
                <div className="input-group" key={key}>
                  <label className="input-label">{key}</label>
                  <input
                    className="input-field"
                    value={paramValues[key]}
                    onChange={(e) => setParamValues({ ...paramValues, [key]: e.target.value })}
                  />
                </div>
              ))
            )}
            {/* Live preview of the resolved text before copying / inserting. */}
            <div className="input-group">
              <label className="input-label">{t("preview")}</label>
              <pre className="snippet-preview snippets-use-preview">
                {resolvedText(useTarget, paramValues) || useTarget.content}
              </pre>
            </div>
            <div className="modal-actions">
              <button type="button" className="btn btn-secondary" onClick={() => setUseTarget(null)}>
                {t("cancel")}
              </button>
              <button type="button" className="btn btn-secondary" onClick={() => void confirmCopy()}>
                <Copy size={12} /> {t("copyUse")}
              </button>
              <button type="button" className="btn btn-primary" onClick={() => void confirmInsertQuickAsk()}>
                <Sparkles size={12} /> {t("insertQuickAsk")}
              </button>
            </div>
          </div>
        </div>
      )}

      {undoItem && (
        <div className="toast toast-warning" role="status">
          <span className="toast-icon">
            <XCircle size={16} />
          </span>
          <span className="toast-text">{t("deletedToast", { name: undoItem.name })}</span>
          <button type="button" className="btn btn-secondary btn-small toast-action" onClick={() => void handleUndo()}>
            <Check size={12} /> {t("undoDelete")}
          </button>
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
