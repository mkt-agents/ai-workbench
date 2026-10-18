/**
 * Registry for desktop apps hosted inside the workbench window.
 *
 * A desktop app has no embeddable URL (DeepSeek Harness desktop and Cursor both
 * draw through private Chromium schemes), so the backend adopts the app's own
 * top-level window instead. Everything it needs to do that is here: the window's
 * image name, and a way to find the executable when the app is not running.
 */
import type { InstalledAppInfo } from "../core/types";

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

/** Apps offered out of the box. Both are Electron shells with their own frame. */
export const BUILTIN_DESKTOP_APPS: DesktopAppDef[] = [
  {
    id: "dsh-desktop",
    labelKey: "providers.dsh",
    processName: "DeepSeek Harness",
    matchNames: ["deepseek harness"],
  },
  {
    id: "cursor",
    labelKey: "providers.cursor",
    processName: "Cursor",
    matchNames: ["cursor"],
  },
];

/** localStorage key for the apps the user added by picking an .exe. */
const CUSTOM_APPS_KEY = "desktop-embed-apps";

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
  try {
    localStorage.setItem(CUSTOM_APPS_KEY, JSON.stringify(apps.filter((a) => a.custom)));
  } catch {
    /* storage full or blocked — the list simply does not persist */
  }
}

/** `D:\apps\Foo Bar\foo.exe` → `foo`. The image name is what tasklist shows. */
export function processNameFromExe(exePath: string): string {
  const file = exePath.split(/[\\/]/).pop() ?? exePath;
  return file.replace(/\.exe$/i, "");
}

/** Executable for `app`: an explicit path wins, else the first scan hit. */
export function matchExe(app: DesktopAppDef, installed: InstalledAppInfo[]): string | null {
  if (app.exePath) return app.exePath;
  const needles = app.matchNames.length > 0 ? app.matchNames : [app.processName.toLowerCase()];
  const hit = installed.find((candidate) => {
    const display = candidate.display_name.toLowerCase();
    return needles.some((needle) => needle.length > 0 && display.includes(needle));
  });
  return hit?.exe_path ?? null;
}
