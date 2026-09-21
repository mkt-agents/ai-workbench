/**
 * Global shortcuts for the 网页工具 bookmarks.
 *
 * This lives outside the page component on purpose: `PluginBrowser` is rendered
 * conditionally by `App`, so registering here (and never unregistering on unmount) is what
 * makes a configured shortcut keep working after the user switches to another page.
 */
import { register, unregister, isRegistered } from "@tauri-apps/plugin-global-shortcut";
import { openBrowser } from "./browser";
import { normalizeHttpUrl } from "./webTools";
import { useGlobalStore } from "../core/store";
import type { WebPlugin } from "../core/types";

/** shortcut → the plugin whose hotkey we currently own */
const owned = new Map<string, string>();

async function tryUnregister(shortcut: string): Promise<void> {
  try {
    await unregister(shortcut);
  } catch {
    /* not registered by us — nothing to release */
  }
}

async function openOnHotkey(id: string, shortcut: string): Promise<void> {
  const plugin = useGlobalStore.getState().webPlugins.find((p) => p.id === id);
  if (!plugin) {
    // Deleted while registered; release rather than keep a dead binding.
    await tryUnregister(shortcut);
    owned.delete(shortcut);
    return;
  }
  const parsed = normalizeHttpUrl(plugin.url);
  if (!parsed) return;
  try {
    // No explicit key: `browserMapKey(url)` is the same identity the list page uses for
    // its "已打开" badge, so a hotkey press focuses the popup instead of spawning one.
    await openBrowser(parsed.href, 900, 700);
    void useGlobalStore.getState().recordPluginOpen(id);
  } catch (e) {
    console.error("打开书签窗口失败:", e);
  }
}

/**
 * Make the OS-level bindings match the stored plugins. Returns the entries that could not
 * be bound (`Name (Shortcut)`), so the caller can say so instead of failing silently.
 */
export async function syncPluginHotkeys(plugins: WebPlugin[]): Promise<string[]> {
  const wanted = new Map<string, WebPlugin>();
  const conflicts: string[] = [];
  for (const plugin of plugins) {
    const shortcut = plugin.hotkey.trim();
    if (!shortcut) continue;
    if (wanted.has(shortcut)) {
      conflicts.push(`${plugin.name} (${shortcut})`);
      continue;
    }
    wanted.set(shortcut, plugin);
  }

  for (const [shortcut, id] of [...owned]) {
    if (wanted.get(shortcut)?.id !== id) {
      await tryUnregister(shortcut);
      owned.delete(shortcut);
    }
  }

  const failed: string[] = conflicts;
  for (const [shortcut, plugin] of wanted) {
    if (owned.get(shortcut) === plugin.id) continue;
    try {
      // Another app (or an earlier binding of ours) may hold it; take it over.
      if (await isRegistered(shortcut)) await tryUnregister(shortcut);
      await register(shortcut, (event) => {
        if (event.state !== "Pressed") return;
        void openOnHotkey(plugin.id, shortcut);
      });
      owned.set(shortcut, plugin.id);
    } catch {
      failed.push(`${plugin.name} (${shortcut})`);
    }
  }
  return failed;
}
