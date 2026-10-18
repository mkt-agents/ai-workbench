const META_LINE =
  /^(?:\[(?:environment|profile|focus)\]|workspace:|current repo:|focused:|git identity:|recent commits|context from workbench:|attached files\/code)/i;

const PREAMBLE =
  /^(?:根据|基于)(?:你|以上|当前|提供|工作台|上下文|附件)|^(?:here(?:'s| is) (?:the|a) |based on (?:the )?(?:context|changes|diff))/i;

const CLOSING =
  /^(?:如果|若)(?:你|还)?(?:需要|想要|希望)|需要的话|随时告诉我|还有什么(?:可以|需要)|let me know|feel free to|happy to /i;

const FENCE_RE = /```[^\n]*\n([\s\S]*?)```/g;

export type KeepResult = { text: string; kind: "answer" | "code" };

/** Fenced blocks worth keeping. Diff dumps are the explanation, not the artifact. */
function fenceBodies(text: string): string[] {
  const out: string[] = [];
  for (const match of text.matchAll(FENCE_RE)) {
    const body = (match[1] || "").replace(/\n$/, "").trim();
    if (!body) continue;
    if (/^diff --git /m.test(body) || /^@@ /m.test(body)) continue;
    out.push(body);
  }
  return out;
}

/**
 * Text the user actually wants to copy or save. Drops a restated workbench
 * dump and a trailing "want me to continue?" offer. When the reply is an
 * explanation plus fenced code, only the fences are kept — unless `whole`
 * (polish, translate, summary, daily report).
 */
export function keepableAnswer(raw: string, whole = false): KeepResult {
  let text = raw.replace(/\r\n/g, "\n").trim();
  if (!text) return { text, kind: "answer" };

  if (!whole) {
    const fences = fenceBodies(text);
    if (fences.length > 0) return { text: fences.join("\n\n"), kind: "code" };
  }

  const ctxAt = text.search(/\n---\nContext from workbench:/);
  if (ctxAt > 0) text = text.slice(0, ctxAt).trim();

  const parts = text.split(/\n{2,}/).map((p) => p.trim()).filter(Boolean);
  while (parts.length && isPreamble(parts[0])) parts.shift();
  while (parts.length && CLOSING.test(parts[parts.length - 1])) parts.pop();

  const next = parts.join("\n\n").trim();
  return { text: next || raw.trim(), kind: "answer" };
}

function isPreamble(paragraph: string): boolean {
  const lines = paragraph.split("\n").map((l) => l.trim()).filter(Boolean);
  if (lines.length === 0) return false;
  if (lines.every((l) => META_LINE.test(l) || /^ {0,2}(?:---|\.\.\.)/.test(l))) return true;
  return lines.length === 1 && PREAMBLE.test(lines[0]) && lines[0].length < 80;
}
