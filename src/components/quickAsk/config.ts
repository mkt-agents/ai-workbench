import type { AIModelConfig, QuickAskChips } from "../../core/types";
import { DEFAULT_PROMPT_GOALS } from "../../lib/promptOptimize";
import type { PromptGoal, PromptScenario } from "../../lib/promptOptimize";
import type { AskMode } from "./route";

/** Only appended when a non-empty workbench context block is actually attached. */
export const CONTEXT_RULES =
  " Context collected from the user's workbench (repos, uncommitted changes, recent commits) is attached after the question. Use the relevant parts to give specific, actionable answers, citing file names and changes only when the user asked about those files. Never claim you cannot access files — the context is everything available. If it is insufficient, say exactly what is missing and still answer from what is given. Do not ask the user to run git/shell commands themselves. Do not echo the context block, git identity, or workspace paths back. The user copies and saves the reply as-is, so leave out preambles and closing offers.";

/** Plain conversation: a capable general assistant, no repo assumptions. */
export const CHAT_SYSTEM =
  "You are a capable, friendly general-purpose AI assistant. Answer the question directly and concisely; use structure (lists, short code blocks, tables) only when it helps. Do not mention or ask for project files, repositories or workbench context unless the user brings them up. Reply in the user's language. The user copies and saves your reply as-is: do not restate hidden context, and do not add a closing offer to continue.";

/** Project mode: developer assistant grounded in the attached workbench context. */
export const PROJECT_SYSTEM =
  "You are an expert developer assistant embedded in AI Workbench. You help developers understand, debug, and improve their code. Be concise but thorough. When you see code changes, explain what they do and why. When you see errors, suggest fixes. Reply in the user's language.";

export type TaskKind = "none" | "debug" | "explain" | "polish" | "translate" | "summarize" | "optimize";

/** One-shot quick actions shown in the composer (everything except plain "none"). */
export const QUICK_TASKS: TaskKind[] = ["explain", "debug", "polish", "translate", "summarize", "optimize"];

/** Task-specific instruction appended after the mode prompt. */
export const TASK_HINT: Record<TaskKind, string> = {
  none: "",
  debug:
    " Task: diagnose the problem. Read error messages and diffs carefully. Explain the root cause, then give concrete fix steps; point to the exact file/line when visible.",
  explain:
    " Task: explain the code or concept clearly, with examples. Structure complex explanations with bullet points or numbered steps.",
  polish:
    " Task: polish the user's text for clarity, tone and professionalism. Return only the improved text unless asked otherwise. Match the user's language and intent.",
  translate:
    " Task: translate the given text faithfully. Use the target language the user specifies; if none, translate between Chinese and English (the opposite of the source). Preserve formatting, terms, names and numbers. Output only the translation.",
  summarize:
    " Task: summarize the given text or changes — key points first, then structured details. Keep numbers, names and technical terms exact.",
  // Fallback only: the optimize flow uses buildPromptOptimizeMessages instead.
  optimize: " Task: rewrite the user's draft into a clearer, actionable prompt. Output only the improved prompt.",
};

/**
 * Self-knowledge block prepended to every system prompt, so "which model are
 * you" is answered from facts instead of guessed from (absent) context.
 */
export function buildMeta(model: AIModelConfig, now: Date = new Date()): string {
  const date = now.toISOString().slice(0, 10);
  return (
    `[Environment] You are answering inside "AI Workbench · Quick Ask", a local Windows desktop developer toolbox. ` +
    `The model serving this conversation is configured as "${model.name}" (model id: ${model.model}, provider: ${model.provider}). ` +
    `If the user asks which model / who you are, answer with exactly this configuration — do not guess another vendor or model. ` +
    `Today is ${date}.\n\n`
  );
}

/** Full system prompt for one request. Project rules apply only when context is attached. */
export function buildSystem(
  model: AIModelConfig,
  mode: AskMode,
  task: TaskKind,
  hasProjectContext: boolean,
): string {
  const base = mode === "project" ? PROJECT_SYSTEM : CHAT_SYSTEM;
  const rules = mode === "project" && hasProjectContext ? CONTEXT_RULES : "";
  return buildMeta(model) + base + TASK_HINT[task] + rules;
}

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
    task: (QUICK_TASKS as string[]).includes(value) ? (value as TaskKind) : "none",
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
/** Follow-up context: send the last N Q&A pairs to the model, clipped per turn. */
export const MAX_HISTORY_TURNS = 50;
/** Verbatim Q&A pairs kept on screen and in the saved session. */
export const MAX_SAVED_TURNS = 50;
/** Older rounds fold into one note once the verbatim window is full. */
export const MAX_DIGEST_CHARS = 2800;
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
