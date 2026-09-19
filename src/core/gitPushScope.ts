import type { TFunction } from "i18next";
import type { RepoBatchItem } from "./types";

/** Repos a batch push may legitimately act on. */
export function isPushable(summary: RepoBatchItem | undefined): boolean {
  if (!summary?.isGit || !summary.hasCommits) return false;
  return summary.hasUpstream ? summary.ahead > 0 : true;
}

type Translate = TFunction<"git">;

/**
 * Body text for the batch-push confirmation: one line per repo plus a warning
 * whenever something other than a plain fast-forward is about to happen.
 * `addedCommits` shifts the ahead counts for a commit-then-push flow, where
 * the cached summaries were read before the commit existed.
 */
export function describePushTargets(
  rows: { label: string; summary: RepoBatchItem }[],
  t: Translate,
  options: { addedCommits?: number } = {}
): { message: string; warning?: string } {
  const added = options.addedCommits ?? 0;
  let risky = false;
  const lines = rows.map(({ label, summary }) => {
    let note: string;
    if (!summary.hasUpstream) {
      risky = true;
      note = t("batch.pushNoUpstream");
    } else if (summary.behind > 0) {
      risky = true;
      note = t("batch.pushBehind", { count: summary.behind });
    } else {
      note = t("batch.pushAhead", { count: summary.ahead + added });
    }
    return `• ${label} — ${note}`;
  });
  return {
    message: `${t("batch.pushConfirmMessage", { count: rows.length })}\n${lines.join("\n")}`,
    warning: risky ? t("batch.pushWarning") : undefined,
  };
}
