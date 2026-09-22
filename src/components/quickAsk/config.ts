import type { QuickAskChips } from "../../core/types";
import { DEFAULT_PROMPT_GOALS } from "../../lib/promptOptimize";
import type { PromptGoal, PromptScenario } from "../../lib/promptOptimize";

export const CONTEXT_RULES =
  " You have access to context from the user's workbench below. Proactively use ALL relevant context to give specific, actionable answers. Never say 'I cannot access files' — the context includes everything available. If context is insufficient, say what is missing and still answer from what is given. Answer directly in the user's language; do not ask them to run git/shell commands themselves.";

export const BASE_SYSTEM =
  "You are an expert developer assistant embedded in AI Workbench. You help developers understand, debug, and improve their code. Be concise but thorough. When you see code changes, explain what they do and why. When you see errors, suggest fixes. Anticipate follow-up needs.";

export type TaskKind = "none" | "debug" | "explain" | "polish" | "translate" | "summarize" | "optimize";

export const TASK_KINDS: TaskKind[] = [
  "none",
  "debug",
  "explain",
  "polish",
  "translate",
  "summarize",
  "optimize",
];

export const TASK_SYSTEM: Record<TaskKind, string> = {
  none:
    BASE_SYSTEM +
    " Adapt your style to the question: conceptual explanations, code review, debugging, or planning." +
    CONTEXT_RULES,
  debug:
    BASE_SYSTEM +
    " Focus on diagnosing the problem. Read error messages and diffs carefully. Explain root cause, then give concrete fix steps. If the error is in shown code, point to the exact line." +
    CONTEXT_RULES,
  explain:
    BASE_SYSTEM +
    " Explain the code or concept clearly. Use examples from the provided context when possible. Structure complex explanations with bullet points or numbered steps." +
    CONTEXT_RULES,
  polish:
    BASE_SYSTEM +
    " Improve the text for clarity, tone, and professionalism. Return only the improved text unless asked otherwise. Match the user's language and intent." +
    CONTEXT_RULES,
  translate:
    BASE_SYSTEM +
    " Translate the given text faithfully. Use the target language the user specifies; if none is given, translate between Chinese and English (the opposite of the source). Preserve formatting, terminology, names and numbers. Output only the translation unless asked otherwise." +
    CONTEXT_RULES,
  summarize:
    BASE_SYSTEM +
    " Summarize the given text or code changes: key points first, then structured details. Be concise; keep numbers, names and technical terms exact." +
    CONTEXT_RULES,
  // Fallback only: the optimize flow uses buildPromptOptimizeMessages instead.
  optimize:
    BASE_SYSTEM +
    " Rewrite the user's draft into a clearer, actionable prompt. Output only the improved prompt." +
    CONTEXT_RULES,
};

export const DEFAULT_CHIPS: QuickAskChips = {
  workspace: true,
  git: true,
  dirty: true,
  clipboard: false,
};

export type OptimizeConfig = { scenario: PromptScenario; goals: PromptGoal[] };

export const DEFAULT_OPTIMIZE: OptimizeConfig = {
  scenario: "general",
  goals: DEFAULT_PROMPT_GOALS,
};

/**
 * Stored task string. The `optimize` task carries its scenario/goals inline
 * (`optimize:code:concrete,format`) so the SQLite `task` column needs no schema
 * change. Anything unparseable degrades to "none".
 */
export function parseTask(value: string): { task: TaskKind; optimize: OptimizeConfig } {
  if (value === "optimize" || value.startsWith("optimize:")) {
    const [, scenario, goals] = value.split(":");
    return {
      task: "optimize",
      optimize: {
        scenario: isScenario(scenario) ? scenario : DEFAULT_OPTIMIZE.scenario,
        goals: parseGoals(goals),
      },
    };
  }
  return {
    task: TASK_KINDS.includes(value as TaskKind) ? (value as TaskKind) : "none",
    optimize: DEFAULT_OPTIMIZE,
  };
}

export function encodeTask(task: TaskKind, optimize: OptimizeConfig): string {
  if (task !== "optimize") return task;
  return `optimize:${optimize.scenario}:${optimize.goals.join(",")}`;
}

function isScenario(v: string | undefined): v is PromptScenario {
  return v === "general" || v === "translate" || v === "product" || v === "code" || v === "agent" || v === "copywriting";
}

function parseGoals(v: string | undefined): PromptGoal[] {
  if (!v) return DEFAULT_OPTIMIZE.goals;
  const valid = v.split(",").filter(
    (g): g is PromptGoal =>
      g === "concrete" || g === "role" || g === "format" || g === "constraints" || g === "shorten" || g === "preserve"
  );
  return valid.length > 0 ? valid : DEFAULT_OPTIMIZE.goals;
}

/** Truncate text to max chars, appending a marker if clipped. */
export function truncate(text: string, max: number): { text: string; truncated: boolean } {
  if (text.length <= max) return { text, truncated: false };
  return { text: text.slice(0, max) + "\n…(truncated)", truncated: true };
}

/** Short session label from the first non-empty line of a question. */
export function makeTitle(question: string, fallback: string): string {
  const firstLine = question.split("\n").map((s) => s.trim()).find(Boolean) || "";
  if (!firstLine) return fallback;
  return firstLine.length > 24 ? firstLine.slice(0, 24) + "…" : firstLine;
}

export const CONTEXT_LIMIT = 12000;
export const SNIPPET_PICK_LIMIT = 24;
export const DIRTY_FILE_LIMIT = 30;
/** Follow-up context: keep the last N Q&A pairs, clipped per turn. */
export const MAX_HISTORY_TURNS = 6;
export const MAX_TURN_CHARS = 4000;
/** Max diff bytes per file when including dirty content in quick-ask context. */
export const MAX_DIFF_PER_FILE = 2000;
/** Max number of files to fetch diff for the PRIMARY repo in quick-ask context. */
export const MAX_DIFF_FILES_PRIMARY = 8;
/** Max number of files to fetch diff for SECONDARY repos (status only by default). */
export const MAX_DIFF_FILES_SECONDARY = 2;
/** How long (ms) a cached context stays fresh before auto-refresh. */
export const CONTEXT_TTL_MS = 120000;
/** Max repos to scan for dirty status (beyond the primary repo). */
export const MAX_SECONDARY_REPOS = 6;
