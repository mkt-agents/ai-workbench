/**
 * Centralized constants for the frontend.
 * Magic numbers scattered across components live here.
 */

/** Quick ask context window character limit before truncation. */
export const QUICKASK_CONTEXT_LIMIT = 2000;

/** Max number of snippets shown in the quick-ask picker. */
export const QUICKASK_SNIPPET_PICK_LIMIT = 24;

/** Max number of dirty files shown per repo in quick-ask context. */
export const QUICKASK_DIRTY_FILE_LIMIT = 30;

/** Clipboard context truncation limit (chars). */
export const QUICKASK_CLIPBOARD_LIMIT = 800;

/** Max repos to scan for dirty state in quick-ask context. */
export const QUICKASK_DIRTY_REPO_LIMIT = 5;

/** Default DeepSeek Harness port. */
export const DSH_DEFAULT_PORT = 3080;

/** Max recent projects kept in the store. */
export const RECENT_PROJECTS_LIMIT = 200;

/** Max workspaces to scan for dirty repos in quick-ask. */
export const QUICKASK_WORKSPACE_SCAN_LIMIT = 8;

/** Default max_tokens for AI generation when not configured. */
export const AI_DEFAULT_MAX_TOKENS = 4096;

/** Min/max bounds for AI max_tokens. */
export const AI_MIN_TOKENS = 32;
export const AI_MAX_TOKENS = 4096;

/** Quick-ask single-shot floor for max_tokens. */
export const QUICKASK_MIN_MAX_TOKENS = 1536;
