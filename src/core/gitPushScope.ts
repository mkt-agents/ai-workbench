import type { TFunction } from "i18next";
import type { RepoBatchItem } from "./types";

/** Repos a batch push may legitimately act on. */
export function isPushable(summary: RepoBatchItem | undefined): boolean {
  if (!summary?.isGit || !summary.hasCommits) return false;
  return summary.hasUpstream ? summary.ahead > 0 : true;
}

type Translate = TFunction<"git">;

export type PushNote = { text: string; risky: boolean };

/**
 * What pushing this one repo would do, in one line — the wording the plain-text
 * confirmation used before the preview dialog existed, kept here so both the
 * section header and the aggregate warning agree. `risky` means more than a
 * plain fast-forward: publishing a branch origin has never seen, or pushing
 * while behind. `addedCommits` shifts the ahead count for a commit-then-push
 * flow, where the cached summary was read before the commit existed.
 */
export function pushTargetNote(
  summary: RepoBatchItem,
  t: Translate,
  options: { addedCommits?: number } = {}
): PushNote {
  const added = options.addedCommits ?? 0;
  if (!summary.hasUpstream) return { text: t("batch.pushNoUpstream"), risky: true };
  if (summary.behind > 0) {
    return { text: t("batch.pushBehind", { count: summary.behind }), risky: true };
  }
  return { text: t("batch.pushAhead", { count: summary.ahead + added }), risky: false };
}
