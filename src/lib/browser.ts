import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useGlobalStore } from "../core/store";
import type { AppTheme } from "../core/types";
import { matchUrlPattern } from "./webTools";
import {
  APP_THEME_CHANGED_EVENT,
  normalizeAppTheme,
  readPersistedTheme,
  toNativeTheme,
} from "./theme";

/** logical key → WebviewWindow (survives for focus/close in this session) */
const windows = new Map<string, WebviewWindow>();
let defaultWindow: WebviewWindow | null = null;
let browserThemeSyncStarted = false;

function currentNativeTheme() {
  return toNativeTheme(normalizeAppTheme(readPersistedTheme() ?? "glass"));
}

/** Keep open tool windows' title bars in sync when Settings changes theme. */
function ensureBrowserThemeSync() {
  if (browserThemeSyncStarted) return;
  browserThemeSyncStarted = true;
  void listen<AppTheme>(APP_THEME_CHANGED_EVENT, (event) => {
    const native = toNativeTheme(normalizeAppTheme(event.payload));
    for (const win of windows.values()) {
      void win.setTheme(native).catch(() => {
        /* window may have closed */
      });
    }
  });
}

type ClosedListener = (key: string) => void;
const closedListeners = new Set<ClosedListener>();

/** Get enabled userscripts that match the given URL */
function getMatchingUserscripts(url: string): Array<{ name: string; code: string; patterns: string[] }> {
  try {
    const state = useGlobalStore.getState();
    const scripts =
      state.userScripts?.filter((s) => s.enabled && s.matchPatterns.some((p) => matchUrlPattern(p, url))) ?? [];
    return scripts.map((s) => ({ name: s.name, code: s.code, patterns: s.matchPatterns }));
  } catch {
    return [];
  }
}

export function onBrowserClosed(cb: ClosedListener): () => void {
  closedListeners.add(cb);
  return () => {
    closedListeners.delete(cb);
  };
}

function notifyClosed(key: string) {
  closedListeners.forEach((cb) => {
    try {
      cb(key);
    } catch {
      /* ignore */
    }
  });
}

function pathBasename(pathname: string): string {
  const parts = decodeURIComponent(pathname || "")
    .replace(/\\/g, "/")
    .split("/")
    .filter(Boolean);
  const base = parts[parts.length - 1] || "";
  return base.replace(/\.[^.]+$/, "") || base || "file";
}

function titleFromUrl(url: string): string {
  try {
    const u = new URL(url);
    if (u.hostname) return u.hostname;
    if (u.protocol === "file:") return pathBasename(u.pathname);
    return "Browser";
  } catch {
    return "Browser";
  }
}

/** Stable map key for a URL (used by UI activeKeys). */
export function browserMapKey(url: string): string {
  try {
    const u = new URL(url);
    // file:// has empty host; keep pathname so D:/a and D:/b stay distinct.
    const path = u.pathname.replace(/\/+$/, "") || "/";
    // Canonical drive letter for Windows file URLs.
    const canonPath =
      u.protocol === "file:"
        ? path.replace(/^\/([a-z]):/i, (_, d: string) => `/${d.toUpperCase()}:`)
        : path;
    return `${u.protocol}//${u.host}${canonPath}${u.search}${u.hash}`;
  } catch {
    return url;
  }
}

/**
 * Tauri window labels only allow `a-zA-Z0-9-/:_`.
 * Hostnames contain `.` so we must hash the logical key.
 */
function toWindowLabel(mapKey: string): string {
  let hash = 2166136261;
  for (let i = 0; i < mapKey.length; i++) {
    hash ^= mapKey.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  const hex = (hash >>> 0).toString(16).padStart(8, "0");
  let host = "site";
  try {
    const raw = mapKey.includes("://") ? mapKey : `https://${mapKey}`;
    const u = new URL(raw);
    const labelSrc = u.hostname || (u.protocol === "file:" ? pathBasename(u.pathname) : "site");
    host = labelSrc.replace(/[^a-zA-Z0-9-]/g, "-").replace(/-+/g, "-").slice(0, 32) || "site";
  } catch {
    /* keep default */
  }
  return `browser-${host}-${hex}`;
}

async function focusWindow(win: WebviewWindow | null) {
  if (!win) return;
  try {
    await win.unminimize();
  } catch {
    /* ignore */
  }
  try {
    await win.show();
  } catch {
    /* ignore */
  }
  try {
    await win.setFocus();
  } catch {
    /* ignore */
  }
}

export async function focusBrowser(): Promise<void> {
  const win = defaultWindow ?? (await WebviewWindow.getByLabel("browser"));
  defaultWindow = win;
  await focusWindow(win);
}

async function createBrowserWindow(url: string, width: number, height: number, mapKey: string) {
  const label = toWindowLabel(mapKey);

  // Recover after main-window reload: label may still exist
  const preexisting = await WebviewWindow.getByLabel(label);
  if (preexisting) {
    ensureBrowserThemeSync();
    windows.set(mapKey, preexisting);
    preexisting.once("tauri://destroyed", () => {
      windows.delete(mapKey);
      if (defaultWindow === preexisting) defaultWindow = null;
      notifyClosed(mapKey);
    });
    void preexisting.setTheme(currentNativeTheme()).catch(() => {
      /* ignore */
    });
    await focusWindow(preexisting);
    return preexisting;
  }

  // Delegate window creation to Rust so the webview is built with an
  // initialization_script — that script re-runs on every page navigation,
  // which keeps the floating nav toolbar alive across in-page links. A plain
  // `new WebviewWindow()` + `eval()` would lose the toolbar on the next load.
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    parsed = new URL("about:blank");
  }
  const userscripts = getMatchingUserscripts(parsed.href);
  ensureBrowserThemeSync();
  await invoke<string>("open_browser_window_with_toolbar", {
    url,
    width,
    height,
    label,
    userscripts,
    theme: currentNativeTheme(),
  });

  const win = await WebviewWindow.getByLabel(label);
  if (!win) {
    throw new Error("窗口创建后无法获取实例");
  }

  // Builder theme covers create; set again in case the window was reused
  // before the theme arg existed, or the OS ignored the builder hint.
  void win.setTheme(currentNativeTheme()).catch(() => {
    /* ignore */
  });

  windows.set(mapKey, win);
  win.once("tauri://destroyed", () => {
    windows.delete(mapKey);
    if (defaultWindow === win) defaultWindow = null;
    notifyClosed(mapKey);
  });
  await focusWindow(win);
  return win;
}

/** Open a URL in its own window, or focus the existing one. */
async function showUrl(url: string, width = 900, height = 700, key?: string) {
  const mapKey = key || browserMapKey(url);

  const existing = windows.get(mapKey);
  if (existing) {
    await focusWindow(existing);
    return existing;
  }

  // Try existing label from a previous session/reload
  const label = toWindowLabel(mapKey);
  const byLabel = await WebviewWindow.getByLabel(label);
  if (byLabel) {
    windows.set(mapKey, byLabel);
    byLabel.once("tauri://destroyed", () => {
      windows.delete(mapKey);
      if (defaultWindow === byLabel) defaultWindow = null;
      notifyClosed(mapKey);
    });
    await focusWindow(byLabel);
    return byLabel;
  }

  // Legacy single "browser" window (only when caller didn't pass a key)
  if (!key) {
    try {
      const navigated = await invoke<boolean>("navigate_browser_window", { url });
      if (navigated) {
        const win = await WebviewWindow.getByLabel("browser");
        defaultWindow = win;
        try {
          await win?.setTitle(titleFromUrl(url));
        } catch {
          /* ignore */
        }
        await focusWindow(win);
        return win;
      }
    } catch (e) {
      console.warn("navigate_browser_window failed:", e);
    }

    const legacy = await WebviewWindow.getByLabel("browser");
    if (legacy) {
      try {
        await legacy.close();
      } catch {
        /* ignore */
      }
      defaultWindow = null;
    }
  }

  return createBrowserWindow(url, width, height, mapKey);
}

/**
 * Open a site URL. http(s) → in-app WebView2 popup.
 * Local `file://` → system browser: WebView2's `file://` origin is `null`, so
 * pages that `fetch` remote APIs (like local SQL tools) fail CORS there even
 * though the same file works when opened in Chrome/Edge.
 * Returns `null` when the system browser was used.
 */
export async function openBrowser(
  url: string,
  width: number = 900,
  height: number = 700,
  key?: string
): Promise<WebviewWindow | null> {
  try {
    let protocol = "";
    try {
      protocol = new URL(url).protocol;
    } catch {
      /* fall through to webview path */
    }
    if (protocol === "file:") {
      await invoke("open_in_browser", { url });
      return null;
    }
    return await showUrl(url, width, height, key);
  } catch (e) {
    console.error("Failed to open browser window:", e);
    throw e;
  }
}

export async function closeBrowser(key?: string): Promise<void> {
  if (key) {
    const win = windows.get(key);
    if (win) {
      try {
        await win.close();
      } catch {
        /* ignore */
      }
      windows.delete(key);
      notifyClosed(key);
    }
    return;
  }

  const keys = [...windows.keys()];
  for (const k of keys) {
    const win = windows.get(k);
    if (!win) continue;
    try {
      await win.close();
    } catch {
      /* ignore */
    }
    windows.delete(k);
    notifyClosed(k);
  }

  const legacy = defaultWindow ?? (await WebviewWindow.getByLabel("browser"));
  if (legacy) {
    try {
      await legacy.close();
    } catch {
      /* ignore */
    }
  }
  defaultWindow = null;
}

export async function isBrowserOpen(key?: string): Promise<boolean> {
  if (key) {
    return windows.has(key);
  }
  const existing = defaultWindow ?? (await WebviewWindow.getByLabel("browser"));
  return Boolean(existing);
}

/** Get all open browser window map keys (current session). */
export function getOpenBrowserKeys(): string[] {
  return Array.from(windows.keys());
}
