import type { TFunction } from "i18next";
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
  return bits.join(" · ");
}
