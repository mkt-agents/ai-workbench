/**
 * 模型配置页提供商品牌图标（Vite 打包自 `src/assets/ai-providers/`）。
 * 有图用图；custom / 未知回落 shortLabel 方块。
 */
import type { AIProviderId } from "./aiProviders";
import openai from "../assets/ai-providers/openai.png";
import anthropic from "../assets/ai-providers/anthropic.png";
import deepseek from "../assets/ai-providers/deepseek.svg";
import google from "../assets/ai-providers/google.png";
import moonshot from "../assets/ai-providers/moonshot.png";
import qwen from "../assets/ai-providers/qwen.png";
import zhipu from "../assets/ai-providers/zhipu.png";
import mimo from "../assets/ai-providers/mimo.png";
import longcat from "../assets/ai-providers/longcat.png";
import agnes from "../assets/ai-providers/agnes.png";
import groq from "../assets/ai-providers/groq.png";
import mistral from "../assets/ai-providers/mistral.png";
import xai from "../assets/ai-providers/xai.png";
import openrouter from "../assets/ai-providers/openrouter.png";
import siliconflow from "../assets/ai-providers/siliconflow.png";
import together from "../assets/ai-providers/together.png";
import ollama from "../assets/ai-providers/ollama.png";

const LOGOS: Partial<Record<AIProviderId, string>> = {
  openai,
  anthropic,
  deepseek,
  google,
  moonshot,
  qwen,
  zhipu,
  mimo,
  longcat,
  agnes,
  groq,
  mistral,
  xai,
  openrouter,
  siliconflow,
  together,
  ollama,
};

export function aiProviderLogoSrc(providerId: string): string | null {
  return LOGOS[providerId as AIProviderId] ?? null;
}
