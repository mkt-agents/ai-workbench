import { useCallback, useEffect, useRef, useState } from "react";
import { availableMonitors, getCurrentWindow, PhysicalPosition, PhysicalSize } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import type { AiHubMode } from "./AiHubHeader";

/**
 * Hide the AI hub window. Goes through Rust: tao resolves hide() via its cached
 * visibility flag, which can desync from the real window (see tray/bubble.rs `vis`)
 * and turn this into a no-op. Falls back to the JS API if the command is unavailable.
 */
export async function hideAiHub(): Promise<void> {
  try {
    await invoke("hide_ai_hub");
  } catch {
    try {
      await getCurrentWindow().hide();
    } catch {
      /* ignore */
    }
  }
}

/**
 * Drag-resizing the frameless window repaints every blurred surface per frame;
 * body.resizing (base.css) drops backdrop-filter until the drag settles.
 */
export function useResizingBodyClass(): void {
  useEffect(() => {
    let idleTimer: number | undefined;
    const onResize = () => {
      document.body.classList.add("resizing");
      window.clearTimeout(idleTimer);
      idleTimer = window.setTimeout(() => document.body.classList.remove("resizing"), 300);
    };
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      window.clearTimeout(idleTimer);
      document.body.classList.remove("resizing");
    };
  }, []);
}

/**
 * Alt+1 / Alt+2: switch between ask and tools modes. Shift is excluded so
 * layout combos like Alt+Shift+2 don't flip the view by accident.
 */
export function useModeHotkeys(setMode: (mode: AiHubMode) => void): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
      if (e.key === "1") {
        e.preventDefault();
        setMode("ask");
      } else if (e.key === "2") {
        e.preventDefault();
        setMode("tools");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setMode]);
}

/** A flag that turns itself off `ms` after the last `flash()` (toasts, "done" ticks). */
export function useTimedFlag(ms: number): [boolean, () => void] {
  const [on, setOn] = useState(false);
  const timerRef = useRef<number | undefined>(undefined);
  const flash = useCallback(() => {
    setOn(true);
    window.clearTimeout(timerRef.current);
    timerRef.current = window.setTimeout(() => setOn(false), ms);
  }, [ms]);
  useEffect(() => () => window.clearTimeout(timerRef.current), []);
  return [on, flash];
}

const BOUNDS_KEY = "qa-window-bounds";

type SavedBounds = { x: number; y: number; width: number; height: number; maximized: boolean };

function readBounds(): SavedBounds | null {
  try {
    const raw = JSON.parse(localStorage.getItem(BOUNDS_KEY) || "null");
    if (!raw || ![raw.x, raw.y, raw.width, raw.height].every(Number.isFinite)) return null;
    return { x: raw.x, y: raw.y, width: raw.width, height: raw.height, maximized: raw.maximized === true };
  } catch {
    return null;
  }
}

function writeBounds(bounds: SavedBounds): void {
  try {
    localStorage.setItem(BOUNDS_KEY, JSON.stringify(bounds));
  } catch {
    /* ignore */
  }
}

/**
 * Remember the window's size, position and maximized state across launches.
 * While maximized only the flag is recorded, so restoring brings back the last
 * normal size. A saved spot is reused only if its title bar still lands on a
 * connected monitor; an unplugged screen must not strand the window off-desktop.
 */
export function useWindowBounds(): { maximized: boolean; toggleMaximized: () => void } {
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    const win = getCurrentWindow();
    let disposed = false;
    let timer: number | undefined;
    const unlisteners: Array<() => void> = [];
    const keep = (unlisten: () => void) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    };
    const save = async () => {
      try {
        const isMax = await win.isMaximized();
        if (disposed) return;
        setMaximized(isMax);
        if (isMax) {
          const prev = readBounds();
          if (prev) writeBounds({ ...prev, maximized: true });
          return;
        }
        const [pos, size] = await Promise.all([win.outerPosition(), win.outerSize()]);
        // Minimized windows report a parking spot around -32000.
        if (pos.x <= -30000 || pos.y <= -30000 || size.width <= 0 || size.height <= 0) return;
        writeBounds({ x: pos.x, y: pos.y, width: size.width, height: size.height, maximized: false });
      } catch {
        /* ignore */
      }
    };
    const schedule = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void save(), 500);
    };
    void (async () => {
      const saved = readBounds();
      if (saved) {
        try {
          const probeX = saved.x + 40;
          const probeY = saved.y + 16;
          const onScreen = (await availableMonitors()).some(
            (m) =>
              probeX >= m.position.x &&
              probeX < m.position.x + m.size.width &&
              probeY >= m.position.y &&
              probeY < m.position.y + m.size.height,
          );
          if (onScreen && !disposed) {
            await win.setSize(new PhysicalSize(saved.width, saved.height));
            await win.setPosition(new PhysicalPosition(saved.x, saved.y));
            if (saved.maximized) await win.maximize();
          }
        } catch {
          /* ignore */
        }
      }
      if (disposed) return;
      setMaximized(await win.isMaximized().catch(() => false));
      try {
        keep(await win.onResized(schedule));
        keep(await win.onMoved(schedule));
      } catch {
        /* ignore */
      }
    })();
    return () => {
      disposed = true;
      window.clearTimeout(timer);
      unlisteners.forEach((u) => u());
    };
  }, []);
  const toggleMaximized = useCallback(() => {
    const win = getCurrentWindow();
    void win
      .isMaximized()
      .then(async (isMax) => {
        await (isMax ? win.unmaximize() : win.maximize());
        setMaximized(!isMax);
      })
      .catch(() => {});
  }, []);
  return { maximized, toggleMaximized };
}