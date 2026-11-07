/**
 * Shared exe-icon cache.
 *
 * `scan_installed_apps` / `scan_desktop_apps` do not carry icons (`icon: None`),
 * so every icon shown in the app library and the tab strip comes from
 * `extract_app_icon` — a native GDI round trip per executable. Two things keep
 * a 120-tile grid from firing 120 IPC calls at once:
 *
 *  - a module-level cache, shared by the page and the library (and surviving
 *    dialog remounts), and
 *  - a small concurrency queue, so extraction trickles in instead of blocking.
 */
import { useGlobalStore } from "../core/store";

/** How many extractions may be in flight at once. */
const CONCURRENCY = 4;

const cache = new Map<string, string>();
const pending = new Map<string, Promise<string | null>>();

interface IconJob {
  path: string;
  resolve: (icon: string | null) => void;
}

const queue: IconJob[] = [];
let inFlight = 0;

function pump(): void {
  while (inFlight < CONCURRENCY && queue.length > 0) {
    const job = queue.shift();
    if (!job) return;
    inFlight += 1;
    void useGlobalStore
      .getState()
      .invokeExtractAppIcon(job.path)
      .then((data) => {
        if (data) cache.set(job.path, data);
        job.resolve(data || null);
      })
      .catch(() => job.resolve(null))
      .finally(() => {
        inFlight -= 1;
        pump();
      });
  }
}

/** A previously extracted icon, if we already have it. */
export function cachedAppIcon(path: string | null | undefined): string | null {
  if (!path) return null;
  return cache.get(path) ?? null;
}

/** Seed the cache from a scan result that already carries a base64 icon. */
export function warmAppIcon(path: string | null | undefined, icon: string | null | undefined): void {
  if (!path || !icon) return;
  cache.set(path, icon);
}

/**
 * Icon for `path`, extracted on demand. Resolves `null` when the executable has
 * no icon (or cannot be read) — callers fall back to a placeholder.
 */
export function loadAppIcon(path: string): Promise<string | null> {
  const hit = cache.get(path);
  if (hit) return Promise.resolve(hit);
  const running = pending.get(path);
  if (running) return running;

  const task = new Promise<string | null>((resolve) => {
    queue.push({ path, resolve });
    pump();
  }).finally(() => {
    pending.delete(path);
  });
  pending.set(path, task);
  return task;
}
