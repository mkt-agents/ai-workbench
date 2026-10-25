import type { PetSkinId } from "../core/types";
import type { BondTier, SpeechPoolName } from "./petBond";
import { pickPoolKey } from "./petBond";
import type { PetMood } from "./petSkins";

/**
 * 天玑那边发生的事 → 桌宠的演出指令。桌宠窗和主窗是两个 webview，但后端
 * `app.emit` 是广播，所以这里只吃事件、不需要任何新命令。
 *
 * 分区互动：摸头 / 戳身 / 裙边 / 抱一下；好感档决定话术池。
 */
export type PetSignal =
  | { kind: "step"; status: string }
  | { kind: "plan" }
  | { kind: "approval" }
  | { kind: "stream" }
  | { kind: "done"; elapsedMs: number }
  | { kind: "limited" }
  | { kind: "error" }
  | { kind: "tap"; skin: PetSkinId }
  | { kind: "pat"; streak?: number }
  | { kind: "poke"; skin: PetSkinId }
  | { kind: "tail" }
  | { kind: "hug" }
  | { kind: "wake" }
  | { kind: "shy" }
  | { kind: "mutter"; topic: "think" | "work" }
  | { kind: "greet"; slot: "morning" | "noon" | "night" }
  | { kind: "swap" }
  | { kind: "idle" }
  | { kind: "struggle" }
  | { kind: "dizzy" }
  | { kind: "return" }
  | { kind: "bondUp" };

export type PetGesturePlay = "pat" | "poke" | "tail" | "hug" | "shy";

export interface PetReaction {
  mood: PetMood;
  /** >0：演完自己回 idle；0：停在这个情绪，等下一条信号 */
  holdMs: number;
  /** aihub 命名空间的话术键；undefined = 只做动作不说话 */
  speechKey?: string;
  speechParams?: Record<string, string | number>;
  /** 好感增减；由 AiHubBubble 写入 settings */
  affinityDelta?: number;
  /** 矢量宠的短手势演出 */
  gesture?: PetGesturePlay;
  /** 完成庆祝：撒花 */
  celebrate?: boolean;
  /** idle 微动作（睡醒伸懒腰等） */
  micro?:
    | "stretch"
    | "shy"
    | "nod"
    | "wave"
    | "lookaround"
    | "wobble"
    | "hop"
    | "dust"
    | "dizzy"
    | "struggle";
}

/** 一轮跑多久才算"干完活"：短答案不值得撒花，免得每次都跳。 */
const CELEBRATE_FROM_MS = 5000;

/** 连摸头：窗口内满 3 次升级开心，满 6 次抗议（计数在 AiHubBubble 里维护）。 */
export const PAT_HOT_STREAK = 3;
export const PAT_OVER_STREAK = 6;

function pool(tier: BondTier, name: SpeechPoolName): string {
  return pickPoolKey(tier, name);
}

export function reactTo(signal: PetSignal, tier: BondTier = 0): PetReaction {
  switch (signal.kind) {
    case "approval":
      return { mood: "waiting", holdMs: 0, speechKey: pool(tier, "waiting") };
    case "plan":
      return { mood: "thinking", holdMs: 0 };
    case "stream":
      return { mood: "thinking", holdMs: 0 };
    case "step":
      if (signal.status === "error") {
        return { mood: "error", holdMs: 2200, speechKey: "petSayStepError" };
      }
      if (signal.status === "denied") {
        return { mood: "error", holdMs: 1600, speechKey: "petSayDenied", affinityDelta: -1 };
      }
      if (signal.status === "conflict") {
        return { mood: "waiting", holdMs: 0, speechKey: "petSayConflict" };
      }
      if (signal.status === "running" || signal.status === "retry") {
        return { mood: "working", holdMs: 0 };
      }
      return { mood: "working", holdMs: 0 };
    case "done": {
      const sec = Math.max(1, Math.round(signal.elapsedMs / 1000));
      if (signal.elapsedMs >= CELEBRATE_FROM_MS) {
        return {
          mood: "happy",
          holdMs: 1800,
          speechKey: pool(tier, "done"),
          speechParams: { sec },
          affinityDelta: 2,
          celebrate: true,
        };
      }
      // 短答也点头回一声，别突然变木桩
      return {
        mood: "happy",
        holdMs: 900,
        speechKey: pool(tier, "done"),
        speechParams: { sec },
        micro: "nod",
      };
    }
    case "limited":
      return { mood: "error", holdMs: 2400, speechKey: "petSayLimited" };
    case "error":
      return { mood: "error", holdMs: 2400, speechKey: "petSayError" };
    case "tap":
      return {
        mood: "happy",
        holdMs: 1200,
        speechKey: speechForSkin(signal.skin),
        affinityDelta: 1,
      };
    case "pat": {
      const streak = signal.streak ?? 0;
      if (streak >= PAT_OVER_STREAK) {
        // 摸过头：害羞抗议，好感只给一点
        return {
          mood: "happy",
          holdMs: 1300,
          speechKey: pool(tier, "overpet"),
          affinityDelta: 1,
          gesture: "shy",
          micro: "shy",
        };
      }
      if (streak >= PAT_HOT_STREAK) {
        // 连摸升级：在普通摸头之上加撒花
        return {
          mood: "happy",
          holdMs: 1400,
          speechKey: pool(tier, "pat"),
          affinityDelta: 2,
          gesture: "pat",
          celebrate: true,
          micro: "nod",
        };
      }
      return {
        mood: "happy",
        holdMs: 1100,
        speechKey: pool(tier, "pat"),
        affinityDelta: 2,
        gesture: "pat",
      };
    }
    case "poke":
      return {
        mood: "happy",
        holdMs: 1000,
        speechKey: pool(tier, "poke"),
        affinityDelta: 1,
        gesture: "poke",
      };
    case "tail":
      return {
        mood: "happy",
        holdMs: 1100,
        speechKey: pool(tier, "tail"),
        affinityDelta: 2,
        gesture: "tail",
      };
    case "hug":
      return {
        mood: "happy",
        holdMs: 1400,
        speechKey: pool(tier, "hug"),
        affinityDelta: 3,
        gesture: "hug",
        celebrate: true,
      };
    case "wake":
      return {
        mood: "happy",
        holdMs: 1400,
        speechKey: pool(tier, "wake"),
        affinityDelta: 1,
        micro: "stretch",
      };
    case "shy":
      return {
        mood: "happy",
        holdMs: 1200,
        speechKey: pool(tier, "shy"),
        affinityDelta: 1,
        gesture: "shy",
        micro: "shy",
      };
    case "mutter":
      return {
        mood: signal.topic === "work" ? "working" : "thinking",
        holdMs: 0,
        speechKey: pool(tier, signal.topic === "work" ? "mutterWork" : "mutterThink"),
      };
    case "greet":
      return {
        mood: "happy",
        holdMs: 1600,
        speechKey: pool(
          tier,
          signal.slot === "morning"
            ? "greetMorning"
            : signal.slot === "noon"
              ? "greetNoon"
              : "greetNight",
        ),
        affinityDelta: 1,
      };
    case "swap":
      return { mood: "happy", holdMs: 900, speechKey: "bubblePetSwap" };
    case "idle":
      return { mood: "sleepy", holdMs: 0 };
    case "struggle":
      // 被拎太久：挣扎扭动 + 抗议；演完回粘性情绪
      return {
        mood: "waiting",
        holdMs: 1800,
        speechKey: pool(tier, "struggle"),
        micro: "struggle",
      };
    case "dizzy":
      // 被甩晕：error 图 + 摇晃 + 星星，晕一会儿再回
      return {
        mood: "error",
        holdMs: 2400,
        speechKey: pool(tier, "dizzy"),
        micro: "dizzy",
      };
    case "return":
      // 离开一阵子回来：招手问好
      return {
        mood: "happy",
        holdMs: 1800,
        speechKey: pool(tier, "return"),
        affinityDelta: 1,
        micro: "wave",
      };
    case "bondUp":
      // 好感升档：撒花庆祝
      return {
        mood: "happy",
        holdMs: 2200,
        speechKey: pool(tier, "bondUp"),
        celebrate: true,
        micro: "hop",
      };
  }
}

function speechForSkin(skin: PetSkinId): string {
  return `bubblePet${skin.charAt(0).toUpperCase()}${skin.slice(1)}`;
}
