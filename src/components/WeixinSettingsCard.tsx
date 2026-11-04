import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { QrCode, ShieldCheck } from "lucide-react";
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

/** 通道状态 → 胶囊配色（与企微页共用一套 token）。 */
function statusTone(state: string | undefined): string {
  if (state === "connected") return "is-connected";
  if (state === "degraded") return "is-error";
  if (state === "stopped") return "is-pending";
  return "";
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
              setQrSvg("");
              setQrcode("");
              setQrDetail(t("weixin.qrExpired"));
              // 二维码撤掉后就没有展示位了，过期原因改由底部提醒条说明
              setError(t("weixin.qrExpired"));
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

  /** 取消扫码：停轮询、清掉二维码与提示，回到待绑定态。 */
  const cancelQr = () => {
    stopQrPoll();
    setQrBusy(false);
    setQrcode("");
    setQrSvg("");
    setQrDetail("");
    setNeedVerify(false);
    setVerifyCode("");
  };

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
    <div className="wecom-settings weixin-settings">
      {/* 概览条：状态胶囊 / 通道 / 绑定态在左，重连与总开关在右，说明折叠进「说明」 */}
      <div className="wecom-hero">
        <div className="wecom-hero-main">
          <span
            className={`wecom-status-pill ${statusTone(status?.state)}`}
            title={status?.detail || undefined}
          >
            <i className="wecom-status-dot" aria-hidden />
            {stateLabel}
          </span>
          <span className="wecom-hero-tag">{t("weixin.channelTag")}</span>
          <span className={`weixin-flag${bound ? " is-bound" : ""}`}>
            {bound ? t("weixin.boundChip") : t("weixin.unboundChip")}
          </span>
        </div>
        <div className="wecom-hero-actions">
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={busy || !settings.weixinEnabled}
            onClick={() =>
              void restartWeixinChannel().then(setStatus).catch((e) => setError(String(e)))
            }
          >
            {t("weixin.restart")}
          </button>
          <label className="toggle" title={t("weixin.enabled")}>
            <input
              type="checkbox"
              checked={Boolean(settings.weixinEnabled)}
              disabled={busy || !bound}
              onChange={(e) => void push({ weixinEnabled: e.target.checked })}
            />
            <span className="toggle-slider" />
          </label>
        </div>
        <details className="wecom-hint wecom-hero-hint">
          <summary>
            <span>{t("weixin.pluginHintShort")}</span>
            <span className="wecom-hint-more">{t("wecom.hintMore")}</span>
          </summary>
          <p className="setting-hint">{t("weixin.pluginHint")}</p>
          <p className="setting-hint">{t("weixin.answerReuseHint")}</p>
          {status?.detail ? <p className="setting-hint">{status.detail}</p> : null}
        </details>
      </div>

      <div className="wecom-rows">
        {/* 1 · 扫码绑定：未绑定是引导态，扫码中是二维码 + 配对输入 */}
        <div className="wecom-block-card">
          <div className="wecom-block-head">
            <div className="wecom-block-head-main">
              <span className="wecom-block-icon" aria-hidden>
                <QrCode size={14} />
              </span>
              <span className="wecom-block-title">{t("weixin.qrBind")}</span>
            </div>
            {qrBusy ? (
              <button type="button" className="btn btn-secondary btn-small" onClick={cancelQr}>
                {t("weixin.qrCancel")}
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-primary btn-small"
                disabled={busy}
                onClick={() => void beginQr()}
              >
                {bound ? t("weixin.rebind") : t("weixin.qrStart")}
              </button>
            )}
          </div>

          {qrBusy ? (
            <div className="wecom-qr-stage">
              <div className="wecom-qr-frame">
                {qrSvg ? (
                  <div
                    className="wecom-qr-img wecom-qr-svg"
                    role="img"
                    aria-label="WeChat QR"
                    dangerouslySetInnerHTML={{ __html: qrSvg }}
                  />
                ) : (
                  <div className="wecom-qr-skeleton" aria-hidden>
                    <QrCode size={30} />
                  </div>
                )}
              </div>
              <div className="wecom-qr-side">
                <p className="wecom-qr-title">{t("weixin.qrScanning")}</p>
                <p className="setting-hint">{qrDetail || t("weixin.qrWaiting")}</p>
                {needVerify ? (
                  <input
                    className="input-field wecom-qr-verify"
                    value={verifyCode}
                    placeholder={t("weixin.verifyPlaceholder")}
                    onChange={(e) => setVerifyCode(e.target.value.trim())}
                  />
                ) : null}
                {qrcode ? (
                  <p className="setting-hint">
                    <code className="tabular-nums">{qrcode.slice(0, 10)}…</code>
                  </p>
                ) : null}
              </div>
            </div>
          ) : (
            <div className={`weixin-empty${bound ? " is-bound" : ""}`}>
              <span className="weixin-empty-icon" aria-hidden>
                {bound ? <ShieldCheck size={20} /> : <QrCode size={20} />}
              </span>
              <div className="weixin-empty-text">
                <p className="weixin-empty-title">
                  {bound ? t("weixin.bindTitle") : t("weixin.empty")}
                </p>
                <p className="setting-hint">
                  {bound ? t("weixin.qrRebindHint") : t("weixin.qrBindHint")}
                </p>
              </div>
            </div>
          )}
        </div>

        {/* 2 · 机器人信息：只读凭证归到一张卡，值用胶囊展示而不是空输入框 */}
        <div className="wecom-block-card is-split">
          <div className="wecom-block-head">
            <div className="wecom-block-head-main">
              <span className="wecom-block-icon" aria-hidden>
                <ShieldCheck size={14} />
              </span>
              <span className="wecom-block-title">{t("weixin.metaTitle")}</span>
            </div>
            {bound ? (
              <button
                type="button"
                className="btn btn-danger btn-small"
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
            ) : null}
          </div>
          <div className="weixin-meta">
            <span className="weixin-meta-label">{t("weixin.botId")}</span>
            <span className={`weixin-meta-value${settings.weixinBotId ? "" : " is-empty"}`}>
              {settings.weixinBotId || "—"}
            </span>
          </div>
          <div className="weixin-meta">
            <span className="weixin-meta-label">{t("weixin.userId")}</span>
            <span className={`weixin-meta-value${settings.weixinUserId ? "" : " is-empty"}`}>
              {settings.weixinUserId || "—"}
            </span>
          </div>
        </div>
      </div>

      {status?.lastError ? (
        <p className="weixin-alert" role="alert">
          {status.lastError}
        </p>
      ) : null}
      {error ? (
        <p className="weixin-alert" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}
