import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Check, ClipboardPaste, Loader2, ScanLine } from "lucide-react";
import {
  WB_CAPTURE_EVENT,
  wbCaptureCbLoginCookies,
  wbCloseCbLoginWindow,
  wbOpenCbLoginWindow,
  wbProbeAccount,
  wbReadCbLoginProbe,
  type CapturedCredential,
  type WbAccountStatus,
  type WbCaptureStats,
  type WbCredentialType,
} from "../../lib/workbuddy";
import { listen } from "@tauri-apps/api/event";
import ModalTitleRow from "../ModalTitleRow";
import CredentialForm from "./CredentialForm";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

type Phase = "opening" | "waiting" | "confirming" | "verifying" | "saved" | "failed";

/** 捕获结果的两个来源：页面脚本回传，或后端直接读 Cookie 库。 */
interface Captured {
  credentialType: WbCredentialType;
  credentialRaw: string;
  via: string;
}

/** 会话 Cookie 至少要像样才值得递到人面前（登录页的 state cookie 短得多）。 */
const MIN_COOKIE_CHARS = 24;
/** 每 4 秒查一次登录窗脚本的自诊断统计：页内会话校验通过才自动抓 Cookie。 */
const COOKIE_POLL_TICKS = 4;

interface Props {
  onClose: () => void;
  onSaved: (label: string, status: WbAccountStatus) => void;
  /** 捕获失败时把用户引到独立的手动粘贴表单。 */
  onFallBackToPaste: () => void;
  /** 失败往往是协议里还没有 loginUrl，给一条直达协议配置的路。 */
  onOpenProtocol?: () => void;
}

/// CodeBuddy renders the QR page itself, so this wizard never draws one — it
/// only drives the login window and waits for the capture bounce.
const WAIT_TIMEOUT_MS = 180_000;

function newBatchId() {
  return Math.random().toString(36).slice(2, 12);
}

export default function ScanAddFlow({ onClose, onSaved, onFallBackToPaste, onOpenProtocol }: Props) {
  const { t } = useTranslation("workbuddy");
  const translateError = useInvokeErrorTranslator();

  const [phase, setPhase] = useState<Phase>("opening");
  /** 分不开：登录窗压根没打开（通常是协议缺 loginUrl）与开了但没抓到凭证。 */
  const [openFailed, setOpenFailed] = useState(false);
  const [captured, setCaptured] = useState<Captured | null>(null);
  const [detail, setDetail] = useState<string | null>(null);
  const [stats, setStats] = useState<WbCaptureStats | null>(null);
  const [addedCount, setAddedCount] = useState(0);
  const [lastLabel, setLastLabel] = useState("");
  const [lastStatus, setLastStatus] = useState<WbAccountStatus>("unverified");
  const [elapsed, setElapsed] = useState(0);
  const batchRef = useRef(newBatchId());
  const deadlineRef = useRef(0);
  const tickRef = useRef(0);
  const statsRef = useRef<WbCaptureStats | null>(null);
  const capturedRef = useRef(false);

  const openWindow = useCallback(async () => {
    setPhase("opening");
    setDetail(null);
    setElapsed(0);
    setOpenFailed(false);
    setCaptured(null);
    setStats(null);
    statsRef.current = null;
    capturedRef.current = false;
    tickRef.current = 0;
    batchRef.current = newBatchId();
    try {
      const url = await wbOpenCbLoginWindow(batchRef.current);
      deadlineRef.current = Date.now() + WAIT_TIMEOUT_MS;
      // 计时器每秒才更新，先按满值显示，否则进窗口第一秒会写着「剩余 0 秒」。
      setElapsed(WAIT_TIMEOUT_MS / 1000);
      setPhase("waiting");
      setDetail(url);
    } catch (e) {
      setPhase("failed");
      setOpenFailed(true);
      setDetail(translateError(e));
    }
  }, [translateError]);

  useEffect(() => {
    void openWindow();
    return () => {
      void wbCloseCbLoginWindow(batchRef.current).catch(() => undefined);
    };
  }, [openWindow]);

  // 会话 Cookie 是 httpOnly 的，页面脚本读不到值，只能读 Cookie 库——但要不要
  // 把它当凭证，先看注入脚本的页内校验：验证不过的 Cookie 纳管了也是废号。
  const grabCookies = useCallback(
    async (manual: boolean) => {
      try {
        const snap = await wbCaptureCbLoginCookies(batchRef.current);
        // 先判登录态：停在登录页时那些 Cookie 只是 state/CSRF 噪声，提示要指对方向。
        if (!snap.loggedIn) {
          if (manual) setDetail(t("scanCookieNotLoggedIn"));
          return false;
        }
        if (snap.truncated || snap.cookieHeader.length < MIN_COOKIE_CHARS) {
          if (manual) setDetail(t("scanCookieUnusable"));
          return false;
        }
        const verified = statsRef.current?.cookieOk === true;
        setCaptured({
          credentialType: "cookie",
          credentialRaw: snap.cookieHeader,
          via: verified ? "verified-cookie" : "cookie-store",
        });
        capturedRef.current = true;
        setPhase("confirming");
        return true;
      } catch (e) {
        if (manual) setDetail(translateError(e));
        return false;
      }
    },
    [t, translateError]
  );

  // 轮询登录窗里的自诊断统计：脚本死活、观察到的请求、页内会话校验结果。
  // 只有校验 200 才自动抓 Cookie；没验证过就静等 token 类捕获或手动操作。
  const pollStats = useCallback(async () => {
    try {
      const rep = await wbReadCbLoginProbe(batchRef.current);
      const s = rep.captureStats;
      if (!s || capturedRef.current) return;
      statsRef.current = s;
      setStats(s);
      if (s.cookieOk) void grabCookies(false);
    } catch {
      // 窗口正在跳转时读取会失败，下一拍再试。
    }
  }, [grabCookies]);

  // Seconds-remaining ticker, so a long wait is visibly counted down.
  useEffect(() => {
    if (phase !== "waiting") return;
    const id = window.setInterval(() => {
      const left = Math.max(0, Math.round((deadlineRef.current - Date.now()) / 1000));
      setElapsed(left);
      tickRef.current += 1;
      if (tickRef.current % COOKIE_POLL_TICKS === 0) void pollStats();
      if (left === 0) {
        setPhase("failed");
        setDetail(t("scanTimeout"));
        void wbCloseCbLoginWindow(batchRef.current).catch(() => undefined);
      }
    }, 1000);
    return () => window.clearInterval(id);
  }, [phase, pollStats, t]);

  // 捕获只负责把凭证递到人面前：入库由下面那张表单的「确认」触发。
  const receive = useCallback(
    (payload: CapturedCredential) => {
      if (payload.batchId !== batchRef.current) return;
      if (payload.truncated || !payload.credentialRaw) {
        setPhase("failed");
        setDetail(t("scanTruncated"));
        return;
      }
      setCaptured({
        credentialType: payload.credentialType === "cookie" ? "cookie" : "token",
        credentialRaw: payload.credentialRaw,
        via: payload.via,
      });
      capturedRef.current = true;
      setPhase("confirming");
    },
    [t]
  );

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void listen<CapturedCredential>(WB_CAPTURE_EVENT, (event) => {
      if (!cancelled) receive(event.payload);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [receive]);

  const confirmSave = async (label: string, id: number) => {
    setPhase("verifying");
    let status: WbAccountStatus = "unverified";
    let reason: string | null = null;
    try {
      const probe = await wbProbeAccount(id);
      status = probe.status;
      reason = probe.reason;
    } catch (e) {
      // 已经入库了，探测失败不该把它报成没加成功。
      reason = translateError(e);
    }
    setLastLabel(label);
    setLastStatus(status);
    setAddedCount((n) => n + 1);
    setPhase("saved");
    setDetail(reason);
    onSaved(label, status);
  };

  const close = () => {
    void wbCloseCbLoginWindow(batchRef.current).catch(() => undefined);
    onClose();
  };

  const busy = phase === "opening" || phase === "verifying";
  const viaLabel = captured
    ? captured.via === "verified-cookie"
      ? t("viaVerifiedCookie")
      : captured.via === "cookie-store"
        ? t("viaCookieStore")
        : captured.via
    : "";
  const probeLabel = stats
    ? stats.cookieOk
      ? t("scanProbeOk")
      : stats.cookieProbe > 0
        ? `HTTP ${stats.cookieProbe}`
        : "…"
    : "";

  return (
    <div className="modal-overlay" onClick={busy ? undefined : close}>
      <div className={`modal wb-modal ${phase === "confirming" ? "wb-modal-wide" : ""}`} onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow
          title={t("scanTitle")}
          onClose={close}
          disabled={busy}
          badge={addedCount > 0 ? <span className="card-title-badge">{addedCount}</span> : undefined}
        />

        <div
          className={`wb-scan-phase wb-scan-${phase}${phase === "saved" && lastStatus !== "active" ? " is-warn" : ""}`}
        >
          {phase === "opening" && (
            <>
              <Loader2 size={16} className="spin" />
              <span>{t("scanOpening")}</span>
            </>
          )}
          {phase === "waiting" && (
            <>
              <ScanLine size={16} />
              <span>{t("scanWaiting", { seconds: elapsed })}</span>
            </>
          )}
          {phase === "confirming" && (
            <>
              <ClipboardPaste size={16} />
              <span>{t("scanConfirm")}</span>
            </>
          )}
          {phase === "verifying" && (
            <>
              <Loader2 size={16} className="spin" />
              <span>{t("scanVerifying")}</span>
            </>
          )}
          {phase === "saved" && (
            <>
              <Check size={16} />
              <span>
                {lastStatus === "active"
                  ? t("scanSaved", { label: lastLabel })
                  : t("scanSavedUnverified", { label: lastLabel })}
              </span>
            </>
          )}
          {phase === "failed" && (
            <>
              <AlertTriangle size={16} />
              <span>{openFailed ? t("scanNotOpened") : t("scanFailed")}</span>
            </>
          )}
        </div>

        {phase === "confirming" && captured ? (
          <CredentialForm
            prefill={{
              label: `${t("scanAutoLabel")} ${addedCount + 1}`,
              credentialType: captured.credentialType,
              credentialRaw: captured.credentialRaw,
              notes: t("scanViaNote", { via: viaLabel }),
              captureVia: viaLabel,
            }}
            submitLabel={t("addAccountSubmit")}
            onCancel={close}
            onSaved={(label, id) => void confirmSave(label, id)}
          />
        ) : (
          <>
            <p className="wb-hint">{t(`scanHelp.${phase}`)}</p>
            {phase === "waiting" && stats && (
              <p className="wb-scan-detail mono">
                {t("scanStats", { calls: stats.calls, auth: stats.auth, probe: probeLabel })}
              </p>
            )}
            {detail && <p className="wb-scan-detail mono">{detail}</p>}

            <div className="modal-actions">
              {phase === "saved" ? (
                <>
                  <button type="button" className="btn btn-secondary" onClick={close}>
                    {t("scanDone")}
                  </button>
                  <button type="button" className="btn btn-primary" onClick={() => void openWindow()}>
                    {t("scanNext")}
                  </button>
                </>
              ) : (
                <>
                  <button type="button" className="btn btn-secondary" onClick={onFallBackToPaste}>
                    {t("manualAdd")}
                  </button>
                  {(phase === "waiting" || phase === "failed") && !openFailed && (
                    <button type="button" className="btn btn-secondary" onClick={() => void grabCookies(true)}>
                      {t("scanGrabCookies")}
                    </button>
                  )}
                  {phase === "failed" && onOpenProtocol && (
                    <button
                      type="button"
                      className={`btn ${openFailed ? "btn-primary" : "btn-secondary"}`}
                      onClick={() => {
                        void wbCloseCbLoginWindow(batchRef.current).catch(() => undefined);
                        onOpenProtocol();
                      }}
                    >
                      {t("protocol")}
                    </button>
                  )}
                  <button
                    type="button"
                    className={`btn ${phase === "failed" && openFailed ? "btn-secondary" : "btn-primary"}`}
                    onClick={close}
                    disabled={busy}
                  >
                    {t("scanDone")}
                  </button>
                </>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  );
}
