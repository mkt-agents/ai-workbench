/**
 * Registry for desktop apps hosted inside the workbench window.
 *
 * A desktop app has no embeddable URL (DeepSeek Harness desktop draws through
 * a private Chromium scheme), so the backend adopts the app's own
 * top-level window instead. Everything it needs to do that is here: the window's
 * image name, and a way to find the executable when the app is not running.
 */
import type { DesktopRunningApp, InstalledAppInfo } from "../core/types";

export interface DesktopAppDef {
  id: string;
  /** Built-ins translate through `desktopApps.providers.<key>`. */
  labelKey?: string;
  /** Custom entries carry their own display label. */
  label?: string;
  /** Window image name, without the `.exe` suffix. */
  processName: string;
  /** Display-name substrings used to locate the executable, lowercased. */
  matchNames: string[];
  /** Extra switches used when we have to launch the app ourselves. */
  args?: string[];
  /** Absolute executable path, when known. */
  exePath?: string;
  custom?: boolean;
}

/** Apps offered out of the box. An Electron shell with its own frame. */
export const BUILTIN_DESKTOP_APPS: DesktopAppDef[] = [
  {
    id: "dsh-desktop",
    labelKey: "providers.dsh",
    processName: "DeepSeek Harness",
    matchNames: ["deepseek harness"],
  },
];

/** localStorage key for the apps the user added by picking an .exe. */
const CUSTOM_APPS_KEY = "desktop-embed-apps";

/**
 * Notified once per failed write. Storage can be full or blocked (private
 * mode, policy); every save used to swallow that, so config silently vanished
 * between restarts. The page registers a listener and says it out loud.
 */
type StorageFailureListener = () => void;
let storageFailureListener: StorageFailureListener | null = null;

export function onStorageFailure(listener: StorageFailureListener | null): void {
  storageFailureListener = listener;
}

function notifyStorageFailure(): void {
  try {
    storageFailureListener?.();
  } catch {
    /* a broken listener must not break the save path */
  }
}

/** SetItem that reports failure instead of pretending it did not happen. */
function writeStorage(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch (err) {
    console.error("desktopApps: storage write failed", key, err);
    notifyStorageFailure();
  }
}

export function loadCustomApps(): DesktopAppDef[] {
  try {
    const raw = localStorage.getItem(CUSTOM_APPS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (item): item is DesktopAppDef =>
          !!item &&
          typeof item === "object" &&
          typeof (item as DesktopAppDef).processName === "string" &&
          typeof (item as DesktopAppDef).id === "string"
      )
      .map((item) => ({ ...item, custom: true }));
  } catch {
    return [];
  }
}

export function saveCustomApps(apps: DesktopAppDef[]): void {
  writeStorage(CUSTOM_APPS_KEY, JSON.stringify(apps.filter((a) => a.custom)));
}

/** `D:\apps\Foo Bar\foo.exe` → `foo`. The image name is what tasklist shows. */
export function processNameFromExe(exePath: string): string {
  const file = exePath.split(/[\\/]/).pop() ?? exePath;
  return file.replace(/\.exe$/i, "");
}

/** Executable for `app`: an explicit path wins, else the first scan hit. */
export function matchExe(app: DesktopAppDef, installed: InstalledAppInfo[]): string | null {
  // A path the user pointed at explicitly beats both the entry and the scan:
  // this is what makes "choose program" work for a built-in app whose
  // executable lives somewhere the scanner did not look.
  const override = loadExePath(app.id);
  if (override) return override;
  if (app.exePath) return app.exePath;
  const needles = app.matchNames.length > 0 ? app.matchNames : [app.processName.toLowerCase()];
  const hit = installed.find((candidate) => {
    const display = candidate.display_name.toLowerCase();
    return needles.some((needle) => needle.length > 0 && display.includes(needle));
  });
  return hit?.exe_path ?? null;
}

/** localStorage key for "I chose this .exe for this app" overrides. */
const EXE_PATHS_KEY = "desktop-embed-exe-paths";

/** In-memory mirror: `matchExe` runs on every tab render. */
let exePathsCache: Record<string, string> | null = null;

function readExePaths(): Record<string, string> {
  if (exePathsCache) return exePathsCache;
  try {
    const raw = localStorage.getItem(EXE_PATHS_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    exePathsCache =
      parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? (parsed as Record<string, string>)
        : {};
  } catch {
    exePathsCache = {};
  }
  return exePathsCache;
}

export function loadExePath(id: string): string | null {
  const path = readExePaths()[id];
  return typeof path === "string" && path.trim() ? path : null;
}

export function saveExePath(id: string, path: string): void {
  const next = { ...readExePaths(), [id]: path };
  exePathsCache = next;
  writeStorage(EXE_PATHS_KEY, JSON.stringify(next));
}

/**
 * localStorage key for "this app's window is owned by *that* image name".
 *
 * The process name an app starts with is derived from its `.exe` file name, but
 * launchers and repackaged Electron builds frequently hand the window to a
 * differently named binary (`App.exe` → `App-bin.exe`). Resolving the real name
 * once and remembering it here is what makes "start" find the window next time.
 */
const PROCESS_NAMES_KEY = "desktop-embed-process-names";

let processNamesCache: Record<string, string> | null = null;

export function loadProcessNames(): Record<string, string> {
  if (processNamesCache) return processNamesCache;
  try {
    const raw = localStorage.getItem(PROCESS_NAMES_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    processNamesCache =
      parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? (parsed as Record<string, string>)
        : {};
  } catch {
    processNamesCache = {};
  }
  return processNamesCache;
}

export function saveProcessName(id: string, processName: string): void {
  const next = { ...loadProcessNames(), [id]: processName };
  processNamesCache = next;
  writeStorage(PROCESS_NAMES_KEY, JSON.stringify(next));
}

/** localStorage key for the app the page last had selected. */
const ACTIVE_APP_KEY = "desktop-embed-active";

/**
 * The tab to land on. The page used to always open on the first built-in app,
 * which is why switching to anything else meant one click every visit.
 */
export function loadActiveId(fallback: string): string {
  try {
    const raw = localStorage.getItem(ACTIVE_APP_KEY);
    return raw && raw.trim() ? raw : fallback;
  } catch {
    return fallback;
  }
}

export function saveActiveId(id: string): void {
  writeStorage(ACTIVE_APP_KEY, id);
}

/** localStorage key for the slot layout: single pane or side-by-side. */
const SPLIT_VIEW_KEY = "desktop-embed-split";

/** localStorage key for the tab strip order (list of app ids, best effort). */
const TAB_ORDER_KEY = "desktop-embed-tab-order";

/** The order the user dragged the tabs into. Unknown ids just sort last. */
export function loadTabOrder(): string[] {
  try {
    const raw = localStorage.getItem(TAB_ORDER_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === "string") : [];
  } catch {
    return [];
  }
}

export function saveTabOrder(ids: string[]): void {
  writeStorage(TAB_ORDER_KEY, JSON.stringify(ids));
}

export function loadSplitView(): boolean {
  try {
    return localStorage.getItem(SPLIT_VIEW_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveSplitView(split: boolean): void {
  writeStorage(SPLIT_VIEW_KEY, split ? "1" : "0");
}

/** localStorage key for the app parked in the secondary (side-by-side) slot. */
const SLOT_B_KEY = "desktop-embed-slot-b";

export function loadSlotBId(): string {
  try {
    return localStorage.getItem(SLOT_B_KEY) ?? "";
  } catch {
    return "";
  }
}

export function saveSlotBId(id: string): void {
  writeStorage(SLOT_B_KEY, id);
}

/**
 * Stable id for an app derived from its process name, so the same app added
 * twice (from the installed list, then from the running list) is one entry.
 */
export function appIdForProcess(processName: string): string {
  const slug = processName
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return `proc-${slug || "app"}`;
}

/** True when `processName` is already one of `apps` (case-insensitive). */
export function hasProcess(apps: DesktopAppDef[], processName: string): boolean {
  const needle = processName.trim().toLowerCase();
  return apps.some((app) => app.processName.trim().toLowerCase() === needle);
}

/** Drop duplicates by process name, keeping the first (built-ins stay first). */
export function dedupeApps(apps: DesktopAppDef[]): DesktopAppDef[] {
  const seen = new Set<string>();
  return apps.filter((app) => {
    const key = app.processName.trim().toLowerCase();
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** An app entry for something the user picked out of an installed-app scan. */
export function appFromInstalled(info: InstalledAppInfo): DesktopAppDef {
  const processName = processNameFromExe(info.exe_path);
  return {
    id: appIdForProcess(processName),
    label: info.display_name || processName,
    processName,
    matchNames: [info.display_name.toLowerCase(), processName.toLowerCase()].filter(Boolean),
    exePath: info.exe_path,
    custom: true,
  };
}

/**
 * An app entry for a window that is running right now. The window title is
 * a document name ("notes - Cursor"), so it is used only as a fallback label —
 * the process name is what the user recognises in a list of apps.
 */
export function appFromRunningApp(running: DesktopRunningApp): DesktopAppDef {
  const processName = running.processName.trim();
  return {
    id: appIdForProcess(processName),
    label: processName,
    processName,
    matchNames: [processName.toLowerCase()],
    exePath: running.exePath ?? undefined,
    custom: true,
  };
}
