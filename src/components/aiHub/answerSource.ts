import type { TFunction } from "i18next";
import type { AiHubTurn } from "../../core/types";
import type { AnswerSource } from "./useAskStream";

/** One-line "what this answer was based on": mode · repos · parts · model · time · tokens. */
export function describeSource(source: AnswerSource | null, t: TFunction): string | null {
  if (!source) return null;
  const bits: string[] = [];
  if (source.mode === "project") {
    bits.push(t("source.project"));
    if (source.summary.repos.length) bits.push(source.summary.repos.join("、"));
    const projectParts = source.summary.parts;
    if (projectParts.length) bits.push(projectParts.map((p) => t(`part.${p}`)).join(" + "));
    else bits.push(t("source.noProjectData"));
  } else {
    bits.push(t("source.chat"));
  }
  bits.push(source.modelName);
  if (source.elapsedMs != null) {
    bits.push(
      source.elapsedMs >= 1000
        ? t("source.elapsedSec", { n: (source.elapsedMs / 1000).toFixed(1) })
        : t("source.elapsedMs", { n: source.elapsedMs })
    );
  }
  if (source.promptTokens != null || source.completionTokens != null) {
    const p = source.promptTokens ?? 0;
    const c = source.completionTokens ?? 0;
    bits.push(t("source.tokens", { n: p + c, in: p, out: c }));
  } else if (source.estimatedTokens != null) {
    bits.push(t("source.tokensEst", { n: source.estimatedTokens }));
  }
  // Be honest about what the model did NOT get: a clipped context block or rounds
  // left out of the request.
  if (source.contextTruncated) bits.push(t("source.ctxClipped"));
  if (source.droppedTurns) bits.push(t("source.roundsLeftOut", { n: Math.ceil(source.droppedTurns / 2) }));
  // Backend-side budget: how full the window was, and whether anything had to be compacted.
  if (source.budgetRatio != null) bits.push(t("source.budget", { n: Math.round(source.budgetRatio * 100) }));
  if (source.trimmedRounds) bits.push(t("source.trimmed", { n: source.trimmedRounds }));
  if (source.cacheHitTokens) bits.push(t("source.cacheHit", { n: compactCount(source.cacheHitTokens) }));
  return bits.join(" · ");
}

/**
 * A loaded session has no live `source` (mode / repos are not persisted with the turn),
 * but the numbers are: show those alone once the message is read back from SQLite.
 * Returns null for turns saved before this telemetry existed.
 */
export function describeTurn(turn: AiHubTurn, t: TFunction): string | null {
  const bits: string[] = [];
  if (turn.promptTokens != null || turn.completionTokens != null) {
    const p = turn.promptTokens ?? 0;
    const c = turn.completionTokens ?? 0;
    bits.push(t("source.tokens", { n: p + c, in: p, out: c }));
  }
  if (turn.elapsedMs != null) {
    bits.push(
      turn.elapsedMs >= 1000
        ? t("source.elapsedSec", { n: (turn.elapsedMs / 1000).toFixed(1) })
        : t("source.elapsedMs", { n: turn.elapsedMs })
    );
  }
  if (turn.budgetRatio != null) bits.push(t("source.budget", { n: Math.round(turn.budgetRatio * 100) }));
  if (turn.trimmedRounds) bits.push(t("source.trimmed", { n: turn.trimmedRounds }));
  if (turn.cacheHitTokens) bits.push(t("source.cacheHit", { n: compactCount(turn.cacheHitTokens) }));
  return bits.length ? bits.join(" · ") : null;
}

/** 12000 → "12k": the source line is one row, not a spreadsheet. */
function compactCount(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : String(n);
}
