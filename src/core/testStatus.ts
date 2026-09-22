/**
 * The single mapping from wire statuses/kinds to their i18n labels.
 *
 * The backend word lists are stable contracts (test_runs.status, error_kind,
 * scenario status); anything not listed here is an older row or a value from a
 * newer backend, and must show verbatim rather than be mislabelled.
 */
import type { TFunction } from "i18next";

const OUTCOME_KEYS: Record<string, string> = {
  running: "statusRunning",
  success: "success",
  failed: "failed",
  error: "error",
  cancelled: "statusCancelled",
  timeout: "statusTimeout",
  // Only written by older builds; never relabel it as an error.
  skipped: "skipped",
};

export function outcomeLabel(t: TFunction<"test">, status: string): string {
  const key = OUTCOME_KEYS[status];
  return key ? t(key) : status;
}
