import { initializeDatabase } from "./sqlite";
import { useGlobalStore } from "./store";
import {
  adoptDbSettings,
  loadSettingsFromDb,
  readLocalSettings,
  seedSettingsToDb,
} from "../lib/settingsDb";

let bootPromise: Promise<void> | null = null;

/**
 * localStorage 是设置的主存，SQLite 里的 `app_settings` 只是副本：
 * - 本地整份丢失（重装 / 清过 WebView2 缓存）→ 用库里的那份回填，再让 store 重新水合；
 * - 本地还在（正常启动）→ 只把库里缺的键补进去，绝不覆盖本地已改的值。
 *
 * 这样多窗口同时启动也不会互相把对方刚改的设置盖回旧值。
 * 这一步失败只是退回「只有 localStorage」的老行为，不拦启动。
 */
async function reconcileSettingsWithDb(): Promise<void> {
  try {
    const db = await loadSettingsFromDb();
    if (Object.keys(db).length === 0) {
      // 首次升级：把本地现有设置整份灌进库
      await seedSettingsToDb(readLocalSettings());
      return;
    }
    const local = readLocalSettings();
    if (Object.keys(local).length === 0) {
      adoptDbSettings(db);
      await useGlobalStore.persist.rehydrate();
      return;
    }
    // 库里缺的键补上（新版本新增的设置项、或之前落库失败漏掉的）
    const missing: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(local)) {
      if (!(key in db)) missing[key] = value;
    }
    if (Object.keys(missing).length > 0) await seedSettingsToDb(missing);
  } catch {
    /* 设置副本不可用：照常启动 */
  }
}

/**
 * StrictMode-safe one-shot boot: ensure the SQLite database is initialized and
 * hydrate the global store. Memoized so React StrictMode double-mount and
 * multiple windows (main + ai-hub) do not re-run init work or race on the
 * shared Zustand persist layer.
 */
export function bootApp(): Promise<void> {
  if (!bootPromise) {
    bootPromise = initializeDatabase()
      .then(() => reconcileSettingsWithDb())
      .then(() => useGlobalStore.getState().initialize());
  }
  return bootPromise;
}

export default bootApp;
