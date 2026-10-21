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
/** Must match `min_inner_size` in tray/bubble.rs (logical px). */
const MIN_SIZE = { width: 640, height: 600 };
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

/** Tolerance for rounding and the clamp of `max_inner_size` (physical px). */
const SIZE_SLACK = 16;

function nearlyLarge(size: { width: number; height: number }, target: Rect): boolean {
  return size.width >= target.width - SIZE_SLACK && size.height >= target.height - SIZE_SLACK;
}

/** The default 720×640, centered in the work area. */
function defaultRect(work: Rect, scale: number): Rect {
  const width = Math.round(720 * scale);
  const height = Math.round(640 * scale);
  return {
    x: Math.round(work.x + (work.width - width) / 2),
    y: Math.round(work.y + (work.height - height) / 2),
    width,
    height,
  };
}

/**
 * Keep a physical-pixel rect inside the window contract and the work area.
 *
 * Saved bounds predate the current minimum size (or were written on another
 * monitor/DPI). Landing at e.g. 436×254 lets the empty state's centered
 * content overflow over the header and composer, so never trust them raw.
 */
function clampRect(rect: Rect, work: Rect, scale: number): Rect {
  const width = Math.round(
    Math.min(
      Math.max(rect.width, MIN_SIZE.width * scale),
      Math.min(LARGE_MAX.width * scale, work.width),
    ),
  );
  const height = Math.round(
    Math.min(
      Math.max(rect.height, MIN_SIZE.height * scale),
      Math.min(LARGE_MAX.height * scale, work.height),
    ),
  );
  return {
    x: Math.round(Math.min(Math.max(rect.x, work.x), work.x + work.width - width)),
    y: Math.round(Math.min(Math.max(rect.y, work.y), work.y + work.height - height)),
    width,
    height,
  };
}

/** Clamp saved bounds to the monitor that still holds their title bar. */
function clampSavedRect(
  saved: Rect,
  monitors: Awaited<ReturnType<typeof availableMonitors>>,
): Rect | null {
  const probeX = saved.x + 40;
  const probeY = saved.y + 16;
  const monitor = monitors.find(
    (m) =>
      probeX >= m.position.x &&
      probeX < m.position.x + m.size.width &&
      probeY >= m.position.y &&
      probeY < m.position.y + m.size.height,
  );
  if (!monitor) return null;
  return clampRect(
    saved,
    {
      x: monitor.workArea.position.x,
      y: monitor.workArea.position.y,
      width: monitor.workArea.size.width,
      height: monitor.workArea.size.height,
    },
    monitor.scaleFactor,
  );
}

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

/** Position is the outer corner (what setPosition takes); size is the inner size (what setSize takes). */
async function currentRect(win: HubWindow): Promise<Rect | null> {
  const [pos, size] = await Promise.all([win.outerPosition(), win.innerSize()]);
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
        const monitor = await currentMonitor().catch(() => null);
        if (
          monitor &&
          (rect.width < MIN_SIZE.width * monitor.scaleFactor - SIZE_SLACK ||
            rect.height < MIN_SIZE.height * monitor.scaleFactor - SIZE_SLACK)
        ) {
          // A hidden/transient window can report a size below the contract;
          // persisting it would restore the broken layout next launch.
          return;
        }
        const ls = largeSize.current;
        if (largeRef.current && ls && (Math.abs(rect.width - ls.width) > SIZE_SLACK || Math.abs(rect.height - ls.height) > SIZE_SLACK)) {
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
          const target = clampSavedRect(saved, await availableMonitors());
          if (target && !disposed) {
            applying.current = true;
            await applyRect(win, target);
            const area = await workAreaOf().catch(() => null);
            const big = area ? nearlyLarge(target, largeRect(area.work, area.scale)) : false;
            if (big) markLarge(true, { width: target.width, height: target.height });
          }
        } catch {
          /* ignore */
        } finally {
          window.setTimeout(() => {
            void currentRect(win)
              .then((rect) => {
                if (rect && largeRef.current) largeSize.current = { width: rect.width, height: rect.height };
              })
              .catch(() => {})
              .finally(() => (applying.current = false));
          }, 300);
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
        const area = await workAreaOf();
        if (!area) return;
        const target = largeRect(area.work, area.scale);
        const osMax = await win.isMaximized().catch(() => false);
        if (osMax) await win.unmaximize();
        const before = await currentRect(win);
        // Decide from the real size too: a lost flag must not turn 还原 into 放大 again.
        if (largeRef.current || osMax || (before && nearlyLarge(before, target))) {
          const normal = readJson(NORMAL_KEY);
          // A remembered "normal" as big as 放大 would make 还原 a no-op; a
          // tiny legacy value must not bring the broken layout back either.
          const remembered = isRect(normal) ? clampRect(normal, area.work, area.scale) : null;
          const back =
            remembered && !nearlyLarge(remembered, target)
              ? remembered
              : clampRect(defaultRect(area.work, area.scale), area.work, area.scale);
          await applyRect(win, back);
          markLarge(false);
        } else {
          if (before) writeJson(NORMAL_KEY, before);
          await applyRect(win, target);
          const after = await currentRect(win);
          markLarge(true, after ?? target);
        }
      } catch {
        /* ignore */
      } finally {
        window.setTimeout(() => {
          void currentRect(win)
            .then((rect) => {
              if (!rect) return;
              // Re-baseline on what the window really ended up as, before user resizes are judged.
              if (largeRef.current) largeSize.current = { width: rect.width, height: rect.height };
              writeJson(BOUNDS_KEY, { ...rect, large: largeRef.current });
            })
            .catch(() => {})
            .finally(() => (applying.current = false));
        }, 300);
      }
    })();
  }, []);
  return { maximized: large, toggleMaximized };
}