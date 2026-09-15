import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { invoke } from "@tauri-apps/api/core";

/** logical key → WebviewWindow (survives for focus/close in this session) */
const windows = new Map<string, WebviewWindow>();
let defaultWindow: WebviewWindow | null = null;

type ClosedListener = (key: string) => void;
const closedListeners = new Set<ClosedListener>();

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

function titleFromUrl(url: string): string {
  try {
    return new URL(url).hostname || "Browser";
  } catch {
    return "Browser";
  }
}

/** Stable map key for a URL (used by UI activeKeys). */
export function browserMapKey(url: string): string {
  try {
    const u = new URL(url);
    const path = u.pathname.replace(/\/+$/, "") || "/";
    return `${u.protocol}//${u.host}${path}${u.search}${u.hash}`;
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
    host = new URL(raw).hostname.replace(/[^a-zA-Z0-9-]/g, "-").replace(/-+/g, "-").slice(0, 32) || "site";
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

function waitForWindowReady(win: WebviewWindow): Promise<void> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = window.setTimeout(() => {
      if (settled) return;
      settled = true;
      reject(new Error("创建窗口超时"));
    }, 15000);

    const done = (err?: unknown) => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timer);
      if (err !== undefined) reject(err instanceof Error ? err : new Error(String(err)));
      else resolve();
    };

    win.once("tauri://created", () => done());
    win.once("tauri://error", (event) => {
      const payload = (event as { payload?: unknown }).payload;
      done(payload ?? "创建窗口失败");
    });
  });
}

async function createBrowserWindow(url: string, width: number, height: number, mapKey: string) {
  const label = toWindowLabel(mapKey);

  // Recover after main-window reload: label may still exist
  const preexisting = await WebviewWindow.getByLabel(label);
  if (preexisting) {
    windows.set(mapKey, preexisting);
    preexisting.once("tauri://destroyed", () => {
      windows.delete(mapKey);
      if (defaultWindow === preexisting) defaultWindow = null;
      notifyClosed(mapKey);
    });
    await focusWindow(preexisting);
    return preexisting;
  }

  const win = new WebviewWindow(label, {
    url,
    width,
    height,
    minWidth: 640,
    minHeight: 480,
    title: titleFromUrl(url),
    resizable: true,
    decorations: true,
    focus: true,
  });

  try {
    await waitForWindowReady(win);
  } catch (e) {
    try {
      await win.close();
    } catch {
      /* ignore */
    }
    throw e;
  }

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

export async function openBrowser(url: string, width: number = 900, height: number = 700, key?: string) {
  try {
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
