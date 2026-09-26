import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import { wbAddAccount, type WbCredentialType } from "../../lib/workbuddy";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

export interface CredentialPrefill {
  label?: string;
  credentialType?: WbCredentialType;
  credentialRaw?: string;
  notes?: string;
  /** 扫码捕获的来源（header:xxx / storage:xxx / cookie）；有值时表单会提示「可修改」。 */
  captureVia?: string | null;
}

interface Props {
  prefill?: CredentialPrefill;
  /** 保存成功回调：入库 id 交回调用方，让它接着探测凭证有效性。 */
  onSaved: (label: string, id: number) => void;
  onCancel?: () => void;
  /** 保存中禁用宿主的关闭交互（遮罩点击 / X），别让人中途把窗口关掉。 */
  onBusyChange?: (busy: boolean) => void;
  submitLabel?: string;
}

/** 凭证录入表单本体：手动粘贴与扫码捕获核对共用同一份字段与校验。 */
export default function CredentialForm({ prefill, onSaved, onCancel, onBusyChange, submitLabel }: Props) {
  const { t } = useTranslation("workbuddy");
  const translateError = useInvokeErrorTranslator();
  const [label, setLabel] = useState(prefill?.label ?? "");
  const [credentialType, setCredentialType] = useState<WbCredentialType>(prefill?.credentialType ?? "token");
  const [credentialRaw, setCredentialRaw] = useState(prefill?.credentialRaw ?? "");
  const [notes, setNotes] = useState(prefill?.notes ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const setSaving = (next: boolean) => {
    setBusy(next);
    onBusyChange?.(next);
  };

  const submit = async () => {
    const trimmedLabel = label.trim();
    const trimmedCredential = credentialRaw.trim();
    if (!trimmedLabel || !trimmedCredential) {
      setError(t("fieldRequired"));
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const id = await wbAddAccount({
        label: trimmedLabel,
        credentialType,
        credentialRaw: trimmedCredential,
        notes: notes.trim() || null,
      });
      onSaved(trimmedLabel, id);
    } catch (e) {
      setError(translateError(e));
      setSaving(false);
    }
  };

  return (
    <>
      {prefill?.captureVia && (
        <p className="wb-capture-note">{t("captureNote", { via: prefill.captureVia })}</p>
      )}
      {credentialType === "cookie" && <p className="wb-capture-note is-warn">{t("captureCookieWarn")}</p>}
      <div className="input-group">
        <label className="input-label">{t("label")}</label>
        <input
          className="input-field"
          value={label}
          autoFocus
          placeholder={t("labelPlaceholder")}
          onChange={(e) => setLabel(e.target.value)}
          disabled={busy}
        />
      </div>
      <div className="input-group">
        <label className="input-label">{t("credentialType")}</label>
        <div className="wb-segmented">
          {(["token", "cookie"] as const).map((kind) => (
            <button
              key={kind}
              type="button"
              className={`wb-segment ${credentialType === kind ? "is-active" : ""}`}
              onClick={() => setCredentialType(kind)}
              disabled={busy}
            >
              {t(kind === "token" ? "typeToken" : "typeCookie")}
            </button>
          ))}
        </div>
      </div>
      <div className="input-group">
        <label className="input-label">{t("credential")}</label>
        <textarea
          className="input-field wb-credential-input"
          rows={5}
          value={credentialRaw}
          placeholder={t(credentialType === "token" ? "credentialPlaceholderToken" : "credentialPlaceholderCookie")}
          onChange={(e) => setCredentialRaw(e.target.value)}
          disabled={busy}
          spellCheck={false}
        />
        <p className="wb-hint">{t("credentialHint")}</p>
      </div>
      <div className="input-group">
        <label className="input-label">{t("notes")}</label>
        <input className="input-field" value={notes} onChange={(e) => setNotes(e.target.value)} disabled={busy} />
      </div>
      {error && <div className="wb-inline-error" role="alert">{error}</div>}
      <div className="modal-actions">
        {onCancel && (
          <button type="button" className="btn btn-secondary" onClick={onCancel} disabled={busy}>
            {t("cancel")}
          </button>
        )}
        <button type="button" className="btn btn-primary" onClick={() => void submit()} disabled={busy}>
          {busy && <Loader2 size={13} className="spin" />}
          <span>{busy ? t("saving") : (submitLabel ?? t("confirm"))}</span>
        </button>
      </div>
    </>
  );
}
