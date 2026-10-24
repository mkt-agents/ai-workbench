import type { PetSkinId } from "../core/types";
import type { PetMood } from "./petSkins";

/**
 * 天玑那边发生的事 → 桌宠的演出指令。桌宠窗和主窗是两个 webview，但后端
 * `app.emit` 是广播，所以这里只吃事件、不需要任何新命令。
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
  | { kind: "swap" }
  | { kind: "idle" };

export interface PetReaction {
  mood: PetMood;
  /** >0：演完自己回 idle；0：停在这个情绪，等下一条信号 */
  holdMs: number;
  /** aihub 命名空间的话术键；undefined = 只做动作不说话 */
  speechKey?: string;
  speechParams?: Record<string, string | number>;
}

/** 一轮跑多久才算"干完活"：短答案不值得撒花，免得每次都跳。 */
const CELEBRATE_FROM_MS = 5000;

export function reactTo(signal: PetSignal): PetReaction {
  switch (signal.kind) {
    case "approval":
      return { mood: "waiting", holdMs: 0, speechKey: "petSayWaiting" };
    case "plan":
      return { mood: "thinking", holdMs: 0 };
    case "stream":
      return { mood: "thinking", holdMs: 0 };
    case "step":
      if (signal.status === "error") {
        return { mood: "error", holdMs: 2200, speechKey: "petSayStepError" };
      }
      if (signal.status === "denied") {
        return { mood: "error", holdMs: 1600, speechKey: "petSayDenied" };
      }
      if (signal.status === "conflict") {
        return { mood: "waiting", holdMs: 0, speechKey: "petSayConflict" };
      }
      if (signal.status === "running" || signal.status === "retry") {
        return { mood: "working", holdMs: 0 };
      }
      return { mood: "working", holdMs: 0 };
    case "done": {
      if (signal.elapsedMs >= CELEBRATE_FROM_MS) {
        const sec = Math.max(1, Math.round(signal.elapsedMs / 1000));
        return { mood: "happy", holdMs: 1800, speechKey: "petSayDone", speechParams: { sec } };
      }
      return { mood: "idle", holdMs: 0 };
    }
    case "limited":
      // 回合数用完：这轮不会跑完，桌宠要一副"卡住了"的样子，而不是撒花。
      return { mood: "error", holdMs: 2400, speechKey: "petSayLimited" };
    case "error":
      return { mood: "error", holdMs: 2400, speechKey: "petSayError" };
    case "tap":
      return { mood: "happy", holdMs: 1200, speechKey: speechForSkin(signal.skin) };
    case "swap":
      return { mood: "happy", holdMs: 900, speechKey: "bubblePetSwap" };
    case "idle":
      return { mood: "sleepy", holdMs: 0 };
  }
}

/** 每只宠物被摸一下说的是自己的话（文案键在 aihub 命名空间，按皮肤 id 拼）。 */
function speechForSkin(skin: PetSkinId): string {
  return `bubblePet${skin.charAt(0).toUpperCase()}${skin.slice(1)}`;
}
