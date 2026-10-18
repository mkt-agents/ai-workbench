/**
 * Three-way merge of whole files for the conflict resolver (IDEA's "Merge
 * Revisions"): line diffs base→ours and base→theirs, folded into chunks that
 * line the three panes up. Nothing is applied here; the user decides per chunk.
 */

/**
 * - `equal`: identical in all three;
 * - `ours` / `theirs`: only that side changed (non-conflicting);
 * - `same`: both made the identical change (needs no decision);
 * - `conflict`: both changed the same lines differently.
 */
export type ChunkKind = "equal" | "ours" | "theirs" | "same" | "conflict";

export type MergeChunk = { kind: ChunkKind; base: string[]; ours: string[]; theirs: string[] };

export type Merge3 = {
  chunks: MergeChunk[];
  /** The result keeps a final newline when the local file had one. */
  trailingNewline: boolean;
};

/** Larger files are merged by hand (with conflict markers) instead. */
export const MERGE_LINE_LIMIT = 20_000;
/** Myers gives up past this many edits; the caller falls back the same way. */
const MAX_EDITS = 3000;

export function splitLines(text: string): { lines: string[]; trailingNewline: boolean } {
  const norm = text.replace(/\r\n/g, "\n");
  if (norm === "") return { lines: [], trailingNewline: false };
  const trailingNewline = norm.endsWith("\n");
  const lines = (trailingNewline ? norm.slice(0, -1) : norm).split("\n");
  return { lines, trailingNewline };
}

/**
 * Matched line pairs `[i, j]` (a[i] === b[j]) of a shortest edit script
 * (Myers O(ND)). Null when the files differ in more than MAX_EDITS lines.
 */
export function myersMatches(a: string[], b: string[], maxEdits = MAX_EDITS): Array<[number, number]> | null {
  const n = a.length;
  const m = b.length;
  // trace[d][k + d] = furthest x on diagonal k after d edits.
  const trace: Int32Array[] = [];
  let x0 = 0;
  while (x0 < n && x0 < m && a[x0] === b[x0]) x0++;
  trace.push(Int32Array.of(x0));
  let found = x0 >= n && x0 >= m ? 0 : -1;
  for (let d = 1; found < 0 && d <= Math.min(n + m, maxEdits); d++) {
    const prev = trace[d - 1];
    const cur = new Int32Array(2 * d + 1);
    for (let k = -d; k <= d; k += 2) {
      const down = k === -d || (k !== d && prev[k - 1 + d - 1] < prev[k + 1 + d - 1]);
      let x = down ? prev[k + 1 + d - 1] : prev[k - 1 + d - 1] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      cur[k + d] = x;
      if (x >= n && y >= m) {
        found = d;
        break;
      }
    }
    trace.push(cur);
  }
  if (found < 0) return null;
  const matches: Array<[number, number]> = [];
  let cx = n;
  let cy = m;
  for (let d = found; d > 0; d--) {
    const prev = trace[d - 1];
    const k = cx - cy;
    const down = k === -d || (k !== d && prev[k - 1 + d - 1] < prev[k + 1 + d - 1]);
    const pk = down ? k + 1 : k - 1;
    const px = prev[pk + d - 1];
    const py = px - pk;
    const sx = down ? px : px + 1;
    const sy = sx - k;
    while (cx > sx && cy > sy) {
      cx--;
      cy--;
      matches.push([cx, cy]);
    }
    cx = px;
    cy = py;
  }
  while (cx > 0 && cy > 0) {
    cx--;
    cy--;
    matches.push([cx, cy]);
  }
  return matches.reverse();
}

/** A changed range: `oLen` base lines at `oStart` became `aLen` lines at `aStart`. */
type Hunk = { oStart: number; oLen: number; aStart: number; aLen: number; side: 0 | 1 };

function hunksOf(base: string[], side: string[], tag: 0 | 1): Hunk[] | null {
  const matches = myersMatches(base, side);
  if (!matches) return null;
  const out: Hunk[] = [];
  let i = 0;
  let j = 0;
  for (const [mi, mj] of [...matches, [base.length, side.length] as [number, number]]) {
    if (mi > i || mj > j) out.push({ oStart: i, oLen: mi - i, aStart: j, aLen: mj - j, side: tag });
    i = mi + 1;
    j = mj + 1;
  }
  return out;
}

function sameLines(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((line, i) => line === b[i]);
}

/**
 * Whole-file three-way chunks. Overlapping or touching changes from both sides
 * form one region (as git does), so adjacent edits are a conflict, not a guess.
 * Null above MERGE_LINE_LIMIT lines or MAX_EDITS edits.
 */
export function diff3Chunks(baseText: string, oursText: string, theirsText: string): Merge3 | null {
  const base = splitLines(baseText);
  const ours = splitLines(oursText);
  const theirs = splitLines(theirsText);
  if ([base, ours, theirs].some((s) => s.lines.length > MERGE_LINE_LIMIT)) return null;
  const ha = hunksOf(base.lines, ours.lines, 0);
  const hb = hunksOf(base.lines, theirs.lines, 1);
  if (!ha || !hb) return null;
  const hunks = [...ha, ...hb].sort((x, y) => x.oStart - y.oStart || x.side - y.side);
  const chunks: MergeChunk[] = [];
  const pushEqual = (from: number, to: number) => {
    if (to <= from) return;
    const lines = base.lines.slice(from, to);
    chunks.push({ kind: "equal", base: lines, ours: lines, theirs: lines });
  };
  let pos = 0;
  let i = 0;
  while (i < hunks.length) {
    const group = [hunks[i++]];
    const start = group[0].oStart;
    let end = start + group[0].oLen;
    while (i < hunks.length && hunks[i].oStart <= end) {
      end = Math.max(end, hunks[i].oStart + hunks[i].oLen);
      group.push(hunks[i++]);
    }
    pushEqual(pos, start);
    const sideLines = (tag: 0 | 1, lines: string[]) => {
      const own = group.filter((h) => h.side === tag);
      if (own.length === 0) return base.lines.slice(start, end);
      const first = own[0];
      const last = own[own.length - 1];
      const from = first.aStart - (first.oStart - start);
      const to = last.aStart + last.aLen + (end - (last.oStart + last.oLen));
      return lines.slice(from, to);
    };
    const o = sideLines(0, ours.lines);
    const t = sideLines(1, theirs.lines);
    const hasOurs = group.some((h) => h.side === 0);
    const hasTheirs = group.some((h) => h.side === 1);
    const kind: ChunkKind = hasOurs && hasTheirs ? (sameLines(o, t) ? "same" : "conflict") : hasOurs ? "ours" : "theirs";
    chunks.push({ kind, base: base.lines.slice(start, end), ours: o, theirs: t });
    pos = end;
  }
  pushEqual(pos, base.lines.length);
  const trailingNewline = ours.lines.length > 0 ? ours.trailingNewline : theirs.trailingNewline || base.trailingNewline;
  return { chunks, trailingNewline };
}
