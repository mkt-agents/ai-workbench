/**
 * 模型配置页提供商品牌图标（`public/ai-providers/`，安装包内本地文件）。
 * 有图用图；custom / 未知回落 shortLabel 方块。
 */
import type { AIProviderId } from "./aiProviders";

const BASE = "/ai-providers";

const LOGOS: Partial<Record<AIProviderId, string>> = {
  openai: `${BASE}/openai.png`,
  anthropic: `${BASE}/anthropic.png`,
  deepseek: `${BASE}/deepseek.svg`,
  google: `${BASE}/google.png`,
  moonshot: `${BASE}/moonshot.png`,
  qwen: `${BASE}/qwen.png`,
  zhipu: `${BASE}/zhipu.png`,
  mimo: `${BASE}/mimo.png`,
  longcat: `${BASE}/longcat.svg`,
  agnes: `${BASE}/agnes.svg`,
  groq: `${BASE}/groq.png`,
  mistral: `${BASE}/mistral.png`,
  xai: `${BASE}/xai.png`,
  openrouter: `${BASE}/openrouter.png`,
  siliconflow: `${BASE}/siliconflow.png`,
  together: `${BASE}/together.png`,
  ollama: `${BASE}/ollama.png`,
};

export function aiProviderLogoSrc(providerId: string): string | null {
  return LOGOS[providerId as AIProviderId] ?? null;
}
