import type { MergeSegment } from "../core/types";
import type { MergeChunk } from "./merge3";

/**
 * Per-chunk decisions of the three-pane resolver. Each side of a change can be
 * applied (`>>` / `<<`) or ignored (`x`); applying both sides of a conflict
 * appends the second after the first, in the order they were applied.
 */
export type SideState = "none" | "applied" | "ignored";
export type Side = "left" | "right";

export type ChunkState = {
  left: SideState;
  right: SideState;
  /** Applied sides, first applied first. */
  order: Side[];
  /** Hand-edited result text for this chunk (wins over the sides). */
  custom?: string;
};

/** Keyed by chunk index. */
export type MergeState = Record<number, ChunkState>;

const EMPTY: ChunkState = { left: "none", right: "none", order: [] };

export function stateOf(state: MergeState, idx: number): ChunkState {
  return state[idx] ?? EMPTY;
}

export function isChange(chunk: MergeChunk): boolean {
  return chunk.kind !== "equal";
}

/** Sides that actually carry a change in this chunk. */
export function sidesOf(chunk: MergeChunk): Side[] {
  switch (chunk.kind) {
    case "ours":
      return ["left"];
    case "theirs":
      return ["right"];
    case "conflict":
      return ["left", "right"];
    default:
      return [];
  }
}

function linesOf(text: string): string[] {
  return text === "" ? [] : text.split("\n");
}

/** Result lines of one chunk under its state. */
export function chunkLines(chunk: MergeChunk, st: ChunkState = EMPTY): string[] {
  if (st.custom !== undefined) return linesOf(st.custom);
  if (chunk.kind === "equal") return chunk.base;
  if (chunk.kind === "same") return chunk.ours;
  const applied = st.order.filter((s) => st[s] === "applied");
  if (applied.length === 0) return chunk.base;
  return applied.flatMap((s) => (s === "left" ? chunk.ours : chunk.theirs));
}

/**
 * No decision left. A one-sided change needs its side applied or ignored; a
 * conflict is settled once either side is applied (the other can still be
 * appended) or both are ignored. A hand edit settles anything.
 */
export function chunkResolved(chunk: MergeChunk, st: ChunkState = EMPTY): boolean {
  if (st.custom !== undefined) return true;
  if (chunk.kind === "conflict") {
    return st.left === "applied" || st.right === "applied" || (st.left === "ignored" && st.right === "ignored");
  }
  return sidesOf(chunk).every((s) => st[s] !== "none");
}

export function counts(chunks: MergeChunk[], state: MergeState): { changes: number; conflicts: number } {
  let changes = 0;
  let conflicts = 0;
  chunks.forEach((c, i) => {
    if (chunkResolved(c, stateOf(state, i))) return;
    if (c.kind === "conflict") conflicts++;
    else changes++;
  });
  return { changes, conflicts };
}

/** `>>` / `<<` (apply) or `x` (ignore) on one side of one chunk. */
export function decide(state: MergeState, idx: number, side: Side, action: "apply" | "ignore"): MergeState {
  const cur = stateOf(state, idx);
  const next: ChunkState = { ...cur, custom: undefined, order: cur.order.filter((s) => s !== side) };
  next[side] = action === "apply" ? "applied" : "ignored";
  if (action === "apply") next.order = [...next.order, side];
  return { ...state, [idx]: next };
}

export function resetChunk(state: MergeState, idx: number): MergeState {
  const next = { ...state };
  delete next[idx];
  return next;
}

export function editChunk(state: MergeState, idx: number, text: string): MergeState {
  return { ...state, [idx]: { ...stateOf(state, idx), custom: text } };
}

/** IDEA's magic wand: apply every one-sided change that is still open. */
export function applyNonConflicting(chunks: MergeChunk[], state: MergeState): MergeState {
  let next = state;
  chunks.forEach((c, i) => {
    const st = stateOf(next, i);
    if (st.custom !== undefined) return;
    if (c.kind === "ours" && st.left === "none") next = decide(next, i, "left", "apply");
    if (c.kind === "theirs" && st.right === "none") next = decide(next, i, "right", "apply");
  });
  return next;
}

/** Open one-sided changes (what the magic wand would apply). */
export function openNonConflicting(chunks: MergeChunk[], state: MergeState): number {
  return chunks.filter((c, i) => (c.kind === "ours" || c.kind === "theirs") && !chunkResolved(c, stateOf(state, i))).length;
}

/** Whole file from one side: that side applied everywhere, the other ignored. */
export function acceptSide(chunks: MergeChunk[], side: Side): MergeState {
  const other: Side = side === "left" ? "right" : "left";
  const next: MergeState = {};
  chunks.forEach((c, i) => {
    const sides = sidesOf(c);
    if (sides.length === 0) return;
    const st: ChunkState = { left: "none", right: "none", order: [] };
    if (sides.includes(side)) {
      st[side] = "applied";
      st.order = [side];
    }
    // A change that only the other side made is dropped: the chunk keeps base.
    if (sides.includes(other)) st[other] = "ignored";
    next[i] = st;
  });
  return next;
}

function joinLines(lines: string[], trailingNewline: boolean): string {
  if (lines.length === 0) return "";
  return lines.join("\n") + (trailingNewline ? "\n" : "");
}

/** The merged file; open chunks contribute their base text. */
export function assembleResult(chunks: MergeChunk[], state: MergeState, trailingNewline: boolean): string {
  return joinLines(
    chunks.flatMap((c, i) => chunkLines(c, stateOf(state, i))),
    trailingNewline
  );
}

/**
 * Full text for hand editing: open conflicts are written back as markers so
 * nothing is silently dropped (the backend refuses to save while any remain).
 */
export function assembleWithMarkers(
  chunks: MergeChunk[],
  state: MergeState,
  labels: { ours: string; theirs: string },
  trailingNewline: boolean
): string {
  const lines = chunks.flatMap((c, i) => {
    const st = stateOf(state, i);
    if (c.kind === "conflict" && !chunkResolved(c, st)) {
      return [`<<<<<<< ${labels.ours}`, ...c.ours, "=======", ...c.theirs, `>>>>>>> ${labels.theirs}`];
    }
    return chunkLines(c, st);
  });
  return joinLines(lines, trailingNewline || lines.length > 0);
}

/** Next open chunk after (dir 1) or before (dir -1) `from`, wrapping around. */
export function nextOpen(chunks: MergeChunk[], state: MergeState, from: number, dir: 1 | -1): number | null {
  const open = chunks.flatMap((c, i) => (chunkResolved(c, stateOf(state, i)) ? [] : [i]));
  if (open.length === 0) return null;
  if (dir === 1) return open.find((i) => i > from) ?? open[0];
  return [...open].reverse().find((i) => i < from) ?? open[open.length - 1];
}

/** git's own merge-file output with markers: the hand-edit fallback for huge files. */
export function segmentsWithMarkers(segments: MergeSegment[], labels: { ours: string; theirs: string }): string {
  const nl = (s: string) => (s && !s.endsWith("\n") ? `${s}\n` : s);
  return segments
    .map((s) =>
      s.kind === "same"
        ? s.text
        : `<<<<<<< ${labels.ours}\n${nl(s.ours)}=======\n${nl(s.theirs)}>>>>>>> ${labels.theirs}\n`
    )
    .join("");
}
