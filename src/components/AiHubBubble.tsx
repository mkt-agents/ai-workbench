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
import type { PetMicro, PressZone } from "./aiHub/PandaGirlPet";
import {
  PET_CONFIG_EVENT,
  PET_GAZE_EVENT,
  PET_HOT_EVENT,
  PET_TRACK_EVENT,
  loadUserPack,
  nextPetSkin,
  normalizePetConfig,
  petSkin,
  type PetConfig,
  type PetConfigPatch,
  type PetMood,
} from "../lib/petSkins";
import {
  reactTo,
  PAT_OVER_STREAK,
  type PetGesturePlay,
  type PetReaction,
  type PetSignal,
} from "../lib/petBrain";
import {
  applyAffinityDelta,
  bondTier,
  dailyDecay,
  greetingSlot,
  pickPoolKey,
  todayKey,
  type BondTier,
  type SpeechPoolName,
} from "../lib/petBond";

/** Below this screen travel, treat pointer-up as a click (not a drag). */
const CLICK_MOVE_PX = 5;
/** Logical margin from work-area edges (matches tray/bubble.rs BUBBLE_MARGIN). */
const EDGE_MARGIN_LOGICAL = 24;
/** Logical top band reserved for main titlebar (matches BUBBLE_TOP_SAFE_LOGICAL). */
const TOP_SAFE_LOGICAL = 96;
/** 气泡语最短 / 最长停留（按字数伸缩）。 */
const SPEECH_MS_MIN = 1600;
const SPEECH_MS_MAX = 3200;
/** 头顶长按多久算「多摸一会儿」。 */
const HEAD_HOLD_MS = 600;
/** 等审批时再次催一声的间隔。 */
const WAIT_NUDGE_MS = 45_000;
/** 就地菜单展开时临时撑高（逻辑 px）；菜单贴在桌宠头顶，底边不动（Rust 同规则）。
 *  高度要盖住 6 个菜单项：6×22 + 5×2 间隙 + 10 padding + 2 边框 + 4 缝隙 = 158。
 *  上界 `BUBBLE_BOX_MAX_H` 是 320，最大档 160 + 158 = 318 仍在区间内。 */
const PET_MENU_EXTRA = 158;
/**
 * 头顶常驻透明留白（对齐鱼娘/大肥鱼）：气泡只在里头淡入淡出，说话时不改窗尺寸，
 * 人物才不会跟着跳。给到三行短句 + 三角缝；菜单仍临时再撑高。
 */
const PET_SPEECH_HEADROOM = 96;
/** 窗宽下限：长句少折行，常驻，避免说话时忽宽忽窄。 */
const PET_BOX_MIN_W = 192;
/** 白天没人摸多久打瞌睡；夜晚更短（对齐大肥鱼「夜里更困」）。 */
const SLEEP_AFTER_MS = 180_000;
const SLEEP_AFTER_NIGHT_MS = 90_000;
/** 身区长按多久算「抱一下」（未拖动）。 */
const HUG_HOLD_MS = 450;
/** 装饰手势演出时长。 */
const GESTURE_MS = 900;
/**
 * 连续戳裙边算害羞的时间窗。害羞放在裙边而不是身区：身区是"主按钮"（戳一下、
 * 长按抱抱），连击计数容易和长按混在一起；裙边本来只是纯互动，手势最不容易误触。
 */
const TAIL_SHY_WINDOW_MS = 1500;
const TAIL_SHY_STREAK = 3;
/**
 * 双击判定窗口的兜底值（ms）。真实值在挂载时向系统要（`system_double_click_ms`）：
 * 写死一个数在慢手 / 触控板上会像"点了没反应"，在快手用户那儿又会把两次独立点击
 * 误判成双击。双击 = 打开天玑；单击只互动不弹窗，所以不会跟"戳一下"打架。
 */
const DOUBLE_TAP_FALLBACK_MS = 500;
/** 系统值离谱时退回兜底（有的机器会读出 0 或极大值）。 */
const DOUBLE_TAP_MIN_MS = 200;
const DOUBLE_TAP_MAX_MS = 1000;
/** 干活碎碎念冷却。 */
const MUTTER_THINK_CD_MS = 25_000;
const MUTTER_WORK_CD_MS = 40_000;
const MUTTER_WORK_AFTER_MS = 8_000;
/** 连摸头计数窗口；升级（开心）/ 抗议阈值由 petBrain 的 PAT_HOT_STREAK / PAT_OVER_STREAK 判。 */
const PAT_WINDOW_MS = 5000;
/** 距上次互动超过这么久，下一次互动先演「回来啦」。 */
const RETURN_AFTER_MS = 30 * 60_000;
/** 被拎着（拖拽中）多久开始挣扎。 */
const STRUGGLE_AFTER_MS = 6000;
/** 甩动检测：拖拽单步速度超过（px/ms）记一次猛甩，累计满阈值演头晕。 */
const SHAKE_SPEED_PX_MS = 2.4;
const SHAKE_LIMIT = 3;
/** 回归问候只盖互动类信号；agent 事件有自己的情绪轴，不抢。 */
const RETURN_GREET_KINDS = new Set<PetSignal["kind"]>([
  "tap",
  "pat",
  "poke",
  "tail",
  "hug",
  "shy",
  "wake",
]);
/** idle 闲聊池按产品轨分流：Work 轨聊日程，Code 轨一半概率聊代码，其余走通用池。 */
function chatterPoolFor(track: "code" | "work" | undefined): SpeechPoolName {
  if (track === "work") return "chatterWork";
  return Math.random() < 0.5 ? "chatterCode" : "chatter";
}
/** 微动作各自的演出收尾时长（ms）。 */
const MICRO_HOLD_MS: Partial<Record<PetMicro, number>> = {
  stretch: 1200,
  shy: 1200,
  dizzy: 2200,
  struggle: 1600,
  lookaround: 1500,
  glance: 1400,
  yawn: 1900,
  rub: 1500,
  wave: 1300,
  peek: 1300,
  bounce: 1300,
  wobble: 1300,
  dust: 1300,
  hop: 1100,
  nod: 1100,
  ear: 1100,
};
function microHoldMs(m: PetMicro): number {
  return MICRO_HOLD_MS[m] ?? 1000;
}

/**
 * 按时段挑一条待机连招链：早晨伸懒腰醒神、午后打哈欠犯困、深夜揉眼，
 * 其余走通用池。每条链 2~3 步，链演完才回 idle。全部是原地动作，不位移。
 */
function pickMicroChain(hour: number): PetMicro[] {
  const pools: PetMicro[][][] = [
    // 深夜（22-5）：揉眼、哈欠
    [
      ["yawn", "rub"],
      ["rub", "lookaround"],
      ["glance", "yawn"],
      ["nod", "rub"],
    ],
    // 早晨（6-11）：伸懒腰醒神
    [
      ["stretch", "lookaround"],
      ["stretch", "wave"],
      ["glance", "ear"],
      ["wave", "bounce", "nod"],
    ],
    // 午后（13-17）：犯困
    [
      ["yawn", "lookaround"],
      ["wobble", "yawn"],
      ["peek", "nod"],
      ["dust", "stretch"],
    ],
  ];
  const generic: PetMicro[][] = [
    ["stretch", "lookaround"],
    ["glance", "ear", "wave"],
    ["wave", "peek", "nod"],
    ["dust", "wobble"],
    ["lookaround", "ear"],
    ["hop", "nod"],
    ["yawn", "lookaround"],
  ];
  const pool =
    hour >= 22 || hour < 5
      ? pools[0]
      : hour >= 6 && hour < 11
        ? pools[1]
        : hour >= 13 && hour < 17
          ? pools[2]
          : generic;
  return pool[Math.floor(Math.random() * pool.length)] ?? ["lookaround"];
}

type HitZone = "head" | "body" | "tail";

/** 一次点按 / 拖拽的全部状态（松手即整块丢弃，从根上避免残留）。 */
interface DragState {
  startX: number;
  startY: number;
  moved: boolean;
  dragging: boolean;
  zone: HitZone;
  hugged: boolean;
  /** 甩动检测：上一步位置 / 时间与累计猛甩次数 */
  lastX: number;
  lastY: number;
  lastT: number;
  shake: number;
  dizzied: boolean;
  /** 承接 pointer capture 的元素：root 是 pointer-events:none，只能落在真正命中的元素上 */
  capture: HTMLElement | null;
  /** 拖动几何：拖起来那一刻的窗口物理坐标与 DPR；位移 = screenX 差值 × scale */
  origin: { x: number; y: number } | null;
  scale: number;
  /** 待落地的目标坐标：rAF 合并，避免每个 pointermove 都丢一条 setPosition */
  target: { x: number; y: number } | null;
  raf: number;
}

function sleepAfterMs(): number {
  const h = new Date().getHours();
  return h >= 22 || h < 6 ? SLEEP_AFTER_NIGHT_MS : SLEEP_AFTER_MS;
}

/**
 * 上=头、中=身、下/后缘=裙边（内部仍叫 tail）。
 * 比例按形象自己的框算：窗顶有常驻说话留白、窗宽也有下限，按整窗算头区会落在
 * 透明留白里永远点不到，下半身则全被算成裙边。
 */
function hitZoneAt(e: { clientX: number; clientY: number }, root: HTMLElement | null): HitZone {
  // 分开查：合在一个选择器里会按文档顺序先命中外层的 .qa-pet（整条舞台）
  const el =
    root?.querySelector<HTMLElement>(".qa-panda-girl") ??
    root?.querySelector<HTMLElement>(".qa-pet") ??
    root;
  const rect = el?.getBoundingClientRect();
  if (!rect?.height || !rect.width) return "body";
  const y = (e.clientY - rect.top) / rect.height;
  const x = (e.clientX - rect.left) / rect.width;
  if (y < 0.4) return "head";
  if (y > 0.78 || x > 0.78) return "tail";
  return "body";
}

/**
 * 命中矩形内的归一化坐标（0..1）→ 分区。阈值与 `hitZoneAt` 对齐；
 * 指针几乎不会真的进窗（窗才一百来像素），悬停摸头靠 Rust 推的全局光标位置算。
 */
function zoneAtNormalized(nx: number, ny: number): HitZone {
  if (!Number.isFinite(nx) || !Number.isFinite(ny)) return "body";
  if (ny < 0.4) return "head";
  if (ny > 0.78 || nx > 0.78) return "tail";
  return "body";
}

/**
 * Desktop pet: 单击只互动、双击打开天玑（换形象走右键菜单），右键开就地菜单，拖到屏幕边缘会吸附。
 * It also listens to the hub's agent events, so its mood reflects what the AI is
 * actually doing on this machine.
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
  const [pressZone, setPressZone] = useState<PressZone | null>(null);
  const [mood, setMood] = useState<PetMood>("idle");
  const [speech, setSpeech] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [gesture, setGesture] = useState<PetGesturePlay | null>(null);
  const [micro, setMicro] = useState<PetMicro>("none");
  const [celebrate, setCelebrate] = useState(false);
  const phaseRef = useRef<BubblePhase>("idle");
  phaseRef.current = phase;
  const [affinity, setAffinity] = useState(() =>
    Number.isFinite(settings.aiHubPetAffinity) ? Number(settings.aiHubPetAffinity) : 20,
  );
  /** 自备素材包的帧数：清单存在模块级镜像里，靠这个数触发重算。 */
  const [packFrames, setPackFrames] = useState(0);
  const packRevRef = useRef(0);
  const dragRef = useRef<DragState | null>(null);
  const fireTimerRef = useRef<number | null>(null);
  const landTimerRef = useRef<number | null>(null);
  const hugTimerRef = useRef<number | null>(null);
  const gestureTimerRef = useRef<number | null>(null);
  /** idle 微行为调度（递归 setTimeout） */
  const microTimerRef = useRef<number | null>(null);
  /** 单次微动作演出收尾 */
  const microHoldTimerRef = useRef<number | null>(null);
  /** 连招链步与步之间的衔接计时 */
  const chainTimerRef = useRef<number | null>(null);
  /** 悬停一会儿 = 摸头：停够 1.1 秒才反应，扫过去不算 */
  const hoverTimerRef = useRef<number | null>(null);
  const menuOpenRef = useRef(false);
  menuOpenRef.current = menuOpen;
  const affinityRef = useRef(affinity);
  affinityRef.current = affinity;
  const greetedOnRef = useRef(settings.aiHubPetGreetedOn);
  /** 指针倾角：直接写 CSS 变量，不走 state，鼠标划过时不用重渲。 */
  const rootRef = useRef<HTMLDivElement | null>(null);
  /** 最近一次视线方向（-1..1）：全局光标与窗内指针共用一台"眼睛"。 */
  const gazeRef = useRef({ x: 0, y: 0 });
  /** 上一次真正写进 CSS 的值：70ms 一条的全局视线不该每次都触发样式重算。 */
  const gazeKeyRef = useRef("");
  /** 指针是否停在角色身上（由 Rust 的命中事件驱动，穿透时也能知道）。 */
  const hoverRef = useRef(false);
  /** 命中态：CSS 用 `.is-hot` 替代 `:hover`，否则切了穿透之后悬停高亮会一直卡着。 */
  const [hot, setHot] = useState(false);
  const speechTimerRef = useRef<number | null>(null);
  const speechShowingRef = useRef(false);
  /** 天玑仍在干活时的粘性情绪：摸一下演完要回到这里，别冲成 idle。 */
  const agentStickyRef = useRef<PetMood | null>(null);
  const waitNudgeRef = useRef<number | null>(null);
  const dragSaidRef = useRef(false);
  const moodTimerRef = useRef<number | null>(null);
  const sleepTimerRef = useRef<number | null>(null);
  const moodRef = useRef<PetMood>("idle");
  const cfgRef = useRef(cfg);
  cfgRef.current = cfg;
  /** 裙边连戳：TAIL_SHY_WINDOW_MS 内满 TAIL_SHY_STREAK 次起害羞。 */
  const tailTapRef = useRef({ count: 0, at: 0 });
  /** 上一次点击的时刻：双击窗口内再来一下 = 打开天玑。 */
  const lastTapAtRef = useRef(0);
  /** 双击窗口（ms）：挂载时换成系统设置的值，失败就用兜底。 */
  const doubleTapMsRef = useRef(DOUBLE_TAP_FALLBACK_MS);
  /** 头区连摸：PAT_WINDOW_MS 内计数，满 3 升级开心、满 6 抗议。 */
  const headPatRef = useRef({ count: 0, at: 0 });
  const mutterAtRef = useRef(0);
  const workStartedAtRef = useRef(0);
  const tierRef = useRef<BondTier>(0);
  /** 拎久挣扎计时（拖拽开始起算，松手作废）。 */
  const struggleTimerRef = useRef<number | null>(null);
  /** 好感升档庆祝：排到当前演出收尾、且确实空闲时才播，不插进来抢台词。 */
  const pendingBondUpRef = useRef(false);
  /** 升档排队的重试计时（在 `play` 之后定义执行器，靠 ref 互相调用）。 */
  const bondUpWaitRef = useRef<number | null>(null);
  const flushBondUpRef = useRef<() => void>(() => {});
  /** 最近一次互动时刻：离开很久后的第一条互动先演「回来啦」。 */
  const lastSeenAtRef = useRef(Date.now());
  /** 产品轨：主窗切轨时经 PET_TRACK_EVENT 同步（settings 只在主窗落盘）。 */
  const trackRef = useRef<"code" | "work" | undefined>(settings.aiHubTrack);

  const skinDef = useMemo(() => petSkin(cfg.skin), [cfg.skin, packFrames]);
  const tier: BondTier = bondTier(affinity);
  tierRef.current = tier;

  const clearTimer = (ref: { current: number | null }) => {
    if (ref.current != null) {
      window.clearTimeout(ref.current);
      ref.current = null;
    }
  };

  /**
   * 尺寸 + 可交互矩形一起下发。
   *
   * 常态 = 形象 + 头顶留白；就地菜单再临时撑高（底边不动，由 Rust 锚定）。说话绝不改尺寸。
   * 能点的只有角色站的那块方窗（两侧留白和头顶留白都交给桌面）；菜单打开时整窗可点，
   * 这样"点到菜单外面"才有机会落到窗内、被 pointerdown 收掉。
   */
  const syncBox = useCallback(() => {
    const size = cfgRef.current.size;
    const w = Math.max(size, PET_BOX_MIN_W);
    const menu = menuOpenRef.current;
    const h = menu ? size + PET_MENU_EXTRA : size + PET_SPEECH_HEADROOM;
    const hit: [number, number, number, number] = menu
      ? [0, 0, w, h]
      : [Math.round((w - size) / 2), h - size, size, size];
    void invoke("set_ai_hub_bubble_box", { width: w, height: h, hit }).catch(() => {});
  }, []);
  const syncBoxRef = useRef(syncBox);
  syncBoxRef.current = syncBox;

  /** 冒一句互动短语；安静模式不说话。只改 DOM，不改窗尺寸。 */
  const say = useCallback((text: string) => {
    if (cfgRef.current.quiet) return;
    setSpeech(text);
    speechShowingRef.current = true;
    const ms = Math.min(
      SPEECH_MS_MAX,
      Math.max(SPEECH_MS_MIN, 1400 + text.length * 48),
    );
    clearTimer(speechTimerRef);
    speechTimerRef.current = window.setTimeout(() => {
      setSpeech(null);
      speechShowingRef.current = false;
      speechTimerRef.current = null;
    }, ms);
  }, []);

  const bumpAffinity = useCallback(
    (delta: number) => {
      if (!delta) return;
      const next = applyAffinityDelta(affinityRef.current, delta);
      affinityRef.current = next;
      setAffinity(next);
      setSettings({ aiHubPetAffinity: next });
    },
    [setSettings],
  );

  /**
   * 连招 runner：按序连播微动作，每步演完停一拍再下一步，链毕触发 onDone。
   * 步前守卫不过（菜单/拖拽/说话/情绪被新信号抢走）整链作废、不触发 onDone ——
   * 打断即意味着来了新信号（sleepTimer 已被重排），不该再走「演完犯困」之类的收尾。
   */
  const runChain = useCallback((steps: PetMicro[], onDone?: () => void) => {
    clearTimer(chainTimerRef);
    const step = (i: number) => {
      const next = steps[i];
      if (!next) {
        onDone?.();
        return;
      }
      if (
        (i === 0 && speechShowingRef.current) ||
        menuOpenRef.current ||
        dragRef.current ||
        phaseRef.current !== "idle" ||
        moodRef.current !== "idle"
      ) {
        return;
      }
      clearTimer(microHoldTimerRef);
      setMicro(next);
      microHoldTimerRef.current = window.setTimeout(() => {
        setMicro("none");
        microHoldTimerRef.current = null;
        chainTimerRef.current = window.setTimeout(() => {
          chainTimerRef.current = null;
          step(i + 1);
        }, 320);
      }, microHoldMs(next));
    };
    step(0);
  }, []);

  /** 情绪演完自己收回 idle；holdMs=0 的情绪一直停着，等下一条信号。 */
  const play = useCallback(
    (reaction: PetReaction) => {
      clearTimer(moodTimerRef);
      // 任何正式演出都打断在跑的连招
      clearTimer(chainTimerRef);
      moodRef.current = reaction.mood;
      setMood(reaction.mood);
      setCelebrate(Boolean(reaction.celebrate));
      if (reaction.affinityDelta) {
        const prevTier = bondTier(affinityRef.current);
        bumpAffinity(reaction.affinityDelta);
        // 好感跨档：记一笔，等这次演出收尾、确实空闲了再撒花（不插播、不抢台词）
        if (bondTier(affinityRef.current) > prevTier) {
          pendingBondUpRef.current = true;
          flushBondUpRef.current();
        }
      }
      if (reaction.gesture) {
        setGesture(reaction.gesture);
        clearTimer(gestureTimerRef);
        gestureTimerRef.current = window.setTimeout(() => {
          setGesture(null);
          gestureTimerRef.current = null;
        }, GESTURE_MS);
      }
      if (reaction.chain?.length) {
        // 连招优先于单发 micro；链演完再切 thenMood（渐进犯困）
        runChain(
          reaction.chain,
          reaction.thenMood
            ? () => {
                moodRef.current = reaction.thenMood!;
                setMood(reaction.thenMood!);
              }
            : undefined,
        );
      } else if (reaction.micro) {
        setMicro(reaction.micro);
        clearTimer(microHoldTimerRef);
        microHoldTimerRef.current = window.setTimeout(() => {
          setMicro("none");
          microHoldTimerRef.current = null;
        }, microHoldMs(reaction.micro));
      }
      if (reaction.speechKey) {
        say(t(reaction.speechKey, reaction.speechParams ?? {}));
      }
      if (reaction.holdMs > 0) {
        moodTimerRef.current = window.setTimeout(() => {
          const back = agentStickyRef.current ?? "idle";
          moodRef.current = back;
          setMood(back);
          setCelebrate(false);
          moodTimerRef.current = null;
        }, reaction.holdMs);
      }
    },
    [bumpAffinity, runChain, say, t],
  );

  /**
   * 升档撒花排队：**不插播**。等当前台词、情绪、拖拽、菜单都收尾了再演；
   * 一直等不到（用户还在连点）就在约 10 秒后丢掉 —— 好感已经升了，不必硬塞一场演出。
   */
  const flushBondUp = useCallback(() => {
    if (bondUpWaitRef.current != null) return;
    let tries = 0;
    const tick = () => {
      bondUpWaitRef.current = null;
      if (!pendingBondUpRef.current) return;
      const idle =
        !speechShowingRef.current &&
        !menuOpenRef.current &&
        !dragRef.current &&
        phaseRef.current === "idle" &&
        moodRef.current === "idle";
      if (idle) {
        pendingBondUpRef.current = false;
        play(reactTo({ kind: "bondUp" }, tierRef.current));
        return;
      }
      if (tries >= 16) {
        pendingBondUpRef.current = false;
        return;
      }
      tries += 1;
      bondUpWaitRef.current = window.setTimeout(tick, 600);
    };
    bondUpWaitRef.current = window.setTimeout(tick, 300);
  }, [play]);
  flushBondUpRef.current = flushBondUp;

  /**
   * 到点渐进犯困：先哈欠揉眼（chain）再换 sleepy 图。
   * 起手时正忙（说话/被按住/菜单开着）链会自己作废，这里 20s 后重试，
   * 别因为撞上一次旁白就永远醒着；已经睡着或来了新信号则不再排。
   */
  const armSleepTimer = useCallback(
    (delay = sleepAfterMs()) => {
      clearTimer(sleepTimerRef);
      sleepTimerRef.current = window.setTimeout(() => {
        sleepTimerRef.current = null;
        // 桌宠被收起（页面 hidden）：什么都不演，等 visibilitychange 再重新起算。
        if (document.hidden) return;
        // 已经睡着：别再被自己的哈欠连招拉回 idle 演一遍，睡就睡踏实。
        if (moodRef.current === "sleepy") return;
        // 天玑还在干活（粘性情绪）：先不困，20 秒后再看一次，别在半路睡着。
        if (agentStickyRef.current) {
          armSleepTimer(20000);
          return;
        }
        const busy =
          menuOpenRef.current ||
          dragRef.current ||
          speechShowingRef.current ||
          phaseRef.current !== "idle";
        play(reactTo({ kind: "idle" }, tierRef.current));
        if (busy && moodRef.current === "idle") {
          armSleepTimer(20000);
        }
      }, delay);
    },
    [play],
  );

  /** 收到任何信号都重排打瞌睡倒计时，再演对应情绪。 */
  const react = useCallback(
    (signal: PetSignal) => {
      const now = Date.now();
      const away = now - lastSeenAtRef.current;
      // 「离开很久」只认互动：跑完一轮 AI 不该把用户的离开计时清零，否则回来时它不说「回来啦」。
      if (RETURN_GREET_KINDS.has(signal.kind)) lastSeenAtRef.current = now;
      clearTimer(sleepTimerRef);
      armSleepTimer();
      const base = reactTo(signal, tierRef.current);
      // 离开很久后的第一条互动：台词换成「回来啦」，动作照演（睡着则优先唤醒）
      if (
        away > RETURN_AFTER_MS &&
        moodRef.current !== "sleepy" &&
        RETURN_GREET_KINDS.has(signal.kind)
      ) {
        play({ ...base, speechKey: pickPoolKey(tierRef.current, "return"), micro: "wave" });
        return;
      }
      play(base);
    },
    [play],
  );

  /** 节流旁白：干活/思考时偶发一句，安静模式由 say 自己挡。 */
  const maybeMutter = useCallback(
    (topic: "think" | "work") => {
      const now = Date.now();
      const cd = topic === "think" ? MUTTER_THINK_CD_MS : MUTTER_WORK_CD_MS;
      if (now - mutterAtRef.current < cd) return;
      mutterAtRef.current = now;
      play(reactTo({ kind: "mutter", topic }, tierRef.current));
    },
    [play],
  );

  /** 企微回合专属旁白（安静模式仍演情绪，只是不说话）。 */
  const wecomAside = useCallback(
    (phase: "start" | "wait" | "done" | "err") => {
      const now = Date.now();
      if (now - mutterAtRef.current < 8_000) return;
      mutterAtRef.current = now;
      const pool =
        phase === "start"
          ? "wecomStart"
          : phase === "wait"
            ? "wecomWait"
            : phase === "done"
              ? "wecomDone"
              : "wecomErr";
      say(t(pickPoolKey(tierRef.current, pool)));
    },
    [say, t],
  );

  const isWecomId = (id: unknown) =>
    typeof id === "string" && id.startsWith("wecom-");

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
      // 双击窗口跟系统设置走（读不到或离谱就用兜底 500ms）
      void invoke<number>("system_double_click_ms")
        .then((ms) => {
          if (Number.isFinite(ms) && ms >= DOUBLE_TAP_MIN_MS && ms <= DOUBLE_TAP_MAX_MS) {
            doubleTapMsRef.current = ms;
          }
        })
        .catch(() => {});
      setPackFrames(await loadUserPack());
      const pos = settings.aiHubBubblePos;
      if (pos && Number.isFinite(pos.x) && Number.isFinite(pos.y)) {
        await invoke("set_ai_hub_bubble_position", { x: pos.x, y: pos.y }).catch(() => {});
      }
      // 一上来就带头顶留白，说话时才不会再改尺寸导致人物跳
      syncBoxRef.current();
      await invoke("ai_hub_bubble_ready").catch(() => {});
      // 没人摸它也要会困：倒计时从显窗那刻就得起，不能等第一条事件。
      armSleepTimer();

      // 每日首次见面问候 + 微衰减
      const today = todayKey();
      const prev = greetedOnRef.current;
      const decayed = dailyDecay(affinityRef.current, prev, today);
      if (decayed !== affinityRef.current) {
        affinityRef.current = decayed;
        setAffinity(decayed);
        setSettings({ aiHubPetAffinity: decayed });
      }
      // 日期无论如何都要记下来：安静模式只是不说话，不代表"今天没见过"——
      // 只随手势更新的话，安静模式下每启动一次就会再衰减一次。
      if (prev !== today) {
        greetedOnRef.current = today;
        setSettings({ aiHubPetGreetedOn: today });
        if (!cfgRef.current.quiet) {
          window.setTimeout(() => play(reactTo({ kind: "greet", slot: greetingSlot() })), 600);
        }
      }

      // 首次使用给一句引导：`bubbleTip` 只有读屏能读到，用户不会主动去右键。
      // 安静模式下不说、也不记已读 —— 等他把声音打开还能看到这一次。
      if (!settings.aiHubPetHintShown && !cfgRef.current.quiet) {
        setSettings({ aiHubPetHintShown: true });
        window.setTimeout(() => say(t("petFirstHint")), 2800);
      }

      // 待机碎碎念（按好感分档）；thinking/working 偶尔也冒一句
      chatterTimer = window.setInterval(() => {
        // 桌宠被收起（park 到屏外）时页面是 hidden 的，别再空转说话。
        if (document.hidden) return;
        if (cfgRef.current.quiet || menuOpenRef.current || dragRef.current) return;
        if (speechShowingRef.current) return;
        const m = moodRef.current;
        if (m === "idle" || m === "sleepy") {
          say(t(pickPoolKey(tierRef.current, chatterPoolFor(trackRef.current))));
          return;
        }
        if ((m === "thinking" || m === "working") && Math.random() < 0.35) {
          say(t(pickPoolKey(tierRef.current, m === "working" ? "mutterWork" : "mutterThink")));
        }
      }, 90_000 + Math.floor(Math.random() * 50_000));

      // idle 微行为：从时段连招池挑一条 2~3 步的小剧场连播（频率保持 12~26s）
      const scheduleMicro = () => {
        microTimerRef.current = window.setTimeout(() => {
          microTimerRef.current = null;
          if (
            !document.hidden &&
            !menuOpenRef.current &&
            !dragRef.current &&
            !speechShowingRef.current &&
            moodRef.current === "idle" &&
            phaseRef.current === "idle"
          ) {
            const chain = pickMicroChain(new Date().getHours());
            runChain(chain);
            if (
              Math.random() < 0.18 &&
              (chain[0] === "wave" ||
                chain[0] === "peek" ||
                chain[0] === "bounce" ||
                chain[0] === "stretch" ||
                chain[0] === "nod" ||
                chain[0] === "hop" ||
                chain[0] === "wobble" ||
                chain[0] === "dust")
            ) {
              say(t(pickPoolKey(tierRef.current, "micro")));
            }
          }
          scheduleMicro();
        }, 12000 + Math.random() * 14000);
      };
      scheduleMicro();
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
        // 用 ref：这个监听只注册一次，闭包里的 syncBox 会过期
        syncBoxRef.current();
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

    // 主窗切产品轨（Code/Work）广播：闲聊话术按轨分流（只更新本地 ref，盘上以主窗为准）
    let unlistenTrack: (() => void) | undefined;
    void listen<{ track?: "code" | "work" }>(PET_TRACK_EVENT, (event) => {
      const track = event.payload?.track;
      if (track === "code" || track === "work") trackRef.current = track;
    }).then((fn) => {
      unlistenTrack = fn;
    });

    // 命中态：Rust 按可交互矩形判定光标在不在角色身上，顺带切窗口的点击穿透。
    // 悬停高亮也改由它驱动 —— 穿透期间 webview 收不到 pointerenter/leave，`:hover` 会一直卡着。
    let unlistenHot: (() => void) | undefined;
    void listen<{ hot?: boolean; x?: number; y?: number }>(PET_HOT_EVENT, (event) => {
      const hot = Boolean(event.payload?.hot);
      const nx = Number(event.payload?.x);
      const ny = Number(event.payload?.y);
      hoverRef.current = hot;
      setHot(hot);
      if (!hot) {
        clearTimer(hoverTimerRef);
        // 回落到最近一次方向（一般就是全局光标方向），硬归零会像"抽回去"。
        applyGaze(gazeRef.current.x, gazeRef.current.y, false);
        // 菜单打开时可交互区就是整窗：光标离开整窗 = 用户去点别处了，顺手收掉菜单
        //（窗口拿不到焦点时 onFocusChanged 不会来，这条是兜底）。
        if (menuOpenRef.current) {
          menuOpenRef.current = false;
          setMenuOpen(false);
          syncBoxRef.current();
        }
        return;
      }
      if (dragRef.current || menuOpenRef.current) return;
      if (zoneAtNormalized(nx, ny) !== "head") {
        clearTimer(hoverTimerRef);
        return;
      }
      // 停在头区够 1.1 秒才算摸头：扫过去不算
      if (hoverTimerRef.current != null) return;
      hoverTimerRef.current = window.setTimeout(() => {
        hoverTimerRef.current = null;
        if (dragRef.current || menuOpenRef.current || !hoverRef.current) return;
        react({ kind: "pat" });
      }, 1100);
    }).then((fn) => {
      unlistenHot = fn;
    });

    // 桌宠被收起（park 到屏外）时页面会变 hidden：停掉说话 / 微动作 / 瞌睡，别在看不见的地方空转。
    const onVisibility = () => {
      if (!document.hidden) {
        armSleepTimer();
        return;
      }
      clearTimer(hoverTimerRef);
      clearTimer(microHoldTimerRef);
      clearTimer(chainTimerRef);
      clearTimer(moodTimerRef);
      clearTimer(sleepTimerRef);
      clearTimer(speechTimerRef);
      clearTimer(waitNudgeRef);
      clearTimer(bondUpWaitRef);
      pendingBondUpRef.current = false;
      speechShowingRef.current = false;
      setSpeech(null);
      setMicro("none");
      setGesture(null);
      setCelebrate(false);
      // 情绪计时器被清掉了，别让"开心/出错"的脸冻在那儿：回到干活中的粘性情绪或 idle。
      const back = agentStickyRef.current ?? "idle";
      moodRef.current = back;
      setMood(back);
    };
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      unlistenTheme?.();
      unlistenCfg?.();
      unlistenGaze?.();
      unlistenTrack?.();
      unlistenHot?.();
      document.removeEventListener("visibilitychange", onVisibility);
      window.clearInterval(chatterTimer);
      clearTimer(struggleTimerRef);
      clearTimer(bondUpWaitRef);
      clearTimer(fireTimerRef);
      clearTimer(landTimerRef);
      clearTimer(hugTimerRef);
      clearTimer(gestureTimerRef);
      clearTimer(microTimerRef);
      clearTimer(microHoldTimerRef);
      clearTimer(hoverTimerRef);
      clearTimer(speechTimerRef);
      clearTimer(moodTimerRef);
      clearTimer(sleepTimerRef);
      clearTimer(chainTimerRef);
      clearTimer(microHoldTimerRef);
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
    on<{ id?: string; status: string }>("agent-step", (payload) => {
      const wecom = isWecomId(payload.id);
      // done / info 只是单步收尾与通知（MCP 探测、压缩提示、重试预告…）：
      // 不代表真的在执行动作，不动情绪——否则没干活的时候熊猫也举键盘盒。
      if (payload.status === "done" || payload.status === "info") {
        workStartedAtRef.current = 0;
        return;
      }
      if (payload.status === "running" || payload.status === "retry") {
        if (!workStartedAtRef.current) {
          workStartedAtRef.current = Date.now();
          if (wecom) wecomAside("start");
        }
        agentStickyRef.current = "working";
        react({ kind: "step", status: payload.status });
        if (!wecom && Date.now() - workStartedAtRef.current >= MUTTER_WORK_AFTER_MS) {
          maybeMutter("work");
        }
        return;
      }
      workStartedAtRef.current = 0;
      if (payload.status === "conflict") agentStickyRef.current = "waiting";
      else agentStickyRef.current = null;
      react({ kind: "step", status: payload.status });
    });
    on<{ id?: string }>("agent-plan", (payload) => {
      agentStickyRef.current = "thinking";
      react({ kind: "plan" });
      if (isWecomId(payload?.id)) wecomAside("start");
      else maybeMutter("think");
    });
    on<{ id?: string }>("agent-approval", (payload) => {
      agentStickyRef.current = "waiting";
      react({ kind: "approval" });
      if (isWecomId(payload?.id)) wecomAside("wait");
    });
    // 流式每 token 一条：已经在思考就别再重渲一次。
    on<{ id?: string }>("generate-text-chunk", (payload) => {
      if (moodRef.current !== "thinking") {
        agentStickyRef.current = "thinking";
        react({ kind: "stream" });
        if (isWecomId(payload?.id)) wecomAside("start");
        else maybeMutter("think");
      }
    });
    on<{ id?: string; elapsedMs?: number; limited?: boolean }>("generate-text-done", (payload) => {
      workStartedAtRef.current = 0;
      agentStickyRef.current = null;
      react(
        payload.limited
          ? { kind: "limited" }
          : { kind: "done", elapsedMs: payload.elapsedMs ?? 0 },
      );
      if (isWecomId(payload?.id)) wecomAside(payload.limited ? "err" : "done");
    });
    on<{ id?: string }>("generate-text-error", (payload) => {
      workStartedAtRef.current = 0;
      agentStickyRef.current = null;
      react({ kind: "error" });
      if (isWecomId(payload?.id)) wecomAside("err");
    });
    return () => {
      dead = true;
      offs.forEach((off) => off());
    };
  }, [maybeMutter, react, wecomAside]);

  /** 等审批太久再催一声（鱼娘会摇尾巴提醒）。 */
  useEffect(() => {
    clearTimer(waitNudgeRef);
    if (mood !== "waiting") return;
    waitNudgeRef.current = window.setInterval(() => {
      if (cfgRef.current.quiet || speechShowingRef.current) return;
      say(t(pickPoolKey(tierRef.current, "waiting")));
    }, WAIT_NUDGE_MS);
    return () => clearTimer(waitNudgeRef);
  }, [mood, say, t]);

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

  /**
   * 吸附到最近的左/右屏幕边缘，并把 Y 夹在标题栏安全带之下。
   *
   * `from` = 拖动结束时已知的最终物理坐标：JS 拖动的最后一帧 setPosition 可能还在路上，
   * 直接拿它算，避免读到上一帧的位置。
   */
  const snapAndPersist = useCallback(async (from?: { x: number; y: number }) => {
    try {
      const win = getCurrentWindow();
      const size = await win.outerSize();
      const pos = from ?? (await win.outerPosition());
      // 跨屏时按窗口中心归属：左上角在副屏、身体压到主屏也不会吸到错的屏。
      const monitor =
        (await monitorFromPoint(pos.x + size.width / 2, pos.y + size.height / 2)) ??
        (await currentMonitor());
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

  /** 换装：熊猫娘 → 圆盘 →（有素材时）自备包，循环 */
  const cyclePet = useCallback(() => {
    setSkin(nextPetSkin(cfgRef.current.skin));
    playFire();
    react({ kind: "swap" });
  }, [playFire, react, setSkin]);

  /**
   * 分区单击：
   * - 双击（`DOUBLE_TAP_MS` 内两下，任何分区）：打开天玑，第二下不再演分区手势
   * - 睡着：先迷糊醒来
   * - 头：连摸（满 3 升级撒花、满 6 抗议）
   * - 裙边：`TAIL_SHY_WINDOW_MS` 内连戳 3 次害羞
   * - 身：戳一下
   *
   * 单击**不开**天玑（双击才开），否则"戳一下就弹窗抢焦点"，连戳类手势也全废。
   * 换形象只走右键菜单的「换个形象」，不绑手势。
   */
  const handleZoneTap = useCallback(
    (zone: HitZone) => {
      playFire();
      if (moodRef.current === "sleepy") {
        react({ kind: "wake" });
        headPatRef.current = { count: 0, at: 0 };
        tailTapRef.current = { count: 0, at: 0 };
        lastTapAtRef.current = 0;
        return;
      }
      const now = Date.now();
      // 快速双击 = 叫出天玑（窗口跟系统双击速度走；点到哪儿都算）
      if (now - lastTapAtRef.current < doubleTapMsRef.current) {
        lastTapAtRef.current = 0;
        void invoke("show_ai_hub").catch((err) => console.error(err));
        return;
      }
      lastTapAtRef.current = now;
      if (zone === "head") {
        // 连摸升级：窗口内计数交给 petBrain 判（满 3 撒花、满 6 抗议）
        if (now - headPatRef.current.at < PAT_WINDOW_MS) {
          headPatRef.current.count += 1;
        } else {
          headPatRef.current.count = 1;
        }
        headPatRef.current.at = now;
        const streak = headPatRef.current.count;
        react({ kind: "pat", streak });
        if (streak >= PAT_OVER_STREAK) headPatRef.current = { count: 0, at: 0 };
        return;
      }
      if (zone === "tail") {
        tailTapRef.current = {
          count:
            now - tailTapRef.current.at < TAIL_SHY_WINDOW_MS
              ? tailTapRef.current.count + 1
              : 1,
          at: now,
        };
        if (tailTapRef.current.count >= TAIL_SHY_STREAK) {
          tailTapRef.current = { count: 0, at: 0 };
          react({ kind: "shy" });
          return;
        }
        react({ kind: "tail" });
        return;
      }
      react({ kind: "poke", skin: cfgRef.current.skin });
    },
    [playFire, react],
  );

  const closeMenu = useCallback(() => {
    setMenuOpen(false);
    menuOpenRef.current = false;
    syncBox();
  }, [syncBox]);

  const toggleMenu = useCallback(() => {
    setMenuOpen((open) => {
      const next = !open;
      menuOpenRef.current = next;
      syncBox();
      return next;
    });
  }, [syncBox]);

  // 主窗改了好感（设置页的「重置」等）：桌宠窗手里的 ref 是挂载时的旧值，跟着盘上的走。
  useEffect(() => {
    const next = Number(settings.aiHubPetAffinity);
    if (!Number.isFinite(next) || next === affinityRef.current) return;
    affinityRef.current = next;
    setAffinity(next);
  }, [settings.aiHubPetAffinity]);

  /** 菜单的另外两条关法：窗口失焦（去点别的应用）与 Esc。 */
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let dead = false;
    void getCurrentWindow()
      .onFocusChanged(({ payload }) => {
        if (!payload) closeMenu();
      })
      .then((fn) => {
        if (dead) fn();
        else unlisten = fn;
      });
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeMenu();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      dead = true;
      unlisten?.();
      window.removeEventListener("keydown", onKey);
    };
  }, [closeMenu]);

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    if (menuOpen) {
      // 菜单开着时的第一下只负责收起，不顺手开窗。
      closeMenu();
      return;
    }
    const zone = hitZoneAt(e, rootRef.current);
    // capture 落在 root 上：它是整棵树的稳定节点（角色换图 / 淡入淡出都不会被替换），
    // 也是 pointer-events:auto 的，能保证拖到窗外也收得到 pointerup。
    // 存进 dragRef，松手时用同一个元素 release（否则会抛 NotFoundError）。
    const capture = e.currentTarget as HTMLElement;
    try {
      capture?.setPointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    const d: DragState = {
      startX: e.screenX,
      startY: e.screenY,
      moved: false,
      dragging: false,
      zone,
      hugged: false,
      lastX: e.screenX,
      lastY: e.screenY,
      lastT: performance.now(),
      shake: 0,
      dizzied: false,
      capture,
      origin: null,
      scale: 1,
      target: null,
      raf: 0,
    };
    dragRef.current = d;
    setPressZone(zone);
    setPhase("pressed");
    dragSaidRef.current = false;
    // 拖动几何先探一次：拖动开始前就拿到窗口物理坐标与 DPR，位移可以直接换算。
    void (async () => {
      try {
        const win = getCurrentWindow();
        const [pos, scale] = await Promise.all([win.outerPosition(), win.scaleFactor()]);
        // 已经拖起来了就以移动里的取值为准，别把起点覆盖成"当前位置"
        if (dragRef.current !== d || d.moved) return;
        d.origin = { x: pos.x, y: pos.y };
        d.scale = scale > 0 ? scale : 1;
      } catch {
        /* ignore */
      }
    })();
    // 身区长按 = 抱一下；头区长按 = 多摸一会儿（未拖动）
    clearTimer(hugTimerRef);
    if (zone === "body") {
      hugTimerRef.current = window.setTimeout(() => {
        hugTimerRef.current = null;
        const d = dragRef.current;
        if (!d || d.moved || d.dragging) return;
        d.hugged = true;
        react({ kind: "hug" });
      }, HUG_HOLD_MS);
    } else if (zone === "head") {
      hugTimerRef.current = window.setTimeout(() => {
        hugTimerRef.current = null;
        const d = dragRef.current;
        if (!d || d.moved || d.dragging) return;
        d.hugged = true;
        react({ kind: "pat" });
      }, HEAD_HOLD_MS);
    }
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
    // 倾角收一点：整只扭太狠会显得「永远斜着」；主要靠视线位移
    // 身体倾角再收一档；左右晃主要靠视线位移，别整只拧
    el.style.setProperty("--qa-tilt-x", (gx * 1.2 * k).toFixed(2));
    el.style.setProperty("--qa-tilt-y", (-gy * 1.0 * k).toFixed(2));
    el.style.setProperty("--qa-gaze-x", (gx * 3.2 * k).toFixed(2));
    el.style.setProperty("--qa-gaze-y", (gy * 2.2 * k).toFixed(2));
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

  /**
   * 拖动位移：`screenX/screenY` 是 CSS 像素，乘 DPR 才是物理像素；
   * 用 rAF 合并，一帧最多一条 `setPosition`。
   */
  const pushDragPosition = (d: DragState, x: number, y: number) => {
    d.target = { x, y };
    if (d.raf) return;
    d.raf = window.requestAnimationFrame(() => {
      d.raf = 0;
      if (dragRef.current !== d || !d.target) return;
      const target = d.target;
      void getCurrentWindow().setPosition(new PhysicalPosition(target.x, target.y)).catch(() => {});
    });
  };

  const onPointerMove = async (e: React.PointerEvent) => {
    aimAt(e);
    // 悬停摸头：进入角色的那一下由 Rust 的命中事件起头，之后在窗内从身上滑到头顶
    // 靠这里续上（穿透时窗外的移动收不到，所以两处都要）。
    if (hoverRef.current && !dragRef.current && !menuOpenRef.current) {
      if (hitZoneAt(e, rootRef.current) === "head") {
        if (hoverTimerRef.current == null) {
          hoverTimerRef.current = window.setTimeout(() => {
            hoverTimerRef.current = null;
            if (dragRef.current || menuOpenRef.current || !hoverRef.current) return;
            react({ kind: "pat" });
          }, 1100);
        }
      } else {
        clearTimer(hoverTimerRef);
      }
    }
    const d = dragRef.current;
    if (!d) return;
    const dx = e.screenX - d.startX;
    const dy = e.screenY - d.startY;
    if (!d.moved && Math.hypot(dx, dy) >= CLICK_MOVE_PX) {
      // 起点几何还没探到就先补一次，并把起点挪到当前指针：否则第一帧会跳一段。
      if (!d.origin) {
        try {
          const win = getCurrentWindow();
          const [pos, scale] = await Promise.all([win.outerPosition(), win.scaleFactor()]);
          if (dragRef.current !== d) return;
          d.origin = { x: pos.x, y: pos.y };
          d.scale = scale > 0 ? scale : 1;
          d.startX = e.screenX;
          d.startY = e.screenY;
        } catch {
          return;
        }
      }
      d.moved = true;
      d.dragging = true;
      clearTimer(hugTimerRef);
      setPhase("dragging");
      if (!dragSaidRef.current) {
        dragSaidRef.current = true;
        say(t(pickPoolKey(tierRef.current, "drag")));
      }
      // 拎久挣扎：拖拽开始起算，松手作废
      clearTimer(struggleTimerRef);
      struggleTimerRef.current = window.setTimeout(() => {
        struggleTimerRef.current = null;
        if (dragRef.current?.dragging) react({ kind: "struggle" });
      }, STRUGGLE_AFTER_MS);
      return;
    }
    if (!d.dragging || !d.origin) return;
    // 甩动检测：只在真的拖起来之后算，快速来回甩累计到阈值演头晕（一次拖拽只演一回）
    const now = performance.now();
    const dt = now - d.lastT;
    if (dt > 0) {
      const speed = Math.hypot(e.screenX - d.lastX, e.screenY - d.lastY) / dt;
      d.shake = speed > SHAKE_SPEED_PX_MS ? d.shake + 1 : Math.max(0, d.shake - 0.5);
      if (d.shake >= SHAKE_LIMIT && !d.dizzied) {
        d.dizzied = true;
        react({ kind: "dizzy" });
      }
    }
    d.lastX = e.screenX;
    d.lastY = e.screenY;
    d.lastT = now;
    pushDragPosition(
      d,
      d.origin.x + Math.round((e.screenX - d.startX) * d.scale),
      d.origin.y + Math.round((e.screenY - d.startY) * d.scale),
    );
  };

  const endPointer = async (e: React.PointerEvent, cancelled: boolean) => {
    const d = dragRef.current;
    dragRef.current = null;
    clearTimer(hugTimerRef);
    clearTimer(struggleTimerRef);
    try {
      d?.capture?.releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    // 丢掉还没落地的那一帧，别在吸附之后又把它挪回去
    if (d?.raf) {
      window.cancelAnimationFrame(d.raf);
      d.raf = 0;
    }
    setPressZone(null);
    if (!d || cancelled) {
      // 拖到一半被系统取消（窗口失焦等）：照样贴回边缘，别把窗留在半空。
      if (d?.dragging) void snapAndPersist(d.target ?? d.origin ?? undefined);
      setPhase("idle");
      return;
    }
    if (d.moved || d.dragging) {
      // 用最后一帧的目标坐标吸附：那一帧的 setPosition 可能还在 IPC 队列里。
      await snapAndPersist(d.target ?? d.origin ?? undefined);
      // 放手：落地弹一下 + 欢呼表情，再站定；被甩晕则保留头晕演出，
      // 不清 microHold 计时器——晕够时长由它自己收回
      setPhase("landing");
      if (!d.dizzied) {
        setMicro("bounce");
        clearTimer(microHoldTimerRef);
      }
      clearTimer(landTimerRef);
      landTimerRef.current = window.setTimeout(() => {
        setPhase("idle");
        setMicro("none");
        landTimerRef.current = null;
      }, 500);
      return;
    }
    // 已经触发过抱：松手不再当单击，也不开窗
    if (d.hugged) {
      setPhase("idle");
      return;
    }
    handleZoneTap(d.zone);
  };

  const runMenuItem = (action: () => void) => {
    action();
    closeMenu();
  };

  return (
    <div
      ref={rootRef}
      className={`qa-bubble-root is-${phase}${menuOpen ? " has-menu" : ""}${hot ? " is-hot" : ""}`}
      data-skin={cfg.skin}
      style={{ "--qa-bubble-size": `${cfg.size}px` } as React.CSSProperties}
      onPointerDown={onPointerDown}
      onPointerMove={(e) => void onPointerMove(e)}
      onPointerUp={(e) => void endPointer(e, false)}
      onPointerCancel={(e) => void endPointer(e, true)}
      onPointerEnter={(e) => {
        // 悬停态与摸头都由 Rust 的命中事件统一驱动（穿透期间这里收不到 enter/leave），
        // 这里只负责把本地指针位置喂给视线层，比全局方向更跟手。
        aimAt(e);
      }}
      onPointerLeave={() => {
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
        <PetStage
          skin={skinDef}
          mood={mood}
          phase={phase}
          bondTier={tier}
          gesture={gesture}
          micro={micro}
          celebrate={celebrate}
          pressZone={pressZone}
        />
      </div>
      {speech && !menuOpen && (
        <span
          className={`qa-pet-speech${speech.length > 10 ? " is-long" : ""}${speech.length > 18 ? " is-xl" : ""}`}
          role="status"
        >
          {speech}
        </span>
      )}
      {menuOpen && (
        <div
          className="qa-bubble-menu"
          role="menu"
          aria-label={t("title")}
          onPointerDown={(e) => e.stopPropagation()}
        >
          {/* 顺序：显示主窗口 → 打开天玑 → 换形象 → 安静 → 隐藏 → 设置（设置压最后） */}
          <button
            type="button"
            role="menuitem"
            onClick={() =>
              runMenuItem(() => void invoke("open_main").catch((err) => console.error(err)))
            }
          >
            {t("petMenuMain")}
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() =>
              runMenuItem(() => void invoke("show_ai_hub").catch((err) => console.error(err)))
            }
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
          <button
            type="button"
            role="menuitem"
            onClick={() =>
              runMenuItem(() => void invoke("open_main_tab", { tab: "settings" }).catch(() => {}))
            }
          >
            {t("petMenuSettings")}
          </button>
        </div>
      )}
    </div>
  );
}
