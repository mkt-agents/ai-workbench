import type { PetGesture, PetMood } from "./petSkins";
import { PET_GESTURES, PET_MOODS } from "./petSkins";

/**
 * 自备素材入库管线：把用户挑的图片（或一张 n×n 拼图）变成一整套能直接播的帧。
 *
 * 三步：抠背景 → 裁到紧包围盒 → 全部塞进**同一边长**的正方形画布（脚贴底、水平居中）。
 * 第三步是关键：帧尺寸不一致的话，一切情绪角色就忽大忽小、还会左右飘。
 */

/** 产出的帧一律是这个边长的正方形 PNG（够 96–160 的窗，再大只是白占体积）。 */
export const FRAME_TARGET = 256;
/** 小图最多放大 2 倍，再多就糊得没法看，宁可让它小一点。 */
export const FRAME_MAX_SCALE = 2;
/** 与背景色差值小于这个容差就当背景抠掉（0–255）。 */
export const KEY_TOLERANCE = 30;
/** 容差外再放宽一圈：抗锯齿的边缘像素是"前景 × 覆盖率 + 背景 × (1-覆盖率)"，得还回去。 */
const KEY_SOFT = KEY_TOLERANCE * 3;

export interface PickedFile {
  name: string;
  dataUrl: string;
}

/** 解码后的一个格子：源位图 + 它在源图上的矩形。 */
export interface SourceCell {
  key: string;
  bitmap: ImageBitmap;
  x: number;
  y: number;
  w: number;
  h: number;
}

export type CellAssignment = PetMood | PetGesture | null;

export interface BuiltFrame {
  name: string;
  dataUrl: string;
}

const SLOT_ALIASES: Record<string, CellAssignment> = {
  idle: "idle",
  stand: "idle",
  normal: "idle",
  thinking: "thinking",
  think: "thinking",
  working: "working",
  work: "working",
  busy: "working",
  waiting: "waiting",
  wait: "waiting",
  approval: "waiting",
  happy: "happy",
  success: "happy",
  celebrate: "happy",
  error: "error",
  fail: "error",
  sleepy: "sleepy",
  sleep: "sleepy",
  drag: "drag",
  hold: "drag",
  tap: "tap",
  click: "tap",
  poke: "tap",
};

/** 文件名里认情绪：`idle.png`、`thinking-2.png`、`click.webm` 都行，认不出返回 null。 */
export function guessSlot(name: string): CellAssignment {
  const head = name.replace(/\.[a-z0-9]+$/i, "").toLowerCase().split(/[-_\s]/)[0];
  const hit = SLOT_ALIASES[head];
  const known = (PET_MOODS as string[]).includes(head) || (PET_GESTURES as string[]).includes(head);
  return hit ?? (known ? (head as CellAssignment) : null);
}

/** 解码每张图并按 rows×cols 切格（位图尺寸只有浏览器知道，所以切格发生在解码之后）。 */
export async function loadCells(files: PickedFile[], rows: number, cols: number): Promise<SourceCell[]> {
  const out: SourceCell[] = [];
  for (const file of files) {
    const blob = await (await fetch(file.dataUrl)).blob();
    const bitmap = await createImageBitmap(blob);
    const cw = Math.floor(bitmap.width / cols);
    const ch = Math.floor(bitmap.height / rows);
    for (let r = 0; r < rows; r++) {
      for (let c = 0; c < cols; c++) {
        out.push({ key: `${file.name}#${r}-${c}`, bitmap, x: c * cw, y: r * ch, w: cw, h: ch });
      }
    }
  }
  return out;
}

function channelGap(r: number, g: number, b: number, bg: [number, number, number]) {
  return Math.max(Math.abs(r - bg[0]), Math.abs(g - bg[1]), Math.abs(b - bg[2]));
}

/**
 * 背景色 = 四角主色。四角本来就透明（素材自带 alpha）或四色差得远（带场景）时返回 null，
 * 交给调用方不抠——硬抠会把角色的白毛/黑眼睛咬掉一块。
 */
export function cornerColor(
  data: Uint8ClampedArray,
  w: number,
  h: number,
): [number, number, number] | null {
  const pts = [
    [1, 1],
    [w - 2, 1],
    [1, h - 2],
    [w - 2, h - 2],
  ].filter(([x, y]) => x >= 0 && y >= 0 && x < w && y < h);
  if (pts.length < 4) return null;
  const rgb = pts.map(([x, y]) => {
    const i = (y * w + x) * 4;
    return { alpha: data[i + 3], color: [data[i], data[i + 1], data[i + 2]] as [number, number, number] };
  });
  if (rgb.some((p) => p.alpha < 250)) return null;
  const avg: [number, number, number] = [
    Math.round(rgb.reduce((s, p) => s + p.color[0], 0) / 4),
    Math.round(rgb.reduce((s, p) => s + p.color[1], 0) / 4),
    Math.round(rgb.reduce((s, p) => s + p.color[2], 0) / 4),
  ];
  return rgb.some((p) => channelGap(p.color[0], p.color[1], p.color[2], avg) > KEY_TOLERANCE * 2) ? null : avg;
}

/**
 * 就地抠像：贴住背景色的像素直接透明；容差外那一圈是抗锯齿的混色像素，
 * 按 `观测 = 前景×覆盖率 + 背景×(1-覆盖率)` 反解回前景本色——不然角色外面会套一圈背景色毛边
 * （洋红底抠出来就是一圈粉边，白底抠出来就是一圈白晕）。
 */
export function keyOut(image: ImageData, bg: [number, number, number] | null, tol = KEY_TOLERANCE) {
  if (!bg) return;
  const d = image.data;
  const band = KEY_SOFT - tol;
  for (let i = 0; i < d.length; i += 4) {
    if (d[i + 3] === 0) continue;
    const gap = channelGap(d[i], d[i + 1], d[i + 2], bg);
    if (gap <= tol) {
      d[i + 3] = 0;
      continue;
    }
    if (gap >= KEY_SOFT) continue;
    const cover = Math.max(0.12, (gap - tol) / band);
    for (let k = 0; k < 3; k++) {
      const mixed = (d[i + k] - bg[k] * (1 - cover)) / cover;
      d[i + k] = Math.min(255, Math.max(0, Math.round(mixed)));
    }
    d[i + 3] = Math.round(d[i + 3] * cover);
  }
}

/** 透明像素不算数：返回真正有内容的紧包围盒。 */
export function alphaBounds(image: ImageData) {
  const { width: w, height: h, data } = image;
  let x0 = w;
  let y0 = h;
  let x1 = -1;
  let y1 = -1;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      if (data[(y * w + x) * 4 + 3] > 8) {
        if (x < x0) x0 = x;
        if (x > x1) x1 = x;
        if (y < y0) y0 = y;
        if (y > y1) y1 = y;
      }
    }
  }
  return x1 < 0 ? null : { x0, y0, w: x1 - x0 + 1, h: y1 - y0 + 1 };
}

function canvasOf(w: number, h: number) {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  return { c, ctx: c.getContext("2d", { willReadFrequently: true })! };
}

async function toDataUrl(canvas: HTMLCanvasElement): Promise<string> {
  const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, "image/png"));
  if (!blob) throw new Error("画布导出失败");
  return new Promise<string>((res, rej) => {
    const fr = new FileReader();
    fr.onload = () => res(String(fr.result));
    fr.onerror = () => rej(new Error("读不回 data URL"));
    fr.readAsDataURL(blob);
  });
}

/** 一格抠成"只留角色 + 紧包围盒"。 */
function cutCell(cell: SourceCell, keyOutOn: boolean) {
  const { c, ctx } = canvasOf(cell.w, cell.h);
  ctx.drawImage(cell.bitmap, cell.x, cell.y, cell.w, cell.h, 0, 0, cell.w, cell.h);
  const image = ctx.getImageData(0, 0, c.width, c.height);
  if (keyOutOn) keyOut(image, cornerColor(image.data, c.width, c.height));
  const box = alphaBounds(image);
  return box ? { image, box, holder: c } : null;
}

/**
 * 归一化并导出。`assignments` 与 `cells` 一一对应；同一槽位分到多格就自动编号
 * （`idle-1.png`、`idle-2.png`），桌宠那边会当序列帧轮播。
 */
export async function buildFrames(
  cells: SourceCell[],
  assignments: CellAssignment[],
  opts: { keyBackground: boolean },
): Promise<BuiltFrame[]> {
  const slots: (PetMood | PetGesture)[] = [];
  const cut: NonNullable<ReturnType<typeof cutCell>>[] = [];
  cells.forEach((cell, i) => {
    const slot = assignments[i];
    if (!slot) return;
    const result = cutCell(cell, opts.keyBackground);
    if (!result) return;
    slots.push(slot);
    cut.push(result);
  });
  if (!cut.length) return [];

  // 统一缩放比：按最大的一格算，缩放比不一致角色就会忽大忽小
  const side = Math.max(...cut.map((p) => Math.max(p.box.w, p.box.h)));
  const scale = Math.min(FRAME_TARGET / side, FRAME_MAX_SCALE);
  const out = Math.max(1, Math.round(side * scale));

  const counts = new Map<string, number>();
  const frames: BuiltFrame[] = [];
  for (let i = 0; i < cut.length; i++) {
    const { image, box, holder } = cut[i];
    holder.getContext("2d")!.putImageData(image, 0, 0);
    const slot = slots[i];
    const n = (counts.get(slot) ?? 0) + 1;
    counts.set(slot, n);
    const { c, ctx } = canvasOf(out, out);
    const dw = Math.max(1, Math.round(box.w * scale));
    const dh = Math.max(1, Math.round(box.h * scale));
    ctx.imageSmoothingQuality = "high";
    // 脚贴底、水平居中：切情绪时不跳位，也不会在窗里浮起来
    ctx.drawImage(holder, box.x0, box.y0, box.w, box.h, Math.round((out - dw) / 2), out - dh, dw, dh);
    frames.push({ name: n > 1 ? `${slot}-${n}.png` : `${slot}.png`, dataUrl: await toDataUrl(c) });
  }
  return frames;
}
