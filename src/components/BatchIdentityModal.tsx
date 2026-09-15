import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import { useGlobalStore } from "../core/store";
import ModalTitleRow from "./ModalTitleRow";
import type { GitAccount } from "../core/types";

type Props = {
  selectedCount: number;
  onClose: () => void;
  onPick: (account: GitAccount) => Promise<void>;
};

/** Pick one saved account to apply to multiple selected repos. */
function BatchIdentityModal({ selectedCount, onClose, onPick }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");
  const accounts = useGlobalStore((s) => s.git.accounts);
  const [busyId, setBusyId] = useState<string | null>(null);

  const handlePick = async (account: GitAccount) => {
    if (busyId) return;
    setBusyId(account.id);
    try {
      await onPick(account);
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal batch-identity-modal" onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow
          title={t("batch.bindTitle", { count: selectedCount })}
          onClose={onClose}
          disabled={!!busyId}
        />
        {accounts.length === 0 ? (
          <div className="runtime-muted">{t("batch.noAccounts")}</div>
        ) : (
          <ul className="batch-identity-list">
            {accounts.map((a) => (
              <li key={a.id}>
                <button
                  type="button"
                  className="batch-identity-item"
                  disabled={!!busyId}
                  onClick={() => void handlePick(a)}
                >
                  <span className="commit-identity-avatar" style={{ background: a.color }}>
                    {a.name.charAt(0).toUpperCase()}
                  </span>
                  <span className="batch-identity-meta">
                    <span className="batch-identity-name">{a.name}</span>
                    <span className="runtime-muted">{a.email}</span>
                  </span>
                  {busyId === a.id ? <Loader2 size={14} className="spin" /> : null}
                </button>
              </li>
            ))}
          </ul>
        )}
        <div className="modal-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose} disabled={!!busyId}>
            {tc("actions.cancel")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default BatchIdentityModal;
