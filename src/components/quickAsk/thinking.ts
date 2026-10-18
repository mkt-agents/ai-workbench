const OPEN = "<think>";
const CLOSE = "</think>";

/**
 * Reasoning models mix their chain of thought into the answer as `<think>…</think>`;
 * some providers move the thought into a separate field and leave a lone `</think>`
 * at the start of the content. Neither is part of the answer.
 * Same rule as Rust `ai_commands::strip_think_tags` — streamed chunks can only be
 * cleaned here, after they are joined.
 * `streaming`: also hide a half-arrived `<thi…` at the very end.
 */
export function stripThinking(text: string, streaming = false): string {
  const lower = text.toLowerCase();
  if (!lower.includes(OPEN) && !lower.includes(CLOSE) && !(streaming && lower.includes("<"))) {
    return text;
  }
  let i = 0;
  const firstClose = lower.indexOf(CLOSE);
  const firstOpen = lower.indexOf(OPEN);
  if (firstClose >= 0 && (firstOpen < 0 || firstClose < firstOpen)) {
    i = firstClose + CLOSE.length;
  }
  let out = "";
  for (;;) {
    const open = lower.indexOf(OPEN, i);
    if (open < 0) {
      out += text.slice(i);
      break;
    }
    out += text.slice(i, open);
    const close = lower.indexOf(CLOSE, open);
    // Unclosed: everything after it is still thinking.
    if (close < 0) return out.trimStart();
    i = close + CLOSE.length;
  }
  if (streaming) {
    const lt = out.lastIndexOf("<");
    const tail = lt >= 0 ? out.slice(lt).toLowerCase() : "";
    if (tail && tail.length < CLOSE.length && (OPEN.startsWith(tail) || CLOSE.startsWith(tail))) {
      out = out.slice(0, lt);
    }
  }
  return i > 0 || out.length !== text.length ? out.trimStart() : out;
}
