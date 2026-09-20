/**
 * Helpers for the free-form text the model returns.
 *
 * The AI commands hand back plain prose (see test_commands.rs: one system + user
 * prompt), so anything the UI shows as a code block or a named section has to be
 * carved out here.
 */

/** Drop a leading/trailing ``` fence (with optional language tag) if the model wrapped the answer. */
export function stripCodeFence(text: string): string {
  const match = text.match(/^\s*```[^\n]*\n([\s\S]*?)\n```\s*$/);
  return match ? match[1].trim() : text.trim();
}

/** Keep only the body of the first fenced block, if any — used for suggested fixes. */
export function firstFencedBlock(text: string): string | null {
  const match = text.match(/```[^\n]*\n([\s\S]*?)```/);
  return match ? match[1].trim() : null;
}

/** Index a `## 标题` markdown document by its section titles. */
export function markdownSections(text: string): Record<string, string> {
  const sections: Record<string, string> = {};
  let current: string | null = null;
  let buffer: string[] = [];
  const flush = () => {
    if (current) sections[current] = buffer.join("\n").trim();
    buffer = [];
  };
  for (const line of text.split("\n")) {
    const heading = line.match(/^#{2,3}\s+(.+?)\s*$/);
    if (heading) {
      flush();
      current = heading[1].trim();
    } else if (current) {
      buffer.push(line);
    }
  }
  flush();
  return sections;
}
