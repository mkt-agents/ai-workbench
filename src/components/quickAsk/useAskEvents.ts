import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useGlobalStore } from "../../core/store";
import type { AppSettings, AppTheme } from "../../core/types";
import {
  APP_THEME_CHANGED_EVENT,
  applyDocumentTheme,
  readPersistedTheme,
} from "../../lib/theme";

export type AskEventsOptions = {
  setSettings: (patch: Partial<AppSettings>) => void;
  getContext: () => Promise<{ text: string; truncated: boolean }>;
  invalidateContext: () => void;
  setInput: (updater: string | ((prev: string) => string)) => void;
  inputRef: { current: HTMLTextAreaElement | null };
  /** Close snippet panel / param modal when text arrives from outside. */
  closePopovers: () => void;
  /** Prefill carried an encoded task string — switch mode/task accordingly. */
  onPrefillTask?: (task: string) => void;
};

/** Cross-window events: quick-ask-shown / quick-ask-prefill / theme changes. */
export function useAskEvents(opts: AskEventsOptions) {
  const { setSettings, getContext, invalidateContext, setInput, inputRef, closePopovers } = opts;
  // Use ref to always have the latest getContext without re-registering listeners
  const getContextRef = useRef(getContext);
  getContextRef.current = getContext;
  const onPrefillTaskRef = useRef(opts.onPrefillTask);
  onPrefillTaskRef.current = opts.onPrefillTask;
  // Deduplicate prefill events (both quick-ask windows receive the same event)
  const prefillIdRef = useRef<string | null>(null);

  useEffect(() => {
    let unlistenShown: (() => void) | undefined;
    let unlistenPrefill: (() => void) | undefined;
    let unlistenTheme: (() => void) | undefined;

    const syncThemeFromStorage = () => {
      const theme = readPersistedTheme();
      if (!theme) return;
      setSettings({ theme });
      applyDocumentTheme(theme);
    };

    void listen("quick-ask-shown", () => {
      syncThemeFromStorage();
      // Reload mutable data from SQLite — the main window may have added/removed
      // models, snippets, or sessions while this window was hidden. bootApp()
      // only runs once on mount (memoised promise), so without this the lists go stale.
      void useGlobalStore.getState().loadAIModels();
      void useGlobalStore.getState().loadSnippets();
      void useGlobalStore.getState().loadQuickAskSessions();
      void useGlobalStore.getState().loadRecentProjects();
      void useGlobalStore.getState().loadWorkspaces();
      // Defer context build by 1.5s so the window paints instantly; the first
      // send will build it immediately anyway if the user is fast.
      invalidateContext();
      window.setTimeout(() => {
        void getContextRef.current();
      }, 1500);
    }).then((fn) => {
      unlistenShown = fn;
    });
    void listen<{ id?: string; text?: string; task?: string }>("quick-ask-prefill", (event) => {
      const payload = event.payload;
      let text = "";
      let id: string | null = null;
      let task: string | null = null;
      if (typeof payload === "string") {
        text = payload;
      } else if (payload && typeof payload === "object") {
        text = (payload.text as string) || "";
        id = (payload.id as string) || null;
        task = (payload.task as string) || null;
      }
      if (!text) return;
      // Deduplicate: if this event was already processed, skip. The task
      // switch must live inside this branch too, or the two quick-ask windows
      // would diverge on mode/task.
      if (id && prefillIdRef.current === id) return;
      if (id) {
        prefillIdRef.current = id;
      }
      if (task) onPrefillTaskRef.current?.(task);
      setInput((prev) => (prev ? `${prev}\n${text}` : text));
      closePopovers();
      requestAnimationFrame(() => inputRef.current?.focus());
    }).then((fn) => {
      unlistenPrefill = fn;
    });
    void listen<AppTheme>(APP_THEME_CHANGED_EVENT, (event) => {
      const theme = event.payload;
      if (!theme) return;
      setSettings({ theme });
      applyDocumentTheme(theme);
    }).then((fn) => {
      unlistenTheme = fn;
    });
    return () => {
      unlistenShown?.();
      unlistenPrefill?.();
      unlistenTheme?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [setSettings]);
}
