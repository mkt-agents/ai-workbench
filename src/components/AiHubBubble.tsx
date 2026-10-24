import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  currentMonitor,
  getCurrentWindow,
  monitorFromPoint,
  PhysicalPosition,
} from "@tauri-apps/api/window";
import { useGlobalStore } from "../core/store";
import type { AppTheme } from "../core/types";
import {
  APP_THEME_CHANGED_EVENT,
  applyDocumentTheme,
  normalizeAppTheme,
  readPersistedTheme,
} from "../lib/theme";

/** Below this screen travel, treat pointer-up as a click (not a drag). */
const CLICK_MOVE_PX = 5;
/** Logical margin from work-area edges (matches tray/bubble.rs BUBBLE_MARGIN). */
const EDGE_MARGIN_LOGICAL = 24;
/** Logical top band reserved for main titlebar (matches BUBBLE_TOP_SAFE_LOGICAL). */
const TOP_SAFE_LOGICAL = 96;

import pikachuAnim from "../assets/pets/pikachu.gif";
import shinyAnim from "../assets/pets/pikachu-shiny.gif";
import robotAnim from "../assets/pets/robot-anim.gif";

type BubblePhase = "idle" | "pressed" | "dragging" | "firing";
/** 桌宠皮肤：双击悬浮球循环切换（均为 PokeAPI 官方像素动画 GIF） */
type PetSkin = "pikachu" | "shiny" | "robot" | "orb";
const PET_ORDER: PetSkin[] = ["pikachu", "shiny", "robot", "orb"];
const PET_KEY = "qa-bubble-pet";
/** 图片皮肤 → 动画 GIF */
const PET_IMAGES: Record<Exclude<PetSkin, "orb">, string> = {
  pikachu: pikachuAnim,
  shiny: shinyAnim,
  robot: robotAnim,
};
/** 单击与双击的判别窗口（ms）：窗内第二击 = 换装，超时 = 打开天玑 */
const DOUBLE_CLICK_MS = 280;

function readPetSkin(): PetSkin {
  try {
    const raw = localStorage.getItem(PET_KEY);
    // 旧存档里的 gundam 皮肤已下线，迁到 robot
    const skin = raw === "gundam" ? "robot" : raw;
    return PET_ORDER.includes(skin as PetSkin) ? (skin as PetSkin) : "pikachu";
  } catch {
    return "pikachu";
  }
}

/** Slim geometric spark — reads cleaner than Lucide Sparkles at small size. */
function BubbleMark({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      width="17"
      height="17"
      viewBox="0 0 18 18"
      fill="none"
      aria-hidden
    >
      <path
        d="M9 1.6c.28 2.85 1.75 4.32 4.6 4.6-2.85.28-4.32 1.75-4.6 4.6-.28-2.85-1.75-4.32-4.6-4.6 2.85-.28 4.32-1.75 4.6-4.6Z"
        stroke="currentColor"
        strokeWidth="1.25"
        strokeLinejoin="round"
      />
      <circle cx="14.2" cy="3.6" r="0.85" fill="currentColor" opacity="0.85" />
      <circle cx="3.9" cy="13.8" r="0.65" fill="currentColor" opacity="0.55" />
    </svg>
  );
}

/**
 * Desktop floating bubble: click shows AI Hub; right-click hides it; drag
 * moves and release snaps to the nearer left/right edge.
 */
export default function AiHubBubble() {
  const { t } = useTranslation("aihub");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const [phase, setPhase] = useState<BubblePhase>("idle");
  const [pet, setPet] = useState<PetSkin>(readPetSkin);
  const [speech, setSpeech] = useState<string | null>(null);
  const dragRef = useRef<{
    startX: number;
    startY: number;
    moved: boolean;
    dragging: boolean;
  } | null>(null);
  const fireTimerRef = useRef<number | null>(null);
  const clickTimerRef = useRef<number | null>(null);
  const speechTimerRef = useRef<number | null>(null);

  useEffect(() => {
    document.documentElement.classList.add("qa-bubble-mode");
    document.body?.classList.add("qa-bubble-mode");
    applyDocumentTheme(normalizeAppTheme(readPersistedTheme() ?? settings.theme));

    void (async () => {
      // Do not call webview.setBackgroundColor here: the runtime path can
      // force an opaque white backing (white ring around the orb). The
      // window is created hidden and only revealed after "ready", so the
      // first paint is already the orb — no flash to suppress.
      try {
        await invoke("ai_hub_bubble_ready");
      } catch {
        /* ignore */
      }
    })();

    let unlisten: (() => void) | undefined;
    void listen<AppTheme>(APP_THEME_CHANGED_EVENT, (event) => {
      applyDocumentTheme(normalizeAppTheme(event.payload));
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
      if (fireTimerRef.current != null) window.clearTimeout(fireTimerRef.current);
      if (clickTimerRef.current != null) window.clearTimeout(clickTimerRef.current);
      if (speechTimerRef.current != null) window.clearTimeout(speechTimerRef.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount-only theme + ready handshake
  }, []);

  useEffect(() => {
    const pos = settings.aiHubBubblePos;
    if (pos && Number.isFinite(pos.x) && Number.isFinite(pos.y)) {
      void invoke("set_ai_hub_bubble_position", { x: pos.x, y: pos.y }).catch(() => {});
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- apply saved pos once on mount
  }, []);

  const persistPosition = useCallback(
    async (x: number, y: number) => {
      setSettings({ aiHubBubblePos: { x, y } });
      try {
        await invoke("set_ai_hub_bubble_position", { x, y });
      } catch {
        /* ignore */
      }
    },
    [setSettings],
  );

  /** Magnet to nearer left/right work-area edge; clamp Y below titlebar band. */
  const snapAndPersist = useCallback(async () => {
    try {
      const win = getCurrentWindow();
      const pos = await win.outerPosition();
      const size = await win.outerSize();
      const monitor =
        (await monitorFromPoint(pos.x, pos.y)) ?? (await currentMonitor());
      if (!monitor) {
        await persistPosition(pos.x, pos.y);
        return;
      }
      const scale = monitor.scaleFactor;
      const margin = Math.round(EDGE_MARGIN_LOGICAL * scale);
      const topSafe = Math.round(TOP_SAFE_LOGICAL * scale);
      const work = monitor.workArea;
      const minX = work.position.x + margin;
      const maxX = work.position.x + work.size.width - size.width - margin;
      const minY = work.position.y + Math.max(margin, topSafe);
      const maxY = work.position.y + work.size.height - size.height - margin;
      const centerX = pos.x + size.width / 2;
      const midX = work.position.x + work.size.width / 2;
      const x = centerX < midX ? minX : Math.max(minX, maxX);
      const y = Math.min(Math.max(pos.y, minY), Math.max(minY, maxY));
      await win.setPosition(new PhysicalPosition(x, y));
      await persistPosition(x, y);
    } catch {
      try {
        const pos = await getCurrentWindow().outerPosition();
        await persistPosition(pos.x, pos.y);
      } catch {
        /* ignore */
      }
    }
  }, [persistPosition]);

  const playFire = useCallback(() => {
    setPhase("firing");
    if (fireTimerRef.current != null) window.clearTimeout(fireTimerRef.current);
    fireTimerRef.current = window.setTimeout(() => {
      setPhase("idle");
      fireTimerRef.current = null;
    }, 420);
  }, []);

  /** 冒一句互动短语，1.4s 后收回 */
  const showSpeech = useCallback((text: string) => {
    setSpeech(text);
    if (speechTimerRef.current != null) window.clearTimeout(speechTimerRef.current);
    speechTimerRef.current = window.setTimeout(() => {
      setSpeech(null);
      speechTimerRef.current = null;
    }, 1400);
  }, []);

  /** 双击换装：皮卡丘 → 异色皮卡丘 → 盖诺赛克特 → 原始悬浮球，循环 */
  const cyclePet = useCallback(() => {
    setPet((prev) => {
      const next = PET_ORDER[(PET_ORDER.indexOf(prev) + 1) % PET_ORDER.length];
      try {
        localStorage.setItem(PET_KEY, next);
      } catch {
        /* ignore */
      }
      return next;
    });
    playFire();
    showSpeech(t("bubblePetSwap"));
  }, [playFire, showSpeech, t]);

  /** 单击：宠物先做个反应，稍等确认不是双击再打开天玑 */
  const handleTap = useCallback(() => {
    playFire();
    const key =
      pet === "pikachu"
        ? "bubblePetPikachu"
        : pet === "shiny"
          ? "bubblePetShiny"
          : pet === "robot"
            ? "bubblePetRobot"
            : "bubblePetOrb";
    showSpeech(t(key));
    if (clickTimerRef.current != null) return;
    clickTimerRef.current = window.setTimeout(() => {
      clickTimerRef.current = null;
      void invoke("show_ai_hub").catch((err) => console.error(err));
    }, DOUBLE_CLICK_MS);
  }, [pet, playFire, showSpeech, t]);

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragRef.current = {
      startX: e.screenX,
      startY: e.screenY,
      moved: false,
      dragging: false,
    };
    setPhase("pressed");
  };

  const onPointerMove = async (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    const dx = e.screenX - d.startX;
    const dy = e.screenY - d.startY;
    if (!d.moved && Math.hypot(dx, dy) >= CLICK_MOVE_PX) {
      d.moved = true;
      d.dragging = true;
      setPhase("dragging");
      try {
        await getCurrentWindow().startDragging();
      } catch {
        /* ignore */
      }
    }
  };

  const endPointer = async (e: React.PointerEvent, cancelled: boolean) => {
    const d = dragRef.current;
    dragRef.current = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    if (!d) {
      setPhase("idle");
      return;
    }
    if (cancelled) {
      setPhase("idle");
      return;
    }
    if (d.moved || d.dragging) {
      await snapAndPersist();
      setPhase("idle");
      return;
    }
    // 第二击落在双击窗口内 → 换装，取消本已排队的「打开天玑」
    if (clickTimerRef.current != null) {
      window.clearTimeout(clickTimerRef.current);
      clickTimerRef.current = null;
      cyclePet();
      return;
    }
    handleTap();
  };

  return (
    <div
      className={`qa-bubble-root is-${phase} pet-${pet}`}
      onPointerDown={onPointerDown}
      onPointerMove={(e) => void onPointerMove(e)}
      onPointerUp={(e) => void endPointer(e, false)}
      onPointerCancel={(e) => void endPointer(e, true)}
      onContextMenu={(e) => {
        e.preventDefault();
        void invoke("hide_ai_hub").catch((err) => console.error(err));
      }}
      title={t("bubbleTip")}
    >
      <div className="qa-bubble-aura" aria-hidden />
      {pet === "orb" ? (
        <div className="qa-bubble-orb" aria-label={t("title")}>
          <BubbleMark className="qa-bubble-icon" />
        </div>
      ) : (
        <div className="qa-pet" aria-label={t("title")}>
          <div className="qa-pet-body">
            <img
              className="qa-pet-img"
              src={PET_IMAGES[pet as Exclude<PetSkin, "orb">]}
              alt=""
              draggable={false}
            />
          </div>
        </div>
      )}
      {speech && <span className="qa-pet-speech">{speech}</span>}
    </div>
  );
}
