import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  AppWindow,
  Loader2,
  Play,
  Plus,
  RefreshCw,
  Trash2,
  Unplug,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import type { DesktopWindowCandidate, DesktopWindowInfo, InstalledAppInfo } from "../core/types";
import {
  BUILTIN_DESKTOP_APPS,
  loadCustomApps,
  matchExe,
  processNameFromExe,
  saveCustomApps,
  type DesktopAppDef,
} from "../lib/desktopApps";

type Phase = "idle" | "running" | "attached" | "launching" | "polling" | "missing" | "error";

/** Generous, because a cold IDE start (IntelliJ, VS Code) blows past a minute. */
const WINDOW_WAIT_MS = 150_000;
const POLL_MS = 500;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Host desktop apps (DeepSeek Harness desktop, Cursor, …) inside the workbench.
 *
 * Each app is a tab; the tab body is a slot the backend positions the app's own
 * window over (the app window is reparented into the workbench, so it is not in
 * the DOM at all). Leaving the page hides the window — the app keeps running and
 * its window is handed back to the desktop when the workbench exits.
 */
export default function DesktopAppsPage() {
  const { t } = useTranslation("desktopApps");
  const confirm = useConfirm();

  const invokeLaunchApp = useGlobalStore((s) => s.invokeLaunchApp);
  const invokeKillApp = useGlobalStore((s) => s.invokeKillApp);
  const invokeIsAppRunning = useGlobalStore((s) => s.invokeIsAppRunning);
  const invokeScanInstalledApps = useGlobalStore((s) => s.invokeScanInstalledApps);
  const invokeScanDesktopApps = useGlobalStore((s) => s.invokeScanDesktopApps);
  const invokeDesktopEmbedFind = useGlobalStore((s) => s.invokeDesktopEmbedFind);
  const invokeDesktopEmbedAttach = useGlobalStore((s) => s.invokeDesktopEmbedAttach);
  const invokeDesktopEmbedRelayout = useGlobalStore((s) => s.invokeDesktopEmbedRelayout);
  const invokeDesktopEmbedShow = useGlobalStore((s) => s.invokeDesktopEmbedShow);
  const invokeDesktopEmbedDetach = useGlobalStore((s) => s.invokeDesktopEmbedDetach);
  const invokeDesktopEmbedCandidates = useGlobalStore((s) => s.invokeDesktopEmbedCandidates);

  const [custom, setCustom] = useState<DesktopAppDef[]>(() => loadCustomApps());
  const [activeId, setActiveId] = useState<string>(BUILTIN_DESKTOP_APPS[0].id);
  const [installed, setInstalled] = useState<InstalledAppInfo[]>([]);
  const [phase, setPhase] = useState<Phase>("idle");
  const [busy, setBusy] = useState(false);
  const [windowTitle, setWindowTitle] = useState<string>("");
  /** Why the last adopt attempt failed, verbatim from the backend. */
  const [errorText, setErrorText] = useState<string>("");
  const [candidates, setCandidates] = useState<DesktopWindowCandidate[]>([]);

  const slotRef = useRef<HTMLDivElement>(null);
  const attachedRef = useRef(false);
  const aliveRef = useRef(true);
  const activeIdRef = useRef(activeId);

  const apps = useMemo(() => [...BUILTIN_DESKTOP_APPS, ...custom], [custom]);
  const active = useMemo(
    () => apps.find((app) => app.id === activeId) ?? apps[0],
    [apps, activeId]
  );
  const processName = active.processName;
  activeIdRef.current = activeId;

  const label = useCallback(
    (app: DesktopAppDef) => (app.labelKey ? t(app.labelKey) : app.label ?? app.processName),
    [t]
  );

  // Executables are only needed to *start* an app, and the scan is slow, so it
  // runs once per visit in the background.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [registry, shortcuts] = await Promise.all([
          invokeScanInstalledApps(),
          invokeScanDesktopApps(),
        ]);
        if (!cancelled) setInstalled([...shortcuts, ...registry]);
      } catch {
        /* no scan: the user can still pick an .exe by hand */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [invokeScanInstalledApps, invokeScanDesktopApps]);

  /** The windows the process owns — what "no usable window" actually saw. */
  const diagnose = useCallback(async () => {
    const list = await invokeDesktopEmbedCandidates(processName).catch(() => []);
    if (aliveRef.current) setCandidates(list);
  }, [invokeDesktopEmbedCandidates, processName]);

  /** Adopt the app's window into the slot. `false` = no window to adopt. */
  const attach = useCallback(
    async (): Promise<boolean> => {
      const rect = slotRef.current?.getBoundingClientRect();
      if (!rect || rect.width < 2 || rect.height < 2) return false;
      try {
        const info: DesktopWindowInfo = await invokeDesktopEmbedAttach({
          processName,
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
        });
        if (!aliveRef.current || activeIdRef.current !== active.id) return true;
        attachedRef.current = true;
        setErrorText("");
        setCandidates([]);
        setWindowTitle(info.title);
        setPhase("attached");
        return true;
      } catch (err) {
        attachedRef.current = false;
        if (aliveRef.current) setErrorText(err instanceof Error ? err.message : String(err));
        return false;
      }
    },
    [active.id, invokeDesktopEmbedAttach, processName]
  );

  // Per-app lifecycle: adopt what is already running, follow the slot on resize,
  // and hide (never detach) when the page goes away.
  useEffect(() => {
    aliveRef.current = true;
    attachedRef.current = false;
    setWindowTitle("");
    setErrorText("");
    setCandidates([]);
    let frame = 0;

    const sync = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        if (!attachedRef.current) return;
        const rect = slotRef.current?.getBoundingClientRect();
        if (!rect) return;
        void invokeDesktopEmbedRelayout({
          processName,
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
        }).then((moved) => {
          if (!moved) {
            // The window is gone (closed from its own side, killed, …).
            attachedRef.current = false;
            setPhase("idle");
          }
        });
      });
    };

    void (async () => {
      setBusy(true);
      try {
        const found = await invokeDesktopEmbedFind(processName).catch(() => null);
        if (!aliveRef.current) return;
        if (!found) {
          setPhase("idle");
          return;
        }
        if (found.attached) {
          attachedRef.current = true;
          setWindowTitle(found.title);
          setPhase("attached");
          void invokeDesktopEmbedShow(processName, true);
          sync();
          return;
        }
        const ok = await attach();
        // Not running yet is the normal case here: stay quiet about it.
        if (!ok && aliveRef.current) {
          setPhase("running");
          setErrorText("");
          setCandidates([]);
        }
      } finally {
        if (aliveRef.current) setBusy(false);
      }
    })();

    const observer = new ResizeObserver(sync);
    if (slotRef.current) observer.observe(slotRef.current);
    window.addEventListener("resize", sync);

    return () => {
      aliveRef.current = false;
      cancelAnimationFrame(frame);
      observer.disconnect();
      window.removeEventListener("resize", sync);
      // Hide, do not detach: the app keeps its window and its state.
      if (attachedRef.current) {
        attachedRef.current = false;
        void invokeDesktopEmbedShow(processName, false);
      }
    };
  }, [attach, invokeDesktopEmbedFind, invokeDesktopEmbedShow, invokeDesktopEmbedRelayout, processName]);

  const startAndAttach = useCallback(async () => {
    setBusy(true);
    setErrorText("");
    setCandidates([]);
    try {
      const exe = matchExe(active, installed);
      if (!exe) {
        setPhase("missing");
        return;
      }
      if (!(await invokeIsAppRunning(processName).catch(() => false))) {
        setPhase("launching");
        try {
          await invokeLaunchApp(exe, active.args?.join(" "));
        } catch (err) {
          if (aliveRef.current) {
            setErrorText(
              t("error.launch", {
                message: err instanceof Error ? err.message : String(err),
              })
            );
            setPhase("error");
          }
          return;
        }
      }
      setPhase("polling");
      const deadline = Date.now() + WINDOW_WAIT_MS;
      while (Date.now() < deadline) {
        await sleep(POLL_MS);
        if (!aliveRef.current || activeIdRef.current !== active.id) return;
        const found = await invokeDesktopEmbedFind(processName).catch(() => null);
        if (found) break;
      }
      const ok = await attach();
      if (!ok && aliveRef.current) {
        setPhase("error");
        await diagnose();
      }
    } catch (err) {
      if (aliveRef.current) {
        setErrorText(err instanceof Error ? err.message : String(err));
        setPhase("error");
        await diagnose();
      }
    } finally {
      if (aliveRef.current) setBusy(false);
    }
  }, [
    active,
    attach,
    diagnose,
    installed,
    invokeDesktopEmbedFind,
    invokeIsAppRunning,
    invokeLaunchApp,
    processName,
    t,
  ]);

  const reattach = useCallback(async () => {
    setBusy(true);
    try {
      if (await attach()) return;
      await diagnose();
      const found = await invokeDesktopEmbedFind(processName).catch(() => null);
      if (aliveRef.current) setPhase(found ? "error" : "idle");
    } finally {
      if (aliveRef.current) setBusy(false);
    }
  }, [attach, diagnose, invokeDesktopEmbedFind, processName]);

  const detach = useCallback(async () => {
    setBusy(true);
    try {
      await invokeDesktopEmbedDetach(processName);
      attachedRef.current = false;
      setPhase("running");
    } finally {
      if (aliveRef.current) setBusy(false);
    }
  }, [invokeDesktopEmbedDetach, processName]);

  const killApp = useCallback(async () => {
    const ok = await confirm({
      title: t("kill.title"),
      message: t("kill.message", { name: label(active) }),
      confirmText: t("kill.confirm"),
      icon: "danger",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await invokeKillApp(processName);
      await invokeDesktopEmbedDetach(processName).catch(() => undefined);
      attachedRef.current = false;
      setPhase("idle");
    } finally {
      if (aliveRef.current) setBusy(false);
    }
  }, [active, confirm, invokeDesktopEmbedDetach, invokeKillApp, label, processName, t]);

  const addApp = useCallback(async () => {
    const path = await invoke<string>("browse_app_file").catch(() => null);
    if (!path) return;
    const stem = processNameFromExe(path);
    const def: DesktopAppDef = {
      id: `custom-${Date.now().toString(36)}`,
      label: stem,
      processName: stem,
      matchNames: [stem.toLowerCase()],
      exePath: path,
      custom: true,
    };
    const next = [...custom, def];
    setCustom(next);
    saveCustomApps(next);
    setActiveId(def.id);
  }, [custom]);

  const removeApp = useCallback(async () => {
    if (!active.custom) return;
    const ok = await confirm({
      title: t("remove.title"),
      message: t("remove.message", { name: label(active) }),
      confirmText: t("remove.confirm"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await invokeDesktopEmbedDetach(processName);
    } catch {
      /* nothing adopted */
    }
    attachedRef.current = false;
    const next = custom.filter((app) => app.id !== active.id);
    setCustom(next);
    saveCustomApps(next);
    setActiveId(BUILTIN_DESKTOP_APPS[0].id);
  }, [active, confirm, custom, invokeDesktopEmbedDetach, label, processName, t]);

  const statusText = (() => {
    switch (phase) {
      case "attached":
        return t("status.attached");
      case "running":
        return t("status.running");
      case "launching":
        return t("status.launching");
      case "polling":
        return t("status.polling");
      case "missing":
        return t("status.missing");
      case "error":
        return t("status.error");
      default:
        return t("status.idle");
    }
  })();

  return (
    <div className="dapps-page">
      {/* One strip only: the hosted app's window paints over everything below,
          so every control has to live in the reserved top band. */}
      <div className="dapps-bar">
        <div className="dapps-tabs" role="tablist" aria-label={t("title")}>
          {apps.map((app) => (
            <button
              key={app.id}
              type="button"
              role="tab"
              aria-selected={app.id === active.id}
              className={`dapps-tab${app.id === active.id ? " is-active" : ""}`}
              onClick={() => setActiveId(app.id)}
            >
              <AppWindow size={14} />
              <span>{label(app)}</span>
            </button>
          ))}
          <button type="button" className="dapps-tab dapps-tab-add" onClick={() => void addApp()}>
            <Plus size={14} />
            <span>{t("addApp")}</span>
          </button>
        </div>

        <span
          className={`dapps-status is-${phase}`}
          title={phase === "attached" && windowTitle ? windowTitle : undefined}
        >
          {busy && (phase === "launching" || phase === "polling") ? (
            <Loader2 size={13} className="dapps-spin" />
          ) : (
            <span className="dapps-dot" />
          )}
          <span className="dapps-status-text">{statusText}</span>
        </span>

        <span className="dapps-spacer" />

        <div className="dapps-actions">
          <button
            type="button"
            className="btn btn-primary btn-small"
            disabled={busy || phase === "attached"}
            onClick={() => void startAndAttach()}
          >
            <Play size={13} />
            {t("actions.launch")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={busy || phase === "idle" || phase === "missing"}
            onClick={() => void reattach()}
          >
            <RefreshCw size={13} />
            {t("actions.reattach")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={busy || phase !== "attached"}
            onClick={() => void detach()}
          >
            <Unplug size={13} />
            {t("actions.detach")}
          </button>
          <button
            type="button"
            className="btn btn-danger btn-small"
            disabled={busy || phase === "idle" || phase === "missing"}
            onClick={() => void killApp()}
          >
            <XCircle size={13} />
            {t("actions.kill")}
          </button>
          {active.custom && (
            <button
              type="button"
              className="btn btn-danger btn-small"
              disabled={busy}
              onClick={() => void removeApp()}
            >
              <Trash2 size={13} />
              {t("actions.remove")}
            </button>
          )}
        </div>
      </div>

      <div className="dapps-body">
        <div className="dapps-slot" ref={slotRef} />
        {phase !== "attached" && (
          <div className="dapps-hint">
            <AppWindow size={28} />
            <p className="dapps-hint-title">{label(active)}</p>
            <p className="dapps-hint-text">
              {phase === "missing"
                ? t("hint.missing")
                : phase === "running"
                  ? t("hint.running")
                  : phase === "error"
                    ? t("hint.error")
                    : t("hint.idle")}
            </p>
            <p className="dapps-hint-note">{t("hint.note")}</p>

            {/* Failure detail lives here: the app window paints over everything
                below the toolbar, so a toast would be unreachable anyway. */}
            {errorText ? (
              <div className="dapps-error">
                <p className="dapps-error-head">
                  <AlertTriangle size={13} />
                  {t("error.title")}
                </p>
                <p className="dapps-error-text">{errorText}</p>
                {candidates.length > 0 ? (
                  <>
                    <p className="dapps-error-head">{t("error.candidates")}</p>
                    <ul className="dapps-error-list">
                      {candidates.map((c, index) => (
                        <li key={`${c.pid}-${index}`}>
                          <span className="dapps-error-tag">{c.class}</span>
                          <span className="dapps-error-tag">
                            {c.visible ? t("error.visible") : t("error.hidden")}
                          </span>
                          {c.owned ? (
                            <span className="dapps-error-tag">{t("error.owned")}</span>
                          ) : null}
                          <span className="dapps-error-win">{c.title || t("error.untitled")}</span>
                        </li>
                      ))}
                    </ul>
                  </>
                ) : null}
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={() => void diagnose()}
                >
                  <RefreshCw size={12} />
                  {t("error.refresh")}
                </button>
              </div>
            ) : null}
          </div>
        )}
      </div>
    </div>
  );
}
