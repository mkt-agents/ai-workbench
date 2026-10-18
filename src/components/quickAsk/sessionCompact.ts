import type { QuickAskTurn } from "../../core/types";
import { MAX_DIGEST_CHARS, MAX_HISTORY_TURNS, MAX_SAVED_TURNS, MAX_TURN_CHARS } from "./config";

function clip(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (!flat) return "";
  return flat.length > max ? flat.slice(0, max) + "…" : flat;
}

/** One line per dropped round, appended under any summary already stored. */
export function foldDigest(previous: string, older: QuickAskTurn[]): string {
  const lines: string[] = [];
  const prior = previous.trim();
  if (prior) lines.push(prior);
  for (let i = 0; i < older.length; i += 2) {
    const question = older[i];
    const answer = older[i + 1];
    if (!question || question.digest) continue;
    const q = clip(question.content, 80);
    const a = clip(answer?.content ?? "", 140);
    const steps = (answer?.steps ?? [])
      .map((s) => s.summary.trim())
      .filter(Boolean)
      .slice(0, 3)
      .join("; ");
    const bits = [q && `Q: ${q}`, a && `A: ${a}`, steps && `Did: ${steps}`].filter(Boolean);
    if (bits.length > 0) lines.push(bits.join(" "));
  }
  let text = lines.join("\n");
  if (text.length <= MAX_DIGEST_CHARS) return text;
  text = text.slice(text.length - MAX_DIGEST_CHARS);
  const nl = text.indexOf("\n");
  if (nl >= 0 && nl < 240) text = text.slice(nl + 1);
  return `…\n${text}`;
}

/**
 * Keep the newest verbatim rounds. Anything older is folded into a single
 * digest turn at the front so the session and the model still see it.
 */
export function compactSession(turns: QuickAskTurn[]): QuickAskTurn[] {
  const previous = turns
    .filter((t) => t.digest)
    .map((t) => t.content)
    .join("\n")
    .trim();
  const rest = turns.filter((t) => !t.digest);
  const keep = MAX_SAVED_TURNS * 2;
  if (rest.length <= keep) {
    return previous ? [{ role: "assistant", content: previous, digest: true }, ...rest] : rest;
  }
  const older = rest.slice(0, rest.length - keep);
  const recent = rest.slice(rest.length - keep);
  const content = foldDigest(previous, older);
  if (!content) return recent;
  return [{ role: "assistant", content, digest: true }, ...recent];
}

export function sessionDigest(turns: QuickAskTurn[]): string {
  return turns
    .filter((t) => t.digest)
    .map((t) => t.content.trim())
    .filter(Boolean)
    .join("\n");
}

/** Recent verbatim rounds for the model. The digest is attached to the system prompt. */
export function requestHistory(turns: QuickAskTurn[]): QuickAskTurn[] {
  return turns
    .filter((t) => !t.digest)
    .slice(-MAX_HISTORY_TURNS * 2)
    .map((t) => ({
      role: t.role,
      content: clipTurn(historyContent(t)),
    }));
}

function historyContent(turn: QuickAskTurn): string {
  if (turn.role !== "assistant" || !turn.steps?.length) return turn.content;
  const lines = turn.steps.map((s) => `- ${s.summary}`).join("\n");
  return `${turn.content}\n\n[操作]\n${lines}`;
}

function clipTurn(text: string): string {
  return text.length > MAX_TURN_CHARS ? text.slice(0, MAX_TURN_CHARS) + "\n…(truncated)" : text;
}

export function withSessionDigest(system: string, turns: QuickAskTurn[]): string {
  const digest = sessionDigest(turns);
  if (!digest) return system;
  return (
    system +
    "\n\nEarlier turns of this conversation were compressed into the notes below. Treat them as facts that already happened. Do not recite the notes unless the user asks what was discussed.\n" +
    digest
  );
}