import { useState } from "react";
import { useTranslation } from "react-i18next";
import { CalendarCheck, Check, Loader2, Pencil, RefreshCw, ShieldAlert, Trash2, X } from "lucide-react";
import {
  wbCheckinNow,
  wbDeleteAccount,
  wbProbeAccount,
  wbUpdateAccount,
  formatExpiry,
  type WbAccount,
  type WbAccountStatus,
} from "../../lib/workbuddy";
import { useConfirm } from "../ConfirmModal";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

interface Props {
  accounts: WbAccount[];
  loading: boolean;
  protocolConfigured: boolean;
  onChanged: () => Promise<void> | void;
  onFlash: (kind: "success" | "error", text: string) => void;
  onAdd: () => void;
  onEdit: (account: WbAccount) => void;
}

const STATUS_KEYS: Record<WbAccountStatus, string> = {
  active: "statusActive",
  unverified: "statusUnverified",
  expired: "statusExpired",
  banned: "statusBanned",
};

function formatTimestamp(ms: number | null): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleString(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export default function AccountPoolTable({
  accounts,
  loading,
  protocolConfigured,
  onChanged,
  onFlash,
  onAdd,
  onEdit,
}: Props) {
  const { t } = useTranslation("workbuddy");
  const confirm = useConfirm();
  const translateError = useInvokeErrorTranslator();
  const [busyId, setBusyId] = useState<number | null>(null);
  const [verifyingId, setVerifyingId] = useState<number | null>(null);

  const probe = async (account: WbAccount) => {
    setVerifyingId(account.id);
    try {
      const result = await wbProbeAccount(account.id);
      if (result.ok) onFlash("success", t("verifyResultActive"));
      else if (result.status === "expired") onFlash("error", t("verifyResultExpired"));
      else onFlash("error", t("verifyResultUnverified", { reason: result.reason ?? "—" }));
      await onChanged();
    } catch (e) {
      onFlash("error", t("verifyFailed", { message: translateError(e) }));
    } finally {
      setVerifyingId(null);
    }
  };

  const toggleEnabled = async (account: WbAccount) => {
    setBusyId(account.id);
    try {
      await wbUpdateAccount(account.id, { enabled: !account.enabled });
      await onChanged();
    } catch (e) {
      onFlash("error", translateError(e));
    } finally {
      setBusyId(null);
    }
  };

  const remove = async (account: WbAccount) => {
    const ok = await confirm({
      title: t("deleteConfirmTitle"),
      message: t("deleteConfirmMessage", { label: account.label }),
      icon: "danger",
    });
    if (!ok) return;
    setBusyId(account.id);
    try {
      await wbDeleteAccount(account.id);
      await onChanged();
      onFlash("success", t("deletedToast", { label: account.label }));
    } catch (e) {
      onFlash("error", translateError(e));
    } finally {
      setBusyId(null);
    }
  };

  const checkin = async (account: WbAccount) => {
    setBusyId(account.id);
    try {
      const results = await wbCheckinNow(account.id);
      const result = results[0];
      if (!result) onFlash("error", t("checkinNothingToDo"));
      else if (result.ok) onFlash("success", `${account.label}: ${result.message}`);
      else onFlash("error", `${account.label}: ${result.message}`);
      await onChanged();
    } catch (e) {
      onFlash("error", translateError(e));
    } finally {
      setBusyId(null);
    }
  };

  if (loading) {
    return (
      <div className="card wb-card">
        <div className="card-body wb-loading">
          <Loader2 size={14} className="spin" />
        </div>
      </div>
    );
  }

  if (accounts.length === 0) {
    return (
      <div className="card wb-card">
        <div className="card-title">
          <span>{t("accountPool")}</span>
        </div>
        <div className="card-body">
          <div className="empty-state">
            <p>{t("emptyTitle")}</p>
            <p>{t("emptyBody")}</p>
            <button type="button" className="btn btn-primary btn-small" onClick={onAdd}>
              {t("manualAdd")}
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="card wb-card">
      <div className="card-title">
        <span>{t("accountPool")}</span>
        <span className="card-title-badge">{accounts.length}</span>
      </div>
      <div className="card-body">
        <table className="wb-table">
          <thead>
            <tr>
              <th>{t("label")}</th>
              <th>{t("credential")}</th>
              <th>{t("status")}</th>
              <th>{t("expiry")}</th>
              <th>{t("lastCheckin")}</th>
              <th className="wb-col-actions">{t("verify")}</th>
            </tr>
          </thead>
          <tbody>
            {accounts.map((account) => {
              const expiry = formatExpiry(account.expUnix);
              const busy = busyId === account.id;
              return (
                <tr key={account.id} className={!account.enabled ? "wb-row-disabled" : undefined}>
                  <td className="wb-cell-label">
                    <span className="wb-label-text">{account.label}</span>
                    {account.email && <span className="wb-sub-text">{account.email}</span>}
                    {account.notes && <span className="wb-sub-text">{account.notes}</span>}
                  </td>
                  <td className="wb-cell-credential">
                    <span className="mono">{account.credentialType}</span>
                    <span className="mono wb-sub-text">{account.credentialPreview}</span>
                  </td>
                  <td>
                    <span className={`wb-status wb-status-${account.status}`}>
                      {account.status === "expired" && <ShieldAlert size={11} />}
                      {t(STATUS_KEYS[account.status] ?? account.status)}
                    </span>
                    {account.checkinFailCount > 0 && (
                      <span className="wb-sub-text">
                        {t("failCount")}: {account.checkinFailCount}
                      </span>
                    )}
                  </td>
                  <td>
                    <span className={`wb-expiry wb-expiry-${expiry.tone}`}>{expiry.text}</span>
                  </td>
                  <td className="wb-cell-time">{formatTimestamp(account.lastCheckinAt)}</td>
                  <td className="wb-cell-actions">
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => onEdit(account)}
                      disabled={busy}
                      title={t("editAccount")}
                    >
                      <Pencil size={12} />
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => void probe(account)}
                      disabled={!protocolConfigured || verifyingId !== null}
                      title={protocolConfigured ? t("verify") : t("protocolMissing")}
                    >
                      {verifyingId === account.id ? (
                        <Loader2 size={12} className="spin" />
                      ) : (
                        <RefreshCw size={12} />
                      )}
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => void checkin(account)}
                      disabled={busy || !account.enabled}
                      title={t("checkinNow")}
                    >
                      <CalendarCheck size={12} />
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => void toggleEnabled(account)}
                      disabled={busy}
                      title={account.enabled ? t("disabled") : t("enabled")}
                    >
                      {account.enabled ? <X size={12} /> : <Check size={12} />}
                    </button>
                    <button
                      type="button"
                      className="btn btn-danger btn-small"
                      onClick={() => void remove(account)}
                      disabled={busy}
                      title={t("delete")}
                    >
                      <Trash2 size={12} />
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}
