import type { AiHubTurn } from "../../core/types";

/**
 * 任务模式：决定这一轮带哪些工具（后端按 `agent_tools::mode_allows` 收窄），
 * 也让「这是不是代码问题」不再靠正则猜。
 */
export type HarnessMode = "chat" | "code" | "doc" | "data" | "research" | "ops";

export const HARNESS_MODES: HarnessMode[] = ["chat", "code", "doc", "data", "research", "ops"];

/** 模式对应的路由：只有「对话」走纯聊天，其余都当项目任务处理。 */
export function modeRoute(mode: HarnessMode): "chat" | "project" {
  return mode === "chat" ? "chat" : "project";
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
export type HarnessBlock =
  | { kind: "text"; text: string }
  | { kind: "image"; mediaType: string; data: string }
  | { kind: "tool_use"; id: string; name: string; input: unknown }
  | { kind: "tool_result"; toolUseId: string; output: string; isError?: boolean };

export type HarnessMessage = {
  role: "user" | "assistant";
  blocks: HarnessBlock[];
  /** 展示用标记，不参与 provider 翻译。 */
  meta?: { digest?: boolean };
};

export function textBlock(text: string): HarnessBlock {
  return { kind: "text", text };
}

/**
 * 界面上的轮次 → 协议消息。
 *
 * 目前每轮只有一个 Text 块（工具调用仍记在 `turn.steps` 里，只用于展示）；
 * 等会话开始持久化原始块之后，这里会直接透传。空内容轮次跳过——Anthropic 会 400。
 */
export function turnsToMessages(turns: AiHubTurn[]): HarnessMessage[] {
  const out: HarnessMessage[] = [];
  for (const turn of turns) {
    if (!turn.content.trim()) continue;
    out.push({
      role: turn.role,
      blocks: [textBlock(turn.content)],
      ...(turn.digest ? { meta: { digest: true } } : {}),
    });
  }
  return out;
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
