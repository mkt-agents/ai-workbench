/** Shared snippet param helpers for Snippets page and Quick Ask. */

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
