import type { TFunction } from "i18next";
import type { AnswerSource } from "./useAskStream";

/** One-line "what this answer was based on": mode · repos · parts · task · model · time · tokens. */
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
  if (source.task !== "none") bits.push(t(`taskShort.${source.task}`));
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
  return bits.join(" · ");
}
