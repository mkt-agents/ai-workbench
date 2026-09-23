/**
 * Parses the AI report's list lines back into structure.
 *
 * The prompt fixes two line shapes (see `output_rules` in
 * `src-tauri/src/report_commands.rs`):
 *
 *   - [P0] 场景名 ｜ 前置：… ｜ 步骤：… ｜ 预期：…
 *   - [已覆盖] 需求点 ｜ 依据：… ｜ 缺口：…
 *
 * Everything else stays a plain bullet. Pure and total: a line the model
 * mangled must never throw, it just falls back to being shown verbatim.
 */

export type Priority = "P0" | "P1" | "P2";

export type CoverageKey = "covered" | "partial" | "missing";

export interface AiLineParts {
  priority: Priority | null;
  verdict: { key: CoverageKey; text: string } | null;
  /** Text before the first separator — the scenario / requirement point. */
  title: string;
  /** `前置：…` style pairs. Empty when the line has no labelled parts. */
  fields: { label: string; value: string }[];
}

/** Full-width bar is what the prompt asks for; the half-width one is a common
 *  model slip, accepted only when it is unambiguous (nothing else split it). */
const SEPARATOR = /\s*｜\s*/;
const SEPARATOR_FALLBACK = /\s*\|\s*/;

const PRIORITY_PREFIX = /^\[(P[0-2])\]\s*/;
/** A leading bracket that is not a priority: a coverage verdict, or other tag. */
const TAG_PREFIX = /^\[([^\]]{1,12})\]\s*/;
const FIELD = /^([^：:]{1,10})[：:]\s*(.*)$/;

const VERDICTS: Record<string, CoverageKey> = {
  已覆盖: "covered",
  部分覆盖: "partial",
  未见对应改动: "missing",
};

/** Splits a line into separator-delimited parts, tolerating the half-width bar. */
function splitParts(text: string): string[] {
  const full = text.split(SEPARATOR);
  if (full.length > 1) return full;
  return text.split(SEPARATOR_FALLBACK);
}

/** Returns null when the line carries no structure worth rendering — the caller
 *  then keeps it as a plain bullet. */
export function parseAiLine(raw: string): AiLineParts | null {
  const text = raw.replace(/^\s*[-*]\s*/, "").trim();
  if (!text) return null;

  let rest = text;
  let priority: Priority | null = null;
  let verdict: AiLineParts["verdict"] = null;

  const priorityMatch = PRIORITY_PREFIX.exec(rest);
  if (priorityMatch) {
    priority = priorityMatch[1] as Priority;
    rest = rest.slice(priorityMatch[0].length);
  } else {
    const tagMatch = TAG_PREFIX.exec(rest);
    const key = tagMatch ? VERDICTS[tagMatch[1].trim()] : undefined;
    if (tagMatch && key) {
      verdict = { key, text: tagMatch[1].trim() };
      rest = rest.slice(tagMatch[0].length);
    }
  }

  const parts = splitParts(rest);
  if (parts.length < 2) return null;

  const fields: AiLineParts["fields"] = [];
  for (const part of parts.slice(1)) {
    const match = FIELD.exec(part.trim());
    // A part without a label means the model drifted off format; dropping it is
    // better than showing a half-labelled row.
    if (match) fields.push({ label: match[1].trim(), value: match[2].trim() });
  }
  if (fields.length === 0) return null;

  return { priority, verdict, title: parts[0].trim(), fields };
}
