/**
 * 天玑引擎的思考强度档位与「这个模型吃不吃」的判定。
 *
 * 只认能确定接受这两条协议参数的线路（与后端 `model_capability::thinking_kind` 同口径）：
 * - Anthropic 官方：`thinking.budget_tokens`（低 1024 / 中 2048 / 高 4096）；
 * - OpenAI 官方 o1 / o3 / o4 / gpt-5：`reasoning_effort`。
 * 其余（DeepSeek chat、qwen、各种兼容网关）后端会静默忽略，前端据此置灰下拉并提示。
 */

export const THINKING_LEVELS = ["low", "medium", "high"] as const;

export type ThinkingLevel = (typeof THINKING_LEVELS)[number];

/** 档位是否合法（空串 = 模型默认，不算非法）。 */
export function isThinkingLevel(value: string | undefined): value is ThinkingLevel {
  return Boolean(value) && (THINKING_LEVELS as readonly string[]).includes(value as string);
}

/** 该模型是否支持可调思考强度。 */
export function thinkingSupported(provider?: string, model?: string): boolean {
  const p = (provider ?? "").trim().toLowerCase();
  if (p === "anthropic") return true;
  const id = (model ?? "").trim().toLowerCase();
  return p === "openai" && /^(o1|o3|o4|gpt-5)/.test(id);
}
