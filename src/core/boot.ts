import { initializeDatabase } from "./sqlite";
import { useGlobalStore } from "./store";

let bootPromise: Promise<void> | null = null;

/**
 * StrictMode-safe one-shot boot: ensure the SQLite database is initialized and
 * hydrate the global store. Memoized so React StrictMode double-mount and
 * multiple windows (main + quick-ask) do not re-run init work or race on the
 * shared Zustand persist layer.
 */
export function bootApp(): Promise<void> {
  if (!bootPromise) {
    bootPromise = initializeDatabase().then(() =>
      useGlobalStore.getState().initialize()
    );
  }
  return bootPromise;
}

export default bootApp;
