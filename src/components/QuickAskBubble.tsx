import { useCallback, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useGlobalStore } from "../core/store";

const CLICK_MOVE_PX = 4;

/**
 * Desktop floating bubble: click toggles Quick Ask; drag moves the window.
 * Solid orb fills the window; Windows circular region clips white corners.
 */
export default function QuickAskBubble() {
  const { t } = useTranslation("quickask");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const dragRef = useRef<{
    startX: number;
    startY: number;
    moved: boolean;
    dragging: boolean;
  } | null>(null);

  useEffect(() => {
    // No app theme chrome — theme body::before paints a rectangle.
    document.documentElement.removeAttribute("data-theme");
    document.documentElement.classList.add("qa-bubble-mode");
    document.body?.classList.add("qa-bubble-mode");

    void (async () => {
      try {
        await getCurrentWebview().setBackgroundColor({
          red: 0,
          green: 0,
          blue: 0,
          alpha: 0,
        });
      } catch {
        /* ignore */
      }
      try {
        await invoke("quick_ask_bubble_ready");
      } catch {
        /* ignore */
      }
    })();
  }, []);

  useEffect(() => {
    const pos = settings.quickAskBubblePos;
    if (pos && Number.isFinite(pos.x) && Number.isFinite(pos.y)) {
      void invoke("set_quick_ask_bubble_position", { x: pos.x, y: pos.y }).catch(() => {});
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- apply saved pos once on mount
  }, []);

  const persistPosition = async () => {
    try {
      const pos = await getCurrentWindow().outerPosition();
      setSettings({ quickAskBubblePos: { x: pos.x, y: pos.y } });
      await invoke("set_quick_ask_bubble_position", { x: pos.x, y: pos.y });
    } catch {
      /* ignore */
    }
  };

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragRef.current = {
      startX: e.screenX,
      startY: e.screenY,
      moved: false,
      dragging: false,
    };
  };

  const onPointerMove = async (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    const dx = e.screenX - d.startX;
    const dy = e.screenY - d.startY;
    if (!d.moved && Math.hypot(dx, dy) >= CLICK_MOVE_PX) {
      d.moved = true;
      d.dragging = true;
      try {
        await getCurrentWindow().startDragging();
      } catch {
        /* ignore */
      }
    }
  };

  const onPointerUp = async (e: React.PointerEvent) => {
    const d = dragRef.current;
    dragRef.current = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    if (!d) return;
    if (d.moved || d.dragging) {
      await persistPosition();
      return;
    }
    try {
      await invoke("tray_toggle_quick_ask");
    } catch (err) {
      console.error(err);
    }
  };

  const hideBubble = useCallback(async () => {
    setSettings({ quickAskBubbleEnabled: false });
    try {
      await emit("quick-ask-bubble-enabled", false);
      await invoke("set_quick_ask_bubble_visible", {
        visible: false,
        x: null,
        y: null,
      });
    } catch {
      /* ignore */
    }
  }, [setSettings]);

  return (
    <div
      className="qa-bubble-root"
      onPointerDown={onPointerDown}
      onPointerMove={(e) => void onPointerMove(e)}
      onPointerUp={(e) => void onPointerUp(e)}
      onContextMenu={(e) => {
        e.preventDefault();
        void hideBubble();
      }}
      title={t("bubbleTip")}
    >
      <div className="qa-bubble-orb" aria-label={t("title")}>
        <Sparkles size={15} strokeWidth={1.75} className="qa-bubble-icon" />
      </div>
    </div>
  );
}
