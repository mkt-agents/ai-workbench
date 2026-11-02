import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalStore } from "../core/store";
import {
  applyWeixinChannel,
  fetchWeixinStatus,
  pollWeixinQr,
  restartWeixinChannel,
  startWeixinQr,
  protectWeixinSecret,
  type WeixinStatus,
} from "../lib/weixinChannel";

function Row(props: {
  label: string;
  control: ReactNode;
  hint?: ReactNode;
}) {
  return (
    <div className="setting-item">
      <div className="setting-row">
        <span className="setting-label">{props.label}</span>
        <div className="setting-control">{props.control}</div>
      </div>
      {props.hint ? <div className="setting-hint-block">{props.hint}</div> : null}
    </div>
  );
}

function Hint({ summary }: { summary: string }) {
  return <p className="setting-hint">{summary}</p>;
}

/**
 * 个人微信 iLink Bot：扫码绑定 + 连接状态。
 * 回答引擎 / 文档目录等设置复用企微页，避免两套配置分叉。
 */
export default function WeixinSettingsCard() {
  const { t } = useTranslation("settings");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const mcpServers = useGlobalStore((s) => s.mcpServers);

  const [status, setStatus] = useState<WeixinStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const [qrBusy, setQrBusy] = useState(false);
  const [qrcode, setQrcode] = useState("");
  const [qrSvg, setQrSvg] = useState("");
  const [qrDetail, setQrDetail] = useState("");
  const [needVerify, setNeedVerify] = useState(false);
  const [verifyCode, setVerifyCode] = useState("");
  const [pollBase, setPollBase] = useState("");
  const pollRef = useRef<number | null>(null);

  const refresh = useCallback(async () => {
    try {
      const st = await fetchWeixinStatus();
      setStatus(st);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = window.setInterval(() => void refresh(), 5000);
    return () => window.clearInterval(id);
  }, [refresh]);

  const stopQrPoll = () => {
    if (pollRef.current != null) {
      window.clearInterval(pollRef.current);
      pollRef.current = null;
    }
  };

  useEffect(() => () => stopQrPoll(), []);

  const push = async (
    next: Partial<typeof settings>,
    plainToken?: string,
  ) => {
    const merged = { ...settings, ...next };
    setSettings(next);
    setBusy(true);
    setError("");
    try {
      const st = await applyWeixinChannel({
        settings: merged,
        models: aiModels,
        mcpServers,
        plainToken,
      });
      setStatus(st);
      if (plainToken) {
        // token already encrypted inside apply; persist ciphertext from store after protect
      }
    } catch (e) {
      setError(String(e));
      if (next.weixinEnabled === true) setSettings({ weixinEnabled: false });
    } finally {
      setBusy(false);
    }
  };

  const beginQr = async () => {
    stopQrPoll();
    setQrBusy(true);
    setError("");
    setQrDetail(t("weixin.qrWaiting"));
    setNeedVerify(false);
    setVerifyCode("");
    try {
      const started = await startWeixinQr(settings.weixinBaseUrl);
      setQrcode(started.qrcode);
      setQrSvg(started.qrSvg || "");
      setPollBase((settings.weixinBaseUrl ?? "").trim());
      pollRef.current = window.setInterval(() => {
        void (async () => {
          try {
            const polled = await pollWeixinQr({
              qrcode: started.qrcode,
              verifyCode: verifyCodeRef.current || undefined,
              baseUrl: pollBaseRef.current || undefined,
            });
            setQrDetail(polled.detail);
            setNeedVerify(Boolean(polled.needVerify));
            if (polled.baseUrl) {
              setPollBase(polled.baseUrl);
              pollBaseRef.current = polled.baseUrl;
            }
            if (polled.status === "confirmed" && polled.botToken) {
              stopQrPoll();
              setQrBusy(false);
              setQrcode("");
              setQrSvg("");
              const token = polled.botToken;
              const protectedToken = await protectWeixinSecret(token);
              setSettings({
                weixinBotToken: protectedToken,
                weixinBotId: polled.botId || "",
                weixinUserId: polled.userId || "",
                weixinBaseUrl: polled.baseUrl || settings.weixinBaseUrl || "",
                weixinEnabled: true,
              });
              await push(
                {
                  weixinBotToken: protectedToken,
                  weixinBotId: polled.botId || "",
                  weixinUserId: polled.userId || "",
                  weixinBaseUrl: polled.baseUrl || settings.weixinBaseUrl || "",
                  weixinEnabled: true,
                },
                token,
              );
              setQrDetail(t("weixin.qrSuccess"));
            } else if (polled.status === "expired") {
              stopQrPoll();
              setQrBusy(false);
              setQrDetail(t("weixin.qrExpired"));
            }
          } catch (e) {
            setQrDetail(String(e));
          }
        })();
      }, 2500);
    } catch (e) {
      setQrBusy(false);
      setError(String(e));
    }
  };

  const verifyCodeRef = useRef("");
  const pollBaseRef = useRef("");
  useEffect(() => {
    verifyCodeRef.current = verifyCode;
  }, [verifyCode]);
  useEffect(() => {
    pollBaseRef.current = pollBase;
  }, [pollBase]);

  const bound = Boolean((settings.weixinBotToken ?? "").trim());
  const stateLabel =
    status?.state === "connected"
      ? t("weixin.stateConnected")
      : status?.state === "degraded"
        ? t("weixin.stateDegraded")
        : status?.state === "stopped"
          ? t("weixin.stateStopped")
          : t("weixin.stateOff");

  return (
    <div className="wecom-settings">
      <p className="setting-hint">{t("weixin.pluginHint")}</p>

      {!bound && !qrBusy ? (
        <div className="wecom-empty">
          <p>{t("weixin.empty")}</p>
          <button type="button" className="btn btn-primary" disabled={busy} onClick={() => void beginQr()}>
            {t("weixin.qrStart")}
          </button>
        </div>
      ) : null}

      <Row
        label={t("weixin.qrBind")}
        control={
          <div className="btn-row">
            <button
              type="button"
              className="btn btn-secondary btn-small"
              disabled={busy || qrBusy}
              onClick={() => void beginQr()}
            >
              {qrBusy ? t("weixin.qrWaiting") : t("weixin.qrStart")}
            </button>
            {qrBusy ? (
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => {
                  stopQrPoll();
                  setQrBusy(false);
                  setQrcode("");
                  setQrSvg("");
                  setQrDetail("");
                  setNeedVerify(false);
                }}
              >
                {t("weixin.qrCancel")}
              </button>
            ) : null}
          </div>
        }
        hint={
          <>
            <Hint summary={t("weixin.qrBindHint")} />
            {qrSvg || qrBusy ? (
              <div className="wecom-qr-box">
                <p className="setting-hint">{t("weixin.qrScanning")}</p>
                {qrSvg ? (
                  <div
                    className="wecom-qr-img wecom-qr-svg"
                    role="img"
                    aria-label="WeChat QR"
                    dangerouslySetInnerHTML={{ __html: qrSvg }}
                  />
                ) : null}
                {needVerify ? (
                  <div className="btn-row" style={{ gap: 8 }}>
                    <input
                      className="input-field"
                      value={verifyCode}
                      placeholder={t("weixin.verifyPlaceholder")}
                      onChange={(e) => setVerifyCode(e.target.value.trim())}
                    />
                  </div>
                ) : null}
                {qrDetail ? <p className="setting-hint">{qrDetail}</p> : null}
                {qrcode ? (
                  <p className="setting-hint">
                    <code className="tabular-nums">{qrcode.slice(0, 10)}…</code>
                  </p>
                ) : null}
              </div>
            ) : null}
          </>
        }
      />

      <Row
        label={t("weixin.enabled")}
        control={
          <label className="switch">
            <input
              type="checkbox"
              checked={Boolean(settings.weixinEnabled)}
              disabled={busy || !bound}
              onChange={(e) => void push({ weixinEnabled: e.target.checked })}
            />
            <span className="switch-slider" />
          </label>
        }
        hint={<Hint summary={stateLabel} />}
      />

      <Row
        label={t("weixin.botId")}
        control={
          <input
            className="input-field"
            value={settings.weixinBotId ?? ""}
            readOnly
            placeholder="—"
          />
        }
      />

      <Row
        label={t("weixin.userId")}
        control={
          <input
            className="input-field"
            value={settings.weixinUserId ?? ""}
            readOnly
            placeholder="—"
          />
        }
      />

      <div className="btn-row" style={{ marginTop: 8 }}>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          disabled={busy || !settings.weixinEnabled}
          onClick={() => void restartWeixinChannel().then(setStatus).catch((e) => setError(String(e)))}
        >
          {t("weixin.restart")}
        </button>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          disabled={busy}
          onClick={() => {
            void push({
              weixinEnabled: false,
              weixinBotToken: "",
              weixinBotId: "",
              weixinUserId: "",
            });
          }}
        >
          {t("weixin.unbind")}
        </button>
      </div>

      <p className="setting-hint">{t("weixin.answerReuseHint")}</p>
      {status?.lastError ? <p className="setting-hint is-error">{status.lastError}</p> : null}
      {error ? <p className="setting-hint is-error">{error}</p> : null}
    </div>
  );
}
