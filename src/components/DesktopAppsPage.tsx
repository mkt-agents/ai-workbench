import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  AppWindow,
  Loader2,
  MoreHorizontal,
  MousePointerClick,
  Play,
  Plus,
  RefreshCw,
  Trash2,
  Unplug,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";
import DropdownMenu from "./DropdownMenu";
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

/** One toolbar button, described once and rendered either in the full row or
 * the narrow "⋯" overflow menu. */
type TbarAction = {
  key: string;
  label: string;
  icon: ReactNode;
  variant: "primary" | "secondary" | "danger";
  title?: string;
  disabled: boolean;
  onClick: () => void;
};

/** Generous, because a cold IDE start (IntelliJ, VS Code) blows past a minute. */
const WINDOW_WAIT_MS = 150_000;
/** One backend attach_wait round: scans every 25ms in-process for this long,
 * then reports nothing found and the outer loop calls again. */
const ATTACH_WAIT_MS = 3_000;
const HEARTBEAT_MS = 400;

/** Module-level exe-icon cache shared by the tab strip (survives re-renders). */
const desktopIconCache = new Map<string, string>();

/**
 * Visibility ops per process run strictly in issue order.
 *
 * A tab-switch hide and a quick switch-back show are two independent IPC
 * calls; if the backend ever landed the hide after the show, the entry would
 * sit at shown=0 while the page is active — the heartbeat keeps re-hiding it
 * and the slot stays blank for good. Chaining through this queue makes the
 * order deterministic; different processes are independent by key.
 */
const showQueues = new Map<string, Promise<void>>();
const enqueueShow = (processName: string, op: () => Promise<void>): Promise<void> => {
  const next = (showQueues.get(processName) ?? Promise.resolve()).then(op, op);
  showQueues.set(
    processName,
    next.finally(() => {
      if (showQueues.get(processName) === next) showQueues.delete(processName);
    })
  );
  return next;
};

/** The tab shows only the app's real icon; the name appears on hover. */
function AppTabIcon({
  app,
  installed,
}: {
  app: DesktopAppDef;
  installed: InstalledAppInfo[];
}) {
  const path = matchExe(app, installed);
  const [fetched, setFetched] = useState<string | null>(
    () => (path ? desktopIconCache.get(path) ?? null : null)
  );

  useEffect(() => {
    if (!path || !path.toLowerCase().endsWith(".exe")) return;
    // The install scan already carries icons — prefer that, fetch otherwise.
    const fromScan = installed.find((item) => item.exe_path === path)?.icon;
    if (fromScan) {
      desktopIconCache.set(path, fromScan);
      setFetched(fromScan);
      return;
    }
    if (desktopIconCache.has(path)) {
      setFetched(desktopIconCache.get(path) ?? null);
      return;
    }
    let cancelled = false;
    useGlobalStore
      .getState()
      .invokeExtractAppIcon(path)
      .then((data) => {
        if (!data) return;
        desktopIconCache.set(path, data);
        if (!cancelled) setFetched(data);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [installed, path]);

  if (fetched) {
    return (
      <img
        className="dapps-tab-icon"
        src={`data:image/png;base64,${fetched}`}
        alt=""
        draggable={false}
      />
    );
  }
  return <AppWindow size={16} />;
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
  const invokeDesktopEmbedAttachWait = useGlobalStore((s) => s.invokeDesktopEmbedAttachWait);
  const invokeDesktopEmbedRelayout = useGlobalStore((s) => s.invokeDesktopEmbedRelayout);
  const invokeDesktopEmbedShow = useGlobalStore((s) => s.invokeDesktopEmbedShow);
  const invokeDesktopEmbedFocus = useGlobalStore((s) => s.invokeDesktopEmbedFocus);
  const invokeDesktopEmbedDetach = useGlobalStore((s) => s.invokeDesktopEmbedDetach);
  const invokeDesktopEmbedCandidates = useGlobalStore((s) => s.invokeDesktopEmbedCandidates);
  const invokeDesktopEmbedAttachWindow = useGlobalStore((s) => s.invokeDesktopEmbedAttachWindow);

  const [custom, setCustom] = useState<DesktopAppDef[]>(() => loadCustomApps());
  const [activeId, setActiveId] = useState<string>(BUILTIN_DESKTOP_APPS[0].id);
  const [installed, setInstalled] = useState<InstalledAppInfo[]>([]);
  const [phase, setPhase] = useState<Phase>("idle");
  const [busy, setBusy] = useState(false);
  const [windowTitle, setWindowTitle] = useState<string>("");
  /** Win32 class of the adopted window — makes a wrong pick visible on hover. */
  const [windowClass, setWindowClass] = useState<string>("");
  /** Set when the adopted window cannot paint anything (see `verify` in Rust). */
  const [warning, setWarning] = useState<string>("");
  /** Why the last adopt attempt failed, verbatim from the backend. */
  const [errorText, setErrorText] = useState<string>("");
  const [candidates, setCandidates] = useState<DesktopWindowCandidate[]>([]);
  /** Window picker: open state plus the list it was opened with. */
  const [picking, setPicking] = useState(false);
  const [pickerList, setPickerList] = useState<DesktopWindowCandidate[]>([]);
  const [pickerQuery, setPickerQuery] = useState("");

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

  /** A candidate's human name: its title, or its class when it has none. */
  const winName = useCallback(
    (c: DesktopWindowCandidate) => c.title.trim() || t("win.fallback", { class: c.class }),
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

  /** Open the picker: the candidates at this moment, adoptable by handle. */
  const openPicker = useCallback(async () => {
    setBusy(true);
    try {
      let list = await invokeDesktopEmbedCandidates(processName).catch(() => []);
      if (!aliveRef.current) return;
      // relayout drops its map entry between remove and re-attach; if we came
      // back attached but no row is flagged current, that is the race — read
      // once more so the picker still marks the embedded window.
      if (attachedRef.current && !list.some((c) => c.current)) {
        list = await invokeDesktopEmbedCandidates(processName).catch(() => list);
        if (!aliveRef.current) return;
      }
      // The hosted window paints over the webview, so it would cover the very
      // dialog that is meant to replace it: hide it while choosing.
      if (attachedRef.current) {
        void enqueueShow(processName, () => invokeDesktopEmbedShow(processName, false));
      }
      setPickerList(list);
      setPickerQuery("");
      setPicking(true);
    } finally {
      if (aliveRef.current) setBusy(false);
    }
  }, [invokeDesktopEmbedCandidates, invokeDesktopEmbedShow, processName]);

  /** Close the picker, bringing the adopted window back if it is still ours. */
  const closePicker = useCallback(() => {
    setPicking(false);
    if (attachedRef.current) {
      // Show, then hand focus over: the user closed a webview dialog, so the
      // intent is to be back inside the hosted app (typing without clicking).
      void enqueueShow(processName, () =>
        invokeDesktopEmbedShow(processName, true).then(() => invokeDesktopEmbedFocus(processName))
      );
    }
  }, [invokeDesktopEmbedFocus, invokeDesktopEmbedShow, processName]);

  /** Adopt exactly the window the user clicked. */
  const attachPicked = useCallback(
    async (hwnd: string) => {
      const rect = slotRef.current?.getBoundingClientRect();
      if (!rect) return;
      setBusy(true);
      try {
        const info: DesktopWindowInfo = await invokeDesktopEmbedAttachWindow({
          processName,
          hwnd,
          x: rect.x,
          y: rect.y,
          width: rect.width,
          height: rect.height,
        });
        if (!aliveRef.current) return;
        attachedRef.current = true;
        setErrorText("");
        setCandidates([]);
        setWindowTitle(info.title);
        setWindowClass(info.class);
        setWarning(info.warning);
        setPhase("attached");
        closePicker();
      } catch (err) {
        if (aliveRef.current) {
          setErrorText(err instanceof Error ? err.message : String(err));
          setPhase("error");
          closePicker();
          await diagnose();
        }
      } finally {
        if (aliveRef.current) setBusy(false);
      }
    },
    [closePicker, diagnose, invokeDesktopEmbedAttachWindow, processName]
  );

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
        if (!aliveRef.current || activeIdRef.current !== active.id) {
          // Adopted while the user already left: the effect cleanup for this
          // app has already run, so nothing else would hide this window — it
          // would keep floating over the workbench. Undo the adoption's
          // visibility here, exactly like startAndAttach does.
          void enqueueShow(processName, () => invokeDesktopEmbedShow(processName, false));
          return true;
        }
        attachedRef.current = true;
        setErrorText("");
        setCandidates([]);
        setWindowTitle(info.title);
        setWindowClass(info.class);
        setWarning(info.warning);
        setPhase("attached");
        return true;
      } catch (err) {
        attachedRef.current = false;
        if (aliveRef.current) setErrorText(err instanceof Error ? err.message : String(err));
        return false;
      }
    },
    [active.id, invokeDesktopEmbedAttach, invokeDesktopEmbedShow, processName]
  );

  // Per-app lifecycle: adopt what is already running, follow the slot on resize,
  // and hide (never detach) when the page goes away.
  useEffect(() => {
    aliveRef.current = true;
    attachedRef.current = false;
    setWindowTitle("");
    setWindowClass("");
    setWarning("");
    setErrorText("");
    setCandidates([]);
    let frame = 0;
    // The window checks run every 400ms and some engines blink their frame
    // (Electron re-composition) — a warn chip must not flash for one bad
    // tick, nor stick forever once the state has cleared.
    let warnStreak = 0;
    let warnCode = "";

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
        })
          .then((result) => {
            if (!aliveRef.current) return;
            // The backend repairs the link itself: take over a window the app
            // un-parented, or adopt the fresh frame that replaced a dead one.
            if (result.state === "lost") {
              attachedRef.current = false;
              setPhase("idle");
              return;
            }
            // Every result refreshes the triad — including plain "ok", so a
            // stale warn chip cannot outlive its cause.
            if (result.title) setWindowTitle(result.title);
            if (result.class) setWindowClass(result.class);
            const code = result.warning || "";
            if (!code) {
              warnStreak = 0;
              warnCode = "";
              setWarning("");
              return;
            }
            // Only surface a condition that persists (~1.2s): one-tick blips
            // while the app repaints its frame are not worth shouting about.
            if (code === warnCode) warnStreak += 1;
            else {
              warnCode = code;
              warnStreak = 1;
            }
            if (warnStreak >= 3) setWarning(code);
          })
          .catch(() => {
            /* transient: the next tick tries again */
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
          setWindowClass(found.class);
          // No setWarning here: the condition may have cleared while the page
          // was away, and a direct set would bypass the debounce below and
          // flash a stale chip until the first heartbeat corrects it.
          setPhase("attached");
          void enqueueShow(processName, () =>
            invokeDesktopEmbedShow(processName, true).then(() => invokeDesktopEmbedFocus(processName))
          ).then(() => sync());
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
    // Resizes alone are not enough: an app that replaces its frame (splash →
    // main window) or un-parents itself must be repaired without user action.
    // Tick fast: a slow tick leaves a stray splash/main window visible on
    // the desktop for whole seconds. relayout's healthy path is a pair of
    // cheap Win32 calls, so 400ms is affordable.
    const heartbeat = window.setInterval(() => {
      if (attachedRef.current) sync();
    }, HEARTBEAT_MS);

    return () => {
      aliveRef.current = false;
      cancelAnimationFrame(frame);
      window.clearInterval(heartbeat);
      observer.disconnect();
      window.removeEventListener("resize", sync);
      // Hide, do not detach: the app keeps its window and its state.
      if (attachedRef.current) {
        attachedRef.current = false;
        void enqueueShow(processName, () => invokeDesktopEmbedShow(processName, false));
      }
    };
  }, [
    attach,
    invokeDesktopEmbedFind,
    invokeDesktopEmbedFocus,
    invokeDesktopEmbedShow,
    invokeDesktopEmbedRelayout,
    processName,
  ]);

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
      // The backend hunts in a tight loop (25ms), so a window appearing on
      // the desktop is adopted within one frame of showing up — a JS timer
      // poll would leave it flashing there for a full tick first. Each call
      // waits ATTACH_WAIT_MS; loop until the overall deadline.
      const deadline = Date.now() + WINDOW_WAIT_MS;
      let lastError = "";
      while (Date.now() < deadline) {
        if (!aliveRef.current || activeIdRef.current !== active.id) return;
        const rect = slotRef.current?.getBoundingClientRect();
        let info: DesktopWindowInfo | null = null;
        try {
          info = await invokeDesktopEmbedAttachWait({
            processName,
            x: rect?.x ?? 0,
            y: rect?.y ?? 0,
            width: rect?.width ?? 1,
            height: rect?.height ?? 1,
            waitMs: ATTACH_WAIT_MS,
          });
        } catch (err) {
          lastError = err instanceof Error ? err.message : String(err);
        }
        if (info) {
          if (!aliveRef.current || activeIdRef.current !== active.id) {
            // Adopted while the user had already left the tab — undo.
            void enqueueShow(processName, () =>
              invokeDesktopEmbedShow(processName, false).catch(() => undefined)
            );
            return;
          }
          attachedRef.current = true;
          setErrorText("");
          setCandidates([]);
          setWindowTitle(info.title);
          setWindowClass(info.class);
          setWarning(info.warning);
          setPhase("attached");
          // A launch-embed adopts while the app is still starting up: its
          // input pipeline can miss the first WM_SETFOCUS entirely (the
          // backend's refocus campaign covers ~1.6s, but a cold Electron
          // start can take longer). Re-fire the focus command twice; it
          // no-ops unless the workbench is the foreground window.
          for (const delay of [400, 1200]) {
            window.setTimeout(() => {
              if (aliveRef.current && activeIdRef.current === active.id && attachedRef.current) {
                void invokeDesktopEmbedFocus(processName).catch(() => undefined);
              }
            }, delay);
          }
          return;
        }
      }
      if (aliveRef.current && activeIdRef.current === active.id) {
        setErrorText(lastError);
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
    invokeDesktopEmbedFocus,
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
    // The hosted window is a native child placed above the webview, so it
    // would cover the confirm dialog: the dialog only "flashes" and its
    // buttons cannot be clicked. Hide it while the dialog is up, exactly like
    // the picker does, and bring it back if the user backs out.
    const wasAttached = attachedRef.current;
    if (wasAttached) {
      void enqueueShow(processName, () =>
        invokeDesktopEmbedShow(processName, false).catch(() => undefined)
      );
    }
    const ok = await confirm({
      title: t("kill.title"),
      message: t("kill.message", { name: label(active) }),
      confirmText: t("kill.confirm"),
      icon: "danger",
    });
    if (!ok) {
      if (wasAttached && attachedRef.current) {
        void enqueueShow(processName, () =>
          invokeDesktopEmbedShow(processName, true).catch(() => undefined)
        );
      }
      return;
    }
    setBusy(true);
    try {
      // Kill FIRST, while the window is still our child: terminating the
      // process destroys the child window in place. Detaching first would
      // release the window as a floating top-level frame on top of the
      // workbench for the whole (potentially seconds-long) kill.
      // Desktop apps (DeepSeek Harness, Cursor) are close-to-tray: a soft kill
      // only hides them, so force a tree kill instead.
      await invokeKillApp(processName, true);
      // Trust the window, not the tasklist: kill_app may report success while
      // the process is still alive (image-name mismatch, stubborn shut-down).
      // Detaching in that state would release a live window as a top-level
      // frame parked over the whole workbench, which looks like a freeze.
      const stillRunning = await invokeIsAppRunning(processName).catch(() => true);
      if (stillRunning) {
        if (attachedRef.current) {
          setWarning("kill-failed");
          // It survived — it is only hidden right now, so bring it back.
          void enqueueShow(processName, () =>
            invokeDesktopEmbedShow(processName, true).catch(() => undefined)
          );
        }
        return;
      }
      // The unparent entry is now stale; drop it so the next attach starts
      // clean even if the kill saw the window as "not running".
      await invokeDesktopEmbedDetach(processName).catch(() => undefined);
      attachedRef.current = false;
      setPhase("idle");
    } catch (e) {
      // The window is still embedded (its process survived); keep phase
      // attached so nothing floats over the workbench.
      if (attachedRef.current) {
        setWarning("kill-failed");
        void enqueueShow(processName, () =>
          invokeDesktopEmbedShow(processName, true).catch(() => undefined)
        );
      } else {
        setErrorText(String(e));
        setPhase("error");
      }
    } finally {
      if (aliveRef.current) setBusy(false);
    }
  }, [
    active,
    confirm,
    invokeDesktopEmbedDetach,
    invokeDesktopEmbedShow,
    invokeIsAppRunning,
    invokeKillApp,
    label,
    processName,
    t,
  ]);

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
    // Same native-window-covers-the-dialog problem as killApp: hide the
    // hosted window while the confirm is up.
    const wasAttached = attachedRef.current;
    if (wasAttached) {
      await enqueueShow(processName, () =>
        invokeDesktopEmbedShow(processName, false).catch(() => undefined)
      );
    }
    const ok = await confirm({
      title: t("remove.title"),
      message: t("remove.message", { name: label(active) }),
      confirmText: t("remove.confirm"),
      icon: "danger",
    });
    if (!ok) {
      if (wasAttached && attachedRef.current) {
        void enqueueShow(processName, () =>
          invokeDesktopEmbedShow(processName, true).catch(() => undefined)
        );
      }
      return;
    }
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
  }, [
    active,
    confirm,
    custom,
    invokeDesktopEmbedDetach,
    invokeDesktopEmbedShow,
    label,
    processName,
    t,
  ]);

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

  const actionItems: TbarAction[] = [
    {
      key: "launch",
      label: t("actions.launch"),
      icon: <Play size={13} />,
      variant: "primary",
      disabled: busy || phase === "attached",
      onClick: () => void startAndAttach(),
    },
    {
      key: "reattach",
      label: t("actions.reattach"),
      icon: <RefreshCw size={13} />,
      variant: "secondary",
      disabled: busy || phase === "idle" || phase === "missing",
      onClick: () => void reattach(),
    },
    {
      key: "picker",
      label: t("picker.action"),
      icon: <MousePointerClick size={13} />,
      variant: "secondary",
      title: t("picker.hint"),
      disabled: busy || phase === "idle",
      onClick: () => void openPicker(),
    },
    {
      key: "detach",
      label: t("actions.detach"),
      icon: <Unplug size={13} />,
      variant: "secondary",
      disabled: busy || phase !== "attached",
      onClick: () => void detach(),
    },
    {
      key: "kill",
      label: t("actions.kill"),
      icon: <XCircle size={13} />,
      variant: "danger",
      disabled: busy || phase === "idle" || phase === "missing",
      onClick: () => void killApp(),
    },
    ...(active.custom
      ? [
          {
            key: "remove",
            label: t("actions.remove"),
            icon: <Trash2 size={13} />,
            variant: "danger" as const,
            disabled: busy,
            onClick: () => void removeApp(),
          },
        ]
      : []),
  ];
  const primaryAction =
    actionItems.find((a) => a.key === (phase === "attached" ? "reattach" : "launch")) ??
    actionItems[0];
  const overflowActions = actionItems.filter((a) => a !== primaryAction);

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
              aria-label={label(app)}
              title={label(app)}
              className={`dapps-tab is-icon${app.id === active.id ? " is-active" : ""}`}
              onClick={() => setActiveId(app.id)}
            >
              <AppTabIcon app={app} installed={installed} />
            </button>
          ))}
          <button type="button" className="dapps-tab dapps-tab-add" onClick={() => void addApp()}>
            <Plus size={14} />
            <span>{t("addApp")}</span>
          </button>
        </div>

        <span
          className={`dapps-status is-${phase}`}
          title={
            phase === "attached" && windowTitle
              ? `${windowClass} · ${windowTitle}`
              : undefined
          }
        >
          {busy && (phase === "launching" || phase === "polling") ? (
            <Loader2 size={13} className="dapps-spin" />
          ) : (
            <span className="dapps-dot" />
          )}
          <span className="dapps-status-text">{statusText}</span>
        </span>

        {phase === "attached" && warning ? (
          <span className="dapps-warn">
            <AlertTriangle size={13} />
            {t(`warn.${warning}`, { defaultValue: "" })}
          </span>
        ) : null}

        <span className="dapps-spacer" />

        <div className="dapps-actions">
          <div className="dapps-actions-wide">
            {actionItems.map((a) => (
              <button
                key={a.key}
                type="button"
                className={`btn btn-${a.variant} btn-small`}
                disabled={a.disabled}
                title={a.title}
                onClick={a.onClick}
              >
                {a.icon}
                {a.label}
              </button>
            ))}
          </div>
          <div className="dapps-actions-narrow">
            <button
              type="button"
              className={`btn btn-${primaryAction.variant} btn-small`}
              disabled={primaryAction.disabled}
              title={primaryAction.title ?? primaryAction.label}
              onClick={primaryAction.onClick}
            >
              {primaryAction.icon}
              {primaryAction.label}
            </button>
            <DropdownMenu
              label={t("actions.more")}
              triggerClassName="btn btn-secondary btn-small"
              items={overflowActions.map((a) => ({
                label: a.label,
                icon: a.icon,
                danger: a.variant === "danger",
                disabled: a.disabled,
                onClick: a.onClick,
              }))}
            >
              <MoreHorizontal size={13} />
            </DropdownMenu>
          </div>
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
                          <span className="dapps-error-win">{winName(c)}</span>
                          <button
                            type="button"
                            className="btn btn-secondary btn-small"
                            disabled={busy || !c.visible}
                            onClick={() => void attachPicked(c.hwnd)}
                          >
                            {t("picker.adopt")}
                          </button>
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

      {picking ? (
        <div className="modal-overlay" onClick={() => { if (!busy) closePicker(); }}>
          <div className="modal modal-lg dapps-picker" onClick={(event) => event.stopPropagation()}>
            <ModalTitleRow
              title={t("picker.title", { name: label(active) })}
              onClose={closePicker}
              disabled={busy}
              badge={
                <span className="card-title-badge">
                  {t("picker.count", { count: pickerList.length })}
                </span>
              }
            />
            <p className="dapps-picker-hint">{t("picker.hint")}</p>
            {pickerList.length === 0 ? (
              <p className="dapps-picker-empty">{t("picker.empty", { process: processName })}</p>
            ) : (
              (() => {
                const needle = pickerQuery.trim().toLowerCase();
                const shown = needle
                  ? pickerList.filter((c) =>
                      [c.title, c.class, c.hwnd, c.processName, String(c.pid)].some((field) =>
                        field.toLowerCase().includes(needle)
                      )
                    )
                  : pickerList;
                // Windows shown on the desktop first (recommended on top); the
                // currently hidden ones below. Both are adoptable — taking one
                // over reveals it — so the note only warns, it never blocks.
                const visible = shown
                  .filter((c) => c.visible || c.current)
                  .sort(
                    (a, b) =>
                      Number(b.current) - Number(a.current) ||
                      Number(b.recommended) - Number(a.recommended) ||
                      b.area - a.area
                  );
                const hidden = shown.filter((c) => !c.visible && !c.current);
                const grouped = visible.length > 0 && hidden.length > 0;
                const row = (c: DesktopWindowCandidate) => {
                  const isHidden = !c.visible && !c.current;
                  return (
                    <li
                      key={`${c.pid}-${c.hwnd}`}
                      className={c.current ? "is-current" : isHidden ? "is-hidden-row" : ""}
                    >
                      <div className="dapps-picker-main">
                        <span className="dapps-picker-title">
                          {winName(c)}
                          {c.current ? (
                            <em className="dapps-picker-badge">{t("picker.current")}</em>
                          ) : c.recommended ? (
                            <em className="dapps-picker-badge is-recommended">
                              {t("picker.recommended")}
                            </em>
                          ) : null}
                        </span>
                        <span className="dapps-picker-meta">
                          <span className="dapps-error-tag">{c.class}</span>
                          {c.width > 0 && c.height > 0 ? (
                            <span className="dapps-error-tag">
                              {t("picker.size", { w: c.width, h: c.height })}
                            </span>
                          ) : c.area > 0 ? (
                            <span className="dapps-error-tag">
                              {Math.round(c.area / 1000)}k px²
                            </span>
                          ) : null}
                          <span className="dapps-error-tag">{c.hwnd}</span>
                        </span>
                      </div>
                      {c.current ? (
                        <span className="dapps-picker-adopted">{t("picker.current")}</span>
                      ) : (
                        <>
                          {isHidden ? (
                            <span className="dapps-picker-blocked">
                              {t("picker.hiddenNote")}
                            </span>
                          ) : null}
                          <button
                            type="button"
                            className="btn btn-primary btn-small"
                            disabled={busy}
                            onClick={() => void attachPicked(c.hwnd)}
                          >
                            {t("picker.adopt")}
                          </button>
                        </>
                      )}
                    </li>
                  );
                };
                return (
                  <>
                    <input
                      className="dapps-picker-search"
                      type="text"
                      placeholder={t("picker.search")}
                      value={pickerQuery}
                      onChange={(event) => setPickerQuery(event.target.value)}
                      spellCheck={false}
                    />
                    {shown.length === 0 ? (
                      <p className="dapps-picker-empty">{t("picker.noMatch")}</p>
                    ) : (
                      <>
                        <p className="dapps-picker-note">{t("picker.visibleTip")}</p>
                        <ul className="dapps-picker-list">
                          {grouped ? (
                            <li className="dapps-picker-group">{t("picker.adoptable")}</li>
                          ) : null}
                          {visible.map(row)}
                          {grouped ? (
                            <li className="dapps-picker-group">{t("picker.hiddenGroup")}</li>
                          ) : null}
                          {hidden.map(row)}
                        </ul>
                      </>
                    )}
                  </>
                );
              })()
            )}
            <div className="dapps-picker-foot">
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={busy}
                onClick={() => void openPicker()}
              >
                <RefreshCw size={12} />
                {t("error.refresh")}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
