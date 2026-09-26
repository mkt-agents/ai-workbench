import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, Loader2, Plus, Trash2, X } from "lucide-react";
import {
  wbIpAdd,
  wbIpDelete,
  wbIpList,
  wbIpSetEnabled,
  type WbIpRule,
  type WbIpRuleKind,
} from "../../lib/workbuddy";
import { useGlobalStore } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

/**
 * IP 管控。网关默认只绑 127.0.0.1，此时本机永远放行、外部根本连不上，
 * 规则要等局域网模式开启才有实际意义——界面里把这一点说明白。
 */
export default function IpControlPanel({ lanEnabled, port }: { lanEnabled: boolean; port: number }) {
  const { t } = useTranslation("workbuddy");
  const confirm = useConfirm();
  const translateError = useInvokeErrorTranslator();
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [rules, setRules] = useState<WbIpRule[]>([]);
  const [loading, setLoading] = useState(true);
  const [kind, setKind] = useState<WbIpRuleKind>("allow");
  const [target, setTarget] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setRules(await wbIpList());
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

  const add = async () => {
    const value = target.trim();
    if (!value) {
      setError(t("ipRequired"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await wbIpAdd({ kind, ipOrCidr: value, note: note.trim() || null });
      setTarget("");
      setNote("");
      await refresh();
    } catch (e) {
      setError(translateError(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (row: WbIpRule) => {
    const ok = await confirm({
      title: t("ipDelete"),
      message: t("ipDeleteConfirm", { target: row.ipOrCidr }),
      icon: "danger",
    });
    if (!ok) return;
    await wbIpDelete(row.id).catch((e) => setError(translateError(e)));
    await refresh();
  };

  const firewallCommand = `netsh advfirewall firewall add rule name="AIWorkbench-WorkBuddy" dir=in action=allow protocol=TCP localport=${port} profile=private`;

  return (
    <div className="card wb-card">
      <div className="card-title">
        <span>{t("ipTitle")}</span>
        <span className="card-title-badge">{rules.length}</span>
      </div>
      <div className="card-body">
        <p className="wb-hint">{lanEnabled ? t("ipHintLan") : t("ipHintLocal")}</p>

        <div className="wb-ip-form">
          <div className="wb-segmented">
            {(["allow", "deny"] as const).map((k) => (
              <button
                key={k}
                type="button"
                className={`wb-segment ${kind === k ? "is-active" : ""}`}
                onClick={() => setKind(k)}
                disabled={busy}
              >
                {t(k === "allow" ? "ipAllow" : "ipDeny")}
              </button>
            ))}
          </div>
          <input
            className="input-field wb-ip-target mono"
            value={target}
            placeholder={t("ipPlaceholder")}
            spellCheck={false}
            onChange={(e) => setTarget(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void add();
            }}
            disabled={busy}
          />
          <input
            className="input-field"
            value={note}
            placeholder={t("ipNotePlaceholder")}
            onChange={(e) => setNote(e.target.value)}
            disabled={busy}
          />
          <button type="button" className="btn btn-primary" onClick={() => void add()} disabled={busy}>
            {busy ? <Loader2 size={13} className="spin" /> : <Plus size={13} />}
            <span>{t("ipAdd")}</span>
          </button>
        </div>

        {error && <div className="wb-inline-error" role="alert">{error}</div>}

        {loading ? (
          <div className="wb-loading">
            <Loader2 size={14} className="spin" />
          </div>
        ) : rules.length === 0 ? (
          <p className="wb-hint">{t("ipEmpty")}</p>
        ) : (
          <table className="wb-table">
            <thead>
              <tr>
                <th>{t("ipRuleKind")}</th>
                <th>{t("ipRuleTarget")}</th>
                <th>{t("notes")}</th>
                <th className="wb-col-actions" />
              </tr>
            </thead>
            <tbody>
              {rules.map((row) => (
                <tr key={row.id} className={row.enabled ? undefined : "wb-row-disabled"}>
                  <td>
                    <span className={`wb-status wb-status-${row.enabled ? "active" : "unverified"}`}>
                      {t(row.kind === "allow" ? "ipAllow" : "ipDeny")}
                    </span>
                  </td>
                  <td className="mono">{row.ipOrCidr}</td>
                  <td className="wb-sub-text">{row.note ?? "—"}</td>
                  <td className="wb-cell-actions">
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => {
                        void wbIpSetEnabled(row.id, !row.enabled).then(refresh).catch((e) => setError(translateError(e)));
                      }}
                      title={row.enabled ? t("disabled") : t("enabled")}
                    >
                      {row.enabled ? <X size={12} /> : <Check size={12} />}
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

        {lanEnabled && (
          <div className="wb-firewall">
            <p className="wb-hint">{t("ipFirewallHint")}</p>
            <div className="wb-base-row">
              <code className="wb-base-url mono">{firewallCommand}</code>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => void copy(firewallCommand)}
                title={t("ipFirewallCopy")}
              >
                <Copy size={12} />
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
