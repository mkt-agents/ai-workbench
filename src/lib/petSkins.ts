import { invoke } from "@tauri-apps/api/core";
import type { PetSkinId } from "../core/types";

/**
 * 桌宠情绪：状态机只有这七个取值。帧清单（本文件）与演出层（PetStage 的 FX、
 * ai-hub-bubble.css 的 mood-* 规则）全部按这里的名字对齐，改名要一起改。
 *
 * 默认皮肤 id 仍叫 `panda`（兼容旧存档），画面是原创 PNG「熊猫娘」
 *（`PandaGirlPet`）：行为对齐大肥鱼互动节奏，素材不搬 dsh-pet。
 */
export type PetMood =
  | "idle"
  | "thinking"
  | "working"
  | "waiting"
  | "happy"
  | "error"
  | "sleepy";

export const PET_MOODS: PetMood[] = [
  "idle",
  "thinking",
  "working",
  "waiting",
  "happy",
  "error",
  "sleepy",
];

/** 交互片段：被拎起与被点按不是情绪，是 phase 轴上的临时演出。 */
export type PetGesture = "drag" | "tap";

export const PET_GESTURES: PetGesture[] = ["drag", "tap"];

/** 自备素材包的皮肤 id：帧不入库，装在应用数据目录里（见 pet_pack_commands.rs）。 */
export const CUSTOM_SKIN: PetSkinId = "custom";

/**
 * 默认形象 id（存档键）。UI 文案是「熊猫娘」；渲染走 `kind: "vector"`（PandaGirlPet PNG）。
 */
export const PANDA_SKIN: PetSkinId = "panda";

/** 换装循环顺序：双击、菜单「换个形象」都按它走。 */
export const PET_ORDER: PetSkinId[] = [PANDA_SKIN, "orb", CUSTOM_SKIN];

export interface PetClip {
  frames: string[];
  /** 0 = 交给素材自己播（视频与 GIF） */
  fps: number;
  /** 一段带 alpha 的 WebM，不是一帧帧静图 */
  video: boolean;
}

export interface PetSkinDef {
  id: PetSkinId;
  /** sprite = 播素材清单；orb = 内置水晶圆盘；vector = 分层 SVG（小肥鱼） */
  kind: "sprite" | "orb" | "vector";
  clips: Partial<Record<PetMood, PetClip>>;
  gestures: Partial<Record<PetGesture, PetClip>>;
  /**
   * 眨眼序列（半闭 → 闭 → 半闭）。只用于 sprite；vector 自己在组件里随机眨。
   */
  blink?: string[];
}

const VIDEO_EXT = /\.(webm|mp4|mov)$/i;

/** 文件名里的情绪段：`idle.png`、`idle-2.png`、`thinking.webm` 都只认到第一个分隔符前。 */
function stemOf(name: string): string {
  return name.replace(/\.[a-z0-9]+$/i, "").replace(/[-_].*$/, "");
}

function toClip(files: { name: string; url: string }[]): PetClip | undefined {
  if (!files.length) return undefined;
  const sorted = [...files].sort((a, b) =>
    a.name.localeCompare(b.name, "en", { numeric: true }),
  );
  const frames = sorted.map((f) => f.url);
  const video = VIDEO_EXT.test(frames[0]);
  return { frames, fps: video ? 0 : frames.length > 6 ? 12 : 8, video };
}

/** 一堆文件按情绪名分桶；认不出的名字直接丢掉。 */
function bucketize(files: { name: string; url: string }[]) {
  const byName = new Map<string, { name: string; url: string }[]>();
  for (const f of files) {
    const key = stemOf(f.name);
    const list = byName.get(key);
    if (list) list.push(f);
    else byName.set(key, [f]);
  }
  const clips: Partial<Record<PetMood, PetClip>> = {};
  for (const mood of PET_MOODS) {
    const clip = toClip(byName.get(mood) ?? []);
    if (clip) clips[mood] = clip;
  }
  const gestures: Partial<Record<PetGesture, PetClip>> = {};
  for (const name of PET_GESTURES) {
    const clip = toClip(byName.get(name) ?? []);
    if (clip) gestures[name] = clip;
  }
  return { clips, gestures };
}

const ORB_DEF: PetSkinDef = { id: "orb", kind: "orb", clips: {}, gestures: {} };

/** 默认小肥鱼：分层矢量，不吃 PNG 节拍表。 */
const FISH_DEF: PetSkinDef = {
  id: PANDA_SKIN,
  kind: "vector",
  clips: {},
  gestures: {},
};

/** 自备包：内存里的一份镜像，由 loadUserPack 刷新。 */
let pack = bucketize([]);

/** 应用数据目录里的 `<名字>.png|webp|gif|webm` → data URL。 */
interface PackFile {
  name: string;
  dataUrl: string;
}

/**
 * 读自备素材包（桌宠窗开场、以及主窗改完素材后要再读一次），返回帧数。
 * 目录不存在或命令失败都当"还没有素材"，让皮肤退回圆盘，而不是抛错刷屏。
 */
export async function loadUserPack(): Promise<number> {
  let files: PackFile[] = [];
  try {
    files = await invoke<PackFile[]>("load_pet_pack");
  } catch {
    files = [];
  }
  pack = bucketize(files.map((f) => ({ name: f.name, url: f.dataUrl })));
  return files.length;
}

function hasUserPack(): boolean {
  return Boolean(pack.clips.idle);
}

export function petSkin(id: PetSkinId): PetSkinDef {
  if (id === "orb") return ORB_DEF;
  if (id === PANDA_SKIN) return FISH_DEF;
  // 自备包还没放素材：用内置小肥鱼顶上，而不是留一个看不见的窗
  if (!pack.clips.idle) return FISH_DEF;
  return { id, kind: "sprite", clips: pack.clips, gestures: pack.gestures };
}

/** 取某情绪的素材；缺则退回 idle。圆盘不吃素材。 */
export function petClipFor(def: PetSkinDef, mood: PetMood): PetClip | undefined {
  if (def.kind !== "sprite") return undefined;
  return def.clips[mood] ?? def.clips.idle;
}

/** 取交互片段（被拎起 / 被点按）；没配就交给情绪素材继续演。 */
export function petGestureFor(def: PetSkinDef, name: PetGesture): PetClip | undefined {
  return def.kind !== "sprite" ? undefined : def.gestures[name];
}

/** 换装只在"有素材的自定义包 + 圆盘"之间循环：空包不该占一格。 */
export function nextPetSkin(id: PetSkinId): PetSkinId {
  const pool = PET_ORDER.filter((s) => s !== CUSTOM_SKIN || hasUserPack());
  const at = pool.indexOf(id);
  return pool[(at + 1) % Math.max(1, pool.length)] ?? "orb";
}

/** 桌宠设置：皮肤 / 边长档位 / 安静模式。主窗写，桌宠窗读。 */
export interface PetConfig {
  skin: PetSkinId;
  /** 窗口边长（逻辑 px），同时也是 Rust 侧桌宠窗的尺寸 */
  size: number;
  quiet: boolean;
}

/** 与 tray/bubble.rs 的 BUBBLE_BOX_MIN / MAX_W 保持一致。 */
export const PET_SIZES = [96, 112, 128, 160] as const;
export const PET_SIZE_DEFAULT = 112;

/**
 * 主窗改完设置（含换装、导入素材）广播给桌宠窗（同 app-theme-changed 的做法）。
 * packRev 变了，桌宠窗才去重读自备包。
 */
export const PET_CONFIG_EVENT = "ai-hub-pet-changed";

/**
 * 桌宠视线事件：Rust 侧（`tray/gaze.rs`）按节拍读全局光标，把「相对桌宠窗中心的方向」
 * 推给桌宠窗。payload `{ x, y }` 是 -1..1 的归一化方向（x 右为正、y 下为正），没有这条
 * 事件时前端就退回窗内 pointer 的局部跟随。
 */
export const PET_GAZE_EVENT = "ai-hub-pet-gaze";

export interface PetConfigPatch extends Partial<PetConfig> {
  packRev?: number;
}

const LEGACY_SKIN_KEY = "qa-bubble-pet";

/** 下线形象的去向：GIF 时代与自绘矢量一律并到自备包，别让旧存档掉回默认值。 */
const SKIN_ALIAS: Record<string, PetSkinId> = {
  pikachu: CUSTOM_SKIN,
  shiny: CUSTOM_SKIN,
  robot: CUSTOM_SKIN,
  gundam: CUSTOM_SKIN,
  mochi: CUSTOM_SKIN,
  mario: CUSTOM_SKIN,
  maid: CUSTOM_SKIN,
  // 自绘机甲（2026-10-25 下线）：素材包空着就落回熊猫，不会出现空窗
  artisan: CUSTOM_SKIN,
};

function isPetSkin(value: unknown): value is PetSkinId {
  return typeof value === "string" && (PET_ORDER as string[]).includes(value);
}

function resolveSkin(value: unknown): PetSkinId | null {
  if (isPetSkin(value)) return value;
  return typeof value === "string" ? (SKIN_ALIAS[value] ?? null) : null;
}

/** 归一化：认不出的皮肤先走旧键与下线映射，尺寸不在档位表里就就近取档。 */
export function normalizePetConfig(raw: Partial<PetConfig> | null | undefined): PetConfig {
  const skin =
    resolveSkin(raw?.skin) ?? resolveSkin(safeRead(LEGACY_SKIN_KEY)) ?? PET_ORDER[0];
  const wanted = Number(raw?.size);
  const size = Number.isFinite(wanted)
    ? PET_SIZES.reduce(
        (best, s) => (Math.abs(s - wanted) < Math.abs(best - wanted) ? s : best),
        PET_SIZE_DEFAULT,
      )
    : PET_SIZE_DEFAULT;
  return { skin, size, quiet: raw?.quiet === true };
}

function safeRead(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
