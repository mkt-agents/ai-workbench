import type { MergeSegment } from "../core/types";

/**
 * Three-way merge chunks for the conflict resolver. Segments come from
 * `git merge-file --diff3` (see git_merge.rs); every conflict chunk needs an
 * explicit user choice before the file can be marked resolved.
 */
export type ConflictSegment = Extract<MergeSegment, { kind: "conflict" }>;

export type ChunkChoice = "ours" | "theirs" | "oursTheirs" | "theirsOurs" | "base" | "custom";

export type ChunkResolution = { choice: ChunkChoice; text: string };

/** Keyed by segment index. */
export type Resolutions = Record<number, ChunkResolution>;

/** Join two blocks so the second starts on its own line. */
export function joinBlocks(first: string, second: string): string {
  if (!first) return second;
  if (!second) return first;
  return first.endsWith("\n") ? first + second : `${first}\n${second}`;
}

export function choiceText(seg: ConflictSegment, choice: Exclude<ChunkChoice, "custom">): string {
  switch (choice) {
    case "ours":
      return seg.ours;
    case "theirs":
      return seg.theirs;
    case "oursTheirs":
      return joinBlocks(seg.ours, seg.theirs);
    case "theirsOurs":
      return joinBlocks(seg.theirs, seg.ours);
    case "base":
      return seg.base;
  }
}

export function conflictIndexes(segments: MergeSegment[]): number[] {
  return segments.flatMap((s, i) => (s.kind === "conflict" ? [i] : []));
}

export function unresolvedCount(segments: MergeSegment[], res: Resolutions): number {
  return conflictIndexes(segments).filter((i) => !res[i]).length;
}

/** The merged file, or null while any conflict chunk is still open. */
export function assembleResult(segments: MergeSegment[], res: Resolutions): string | null {
  let out = "";
  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    if (seg.kind === "same") {
      out += seg.text;
      continue;
    }
    const picked = res[i];
    if (!picked) return null;
    out += picked.text;
  }
  return out;
}

function withNewline(text: string): string {
  return text && !text.endsWith("\n") ? `${text}\n` : text;
}

/**
 * Full text for hand editing: open chunks are written back as conflict markers,
 * so nothing is silently dropped and the backend refuses to save until they go.
 */
export function assembleWithMarkers(
  segments: MergeSegment[],
  res: Resolutions,
  labels: { ours: string; theirs: string }
): string {
  let out = "";
  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    if (seg.kind === "same") {
      out += seg.text;
    } else if (res[i]) {
      out += res[i].text;
    } else {
      out +=
        `<<<<<<< ${labels.ours}\n${withNewline(seg.ours)}` +
        `=======\n${withNewline(seg.theirs)}>>>>>>> ${labels.theirs}\n`;
    }
  }
  return out;
}

/** Next open conflict after (dir 1) or before (dir -1) `from`, wrapping around. */
export function nextUnresolved(
  segments: MergeSegment[],
  res: Resolutions,
  from: number,
  dir: 1 | -1
): number | null {
  const open = conflictIndexes(segments).filter((i) => !res[i]);
  if (open.length === 0) return null;
  if (dir === 1) return open.find((i) => i > from) ?? open[0];
  return [...open].reverse().find((i) => i < from) ?? open[open.length - 1];
}

/** Take one side for every chunk that is still open; chosen chunks stay. */
export function resolveRemaining(segments: MergeSegment[], res: Resolutions, side: "ours" | "theirs"): Resolutions {
  const next: Resolutions = { ...res };
  for (const i of conflictIndexes(segments)) {
    if (next[i]) continue;
    const seg = segments[i] as ConflictSegment;
    next[i] = { choice: side, text: choiceText(seg, side) };
  }
  return next;
}

export function lineCount(text: string): number {
  if (!text) return 0;
  const n = text.split("\n").length;
  return text.endsWith("\n") ? n - 1 : n;
}
