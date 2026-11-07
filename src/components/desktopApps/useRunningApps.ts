import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useGlobalStore } from "../../core/store";
import type { DesktopRunningApp } from "../../core/types";

/** Slow enough to be free, fast enough that the dots do not lie for long. */
const POLL_MS = 8_000;

/**
 * Same rows, same order? The backend sorts deterministically, so an equal
 * answer must not re-render the page (and the library) every 8 seconds.
 */
function sameRows(a: DesktopRunningApp[], b: DesktopRunningApp[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    const left = a[i];
    const right = b[i];
    if (
      left.processName !== right.processName ||
      left.pid !== right.pid ||
      left.title !== right.title ||
      left.visible !== right.visible
    ) {
      return false;
    }
  }
  return true;
}

export interface RunningAppsState {
  /** Everything the backend could host right now, one row per process. */
  apps: DesktopRunningApp[];
  /** Lowercased process name → row, for the bar's status dots. */
  byProcess: Map<string, DesktopRunningApp>;
  /** True while a user-visible scan is in flight (library spinner / empty state). */
  loading: boolean;
  /**
   * The last scan failed. The 8s poll keeps retrying, so this self-clears on
   * the first success — the UI just has to say why the list is empty.
   */
  error: boolean;
  /**
   * Force a scan now. `silent` skips the spinner: the 8s poll must not make the
   * page flicker its loading state forever.
   */
  refresh: (silent?: boolean) => Promise<void>;
}

/**
 * Polls `desktop_embed_running_apps`.
 *
 * One IPC covers every app rather than one `is_app_running` per tab, and the
 * backend memoises the scan for 1.5s so overlapping callers share it.
 *
 * Failure is not fatal (the page's primary flow does not depend on knowing
 * what is running), but it is surfaced via `error` so an empty list never
 * looks like "nothing is running" when the scan actually died.
 */
export function useRunningApps(enabled = true): RunningAppsState {
  const invokeRunningApps = useGlobalStore((s) => s.invokeDesktopEmbedRunningApps);
  const [apps, setApps] = useState<DesktopRunningApp[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);
  const aliveRef = useRef(true);

  const refresh = useCallback(
    async (silent = false) => {
      if (!silent) setLoading(true);
      try {
        const list = await invokeRunningApps();
        if (aliveRef.current) {
          setApps((prev) => (sameRows(prev, list) ? prev : list));
          setError(false);
        }
      } catch {
        if (aliveRef.current) {
          setApps([]);
          setError(true);
        }
      } finally {
        if (!silent && aliveRef.current) setLoading(false);
      }
    },
    [invokeRunningApps]
  );

  useEffect(() => {
    aliveRef.current = true;
    if (!enabled) {
      setApps([]);
      return () => {
        aliveRef.current = false;
      };
    }

    let timer = 0;
    const schedule = () => {
      timer = window.setTimeout(() => {
        // A hidden window cannot show dots, and the user may have been away for
        // hours: skip the scan and try again on the next tick.
        if (document.visibilityState === "visible") void refresh(true);
        schedule();
      }, POLL_MS);
    };

    void refresh();
    schedule();

    // Coming back to the window is the moment the user expects the dots to be
    // right — do not wait up to 8s for the next tick.
    const onVisible = () => {
      if (document.visibilityState === "visible") void refresh(true);
    };
    document.addEventListener("visibilitychange", onVisible);

    return () => {
      aliveRef.current = false;
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [enabled, refresh]);

  const byProcess = useMemo(() => {
    const map = new Map<string, DesktopRunningApp>();
    for (const app of apps) {
      const key = app.processName.trim().toLowerCase();
      if (key && !map.has(key)) map.set(key, app);
    }
    return map;
  }, [apps]);

  return { apps, byProcess, loading, error, refresh };
}
