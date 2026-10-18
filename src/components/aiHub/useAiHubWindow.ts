import { useCallback, useEffect, useRef, useState } from "react";
import { availableMonitors, currentMonitor, getCurrentWindow, PhysicalPosition, PhysicalSize } from "@tauri-apps/api/window";
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
/** Size and spot before 放大, for 还原. */
const NORMAL_KEY = "qa-window-normal";
/**
 * 放大 is a capped size, not full screen: on a wide monitor full screen leaves the
 * reading column floating in empty space. Matches `max_inner_size` in tray/bubble.rs.
 */
export const LARGE_MAX = { width: 1120, height: 960 };
/** Room left around the enlarged window inside the monitor's work area (logical px). */
const LARGE_MARGIN = 48;

type Rect = { x: number; y: number; width: number; height: number };
type SavedBounds = Rect & { large: boolean };

function isRect(raw: unknown): raw is Rect {
  const r = raw as Rect | null;
  return !!r && [r.x, r.y, r.width, r.height].every(Number.isFinite);
}

function readJson(key: string): unknown {
  try {
    return JSON.parse(localStorage.getItem(key) || "null");
  } catch {
    return null;
  }
}

function writeJson(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* ignore */
  }
}

function readBounds(): SavedBounds | null {
  const raw = readJson(BOUNDS_KEY) as (Rect & { large?: boolean; maximized?: boolean }) | null;
  if (!isRect(raw)) return null;
  // `maximized` is the flag older versions stored for real OS maximize.
  return { x: raw.x, y: raw.y, width: raw.width, height: raw.height, large: raw.large === true || raw.maximized === true };
}

/** The enlarged window: capped, centered in the work area (physical px). */
export function largeRect(work: Rect, scale: number): Rect {
  const width = Math.round(Math.min(LARGE_MAX.width * scale, work.width - LARGE_MARGIN * scale));
  const height = Math.round(Math.min(LARGE_MAX.height * scale, work.height - LARGE_MARGIN * scale));
  return {
    x: Math.round(work.x + (work.width - width) / 2),
    y: Math.round(work.y + (work.height - height) / 2),
    width,
    height,
  };
}

type HubWindow = ReturnType<typeof getCurrentWindow>;

async function workAreaOf(): Promise<{ work: Rect; scale: number } | null> {
  const m = await currentMonitor();
  if (!m) return null;
  const { position, size } = m.workArea;
  return { work: { x: position.x, y: position.y, width: size.width, height: size.height }, scale: m.scaleFactor };
}

async function applyRect(win: HubWindow, r: Rect): Promise<void> {
  await win.setSize(new PhysicalSize(r.width, r.height));
  await win.setPosition(new PhysicalPosition(r.x, r.y));
}

async function currentRect(win: HubWindow): Promise<Rect | null> {
  const [pos, size] = await Promise.all([win.outerPosition(), win.outerSize()]);
  // Minimized windows report a parking spot around -32000.
  if (pos.x <= -30000 || pos.y <= -30000 || size.width <= 0 || size.height <= 0) return null;
  return { x: pos.x, y: pos.y, width: size.width, height: size.height };
}

/**
 * Remember the window's size, position and 放大 state across launches, and
 * toggle 放大 / 还原. A saved spot is reused only if its title bar still lands on
 * a connected monitor; an unplugged screen must not strand the window off-desktop.
 */
export function useWindowBounds(): { maximized: boolean; toggleMaximized: () => void } {
  const [large, setLarge] = useState(false);
  const largeRef = useRef(false);
  /** Size 放大 produced: a manual resize away from it ends the enlarged state. */
  const largeSize = useRef<{ width: number; height: number } | null>(null);
  /** Our own setSize / setPosition must not be recorded as a user resize. */
  const applying = useRef(false);

  const markLarge = (on: boolean, size?: { width: number; height: number }) => {
    largeRef.current = on;
    largeSize.current = on ? size ?? null : null;
    setLarge(on);
  };

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
      if (applying.current) return;
      try {
        const rect = await currentRect(win);
        if (disposed || !rect) return;
        const ls = largeSize.current;
        if (largeRef.current && ls && (Math.abs(rect.width - ls.width) > 8 || Math.abs(rect.height - ls.height) > 8)) {
          markLarge(false);
        }
        writeJson(BOUNDS_KEY, { ...rect, large: largeRef.current });
      } catch {
        /* ignore */
      }
    };
    const schedule = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void save(), 500);
    };
    void (async () => {
      try {
        // Older versions really maximized the window; that state is no longer offered.
        if (await win.isMaximized()) await win.unmaximize();
      } catch {
        /* ignore */
      }
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
            applying.current = true;
            await applyRect(win, saved);
            if (saved.large) markLarge(true, { width: saved.width, height: saved.height });
          }
        } catch {
          /* ignore */
        } finally {
          window.setTimeout(() => (applying.current = false), 300);
        }
      }
      if (disposed) return;
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
    void (async () => {
      applying.current = true;
      try {
        if (largeRef.current) {
          const normal = readJson(NORMAL_KEY);
          const area = await workAreaOf();
          if (isRect(normal)) {
            await applyRect(win, normal);
          } else if (area) {
            // No remembered size: the default 720×640, centered.
            const w = Math.round(720 * area.scale);
            const h = Math.round(640 * area.scale);
            await applyRect(win, {
              x: Math.round(area.work.x + (area.work.width - w) / 2),
              y: Math.round(area.work.y + (area.work.height - h) / 2),
              width: w,
              height: h,
            });
          }
          markLarge(false);
        } else {
          const area = await workAreaOf();
          const before = await currentRect(win);
          if (!area) return;
          if (before) writeJson(NORMAL_KEY, before);
          const target = largeRect(area.work, area.scale);
          await applyRect(win, target);
          markLarge(true, { width: target.width, height: target.height });
        }
      } catch {
        /* ignore */
      } finally {
        window.setTimeout(() => {
          applying.current = false;
          void currentRect(win)
            .then((rect) => rect && writeJson(BOUNDS_KEY, { ...rect, large: largeRef.current }))
            .catch(() => {});
        }, 300);
      }
    })();
  }, []);

  return { maximized: large, toggleMaximized };
}