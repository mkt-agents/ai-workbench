import type { AiHubBlock, AiHubTurn } from "../../core/types";

/**
 * 任务模式：决定这一轮带哪些工具（后端按 `agent_tools::mode_allows` 收窄），
 * 也让「这是不是代码问题」不再靠正则猜。
 */
export type HarnessMode = "chat" | "code" | "doc" | "data" | "research" | "ops";

export const HARNESS_MODES: HarnessMode[] = ["chat", "code", "doc", "data", "research", "ops"];

/** 产品轨：Code / Work，与任务模式正交；Work 默认钉住语义偏 doc。 */
export type HarnessTrack = "code" | "work";

/** 模式对应的路由：只有「对话」走纯聊天，其余都当项目任务处理。 */
export function modeRoute(mode: HarnessMode): "chat" | "project" {
  return mode === "chat" ? "chat" : "project";
}

/**
 * 有效任务模式：用户 `/mode` 钉住优先；否则 Work 轨默认 `doc`，Code 轨空串（后端不收窄）。
 */
export function effectiveTaskMode(
  pinned: HarnessMode | null | undefined,
  track: HarnessTrack | string | undefined,
): string {
  if (pinned) return pinned;
  return track === "work" ? "doc" : "";
}

/**
 * 内容块协议（Rust 侧 `src-tauri/src/agent_protocol.rs` 的镜像）。
 *
 * 为什么要有这一层：以前历史只传 `{ role, content }` 纯文本，模型看不到自己上一轮
 * 调过什么工具、拿到了什么。协议化之后 `tool_use` / `tool_result` 可以和文本一样留在
 * 历史里；后端负责翻成 OpenAI / Anthropic 各自的形状。
 *
 * 字段名必须与 Rust 一致：`kind` 用 snake_case，其余 camelCase。
 */
export type HarnessBlock = AiHubBlock;

export type HarnessMessage = {
  role: "user" | "assistant";
  blocks: HarnessBlock[];
  /** 展示用标记，不参与 provider 翻译。 */
  meta?: { digest?: boolean };
};

export function textBlock(text: string): HarnessBlock {
  return { kind: "text", text };
}

/** 单条工具结果回灌进历史时的字符上限（与 UI STEP_DETAIL_KEEP 对齐量级，避免爆预算）。 */
export const BLOCK_TOOL_OUTPUT_CAP = 2000;
/** 一整轮工具结果合计上限；超出后更早的结果被截短。 */
export const BLOCK_TOOL_BUDGET = 12_000;

type StepLike = {
  tool: string;
  status: string;
  summary: string;
  arg?: string;
  detail?: string;
};

/**
 * 把一轮的 steps + 最终回答折成可持久化的内容块。
 * 形状：若干 tool_use + 对应 tool_result + 最终 text（有的话）。
 * 没有工具步骤时只留 text，与旧会话一致。
 */
export function stepsToBlocks(answer: string, steps?: StepLike[]): HarnessBlock[] | undefined {
  const usable = (steps ?? []).filter((s) => s.tool && s.tool !== "conflict" && s.summary);
  if (usable.length === 0) {
    const text = answer.trim();
    return text ? [textBlock(answer)] : undefined;
  }
  const blocks: HarnessBlock[] = [];
  let budget = BLOCK_TOOL_BUDGET;
  usable.forEach((step, i) => {
    const id = `hist-${i}-${step.tool}`;
    const input =
      step.arg && step.arg.trim()
        ? { arg: step.arg.slice(0, 500) }
        : { summary: step.summary.slice(0, 200) };
    blocks.push({ kind: "tool_use", id, name: step.tool, input });
    const isError = step.status === "error" || step.status === "denied";
    const raw = (step.detail || step.summary || "").trim();
    const cap = Math.min(BLOCK_TOOL_OUTPUT_CAP, Math.max(120, budget));
    const output =
      raw.length > cap ? `${raw.slice(0, cap)}\n…(truncated)` : raw || step.summary;
    budget = Math.max(0, budget - output.length);
    blocks.push({
      kind: "tool_result",
      toolUseId: id,
      output,
      ...(isError ? { isError: true } : {}),
    });
  });
  if (answer.trim()) blocks.push(textBlock(answer));
  return blocks;
}

/**
 * 界面上的轮次 → 协议消息。
 *
 * 有 `blocks` 时按协议拆开：assistant 的 tool_use（+ 可选 text）一条，
 * tool_result 跟一条 user（OpenAI 侧翻成 role:tool）；旧会话回退纯文本。
 */
export function turnsToMessages(turns: AiHubTurn[]): HarnessMessage[] {
  const out: HarnessMessage[] = [];
  for (const turn of turns) {
    const meta = turn.digest ? { digest: true as const } : undefined;
    if (turn.blocks && turn.blocks.length > 0) {
      out.push(...expandBlocks(turn.role, turn.blocks, meta));
      continue;
    }
    if (!turn.content.trim()) continue;
    out.push({
      role: turn.role,
      blocks: [textBlock(turn.content)],
      ...(meta ? { meta } : {}),
    });
  }
  return out;
}

/** 把混在一轮里的块拆成 provider 友好的消息序列。 */
function expandBlocks(
  role: "user" | "assistant",
  blocks: HarnessBlock[],
  meta?: { digest: true },
): HarnessMessage[] {
  if (role === "user") {
    return [{ role: "user", blocks, ...(meta ? { meta } : {}) }];
  }
  const uses: HarnessBlock[] = [];
  const results: HarnessBlock[] = [];
  const texts: HarnessBlock[] = [];
  const images: HarnessBlock[] = [];
  for (const block of blocks) {
    if (block.kind === "tool_use") uses.push(block);
    else if (block.kind === "tool_result") results.push(block);
    else if (block.kind === "text") texts.push(block);
    else images.push(block);
  }
  const out: HarnessMessage[] = [];
  if (uses.length > 0) {
    out.push({ role: "assistant", blocks: uses });
  }
  if (results.length > 0) {
    // user 角色承载 tool_result → provider_messages 翻成 OpenAI role:tool / Anthropic user 块
    out.push({ role: "user", blocks: results });
  }
  const closing = [...texts, ...images];
  if (closing.length > 0) {
    out.push({ role: "assistant", blocks: closing, ...(meta ? { meta } : {}) });
  } else if (out.length === 0 && meta) {
    // digest 轮：至少留一条空文本标记，避免被整轮丢掉
    out.push({ role: "assistant", blocks: [textBlock("")], meta });
  }
  return out.filter((m) => m.blocks.length > 0);
}

/** 这一问本身：正文与附件文本合成一条 user 消息，图片按块追加在后面。 */
export function buildUserMessage(text: string, images: { mediaType: string; data: string }[] = []): HarnessMessage {
  const blocks: HarnessBlock[] = [];
  if (text.trim()) blocks.push(textBlock(text));
  for (const image of images) {
    blocks.push({ kind: "image", mediaType: image.mediaType, data: image.data });
  }
  return { role: "user", blocks };
}

/** 旧后端兼容：把协议消息退回成纯文本轮次（消息里没有 tool 块时用）。 */
export function messagesToTurns(messages: HarnessMessage[]): { role: string; content: string }[] {
  return messages
    .filter((msg) => msg.blocks.length > 0)
    .map((msg) => ({
      role: msg.role,
      content: msg.blocks
        .filter((block): block is { kind: "text"; text: string } => block.kind === "text")
        .map((block) => block.text)
        .join("\n"),
    }))
    .filter((turn) => turn.content.trim() !== "");
}
