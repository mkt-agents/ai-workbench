/**
 * Module-level queue for vulnerability scans, mirroring src/core/testRuns.ts:
 * the queue must survive the page unmounting, and only ever run one scan at a
 * time (OSV rate limits are shared per IP, and the backend's CancelGuard key
 * is the project id, so two live scans of one project would collide).
 *
 * Not persisted — a queue is live process state. The listener for
 * `vuln-scan-progress` is installed once per app lifetime and never removed.
 */
import { listen } from "@tauri-apps/api/event";
import { useSyncExternalStore } from "react";
import { tauriInvoke } from "./store/helpers";
import type { VulnScanOutcome } from "./types";

export interface VulnScanTask {
  projectId: string;
  projectName: string;
  includeUntracked: boolean;
}

export interface ActiveScan extends VulnScanTask {
  phase: "deps" | "secrets" | "done" | string;
  done: number;
  total: number;
}

export interface ScanFinishedEvent {
  task: VulnScanTask;
  outcome?: VulnScanOutcome;
  error?: string;
}

const RESULTS_KEEP = 8;

let queue: VulnScanTask[] = [];
let active: ActiveScan | null = null;
let running = false;
const results: ScanFinishedEvent[] = [];
const finishedCallbacks = new Set<(event: ScanFinishedEvent) => void>();

let listeners = new Set<() => void>();
let cachedSnapshot: { queue: VulnScanTask[]; active: ActiveScan | null; results: ScanFinishedEvent[] } | null = null;

function emit() {
  cachedSnapshot = null;
  for (const listener of listeners) listener();
}

export function subscribeVulnScans(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}

function getSnapshot() {
  if (!cachedSnapshot) {
    cachedSnapshot = { queue: [...queue], active, results: [...results] };
  }
  return cachedSnapshot;
}

export function useVulnScans() {
  return useSyncExternalStore(subscribeVulnScans, getSnapshot, getSnapshot);
}

/** Fired for every scan that ends, in queue order. Returns the unsubscribe. */
export function onScanFinished(callback: (event: ScanFinishedEvent) => void): () => void {
  finishedCallbacks.add(callback);
  return () => {
    finishedCallbacks.delete(callback);
  };
}

export function enqueueScans(tasks: VulnScanTask[]): void {
  if (tasks.length === 0) return;
  // De-duplicate by project: re-queueing the active or pending project is noise.
  const known = new Set([...queue.map((t) => t.projectId), ...(active ? [active.projectId] : [])]);
  queue.push(...tasks.filter((t) => !known.has(t.projectId)));
  void pump();
}

export function cancelActiveScan(): void {
  if (!active) return;
  void tauriInvoke("cancel_request", { id: active.projectId });
}

async function pump(): Promise<void> {
  if (running) return;
  running = true;
  while (queue.length > 0) {
    const task = queue.shift() as VulnScanTask;
    active = { ...task, phase: "deps", done: 0, total: 0 };
    emit();
    try {
      const outcome = await tauriInvoke<VulnScanOutcome>("scan_project_vulns", {
        projectId: task.projectId,
        includeUntracked: task.includeUntracked,
      });
      pushResult({ task, outcome });
    } catch (e) {
      pushResult({ task, error: String(e) });
    }
    active = null;
    emit();
  }
  running = false;
}

function pushResult(event: ScanFinishedEvent): void {
  results.unshift(event);
  if (results.length > RESULTS_KEEP) results.length = RESULTS_KEEP;
  for (const callback of finishedCallbacks) callback(event);
}

// Installed once for the lifetime of the module, like testRuns' listener:
// progress must keep landing in the store even with the vuln page unmounted.
void listen<{ projectId: string; phase: string; done: number; total: number }>(
  "vuln-scan-progress",
  (event) => {
    const { projectId, phase, done, total } = event.payload;
    if (active && active.projectId === projectId) {
      active = { ...active, phase, done, total };
      emit();
    }
  }
);
