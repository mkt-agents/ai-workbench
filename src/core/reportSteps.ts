/**
 * Report workflow steps, derived — never stored.
 *
 * The five steps (采集 → AI → 选测 → 运行 → 验收) used to be implicit: each
 * section hid behind the previous button press, and a page reload forgot where
 * you were. Deriving each step's state from what the DB and the run store
 * already know means the workflow is re-enterable from any step with nothing
 * to keep in sync.
 */
import type { ScenarioSummary } from "./types";

export type StepId = "collect" | "ai" | "select" | "run" | "accept";
export type StepState = "empty" | "ready" | "done" | "running";

export interface StepInput {
  hasReport: boolean;
  fileCount: number;
  ai: string;
  selectionTargets: number;
  linkedRuns: number;
  running: boolean;
  summary: ScenarioSummary | null;
  /** The stored acceptance stamp; lets a tester close the loop by hand. */
  acceptedAt: string | null;
}

export const STEP_ORDER: StepId[] = ["collect", "ai", "select", "run", "accept"];

export function deriveSteps(input: StepInput): Record<StepId, StepState> {
  const { hasReport, ai, selectionTargets, linkedRuns, running, summary, acceptedAt } = input;
  const accepted =
    !!acceptedAt ||
    (!!summary && summary.total > 0 && summary.pending === 0 && summary.percent >= 100);
  return {
    collect: hasReport ? "done" : "ready",
    ai: !hasReport ? "empty" : ai ? "done" : "ready",
    select: !hasReport ? "empty" : selectionTargets > 0 ? "done" : "ready",
    run: !hasReport ? "empty" : running ? "running" : linkedRuns > 0 ? "done" : "ready",
    accept: !hasReport
      ? "empty"
      : summary && summary.total > 0
        ? accepted
          ? "done"
          : "ready"
        : "empty",
  };
}
