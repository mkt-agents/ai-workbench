import type { Theme } from "@tauri-apps/api/window";
import type { AppTheme } from "../core/types";

const VALID_THEMES: ReadonlySet<string> = new Set([
  "light",
  "dark",
  "system",
  "glass",
  "ice",
  "silver",
]);

/** Coerce removed / unknown theme ids (e.g. old localStorage) back to default. */
export function normalizeAppTheme(theme: string | null | undefined): AppTheme {
  return theme && VALID_THEMES.has(theme) ? (theme as AppTheme) : "glass";
}

/**
 * Map app themes to the native window chrome theme (Windows title bar only
 * knows light/dark). `null` = follow OS. glass / ice / silver → dark.
 */
export function toNativeTheme(theme: AppTheme): Theme | null {
  const next = normalizeAppTheme(theme);
  if (next === "system") return null;
  if (next === "light") return "light";
  return "dark";
}

/** Sync the current window's native title-bar theme (no-op outside Tauri). */
export async function syncNativeWindowTheme(theme: AppTheme): Promise<void> {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setTheme(toNativeTheme(theme));
  } catch {
    /* browser / permission / unsupported */
  }
}

/** Apply theme to the current document (each Tauri webview has its own). */
export function applyDocumentTheme(theme: AppTheme): void {
  const next = normalizeAppTheme(theme);
  if (next === "system") {
    document.documentElement.removeAttribute("data-theme");
  } else {
    document.documentElement.setAttribute("data-theme", next);
  }
  void syncNativeWindowTheme(next);
}

const SETTINGS_STORAGE_KEY = "workbench-settings";

/** Read persisted theme from localStorage (cross-window sync for AI Hub). */
export function readPersistedTheme(): AppTheme | null {
  try {
    const raw = localStorage.getItem(SETTINGS_STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as { state?: { settings?: { theme?: string } } };
    const theme = parsed?.state?.settings?.theme;
    if (theme && VALID_THEMES.has(theme)) return theme as AppTheme;
  } catch {
    /* ignore */
  }
  return null;
}

export const APP_THEME_CHANGED_EVENT = "app-theme-changed";
