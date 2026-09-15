import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalStore } from "../core/store";
import ModalTitleRow from "./ModalTitleRow";

type Props = {
  repoPath: string;
  onClose: () => void;
  onSaved: () => void;
};

function isValidEmail(email: string): boolean {
  const parts = email.trim().split("@");
  return parts.length === 2 && parts[0].length > 0 && parts[1].length > 0;
}

/**
 * 仓库身份绑定弹窗：为指定仓库设置预设身份（存 git_repo_configs 表）。
 * 只做存储，不直接写 git config；实际应用由仓库页「应用预设」或提交页一键完成。
 */
function RepoBindingModal({ repoPath, onClose, onSaved }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");

  const accounts = useGlobalStore((s) => s.git.accounts);
  const repoConfigs = useGlobalStore((s) => s.git.repoConfigs);
  const loadAccounts = useGlobalStore((s) => s.loadAccounts);
  const loadRepoConfigs = useGlobalStore((s) => s.loadRepoConfigs);
  const addRepoConfig = useGlobalStore((s) => s.addRepoConfig);
  const updateRepoConfig = useGlobalStore((s) => s.updateRepoConfig);
  const invokeGetRepoGitConfig = useGlobalStore((s) => s.invokeGetRepoGitConfig);
  const invokeSetRepoGitConfig = useGlobalStore((s) => s.invokeSetRepoGitConfig);

  const existing = repoConfigs.find((c) => c.path === repoPath);

  const [userName, setUserName] = useState("");
  const [email, setEmail] = useState("");
  const [accountId, setAccountId] = useState("");
  const [applyNow, setApplyNow] = useState(true); // 默认保存即应用
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  // 初始化：已有绑定则带入；否则读取当前实际身份作为起点
  useEffect(() => {
    void loadAccounts();
    void loadRepoConfigs();
  }, [loadAccounts, loadRepoConfigs]);

  useEffect(() => {
    if (existing) {
      setUserName(existing.userName);
      setEmail(existing.email);
      setAccountId(existing.accountId || "");
      return;
    }
    let cancelled = false;
    invokeGetRepoGitConfig(repoPath)
      .then(([name, addr]) => {
        if (cancelled) return;
        setUserName(name || "");
        setEmail(addr || "");
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [existing, invokeGetRepoGitConfig, repoPath]);

  const applyAccount = useCallback(
    (id: string) => {
      setAccountId(id);
      const acc = accounts.find((a) => a.id === id);
      if (acc) {
        setUserName(acc.name);
        setEmail(acc.email);
      }
    },
    [accounts]
  );

  const handleSave = async () => {
    const name = userName.trim();
    const addr = email.trim();
    if (!name || !addr) {
      setError(t("repos.nameEmailRequired"));
      return;
    }
    if (!isValidEmail(addr)) {
      setError(t("repos.invalidEmail"));
      return;
    }
    setSaving(true);
    setError("");
    try {
      if (existing) {
        await updateRepoConfig(repoPath, {
          userName: name,
          email: addr,
          accountId: accountId || undefined,
        });
      } else {
        const projectName = repoPath.split(/[/\\]/).filter(Boolean).pop() || repoPath;
        await addRepoConfig({
          path: repoPath,
          name: projectName,
          userName: name,
          email: addr,
          accountId: accountId || undefined,
        });
      }
      // 保存即应用：直接写仓库级 git config，消除"存了但没生效"的断链
      if (applyNow) {
        await invokeSetRepoGitConfig(repoPath, name, addr);
      }
      onSaved();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow
          title={existing ? t("repos.editBindingTitle") : t("repos.bindTitle")}
          onClose={onClose}
        />
        <div className="runtime-muted repos-modal-path" title={repoPath}>
          {repoPath}
        </div>

        {accounts.length > 0 && (
          <div className="input-group">
            <label className="input-label">{t("repos.fromAccount")}</label>
            <select
              className="input-field"
              value={accountId}
              onChange={(e) => applyAccount(e.target.value)}
            >
              <option value="">…</option>
              {accounts.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.note?.trim()
                    ? `${a.name} <${a.email}> · ${a.note.trim()}`
                    : `${a.name} <${a.email}>`}
                </option>
              ))}
            </select>
          </div>
        )}

        <div className="input-group">
          <label className="input-label">{t("repos.userName")}</label>
          <input
            className="input-field"
            value={userName}
            onChange={(e) => {
              setUserName(e.target.value);
              setAccountId("");
            }}
          />
        </div>
        <div className="input-group">
          <label className="input-label">{t("repos.email")}</label>
          <input
            className="input-field"
            value={email}
            onChange={(e) => {
              setEmail(e.target.value);
              setAccountId("");
            }}
          />
        </div>

        <label className="repos-apply-now">
          <input
            type="checkbox"
            checked={applyNow}
            onChange={(e) => setApplyNow(e.target.checked)}
          />
          <span>{t("repos.applyNow")}</span>
        </label>

        {error && <div className="repos-modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn btn-secondary" onClick={onClose}>
            {tc("actions.cancel")}
          </button>
          <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
            {saving ? t("repos.saving") : tc("actions.save")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default RepoBindingModal;
