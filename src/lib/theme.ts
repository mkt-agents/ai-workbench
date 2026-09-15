import type { AppTheme } from "../core/types";

/** Apply theme to the current document (each Tauri webview has its own). */
export function applyDocumentTheme(theme: AppTheme): void {
  if (theme === "system") {
    document.documentElement.removeAttribute("data-theme");
  } else {
    document.documentElement.setAttribute("data-theme", theme);
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
    if (
      theme === "light" ||
      theme === "dark" ||
      theme === "system" ||
      theme === "glass" ||
      theme === "ice" ||
      theme === "silver"
    ) {
      return theme;
    }
  } catch {
    /* ignore */
  }
  return null;
}

export const APP_THEME_CHANGED_EVENT = "app-theme-changed";
