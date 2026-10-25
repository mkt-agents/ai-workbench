import type { AiHubBlock, AiHubTurn } from "../../core/types";
import { MAX_DIGEST_CHARS, MAX_HISTORY_CHARS, MAX_HISTORY_TURNS, MAX_SAVED_TURNS, MAX_TURN_CHARS } from "./config";
import { BLOCK_TOOL_OUTPUT_CAP } from "./harness";

function clip(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (!flat) return "";
  return flat.length > max ? flat.slice(0, max) + "…" : flat;
}

/** 预算裁剪时缩短 tool_result，保留 tool_use id 对齐。 */
function clipBlocks(blocks: AiHubBlock[]): AiHubBlock[] {
  return blocks.map((block) => {
    if (block.kind === "text") {
      return block.text.length > MAX_TURN_CHARS
        ? { ...block, text: block.text.slice(0, MAX_TURN_CHARS) + "\n…(truncated)" }
        : block;
    }
    if (block.kind === "tool_result" && block.output.length > BLOCK_TOOL_OUTPUT_CAP) {
      return { ...block, output: block.output.slice(0, BLOCK_TOOL_OUTPUT_CAP) + "\n…(truncated)" };
    }
    if (block.kind === "image") {
      // 历史里的截图不重发：换成短占位，省 token
      return { kind: "text", text: "[早先的截图已省略]" };
    }
    return block;
  });
}

function blocksChars(blocks: AiHubBlock[] | undefined): number {
  if (!blocks?.length) return 0;
  return blocks.reduce((n, b) => {
    if (b.kind === "text") return n + b.text.length;
    if (b.kind === "tool_result") return n + b.output.length;
    if (b.kind === "tool_use") return n + b.name.length + 40;
    return n;
  }, 0);
}

/** One line per dropped round, appended under any summary already stored. */
export function foldDigest(previous: string, older: AiHubTurn[]): string {
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
export function compactSession(turns: AiHubTurn[]): AiHubTurn[] {
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

export function sessionDigest(turns: AiHubTurn[]): string {
  return turns
    .filter((t) => t.digest)
    .map((t) => t.content.trim())
    .filter(Boolean)
    .join("\n");
}

/**
 * Recent verbatim rounds for the model. The digest is attached to the system prompt.
 * Past `maxChars` the oldest rounds are dropped — 50 × 4000 chars would blow the
 * context window and the provider then answers with an opaque 400.
 * 保留 `blocks`（含 tool_use / tool_result），好让下一轮模型看见上一轮工具事实。
 */
export function requestHistory(
  turns: AiHubTurn[],
  maxChars = MAX_HISTORY_CHARS,
): { turns: AiHubTurn[]; dropped: number } {
  const pairs: AiHubTurn[] = turns
    .filter((t) => !t.digest)
    .slice(-MAX_HISTORY_TURNS * 2)
    .map((t) => {
      const content = clipTurn(historyContent(t));
      const blocks = t.blocks?.length ? clipBlocks(t.blocks) : undefined;
      return {
        role: t.role,
        content,
        ...(blocks ? { blocks } : {}),
        ...(t.steps?.length ? { steps: t.steps } : {}),
      };
    });
  let total = pairs.reduce(
    (n, t) => n + Math.max(t.content.length, blocksChars(t.blocks)),
    0,
  );
  let dropped = 0;
  while (total > maxChars && pairs.length - dropped > 1) {
    const gone = pairs[dropped];
    total -= Math.max(gone.content.length, blocksChars(gone.blocks));
    dropped += 1;
  }
  return { turns: pairs.slice(dropped), dropped };
}

function historyContent(turn: AiHubTurn): string {
  if (turn.role !== "assistant" || !turn.steps?.length) return turn.content;
  // Mark what did not happen, so a follow-up does not assume a refused / failed step ran.
  const mark = (status: string) =>
    status === "error" ? "（失败）" : status === "denied" ? "（未执行）" : "";
  const lines = turn.steps.map((s) => `- ${s.summary}${mark(s.status)}`).join("\n");
  return `${turn.content}\n\n[操作]\n${lines}`;
}

function clipTurn(text: string): string {
  return text.length > MAX_TURN_CHARS ? text.slice(0, MAX_TURN_CHARS) + "\n…(truncated)" : text;
}

export function withSessionDigest(system: string, turns: AiHubTurn[]): string {
  const digest = sessionDigest(turns);
  if (!digest) return system;
  return (
    system +
    "\n\nEarlier turns of this conversation were compressed into the notes below. Treat them as facts that already happened. Do not recite the notes unless the user asks what was discussed.\n" +
    digest
  );
}