/**
 * Bounded-concurrency helpers shared by the Git pages.
 *
 * Both `GitReposPage` and `CommitChangelist` carried their own copy of these,
 * which had already diverged (one tracked errors, the other took a concurrency
 * argument). Keeping one implementation matters because every task here spawns
 * a git subprocess: the pool size *is* the process cap.
 */

/** Parallel git subprocesses per page. Raised by the batch summary command. */
export const GIT_CONCURRENCY = 4;

export async function mapPool<T>(
  items: T[],
  concurrency: number,
  fn: (item: T) => Promise<void>
): Promise<void> {
  if (items.length === 0) return;
  let next = 0;
  const workers = Array.from({ length: Math.min(concurrency, items.length) }, async () => {
    while (true) {
      const i = next++;
      if (i >= items.length) break;
      await fn(items[i]);
    }
  });
  await Promise.all(workers);
}

export type PoolResult = { ok: number; fail: number; errors: string[] };

export async function mapPoolCounted<T>(
  items: T[],
  concurrency: number,
  fn: (item: T) => Promise<void>,
  options?: {
    onProgress?: (done: number, total: number) => void;
    /** Formats one failure for the summary; callers keep their own wording. */
    describeError?: (item: T, error: unknown) => string;
  }
): Promise<PoolResult> {
  let ok = 0;
  let fail = 0;
  let done = 0;
  const errors: string[] = [];
  const total = items.length;
  await mapPool(items, concurrency, async (item) => {
    try {
      await fn(item);
      ok += 1;
    } catch (e) {
      fail += 1;
      errors.push(options?.describeError ? options.describeError(item, e) : String(e));
    } finally {
      done += 1;
      options?.onProgress?.(done, total);
    }
  });
  return { ok, fail, errors };
}
