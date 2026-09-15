import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { User } from "lucide-react";
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

/**
 * 账号管理弹窗：Git 账号 CRUD（原 AccountManager 页面改造为弹窗）。
 * 列表与表单同层切换（view: list | form），不做弹窗套弹窗。
 */
function AccountManagerModal({ onClose }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();
  const accounts = useGlobalStore((s) => s.git.accounts);
  const loadAccounts = useGlobalStore((s) => s.loadAccounts);
  const addAccount = useGlobalStore((s) => s.addAccount);
  const updateAccount = useGlobalStore((s) => s.updateAccount);
  const deleteAccount = useGlobalStore((s) => s.deleteAccount);

  // 同层视图切换：list（账号列表）/ form（添加/编辑表单）
  const [view, setView] = useState<"list" | "form">("list");
  const [editingAccount, setEditingAccount] = useState<GitAccount | null>(null);
  const [formData, setFormData] = useState({ name: "", email: "", note: "" });
  const [error, setError] = useState("");

  useEffect(() => {
    loadAccounts();
  }, [loadAccounts]);

  const openAdd = () => {
    setEditingAccount(null);
    setFormData({ name: "", email: "", note: "" });
    setError("");
    setView("form");
  };

  const openEdit = (account: GitAccount) => {
    setEditingAccount(account);
    setFormData({ name: account.name, email: account.email, note: account.note ?? "" });
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
      if (editingAccount) {
        await updateAccount(editingAccount.id, {
          name,
          email,
          note: note || undefined,
        });
      } else {
        await addAccount({
          id: Date.now().toString(),
          name,
          email,
          note: note || undefined,
          color: colors[accounts.length % colors.length],
        });
      }
      backToList();
    } catch (e) {
      setError(String(e));
    }
  };

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
            {error && <div className="repos-modal-error">{error}</div>}
            <div className="btn-group">
              <button className="btn btn-primary" onClick={openAdd}>
                + {t("accountManager.addAccount")}
              </button>
            </div>
          </>
        ) : (
          <>
            <ModalTitleRow
              title={editingAccount ? t("accountManager.editAccount") : t("accountManager.addAccount")}
              onClose={backToList}
            />
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
            {error && <div className="repos-modal-error">{error}</div>}
            <div className="modal-actions">
              <button className="btn btn-secondary" onClick={backToList}>
                {t("accountManager.backToList")}
              </button>
              <button className="btn btn-primary" onClick={handleSubmit}>
                {editingAccount ? tc("actions.save") : tc("actions.add")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default AccountManagerModal;
