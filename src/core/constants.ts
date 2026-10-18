/**
 * Centralized constants for the frontend.
 * Magic numbers scattered across components live here.
 */

/** AI Hub context window character limit before truncation. */
export const AIHUB_CONTEXT_LIMIT = 2000;

/** Max number of snippets shown in the ai-hub picker. */
export const AIHUB_SNIPPET_PICK_LIMIT = 24;

/** Max number of dirty files shown per repo in ai-hub context. */
export const AIHUB_DIRTY_FILE_LIMIT = 30;

/** Clipboard context truncation limit (chars). */
export const AIHUB_CLIPBOARD_LIMIT = 800;

/** Max repos to scan for dirty state in ai-hub context. */
export const AIHUB_DIRTY_REPO_LIMIT = 5;

/** Default DeepSeek Harness port. */
export const DSH_DEFAULT_PORT = 3080;

/** Max recent projects kept in the store. */
export const RECENT_PROJECTS_LIMIT = 200;

/** Max workspaces to scan for dirty repos in ai-hub. */
export const AIHUB_WORKSPACE_SCAN_LIMIT = 8;

/** Default max_tokens for AI generation when not configured. */
export const AI_DEFAULT_MAX_TOKENS = 4096;

/** Min/max bounds for AI max_tokens. */
export const AI_MIN_TOKENS = 32;
export const AI_MAX_TOKENS = 4096;

/** AI Hub single-shot floor for max_tokens. */
export const AIHUB_MIN_MAX_TOKENS = 1536;
