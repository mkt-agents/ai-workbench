import { useEffect, useMemo, useState, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";
import type { HostProfile } from "../core/types";
import {
  Archive,
  Check,
  CheckCircle2,
  FileText,
  Globe,
  HardDrive,
  History,
  Network,
  Pencil,
  Plus,
  RotateCw,
  Save,
  ShieldAlert,
  ShieldCheck,
  Trash2,
  Undo2,
  Upload,
  XCircle,
} from "lucide-react";

/** Count active mappings vs comment lines for the editor header stats. */
function parseHostsStats(content: string): { active: number; comments: number } {
  let active = 0;
  let comments = 0;
  for (const line of content.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    if (trimmed.startsWith("#")) comments += 1;
    else active += 1;
  }
  return { active, comments };
}

/** Line-ending / trailing-whitespace insensitive, so a profile written verbatim still reads as current. */
function normalizeHosts(content: string): string {
  return content.replace(/\r\n/g, "\n").trim();
}

/** `hosts-20261010-202244.bak` → `2026-10-10 20:22:44`; anything else is shown as-is. */
function formatBackupName(name: string): string {
  const m = /^hosts-(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})\.bak$/.exec(name);
  return m ? `${m[1]}-${m[2]}-${m[3]} ${m[4]}:${m[5]}:${m[6]}` : name;
}

/**
 * Comment / uncomment every line touched by the selection: if all non-empty lines are
 * already commented, strip one `#`; otherwise prefix `#`. Returns the new text and the
 * selection re-spanning the same lines.
 */
function toggleCommentLines(text: string, selStart: number, selEnd: number) {
  const lineStart = text.lastIndexOf("\n", selStart - 1) + 1;
  const endProbe = selEnd > selStart && text[selEnd - 1] === "\n" ? selEnd - 1 : selEnd;
  const nl = text.indexOf("\n", endProbe);
  const lineEnd = nl === -1 ? text.length : nl;
  const lines = text.slice(lineStart, lineEnd).split("\n");
  const filled = lines.filter((l) => l.trim());
  const uncomment = filled.length > 0 && filled.every((l) => l.trimStart().startsWith("#"));
  const next = lines
    .map((l) => {
      if (!l.trim()) return l;
      return uncomment ? l.replace(/^(\s*)#\s?/, "$1") : `#${l}`;
    })
    .join("\n");
  return {
    text: text.slice(0, lineStart) + next + text.slice(lineEnd),
    selStart: lineStart,
    selEnd: lineStart + next.length,
  };
}

function HostsManager() {
  const { t } = useTranslation("hosts");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();
  const hostProfiles = useGlobalStore((s) => s.hostProfiles);
  const loadHostProfiles = useGlobalStore((s) => s.loadHostProfiles);
  const addHostProfile = useGlobalStore((s) => s.addHostProfile);
  const updateHostProfile = useGlobalStore((s) => s.updateHostProfile);
  const deleteHostProfile = useGlobalStore((s) => s.deleteHostProfile);

  const invokeReadSystemHosts = useGlobalStore((s) => s.invokeReadSystemHosts);
  const invokeIsAdmin = useGlobalStore((s) => s.invokeIsAdmin);
  const invokeRelaunchAsAdmin = useGlobalStore((s) => s.invokeRelaunchAsAdmin);
  const invokeWriteSystemHosts = useGlobalStore((s) => s.invokeWriteSystemHosts);
  const invokeListHostBackups = useGlobalStore((s) => s.invokeListHostBackups);
  const invokeRestoreHostBackup = useGlobalStore((s) => s.invokeRestoreHostBackup);
  const invokeDeleteHostBackup = useGlobalStore((s) => s.invokeDeleteHostBackup);

  const [systemContent, setSystemContent] = useState("");
  /** Content as last read from / written to the system file; difference = unsaved edits. */
  const [baseline, setBaseline] = useState("");
  const readGen = useRef(0);
  const [canWrite, setCanWrite] = useState(true);
  const [backupBusy, setBackupBusy] = useState(false);
  const [backups, setBackups] = useState<{ path: string; name: string }[]>([]);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<HostProfile | null>(null);
  const [formData, setFormData] = useState({ name: "", content: "" });

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 4000);
  }, []);

  const refreshMeta = async () => {
    await Promise.all([
      loadHostProfiles(),
      invokeIsAdmin().then(setCanWrite),
      invokeListHostBackups().then(setBackups).catch(() => setBackups([])),
    ]);
  };

  const reloadEditor = async () => {
    const gen = ++readGen.current;
    const c = await invokeReadSystemHosts();
    if (gen !== readGen.current) return;
    setSystemContent(c);
    setBaseline(c);
  };

  const hostStats = useMemo(() => parseHostsStats(systemContent), [systemContent]);
  const editorDirty = systemContent !== baseline;
  const lineCount = useMemo(() => systemContent.split("\n").length, [systemContent]);
  const baselineNorm = useMemo(() => normalizeHosts(baseline), [baseline]);
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const gutterRef = useRef<HTMLDivElement>(null);
  const [relaunching, setRelaunching] = useState(false);

  const editContent = (next: string) => {
    readGen.current += 1;
    setSystemContent(next);
  };

  useEffect(() => {
    refreshMeta();
    void reloadEditor();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Esc closes the profile modal, matching the overlay-click affordance.
  useEffect(() => {
    if (!showModal) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowModal(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [showModal]);

  const readSystem = async () => {
    if (editorDirty) {
      const ok = await confirm({
        title: t("discardTitle"),
        message: t("discardMessage"),
        confirmText: t("read"),
        icon: "warning",
      });
      if (!ok) return;
    }
    try {
      await reloadEditor();
      showMsg("success", t("readOk"));
    } catch (e) {
      showMsg("error", t("readFailed", { error: String(e) }));
    }
  };

  const writeSystem = async () => {
    if (!canWrite) {
      showMsg("error", t("cannotWrite"));
      return;
    }
    if (systemContent === baseline) {
      showMsg("success", t("noChanges"));
      return;
    }
    // Overwriting the system hosts file affects every app on the machine; confirm first.
    const ok = await confirm({
      title: t("writeTitle"),
      message: t("writeMessage"),
      warning: t("writeWarning"),
      confirmText: t("write"),
      icon: "warning",
    });
    if (!ok) return;
    try {
      const msg = await invokeWriteSystemHosts(systemContent);
      await reloadEditor();
      showMsg("success", msg || t("writeOk"));
      await refreshMeta();
    } catch (e) {
      showMsg("error", t("writeFailed", { error: String(e) }));
    }
  };

  const handleBackupNow = async () => {
    if (backupBusy) return;
    setBackupBusy(true);
    try {
      const path = await invoke<string>("backup_hosts_now");
      await refreshMeta();
      showMsg("success", t("backupOk", { path }));
    } catch (e) {
      showMsg("error", t("backupFailed", { error: String(e) }));
    } finally {
      setBackupBusy(false);
    }
  };

  /** Prefill a new profile with the current editor content. */
  const handleSaveAsProfile = () => {
    setEditing(null);
    setFormData({ name: "", content: systemContent });
    setShowModal(true);
  };

  const applyProfile = async (profile: HostProfile) => {
    if (!canWrite) {
      showMsg("error", t("cannotWrite"));
      return;
    }
    const ok = await confirm({
      title: t("applyTitle"),
      message: t("applyMessage", { name: profile.name }),
      warning: t("applyWarning"),
      confirmText: t("apply"),
      icon: "warning",
    });
    if (!ok) return;
    try {
      const msg = await invokeWriteSystemHosts(profile.content);
      await reloadEditor();
      showMsg("success", msg || t("applyOk", { name: profile.name }));
      await refreshMeta();
    } catch (e) {
      showMsg("error", t("applyFailed", { error: String(e) }));
    }
  };

  const handleAdd = () => {
    setEditing(null);
    setFormData({ name: "", content: systemContent || "# hosts\n127.0.0.1 localhost\n" });
    setShowModal(true);
  };

  const handleEdit = (profile: HostProfile) => {
    setEditing(profile);
    setFormData({ name: profile.name, content: profile.content });
    setShowModal(true);
  };

  const handleDelete = async (id: string) => {
    const profile = hostProfiles.find((p) => p.id === id);
    const ok = await confirm({
      title: t("deleteTitle"),
      message: t("deleteMessage", { name: profile?.name || id }),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await deleteHostProfile(id);
      showMsg("success", t("deleted"));
    } catch (e) {
      showMsg("error", `${tc("status.error")}: ${e}`);
    }
  };

  const handleSubmit = async () => {
    if (!formData.name.trim() || !formData.content.trim()) {
      showMsg("error", t("nameRequired"));
      return;
    }
    const nameNorm = formData.name.trim().toLowerCase();
    if (
      hostProfiles.some(
        (p) => p.id !== editing?.id && p.name.trim().toLowerCase() === nameNorm
      )
    ) {
      showMsg("error", t("nameDuplicate"));
      return;
    }
    try {
      if (editing) {
        await updateHostProfile(editing.id, {
          name: formData.name.trim(),
          content: formData.content,
        });
      } else {
        await addHostProfile({
          id: Date.now().toString(),
          name: formData.name.trim(),
          content: formData.content,
        });
      }
      showMsg("success", t("saved"));
      setShowModal(false);
    } catch (e) {
      showMsg("error", `${tc("status.error")}: ${e}`);
    }
  };

  const restoreBackup = async (path: string, name: string) => {
    if (!canWrite) {
      showMsg("error", t("cannotWrite"));
      return;
    }
    const ok = await confirm({
      title: t("restoreTitle"),
      message: t("restoreMessage", { name }),
      confirmText: t("restore"),
      icon: "warning",
    });
    if (!ok) return;
    try {
      const msg = await invokeRestoreHostBackup(path);
      await reloadEditor();
      showMsg("success", msg || t("restoreOk"));
      await refreshMeta();
    } catch (e) {
      showMsg("error", t("restoreFailed", { error: String(e) }));
    }
  };

  const deleteBackup = async (path: string, name: string) => {
    const ok = await confirm({
      title: t("deleteBackupTitle"),
      message: t("deleteBackupMessage", { name: formatBackupName(name) }),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await invokeDeleteHostBackup(path);
      setBackups((prev) => prev.filter((b) => b.path !== path));
      showMsg("success", t("deleted"));
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const relaunchAsAdmin = async () => {
    if (relaunching) return;
    if (editorDirty) {
      const ok = await confirm({
        title: t("relaunchAdmin"),
        message: t("relaunchDirty"),
        confirmText: t("relaunchAdmin"),
        icon: "warning",
      });
      if (!ok) return;
    }
    setRelaunching(true);
    try {
      await invokeRelaunchAsAdmin();
    } catch (e) {
      showMsg("error", String(e));
      setRelaunching(false);
    }
  };

  const onEditorKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const key = e.key.toLowerCase();
    if (key === "s") {
      e.preventDefault();
      void writeSystem();
    } else if (key === "/") {
      e.preventDefault();
      const el = e.currentTarget;
      const r = toggleCommentLines(el.value, el.selectionStart, el.selectionEnd);
      editContent(r.text);
      requestAnimationFrame(() => {
        el.setSelectionRange(r.selStart, r.selEnd);
      });
    }
  };

  return (
    <div className="page-scrollable hosts-page">
      {!canWrite && (
        <div className="hosts-admin-banner">
          <ShieldAlert size={15} />
          <span className="hosts-admin-text">{t("adminWarning")}</span>
          <button
            className="btn btn-secondary btn-small"
            onClick={() => void relaunchAsAdmin()}
            disabled={relaunching}
          >
            {relaunching ? <RotateCw size={12} className="spin" /> : <ShieldCheck size={12} />}
            {t("relaunchAdmin")}
          </button>
        </div>
      )}

      <div className="hosts-layout">
        <section className="card hosts-card hosts-editor-card">
          <header className="hosts-card-head">
            <h3 className="hosts-card-title">
              <FileText size={14} />
              {t("systemEditor")}
            </h3>
            {editorDirty && <span className="hosts-unsaved">{t("unsaved")}</span>}
            <span className="hosts-title-meta">{t("statsEntries", hostStats)}</span>
          </header>
          <div className="hosts-editor-wrap">
            <div className="hosts-gutter" ref={gutterRef} aria-hidden>
              {Array.from({ length: lineCount }, (_, i) => (
                <div key={i}>{i + 1}</div>
              ))}
            </div>
            <textarea
              ref={editorRef}
              className="hosts-editor hosts-editor-page"
              value={systemContent}
              onChange={(e) => editContent(e.target.value)}
              onKeyDown={onEditorKeyDown}
              onScroll={(e) => {
                if (gutterRef.current) gutterRef.current.scrollTop = e.currentTarget.scrollTop;
              }}
              wrap="off"
              spellCheck={false}
            />
          </div>
          <footer className="hosts-editor-actions">
            <div className="hosts-actions-group">
              <button className="btn btn-secondary btn-small" onClick={readSystem}>
                <RotateCw size={12} />
                {t("read")}
              </button>
              <button
                className="btn btn-secondary btn-small"
                onClick={() => void handleBackupNow()}
                disabled={backupBusy}
              >
                <Archive size={12} />
                {t("backupNow")}
              </button>
              <span className="hosts-shortcuts">{t("shortcutsHint")}</span>
            </div>
            <div className="hosts-actions-group">
              {editorDirty && (
                <button
                  className="btn btn-secondary btn-small"
                  onClick={() => {
                    editContent(baseline);
                    editorRef.current?.focus();
                  }}
                >
                  <Undo2 size={12} />
                  {t("revert")}
                </button>
              )}
              <button
                className="btn btn-secondary btn-small"
                onClick={handleSaveAsProfile}
                disabled={!systemContent.trim()}
              >
                <Save size={12} />
                {t("saveAsProfile")}
              </button>
              <button
                className="btn btn-primary btn-small"
                onClick={writeSystem}
                disabled={!canWrite || !editorDirty}
                title={!canWrite ? t("cannotWrite") : !editorDirty ? t("noChanges") : undefined}
              >
                <Upload size={12} />
                {t("write")}
              </button>
            </div>
          </footer>
        </section>

        <aside className="hosts-side">
          <section className="card hosts-card">
            <header className="hosts-card-head">
              <h3 className="hosts-card-title">
                <Network size={14} />
                {t("profiles")}
              </h3>
              <button className="btn btn-secondary btn-small hosts-head-btn" onClick={handleAdd}>
                <Plus size={12} />
                {t("addProfile")}
              </button>
            </header>
            {hostProfiles.length === 0 ? (
              <div className="hosts-empty">
                <Globe size={22} strokeWidth={1.5} />
                <p>{t("noProfiles")}</p>
              </div>
            ) : (
              <ul className="hosts-list">
                {hostProfiles.map((p) => {
                  const isCurrent = baselineNorm !== "" && normalizeHosts(p.content) === baselineNorm;
                  return (
                  <li key={p.id} className={`hosts-row${isCurrent ? " is-current" : ""}`}>
                    <span className="hosts-row-icon">
                      {isCurrent ? <CheckCircle2 size={13} /> : <Network size={13} />}
                    </span>
                    <div className="hosts-row-info" title={p.content.slice(0, 400)}>
                      <div className="hosts-row-name">
                        {p.name}
                        {isCurrent && <span className="hosts-current-tag">{t("current")}</span>}
                      </div>
                      <div className="hosts-row-sub">
                        {t("entriesCount", {
                          count: p.content
                            .split("\n")
                            .filter((l) => l.trim() && !l.trim().startsWith("#")).length,
                        })}
                      </div>
                    </div>
                    <div className="hosts-row-actions">
                      <button
                        type="button"
                        className="hosts-icon-btn"
                        onClick={() => handleEdit(p)}
                        title={tc("actions.edit")}
                        aria-label={`${tc("actions.edit")} ${p.name}`}
                      >
                        <Pencil size={12} />
                      </button>
                      <button
                        type="button"
                        className="hosts-icon-btn is-danger"
                        onClick={() => handleDelete(p.id)}
                        title={tc("actions.delete")}
                        aria-label={`${tc("actions.delete")} ${p.name}`}
                      >
                        <Trash2 size={12} />
                      </button>
                      {!isCurrent && (
                        <button
                          className="btn btn-secondary btn-small"
                          onClick={() => applyProfile(p)}
                          disabled={!canWrite}
                          title={!canWrite ? t("cannotWrite") : undefined}
                        >
                          {t("apply")}
                        </button>
                      )}
                    </div>
                  </li>
                  );
                })}
              </ul>
            )}
          </section>

          <section className="card hosts-card">
            <header className="hosts-card-head">
              <h3 className="hosts-card-title">
                <HardDrive size={14} />
                {t("backups")}
              </h3>
              {backups.length > 0 && <span className="hosts-title-meta">{backups.length}</span>}
            </header>
            {backups.length === 0 ? (
              <div className="hosts-empty">
                <HardDrive size={22} strokeWidth={1.5} />
                <p>{t("noBackups")}</p>
              </div>
            ) : (
              <ul className="hosts-list">
                {backups.map((b) => (
                  <li key={b.path} className="hosts-row">
                    <span className="hosts-row-icon">
                      <History size={13} />
                    </span>
                    <div className="hosts-row-info" title={b.path}>
                      <div className="hosts-row-name hosts-row-mono">{formatBackupName(b.name)}</div>
                    </div>
                    <div className="hosts-row-actions">
                      <button
                        type="button"
                        className="hosts-icon-btn is-danger"
                        onClick={() => void deleteBackup(b.path, b.name)}
                        title={tc("actions.delete")}
                        aria-label={`${tc("actions.delete")} ${formatBackupName(b.name)}`}
                      >
                        <Trash2 size={12} />
                      </button>
                      <button
                        className="btn btn-secondary btn-small"
                        onClick={() => restoreBackup(b.path, b.name)}
                        disabled={!canWrite}
                        title={!canWrite ? t("cannotWrite") : undefined}
                      >
                        {t("restore")}
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </aside>
      </div>

      {showModal && (
        <div className="modal-overlay" onClick={() => setShowModal(false)}>
          <div
            className="modal hosts-profile-modal"
            onClick={(e) => e.stopPropagation()}
          >
            <ModalTitleRow
              title={editing ? t("editProfile") : t("addProfile")}
              onClose={() => setShowModal(false)}
            />
            <div className="hosts-profile-modal-body">
              <div className="input-group">
                <label className="input-label">{t("name")}</label>
                <input
                  className="input-field"
                  value={formData.name}
                  onChange={(e) => setFormData({ ...formData, name: e.target.value })}
                />
              </div>
              <div className="input-group hosts-profile-content-group">
                <label className="input-label">{t("content")}</label>
                <textarea
                  className="hosts-editor hosts-profile-editor"
                  value={formData.content}
                  onChange={(e) => setFormData({ ...formData, content: e.target.value })}
                  spellCheck={false}
                />
              </div>
            </div>
            <div className="modal-actions">
              <button className="btn btn-secondary" onClick={() => setShowModal(false)}>
                {tc("actions.cancel")}
              </button>
              <button
                className="btn btn-primary"
                onClick={handleSubmit}
                disabled={!formData.name.trim() || !formData.content.trim()}
              >
                {editing ? tc("actions.save") : tc("actions.add")}
              </button>
            </div>
          </div>
        </div>
      )}

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <XCircle size={16} />}
          </span>
          {message.text}
        </div>
      )}
    </div>
  );
}

export default HostsManager;
