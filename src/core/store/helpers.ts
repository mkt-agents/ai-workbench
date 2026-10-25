import type { AIModelConfig, DshInstance, GitRepoSummary, GitScannedRepo, GitStatusEntry, InstallableVersion, RuntimeKind, RuntimeSwitchPlan, RuntimeSwitchResult, RuntimeVersion, Snippet } from '../types';

/** Set once the example below has been offered, so deleting it is final. */
export const EXAMPLE_SNIPPET_SEEDED_KEY = 'snippets-example-seeded';

/** The single example template, shown on an empty table to demo `{{param}}`. */
export function buildExampleSnippet(now = new Date().toISOString()): Snippet {
  return {
    id: 'seed-debug',
    name: '报错诊断',
    content: '错误信息：\n{{error}}\n\n请分析根因、最小复现与修复建议。',
    tags: 'debug,prompt',
    params: 'error',
    kind: 'prompt',
    scenario: 'general',
    useCount: 0,
    createdAt: now,
    updatedAt: now,
  };
}

/** Other defaults older builds seeded: id → original [name, content]. */
const LEGACY_SEED_SNIPPETS: Record<string, [string, string]> = {
  'seed-git-status': ['git status 简报', '请根据以下 git 状态，用中文总结当前改动风险与建议下一步：\n{{status}}'],
  'seed-curl': ['curl JSON POST', 'curl -X POST "{{url}}" -H "Content-Type: application/json" -d \'{{body}}\''],
  'seed-prompt-role': ['角色提示框架', '你是{{role}}。目标：{{goal}}。约束：{{constraints}}。请给出可执行步骤。'],
  'seed-commit': ['Commit message 模板', 'type(scope): summary\n\nWhy: {{why}}\nWhat: {{what}}'],
};

/** A seeded default the user never edited (name and content still original). */
export function isUntouchedLegacySeed(s: Pick<Snippet, 'id' | 'name' | 'content'>): boolean {
  const seed = LEGACY_SEED_SNIPPETS[s.id];
  return !!seed && seed[0] === s.name && seed[1] === s.content;
}

/**
 * Dynamically invoke a Tauri command. The dynamic import avoids a top-level
 * `@tauri-apps/api/core` import so tree-shaking and tests are unaffected.
 */
const tableTails = new Map<string, Promise<void>>();

/**
 * Run table loads and full-table saves one at a time. Callers must snapshot
 * store state inside `task`, after the previous write has landed.
 */
export function withTable<T>(table: string, task: () => Promise<T>): Promise<T> {
  const prev = tableTails.get(table) ?? Promise.resolve();
  const run = prev.then(task, task);
  tableTails.set(
    table,
    run.then(
      () => undefined,
      () => undefined,
    ),
  );
  return run;
}

export async function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<T>(cmd, args);
}

/**
 * Optimistic update with automatic rollback.
 *
 * Applies `next` to the store immediately, persists it, and rolls back to
 * `previous` if the persist fails. Re-throws the underlying error so callers
 * can surface it.
 */
export async function optimisticUpdate<T>(
  previous: T,
  next: T,
  apply: (value: T) => void,
  persist: (value: T) => Promise<void>,
): Promise<void> {
  apply(next);
  try {
    await persist(next);
  } catch (e) {
    apply(previous);
    throw e;
  }
}

// Re-export the types this module references so invocations.ts can import from one place.
export type { AIModelConfig, DshInstance, GitRepoSummary, GitScannedRepo, GitStatusEntry, InstallableVersion, RuntimeKind, RuntimeSwitchPlan, RuntimeSwitchResult, RuntimeVersion };
