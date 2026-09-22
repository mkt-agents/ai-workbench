/**
 * Shared, module-level store for live test-run sessions.
 *
 * Run state used to live inside TestManager, which unmounts on tab switch — so
 * navigating away mid-run lost the spinner, the cancel button and the live
 * output while the process kept running, and a second concurrent run overwrote
 * the first one's tail. This module owns one session per run (keyed by the
 * run_id it hands to `run_test`), installed once per app lifetime.
 *
 * Like gitCache it is deliberately not part of the persisted store: a run is
 * live process state and must never be restored from disk.
 */
import { listen } from "@tauri-apps/api/event";
import { tauriInvoke } from "./store/helpers";
import type { TestRunOutcome, TestRunResult } from "./types";

export type RunSource = "manual" | "batch" | "change-report" | "coverage-rerun";
export type RunStatus = "running" | TestRunOutcome;

export interface RunSession {
  runId: string;
  projectId: string;
  projectName: string;
  source: RunSource;
  /** Set when the run was started from a change report, for auto-linking. */
  reportId?: string;
  /** Epoch ms; also feeds the elapsed timer. */
  startedAt: number;
  endedAt?: number;
  /** Rolling window of the live tail; the full log arrives with `result`. */
  lines: string[];
  status: RunStatus;
  result?: TestRunResult;
  /** Present when the command itself rejected (spawn/bookkeeping failure). */
  error?: string;
}

/** Newest finished sessions kept for reopening results without a re-run. */
const HISTORY_KEEP = 30;
const MAX_TAIL_LINES = 500;

const sessions = new Map<string, RunSession>();
/** Insertion order, so the array handed to React is stable and predictable. */
const ordered: string[] = [];

let listeners = new Set<() => void>();
let cachedSnapshot: RunSession[] | null = null;

function emit() {
  cachedSnapshot = null;
  for (const listener of listeners) listener();
}

export function subscribeRuns(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}

/** Stable array identity between changes, as useSyncExternalStore requires. */
export function getRunsSnapshot(): RunSession[] {
  if (!cachedSnapshot) {
    cachedSnapshot = ordered.map((id) => sessions.get(id)!).filter(Boolean);
  }
  return cachedSnapshot;
}

export function getRun(runId: string): RunSession | undefined {
  return sessions.get(runId);
}

/** The live run of a project, if any — the card's cancel/spinner source. */
export function activeRunOf(projectId: string): RunSession | undefined {
  for (const id of ordered) {
    const s = sessions.get(id);
    if (s && s.projectId === projectId && s.status === "running") return s;
  }
  return undefined;
}

export function runningRuns(): RunSession[] {
  return getRunsSnapshot().filter((s) => s.status === "running");
}

/** Newest terminal session for a project — diagnostics pre-fill source. */
export function latestFinishedOf(projectId: string): RunSession | undefined {
  for (let i = ordered.length - 1; i >= 0; i -= 1) {
    const s = sessions.get(ordered[i]);
    if (s && s.projectId === projectId && s.status !== "running") return s;
  }
  return undefined;
}

function pruneFinished() {
  const finished = ordered.filter((id) => sessions.get(id)?.status !== "running");
  for (const id of finished.slice(0, Math.max(0, finished.length - HISTORY_KEEP))) {
    sessions.delete(id);
    ordered.splice(ordered.indexOf(id), 1);
  }
}

let listening: Promise<() => void> | null = null;

interface OutputChunk {
  projectId: string;
  /** Absent in payloads from a backend built before run routing existed. */
  runId?: string;
  text: string;
  done: boolean;
}

function ensureListening() {
  if (listening) return;
  listening = listen<OutputChunk>("test-run-output", (event) => {
    const { projectId, runId, text, done } = event.payload;
    const target = runId
      ? sessions.get(runId)
      : activeRunOf(projectId);
    if (!target) return;
    if (done) {
      // The final result (with the full log) arrives through the command
      // resolution; the tail simply stops growing.
      return;
    }
    const next = [
      ...target.lines,
      ...text.split(/\r?\n/).filter((line) => line.trim().length > 0),
    ];
    target.lines = next.length > MAX_TAIL_LINES ? next.slice(-MAX_TAIL_LINES) : next;
    emit();
  });
}

/**
 * Register a session and kick off `run_test`. Returns immediately so the UI
 * paints the spinner; the awaited promise settles with the terminal session
 * (status/result filled, change-report link attached when relevant).
 */
export function startRun(input: {
  projectId: string;
  projectName: string;
  source?: RunSource;
  reportId?: string;
  args?: string;
  /** Ask the backend to append its per-framework coverage flags. */
  coverage?: boolean;
}): { runId: string; done: Promise<RunSession> } {
  ensureListening();
  const runId = crypto.randomUUID();
  const session: RunSession = {
    runId,
    projectId: input.projectId,
    projectName: input.projectName,
    source: input.source ?? "manual",
    reportId: input.reportId,
    startedAt: Date.now(),
    lines: [],
    status: "running",
  };
  sessions.set(runId, session);
  ordered.push(runId);
  emit();

  const done = tauriInvoke<TestRunResult>("run_test", {
    projectId: input.projectId,
    args: input.args || null,
    runId,
    coverage: input.coverage ? true : null,
  })
    .then((result) => {
      session.status = result.status;
      session.result = result;
      session.endedAt = Date.now();
      if (session.source === "change-report" && session.reportId) {
        // Bookkeeping only: a failed link must not sink the run itself.
        void tauriInvoke("link_change_run", {
          reportId: session.reportId,
          runId: result.id,
        }).catch(() => {});
      }
      return session;
    })
    .catch((e) => {
      session.status = "error";
      session.error = String(e);
      session.endedAt = Date.now();
      return session;
    })
    .finally(() => {
      // The backend flushes a final `done` chunk on its own schedule; give the
      // tail a beat to settle before the buffer stops being interesting.
      session.lines = session.lines.slice(-MAX_TAIL_LINES);
      pruneFinished();
      emit();
    });

  return { runId, done };
}

export function cancelRun(projectId: string): Promise<void> {
  return tauriInvoke("cancel_test_run", { projectId });
}

/** Drop one session from the dock (no-op while it is still running). */
export function removeRun(runId: string): void {
  const session = sessions.get(runId);
  if (!session || session.status === "running") return;
  sessions.delete(runId);
  ordered.splice(ordered.indexOf(runId), 1);
  emit();
}

/** Drop finished sessions from the dock (running ones stay). */
export function clearFinishedRuns(): void {
  for (const id of [...ordered]) {
    if (sessions.get(id)?.status !== "running") {
      sessions.delete(id);
      ordered.splice(ordered.indexOf(id), 1);
    }
  }
  emit();
}
