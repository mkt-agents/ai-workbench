import { useEffect, useRef } from "react";
import { listen, type EventCallback, type EventName, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Safe Tauri event listener hook.
 *
 * Correctly handles the async `listen()` resolution: the unlisten function is
 * captured in a ref so cleanup always works, even if the component unmounts
 * before the listener is registered.
 */
export function useTauriEvent<T extends unknown>(
  event: EventName,
  handler: EventCallback<T>,
  deps: unknown[] = [],
) {
  const unlistenRef = useRef<UnlistenFn | null>(null);
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    let cancelled = false;
    void listen<T>(event, (e) => {
      if (!cancelled) handlerRef.current(e);
    }).then((fn) => {
      if (cancelled) {
        fn();
      } else {
        unlistenRef.current = fn;
      }
    });
    return () => {
      cancelled = true;
      unlistenRef.current?.();
      unlistenRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [event, ...deps]);
}

export default useTauriEvent;
