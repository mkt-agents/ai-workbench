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

/**
 * 每个池上一次取到的 key。池数组是模块级常量，用 WeakMap 记住「上一句」，
 * 取到同一条就顺移一格——连着蹦同一句最出戏（池里只有 1 条时自然不生效）。
 */
const lastPick = new WeakMap<string[], string>();

/** 按时段挑一句；pools 按好感档 [低, 中, 高]。 */
export function pickLine(tier: BondTier, pools: [string[], string[], string[]]): string {
  const pool = pools[tier].length ? pools[tier] : pools[0];
  if (!pool.length) return "";
  const prev = lastPick.get(pool);
  let pick = pool[Math.floor(Math.random() * pool.length)]!;
  if (pool.length > 1 && pick === prev) {
    pick = pool[(pool.indexOf(pick) + 1) % pool.length]!;
  }
  lastPick.set(pool, pick);
  return pick;
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
    ["petPatLo1", "petPatLo2", "petPatLo3", "petPatLo4", "petPatLo5"],
    ["petPatMid1", "petPatMid2", "petPatMid3", "petPatMid4", "petPatMid5"],
    ["petPatHi1", "petPatHi2", "petPatHi3", "petPatHi4", "petPatHi5"],
  ],
  tail: [
    ["petTailLo1", "petTailLo2", "petTailLo3", "petTailLo4", "petTailLo5"],
    ["petTailMid1", "petTailMid2", "petTailMid3", "petTailMid4", "petTailMid5"],
    ["petTailHi1", "petTailHi2", "petTailHi3", "petTailHi4", "petTailHi5"],
  ],
  hug: [
    ["petHugLo1", "petHugLo2", "petHugLo3", "petHugLo4", "petHugLo5"],
    ["petHugMid1", "petHugMid2", "petHugMid3", "petHugMid4", "petHugMid5"],
    ["petHugHi1", "petHugHi2", "petHugHi3", "petHugHi4", "petHugHi5"],
  ],
  poke: [
    ["petPokeLo1", "petPokeLo2", "petPokeLo3", "petPokeLo4", "petPokeLo5"],
    ["petPokeMid1", "petPokeMid2", "petPokeMid3", "petPokeMid4", "petPokeMid5"],
    ["petPokeHi1", "petPokeHi2", "petPokeHi3", "petPokeHi4", "petPokeHi5"],
  ],
  shy: [
    ["petShyLo1", "petShyLo2", "petShyLo3", "petShyLo4"],
    ["petShyMid1", "petShyMid2", "petShyMid3", "petShyMid4"],
    ["petShyHi1", "petShyHi2", "petShyHi3", "petShyHi4"],
  ],
  wake: [
    ["petWakeLo1", "petWakeLo2", "petWakeLo3", "petWakeLo4"],
    ["petWakeMid1", "petWakeMid2", "petWakeMid3", "petWakeMid4"],
    ["petWakeHi1", "petWakeHi2", "petWakeHi3", "petWakeHi4"],
  ],
  waiting: [
    ["petWaitLo1", "petWaitLo2", "petWaitLo3", "petWaitLo4"],
    ["petWaitMid1", "petWaitMid2", "petWaitMid3", "petWaitMid4"],
    ["petWaitHi1", "petWaitHi2", "petWaitHi3", "petWaitHi4"],
  ],
  done: [
    ["petDoneLo1", "petDoneLo2", "petDoneLo3", "petDoneLo4"],
    ["petDoneMid1", "petDoneMid2", "petDoneMid3", "petDoneMid4"],
    ["petDoneHi1", "petDoneHi2", "petDoneHi3", "petDoneHi4"],
  ],
  mutterThink: [
    ["petMutterThinkLo1", "petMutterThinkLo2", "petMutterThinkLo3"],
    ["petMutterThinkMid1", "petMutterThinkMid2", "petMutterThinkMid3"],
    ["petMutterThinkHi1", "petMutterThinkHi2", "petMutterThinkHi3"],
  ],
  mutterWork: [
    ["petMutterWorkLo1", "petMutterWorkLo2", "petMutterWorkLo3"],
    ["petMutterWorkMid1", "petMutterWorkMid2", "petMutterWorkMid3"],
    ["petMutterWorkHi1", "petMutterWorkHi2", "petMutterWorkHi3"],
  ],
  chatter: [
    ["petChatterLo1", "petChatterLo2", "petChatterLo3", "petChatterLo4", "petChatterLo5", "petChatterLo6"],
    ["petChatterMid1", "petChatterMid2", "petChatterMid3", "petChatterMid4", "petChatterMid5", "petChatterMid6"],
    ["petChatterHi1", "petChatterHi2", "petChatterHi3", "petChatterHi4", "petChatterHi5", "petChatterHi6"],
  ],
  chatterCode: [
    ["petChatterCodeLo1", "petChatterCodeLo2", "petChatterCodeLo3", "petChatterCodeLo4"],
    ["petChatterCodeMid1", "petChatterCodeMid2", "petChatterCodeMid3", "petChatterCodeMid4"],
    ["petChatterCodeHi1", "petChatterCodeHi2", "petChatterCodeHi3", "petChatterCodeHi4"],
  ],
  chatterWork: [
    ["petChatterWorkLo1", "petChatterWorkLo2", "petChatterWorkLo3", "petChatterWorkLo4"],
    ["petChatterWorkMid1", "petChatterWorkMid2", "petChatterWorkMid3", "petChatterWorkMid4"],
    ["petChatterWorkHi1", "petChatterWorkHi2", "petChatterWorkHi3", "petChatterWorkHi4"],
  ],
  micro: [
    ["petMicroLo1", "petMicroLo2", "petMicroLo3"],
    ["petMicroMid1", "petMicroMid2", "petMicroMid3", "petMicroMid4"],
    ["petMicroHi1", "petMicroHi2", "petMicroHi3", "petMicroHi4"],
  ],
  greetMorning: [
    ["petGreetMorningLo", "petGreetMorningLo2"],
    ["petGreetMorningMid", "petGreetMorningMid2"],
    ["petGreetMorningHi", "petGreetMorningHi2"],
  ],
  greetNoon: [
    ["petGreetNoonLo", "petGreetNoonLo2"],
    ["petGreetNoonMid", "petGreetNoonMid2"],
    ["petGreetNoonHi", "petGreetNoonHi2"],
  ],
  greetNight: [
    ["petGreetNightLo", "petGreetNightLo2"],
    ["petGreetNightMid", "petGreetNightMid2"],
    ["petGreetNightHi", "petGreetNightHi2"],
  ],
  drag: [
    ["petDragLo1", "petDragLo2", "petDragLo3", "petDragLo4"],
    ["petDragMid1", "petDragMid2", "petDragMid3", "petDragMid4"],
    ["petDragHi1", "petDragHi2", "petDragHi3", "petDragHi4"],
  ],
  /** 好感升档（0→1→2）：一次性庆祝，不走日常池。 */
  bondUp: [
    ["petBondUpLo1", "petBondUpLo2"],
    ["petBondUpMid1", "petBondUpMid2"],
    ["petBondUpHi1", "petBondUpHi2"],
  ],
  /** 离开一阵子回来的第一句。 */
  return: [
    ["petReturnLo1", "petReturnLo2", "petReturnLo3"],
    ["petReturnMid1", "petReturnMid2", "petReturnMid3"],
    ["petReturnHi1", "petReturnHi2", "petReturnHi3"],
  ],
  /** 被拎太久挣扎。 */
  struggle: [
    ["petStruggleLo1", "petStruggleLo2", "petStruggleLo3"],
    ["petStruggleMid1", "petStruggleMid2", "petStruggleMid3"],
    ["petStruggleHi1", "petStruggleHi2", "petStruggleHi3"],
  ],
  /** 被甩晕。 */
  dizzy: [
    ["petDizzyLo1", "petDizzyLo2", "petDizzyLo3"],
    ["petDizzyMid1", "petDizzyMid2", "petDizzyMid3"],
    ["petDizzyHi1", "petDizzyHi2", "petDizzyHi3"],
  ],
  /** 摸头过头抗议。 */
  overpet: [
    ["petOverpetLo1", "petOverpetLo2"],
    ["petOverpetMid1", "petOverpetMid2"],
    ["petOverpetHi1", "petOverpetHi2"],
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
