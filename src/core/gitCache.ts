/**
 * Shared, cache-first store for repo card state.
 *
 * The repo page and the commit page used to keep separate copies of the same
 * summaries and re-read them with ~6 git subprocesses per repo on every visit
 * (plus a second full pass), which at a few dozen repos meant hundreds of
 * `git.exe` spawns per refresh. This module holds one copy, fetched through the
 * batched `git_summarize_repos` command, and pages render whatever it already
 * has while the refresh runs in the background.
 *
 * It is deliberately an external store rather than part of the persisted app
 * state: repo status goes stale in seconds and must never be restored from disk.
 */
import { tauriInvoke } from "./store/helpers";
import type { RepoBatchItem } from "./types";

/** How long a cached entry is considered good for a background refresh. */
const REPO_CACHE_TTL_MS = 5_000;

type Mode = "summary" | "status";

type Entry = { item: RepoBatchItem; mode: Mode; at: number };

const entries = new Map<string, Entry>();
/** Paths a write invalidated; must be re-read even inside the TTL. */
const stale = new Set<string>();
/** Paths whose background refresh is paused (a batch job is mid-flight). */
const held = new Map<string, number>();
/** Bumped on invalidate so in-flight reads cannot resurrect old state. */
const epoch = new Map<string, number>();
/** (mode, path) pairs already requested, so concurrent callers share one read. */
const pending = new Map<Mode, Set<string>>();

let listeners = new Set<() => void>();
let cachedSnapshot: Record<string, RepoBatchItem> = {};
let snapshotDirty = true;

function emit() {
  snapshotDirty = true;
  for (const listener of listeners) listener();
}

export function subscribeRepos(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}

/** Stable object identity between changes, as useSyncExternalStore requires. */
export function getRepoSnapshot(): Record<string, RepoBatchItem> {
  if (snapshotDirty) {
    const next: Record<string, RepoBatchItem> = {};
    for (const [path, entry] of entries) next[path] = entry.item;
    cachedSnapshot = next;
    snapshotDirty = false;
  }
  return cachedSnapshot;
}

export function reposLoaded(): boolean {
  return entries.size > 0;
}

function needsFetch(path: string, mode: Mode, force: boolean): boolean {
  if (force) return true;
  if (stale.has(path)) return true;
  const entry = entries.get(path);
  if (!entry) return true;
  // A summary-only entry cannot answer "which files changed".
  if (mode === "status" && entry.mode !== "status") return true;
  return Date.now() - entry.at > REPO_CACHE_TTL_MS;
}

async function fetchMode(paths: string[], mode: Mode): Promise<void> {
  const requested = pending.get(mode) ?? new Set<string>();
  const todo = paths.filter((p) => !requested.has(p));
  if (todo.length === 0) return;
  for (const p of todo) requested.add(p);
  pending.set(mode, requested);

  const startedAt = new Map(todo.map((p) => [p, epoch.get(p) ?? 0]));
  try {
    const items = await tauriInvoke<RepoBatchItem[]>("git_summarize_repos", {
      paths: todo,
      includeStatus: mode === "status",
    });
    let changed = false;
    for (const item of items) {
      // Something wrote to this repo while we were reading; keep it stale
      // instead of showing a pre-write snapshot as current.
      if ((epoch.get(item.path) ?? 0) !== (startedAt.get(item.path) ?? 0)) continue;
      entries.set(item.path, {
        item,
        mode,
        at: Date.now(),
      });
      stale.delete(item.path);
      changed = true;
    }
    if (changed) emit();
  } finally {
    const still = pending.get(mode);
    if (still) for (const p of todo) still.delete(p);
  }
}

/**
 * Refresh the given repos, fetching only what is missing, stale or too old.
 * Await it to be sure the snapshot settled; call it without await to let the
 * UI paint from cache first.
 */
export async function refreshRepos(
  paths: string[],
  options: { withStatus?: boolean; force?: boolean } = {}
): Promise<void> {
  const mode: Mode = options.withStatus ? "status" : "summary";
  const active = paths.filter(
    (p) => (held.get(p) ?? 0) === 0 && needsFetch(p, mode, options.force ?? false)
  );
  if (active.length === 0) return;
  await fetchMode(active, mode);
}

/** Mark repos as changed by a write, so the next refresh re-reads them. */
export function invalidateRepos(paths: string[]): void {
  let touched = false;
  for (const path of paths) {
    stale.add(path);
    epoch.set(path, (epoch.get(path) ?? 0) + 1);
    touched = true;
  }
  if (touched) emit();
}

/** Pause background refresh for these paths (batch pull/push in flight). */
export function holdRepos(paths: string[]): void {
  for (const path of paths) held.set(path, (held.get(path) ?? 0) + 1);
}

export function releaseRepos(paths: string[]): void {
  for (const path of paths) {
    const count = (held.get(path) ?? 0) - 1;
    if (count > 0) held.set(path, count);
    else held.delete(path);
  }
}

/** Forget a repo entirely (removed from the favourites list). */
export function dropRepos(paths: string[]): void {
  let changed = false;
  for (const path of paths) {
    if (entries.delete(path)) changed = true;
    stale.delete(path);
    held.delete(path);
  }
  if (changed) emit();
}
