import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, Loader2, Plus, RefreshCw, Trash2, X } from "lucide-react";
import {
  wbKeyCreate,
  wbKeyDelete,
  wbKeyList,
  wbKeyReset,
  wbKeySetEnabled,
  type WbApiKey,
} from "../../lib/workbuddy";
import { useGlobalStore } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

function formatWhen(ms: number | null): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** 分发出去的网关密钥：明文只在创建/重置那一刻出现一次。 */
export default function ApiKeyTable({ onChanged }: { onChanged?: () => void }) {
  const { t } = useTranslation("workbuddy");
  const confirm = useConfirm();
  const translateError = useInvokeErrorTranslator();
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [keys, setKeys] = useState<WbApiKey[]>([]);
  const [loading, setLoading] = useState(true);
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setKeys(await wbKeyList());
      setError(null);
    } catch (e) {
      setError(translateError(e));
    } finally {
      setLoading(false);
    }
  }, [translateError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const create = async () => {
    const name = label.trim();
    if (!name) {
      setError(t("keyLabelRequired"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const created = await wbKeyCreate(name);
      setLabel("");
      setSecret(created.key);
      setCopied(false);
      await refresh();
      onChanged?.();
    } catch (e) {
      setError(translateError(e));
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (row: WbApiKey) => {
    await wbKeySetEnabled(row.id, !row.enabled).catch((e) => setError(translateError(e)));
    await refresh();
  };

  const reset = async (row: WbApiKey) => {
    const ok = await confirm({
      title: t("keyReset"),
      message: t("keyResetConfirm", { label: row.label }),
      warning: t("keyResetWarning"),
      icon: "warning",
    });
    if (!ok) return;
    try {
      setSecret(await wbKeyReset(row.id));
      setCopied(false);
      await refresh();
    } catch (e) {
      setError(translateError(e));
    }
  };

  const remove = async (row: WbApiKey) => {
    const ok = await confirm({
      title: t("keyDelete"),
      message: t("keyDeleteConfirm", { label: row.label, count: row.callCount }),
      icon: "danger",
    });
    if (!ok) return;
    await wbKeyDelete(row.id).catch((e) => setError(translateError(e)));
    await refresh();
    onChanged?.();
  };

  return (
    <div className="card wb-card">
      <div className="card-title">
        <span>{t("keysTitle")}</span>
        <span className="card-title-badge">{keys.length}</span>
      </div>
      <div className="card-body">
        {secret && (
          <div className="wb-secret-once" role="status">
            <div className="wb-secret-head">
              <span>{t("keyShownOnce")}</span>
              <button type="button" className="btn btn-secondary btn-small" onClick={() => setSecret(null)}>
                <X size={12} />
              </button>
            </div>
            <div className="wb-base-row">
              <code className="wb-base-url mono">{secret}</code>
              <button
                type="button"
                className="btn btn-primary btn-small"
                onClick={() => {
                  void copy(secret);
                  setCopied(true);
                }}
              >
                {copied ? <Check size={12} /> : <Copy size={12} />}
                <span>{copied ? t("keyCopied") : t("keyCopy")}</span>
              </button>
            </div>
          </div>
        )}

        <div className="wb-key-create">
          <input
            className="input-field"
            value={label}
            placeholder={t("keyLabelPlaceholder")}
            onChange={(e) => setLabel(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void create();
            }}
            disabled={busy}
          />
          <button type="button" className="btn btn-primary" onClick={() => void create()} disabled={busy}>
            {busy ? <Loader2 size={13} className="spin" /> : <Plus size={13} />}
            <span>{t("keyCreate")}</span>
          </button>
        </div>

        {error && <div className="wb-inline-error" role="alert">{error}</div>}

        {loading ? (
          <div className="wb-loading">
            <Loader2 size={14} className="spin" />
          </div>
        ) : keys.length === 0 ? (
          <p className="wb-hint">{t("keysEmpty")}</p>
        ) : (
          <table className="wb-table">
            <thead>
              <tr>
                <th>{t("keyLabel")}</th>
                <th>{t("keyValue")}</th>
                <th>{t("keyCalls")}</th>
                <th>{t("keyLastUsed")}</th>
                <th className="wb-col-actions" />
              </tr>
            </thead>
            <tbody>
              {keys.map((row) => (
                <tr key={row.id} className={row.enabled ? undefined : "wb-row-disabled"}>
                  <td className="wb-label-text">
                    {row.label}
                    <span className="wb-sub-text">
                      {t("keyRotatedAt")}: {formatWhen(row.rotatedAt ?? row.createdAt)}
                    </span>
                  </td>
                  <td className="mono wb-sub">{row.keyMasked}</td>
                  <td className="mono">{row.callCount}</td>
                  <td className="wb-cell-time">{formatWhen(row.lastUsedAt)}</td>
                  <td className="wb-cell-actions">
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => void toggle(row)}
                      title={row.enabled ? t("disabled") : t("enabled")}
                    >
                      {row.enabled ? <X size={12} /> : <Check size={12} />}
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => void reset(row)}
                      title={t("keyReset")}
                    >
                      <RefreshCw size={12} />
                    </button>
                    <button
                      type="button"
                      className="btn btn-danger btn-small"
                      onClick={() => void remove(row)}
                      title={t("delete")}
                    >
                      <Trash2 size={12} />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
