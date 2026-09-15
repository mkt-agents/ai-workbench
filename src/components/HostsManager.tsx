import { useEffect, useMemo, useState, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";
import type { HostProfile } from "../core/types";
import { Network, Globe, HardDrive, Check, XCircle } from "lucide-react";

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
  const invokeWriteSystemHosts = useGlobalStore((s) => s.invokeWriteSystemHosts);
  const invokeListHostBackups = useGlobalStore((s) => s.invokeListHostBackups);
  const invokeRestoreHostBackup = useGlobalStore((s) => s.invokeRestoreHostBackup);

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

  return (
    <div className="page-scrollable">
      {!canWrite && <div className="admin-warning">{t("adminWarning")}</div>}

      <div className="card">
        <div className="card-title">
          {t("systemEditor")}
          {editorDirty && <span className="hosts-unsaved">{t("unsaved")}</span>}
          <span className="hosts-title-meta">
            {t("statsEntries", hostStats)}
          </span>
        </div>
        <textarea
          className="hosts-editor"
          value={systemContent}
          onChange={(e) => {
            readGen.current += 1;
            setSystemContent(e.target.value);
          }}
          spellCheck={false}
        />
        <div className="btn-group">
          <button className="btn btn-secondary" onClick={readSystem}>
            {t("read")}
          </button>
          <button className="btn btn-primary" onClick={writeSystem} disabled={!canWrite}>
            {t("write")}
          </button>
          <button
            className="btn btn-secondary"
            onClick={handleSaveAsProfile}
            disabled={!systemContent.trim()}
            title={t("saveAsProfile")}
          >
            {t("saveAsProfile")}
          </button>
          <button
            className="btn btn-secondary"
            onClick={() => void handleBackupNow()}
            disabled={backupBusy}
            title={t("backupNow")}
          >
            {t("backupNow")}
          </button>
        </div>
      </div>

      <div className="card">
        <div className="card-title">
          {t("profiles")}
          <button
            className="btn btn-primary btn-small"
            style={{ float: "right", marginTop: "-4px" }}
            onClick={handleAdd}
          >
            + {t("addProfile")}
          </button>
        </div>
        {hostProfiles.length === 0 ? (
          <div className="empty-state">
            <div className="empty-state-icon">
              <Globe size={32} strokeWidth={1.5} />
            </div>
            <p>{t("noProfiles")}</p>
          </div>
        ) : (
          <ul className="account-list">
            {hostProfiles.map((p) => (
              <li key={p.id} className="account-item">
                <div className="account-avatar">
                  <Network size={16} />
                </div>
                <div className="account-info" title={p.content.slice(0, 400)}>
                  <div className="account-name">{p.name}</div>
                  <div className="account-email">
                    {t("entriesCount", {
                      count: p.content
                        .split("\n")
                        .filter((l) => l.trim() && !l.trim().startsWith("#")).length,
                    })}
                  </div>
                </div>
                <div className="account-actions">
                  <button
                    className="btn btn-primary btn-small"
                    onClick={() => applyProfile(p)}
                    disabled={!canWrite}
                  >
                    {t("apply")}
                  </button>
                  <button className="btn btn-secondary btn-small" onClick={() => handleEdit(p)}>
                    {tc("actions.edit")}
                  </button>
                  <button className="btn btn-danger btn-small" onClick={() => handleDelete(p.id)}>
                    {tc("actions.delete")}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      <div className="card">
        <div className="card-title">{t("backups")}</div>
        {backups.length === 0 ? (
          <div className="empty-state">
            <div className="empty-state-icon">
              <HardDrive size={32} strokeWidth={1.5} />
            </div>
            <p>{t("noBackups")}</p>
          </div>
        ) : (
          <ul className="account-list">
            {backups.map((b) => (
              <li key={b.path} className="account-item">
                <div className="account-info">
                  <div className="account-name">{b.name}</div>
                </div>
                <div className="account-actions">
                  <button
                    className="btn btn-secondary btn-small"
                    onClick={() => restoreBackup(b.path, b.name)}
                    disabled={!canWrite}
                  >
                    {t("restore")}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      {showModal && (
        <div className="modal-overlay" onClick={() => setShowModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <ModalTitleRow title={editing ? t("editProfile") : t("addProfile")} onClose={() => setShowModal(false)} />
            <div className="input-group">
              <label className="input-label">{t("name")}</label>
              <input
                className="input-field"
                value={formData.name}
                onChange={(e) => setFormData({ ...formData, name: e.target.value })}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("content")}</label>
              <textarea
                className="hosts-editor"
                style={{ height: "200px" }}
                value={formData.content}
                onChange={(e) => setFormData({ ...formData, content: e.target.value })}
                spellCheck={false}
              />
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
