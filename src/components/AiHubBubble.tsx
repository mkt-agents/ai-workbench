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
/**
 * 单击后等这么久没有第二击才打开天玑（ms）。双击本身以原生 dblclick 为准（跟系统
 * 双击速度走，默认 500ms）；这里定得太短，正常速度的双击第二下就会落在窗外，
 * 先把天玑开出来抢走焦点。
 */
const DOUBLE_CLICK_MS = 400;
/** 气泡语最短 / 最长停留（按字数伸缩）。 */
const SPEECH_MS_MIN = 1600;
const SPEECH_MS_MAX = 3200;
/** 头顶长按多久算「多摸一会儿」。 */
const HEAD_HOLD_MS = 600;
/** 等审批时再次催一声的间隔。 */
const WAIT_NUDGE_MS = 45_000;
/** 就地菜单展开时临时撑高（逻辑 px）；菜单贴在桌宠头顶，底边不动（Rust 同规则）。
 *  高度要盖住 5 个菜单项：5×22 + 4×2 间隙 + 10 padding + 2 边框 + 4 缝隙 = 136。 */
const PET_MENU_EXTRA = 136;
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
/** 连续戳身算害羞的时间窗。 */
const SHY_WINDOW_MS = 2500;
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
  const dragRef = useRef<{
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
  } | null>(null);
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
  const clickTimerRef = useRef<number | null>(null);
  const affinityRef = useRef(affinity);
  affinityRef.current = affinity;
  const greetedOnRef = useRef(settings.aiHubPetGreetedOn);
  /** 指针倾角：直接写 CSS 变量，不走 state，鼠标划过时不用重渲。 */
  const rootRef = useRef<HTMLDivElement | null>(null);
  /** 最近一次视线方向（-1..1）：全局光标与窗内指针共用一台"眼睛"。 */
  const gazeRef = useRef({ x: 0, y: 0 });
  /** 上一次真正写进 CSS 的值：70ms 一条的全局视线不该每次都触发样式重算。 */
  const gazeKeyRef = useRef("");
  /** 指针是否停在窗里：在窗里就以本地精确值为准，离开后才吃全局方向。 */
  const hoverRef = useRef(false);
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
  /** 身区连戳：2.5s 内满 3 次起害羞。 */
  const bodyTapRef = useRef({ count: 0, at: 0 });
  /** 头区连摸：PAT_WINDOW_MS 内计数，满 3 升级开心、满 6 抗议。 */
  const headPatRef = useRef({ count: 0, at: 0 });
  const mutterAtRef = useRef(0);
  const workStartedAtRef = useRef(0);
  const tierRef = useRef<BondTier>(0);
  /** 拎久挣扎计时（拖拽开始起算，松手作废）。 */
  const struggleTimerRef = useRef<number | null>(null);
  /** 好感升档庆祝（升档当次反应演完再接一场）。 */
  const bondUpTimerRef = useRef<number | null>(null);
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

  const pushBox = useCallback((width: number, height: number) => {
    void invoke("set_ai_hub_bubble_box", { width, height }).catch(() => {});
  }, []);

  /** 常态 = 形象 + 头顶留白；菜单再临时撑高。说话绝不改尺寸。 */
  const syncBox = useCallback(() => {
    const size = cfgRef.current.size;
    const w = Math.max(size, PET_BOX_MIN_W);
    if (menuOpenRef.current) {
      pushBox(w, size + PET_MENU_EXTRA);
      return;
    }
    pushBox(w, size + PET_SPEECH_HEADROOM);
  }, [pushBox]);
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
        // 好感跨档：当次反应演完再接一场撒花庆祝，不与当前台词抢帧
        if (bondTier(affinityRef.current) > prevTier) {
          clearTimer(bondUpTimerRef);
          bondUpTimerRef.current = window.setTimeout(() => {
            bondUpTimerRef.current = null;
            play(reactTo({ kind: "bondUp" }, tierRef.current));
          }, 1800);
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
   * 到点渐进犯困：先哈欠揉眼（chain）再换 sleepy 图。
   * 起手时正忙（说话/被按住/菜单开着）链会自己作废，这里 20s 后重试，
   * 别因为撞上一次旁白就永远醒着；已经睡着或来了新信号则不再排。
   */
  const armSleepTimer = useCallback(
    (delay = sleepAfterMs()) => {
      clearTimer(sleepTimerRef);
      sleepTimerRef.current = window.setTimeout(() => {
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

  /** 收到任何信号都算"桌面有动静"：重新计打瞌睡倒计时，再演对应情绪。 */
  const react = useCallback(
    (signal: PetSignal) => {
      const now = Date.now();
      const away = now - lastSeenAtRef.current;
      lastSeenAtRef.current = now;
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
      if (prev !== today && !cfgRef.current.quiet) {
        greetedOnRef.current = today;
        setSettings({ aiHubPetGreetedOn: today });
        window.setTimeout(() => play(reactTo({ kind: "greet", slot: greetingSlot() })), 600);
      }

      // 待机碎碎念（按好感分档）；thinking/working 偶尔也冒一句
      chatterTimer = window.setInterval(() => {
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

    return () => {
      unlistenTheme?.();
      unlistenCfg?.();
      unlistenGaze?.();
      unlistenTrack?.();
      window.clearInterval(chatterTimer);
      clearTimer(struggleTimerRef);
      clearTimer(bondUpTimerRef);
      clearTimer(fireTimerRef);
      clearTimer(landTimerRef);
      clearTimer(hugTimerRef);
      clearTimer(gestureTimerRef);
      clearTimer(microTimerRef);
      clearTimer(microHoldTimerRef);
      clearTimer(hoverTimerRef);
      clearTimer(clickTimerRef);
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

  /** 换装：熊猫娘 → 圆盘 →（有素材时）自备包，循环 */
  const cyclePet = useCallback(() => {
    setSkin(nextPetSkin(cfgRef.current.skin));
    playFire();
    react({ kind: "swap" });
  }, [playFire, react, setSkin]);

  /**
   * 分区单击：
   * - 睡着：先迷糊醒来，不开天玑
   * - 头 / 裙边：只互动
   * - 身：连戳满 3 次害羞；否则戳一下后延迟开天玑
   */
  const handleZoneTap = useCallback(
    (zone: HitZone) => {
      playFire();
      if (moodRef.current === "sleepy") {
        react({ kind: "wake" });
        bodyTapRef.current = { count: 0, at: 0 };
        headPatRef.current = { count: 0, at: 0 };
        return;
      }
      if (zone === "head") {
        // 连摸升级：窗口内计数交给 petBrain 判（满 3 撒花、满 6 抗议）
        const now = Date.now();
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
        react({ kind: "tail" });
        return;
      }
      const now = Date.now();
      if (now - bodyTapRef.current.at < SHY_WINDOW_MS) {
        bodyTapRef.current.count += 1;
      } else {
        bodyTapRef.current.count = 1;
      }
      bodyTapRef.current.at = now;
      if (bodyTapRef.current.count >= 3) {
        react({ kind: "shy" });
        bodyTapRef.current = { count: 0, at: 0 };
        return;
      }
      react({ kind: "poke", skin: cfgRef.current.skin });
      if (clickTimerRef.current != null) return;
      clickTimerRef.current = window.setTimeout(() => {
        clickTimerRef.current = null;
        void invoke("show_ai_hub").catch((err) => console.error(err));
      }, DOUBLE_CLICK_MS);
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

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    if (menuOpen) {
      // 菜单开着时的第一下只负责收起，不顺手开窗。
      closeMenu();
      return;
    }
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    const zone = hitZoneAt(e, rootRef.current);
    dragRef.current = {
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
    };
    setPressZone(zone);
    setPhase("pressed");
    dragSaidRef.current = false;
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

  const onPointerMove = async (e: React.PointerEvent) => {
    aimAt(e);
    // 悬停摸头：从身上滑到头顶也要能触发（不只 pointerEnter）
    if (hoverRef.current && !dragRef.current) {
      const zone = hitZoneAt(e, rootRef.current);
      if (zone === "head") {
        if (hoverTimerRef.current == null) {
          hoverTimerRef.current = window.setTimeout(() => {
            hoverTimerRef.current = null;
            if (dragRef.current || menuOpenRef.current) return;
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
      try {
        await getCurrentWindow().startDragging();
      } catch {
        /* ignore */
      }
      return;
    }
    // 甩动检测：只在真的拖起来之后算，快速来回甩累计到阈值演头晕（一次拖拽只演一回）
    if (d.dragging) {
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
    }
  };

  const endPointer = async (e: React.PointerEvent, cancelled: boolean) => {
    const d = dragRef.current;
    dragRef.current = null;
    clearTimer(hugTimerRef);
    clearTimer(struggleTimerRef);
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    setPressZone(null);
    if (!d || cancelled) {
      setPhase("idle");
      return;
    }
    if (d.moved || d.dragging) {
      await snapAndPersist();
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
      className={`qa-bubble-root is-${phase}${menuOpen ? " has-menu" : ""}`}
      data-skin={cfg.skin}
      style={{ "--qa-bubble-size": `${cfg.size}px` } as React.CSSProperties}
      onPointerDown={onPointerDown}
      onPointerMove={(e) => void onPointerMove(e)}
      onPointerUp={(e) => void endPointer(e, false)}
      onPointerCancel={(e) => void endPointer(e, true)}
      onPointerEnter={(e) => {
        hoverRef.current = true;
        // 摸头：停在头区够 1.1 秒才高兴，扫过去不算
        clearTimer(hoverTimerRef);
        const zone = hitZoneAt(e, rootRef.current);
        if (zone !== "head") return;
        hoverTimerRef.current = window.setTimeout(() => {
          hoverTimerRef.current = null;
          if (dragRef.current || menuOpenRef.current) return;
          react({ kind: "pat" });
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
