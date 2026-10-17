import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Loader2 } from "lucide-react";
import { useGlobalStore } from "../core/store";
import ModalTitleRow from "./ModalTitleRow";
import type { GitAccount } from "../core/types";

type Props = {
  selectedCount: number;
  onClose: () => void;
  onPick: (account: GitAccount) => Promise<void>;
};

/** Pick one saved account, then confirm, to apply it to the selected repos. */
function BatchIdentityModal({ selectedCount, onClose, onPick }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");
  const accounts = useGlobalStore((s) => s.git.accounts);
  const [pickedId, setPickedId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const listRef = useRef<HTMLUListElement>(null);
  const picked = accounts.find((a) => a.id === pickedId) ?? null;

  const confirm = async (account: GitAccount) => {
    if (busy) return;
    setBusy(true);
    try {
      await onPick(account);
    } finally {
      setBusy(false);
    }
  };

  const moveFocus = (from: HTMLButtonElement, delta: number) => {
    const buttons = [
      ...(listRef.current?.querySelectorAll<HTMLButtonElement>("button.batch-identity-item") ?? []),
    ];
    buttons[buttons.indexOf(from) + delta]?.focus();
  };

  return (
    <div className="modal-overlay">
      <div className="modal batch-identity-modal">
        <ModalTitleRow
          title={t("batch.bindHeading")}
          onClose={onClose}
          disabled={busy}
          badge={<span className="card-title-badge">{t("batch.repoBadge", { count: selectedCount })}</span>}
        />
        <p className="input-hint bi-hint">{t("batch.bindHint")}</p>
        {accounts.length === 0 ? (
          <div className="bi-empty">{t("batch.noAccounts")}</div>
        ) : (
          <ul className="batch-identity-list" ref={listRef}>
            {accounts.map((a) => {
              const active = a.id === pickedId;
              return (
                <li key={a.id}>
                  <button
                    type="button"
                    className={`batch-identity-item${active ? " is-active" : ""}`}
                    aria-pressed={active}
                    disabled={busy}
                    onClick={() => setPickedId(a.id)}
                    onKeyDown={(e) => {
                      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                        e.preventDefault();
                        moveFocus(e.currentTarget, e.key === "ArrowDown" ? 1 : -1);
                      }
                      if (e.key === "Enter" && active) {
                        e.preventDefault();
                        void confirm(a);
                      }
                    }}
                  >
                    <span className="commit-identity-avatar" style={{ background: a.color }}>
                      {a.name.charAt(0).toUpperCase()}
                    </span>
                    <span className="batch-identity-meta">
                      <span className="batch-identity-name">{a.name}</span>
                      <span className="bi-email">{a.email}</span>
                    </span>
                    {active ? <Check size={14} className="bi-check" /> : null}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
        <div className="modal-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose} disabled={busy}>
            {tc("actions.cancel")}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={!picked || busy}
            onClick={() => picked && void confirm(picked)}
          >
            {busy ? <Loader2 size={14} className="spin" /> : null}
            {t("batch.bindApply", { count: selectedCount })}
          </button>
        </div>
      </div>
    </div>
  );
}

export default BatchIdentityModal;
