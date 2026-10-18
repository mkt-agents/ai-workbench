import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
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
