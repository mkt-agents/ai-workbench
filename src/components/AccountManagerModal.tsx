import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Loader2, PenLine, Plus, Tag, User } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";
import type { GitAccount } from "../core/types";

type Props = {
  onClose: () => void;
};

function isValidEmail(email: string): boolean {
  const parts = email.trim().split("@");
  return parts.length === 2 && parts[0].length > 0 && parts[1].length > 0;
}

/** Parse a comma/newline-separated host list into clean unique hosts. */
function parseHosts(input: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of input.split(/[,\n]/)) {
    const h = raw.trim().toLowerCase();
    if (h && !seen.has(h)) {
      seen.add(h);
      out.push(h);
    }
  }
  return out;
}

/**
 * 账号管理弹窗：Git 账号 CRUD + 域名绑定。
 * 列表与表单同层切换（view: list | form），不做弹窗套弹窗。
 */
function AccountManagerModal({ onClose }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();
  const accounts = useGlobalStore((s) => s.git.accounts);
  const hostConfigs = useGlobalStore((s) => s.git.hostConfigs);
  const loadAccounts = useGlobalStore((s) => s.loadAccounts);
  const loadHostConfigs = useGlobalStore((s) => s.loadHostConfigs);
  const addAccount = useGlobalStore((s) => s.addAccount);
  const updateAccount = useGlobalStore((s) => s.updateAccount);
  const deleteAccount = useGlobalStore((s) => s.deleteAccount);
  const addHostConfigs = useGlobalStore((s) => s.addHostConfigs);
  const deleteHostConfigsForAccount = useGlobalStore((s) => s.deleteHostConfigsForAccount);
  const deleteRepoConfigsForAccount = useGlobalStore((s) => s.deleteRepoConfigsForAccount);
  const invokeGetGitConfig = useGlobalStore((s) => s.invokeGetGitConfig);
  const invokeSetGitConfig = useGlobalStore((s) => s.invokeSetGitConfig);

  // Global git config (user.name / user.email)
  const [globalGit, setGlobalGit] = useState({ name: "", email: "" });
  const [editingGlobal, setEditingGlobal] = useState(false);
  const [editGlobalForm, setEditGlobalForm] = useState({ name: "", email: "" });
  const [globalSaving, setGlobalSaving] = useState(false);
  const [globalError, setGlobalError] = useState("");

  // 同层视图切换：list（账号列表）/ form（添加/编辑表单）
  const [view, setView] = useState<"list" | "form">("list");
  const [editingAccount, setEditingAccount] = useState<GitAccount | null>(null);
  const [formData, setFormData] = useState({ name: "", email: "", note: "" });
  const [hostsInput, setHostsInput] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    loadAccounts();
    loadHostConfigs();
    // Fetch current global git config
    invokeGetGitConfig("global")
      .then(([name, email]) => {
        setGlobalGit({ name: name || "", email: email || "" });
      })
      .catch(() => {});
  }, [loadAccounts, loadHostConfigs, invokeGetGitConfig]);

  const startEditGlobal = () => {
    setEditGlobalForm({ name: globalGit.name, email: globalGit.email });
    setGlobalError("");
    setEditingGlobal(true);
  };

  const cancelEditGlobal = () => {
    setEditingGlobal(false);
    setGlobalError("");
  };

  const saveGlobalGit = async () => {
    const name = editGlobalForm.name.trim();
    const email = editGlobalForm.email.trim();
    if (!name || !email) {
      setGlobalError(t("accountManager.nameEmailRequired"));
      return;
    }
    if (!isValidEmail(email)) {
      setGlobalError(t("accountManager.invalidEmail"));
      return;
    }
    setGlobalSaving(true);
    setGlobalError("");
    try {
      await invokeSetGitConfig("global", name, email);
      setGlobalGit({ name, email });
      setEditingGlobal(false);
    } catch (e) {
      setGlobalError(String(e));
    } finally {
      setGlobalSaving(false);
    }
  };

  const openAdd = () => {
    setEditingAccount(null);
    setFormData({ name: "", email: "", note: "" });
    setHostsInput("");
    setError("");
    setView("form");
  };

  const openEdit = (account: GitAccount) => {
    setEditingAccount(account);
    setFormData({ name: account.name, email: account.email, note: account.note ?? "" });
    const boundHosts = hostConfigs
      .filter((h) => h.accountId === account.id)
      .map((h) => h.host)
      .join(", ");
    setHostsInput(boundHosts);
    setError("");
    setView("form");
  };

  const backToList = () => {
    setView("list");
    setError("");
  };

  const handleDelete = async (id: string) => {
    const account = accounts.find((a) => a.id === id);
    const ok = await confirm({
      title: t("accountManager.deleteTitle"),
      message: t("accountManager.confirmDelete", { name: account?.name || id }),
      warning: t("accountManager.deleteWarning"),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await deleteAccount(id);
      // Also remove host bindings and path bindings for this account
      await deleteHostConfigsForAccount(id);
      await deleteRepoConfigsForAccount(id);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSubmit = async () => {
    const name = formData.name.trim();
    const email = formData.email.trim();
    const note = formData.note.trim();
    if (!name || !email) {
      setError(t("accountManager.nameEmailRequired"));
      return;
    }
    if (!isValidEmail(email)) {
      setError(t("accountManager.invalidEmail"));
      return;
    }

    const colors = ["#5f8f72", "#6a9a88", "#7a9068", "#4d7a6c", "#8a9e7a", "#5a8578"];

    try {
      let accountId: string;
      if (editingAccount) {
        await updateAccount(editingAccount.id, { name, email, note: note || undefined });
        accountId = editingAccount.id;
      } else {
        const newId = Date.now().toString();
        await addAccount({
          id: newId,
          name,
          email,
          note: note || undefined,
          color: colors[accounts.length % colors.length],
        });
        accountId = newId;
      }
      // Sync host bindings: remove old, add new
      if (editingAccount) {
        await deleteHostConfigsForAccount(editingAccount.id);
      }
      const hosts = parseHosts(hostsInput);
      if (hosts.length > 0) {
        await addHostConfigs(hosts.map((host) => ({ host, accountId })));
      }
      backToList();
    } catch (e) {
      setError(String(e));
    }
  };

  const accountHosts = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const hc of hostConfigs) {
      const list = map.get(hc.accountId) || [];
      list.push(hc.host);
      map.set(hc.accountId, list);
    }
    return map;
  }, [hostConfigs]);

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal repos-accounts-modal" onClick={(e) => e.stopPropagation()}>
        {view === "list" ? (
          <>
            <ModalTitleRow
              title={t("accountManager.title")}
              onClose={onClose}
              badge={
                <span className="card-title-badge">{accounts.length}</span>
              }
            />
            {/* Global git config (email / author) */}
            <div className="account-global-git">
              <div className="account-global-git-header">
                <span className="account-global-git-title">{t("accountManager.globalGitConfig")}</span>
                {!editingGlobal && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={startEditGlobal}
                    title={t("accountManager.editGlobalGit")}
                  >
                    <PenLine size={12} />
                    <span>{t("accountManager.editGlobalGit")}</span>
                  </button>
                )}
              </div>
              {editingGlobal ? (
                <div className="account-global-git-edit">
                  <div className="input-group">
                    <label className="input-label">{t("accountManager.name")}</label>
                    <input
                      className="input-field"
                      value={editGlobalForm.name}
                      onChange={(e) => setEditGlobalForm({ ...editGlobalForm, name: e.target.value })}
                      placeholder={t("accountManager.namePlaceholder")}
                    />
                  </div>
                  <div className="input-group">
                    <label className="input-label">{t("accountManager.email")}</label>
                    <input
                      className="input-field"
                      value={editGlobalForm.email}
                      onChange={(e) => setEditGlobalForm({ ...editGlobalForm, email: e.target.value })}
                      placeholder={t("accountManager.emailPlaceholder")}
                    />
                  </div>
                  {globalError && <div className="repos-modal-error">{globalError}</div>}
                  <div className="modal-actions">
                    <button
                      type="button"
                      className="btn btn-secondary"
                      onClick={cancelEditGlobal}
                      disabled={globalSaving}
                    >
                      {tc("actions.cancel")}
                    </button>
                    <button
                      type="button"
                      className="btn btn-primary"
                      onClick={saveGlobalGit}
                      disabled={globalSaving}
                    >
                      {globalSaving ? <Loader2 size={14} className="spin" /> : <Check size={14} />}
                      <span>{tc("actions.save")}</span>
                    </button>
                  </div>
                </div>
              ) : (
                <div className="account-global-git-view">
                  {globalGit.name || globalGit.email ? (
                    <>
                      <span className="account-global-git-name">{globalGit.name || "—"}</span>
                      <span className="account-global-git-email">&lt;{globalGit.email || "—"}&gt;</span>
                    </>
                  ) : (
                    <span className="runtime-muted">{t("accountManager.noGlobalGit")}</span>
                  )}
                </div>
              )}
            </div>

            {/* Binding rule hint */}
            <div className="account-rule-hint">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>
              <span>{t("accountManager.bindingRule")}</span>
            </div>

            <div className="account-scroll-area">
              {accounts.length === 0 ? (
                <div className="empty-state">
                  <div className="empty-state-icon">
                    <User size={32} strokeWidth={1.5} />
                  </div>
                  <p>{t("accountManager.noAccounts")}</p>
                </div>
              ) : (
                <div className="account-grid">
                  {accounts.map((account) => (
                    <div key={account.id} className="account-card">
                      <div className="account-card-header">
                        <div className="account-avatar" style={{ background: account.color }}>
                          {account.name.charAt(0).toUpperCase()}
                        </div>
                        <div className="account-info">
                          <div className="account-name">{account.name}</div>
                          <div className="account-email">{account.email}</div>
                          {account.note?.trim() ? (
                            <div className="account-note" title={account.note.trim()}>
                              {account.note.trim()}
                            </div>
                          ) : null}
                          {(accountHosts.get(account.id)?.length ?? 0) > 0 && (
                            <div className="account-hosts">
                              <Tag size={11} />
                              {accountHosts.get(account.id)!.map((host) => (
                                <span key={host} className="account-host-tag" title={t("accountManager.hostHint")}>
                                  {host}
                                </span>
                              ))}
                            </div>
                          )}
                        </div>
                      </div>
                      <div className="account-card-actions">
                        <button className="btn btn-secondary btn-small" onClick={() => openEdit(account)}>
                          {tc("actions.edit")}
                        </button>
                        <button className="btn btn-danger btn-small" onClick={() => handleDelete(account.id)}>
                          {tc("actions.delete")}
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
              {error && <div className="repos-modal-error" style={{ margin: "0 16px" }}>{error}</div>}
            </div>

            <div className="account-add-btn-row">
              <button className="btn btn-primary" onClick={openAdd}>
                <Plus size={14} />
                {t("accountManager.addAccount")}
              </button>
            </div>
          </>
        ) : (
          <>
            <ModalTitleRow
              title={editingAccount ? t("accountManager.editAccount") : t("accountManager.addAccount")}
              onClose={backToList}
            />
            <div className="account-form">
              <div className="input-group">
                <label className="input-label">{t("accountManager.name")}</label>
                <input
                  className="input-field"
                  value={formData.name}
                  onChange={(e) => setFormData({ ...formData, name: e.target.value })}
                  placeholder={t("accountManager.namePlaceholder")}
                  autoFocus
                />
              </div>
              <div className="input-group">
                <label className="input-label">{t("accountManager.email")}</label>
                <input
                  className="input-field"
                  value={formData.email}
                  onChange={(e) => setFormData({ ...formData, email: e.target.value })}
                  placeholder={t("accountManager.emailPlaceholder")}
                />
              </div>
              <div className="input-group">
                <label className="input-label">{t("accountManager.note")}</label>
                <textarea
                  className="input-field"
                  rows={2}
                  value={formData.note}
                  onChange={(e) => setFormData({ ...formData, note: e.target.value })}
                  placeholder={t("accountManager.notePlaceholder")}
                  style={{ resize: "vertical" }}
                />
              </div>
              <div className="input-group">
                <label className="input-label">{t("accountManager.bindHosts")}</label>
                <textarea
                  className="input-field"
                  rows={2}
                  value={hostsInput}
                  onChange={(e) => setHostsInput(e.target.value)}
                  placeholder={t("accountManager.bindHostsPlaceholder")}
                  style={{ resize: "vertical" }}
                />
                <span className="runtime-muted" style={{ fontSize: 12 }}>
                  {t("accountManager.hostHint")}
                </span>
              </div>
              {error && <div className="repos-modal-error">{error}</div>}
              <div className="modal-actions">
                <button className="btn btn-secondary" onClick={backToList}>
                  {t("accountManager.backToList")}
                </button>
                <button className="btn btn-primary" onClick={handleSubmit}>
                  {editingAccount ? tc("actions.save") : tc("actions.add")}
                </button>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default AccountManagerModal;
