import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  AlertTriangle,
  AppWindow,
  Columns2,
  FolderSearch,
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
import AppLibraryDialog from "./desktopApps/AppLibraryDialog";
import { useRunningApps } from "./desktopApps/useRunningApps";
import { OrbitalLoadingStage } from "./OrbitalLoadingStage";
import type {
  DesktopRelayoutResult,
  DesktopWindowCandidate,
  DesktopWindowInfo,
  InstalledAppInfo,
} from "../core/types";
import {
  BUILTIN_DESKTOP_APPS,
  appFromInstalled,
  appFromRunningApp,
  appIdForProcess,
  dedupeApps,
  floatBarVeilBgr,
  loadActiveId,
  loadCustomApps,
  loadExePath,
  loadProcessNames,
  loadSlotBId,
  loadSplitView,
  loadTabOrder,
  matchExe,
  onStorageFailure,
  processNameFromExe,
  saveActiveId,
  saveCustomApps,
  saveExePath,
  saveProcessName,
  saveSlotBId,
  saveSplitView,
  saveTabOrder,
  type DesktopAppDef,
} from "../lib/desktopApps";
import { cachedAppIcon, loadAppIcon, warmAppIcon } from "../lib/appIcons";

type Phase =
  | "idle"
  | "running"
  | "attached"
  | "launching"
  | "polling"
  | "closing"
  | "missing"
  | "error";

/** Logical slot rect + devicePixelRatio, as sent to the embed backend. */
type SlotRect = { x: number; y: number; width: number; height: number; dpr: number };

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
/** Safety net for the outgoing tab's window: normally it is hidden the moment
 * the incoming window's show completes (which covers it), but if that never
 * lands (IPC failure, crash) the timeout hides it anyway. */
const PENDING_HIDE_TIMEOUT_MS = 250;

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
  const wrapped = async () => {
    try {
      await op();
    } finally {
      // A visibility change just settled: any outgoing window parked behind
      // the (now on-screen or now-gone) front window can leave safely.
      flushPendingHostedHides(processName);
    }
  };
  const next = (showQueues.get(processName) ?? Promise.resolve()).then(wrapped, wrapped);
  // The map must hold the promise this callback actually settles; comparing
  // against `next` (the promise `finally` was derived from) never matches, so
  // every process used to leave a settled entry behind forever.
  let settled: Promise<void>;
  settled = next.finally(() => {
    if (showQueues.get(processName) === settled) showQueues.delete(processName);
  });
  showQueues.set(processName, settled);
  return next;
};

/**
 * Outgoing tab windows waiting to leave. The hide must not race the incoming
 * window's show: hiding first exposed the slot background as a white flash
 * whenever the show took longer than the old fixed delay. Instead the hide is
 * parked here and runs when ANY visibility op settles (the incoming window is
 * on screen and covers this one) or the safety timeout fires — whichever first.
 */
type PendingHostedHide = { hide: () => void; timer: number };
const pendingHostedHides = new Map<string, PendingHostedHide>();

/** Run every parked hide except `except` (the window that just came on screen). */
function flushPendingHostedHides(except?: string): void {
  for (const [process, entry] of [...pendingHostedHides]) {
    if (process === except) continue;
    window.clearTimeout(entry.timer);
    pendingHostedHides.delete(process);
    entry.hide();
  }
}

/** The window is coming back on screen: its parked hide must not fire. */
function cancelPendingHostedHide(processName: string): void {
  const entry = pendingHostedHides.get(processName);
  if (entry) {
    window.clearTimeout(entry.timer);
    pendingHostedHides.delete(processName);
  }
}

/** The tab shows only the app's real icon; the name appears on hover. */
function AppTabIcon({
  app,
  installed,
}: {
  app: DesktopAppDef;
  installed: InstalledAppInfo[];
}) {
  const path = matchExe(app, installed);
  const [fetched, setFetched] = useState<string | null>(() => cachedAppIcon(path));

  useEffect(() => {
    if (!path) return;
    // A scan hit beats an extraction: it is already in hand.
    const fromScan = installed.find((item) => item.exe_path === path)?.icon;
    if (fromScan) {
      warmAppIcon(path, fromScan);
      setFetched(fromScan);
      return;
    }
    const cached = cachedAppIcon(path);
    if (cached) {
      setFetched(cached);
      return;
    }
    if (!path.toLowerCase().endsWith(".exe")) return;
    let cancelled = false;
    void loadAppIcon(path).then((data) => {
      if (!cancelled && data) setFetched(data);
    });
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

  /**
   * Last known phase per app id. Switching apps used to reset every piece of
   * state, so coming back to an app that was still hosted flashed the "not
   * running" hero before the backend answered. The cache only feeds the first
   * frame; the per-app effect still corrects it.
   */
  const phaseCacheRef = useRef<Map<string, Phase>>(new Map());

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
  const invokeDesktopEmbedSetVeilBgr = useGlobalStore((s) => s.invokeDesktopEmbedSetVeilBgr);
  const invokeDesktopEmbedReactivate = useGlobalStore((s) => s.invokeDesktopEmbedReactivate);
  const invokeDesktopEmbedRestore = useGlobalStore((s) => s.invokeDesktopEmbedRestore);
  const invokeDesktopEmbedDetach = useGlobalStore((s) => s.invokeDesktopEmbedDetach);
  const invokeDesktopEmbedCandidates = useGlobalStore((s) => s.invokeDesktopEmbedCandidates);
  const invokeDesktopEmbedAttachWindow = useGlobalStore((s) => s.invokeDesktopEmbedAttachWindow);
  const invokeDesktopEmbedList = useGlobalStore((s) => s.invokeDesktopEmbedList);
  const invokeDesktopEmbedProcessForExe = useGlobalStore(
    (s) => s.invokeDesktopEmbedProcessForExe
  );

  const [custom, setCustom] = useState<DesktopAppDef[]>(() => loadCustomApps());
  // Land on the app used last time: always opening on the first built-in app
  // was one of the reasons switching felt like work. A remembered id whose app
  // is gone (a removed custom entry) must fall back, or no tab is selected.
  const [activeId, setActiveId] = useState<string>(() => {
    const remembered = loadActiveId(BUILTIN_DESKTOP_APPS[0].id);
    const known = [...BUILTIN_DESKTOP_APPS, ...loadCustomApps()].some(
      (app) => app.id === remembered
    );
    return known ? remembered : BUILTIN_DESKTOP_APPS[0].id;
  });
  const [installed, setInstalled] = useState<InstalledAppInfo[]>([]);
  /** Start-Menu/Desktop shortcuts — what the library shows by default. */
  const [installedShortcuts, setInstalledShortcuts] = useState<InstalledAppInfo[]>([]);
  /** Registry + Program Files scan — only listed on request (runtimes, SDKs). */
  const [installedAll, setInstalledAll] = useState<InstalledAppInfo[]>([]);
  /**
   * App id → the image name that really owns its window. Filled the first time
   * a name derived from the file name fails to find anything (launchers,
   * renamed vendor binaries), and remembered across restarts.
   */
  const [processOverrides, setProcessOverrides] = useState<Record<string, string>>(() =>
    loadProcessNames()
  );
  const [libraryOpen, setLibraryOpen] = useState(false);
  /**
   * Slot layout. `single` is today's behavior to the pixel; `split` shows a
   * second pane beside the primary one. The secondary pane hosts a simpler
   * lifecycle (attach + heartbeat, no find/resolve/auto-start) — the full one
   * belongs to whichever app is the active tab.
   */
  const [splitView, setSplitView] = useState<boolean>(() => loadSplitView());
  /** App parked in the secondary pane ("" = chooser). */
  const [slotBId, setSlotBId] = useState<string>(() => loadSlotBId());
  const [slotBPhase, setSlotBPhase] = useState<Phase>("idle");
  const [slotBBusy, setSlotBBusy] = useState(false);
  /** One-shot "settings could not be saved" chip (storage full / blocked). */
  const [storageWarn, setStorageWarn] = useState(false);
  /** Tab strip order the user dragged into (persisted; unknown ids sort last). */
  const [tabOrder, setTabOrder] = useState<string[]>(() => loadTabOrder());
  /** App id currently being dragged (ref for the handlers, state for styling). */
  const dragTabIdRef = useRef<string | null>(null);
  const [dragTabId, setDragTabId] = useState<string | null>(null);
  /**
   * Which webview overlay currently covers the slot. The hosted window is a
   * native child painting above the webview, so any of these needs it hidden;
   * visibility is recomputed from the whole set on every transition, so two
   * overlapping overlays (menu → picker, library under picker) can never
   * double-fire a hide/show race or leave the window stuck hidden.
   */
  const overlaysRef = useRef({ library: false, picker: false, menu: false, confirm: false });
  /** Coalescing timer for setOverlay: the last flag set within a tick wins. */
  const overlaySyncTimerRef = useRef(0);
  /**
   * The name derived from the .exe found nothing and the backend is resolving
   * which image really owns the window (launchers, renamed binaries). Showing
   * this beats a bare "等待窗口出现…" that never explains itself.
   */
  const [locating, setLocating] = useState(false);
  /** The installed-app scan IPC died — the library must say why it is empty. */
  const [installedScanFailed, setInstalledScanFailed] = useState(false);
  const [phase, setPhase] = useState<Phase>(() => phaseCacheRef.current.get(activeId) ?? "idle");
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
  /** Row highlighted by the arrow keys (-1 = nothing, Enter is a no-op). */
  const [pickerActive, setPickerActive] = useState(-1);

  /**
   * Processes whose window is currently parented into the workbench (backed by
   * `desktop_embed_list` on mount). Switching to one of these can show it right
   * away instead of waiting for `find` to confirm — that round trip was the
   * visible pause on every tab switch.
   */
  const hostedRef = useRef<Set<string>>(new Set());
  /** Set by "add an app": the new tab opens itself once its window is located. */
  const autoStartRef = useRef<string | null>(null);
  /**
   * Bumped by "取消": a startAndAttach run whose token no longer matches must
   * unwind without touching state — the canceller already reset it. A token
   * (not a boolean) also makes a rapid start→cancel→start unambiguous.
   */
  const cancelLaunchRef = useRef(0);
  /**
   * The process the ACTIVE slot last attached ("" = none). The next effect
   * start reads this for orphan detection: a window still on screen whose
   * handoff never got registered must be hidden by someone, or it floats
   * over the new tab forever.
   */
  const lastActiveHostedRef = useRef("");
  /** Mirror of the secondary slot's process, read at effect start w/o deps. */
  const slotBProcessRef = useRef("");
  /** Latest `startAndAttach`, reachable from the older per-app effect. */
  const startRef = useRef<(() => Promise<void>) | null>(null);

  const slotRef = useRef<HTMLDivElement>(null);
  const slotBRef = useRef<HTMLDivElement>(null);
  const storageWarnTimer = useRef(0);
  const attachedRef = useRef(false);
  const aliveRef = useRef(true);
  const activeIdRef = useRef(activeId);
  /** Live process name for listeners that must not re-subscribe per tab. */
  const processNameRef = useRef(BUILTIN_DESKTOP_APPS[0].processName);
  /** Bumped on every per-app effect run; stale async results must not win. */
  const generationRef = useRef(0);
  const pickerSearchRef = useRef<HTMLInputElement>(null);
  const pickerListRef = useRef<HTMLUListElement>(null);
  /** Focused element when the picker opened; restored if nothing is embedded. */
  const returnFocusRef = useRef<HTMLElement | null>(null);

  /**
   * Slot rect plus the live devicePixelRatio. The backend maps CSS px to
   * physical px with this instead of the bare DPI scale: WebView2's
   * devicePixelRatio also carries page zoom (Ctrl+wheel) and per-monitor DPI
   * changes, so the hosted window stays glued to the slot in both cases.
   */
  const slotRect = useCallback(
    () => {
      const rect = slotRef.current?.getBoundingClientRect();
      if (!rect) return null;
      return {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        dpr: window.devicePixelRatio,
      };
    },
    []
  );

  /** Any webview overlay open right now? The effect reads this as a ref. */
  const anyOverlayOpen = useCallback(
    () => Object.values(overlaysRef.current).some(Boolean),
    []
  );

  /**
   * Flip one overlay's flag and recompute the hosted window's visibility from
   * the whole set — one enqueued hide on open, one enqueued show+focus on the
   * transition back to "nothing open". This replaces the three (then five)
   * ad-hoc hide/show pairs whose interleaving used to produce the
   * disappear→reappear→flash-again dance around every dialog.
   *
   * Transitions are coalesced through a 0ms timer: closing the ⋯ menu and
   * opening the picker happen back-to-back in one click, and enqueueing both
   * would order a show-then-hide (or hide-then-show) race — the window could
   * flash over the picker. Only the FINAL flag set runs an operation; the
   * promise resolves once that operation settles (or immediately when the
   * flags were overwritten by a direct write + `cancelOverlaySync`).
   */
  const setOverlay = useCallback(
    (key: "library" | "picker" | "menu" | "confirm", open: boolean): Promise<void> => {
      overlaysRef.current[key] = open;
      window.clearTimeout(overlaySyncTimerRef.current);
      return new Promise<void>((resolve) => {
        overlaySyncTimerRef.current = window.setTimeout(() => {
          const anyOpen = anyOverlayOpen();
          if (!attachedRef.current) {
            resolve();
            return;
          }
          const process = processNameRef.current;
          void enqueueShow(process, () =>
            invokeDesktopEmbedShow(process, !anyOpen)
              .then(() => {
                if (!anyOpen) return invokeDesktopEmbedFocus(process);
              })
              .catch(() => undefined)
          ).then(resolve, resolve);
        }, 0);
      });
    },
    [anyOverlayOpen, invokeDesktopEmbedFocus, invokeDesktopEmbedShow]
  );

  /**
   * For sites that flip an overlay flag DIRECTLY (adopt / switch / remove
   * paths, which manage visibility through the per-app effect): drop any
   * coalescing operation that was computed from the now-stale flag set.
   */
  const cancelOverlaySync = useCallback(() => {
    window.clearTimeout(overlaySyncTimerRef.current);
  }, []);

  const apps = useMemo(() => {
    const all = dedupeApps([...BUILTIN_DESKTOP_APPS, ...custom]);
    // The user's dragged order wins; ids the order does not know yet (a fresh
    // install, a newly added app) keep their natural order at the end — the
    // sort is stable, so equal ranks never reshuffle.
    const rank = new Map(tabOrder.map((id, index) => [id, index]));
    return [...all].sort(
      (a, b) =>
        (rank.get(a.id) ?? Number.MAX_SAFE_INTEGER) -
        (rank.get(b.id) ?? Number.MAX_SAFE_INTEGER)
    );
  }, [custom, tabOrder]);
  const active = useMemo(
    () => apps.find((app) => app.id === activeId) ?? apps[0],
    [apps, activeId]
  );
  /** The name every embed command matches on: the resolved one wins. */
  const processName = processOverrides[active.id] ?? active.processName;
  activeIdRef.current = activeId;
  processNameRef.current = processName;

  /** Everything the backend could host right now: status dots + library list. */
  const running = useRunningApps();

  const label = useCallback(
    (app: DesktopAppDef) => (app.labelKey ? t(app.labelKey) : app.label ?? app.processName),
    [t]
  );

  /** A candidate's human name: its title, or its class when it has none. */
  const winName = useCallback(
    (c: DesktopWindowCandidate) => c.title.trim() || t("win.fallback", { class: c.class }),
    [t]
  );

  // One ordered row list shared by rendering and keyboard navigation, so
  // Enter always adopts the row the arrow keys highlight.
  const pickerRows = useMemo(() => {
    const needle = pickerQuery.trim().toLowerCase();
    const shown = needle
      ? pickerList.filter((c) =>
          [c.title, c.class, c.hwnd, c.processName, String(c.pid)].some((field) =>
            String(field ?? "").toLowerCase().includes(needle)
          )
        )
      : pickerList;
    // Windows shown on the desktop first (recommended on top); the currently
    // hidden ones below. Both are adoptable — taking one over reveals it.
    const visible = shown
      .filter((c) => c.visible || c.current)
      .sort(
        (a, b) =>
          Number(b.current) - Number(a.current) ||
          Number(b.recommended) - Number(a.recommended) ||
          b.area - a.area
      );
    const hidden = shown.filter((c) => !c.visible && !c.current);
    return { shown, visible, hidden, rows: [...visible, ...hidden] };
  }, [pickerList, pickerQuery]);

  useEffect(() => {
    setPickerActive(-1);
  }, [pickerQuery, pickerList]);

  useEffect(() => {
    if (!picking) return;
    pickerSearchRef.current?.focus();
  }, [picking]);

  useEffect(() => {
    if (!picking || pickerActive < 0) return;
    const rows = pickerListRef.current?.querySelectorAll<HTMLElement>(".dapps-picker-row");
    rows?.[pickerActive]?.scrollIntoView({ block: "nearest" });
  }, [pickerActive, picking]);

  // Executables are only needed to *start* an app, and the scan is slow, so it
  // runs once per visit in the background. Shortcuts and the registry scan are
  // kept apart as well: the app library shows shortcuts by default (they are
  // what a user calls "my apps") and hides runtimes/SDKs behind a toggle.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [registry, shortcuts] = await Promise.all([
          invokeScanInstalledApps(),
          invokeScanDesktopApps(),
        ]);
        if (cancelled) return;
        setInstalledShortcuts(shortcuts);
        setInstalledAll(registry);
        setInstalled([...shortcuts, ...registry]);
        setInstalledScanFailed(false);
      } catch {
        /* no scan: the user can still pick an .exe by hand — but say why the
           library is empty instead of leaving it silently blank */
        setInstalledScanFailed(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [invokeScanInstalledApps, invokeScanDesktopApps]);

  // Seed "what we already host" once: a tab whose window is still parented in
  // can be shown immediately instead of going through `find` first.
  useEffect(() => {
    let cancelled = false;
    void invokeDesktopEmbedList()
      .then((list) => {
        if (cancelled) return;
        for (const info of list) {
          if (info.attached) hostedRef.current.add(info.processName.trim().toLowerCase());
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [invokeDesktopEmbedList]);

  // Keep the phase cache in step with the live state, so switching away always
  // leaves the true phase behind.
  useEffect(() => {
    phaseCacheRef.current.set(activeId, phase);
  }, [activeId, phase]);

  /**
   * Place the slot a couple more times right after an adoption.
   *
   * Shells that restore a saved window size (or re-apply their own decorations)
   * do it a beat after the takeover; the 400ms heartbeat alone can lose that
   * race and leave a window that is parented but visibly smaller than the slot.
   */
  const nudgeGeometry = useCallback(
    (targetId: string, process: string) => {
      for (const delay of [250, 700]) {
        window.setTimeout(() => {
          if (
            !aliveRef.current ||
            activeIdRef.current !== targetId ||
            !attachedRef.current
          ) {
            return;
          }
          const rect = slotRect();
          if (!rect || rect.width < 2 || rect.height < 2) return;
          void invokeDesktopEmbedRelayout({ processName: process, ...rect }).catch(
            () => undefined
          );
        }, delay);
      }
    },
    [invokeDesktopEmbedRelayout, slotRect]
  );

  /** Switch tabs, snapshotting the outgoing app's phase for a flash-free return. */
  const switchApp = useCallback(
    (id: string) => {
      if (id === activeIdRef.current) return;
      phaseCacheRef.current.set(activeIdRef.current, phase);
      saveActiveId(id);
      // In split view, clicking the secondary pane's tab swaps the panes: the
      // clicked app moves to the primary slot, where the full lifecycle
      // (find / resolve / heartbeat) lives, and the old primary parks in the
      // secondary one.
      if (splitView && id === slotBId) {
        const previous = activeIdRef.current;
        setSlotBId(previous);
        saveSlotBId(previous);
        setSlotBPhase(phaseCacheRef.current.get(previous) === "attached" ? "attached" : "idle");
      }
      setActiveId(id);
      setPhase(phaseCacheRef.current.get(id) ?? "idle");
      setWarning("");
      setErrorText("");
      setCandidates([]);
    },
    [phase, slotBId, splitView]
  );

  /**
   * Drop a dragged tab onto `targetId`: the dragged app moves to the target's
   * position (before it). Live-reordered on drag-over so the strip previews
   * the drop while dragging; each swap persists — swaps are per-target-change,
   * so this is a handful of writes per drag, not a storm.
   */
  const reorderTabs = useCallback(
    (draggedId: string, targetId: string) => {
      if (draggedId === targetId) return;
      const ids = apps.map((app) => app.id);
      const from = ids.indexOf(draggedId);
      const to = ids.indexOf(targetId);
      if (from < 0 || to < 0) return;
      ids.splice(to, 0, ...ids.splice(from, 1));
      setTabOrder(ids);
      saveTabOrder(ids);
    },
    [apps]
  );

  /** localStorage writes failing must be visible, not silent. */
  useEffect(() => {
    onStorageFailure(() => {
      setStorageWarn(true);
      window.clearTimeout(storageWarnTimer.current);
      storageWarnTimer.current = window.setTimeout(() => setStorageWarn(false), 6_000);
    });
    return () => {
      onStorageFailure(null);
      window.clearTimeout(storageWarnTimer.current);
    };
  }, []);

  /**
   * Which app the secondary pane hosts. "" whenever split is off, nothing is
   * parked there, or the parked app became the active tab (it must not be
   * managed by both panes). Deriving it here keeps the effect below free of
   * `activeId` as a dependency — switching primary tabs does not flicker the
   * secondary pane.
   */
  const slotBAppId = splitView && slotBId && slotBId !== activeId ? slotBId : "";
  slotBProcessRef.current = (() => {
    if (!slotBAppId) return "";
    const app = apps.find((a) => a.id === slotBAppId);
    return app ? processOverrides[app.id] ?? app.processName : "";
  })();

  // Secondary pane lifecycle: adopt what is running, follow its slot rect on
  // the same 400ms heartbeat, hide (never detach) when it goes away. No
  // find/resolve/auto-start here — that is the primary slot's job.
  useEffect(() => {
    if (!slotBAppId) return;
    const app = apps.find((a) => a.id === slotBAppId);
    if (!app) return;
    const name = processOverrides[app.id] ?? app.processName;
    let alive = true;
    let attached = false;
    let inFlight = false;
    let pendingRect: SlotRect | null = null;

    const rectOf = (): SlotRect | null => {
      const rect = slotBRef.current?.getBoundingClientRect();
      if (!rect) return null;
      return {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        dpr: window.devicePixelRatio,
      };
    };

    const flush = () => {
      if (inFlight || !attached || !alive) return;
      const rect = pendingRect;
      if (!rect) return;
      pendingRect = null;
      inFlight = true;
      void invokeDesktopEmbedRelayout({ processName: name, ...rect })
        .catch(() => undefined)
        .finally(() => {
          inFlight = false;
          if (pendingRect) flush();
        });
    };

    const sync = () => {
      if (!attached || !alive) return;
      const rect = rectOf();
      // Degenerate rects (layout not settled) would park the window in a sliver.
      if (!rect || rect.width < 2 || rect.height < 2) return;
      pendingRect = rect;
      flush();
    };

    setSlotBBusy(true);
    void (async () => {
      try {
        const rect = rectOf();
        const info: DesktopWindowInfo | null = await invokeDesktopEmbedAttach({
          processName: name,
          x: rect?.x ?? 0,
          y: rect?.y ?? 0,
          width: rect?.width ?? 1,
          height: rect?.height ?? 1,
          dpr: rect?.dpr ?? window.devicePixelRatio,
        }).catch(() => null);
        if (!alive) {
          if (info) {
            void enqueueShow(name, () =>
              invokeDesktopEmbedShow(name, false).catch(() => undefined)
            );
          }
          return;
        }
        if (info) {
          attached = true;
          setSlotBPhase("attached");
          // The primary slot's hide-on-switch may have left this entry with
          // shown=0; a visible pane wants it back (without stealing focus).
          if (!anyOverlayOpen()) {
            void enqueueShow(name, () =>
              invokeDesktopEmbedShow(name, true).catch(() => undefined)
            );
          }
          sync();
        } else {
          setSlotBPhase("running");
        }
      } finally {
        if (alive) setSlotBBusy(false);
      }
    })();

    const heartbeat = window.setInterval(sync, HEARTBEAT_MS);
    const observer = new ResizeObserver(sync);
    if (slotBRef.current) observer.observe(slotBRef.current);
    window.addEventListener("resize", sync);

    return () => {
      alive = false;
      window.clearInterval(heartbeat);
      observer.disconnect();
      window.removeEventListener("resize", sync);
      if (attached) {
        void enqueueShow(name, () =>
          invokeDesktopEmbedShow(name, false).catch(() => undefined)
        );
      }
    };
  }, [
    anyOverlayOpen,
    apps,
    invokeDesktopEmbedAttach,
    invokeDesktopEmbedRelayout,
    invokeDesktopEmbedShow,
    processOverrides,
    slotBAppId,
  ]);

  /** The windows the process owns — what "no usable window" actually saw. */
  const diagnose = useCallback(async () => {
    const gen = generationRef.current;
    const list = await invokeDesktopEmbedCandidates(processName).catch(() => []);
    if (aliveRef.current && generationRef.current === gen) setCandidates(list);
  }, [invokeDesktopEmbedCandidates, processName]);

  /** Open the picker: the candidates at this moment, adoptable by handle. */
  const openPicker = useCallback(async () => {
    // The picker outlives a tab switch unless every step is pinned to the
    // app it was opened for: a late candidate list must never surface under
    // the next app's name.
    const gen = generationRef.current;
    const process = processName;
    const isCurrent = () => aliveRef.current && generationRef.current === gen;
    // Opening from the toolbar/menu keeps that control as the return target;
    // the in-dialog refresh button must not overwrite it.
    if (!picking) {
      returnFocusRef.current =
        document.activeElement instanceof HTMLElement ? document.activeElement : null;
    }
    setPickerActive(-1);
    setBusy(true);
    try {
      let list = await invokeDesktopEmbedCandidates(process).catch(() => []);
      if (!isCurrent()) return;
      // relayout drops its map entry between remove and re-attach; if we came
      // back attached but no row is flagged current, that is the race — read
      // once more so the picker still marks the embedded window.
      if (attachedRef.current && !list.some((c) => c.current)) {
        list = await invokeDesktopEmbedCandidates(process).catch(() => list);
        if (!isCurrent()) return;
      }
      // The hosted window paints over the webview, so it would cover the very
      // dialog that is meant to replace it: hide it while choosing.
      setOverlay("picker", true);
      setPickerList(list);
      setPickerQuery("");
      setPicking(true);
    } finally {
      if (isCurrent()) setBusy(false);
    }
  }, [invokeDesktopEmbedCandidates, picking, processName, setOverlay]);

  /** Close the picker, bringing the adopted window back if it is still ours. */
  const closePicker = useCallback(() => {
    setPicking(false);
    setPickerActive(-1);
    // One show+focus, computed from the whole overlay set: the library may
    // still be open underneath, in which case the window stays hidden.
    void setOverlay("picker", false);
    if (!attachedRef.current) {
      // Nothing to hand focus to: put it back on the control that opened the
      // dialog (the menu item that triggered it is already unmounted, so fall
      // back to the toolbar's picker button / overflow trigger).
      const target = returnFocusRef.current;
      if (target?.isConnected) {
        target.focus();
        return;
      }
      const pickerAction = Array.from(
        document.querySelectorAll<HTMLElement>('[data-action="picker"]')
      ).find((el) => el.offsetParent !== null);
      (
        pickerAction ??
        document.querySelector<HTMLElement>('.dapps-actions [aria-haspopup="menu"]')
      )?.focus();
    }
  }, [setOverlay]);

  /** Adopt exactly the window the user clicked. */
  const attachPicked = useCallback(
    async (hwnd: string) => {
      const rect = slotRect();
      if (!rect) return;
      const gen = generationRef.current;
      const process = processName;
      const isCurrent = () => aliveRef.current && generationRef.current === gen;
      setBusy(true);
      try {
        const info: DesktopWindowInfo = await invokeDesktopEmbedAttachWindow({
          processName: process,
          hwnd,
          ...rect,
        });
        if (!isCurrent()) {
          // Adopted after the user left this app: undo the visibility exactly
          // like the auto-attach path, so it cannot float over the workbench.
          void enqueueShow(process, () =>
            invokeDesktopEmbedShow(process, false).catch(() => undefined)
          );
          return;
        }
        hostedRef.current.add(process.trim().toLowerCase());
        attachedRef.current = true;
        lastActiveHostedRef.current = process;
        setErrorText("");
        setCandidates([]);
        setWindowTitle(info.title);
        setWindowClass(info.class);
        setWarning(info.warning);
        setPhase("attached");
        nudgeGeometry(active.id, process);
        closePicker();
      } catch (err) {
        if (isCurrent()) {
          setErrorText(err instanceof Error ? err.message : String(err));
          setPhase("error");
          closePicker();
          await diagnose();
        }
      } finally {
        if (isCurrent()) setBusy(false);
      }
    },
    [
      active.id,
      closePicker,
      diagnose,
      invokeDesktopEmbedAttachWindow,
      invokeDesktopEmbedShow,
      nudgeGeometry,
      processName,
      slotRect,
    ]
  );

  /** Arrow keys move the highlight, Enter adopts it; focus stays in search. */
  const onPickerKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>) => {
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Enter") return;
      const adoptable = pickerRows.rows
        .map((candidate, index) => (candidate.current ? -1 : index))
        .filter((index) => index >= 0);
      if (adoptable.length === 0) return;
      if (event.key === "Enter") {
        // Enter belongs to the focused button (adopt / refresh) unless the
        // search box itself has focus.
        if (event.target !== pickerSearchRef.current || busy || pickerActive < 0) return;
        const candidate = pickerRows.rows[pickerActive];
        if (!candidate || candidate.current) return;
        event.preventDefault();
        void attachPicked(candidate.hwnd);
        return;
      }
      event.preventDefault();
      const at = adoptable.indexOf(pickerActive);
      const next =
        event.key === "ArrowDown"
          ? adoptable[at < 0 ? 0 : Math.min(at + 1, adoptable.length - 1)]
          : adoptable[at < 0 ? adoptable.length - 1 : Math.max(at - 1, 0)];
      setPickerActive(next);
    },
    [attachPicked, busy, pickerActive, pickerRows]
  );

  /** Adopt the app's window into the slot. `false` = no window to adopt. */
  const attach = useCallback(
    async (): Promise<boolean> => {
      const rect = slotRect();
      if (!rect || rect.width < 2 || rect.height < 2) return false;
      try {
        const info: DesktopWindowInfo = await invokeDesktopEmbedAttach({
          processName,
          ...rect,
        });
        if (!aliveRef.current || activeIdRef.current !== active.id) {
          // Adopted while the user already left: the effect cleanup for this
          // app has already run, so nothing else would hide this window — it
          // would keep floating over the workbench. Undo the adoption's
          // visibility here, exactly like startAndAttach does.
          void enqueueShow(processName, () =>
            invokeDesktopEmbedShow(processName, false).catch(() => undefined)
          );
          return true;
        }
        hostedRef.current.add(processName.trim().toLowerCase());
        attachedRef.current = true;
        lastActiveHostedRef.current = processName;
        setErrorText("");
        setCandidates([]);
        setWindowTitle(info.title);
        setWindowClass(info.class);
        setWarning(info.warning);
        setPhase("attached");
        nudgeGeometry(active.id, processName);
        if (anyOverlayOpen()) {
          // An attach forces the native window visible; with a dialog up it
          // would cover it, so re-apply the visibility the overlay set wants.
          void setOverlay("library", overlaysRef.current.library);
        }
        return true;
      } catch (err) {
        if (aliveRef.current && activeIdRef.current === active.id) {
          attachedRef.current = false;
          setErrorText(err instanceof Error ? err.message : String(err));
        }
        return false;
      }
    },
    [
      active.id,
      anyOverlayOpen,
      invokeDesktopEmbedAttach,
      nudgeGeometry,
      processName,
      setOverlay,
      slotRect,
    ]
  );

  // Per-app lifecycle: adopt what is already running, follow the slot on resize,
  // and hide (never detach) when the page goes away.
  useEffect(() => {
    // `aliveRef` alone cannot tell one app's effect from the next: the new
    // effect flips it back to true while the old app's async work is still in
    // flight. A generation token is what makes "this result is mine" decidable.
    const gen = ++generationRef.current;
    const isCurrent = () => aliveRef.current && generationRef.current === gen;
    aliveRef.current = true;
    attachedRef.current = false;
    // This window is coming back on screen (or about to be adopted): its own
    // parked hide must never fire mid-show.
    cancelPendingHostedHide(processName);
    // Orphan guard: the outgoing window is normally parked for a hide by this
    // effect's cleanup (attached → switch). But when the heartbeat had already
    // declared it lost (attachedRef false → no handoff registered), nobody
    // would ever hide it — the window floats over the new tab forever, its
    // map entry stale while the actual window is alive and shown. Hide it
    // here, unless the normal handoff did register, or it now lives in the
    // secondary pane.
    const previousHosted = lastActiveHostedRef.current;
    lastActiveHostedRef.current = "";
    if (
      previousHosted &&
      previousHosted !== processName.trim().toLowerCase() &&
      previousHosted !== slotBProcessRef.current &&
      !pendingHostedHides.has(previousHosted)
    ) {
      void enqueueShow(previousHosted, () =>
        invokeDesktopEmbedShow(previousHosted, false).catch(() => undefined)
      );
    }
    setWindowTitle("");
    setWindowClass("");
    setWarning("");
    setErrorText("");
    setCandidates([]);
    setLocating(false);
    // A picker opened for the previous app must not survive the switch: its
    // list carries old HWNDs and would be adopted under the new process name.
    setPicking(false);
    setPickerList([]);
    setPickerQuery("");
    // The window checks run every 400ms and some engines blink their frame
    // (Electron re-composition) — a warn chip must not flash for one bad
    // tick, nor stick forever once the state has cleared.
    let warnStreak = 0;
    let warnCode = "";
    // relayout follows the slot frame by frame across an async IPC boundary.
    // During the sidebar width animation a request per frame overlaps, and
    // Tauri does not guarantee the order the commands execute in — an old
    // rect could land after a newer one (the window visibly jumps back).
    // Keep one call in flight and only queue the newest rect behind it.
    let pendingRect: SlotRect | null = null;
    let inFlight = false;
    let settleTimer = 0;
    let lastRectKey = "";
    /**
     * Whether this effect run already revealed the window. The optimistic
     * path shows it before `find` answers; when `find` then confirms, a
     * second show would re-run the backend focus cycle (hide → focus → show)
     * — the visible double blink on every tab switch.
     */
    let shownThisRun = false;

    const applyRelayout = (result: DesktopRelayoutResult) => {
      if (!isCurrent()) return;
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
    };

    const flushRelayout = () => {
      if (inFlight) return;
      if (!isCurrent() || !attachedRef.current) {
        pendingRect = null;
        return;
      }
      if (!pendingRect) return;
      const rect = pendingRect;
      pendingRect = null;
      inFlight = true;
      void invokeDesktopEmbedRelayout({ processName, ...rect })
        .then((result) => {
          // A newer target is already queued: its result is the one that
          // describes the final state. Applying this stale result could
          // flash an old warning or title over the new one.
          if (pendingRect) return;
          applyRelayout(result);
        })
        .catch(() => {
          /* transient: the next tick tries again */
        })
        .finally(() => {
          inFlight = false;
          if (isCurrent() && attachedRef.current) {
            if (pendingRect) flushRelayout();
          } else {
            pendingRect = null;
          }
        });
    };

    const queueRelayout = (rect: SlotRect) => {
      pendingRect = rect;
      flushRelayout();
    };

    const sync = () => {
      if (!attachedRef.current) return;
      const rect = slotRect();
      // A degenerate rect (page just remounted, layout not settled yet)
      // would place the hosted window into a sliver — wait for a real one.
      if (!rect || rect.width < 2 || rect.height < 2) return;
      const key = `${rect.x},${rect.y},${rect.width},${rect.height},${rect.dpr}`;
      if (key !== lastRectKey) {
        lastRectKey = key;
        // ResizeObserver goes quiet the moment the width animation ends. One
        // call after the layout has been stable for a couple of frames makes
        // sure the window lands on the settled rect even if the last
        // in-flight request was coalesced away.
        window.clearTimeout(settleTimer);
        settleTimer = window.setTimeout(() => {
          if (!isCurrent() || !attachedRef.current) return;
          const settled = slotRect();
          if (!settled || settled.width < 2 || settled.height < 2) return;
          queueRelayout(settled);
        }, 140);
      }
      queueRelayout(rect);
    };

    /**
     * Reveal the hosted window — unless a webview overlay (library, picker,
     * menu, confirm) is on screen, in which case it must stay hidden: it is a
     * native child and would paint over the dialog. `overlaysRef` is read as a
     * ref because this effect must not re-subscribe on every dialog toggle.
     *
     * Order matters against flicker: `show(true)` displays at the LAST known
     * rect (SWP_NOMOVE|NOSIZE), so showing first and relayouting second made a
     * switched-to app visibly land at its stale geometry and then jump. Park
     * it at the slot's current rect while still hidden (relayout places with
     * HIDEWINDOW while the shown flag is 0), THEN show — the window appears
     * once, at the right place.
     */
    const showHosted = () => {
      if (anyOverlayOpen()) {
        void enqueueShow(processName, () =>
          invokeDesktopEmbedShow(processName, false).catch(() => undefined)
        );
        return;
      }
      shownThisRun = true;
      void enqueueShow(processName, async () => {
        const rect = slotRect();
        if (rect && rect.width >= 2 && rect.height >= 2) {
          await invokeDesktopEmbedRelayout({ processName, ...rect }).catch(() => undefined);
        }
        // The entry may be gone (the app recreated its window between the
        // optimistic show and here): a rejection must not escape into the
        // queue chain as an unhandled rejection.
        await invokeDesktopEmbedShow(processName, true).catch(() => undefined);
        await invokeDesktopEmbedFocus(processName).catch(() => undefined);
      });
    };

    /**
     * "Add an app" means "open it": only that explicit path auto-starts. The
     * page itself still never launches anything behind the user's back.
     * Consumed on every settle path — including "no window found", which is the
     * normal answer for an installed app that is not running yet.
     */
    const maybeAutoStart = () => {
      if (autoStartRef.current !== active.id) return;
      autoStartRef.current = null;
      // Deferred past this effect's `finally`: otherwise `busy` would flip back
      // to false while the launch we just triggered is still running.
      window.setTimeout(() => {
        if (aliveRef.current && activeIdRef.current === active.id) {
          void startRef.current?.();
        }
      }, 0);
    };

    // Optimistic path: a window we already host can go back on screen before
    // `find` answers. The call below still runs and corrects us either way.
    const hostedKey = processName.trim().toLowerCase();
    if (hostedRef.current.has(hostedKey)) {
      const rect = slotRect();
      if (rect) {
        attachedRef.current = true;
        lastActiveHostedRef.current = processName;
        setPhase("attached");
        showHosted();
        void invokeDesktopEmbedRelayout({ processName, ...rect })
          .then((result) => {
            if (isCurrent()) applyRelayout(result);
          })
          .catch(() => undefined);
      }
    } else {
      // No incoming window: the slot will show the placeholder, so the
      // outgoing window must leave now instead of waiting for a show that
      // would never come (the placeholder would stay covered behind it).
      flushPendingHostedHides();
    }

    void (async () => {
      setBusy(true);
      try {
        const found = await invokeDesktopEmbedFind(processName).catch(() => null);
        if (!isCurrent()) return;
        if (!found) {
          hostedRef.current.delete(hostedKey);
          // Undo the optimistic state: the window is gone (app quit, or it was
          // never ours), and a stale `attached` would keep the heartbeat alive.
          attachedRef.current = false;
          // The name came from the .exe file name. Ask the backend which image
          // actually owns a window for the launch path: a launcher hands the
          // window to a differently named binary, and without this the app
          // never embeds no matter how often it is started.
          const exe = loadExePath(active.id) ?? active.exePath ?? null;
          if (exe) setLocating(true);
          const resolved = exe
            ? await invokeDesktopEmbedProcessForExe(exe).catch(() => null)
            : null;
          if (!isCurrent()) return;
          setLocating(false);
          const real = resolved?.trim() ?? "";
          if (real && real.toLowerCase() !== hostedKey) {
            saveProcessName(active.id, real);
            setProcessOverrides((prev) => ({ ...prev, [active.id]: real }));
            // The effect re-runs with the resolved name and finds the window.
            return;
          }
          setPhase("idle");
          maybeAutoStart();
          return;
        }
        if (found.attached) {
          hostedRef.current.add(hostedKey);
          autoStartRef.current = null;
          attachedRef.current = true;
          lastActiveHostedRef.current = processName;
          setWindowTitle(found.title);
          setWindowClass(found.class);
          // No setWarning here: the condition may have cleared while the page
          // was away, and a direct set would bypass the debounce below and
          // flash a stale chip until the first heartbeat corrects it.
          setPhase("attached");
          // The optimistic path may have shown it already: a second show
          // re-runs the backend focus cycle (hide → focus → show) — flicker.
          if (!shownThisRun) showHosted();
          if (!anyOverlayOpen()) {
            window.setTimeout(() => {
              if (isCurrent()) sync();
            }, 0);
          }
          return;
        }
        const ok = await attach();
        if (!isCurrent()) return;
        if (ok) {
          hostedRef.current.add(hostedKey);
          autoStartRef.current = null;
          return;
        }
        // Not running yet is the normal case here: stay quiet about it.
        setPhase("running");
        setErrorText("");
        setCandidates([]);
        maybeAutoStart();
      } finally {
        if (isCurrent()) setBusy(false);
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
      window.clearTimeout(settleTimer);
      pendingRect = null;
      window.clearInterval(heartbeat);
      observer.disconnect();
      window.removeEventListener("resize", sync);
      // Hide, do not detach: the app keeps its window and its state.
      if (!attachedRef.current) return;
      attachedRef.current = false;
      const genAtCleanup = gen;
      const previousActiveId = active.id;
      const processToHide = processName;
      const hide = () => {
        // But if this app became the active tab again, its effect has
        // already shown the window — hiding here would tuck away a live
        // pane (and the heartbeat would keep it hidden, shown=0).
        if (aliveRef.current && activeIdRef.current === previousActiveId) return;
        if (generationRef.current === genAtCleanup && aliveRef.current) return;
        void enqueueShow(processToHide, () =>
          invokeDesktopEmbedShow(processToHide, false).catch(() => undefined)
        );
      };
      // Decide after React runs the next effect in this commit:
      // - Still on 协同办公 (switched apps): park for the show handoff so
      //   the slot does not flash empty between hide and show.
      // - Left the page for another menu: hide NOW. The parked 250ms path
      //   was for in-page tab switches; with no incoming show to flush it,
      //   the native child floated over 工作待办 / Git / … for a quarter
      //   second — that is the flash when switching the sidebar.
      queueMicrotask(() => {
        if (!aliveRef.current) {
          cancelPendingHostedHide(processToHide);
          hide();
          flushPendingHostedHides();
          return;
        }
        const timer = window.setTimeout(() => {
          pendingHostedHides.delete(processToHide);
          hide();
        }, PENDING_HIDE_TIMEOUT_MS);
        cancelPendingHostedHide(processToHide);
        pendingHostedHides.set(processToHide, { hide, timer });
      });
    };
  }, [
    active.id,
    anyOverlayOpen,
    attach,
    invokeDesktopEmbedFind,
    invokeDesktopEmbedFocus,
    invokeDesktopEmbedProcessForExe,
    invokeDesktopEmbedShow,
    invokeDesktopEmbedRelayout,
    processName,
    slotRect,
  ]);

  // Keep the native reactivate veil in sync with `--float-bar-bg` so hide/show
  // flashes the theme's solid float surface instead of bare webview white.
  useEffect(() => {
    const sync = () => {
      void invokeDesktopEmbedSetVeilBgr(floatBarVeilBgr()).catch(() => undefined);
    };
    sync();
    window.addEventListener("app-theme-changed", sync);
    return () => window.removeEventListener("app-theme-changed", sync);
  }, [invokeDesktopEmbedSetVeilBgr]);

  // Re-activate the hosted window whenever the workbench regains the
  // foreground, so an image paste (and typing) works without switching tabs.
  //
  // The embedded window is a cross-process WS_CHILD whose input queue is
  // attached to the host thread; Win32 focus is per-queue, and Chromium's own
  // document-active / page-visibility state is separate again. When the user
  // leaves the workbench (企微 screenshot, a browser, the AI Hub window, …)
  // and comes back, the child regains HWND keyboard focus, but the hosted
  // Chromium document does NOT re-activate on its own — and the ASYNC
  // clipboard path an image Ctrl+V uses is gated on `document.hasFocus()`, so
  // the image paste is silently dropped (a plain text paste has a lower bar
  // and sometimes slips through). Switching tabs "fixes" it only because the
  // window really goes hidden on another tab and Chromium processes
  // WasHidden→WasShown, which re-activates the document. `desktop_embed_reactivate`
  // reproduces that occlusion deterministically (hide → one pump turn → show),
  // and is a no-op unless the host is foreground and the window is shown, so it
  // is safe to fire on every foreground gain.
  //
  // This onFocusChanged path is the fast path when activation goes through the
  // Tauri chrome. Clicking straight into the foreign child often skips it —
  // reliable fallback is `relayout`'s host_was_background detection in
  // desktop_embed.rs (same reactivate, debounced so the two do not double-blink).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let dead = false;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (!focused || !attachedRef.current) return;
        void invokeDesktopEmbedReactivate(processNameRef.current).catch(() => undefined);
      })
      .then((fn) => {
        if (dead) fn();
        else unlisten = fn;
      });
    return () => {
      dead = true;
      unlisten?.();
    };
  }, [invokeDesktopEmbedReactivate]);

  const startAndAttach = useCallback(async () => {
    setBusy(true);
    setErrorText("");
    setCandidates([]);
    // Token-scoped: "取消" bumps the ref and this run unwinds silently.
    const cancelToken = ++cancelLaunchRef.current;
    const cancelled = () => cancelLaunchRef.current !== cancelToken;
    try {
      const exe = matchExe(active, installed);
      if (!exe) {
        // A tab switch while this was in flight must not rewrite the new tab's
        // phase; same guard as the rest of this callback.
        if (aliveRef.current && activeIdRef.current === active.id) setPhase("missing");
        return;
      }
      // Shared launch/resolve steps for both paths below.
      const launchNow = async (): Promise<boolean> => {
        if (cancelled()) return false;
        if (aliveRef.current && activeIdRef.current === active.id) setPhase("launching");
        try {
          await invokeLaunchApp(exe, active.args?.join(" "));
          return true;
        } catch (err) {
          if (aliveRef.current && activeIdRef.current === active.id) {
            setErrorText(
              t("error.launch", {
                message: err instanceof Error ? err.message : String(err),
              })
            );
            setPhase("error");
          }
          return false;
        }
      };
      // A window may already exist under a differently named image (launchers,
      // renamed vendor binaries): handing over to the per-app effect with the
      // real name beats launching a duplicate. Returns true when corrected.
      const resolveRealName = async (): Promise<boolean> => {
        setLocating(true);
        const resolved = await invokeDesktopEmbedProcessForExe(exe).catch(() => null);
        if (!aliveRef.current || activeIdRef.current !== active.id || cancelled()) return false;
        setLocating(false);
        const real = resolved?.trim() ?? "";
        if (real && real.toLowerCase() !== processName.trim().toLowerCase()) {
          saveProcessName(active.id, real);
          setProcessOverrides((prev) => ({ ...prev, [active.id]: real }));
          return true;
        }
        return false;
      };

      // Cold start fast path: no process → launch right away, no 3s detour.
      const runningNow = await invokeIsAppRunning(processName).catch(() => false);
      if (!aliveRef.current || activeIdRef.current !== active.id || cancelled()) return;
      let launchAttempted = false;
      if (!runningNow) {
        if (await resolveRealName()) return;
        if (cancelled()) return;
        launchAttempted = true;
        if (!(await launchNow())) return;
      }
      if (!aliveRef.current || activeIdRef.current !== active.id || cancelled()) return;
      setPhase("polling");
      // The backend hunts in a tight loop (25ms), so a window appearing on
      // the desktop is adopted within one frame of showing up — a JS timer
      // poll would leave it flashing there for a full tick first. Each call
      // waits ATTACH_WAIT_MS; loop until the overall deadline.
      const deadline = Date.now() + WINDOW_WAIT_MS;
      let lastError = "";
      while (Date.now() < deadline) {
        if (!aliveRef.current || activeIdRef.current !== active.id || cancelled()) return;
        const rect = slotRect();
        let info: DesktopWindowInfo | null = null;
        try {
          info = await invokeDesktopEmbedAttachWait({
            processName,
            x: rect?.x ?? 0,
            y: rect?.y ?? 0,
            width: rect?.width ?? 1,
            height: rect?.height ?? 1,
            dpr: rect?.dpr ?? window.devicePixelRatio,
            waitMs: ATTACH_WAIT_MS,
          });
        } catch (err) {
          lastError = err instanceof Error ? err.message : String(err);
        }
        if (info) {
          if (
            !aliveRef.current ||
            activeIdRef.current !== active.id ||
            // Cancelled just as the window appeared: the adoption already
            // happened backend-side, so undo its visibility exactly like the
            // left-the-tab path instead of floating it over the workbench.
            cancelled()
          ) {
            // Adopted while the user had already left the tab — undo.
            void enqueueShow(processName, () =>
              invokeDesktopEmbedShow(processName, false).catch(() => undefined)
            );
            return;
          }
          attachedRef.current = true;
          lastActiveHostedRef.current = processName;
          hostedRef.current.add(processName.trim().toLowerCase());
          setErrorText("");
          setCandidates([]);
          setWindowTitle(info.title);
          setWindowClass(info.class);
          setWarning(info.warning);
          setPhase("attached");
          if (anyOverlayOpen()) {
            // The user opened a dialog while the app was still starting:
            // keep the freshly adopted window behind it.
            void setOverlay("library", overlaysRef.current.library);
          }
          nudgeGeometry(active.id, processName);
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
        if (!launchAttempted) {
          // The process IS running (the fast path skipped the launch) but a
          // full wait round produced no window: close-to-tray and
          // background-instance shells (360ChromeX keeps eight processes with
          // nothing to adopt) mistake `is_app_running` for "usable". A launch
          // is what makes a single-instance app surface its window — and for
          // genuinely multi-instance apps, a new window is what 启动 means.
          launchAttempted = true;
          if (await resolveRealName()) return;
          if (cancelled()) return;
          if (!(await launchNow())) return;
          if (!aliveRef.current || activeIdRef.current !== active.id || cancelled()) return;
          setPhase("polling");
          continue;
        }
        // Nothing adopted yet. The launch may have handed the window to a
        // differently named image (launchers, renamed vendor binaries) — that
        // is the single most common reason a freshly added app never embeds.
        // Ask the backend which process owns a window for the path we started
        // and hand over to the per-app effect with the real name.
        if (await resolveRealName()) return;
      }
      if (aliveRef.current && activeIdRef.current === active.id && !cancelled()) {
        setErrorText(lastError);
        setPhase("error");
        await diagnose();
      }
    } catch (err) {
      if (aliveRef.current && activeIdRef.current === active.id && !cancelled()) {
        setErrorText(err instanceof Error ? err.message : String(err));
        setPhase("error");
        await diagnose();
      }
    } finally {
      if (aliveRef.current && activeIdRef.current === active.id) setBusy(false);
    }
  }, [
    active,
    anyOverlayOpen,
    attach,
    diagnose,
    installed,
    invokeDesktopEmbedFind,
    invokeDesktopEmbedFocus,
    invokeDesktopEmbedProcessForExe,
    invokeIsAppRunning,
    invokeLaunchApp,
    nudgeGeometry,
    processName,
    setOverlay,
    t,
  ]);

  // The per-app effect is declared above, so it reaches this through a ref:
  // that is how "add an app" opens the thing the user just added.
  startRef.current = startAndAttach;

  /**
   * Open the app library; the hosted window must step aside while it is up.
   * The hide is fired, not awaited: the queue may still be settling a previous
   * show, and the dialog can appear a frame later without being covered.
   */
  const openLibrary = useCallback(() => {
    // Flip the flag before anything async: an attach that lands between this
    // click and the next render must already know the dialog is coming.
    overlaysRef.current.library = true;
    setLibraryOpen(true);
    void setOverlay("library", true);
    void running.refresh();
  }, [running, setOverlay]);

  const closeLibrary = useCallback(() => {
    setLibraryOpen(false);
    void setOverlay("library", false);
  }, [setOverlay]);

  /** Re-run the installed-app scan (the library's empty state offers this). */
  const rescanInstalled = useCallback(async () => {
    try {
      const [registry, shortcuts] = await Promise.all([
        invokeScanInstalledApps(),
        invokeScanDesktopApps(),
      ]);
      setInstalledShortcuts(shortcuts);
      setInstalledAll(registry);
      setInstalled([...shortcuts, ...registry]);
      setInstalledScanFailed(false);
    } catch {
      /* keep the previous list: a failed rescan must not empty the library,
         but flag it so the empty state can offer the retry again */
      setInstalledScanFailed(true);
    }
  }, [invokeScanInstalledApps, invokeScanDesktopApps]);

  const reattach = useCallback(async () => {
    const gen = generationRef.current;
    setBusy(true);
    try {
      if (await attach()) return;
      await diagnose();
      const found = await invokeDesktopEmbedFind(processName).catch(() => null);
      if (aliveRef.current && generationRef.current === gen && activeIdRef.current === active.id) {
        setPhase(found ? "error" : "idle");
      }
    } finally {
      if (aliveRef.current && generationRef.current === gen) setBusy(false);
    }
  }, [active.id, attach, diagnose, invokeDesktopEmbedFind, processName]);

  const detach = useCallback(async () => {
    setBusy(true);
    setPhase("closing");
    // Native child paints above the webview — hide first so the closing
    // overlay is visible for the whole detach, same idea as kill.
    void enqueueShow(processName, () =>
      invokeDesktopEmbedShow(processName, false).catch(() => undefined)
    );
    try {
      await invokeDesktopEmbedDetach(processName);
      hostedRef.current.delete(processName.trim().toLowerCase());
      if (activeIdRef.current === active.id) {
        attachedRef.current = false;
        setPhase("running");
      }
    } catch {
      if (aliveRef.current && activeIdRef.current === active.id && attachedRef.current) {
        setPhase("attached");
        void enqueueShow(processName, () =>
          invokeDesktopEmbedShow(processName, true).catch(() => undefined)
        );
      }
    } finally {
      if (aliveRef.current && activeIdRef.current === active.id) setBusy(false);
    }
  }, [active.id, invokeDesktopEmbedDetach, invokeDesktopEmbedShow, processName]);

  /** Clear an app-side minimize/maximize, then re-place it in the slot now. */
  const restoreWindow = useCallback(async () => {
    const rect = slotRect();
    setBusy(true);
    try {
      await invokeDesktopEmbedRestore(processName);
      if (!rect) return;
      const result = await invokeDesktopEmbedRelayout({ processName, ...rect });
      if (!aliveRef.current || activeIdRef.current !== active.id) return;
      if (result.state === "lost") {
        attachedRef.current = false;
        setPhase("idle");
        return;
      }
      if (result.title) setWindowTitle(result.title);
      if (result.class) setWindowClass(result.class);
      setWarning(result.warning || "");
    } catch {
      if (aliveRef.current && activeIdRef.current === active.id) setWarning("restore-failed");
    } finally {
      if (aliveRef.current && activeIdRef.current === active.id) setBusy(false);
    }
  }, [
    active.id,
    invokeDesktopEmbedRelayout,
    invokeDesktopEmbedRestore,
    processName,
    slotRect,
  ]);

  /** The hosted window paints above the menu; hide it while the menu is up. */
  const setOverflowOpen = useCallback(
    (open: boolean) => {
      void setOverlay("menu", open);
    },
    [setOverlay]
  );

  const killApp = useCallback(async () => {
    // The hosted window is a native child placed above the webview, so it
    // would cover the confirm dialog: the dialog only "flashes" and its
    // buttons cannot be clicked. Hide it while the dialog is up, exactly like
    // the picker does, and bring it back if the user backs out.
    // Awaited: the hide lands before the dialog renders, so the native window
    // can never cover the confirm for even one frame.
    await setOverlay("confirm", true);
    const ok = await confirm({
      title: t("kill.title"),
      message: t("kill.message", { name: label(active) }),
      confirmText: t("kill.confirm"),
      icon: "danger",
    });
    if (!ok) {
      // Backed out: restore exactly per the overlay set (nothing else open).
      void setOverlay("confirm", false);
      return;
    }
    // Proceeding: the window must stay hidden for the whole kill; drop the
    // flag WITHOUT a visibility sync — the kill flow manages it from here.
    overlaysRef.current.confirm = false;
    setBusy(true);
    setPhase("closing");
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
          setPhase("attached");
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
      hostedRef.current.delete(processName.trim().toLowerCase());
      if (activeIdRef.current === active.id) {
        attachedRef.current = false;
        setPhase("idle");
      }
    } catch (e) {
      // The window is still embedded (its process survived); keep phase
      // attached so nothing floats over the workbench.
      if (attachedRef.current) {
        setPhase("attached");
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

  /**
   * Put an app in the bar and open it. Shared by all three library sources, so
   * "add" always means the same thing: one click, then the app is on screen.
   */
  const adoptApp = useCallback(
    (def: DesktopAppDef) => {
      const key = def.processName.trim().toLowerCase();
      const existing = apps.find((app) => app.processName.trim().toLowerCase() === key);
      const target = existing ?? def;
      /** What we knew about this app before the click — reused for the first frame. */
      const cached = phaseCacheRef.current.get(target.id);
      if (!existing) {
        const next = dedupeApps([...custom, def]);
        setCustom(next);
        saveCustomApps(next);
        phaseCacheRef.current.delete(target.id);
      }

      // The library closes without a visibility sync: a tab switch (or an
      // immediate start) follows, and the per-app effect manages visibility
      // for whichever process ends up hosted.
      overlaysRef.current.library = false;
      cancelOverlaySync();
      setLibraryOpen(false);
      setWarning("");
      setErrorText("");
      setCandidates([]);

      if (target.id === activeIdRef.current) {
        // Already the current tab: nothing switches, so open it right here.
        autoStartRef.current = null;
        // The library just hid this app's window (shown=0), and attach_wait
        // reuses an existing entry with its shown flag — without this the
        // slot would go permanently blank.
        if (!anyOverlayOpen()) {
          const restoreName = processOverrides[target.id] ?? target.processName;
          void enqueueShow(restoreName, () =>
            invokeDesktopEmbedShow(restoreName, true).catch(() => undefined)
          );
        }
        void startRef.current?.();
        return;
      }

      autoStartRef.current = target.id;
      phaseCacheRef.current.set(activeIdRef.current, phase);
      saveActiveId(target.id);
      setActiveId(target.id);
      setPhase(cached ?? "idle");
    },
    [anyOverlayOpen, apps, custom, invokeDesktopEmbedShow, phase, processOverrides]
  );

  /**
   * Pick an .exe. With `targetId` the path is remembered for that existing
   * entry ("指定程序" on a missing app) instead of adding a second tab.
   */
  const addAppFromFile = useCallback(
    async (targetId?: string) => {
      const path = await invoke<string>("browse_app_file").catch(() => null);
      if (!path) return;
      const stem = processNameFromExe(path);
      const target = targetId ? apps.find((app) => app.id === targetId) : undefined;
      const clashes = apps.some(
        (app) =>
          app.id !== target?.id && app.processName.trim().toLowerCase() === stem.toLowerCase()
      );

      if (target && !clashes) {
        saveExePath(target.id, path);
        if (target.custom) {
          const next = custom.map((app) =>
            app.id === target.id ? { ...app, exePath: path } : app
          );
          setCustom(next);
          saveCustomApps(next);
        }
        overlaysRef.current.library = false;
        cancelOverlaySync();
        setLibraryOpen(false);
        setPhase("idle");
        autoStartRef.current = null;
        void startRef.current?.();
        return;
      }

      adoptApp({
        id: appIdForProcess(stem),
        label: stem,
        processName: stem,
        matchNames: [stem.toLowerCase()],
        exePath: path,
        custom: true,
      });
    },
    [adoptApp, apps, custom]
  );

  const removeApp = useCallback(async () => {
    if (!active.custom) return;
    // Same native-window-covers-the-dialog problem as killApp: hide the
    // hosted window while the confirm is up (awaited, so the dialog never
    // renders behind the native window even for a frame).
    await setOverlay("confirm", true);
    const ok = await confirm({
      title: t("remove.title"),
      message: t("remove.message", { name: label(active) }),
      confirmText: t("remove.confirm"),
      icon: "danger",
    });
    if (!ok) {
      await setOverlay("confirm", false);
      return;
    }
    // Proceeding: the app gets detached below; no restore sync wanted.
    overlaysRef.current.confirm = false;
    try {
      await invokeDesktopEmbedDetach(processName);
    } catch {
      // Detach failed but the window may still be hosted and hidden behind
      // the confirm dialog: restore it, or it would float invisible forever.
      void enqueueShow(processName, () =>
        invokeDesktopEmbedShow(processName, true).catch(() => undefined)
      );
    }
    attachedRef.current = false;
    hostedRef.current.delete(processName.trim().toLowerCase());
    const next = custom.filter((app) => app.id !== active.id);
    setCustom(next);
    saveCustomApps(next);
    phaseCacheRef.current.delete(active.id);
    saveActiveId(BUILTIN_DESKTOP_APPS[0].id);
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
    // The resolve detour is the confusing beat of a first launch: name it,
    // instead of a bare "waiting…" that never explains what is happening.
    if (locating) return t("status.locating");
    switch (phase) {
      case "attached":
        return t("status.attached");
      case "running":
        return t("status.running");
      case "launching":
        return t("status.launching");
      case "polling":
        return t("status.polling");
      case "closing":
        return t("status.closing");
      case "missing":
        return t("status.missing");
      case "error":
        return t("status.error");
      default:
        return t("status.idle");
    }
  })();

  /**
   * Stop waiting for the window: the launched process keeps running in the
   * background (killing it here would be surprising), the in-flight
   * startAndAttach unwinds via its token, and the tab goes back to idle.
   */
  const cancelLaunch = useCallback(() => {
    cancelLaunchRef.current += 1;
    if (aliveRef.current && activeIdRef.current === active.id) {
      setPhase("idle");
      setErrorText("");
      setCandidates([]);
      setBusy(false);
      setLocating(false);
    }
  }, [active.id]);

  /**
   * The single action that matters right now. Everything diagnostic moved into
   * the ⋯ menu: "重新嵌入 / 手动选窗口 / 脱离窗口" were never everyday verbs.
   */
  const primaryAction: TbarAction | null = (() => {
    switch (phase) {
      case "attached":
        return null;
      // Waiting up to 150s with only a disabled spinner used to trap the user
      // (a tab switch was the only way out): launching/polling now offer the
      // everyday escape — cancel the wait, keep the app running.
      case "launching":
      case "polling":
        return {
          key: "cancel",
          label: t("actions.cancel"),
          icon: <XCircle size={13} />,
          variant: "secondary",
          disabled: false,
          title: t("actions.cancelHint"),
          onClick: cancelLaunch,
        };
      case "closing":
        return {
          key: "busy",
          label: statusText,
          icon: <Loader2 size={13} className="dapps-spin" />,
          variant: "secondary",
          disabled: true,
          onClick: () => undefined,
        };
      case "missing":
        return {
          key: "choose-exe",
          label: t("actions.chooseExe"),
          icon: <FolderSearch size={13} />,
          variant: "primary",
          disabled: false,
          onClick: () => void addAppFromFile(active.id),
        };
      case "running":
        return {
          key: "open",
          label: t("actions.open"),
          icon: <Play size={13} />,
          variant: "primary",
          disabled: busy,
          onClick: () => void reattach(),
        };
      // The usual reason an attach failed is a wrong/missing window pick, so
      // the manual picker — not "retry the same thing" — is the main path.
      case "error":
        return {
          key: "picker",
          label: t("picker.action"),
          icon: <MousePointerClick size={13} />,
          variant: "primary",
          disabled: busy,
          onClick: () => void openPicker(),
        };
      default:
        return {
          key: "start",
          label: t("actions.start"),
          icon: <Play size={13} />,
          variant: "primary",
          disabled: busy,
          onClick: () => void startAndAttach(),
        };
    }
  })();

  const menuActions: TbarAction[] = [
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
      label: t("actions.picker"),
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

  /**
   * Secondary pane overlay: a chooser when nothing is parked there, else the
   * parked app's status ("attach pending" / not running) with a swap shortcut.
   * Nothing renders while its window is actually attached.
   */
  const slotBPanel = (() => {
    const appB = apps.find((a) => a.id === slotBAppId) ?? null;
    if (appB && slotBPhase === "attached") return null;
    return (
      <div className="dapps-hint is-idle">
        {appB ? (
          <>
            <div className="dapps-idle-hero" aria-hidden="true">
              <div className="dapps-idle-icon">
                <AppTabIcon app={appB} installed={installed} />
              </div>
            </div>
            <div className="dapps-hint-body">
              <p className="dapps-hint-title">{label(appB)}</p>
              <p className="dapps-hint-text">
                {slotBBusy ? t("hint.starting") : t("slot.bNotRunning")}
              </p>
              <div className="dapps-hint-actions">
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={() => switchApp(appB.id)}
                >
                  {t("slot.makePrimary")}
                </button>
              </div>
            </div>
          </>
        ) : (
          <>
            <p className="dapps-hint-title">{t("slot.bEmpty")}</p>
            <div className="dapps-slot-b-choices">
              {apps
                .filter((a) => a.id !== active.id)
                .slice(0, 8)
                .map((a) => (
                  <button
                    key={a.id}
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => {
                      setSlotBId(a.id);
                      saveSlotBId(a.id);
                      setSlotBPhase("idle");
                    }}
                  >
                    <AppTabIcon app={a} installed={installed} />
                    {label(a)}
                  </button>
                ))}
            </div>
          </>
        )}
        {slotBId ? (
          <button
            type="button"
            className="dapps-slot-b-close"
            title={t("slot.bClear")}
            onClick={() => {
              setSlotBId("");
              saveSlotBId("");
              setSlotBPhase("idle");
              setSlotBBusy(false);
            }}
          >
            <XCircle size={13} />
          </button>
        ) : null}
      </div>
    );
  })();

  return (
    <div className="dapps-page">
      {/* One strip only: the hosted app's window paints over everything below,
          so every control has to live in the reserved top band. */}
      <div className="dapps-bar">
        <div className="dapps-tabs" role="tablist" aria-label={t("title")}>
          {apps.map((app) => {
            const key = app.processName.trim().toLowerCase();
            const attachedHere = app.id === active.id && phase === "attached";
            const hosted = attachedHere || hostedRef.current.has(key);
            const runningHere = running.byProcess.has(key);
            return (
              <button
                key={app.id}
                type="button"
                role="tab"
                aria-selected={app.id === active.id}
                aria-label={label(app)}
                title={runningHere ? t("tab.running", { name: label(app) }) : label(app)}
                className={`dapps-tab${app.id === active.id ? " is-active" : ""}${
                  dragTabId === app.id ? " is-dragging" : ""
                }`}
                draggable
                onDragStart={(event) => {
                  dragTabIdRef.current = app.id;
                  setDragTabId(app.id);
                  event.dataTransfer.effectAllowed = "move";
                  event.dataTransfer.setData("text/plain", app.id);
                }}
                onDragEnd={() => {
                  dragTabIdRef.current = null;
                  setDragTabId(null);
                }}
                onDragOver={(event) => {
                  // Live preview: moving the dragged tab onto this slot as
                  // soon as it hovers here (once per target change).
                  const dragged = dragTabIdRef.current;
                  if (!dragged || dragged === app.id) return;
                  event.preventDefault();
                  event.dataTransfer.dropEffect = "move";
                  reorderTabs(dragged, app.id);
                }}
                onDrop={(event) => {
                  event.preventDefault();
                  const dragged = dragTabIdRef.current;
                  dragTabIdRef.current = null;
                  setDragTabId(null);
                  if (dragged) reorderTabs(dragged, app.id);
                }}
                onClick={() => switchApp(app.id)}
              >
                <AppTabIcon app={app} installed={installed} />
                <span className="dapps-tab-label">{label(app)}</span>
                <span
                  className={`dapps-tab-dot${
                    hosted ? " is-attached" : runningHere ? " is-running" : ""
                  }`}
                />
              </button>
            );
          })}
          <button
            type="button"
            className="dapps-tab dapps-tab-add"
            title={t("library.title")}
            onClick={() => void openLibrary()}
          >
            <Plus size={14} />
            <span className="dapps-tab-label">{t("addApp")}</span>
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
          {busy &&
          (phase === "launching" || phase === "polling" || phase === "closing") ? (
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
            {warning === "minimized" ||
            warning === "maximized" ||
            warning === "size-mismatch" ||
            warning === "restore-failed" ? (
              <button
                type="button"
                className="dapps-warn-fix"
                disabled={busy}
                onClick={() => void restoreWindow()}
              >
                {t("warn.restore")}
              </button>
            ) : null}
            {/* The app undid our work (restored its own frame, hid the window,
                clamped its size): one click re-runs the whole takeover. */}
            {warning === "not-child" ||
            warning === "invisible" ||
            warning === "size" ||
            warning === "restore-failed" ? (
              <button
                type="button"
                className="dapps-warn-fix"
                disabled={busy}
                onClick={() => void reattach()}
              >
                {t("warn.reattach")}
              </button>
            ) : null}
          </span>
        ) : null}

        {storageWarn ? (
          <span className="dapps-storage-warn" role="status">
            <AlertTriangle size={12} />
            {t("storage.warn")}
          </span>
        ) : null}

        <span className="dapps-spacer" />

        <div className="dapps-actions">
          <button
            type="button"
            className={`btn btn-secondary btn-small dapps-split-btn${splitView ? " is-on" : ""}`}
            title={splitView ? t("slot.single") : t("slot.split")}
            aria-pressed={splitView}
            onClick={() => {
              const next = !splitView;
              setSplitView(next);
              saveSplitView(next);
            }}
          >
            <Columns2 size={13} />
          </button>
          {primaryAction ? (
            <button
              type="button"
              data-action={primaryAction.key}
              className={`btn btn-${primaryAction.variant} btn-small dapps-primary`}
              disabled={primaryAction.disabled}
              title={primaryAction.title ?? primaryAction.label}
              onClick={primaryAction.onClick}
            >
              {primaryAction.icon}
              <span className="dapps-primary-label">{primaryAction.label}</span>
            </button>
          ) : null}
          <DropdownMenu
            label={t("actions.more")}
            triggerClassName="btn btn-secondary btn-small"
            onOpenChange={setOverflowOpen}
            items={menuActions.map((a) => ({
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

      <div className="dapps-body">
        <div className={`dapps-slot-area${splitView ? " is-split" : ""}`}>
          <div className="dapps-slot-wrap">
            <div className="dapps-slot" ref={slotRef} />
            {phase !== "attached" && (
          phase === "launching" || phase === "polling" || phase === "closing" ? (
            <OrbitalLoadingStage
              variant={phase === "closing" ? "stopping" : "starting"}
              icon={<AppTabIcon app={active} installed={installed} />}
              title={label(active)}
              status={
                phase === "closing"
                  ? t("hint.closingLabel")
                  : phase === "launching"
                    ? t("hint.startingLabel")
                    : t("hint.waitingLabel")
              }
              hint={
                phase === "closing"
                  ? t("hint.closing")
                  : phase === "launching"
                    ? t("hint.starting")
                    : t("hint.waiting")
              }
            />
          ) : (
          <div className="dapps-hint is-idle">
            <div className="dapps-idle-hero" aria-hidden="true">
              <span className="dapps-idle-glow" />
              <div className="dapps-idle-icon">
                <AppTabIcon app={active} installed={installed} />
              </div>
            </div>
            <div className="dapps-hint-body">
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
              <div className="dapps-hint-actions">
                {primaryAction ? (
                  <button
                    type="button"
                    className={`btn btn-${primaryAction.variant} dapps-hint-primary`}
                    disabled={primaryAction.disabled}
                    onClick={primaryAction.onClick}
                  >
                    {primaryAction.icon}
                    {primaryAction.label}
                  </button>
                ) : null}
                {phase === "error" ? (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    disabled={busy}
                    onClick={() => void reattach()}
                  >
                    <RefreshCw size={12} />
                    {t("actions.open")}
                  </button>
                ) : null}
                <button
                  type="button"
                  className="dapps-hint-link"
                  onClick={() => void openLibrary()}
                >
                  {t("hint.pickAnother")}
                </button>
              </div>
              <p className="dapps-hint-note">{t("hint.note")}</p>
            </div>

            {/* Failure detail lives here: the app window paints over everything
                below the toolbar, so a toast would be unreachable anyway. */}
            {errorText ? (
              <div className="dapps-error">
                <p className="dapps-error-head">
                  <AlertTriangle size={13} />
                  {t("error.title")}
                </p>
                <p className="dapps-error-text">{t("error.plain")}</p>
                {/* The verbatim backend message (class names, handles, pids)
                    is for diagnosis, not for reading: kept one click away. */}
                <details className="dapps-error-details">
                  <summary>{t("error.details")}</summary>
                  <p className="dapps-error-text">{errorText}</p>
                </details>
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
                            disabled={busy}
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
          )
        )}
          </div>
          {splitView ? (
            <div className="dapps-slot-wrap">
              <div className="dapps-slot" ref={slotBRef} />
              {slotBPanel}
            </div>
          ) : null}
        </div>
      </div>

      {picking ? (
        <div className="modal-overlay" onClick={() => { if (!busy) closePicker(); }}>
          <div
            className="modal modal-lg dapps-picker"
            onClick={(event) => event.stopPropagation()}
            onKeyDown={onPickerKeyDown}
          >
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
                const { shown, visible, hidden } = pickerRows;
                const grouped = visible.length > 0 && hidden.length > 0;
                const row = (c: DesktopWindowCandidate, index: number) => {
                  const isHidden = !c.visible && !c.current;
                  return (
                    <li
                      key={`${c.pid}-${c.hwnd}`}
                      className={`dapps-picker-row${
                        c.current ? " is-current" : isHidden ? " is-hidden-row" : ""
                      }${pickerActive === index ? " is-active" : ""}`}
                      onMouseEnter={() => setPickerActive(index)}
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
                              {t("picker.area", { k: Math.round(c.area / 1000) })}
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
                      ref={pickerSearchRef}
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
                        <ul className="dapps-picker-list" ref={pickerListRef}>
                          {grouped ? (
                            <li className="dapps-picker-group">{t("picker.adoptable")}</li>
                          ) : null}
                          {visible.map((c, index) => row(c, index))}
                          {grouped ? (
                            <li className="dapps-picker-group">{t("picker.hiddenGroup")}</li>
                          ) : null}
                          {hidden.map((c, index) => row(c, visible.length + index))}
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

      {libraryOpen ? (
        <AppLibraryDialog
          apps={apps}
          installed={installedShortcuts}
          installedAll={installedAll}
          running={running.apps}
          runningLoading={running.loading}
          runningScanFailed={running.error}
          installedScanFailed={installedScanFailed}
          onRefreshRunning={() => void running.refresh()}
          onRescanInstalled={() => void rescanInstalled()}
          onPickInstalled={(info) => adoptApp(appFromInstalled(info))}
          onPickRunning={(item) => adoptApp(appFromRunningApp(item))}
          onPickFile={() => void addAppFromFile()}
          onSwitch={(app) => {
            // Picking the app we are already on still has to restore its window:
            // opening the library hid it.
            if (app.id === activeIdRef.current) {
              closeLibrary();
              return;
            }
            overlaysRef.current.library = false;
            cancelOverlaySync();
            setLibraryOpen(false);
            switchApp(app.id);
          }}
          onClose={closeLibrary}
        />
      ) : null}
    </div>
  );
}
