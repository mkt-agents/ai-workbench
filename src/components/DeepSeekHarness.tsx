import { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  Play,
  Square,
  Loader2,
  RefreshCw,
  ExternalLink,
  Globe,
  Copy,
  Check,
  ArrowRight,
  Tag,
  Download,
  Undo2,
  X,
} from "lucide-react";

import { useGlobalStore } from "../core/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useConfirm } from "./ConfirmModal";

const DSH_HOST = "127.0.0.1";
const DEFAULT_PORT = 3080;

function dshUrl(port: number) {
  return `http://${DSH_HOST}:${port}`;
}

const DSH_PORT_KEY = "workbench-dsh-port";

function loadStartPort(): number {
  try {
    const n = Number(localStorage.getItem(DSH_PORT_KEY));
    if (Number.isInteger(n) && n > 0 && n < 65536) return n;
  } catch {
    /* ignore */
  }
  return DEFAULT_PORT;
}

function DeepSeekHarness() {
  const { t } = useTranslation("ai");
  const invokeStartDsh = useGlobalStore((s) => s.invokeStartDsh);
  const invokeStopDsh = useGlobalStore((s) => s.invokeStopDsh);
  const invokeListDsh = useGlobalStore((s) => s.invokeListDsh);
  const invokeRestoreDshAuth = useGlobalStore((s) => s.invokeRestoreDshAuth);
  const invokeInstallDsh = useGlobalStore((s) => s.invokeInstallDsh);
  const invokeUpdateDsh = useGlobalStore((s) => s.invokeUpdateDsh);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeGetDshVersions = useGlobalStore((s) => s.invokeGetDshVersions);
  const loadDshStatus = useGlobalStore((s) => s.loadDshStatus);
  const refreshDshStatus = useGlobalStore((s) => s.refreshDshStatus);
  const confirm = useConfirm();

  const dshNodejsInstalled = useGlobalStore((s) => s.dshNodejsInstalled);
  const dshVersion = useGlobalStore((s) => s.dshVersion);
  const dshLatestVersion = useGlobalStore((s) => s.dshLatestVersion);
  const dshHasUpdate = useGlobalStore((s) => s.dshHasUpdate);

  const [isRunning, setIsRunning] = useState(false);
  const [activePort, setActivePort] = useState(DEFAULT_PORT);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [status, setStatus] = useState<string>("");
  const [iframeKey, setIframeKey] = useState(0);
  const [iframeLoading, setIframeLoading] = useState(false);
  const [copied, setCopied] = useState(false);
  const [updating, setUpdating] = useState(false);
  const [updateProgress, setUpdateProgress] = useState(0);
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);

  const [restoringAuth, setRestoringAuth] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [installProgress, setInstallProgress] = useState(0);
  const [installMsg, setInstallMsg] = useState<string | null>(null);

  const [versions, setVersions] = useState<string[]>([]);
  const [selectedVersion, setSelectedVersion] = useState<string>("");
  /** Port for the next start; persisted so "port already in use" is fixable in-UI. */
  const [startPort, setStartPort] = useState<number>(() => loadStartPort());
  const startPortValid = Number.isInteger(startPort) && startPort > 0 && startPort < 65536;
  const [alertBanner, setAlertBanner] = useState<string | null>(null);
  const [showOnboarding, setShowOnboarding] = useState(
    () => localStorage.getItem("workbench-onboarding-dismissed") !== "1"
  );

  const statusTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const clearStatusAfter = useCallback((delay = 3000) => {
    if (statusTimerRef.current) clearTimeout(statusTimerRef.current);
    statusTimerRef.current = setTimeout(() => {
      setStatus("");
      statusTimerRef.current = null;
    }, delay);
  }, []);

  const showCriticalAlert = useCallback((text: string) => {
    setAlertBanner(text);
  }, []);

  const dismissOnboarding = () => {
    localStorage.setItem("workbench-onboarding-dismissed", "1");
    setShowOnboarding(false);
  };

  const iframeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const iframeSettleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const iframeElRef = useRef<HTMLIFrameElement | null>(null);
  /** One soft remount after first paint — DSH often returns 200 before SPA is ready (white screen). */
  const autoReloadDoneRef = useRef(false);
  const startCancelledRef = useRef(false);
  const pollAbortRef = useRef(false);

  const clearIframeTimers = useCallback(() => {
    if (iframeTimerRef.current) {
      clearTimeout(iframeTimerRef.current);
      iframeTimerRef.current = null;
    }
    if (iframeSettleTimerRef.current) {
      clearTimeout(iframeSettleTimerRef.current);
      iframeSettleTimerRef.current = null;
    }
  }, []);

  const remountIframe = useCallback(() => {
    setIframeLoading(true);
    setIframeKey((k) => k + 1);
  }, []);

  useEffect(() => {
    if (dshNodejsInstalled === false) {
      showCriticalAlert(t("dsh.nodejsRequired"));
    }
  }, [dshNodejsInstalled, showCriticalAlert, t]);

  const serviceUrl = dshUrl(activePort);

  const checkStatus = useCallback(async () => {
    try {
      const list = await invokeListDsh();
      if (list.length > 0) {
        const instance = list.find((i) => i.port === DEFAULT_PORT) ?? list[0];
        setActivePort(instance.port);
        setIsRunning(true);
        setStatus(t("dsh.running"));
        // Show mask + remount so we don't stick on a blank first paint.
        autoReloadDoneRef.current = false;
        setIframeLoading(true);
        setIframeKey((k) => k + 1);
      } else {
        setIsRunning(false);
        setIframeLoading(false);
      }
    } catch {
      setIsRunning(false);
      setIframeLoading(false);
    }
  }, [invokeListDsh, t]);

  // Stable load/settle handling (avoids ref-callback resetting timers on every render).
  useEffect(() => {
    if (!isRunning) return;
    const el = iframeElRef.current;
    if (!el) return;

    setIframeLoading(true);
    clearIframeTimers();

    const finishLoading = () => {
      // First successful load: soft remount while mask stays up (avoids brief white flash).
      if (!autoReloadDoneRef.current) {
        autoReloadDoneRef.current = true;
        remountIframe();
        return;
      }
      setIframeLoading(false);
    };

    const onLoad = () => {
      if (iframeTimerRef.current) {
        clearTimeout(iframeTimerRef.current);
        iframeTimerRef.current = null;
      }
      iframeSettleTimerRef.current = setTimeout(() => {
        iframeSettleTimerRef.current = null;
        finishLoading();
      }, 700);
    };

    el.addEventListener("load", onLoad);
    iframeTimerRef.current = setTimeout(() => {
      iframeTimerRef.current = null;
      finishLoading();
    }, 10000);

    return () => {
      el.removeEventListener("load", onLoad);
      clearIframeTimers();
    };
  }, [isRunning, iframeKey, serviceUrl, clearIframeTimers, remountIframe]);

  useEffect(() => {
    // Nothing here gates rendering: the toolbar/empty state already fall back to
    // "detecting" until the store resolves, and the checks themselves run off the
    // main thread (spawn_blocking), so they cannot freeze the window.
    loadDshStatus();
    // Refresh version / update badge when entering the page
    const refreshTimer = setTimeout(() => {
      refreshDshStatus().catch(() => {});
    }, 50);

    const timer1 = setTimeout(() => {
      checkStatus();
    }, 100);

    const timer2 = setTimeout(() => {
      invokeGetDshVersions()
        .then((v) => {
          setVersions(v);
          if (v.length > 0) {
            setSelectedVersion((cur) => cur || v[v.length - 1]);
          }
        })
        .catch(() => {});
    }, 200);

    return () => {
      clearTimeout(refreshTimer);
      clearTimeout(timer1);
      clearTimeout(timer2);
      pollAbortRef.current = true;
      clearIframeTimers();
      if (statusTimerRef.current) {
        clearTimeout(statusTimerRef.current);
      }
    };
  }, [loadDshStatus, refreshDshStatus, invokeGetDshVersions, checkStatus, clearIframeTimers]);

  useEffect(() => {
    const unlisten = listen<{ stage: string; percent: number; message: string }>(
      "dsh:install_progress",
      (event) => {
        const { percent, message } = event.payload;
        setInstallProgress(percent);
        setInstallMsg(message);
      }
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const unlisten = listen<{ stage: string; percent: number; message: string }>(
      "dsh:update_progress",
      (event) => {
        const { percent, message } = event.payload;
        setUpdateProgress(percent);
        setUpdateMsg(message);
      }
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const handleInstall = useCallback(
    async (version?: string) => {
      setInstalling(true);
      setInstallProgress(0);
      setInstallMsg(t("dsh.installingProgress"));
      try {
        await invokeInstallDsh(version || undefined);
        setInstallProgress(100);
        setInstallMsg(t("dsh.installDone"));
        await refreshDshStatus();
      } catch (error) {
        setInstallMsg(t("dsh.installFailed", { error: String(error) }));
      } finally {
        setInstalling(false);
        setTimeout(() => {
          setInstallMsg(null);
          setInstallProgress(0);
        }, 3000);
      }
    },
    [invokeInstallDsh, refreshDshStatus, t]
  );

  const cancelStartWait = () => {
    startCancelledRef.current = true;
    setStarting(false);
    // Process may still be coming up — allow Stop
    setIsRunning(true);
    setStatus(t("dsh.cancelWaitHint"));
    clearStatusAfter(5000);
  };

  const startOnPort = async (port: number = DEFAULT_PORT) => {
    // Clear the flags a previous stop/cancel left behind: without this reset the guard
    // below returned right after a successful start, leaving the toolbar on "未启动"
    // while the service was actually running.
    startCancelledRef.current = false;
    pollAbortRef.current = false;
    setStarting(true);
    setStatus(t("dsh.starting"));
    try {
      const result = await invokeStartDsh(port);
      if (result?.auth_patch_warning) {
        showCriticalAlert(
          `${t("dsh.authPatchFailed")}: ${result.auth_patch_warning}`
        );
      }
      const portUsed = result?.port ?? port;
      setActivePort(portUsed);
      if (startCancelledRef.current || pollAbortRef.current) {
        return;
      }
      // The backend resolves only once the port listens *and* plain requests are
      // authorized — strictly stronger than the old client-side port/HTTP poll, which
      // together with its fixed 600ms delay cost ~1.6s on every start.
      autoReloadDoneRef.current = false;
      setIsRunning(true);
      setStatus(t("dsh.running"));
      remountIframe();
    } catch (error) {
      const msg = String(error);
      setStatus(t("dsh.startFailed", { error: msg }));
      if (/端口|port|Node|npx|not installed|占用/i.test(msg)) {
        showCriticalAlert(t("dsh.startFailed", { error: msg }));
      }
      clearStatusAfter(6000);
    } finally {
      setStarting(false);
    }
  };

  const handleStart = async () => {
    await startOnPort(startPortValid ? startPort : DEFAULT_PORT);
  };

  const handleOpenInBrowser = async () => {
    try {
      await invoke("open_in_browser", { url: serviceUrl });
    } catch (error) {
      setStatus(t("dsh.startFailed", { error: String(error) }));
      clearStatusAfter(5000);
    }
  };

  const handlePortChange = (raw: string) => {
    const n = Number(raw);
    // Keep the last valid port while typing; an invalid entry just never sticks.
    if (!raw.trim() || !Number.isInteger(n) || n <= 0 || n >= 65536) return;
    setStartPort(n);
    try {
      localStorage.setItem(DSH_PORT_KEY, String(n));
    } catch {
      /* ignore */
    }
  };

  const handleStop = async () => {
    const ok = await confirm({
      title: t("dsh.stopTitle"),
      message: t("dsh.stopMessage"),
      confirmText: t("dsh.stopConfirm"),
      cancelText: t("models.cancel"),
    });
    if (!ok) return;

    // Abort any in-flight start poll
    startCancelledRef.current = true;
    pollAbortRef.current = true;

    setStopping(true);
    try {
      await invokeStopDsh(activePort);
      setIsRunning(false);
      setIframeLoading(false);
      autoReloadDoneRef.current = false;
      setStatus(t("dsh.stopped"));
      clearStatusAfter();
    } catch (error) {
      setStatus(t("dsh.stopFailed", { error: String(error) }));
      clearStatusAfter(5000);
    } finally {
      setStopping(false);
      setStarting(false);
    }
  };

  const handleRefresh = async () => {
    setStatus(t("dsh.checking"));
    try {
      const list = await invokeListDsh();
      if (list.length > 0) {
        const instance = list.find((i) => i.port === activePort) ?? list[0];
        setActivePort(instance.port);
        setIsRunning(true);
        setStatus(t("dsh.running"));
        autoReloadDoneRef.current = false;
        remountIframe();
      } else {
        setIsRunning(false);
        setIframeLoading(false);
        setStatus(t("dsh.notRunning"));
        clearStatusAfter();
      }
    } catch {
      setIframeLoading(false);
      setStatus(t("dsh.checkFailed"));
      clearStatusAfter();
    }
  };

  /// Undo the auth-bypass patch by restoring the original connection file. When no
  /// backup exists the backend reports that the file was never modified (newer DSH
  /// versions need no patch) instead of failing; its message is surfaced as-is.
  const handleRestoreAuth = async () => {
    if (busy) return;
    const ok = await confirm({
      title: t("dsh.restoreAuthTitle"),
      message: t("dsh.restoreAuthMessage"),
      confirmText: t("dsh.restoreAuth"),
      icon: "warning",
    });
    if (!ok) return;
    setRestoringAuth(true);
    try {
      const msg = await invokeRestoreDshAuth();
      setStatus(msg || t("dsh.restoreAuthDone"));
      clearStatusAfter(5000);
    } catch (error) {
      setStatus(t("dsh.restoreAuthFailed", { error: String(error) }));
      clearStatusAfter(6000);
    } finally {
      setRestoringAuth(false);
    }
  };

  const handleUpdate = async () => {
    if (isRunning) {
      const ok = await confirm({
        title: t("dsh.updateTitle"),
        message: t("dsh.updateMessageRunning"),
        confirmText: t("dsh.updateConfirm"),
        cancelText: t("models.cancel"),
      });
      if (!ok) return;
    }

    setUpdating(true);
    setUpdateProgress(0);
    setUpdateMsg(t("dsh.updatePreparing"));

    try {
      if (isRunning) {
        setUpdateMsg(t("dsh.updateStopping"));
        startCancelledRef.current = true;
        try {
          await invokeStopDsh(activePort);
        } catch {
          // continue — update may still work
        }
        setIsRunning(false);
        setIframeLoading(false);
      }

      setUpdateMsg(t("dsh.updateInProgress"));
      // Pin the version that was advertised: `latest` can point at the build already
      // installed (DSH ships rc builds on both tags), which made the update a no-op.
      const result = await invokeUpdateDsh(dshLatestVersion);
      setUpdateProgress(100);
      setUpdateMsg(result);
      await refreshDshStatus();

      const startNow = await confirm({
        title: t("dsh.updateTitle"),
        message: t("dsh.updateDoneAskStart"),
        confirmText: t("dsh.updateStartConfirm"),
        cancelText: t("models.cancel"),
      });
      if (startNow) {
        setUpdateMsg(t("dsh.updateDoneStarted"));
        await startOnPort(DEFAULT_PORT);
      } else {
        setUpdateMsg(t("dsh.updateDone"));
      }
    } catch (error) {
      setUpdateProgress(0);
      setUpdateMsg(t("dsh.updateFailed", { error: String(error) }));
    } finally {
      setUpdating(false);
      setTimeout(() => {
        setUpdateMsg(null);
        setUpdateProgress(0);
      }, 4000);
    }
  };

  const copyUrl = async () => {
    const url = serviceUrl;
    try {
      await invokeCopyToClipboard(url);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
      return;
    } catch {
      /* fallthrough */
    }
    try {
      await navigator.clipboard.writeText(url);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
      return;
    } catch {
      /* fallthrough */
    }
    try {
      const ta = document.createElement("textarea");
      ta.value = url;
      ta.style.position = "fixed";
      ta.style.left = "-9999px";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch (err) {
      console.error("All clipboard methods failed:", err);
    }
  };

  const busy = starting || stopping || updating || installing || restoringAuth;
  const installed = !!dshVersion && dshVersion !== "";
  const detecting = dshVersion === null;
  const progressMsg = updateMsg || installMsg;
  const progressPct = updateMsg ? updateProgress : installProgress;
  const progressActive = updating || installing;
  const statusLabel =
    status ||
    (detecting
      ? t("dsh.detecting")
      : isRunning
        ? t("dsh.running")
        : installed
          ? t("dsh.idle")
          : t("dsh.notInstalledShort"));

  return (
    <div className="dsh-container">
      {alertBanner && (
        <div className="alert-banner alert-banner-warning" role="alert">
          <span className="alert-banner-text">{alertBanner}</span>
          <button
            type="button"
            className="alert-banner-dismiss"
            onClick={() => setAlertBanner(null)}
            aria-label={t("dsh.dismiss")}
          >
            <X size={14} />
          </button>
        </div>
      )}
      {showOnboarding && !isRunning && (
        <div className="alert-banner alert-banner-info onboarding-checklist">
          <div className="onboarding-body">
            <div className="onboarding-title">{t("dsh.onboardingTitle")}</div>
            <ol className="onboarding-steps">
              <li className={dshNodejsInstalled ? "done" : ""}>
                {t("dsh.onboardingStep1")}
                {dshNodejsInstalled ? " ✓" : ""}
              </li>
              <li className={dshVersion ? "done" : ""}>
                {t("dsh.onboardingStep2")}
                {dshVersion ? " ✓" : ""}
              </li>
              <li>{t("dsh.onboardingStep3")}</li>
            </ol>
            <div className="onboarding-actions">
              <button type="button" className="btn btn-secondary btn-small" onClick={dismissOnboarding}>
                {t("dsh.onboardingDismiss")}
              </button>
            </div>
          </div>
          <button
            type="button"
            className="alert-banner-dismiss"
            onClick={dismissOnboarding}
            aria-label={t("dsh.dismiss")}
          >
            <X size={14} />
          </button>
        </div>
      )}

      <div className={`dsh-stage ${isRunning ? "is-running" : ""}`}>
        <div className="dsh-toolbar">
          <div className="dsh-toolbar-primary">
            <div className="dsh-toolbar-context">
              <div
                className="dsh-status"
                title={statusLabel}
                aria-label={statusLabel}
              >
                <span
                  className={`dsh-status-dot ${isRunning ? "running" : ""} ${starting || installing || updating ? "busy" : ""}`}
                />
                {!isRunning && (
                  <span className="dsh-status-text">{statusLabel}</span>
                )}
              </div>
              {isRunning && (
                <>
                  <button
                    type="button"
                    className="dsh-url-badge"
                    onClick={copyUrl}
                    title={t("dsh.copyUrl")}
                  >
                    <Globe size={11} />
                    <span>
                      {DSH_HOST}:{activePort}
                    </span>
                    {copied ? <Check size={10} /> : <Copy size={10} />}
                  </button>
                  <button
                    type="button"
                    className="dsh-url-badge"
                    onClick={() => void handleOpenInBrowser()}
                    title={t("dsh.openInBrowser")}
                    aria-label={t("dsh.openInBrowser")}
                  >
                    <ExternalLink size={11} />
                  </button>
                </>
              )}
              {!detecting && (installed || dshHasUpdate) && (
                <div className="dsh-version-group">
                  <span className="dsh-version-label">
                    <Tag size={10} strokeWidth={1.5} />
                    <span className="dsh-version-text">{installed ? dshVersion : t("dsh.notInstalledShort")}</span>
                  </span>
                  {dshHasUpdate && dshLatestVersion && (
                    <button
                      type="button"
                      className="dsh-update-chip"
                      onClick={handleUpdate}
                      disabled={updating || dshNodejsInstalled === false || installing}
                      title={t("dsh.updateTo", { version: dshLatestVersion })}
                      aria-label={t("dsh.updateTo", { version: dshLatestVersion })}
                    >
                      {updating ? (
                        <Loader2 size={11} className="spin" />
                      ) : (
                        <ArrowRight size={11} strokeWidth={2} />
                      )}
                      <span>v{dshLatestVersion}</span>
                    </button>
                  )}
                </div>
              )}
            </div>

            <div className="dsh-toolbar-actions">
              {detecting && (
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  disabled
                  title={t("dsh.detecting")}
                  aria-label={t("dsh.detecting")}
                >
                  <Loader2 size={14} className="spin" />
                </button>
              )}

              {!detecting && !installed && !installing && (
                <>
                  {versions.length > 0 && (
                    <select
                      className="dsh-version-select"
                      value={selectedVersion}
                      onChange={(e) => setSelectedVersion(e.target.value)}
                      aria-label={t("dsh.install")}
                    >
                      {versions.map((v) => (
                        <option key={v} value={v}>
                          {v}
                        </option>
                      ))}
                    </select>
                  )}
                  <button
                    type="button"
                    className="btn commit-icon-btn commit-icon-btn-primary"
                    onClick={() => handleInstall(selectedVersion || undefined)}
                    disabled={dshNodejsInstalled === false}
                  >
                    <Download size={14} />
                    <span>{t("dsh.install")}</span>
                  </button>
                </>
              )}

              {installing && (
                <button type="button" className="btn commit-icon-btn" disabled>
                  <Loader2 size={14} className="spin" />
                  <span>{t("dsh.installing")}</span>
                </button>
              )}

              {installed && !installing && !isRunning && !starting && (
                <>
                  <input
                    type="number"
                    className="dsh-version-select dsh-port-input"
                    value={startPort}
                    min={1}
                    max={65535}
                    onChange={(e) => handlePortChange(e.target.value)}
                    aria-label={t("dsh.port")}
                    title={t("dsh.port")}
                  />
                  <button
                    type="button"
                    className="btn commit-icon-btn commit-icon-btn-primary"
                    onClick={handleStart}
                    disabled={dshNodejsInstalled === false || updating || !startPortValid}
                  >
                    <Play size={14} />
                    <span>{t("dsh.start")}</span>
                  </button>
                </>
              )}

              {starting && (
                <>
                  <button type="button" className="btn commit-icon-btn" disabled>
                    <Loader2 size={14} className="spin" />
                    <span>{t("dsh.starting")}</span>
                  </button>
                  <button
                    type="button"
                    className="btn commit-icon-btn"
                    onClick={cancelStartWait}
                    title={t("dsh.cancelWaitTitle")}
                  >
                    <X size={14} />
                    <span>{t("dsh.cancelWait")}</span>
                  </button>
                </>
              )}

              {(isRunning || stopping) && !starting && (
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  onClick={handleStop}
                  disabled={stopping}
                  title={t("dsh.stop")}
                  aria-label={t("dsh.stop")}
                >
                  {stopping ? <Loader2 size={14} className="spin" /> : <Square size={14} />}
                  {!isRunning && <span>{t("dsh.stop")}</span>}
                </button>
              )}

              {!detecting && (
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  onClick={handleRefresh}
                  disabled={busy}
                  title={t("dsh.refresh")}
                  aria-label={t("dsh.refresh")}
                >
                  <RefreshCw size={14} />
                </button>
              )}

              {installed && !detecting && (
                <button
                  type="button"
                  className="btn commit-icon-btn"
                  onClick={handleRestoreAuth}
                  disabled={busy}
                  title={t("dsh.restoreAuthTitle")}
                  aria-label={t("dsh.restoreAuthTitle")}
                >
                  {restoringAuth ? <Loader2 size={14} className="spin" /> : <Undo2 size={14} />}
                </button>
              )}
            </div>
          </div>

          {progressMsg && (
            <div className="dsh-progress-row">
              <span className="dsh-progress-text">{progressMsg}</span>
              {progressActive && (
                <div className="dsh-progress-bar">
                  <div
                    className="dsh-progress-fill"
                    style={{ width: `${Math.min(progressPct, 100)}%` }}
                  />
                </div>
              )}
            </div>
          )}
        </div>

        <div className="dsh-iframe-container">
          {isRunning ? (
            <>
              <iframe
                key={iframeKey}
                ref={iframeElRef}
                src={serviceUrl}
                className={`dsh-iframe${iframeLoading ? " is-loading" : ""}`}
                title="DeepSeek Harness"
                allow="clipboard-write; clipboard-read"
              />
              {iframeLoading && (
                <div className="dsh-iframe-loading" aria-busy="true">
                  <Loader2 size={24} className="spin" />
                  <span className="dsh-iframe-loading-text">{t("dsh.loadingUi")}</span>
                </div>
              )}
            </>
          ) : (
            <div className="dsh-empty-state">
              <div className="dsh-empty-state-icon">
                <img src="/deepseek-logo.png" alt="DeepSeek" />
              </div>
              <div className="dsh-empty-state-title">{t("dsh.serviceTitle")}</div>
              {dshNodejsInstalled === false ? (
                <>
                  <div className="dsh-empty-state-desc">{t("dsh.nodejsRequired")}</div>
                  <div className="dsh-empty-state-hint">{t("dsh.nodejsHint")}</div>
                  <a
                    className="btn btn-primary btn-small dsh-install-link"
                    href="https://nodejs.org/"
                    target="_blank"
                    rel="noopener noreferrer"
                  >
                    <ExternalLink size={12} />
                    {t("dsh.nodejsDownload")}
                  </a>
                </>
              ) : !installed ? (
                <>
                  <div className="dsh-empty-state-desc">{t("dsh.notInstalled")}</div>
                  <div className="dsh-empty-state-hint">{t("dsh.clickInstall")}</div>
                </>
              ) : (
                <div className="dsh-empty-state-desc">{t("dsh.clickStart")}</div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export default DeepSeekHarness;
