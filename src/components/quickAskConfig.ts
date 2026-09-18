import type { QuickAskChips } from "../core/types";

export const CONTEXT_RULES =
  " You have access to context from the user's workbench below. Proactively use ALL relevant context to give specific, actionable answers. Never say 'I cannot access files' — the context includes everything available. If context is insufficient, say what is missing and still answer from what is given. Answer directly in the user's language; do not ask them to run git/shell commands themselves.";

export const BASE_SYSTEM =
  "You are an expert developer assistant embedded in AI Workbench. You help developers understand, debug, and improve their code. Be concise but thorough. When you see code changes, explain what they do and why. When you see errors, suggest fixes. Anticipate follow-up needs.";

export type TaskKind = "none" | "debug" | "explain" | "polish";

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
};

export const DEFAULT_CHIPS: QuickAskChips = {
  workspace: true,
  git: true,
  dirty: true,
  clipboard: false,
};

/** Coerce a stored task string back to a valid TaskKind. */
export function validTask(value: string): TaskKind {
  return value === "debug" || value === "explain" || value === "polish" ? value : "none";
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
