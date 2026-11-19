import type { AiHubContextInfo } from "../../core/types";

/** Window occupancy and compression pressure deliberately use different denominators. */
export function contextMeter(info: AiHubContextInfo | null) {
  const used = info?.totalInputTokens;
  const window = info?.budget?.effectiveWindow;
  if (info?.usageKind === "compaction" || used == null || window == null
    || !Number.isFinite(used) || used < 0 || !Number.isFinite(window) || window <= 0) return null;
  const content = info?.estimatedTokens;
  const budget = info?.inputBudget;
  const pressure = content != null && budget != null && Number.isFinite(content)
    && Number.isFinite(budget) && budget > 0 ? content / budget : 0;
  return {
    percent: Math.round(used / window * 100),
    level: used >= window || pressure >= 1 ? "full" : pressure >= 0.8 ? "high" : "normal",
  };
}
