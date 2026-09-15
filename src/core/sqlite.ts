/**
 * Client-side storage entry point.
 *
 * NOTE: The actual database schema and connection live in the Rust backend
 * (`src-tauri/src/main.rs` `setup()` hook). The SQLite database file is created
 * at `%APPDATA%\com.ai-workbench.app\ai-workbench.db` and all entity data is read
 * and written through typed Tauri commands `db_load` / `db_save`.
 *
 * This function is kept as a clear hook for startup; the promise is memoized so
 * React StrictMode double-mount does not re-run init work.
 */
let initPromise: Promise<void> | null = null;

export async function initializeDatabase(): Promise<void> {
  if (!initPromise) {
    initPromise = Promise.resolve();
  }
  return initPromise;
}
