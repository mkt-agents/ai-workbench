/**
 * 桌宠好感度：话术池、腮红与撒花密度；不做仪表盘 UI。
 * 存 settings.aiHubPetAffinity（0–100）。
 */

export type BondTier = 0 | 1 | 2;

const TIER_LO = 30;
const TIER_HI = 70;

export function bondTier(affinity: number): BondTier {
  if (affinity >= TIER_HI) return 2;
  if (affinity >= TIER_LO) return 1;
  return 0;
}

export function clampAffinity(n: number): number {
  if (!Number.isFinite(n)) return 0;
  return Math.max(0, Math.min(100, Math.round(n)));
}

/** 每天首次见面可微衰减，下限 0。 */
export function dailyDecay(affinity: number, greetedOn: string | undefined, today: string): number {
  if (greetedOn === today) return affinity;
  return clampAffinity(affinity - 1);
}

export function applyAffinityDelta(affinity: number, delta: number): number {
  return clampAffinity(affinity + delta);
}

/** 按时段挑一句；pools 按好感档 [低, 中, 高]。 */
export function pickLine(tier: BondTier, pools: [string[], string[], string[]]): string {
  const pool = pools[tier];
  if (!pool.length) return pools[0][0] ?? "";
  return pool[Math.floor(Math.random() * pool.length)]!;
}

export function greetingSlot(date = new Date()): "morning" | "noon" | "night" {
  const h = date.getHours();
  if (h < 11) return "morning";
  if (h < 18) return "noon";
  return "night";
}

export function todayKey(date = new Date()): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

/** 撒花粒子数量：好感越高越多。 */
export function petalCount(tier: BondTier): number {
  return 4 + tier * 3;
}

/** i18n 键池：低 / 中 / 高好感。 */
export type SpeechPool = [string[], string[], string[]];

export const SPEECH_POOLS = {
  pat: [
    ["petPatLo1", "petPatLo2", "petPatLo3"],
    ["petPatMid1", "petPatMid2", "petPatMid3"],
    ["petPatHi1", "petPatHi2", "petPatHi3"],
  ],
  tail: [
    ["petTailLo1", "petTailLo2", "petTailLo3"],
    ["petTailMid1", "petTailMid2", "petTailMid3"],
    ["petTailHi1", "petTailHi2", "petTailHi3"],
  ],
  hug: [
    ["petHugLo1", "petHugLo2", "petHugLo3"],
    ["petHugMid1", "petHugMid2", "petHugMid3"],
    ["petHugHi1", "petHugHi2", "petHugHi3"],
  ],
  poke: [
    ["petPokeLo1", "petPokeLo2", "petPokeLo3"],
    ["petPokeMid1", "petPokeMid2", "petPokeMid3"],
    ["petPokeHi1", "petPokeHi2", "petPokeHi3"],
  ],
  shy: [
    ["petShyLo1", "petShyLo2"],
    ["petShyMid1", "petShyMid2"],
    ["petShyHi1", "petShyHi2"],
  ],
  wake: [
    ["petWakeLo1", "petWakeLo2"],
    ["petWakeMid1", "petWakeMid2"],
    ["petWakeHi1", "petWakeHi2"],
  ],
  waiting: [
    ["petWaitLo1", "petWaitLo2"],
    ["petWaitMid1", "petWaitMid2"],
    ["petWaitHi1", "petWaitHi2"],
  ],
  done: [
    ["petDoneLo1", "petDoneLo2"],
    ["petDoneMid1", "petDoneMid2"],
    ["petDoneHi1", "petDoneHi2"],
  ],
  mutterThink: [
    ["petMutterThinkLo1", "petMutterThinkLo2"],
    ["petMutterThinkMid1", "petMutterThinkMid2"],
    ["petMutterThinkHi1", "petMutterThinkHi2"],
  ],
  mutterWork: [
    ["petMutterWorkLo1", "petMutterWorkLo2"],
    ["petMutterWorkMid1", "petMutterWorkMid2"],
    ["petMutterWorkHi1", "petMutterWorkHi2"],
  ],
  chatter: [
    ["petChatterLo1", "petChatterLo2", "petChatterLo3", "petChatterLo4"],
    ["petChatterMid1", "petChatterMid2", "petChatterMid3", "petChatterMid4"],
    ["petChatterHi1", "petChatterHi2", "petChatterHi3", "petChatterHi4"],
  ],
  micro: [
    ["petMicroLo1", "petMicroLo2"],
    ["petMicroMid1", "petMicroMid2", "petMicroMid3"],
    ["petMicroHi1", "petMicroHi2", "petMicroHi3"],
  ],
  greetMorning: [
    ["petGreetMorningLo"],
    ["petGreetMorningMid"],
    ["petGreetMorningHi"],
  ],
  greetNoon: [
    ["petGreetNoonLo"],
    ["petGreetNoonMid"],
    ["petGreetNoonHi"],
  ],
  greetNight: [
    ["petGreetNightLo"],
    ["petGreetNightMid"],
    ["petGreetNightHi"],
  ],
  drag: [
    ["petDragLo1", "petDragLo2"],
    ["petDragMid1", "petDragMid2"],
    ["petDragHi1", "petDragHi2"],
  ],
  wecomStart: [
    ["petWecomStartLo1", "petWecomStartLo2"],
    ["petWecomStartMid1", "petWecomStartMid2"],
    ["petWecomStartHi1", "petWecomStartHi2"],
  ],
  wecomWait: [
    ["petWecomWaitLo1", "petWecomWaitLo2"],
    ["petWecomWaitMid1", "petWecomWaitMid2"],
    ["petWecomWaitHi1", "petWecomWaitHi2"],
  ],
  wecomDone: [
    ["petWecomDoneLo1", "petWecomDoneLo2"],
    ["petWecomDoneMid1", "petWecomDoneMid2"],
    ["petWecomDoneHi1", "petWecomDoneHi2"],
  ],
  wecomErr: [
    ["petWecomErrLo1", "petWecomErrLo2"],
    ["petWecomErrMid1", "petWecomErrMid2"],
    ["petWecomErrHi1", "petWecomErrHi2"],
  ],
} as const satisfies Record<string, SpeechPool>;

export type SpeechPoolName = keyof typeof SPEECH_POOLS;

export function pickPoolKey(tier: BondTier, pool: SpeechPoolName): string {
  return pickLine(tier, SPEECH_POOLS[pool] as SpeechPool);
}
