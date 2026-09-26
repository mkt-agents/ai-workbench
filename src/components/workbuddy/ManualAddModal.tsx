import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import ModalTitleRow from "../ModalTitleRow";
import CredentialForm from "./CredentialForm";
import { wbUpdateAccount, type WbAccount } from "../../lib/workbuddy";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

interface Props {
  /** 传入则为编辑模式：只改 label/notes，凭证不可改（后端 patch 本就不含凭证）。 */
  editAccount?: WbAccount;
  onClose: () => void;
  onSaved: (label: string) => void;
}

/** 手动粘贴凭证：扫码捕获不可用时的兜底路径，UI 上常驻。 */
export default function ManualAddModal({ editAccount, onClose, onSaved }: Props) {
  const { t } = useTranslation("workbuddy");
  const translateError = useInvokeErrorTranslator();
  const [saving, setSaving] = useState(false);
  const [label, setLabel] = useState(editAccount?.label ?? "");
  const [notes, setNotes] = useState(editAccount?.notes ?? "");
  const [error, setError] = useState<string | null>(null);

  const saveEdit = async () => {
    if (!editAccount) return;
    const trimmed = label.trim();
    if (!trimmed) {
      setError(t("fieldRequired"));
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await wbUpdateAccount(editAccount.id, { label: trimmed, notes: notes.trim() || undefined });
      onSaved(trimmed);
    } catch (e) {
      setError(translateError(e));
      setSaving(false);
    }
  };

  return (
    <div className="modal-overlay" onClick={saving ? undefined : onClose}>
      <div className="modal wb-modal" onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow title={t(editAccount ? "editAccount" : "manualAdd")} onClose={onClose} disabled={saving} />
        {editAccount ? (
          <>
            <div className="input-group">
              <label className="input-label">{t("label")}</label>
              <input
                className="input-field"
                value={label}
                autoFocus
                onChange={(e) => setLabel(e.target.value)}
                disabled={saving}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("notes")}</label>
              <input
                className="input-field"
                value={notes}
                onChange={(e) => setNotes(e.target.value)}
                disabled={saving}
              />
            </div>
            {error && <div className="wb-inline-error" role="alert">{error}</div>}
            <div className="modal-actions">
              <button type="button" className="btn btn-secondary" onClick={onClose} disabled={saving}>
                {t("cancel")}
              </button>
              <button type="button" className="btn btn-primary" onClick={() => void saveEdit()} disabled={saving}>
                {saving && <Loader2 size={13} className="spin" />}
                <span>{saving ? t("saving") : t("confirm")}</span>
              </button>
            </div>
          </>
        ) : (
          <CredentialForm onCancel={onClose} onBusyChange={setSaving} onSaved={(label) => onSaved(label)} />
        )}
      </div>
    </div>
  );
}
