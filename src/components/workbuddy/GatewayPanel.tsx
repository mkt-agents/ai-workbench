import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Loader2, Play, Square } from "lucide-react";
import {
  wbGatewayStart,
  wbGatewayStatus,
  wbGatewayStop,
  wbSetLanMode,
  type GatewayStatus,
} from "../../lib/workbuddy";
import { useGlobalStore } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

/** 网关状态卡：端口 / 启停 / 局域网开关 / 接入地址复制。 */
export default function GatewayPanel({ onChanged }: { onChanged?: () => void }) {
  const { t } = useTranslation("workbuddy");
  const confirm = useConfirm();
  const translateError = useInvokeErrorTranslator();
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [port, setPort] = useState("8787");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const next = await wbGatewayStatus();
      setStatus(next);
      setPort(String(next.port));
      setError(null);
    } catch (e) {
      setError(translateError(e));
    }
  }, [translateError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = async (fn: () => Promise<GatewayStatus>) => {
    setBusy(true);
    try {
      setStatus(await fn());
      onChanged?.();
    } catch (e) {
      setError(translateError(e));
    } finally {
      setBusy(false);
    }
  };

  const toggleLan = async (enabled: boolean) => {
    if (enabled) {
      const ok = await confirm({
        title: t("gatewayLan"),
        message: t("gatewayLanWarning"),
        warning: t("gatewayLanConfirm"),
        icon: "warning",
      });
      if (!ok) return;
    }
    await run(() => wbSetLanMode(enabled));
  };

  const baseUrl = status ? `http://127.0.0.1:${status.port}/v1` : "";

  return (
    <div className="wb-gateway-bar">
      <div className="wb-gateway-line">
        <span className="wb-gateway-name">{t("gatewayTitle")}</span>
        <span className={`wb-status wb-status-${status?.running ? "active" : "unverified"}`}>
          {status?.running ? t("gatewayRunning") : t("gatewayStopped")}
        </span>
        <label className="wb-field">
          <span className="input-label">{t("gatewayPort")}</span>
          <input
            className="input-field wb-port-input mono"
            value={port}
            inputMode="numeric"
            disabled={busy || status?.running}
            onChange={(e) => setPort(e.target.value.replace(/\D/g, "").slice(0, 5))}
          />
        </label>
        {status?.running ? (
          <button type="button" className="btn btn-danger" onClick={() => void run(wbGatewayStop)} disabled={busy}>
            <Square size={13} />
            <span>{t("gatewayStop")}</span>
          </button>
        ) : (
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => void run(() => wbGatewayStart(Number(port) || undefined))}
            disabled={busy || !port}
          >
            <Play size={13} />
            <span>{t("gatewayStart")}</span>
          </button>
        )}
        {busy && <Loader2 size={13} className="spin" />}
        <label className="wb-check">
          <input
            type="checkbox"
            checked={status?.lanEnabled ?? false}
            disabled={busy}
            onChange={(e) => void toggleLan(e.target.checked)}
          />
          <span>{t("gatewayLan")}</span>
        </label>
        <div className="wb-facts wb-facts-right">
          <span>
            {t("gatewayKeys")}: <strong>{status?.keyCount ?? 0}</strong>
          </span>
          <span>
            {t("gatewayAccounts")}: <strong>{status?.accountEligible ?? 0}</strong>/{status?.accountTotal ?? 0}
          </span>
        </div>
      </div>

      <div className="wb-gateway-line wb-gateway-sub">
        {status?.running ? (
          <>
            <code className="wb-base-url mono">{baseUrl}</code>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => void copy(baseUrl)}
              title={t("gatewayCopyUrl")}
            >
              <Copy size={12} />
              <span>{t("gatewayCopyUrl")}</span>
            </button>
          </>
        ) : (
          <span className="wb-hint-inline">{t("gatewayStoppedHint")}</span>
        )}
        <span className="wb-hint-inline">{status?.lanEnabled ? t("gatewayBindLan") : t("gatewayBindLocal")}</span>
        {status?.running && status.lanEnabled && <span className="wb-hint-inline">{t("gatewayLanUrlHint")}</span>}
      </div>

      {error && <div className="wb-inline-error" role="alert">{error}</div>}
    </div>
  );
}
