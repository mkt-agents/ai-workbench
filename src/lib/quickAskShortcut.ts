/**
 * Register / re-register the global quick-ask shortcut.
 */
import { register, unregister, isRegistered } from "@tauri-apps/plugin-global-shortcut";
import { invoke } from "@tauri-apps/api/core";

const DEFAULT_SHORTCUT = "Ctrl+Alt+K";

let currentShortcut: string | null = null;

export function normalizeShortcut(raw?: string | null): string {
  const s = (raw || DEFAULT_SHORTCUT).trim();
  return s || DEFAULT_SHORTCUT;
}

async function tryUnregister(shortcut: string): Promise<void> {
  try {
    await unregister(shortcut);
  } catch {
    /* ignore — may not be registered */
  }
}

function friendlyRegisterError(err: unknown): string {
  const raw = String(err ?? "").trim() || "unknown";
  if (/already registered/i.test(raw)) {
    return "快捷键已被占用，请换一组或重启应用后再试";
  }
  return raw;
}

async function bindShortcut(shortcut: string): Promise<void> {
  await register(shortcut, async (event) => {
    if (event.state !== "Pressed") return;
    try {
      await invoke("tray_toggle_quick_ask");
    } catch (e) {
      console.error("toggle quick-ask failed", e);
    }
  });
}

export async function registerQuickAskShortcut(
  shortcut?: string | null
): Promise<{ ok: boolean; error?: string }> {
  const next = normalizeShortcut(shortcut);

  // Same shortcut already owned by this app — treat as success.
  if (currentShortcut === next) {
    try {
      if (await isRegistered(next)) {
        return { ok: true };
      }
    } catch {
      /* fall through and re-bind */
    }
  }

  try {
    if (currentShortcut && currentShortcut !== next) {
      await tryUnregister(currentShortcut);
    }
    await tryUnregister(next);

    try {
      await bindShortcut(next);
    } catch (e) {
      if (!/already registered/i.test(String(e))) throw e;
      await tryUnregister(next);
      await bindShortcut(next);
    }

    currentShortcut = next;
    return { ok: true };
  } catch (e) {
    return { ok: false, error: friendlyRegisterError(e) };
  }
}

export async function unregisterQuickAskShortcut(): Promise<void> {
  if (!currentShortcut) return;
  await tryUnregister(currentShortcut);
  currentShortcut = null;
}

export { DEFAULT_SHORTCUT };
