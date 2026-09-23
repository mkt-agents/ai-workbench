/**
 * Pure helpers for the regression report's "概览" risk banner.
 *
 * Kept separate from the Rust `change_report` module so the UI can roll up the
 * static per-file risk flags into a one-line summary without re-deriving them.
 */
import type { ChangeReport } from "../components/testReportTypes";

/** Risk types that the banner counts as "high risk" and surfaces prominently. */
export const HIGH_RISK_TYPES = ["security", "money", "migration", "sql"] as const;

export type HighRiskType = (typeof HIGH_RISK_TYPES)[number];

export interface RiskCount {
  type: HighRiskType;
  count: number;
}

/**
 * Count high-risk flags across all changed files. A file flagged `security`
 * twice still counts once here — the banner is about "how many files", not
 * flags. Order follows HIGH_RISK_TYPES so the banner renders deterministically.
 */
export function summarizeRisks(report: ChangeReport | null): RiskCount[] {
  if (!report) return [];
  const counts = new Map<HighRiskType, number>();
  for (const type of HIGH_RISK_TYPES) counts.set(type, 0);
  for (const file of report.files) {
    // De-dup within a file so a doubly-tagged file does not double-count.
    const unique = new Set(file.risks);
    for (const type of HIGH_RISK_TYPES) {
      if (unique.has(type)) counts.set(type, (counts.get(type) ?? 0) + 1);
    }
  }
  return [...counts.entries()]
    .filter(([, count]) => count > 0)
    .map(([type, count]) => ({ type, count }));
}
