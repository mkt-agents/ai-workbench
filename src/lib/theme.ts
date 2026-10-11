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

/** Apply theme to the current document (each Tauri webview has its own). */
export function applyDocumentTheme(theme: AppTheme): void {
  const next = normalizeAppTheme(theme);
  if (next === "system") {
    document.documentElement.removeAttribute("data-theme");
  } else {
    document.documentElement.setAttribute("data-theme", next);
  }
}

const SETTINGS_STORAGE_KEY = "workbench-settings";

/** Read persisted theme from localStorage (cross-window sync for Quick Ask). */
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
