/** Shared snippet param helpers for Snippets page and AI Hub. */

/** Prompt-kind snippet scenarios (labels: `ai.prompts.scenarios.*`). */
export const PROMPT_SCENARIOS = [
  "general",
  "translate",
  "product",
  "code",
  "agent",
  "copywriting",
] as const;

const LEGACY_PROMPT_TEMPLATES_KEY = "ai-workbench.prompt-studio.custom-templates";

type LegacyPromptTemplate = {
  id: string;
  scenario: string;
  title: string;
  body: string;
  at: number;
};

/**
 * One-shot: old Prompt Studio custom templates lived in localStorage.
 * Returns whatever is still there and clears the key so store can merge into snippets.
 */
export function takeLegacyCustomTemplates(): LegacyPromptTemplate[] {
  try {
    const raw = localStorage.getItem(LEGACY_PROMPT_TEMPLATES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    const allowed = new Set<string>(PROMPT_SCENARIOS);
    const items: LegacyPromptTemplate[] = [];
    for (const entry of parsed) {
      if (!entry || typeof entry !== "object") continue;
      const o = entry as Record<string, unknown>;
      if (typeof o.id !== "string" || !o.id) continue;
      if (typeof o.scenario !== "string" || !allowed.has(o.scenario)) continue;
      if (typeof o.title !== "string" || !o.title.trim()) continue;
      if (typeof o.body !== "string") continue;
      const at = typeof o.at === "number" && Number.isFinite(o.at) ? o.at : Date.now();
      items.push({
        id: o.id,
        scenario: o.scenario,
        title: o.title.trim(),
        body: o.body,
        at,
      });
    }
    if (items.length > 0) {
      try {
        localStorage.removeItem(LEGACY_PROMPT_TEMPLATES_KEY);
      } catch {
        /* ignore */
      }
    }
    return items.sort((a, b) => b.at - a.at);
  } catch {
    return [];
  }
}

export function fillParams(content: string, values: Record<string, string>): string {
  return content.replace(/\{\{(\w+)\}\}/g, (_, key: string) => values[key] ?? `{{${key}}}`);
}

export function parseParamNames(params: string, content: string): string[] {
  const fromField = params
    .split(/[,，\s]+/)
    .map((s) => s.trim())
    .filter(Boolean);
  if (fromField.length) return fromField;
  const found = new Set<string>();
  content.replace(/\{\{(\w+)\}\}/g, (_, k: string) => {
    found.add(k);
    return "";
  });
  return [...found];
}

/** Split comma/space tags from a snippet tags field. */
export function splitTags(tags: string): string[] {
  return tags
    .split(/[,，\s]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}
