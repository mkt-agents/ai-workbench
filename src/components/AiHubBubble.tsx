import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import {
  currentMonitor,
  getCurrentWindow,
  monitorFromPoint,
  PhysicalPosition,
} from "@tauri-apps/api/window";
import { useGlobalStore } from "../core/store";
import type { AppTheme, PetSkinId } from "../core/types";
import {
  APP_THEME_CHANGED_EVENT,
  applyDocumentTheme,
  normalizeAppTheme,
  readPersistedTheme,
} from "../lib/theme";
import PetStage, { type BubblePhase } from "./aiHub/PetStage";
import {
  PET_CONFIG_EVENT,
  PET_GAZE_EVENT,
  loadUserPack,
  nextPetSkin,
  normalizePetConfig,
  petSkin,
  type PetConfig,
  type PetConfigPatch,
  type PetMood,
} from "../lib/petSkins";
import { reactTo, type PetReaction, type PetSignal } from "../lib/petBrain";

/** Below this screen travel, treat pointer-up as a click (not a drag). */
const CLICK_MOVE_PX = 5;
/** Logical margin from work-area edges (matches tray/bubble.rs BUBBLE_MARGIN). */
const EDGE_MARGIN_LOGICAL = 24;
/** Logical top band reserved for main titlebar (matches BUBBLE_TOP_SAFE_LOGICAL). */
const TOP_SAFE_LOGICAL = 96;
/** 单击与双击的判别窗口（ms）：窗内第二击 = 换装，超时 = 打开天玑 */
const DOUBLE_CLICK_MS = 280;
/** 气泡语停留时长。 */
const SPEECH_MS = 1600;
/** 就地菜单展开时临时撑高（逻辑 px）；菜单贴在桌宠头顶，底边不动（Rust 同规则）。
 *  高度要盖住 5 个菜单项：5×22 + 4×2 间隙 + 10 padding + 2 边框 + 4 缝隙 = 136。 */
const PET_MENU_EXTRA = 136;
/** 没有新信号超过这个时间就打瞌睡。 */
const SLEEP_AFTER_MS = 180_000;

/**
 * Desktop pet: click opens the hub, right-click opens an in-place menu, drag snaps
 * to the nearest edge. It also listens to the hub's agent events, so its mood
 * reflects what the AI is actually doing on this machine.
 */
export default function AiHubBubble() {
  const { t } = useTranslation("aihub");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const [cfg, setCfg] = useState<PetConfig>(() =>
    normalizePetConfig({
      skin: settings.aiHubPetSkin,
      size: settings.aiHubPetSize,
      quiet: settings.aiHubPetQuiet,
    }),
  );
  const [phase, setPhase] = useState<BubblePhase>("idle");
  const [mood, setMood] = useState<PetMood>("idle");
  const [speech, setSpeech] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  /** 自备素材包的帧数：清单存在模块级镜像里，靠这个数触发重算。 */
  const [packFrames, setPackFrames] = useState(0);
  const packRevRef = useRef(0);
  const dragRef = useRef<{
    startX: number;
    startY: number;
    moved: boolean;
    dragging: boolean;
  } | null>(null);
  const fireTimerRef = useRef<number | null>(null);
  const landTimerRef = useRef<number | null>(null);
  /** 悬停一会儿 = 摸头：停够 1.1 秒才反应，扫过去不算 */
  const hoverTimerRef = useRef<number | null>(null);
  const menuOpenRef = useRef(false);
  menuOpenRef.current = menuOpen;
  const clickTimerRef = useRef<number | null>(null);
  /** 指针倾角：直接写 CSS 变量，不走 state，鼠标划过时不用重渲。 */
  const rootRef = useRef<HTMLDivElement | null>(null);
  /** 最近一次视线方向（-1..1）：全局光标与窗内指针共用一台"眼睛"。 */
  const gazeRef = useRef({ x: 0, y: 0 });
  /** 上一次真正写进 CSS 的值：70ms 一条的全局视线不该每次都触发样式重算。 */
  const gazeKeyRef = useRef("");
  /** 指针是否停在窗里：在窗里就以本地精确值为准，离开后才吃全局方向。 */
  const hoverRef = useRef(false);
  const speechTimerRef = useRef<number | null>(null);
  const moodTimerRef = useRef<number | null>(null);
  const sleepTimerRef = useRef<number | null>(null);
  const moodRef = useRef<PetMood>("idle");
  const cfgRef = useRef(cfg);
  cfgRef.current = cfg;

  const skinDef = useMemo(() => petSkin(cfg.skin), [cfg.skin, packFrames]);

  const clearTimer = (ref: { current: number | null }) => {
    if (ref.current != null) {
      window.clearTimeout(ref.current);
      ref.current = null;
    }
  };

  /** 冒一句互动短语；安静模式不说话。 */
  const say = useCallback((text: string) => {
    if (cfgRef.current.quiet) return;
    setSpeech(text);
    clearTimer(speechTimerRef);
    speechTimerRef.current = window.setTimeout(() => {
      setSpeech(null);
      speechTimerRef.current = null;
    }, SPEECH_MS);
  }, []);

  /** 情绪演完自己收回 idle；holdMs=0 的情绪一直停着，等下一条信号。 */
  const play = useCallback(
    (reaction: PetReaction) => {
      clearTimer(moodTimerRef);
      moodRef.current = reaction.mood;
      setMood(reaction.mood);
      if (reaction.speechKey) {
        say(t(reaction.speechKey, reaction.speechParams ?? {}));
      }
      if (reaction.holdMs > 0) {
        moodTimerRef.current = window.setTimeout(() => {
          moodRef.current = "idle";
          setMood("idle");
          moodTimerRef.current = null;
        }, reaction.holdMs);
      }
    },
    [say, t],
  );

  /** 收到任何信号都算"桌面有动静"：重新计打瞌睡倒计时，再演对应情绪。 */
  const react = useCallback(
    (signal: PetSignal) => {
      clearTimer(sleepTimerRef);
      sleepTimerRef.current = window.setTimeout(
        () => play(reactTo({ kind: "idle" })),
        SLEEP_AFTER_MS,
      );
      play(reactTo(signal));
    },
    [play],
  );

  const pushBox = useCallback((width: number, height: number) => {
    void invoke("set_ai_hub_bubble_box", { width, height }).catch(() => {});
  }, []);

  useEffect(() => {
    document.documentElement.classList.add("qa-bubble-mode");
    document.body?.classList.add("qa-bubble-mode");
    applyDocumentTheme(normalizeAppTheme(readPersistedTheme() ?? settings.theme));

    let chatterTimer = 0;
    void (async () => {
      // Do not call webview.setBackgroundColor here: the runtime path can
      // force an opaque white backing (white ring around the orb). The
      // window is created hidden and only revealed after "ready", so the
      // first paint is already the orb — no flash to suppress.
      //
      // 顺序不能换：先把存档尺寸推给 Rust（位置与标题栏危险带都按尺寸算），再报 ready
      // 让后端显窗。反过来做，非默认档位的桌宠会先按 112 出现、再跳一次才到位。
      // 自备素材先读进来：第一帧就该是导入的形象，不要先闪一下圆盘
      setPackFrames(await loadUserPack());
      const pos = settings.aiHubBubblePos;
      if (pos && Number.isFinite(pos.x) && Number.isFinite(pos.y)) {
        await invoke("set_ai_hub_bubble_position", { x: pos.x, y: pos.y }).catch(() => {});
      }
      await invoke("set_ai_hub_bubble_box", {
        width: cfgRef.current.size,
        height: cfgRef.current.size,
      }).catch(() => {});
      await invoke("ai_hub_bubble_ready").catch(() => {});
      // 没人摸它也要会困：倒计时从显窗那刻就得起，不能等第一条事件。
      sleepTimerRef.current = window.setTimeout(
        () => play(reactTo({ kind: "idle" })),
        SLEEP_AFTER_MS,
      );

      // 待机时自己唠叨一句（安静模式 / 菜单开着 / 正在干活时都不吭声）
      chatterTimer = window.setInterval(() => {
        if (cfgRef.current.quiet || menuOpenRef.current || dragRef.current) return;
        if (moodRef.current !== "idle" && moodRef.current !== "sleepy") return;
        const keys = ["petChatter1", "petChatter2", "petChatter3", "petChatter4"];
        say(t(keys[Math.floor(Math.random() * keys.length)]));
      }, 105_000);
    })();

    let unlistenTheme: (() => void) | undefined;
    void listen<AppTheme>(APP_THEME_CHANGED_EVENT, (event) => {
      applyDocumentTheme(normalizeAppTheme(event.payload));
    }).then((fn) => {
      unlistenTheme = fn;
    });

    // 主窗设置页改形象 / 尺寸 / 安静模式后广播过来（同主题事件的做法）。
    let unlistenCfg: (() => void) | undefined;
    void listen<PetConfigPatch>(PET_CONFIG_EVENT, (event) => {
      const payload = event.payload ?? {};
      const next = normalizePetConfig({ ...cfgRef.current, ...payload });
      setCfg(next);
      if (next.size !== cfgRef.current.size) {
        // 用 ref 而不是闭包里的 menuOpen：这个监听只注册一次，闭包里的值永远是首次渲染的 false
        pushBox(next.size, next.size + (menuOpenRef.current ? PET_MENU_EXTRA : 0));
      }
      // 主窗换了自备素材（导入或清除）：重读目录，皮肤清单才会变
      if (payload.packRev !== undefined && payload.packRev !== packRevRef.current) {
        packRevRef.current = payload.packRev;
        void loadUserPack().then(setPackFrames);
      }
    }).then((fn) => {
      unlistenCfg = fn;
    });

    // 全局光标视线（Rust 侧轮询，只在桌宠窗可见时发）：窗只有一百来像素，指针几乎不会
    // 真的进来，没有这条事件的话熊猫永远不知道用户在哪儿 —— 这才是"呆板"的根。
    let unlistenGaze: (() => void) | undefined;
    void listen<{ x: number; y: number }>(PET_GAZE_EVENT, (event) => {
      const { x, y } = event.payload ?? { x: 0, y: 0 };
      if (!Number.isFinite(x) || !Number.isFinite(y)) return;
      gazeRef.current = { x, y };
      // 指针正在窗里（或被拎着）时以本地精确值为准，松手/离开后才吃全局方向
      if (hoverRef.current || dragRef.current) return;
      applyGaze(x, y, false);
    }).then((fn) => {
      unlistenGaze = fn;
    });

    return () => {
      unlistenTheme?.();
      unlistenCfg?.();
      unlistenGaze?.();
      window.clearInterval(chatterTimer);
      clearTimer(fireTimerRef);
      clearTimer(landTimerRef);
      clearTimer(hoverTimerRef);
      clearTimer(clickTimerRef);
      clearTimer(speechTimerRef);
      clearTimer(moodTimerRef);
      clearTimer(sleepTimerRef);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount-only: theme + ready handshake + saved geometry
  }, []);

  /** 天玑在做什么，桌宠就演什么：后端 app.emit 是广播，这里只吃事件、不加命令。 */
  useEffect(() => {
    let dead = false;
    const offs: (() => void)[] = [];
    const on = <T,>(name: string, handler: (payload: T) => void) => {
      void listen<T>(name, (event) => {
        if (!dead) handler(event.payload);
      }).then((un) => {
        if (dead) un();
        else offs.push(un);
      });
    };
    on<{ status: string }>("agent-step", (payload) => {
      // done / info 只是单步收尾与通知（MCP 探测、压缩提示、重试预告…）：
      // 不代表真的在执行动作，不动情绪——否则没干活的时候熊猫也举键盘盒。
      if (payload.status === "done" || payload.status === "info") return;
      react({ kind: "step", status: payload.status });
    });
    on("agent-plan", () => react({ kind: "plan" }));
    on("agent-approval", () => react({ kind: "approval" }));
    // 流式每 token 一条：已经在思考就别再重渲一次。
    on("generate-text-chunk", () => {
      if (moodRef.current !== "thinking") react({ kind: "stream" });
    });
    on<{ elapsedMs?: number; limited?: boolean }>("generate-text-done", (payload) =>
      react(payload.limited ? { kind: "limited" } : { kind: "done", elapsedMs: payload.elapsedMs ?? 0 }),
    );
    on("generate-text-error", () => react({ kind: "error" }));
    return () => {
      dead = true;
      offs.forEach((off) => off());
    };
  }, [react]);

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
    clearTimer(fireTimerRef);
    fireTimerRef.current = window.setTimeout(() => {
      setPhase("idle");
      fireTimerRef.current = null;
    }, 420);
  }, []);

  const setSkin = useCallback(
    (next: PetSkinId) => {
      setCfg((prev) => ({ ...prev, skin: next }));
      setSettings({ aiHubPetSkin: next });
      void emit(PET_CONFIG_EVENT, { skin: next }).catch(() => {});
    },
    [setSettings],
  );

  /** 换装：麻薯 → 超级玛丽 → 原始悬浮球，循环 */
  const cyclePet = useCallback(() => {
    setSkin(nextPetSkin(cfgRef.current.skin));
    playFire();
    react({ kind: "swap" });
  }, [playFire, react, setSkin]);

  /** 单击：宠物先做个反应，稍等确认不是双击再打开天玑 */
  const handleTap = useCallback(() => {
    playFire();
    react({ kind: "tap", skin: cfgRef.current.skin });
    if (clickTimerRef.current != null) return;
    clickTimerRef.current = window.setTimeout(() => {
      clickTimerRef.current = null;
      void invoke("show_ai_hub").catch((err) => console.error(err));
    }, DOUBLE_CLICK_MS);
  }, [playFire, react]);

  const closeMenu = useCallback(() => {
    setMenuOpen(false);
    pushBox(cfgRef.current.size, cfgRef.current.size);
  }, [pushBox]);

  const toggleMenu = useCallback(() => {
    setMenuOpen((open) => {
      const next = !open;
      const size = cfgRef.current.size;
      pushBox(size, next ? size + PET_MENU_EXTRA : size);
      return next;
    });
  }, [pushBox]);

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    if (menuOpen) {
      // 菜单开着时的第一下只负责收起，不顺手开窗。
      closeMenu();
      return;
    }
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragRef.current = {
      startX: e.screenX,
      startY: e.screenY,
      moved: false,
      dragging: false,
    };
    setPhase("pressed");
  };

  /**
   * 视线：把方向（-1..1，x 右为正、y 下为正）写进 CSS 变量。
   *
   * 分两层给值 —— 图像层（`--qa-gaze-*`）幅度大、缓动快，像眼球先扫过去；3D 身体层
   * （`--qa-tilt-*`）幅度小、缓动慢，像重心随后跟上。两层同速同幅就会像一整张贴纸在平移，
   * 反而更假。
   */
  const applyGaze = useCallback((gx: number, gy: number, near: boolean) => {
    const el = rootRef.current;
    if (!el) return;
    // near = 指针真在窗里，可以给满；远处光标只给六成，别让它整只拧到极限
    const k = near ? 1 : 0.62;
    const key = `${gx.toFixed(2)}|${gy.toFixed(2)}|${k}`;
    if (key === gazeKeyRef.current) return;
    gazeKeyRef.current = key;
    // 图像层跟着光标同向平移（光标在右边就往右挪，就是"看过去"），
    // 身体层的 rotateX 反过来：负角 = 顶端前倾，才像低头凑近而不是后仰。
    el.style.setProperty("--qa-tilt-x", (gx * 3.2 * k).toFixed(2));
    el.style.setProperty("--qa-tilt-y", (-gy * 2.4 * k).toFixed(2));
    el.style.setProperty("--qa-gaze-x", (gx * 3.6 * k).toFixed(2));
    el.style.setProperty("--qa-gaze-y", (gy * 2.4 * k).toFixed(2));
  }, []);

  /** 指针在窗内：用局部坐标，比全局光标更跟手（窗很小，局部能给到满幅）。 */
  const aimAt = (e: React.PointerEvent) => {
    const rect = rootRef.current?.getBoundingClientRect();
    if (!rect?.width || !rect?.height) return;
    const gx = ((e.clientX - rect.left) / rect.width - 0.5) * 2;
    const gy = ((e.clientY - rect.top) / rect.height - 0.5) * 2;
    gazeRef.current = { x: gx, y: gy };
    applyGaze(gx, gy, true);
  };

  /** 指针离开：回落到最近一次方向（一般就是全局光标方向），硬归零会像"抽回去"。 */
  const settleAim = () => applyGaze(gazeRef.current.x, gazeRef.current.y, false);

  const onPointerMove = async (e: React.PointerEvent) => {
    aimAt(e);
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
    if (!d || cancelled) {
      setPhase("idle");
      return;
    }
    if (d.moved || d.dragging) {
      await snapAndPersist();
      // 放手后回弹一下再站定：直接切 idle 会像贴纸瞬移
      setPhase("landing");
      clearTimer(landTimerRef);
      landTimerRef.current = window.setTimeout(() => {
        setPhase("idle");
        landTimerRef.current = null;
      }, 320);
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

  const runMenuItem = (action: () => void) => {
    action();
    closeMenu();
  };

  return (
    <div
      ref={rootRef}
      className={`qa-bubble-root is-${phase}${menuOpen ? " has-menu" : ""}`}
      data-skin={cfg.skin}
      style={{ "--qa-bubble-size": `${cfg.size}px` } as React.CSSProperties}
      onPointerDown={onPointerDown}
      onPointerMove={(e) => void onPointerMove(e)}
      onPointerUp={(e) => void endPointer(e, false)}
      onPointerCancel={(e) => void endPointer(e, true)}
      onPointerEnter={() => {
        hoverRef.current = true;
        // 摸头：停够 1.1 秒才高兴，扫过去不算
        clearTimer(hoverTimerRef);
        hoverTimerRef.current = window.setTimeout(() => {
          hoverTimerRef.current = null;
          if (dragRef.current || menuOpenRef.current) return;
          react({ kind: "tap", skin: cfgRef.current.skin });
        }, 1100);
      }}
      onPointerLeave={() => {
        hoverRef.current = false;
        clearTimer(hoverTimerRef);
        settleAim();
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        toggleMenu();
      }}
      aria-label={t("bubbleTip")}
      role="button"
      tabIndex={-1}
    >
      <div className="qa-bubble-stage">
        <div className="qa-bubble-aura" aria-hidden />
        <PetStage skin={skinDef} mood={mood} phase={phase} />
        {speech && !menuOpen && <span className="qa-pet-speech">{speech}</span>}
      </div>
      {menuOpen && (
        <div
          className="qa-bubble-menu"
          role="menu"
          aria-label={t("title")}
          onPointerDown={(e) => e.stopPropagation()}
        >
          <button
            type="button"
            role="menuitem"
            onClick={() => runMenuItem(() => void invoke("open_main").catch(() => {}))}
          >
            {t("petMenuMain")}
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => runMenuItem(() => void invoke("show_ai_hub").catch(() => {}))}
          >
            {t("petMenuOpen")}
          </button>
          <button type="button" role="menuitem" onClick={() => runMenuItem(cyclePet)}>
            {t("petMenuSwap")}
          </button>
          <button
            type="button"
            role="menuitem"
            aria-pressed={cfg.quiet}
            onClick={() =>
              runMenuItem(() => {
                const quiet = !cfgRef.current.quiet;
                setCfg((prev) => ({ ...prev, quiet }));
                setSettings({ aiHubPetQuiet: quiet });
                void emit(PET_CONFIG_EVENT, { quiet }).catch(() => {});
              })
            }
          >
            {cfg.quiet ? t("petMenuLoud") : t("petMenuQuiet")}
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() =>
              runMenuItem(() => {
                setSettings({ aiHubBubbleEnabled: false });
                void invoke("set_ai_hub_bubble_visible", { visible: false }).catch(() => {});
              })
            }
          >
            {t("petMenuHide")}
          </button>
        </div>
      )}
    </div>
  );
}
