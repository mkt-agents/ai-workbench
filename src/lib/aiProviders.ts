import type { AIModelConfig, AuthType, ModelCapabilityCache } from "../core/types";

export type AIProviderId = AIModelConfig["provider"];
export type AuthMode = "bearer" | "api-key" | "anthropic";

export type ModelPreset = {
  id: string;
  label?: string;
  modality?: string;
  /** Explicit provider metadata from /models, never inferred catalogue values. */
  maxOutputTokens?: number;
  contextWindowTokens?: number;
};

type ModelIdentity = { provider: string; baseUrl: string; model: string };

function normalizeModelIdentity(identity: ModelIdentity): ModelIdentity {
  return {
    provider: identity.provider.trim().toLowerCase(),
    baseUrl: identity.baseUrl.trim().replace(/\/+$/, ""),
    model: identity.model.trim(),
  };
}

/** Validate persisted/imported metadata and discard it after an identity change. */
export function readModelCapabilityCache(raw: unknown, identity: ModelIdentity): ModelCapabilityCache | undefined {
  let value: unknown = raw;
  if (typeof value === "string") {
    try { value = JSON.parse(value); } catch { return undefined; }
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const cache = value as Record<string, unknown>;
  if (typeof cache.provider !== "string" || typeof cache.baseUrl !== "string" || typeof cache.model !== "string") return undefined;
  const stored = normalizeModelIdentity({ provider: cache.provider, baseUrl: cache.baseUrl, model: cache.model });
  const current = normalizeModelIdentity(identity);
  if (!current.provider || !current.baseUrl || !current.model || stored.provider !== current.provider || stored.baseUrl !== current.baseUrl || stored.model !== current.model) return undefined;
  const validTokenCount = (n: unknown, min: number): n is number =>
    typeof n === "number" && Number.isInteger(n) && n >= min && n <= 2000000;
  const contextWindowTokens = validTokenCount(cache.contextWindowTokens, 4096) ? cache.contextWindowTokens : undefined;
  const maxOutputTokens = validTokenCount(cache.maxOutputTokens, 1) ? cache.maxOutputTokens : undefined;
  if (contextWindowTokens === undefined && maxOutputTokens === undefined) return undefined;
  // Copy only the public metadata fields, including when importing untrusted JSON.
  return { ...current, contextWindowTokens, maxOutputTokens };
}

export function capabilityCacheFromModel(identity: ModelIdentity, model?: ModelPreset): ModelCapabilityCache | undefined {
  if (!model || model.id.trim() !== identity.model.trim()) return undefined;
  return readModelCapabilityCache({
    ...identity,
    contextWindowTokens: model.contextWindowTokens,
    maxOutputTokens: model.maxOutputTokens,
  }, identity);
}

export type AIProviderMeta = {
  value: AIProviderId;
  label: string;
  shortLabel: string;
  defaultUrl: string;
  /** Prefill for new configs / provider switch — not a dropdown list. */
  defaultModel: string;
  authMode: AuthMode;
  /** Whether GET {base}/models is supported. */
  supportsModelList: boolean;
  urlsByAuthType?: Partial<Record<AuthType, string>>;
};

export const CUSTOM_MODEL_VALUE = "__custom__";

const MIMO_URLS: Partial<Record<AuthType, string>> = {
  api: "https://api.xiaomimimo.com/v1",
  token_plan: "https://token-plan-cn.xiaomimimo.com/v1",
};

export const AI_PROVIDERS: AIProviderMeta[] = [
  {
    value: "deepseek",
    label: "DeepSeek",
    shortLabel: "DS",
    defaultUrl: "https://api.deepseek.com/v1",
    defaultModel: "deepseek-chat",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "moonshot",
    label: "Kimi (Moonshot)",
    shortLabel: "Kimi",
    defaultUrl: "https://api.moonshot.cn/v1",
    defaultModel: "moonshot-v1-8k",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "qwen",
    label: "通义千问",
    shortLabel: "Qwen",
    defaultUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    defaultModel: "qwen-plus",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "zhipu",
    label: "智谱 GLM",
    shortLabel: "GLM",
    defaultUrl: "https://open.bigmodel.cn/api/paas/v4",
    defaultModel: "glm-4-flash",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "mimo",
    label: "小米 MiMo",
    shortLabel: "MiMo",
    defaultUrl: MIMO_URLS.api!,
    defaultModel: "mimo-v2.5-pro",
    authMode: "api-key",
    supportsModelList: true,
    urlsByAuthType: MIMO_URLS,
  },
  {
    value: "longcat",
    label: "LongCat",
    shortLabel: "Cat",
    defaultUrl: "https://api.longcat.chat/openai",
    defaultModel: "LongCat-2.0",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "agnes",
    label: "Agnes",
    shortLabel: "Agnes",
    defaultUrl: "https://api.agnes-ai.cn/v1",
    defaultModel: "agnes-2.5-flash",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "openai",
    label: "OpenAI",
    shortLabel: "AI",
    defaultUrl: "https://api.openai.com/v1",
    defaultModel: "gpt-4o",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "anthropic",
    label: "Anthropic",
    shortLabel: "Claude",
    defaultUrl: "https://api.anthropic.com",
    defaultModel: "claude-sonnet-4-20250514",
    authMode: "anthropic",
    supportsModelList: false,
  },
  {
    value: "google",
    label: "Google Gemini",
    shortLabel: "Gemini",
    defaultUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    defaultModel: "gemini-2.0-flash",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "groq",
    label: "Groq",
    shortLabel: "Groq",
    defaultUrl: "https://api.groq.com/openai/v1",
    defaultModel: "llama-3.3-70b-versatile",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "mistral",
    label: "Mistral",
    shortLabel: "Mistral",
    defaultUrl: "https://api.mistral.ai/v1",
    defaultModel: "mistral-large-latest",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "xai",
    label: "xAI (Grok)",
    shortLabel: "Grok",
    defaultUrl: "https://api.x.ai/v1",
    defaultModel: "grok-2-latest",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "openrouter",
    label: "OpenRouter",
    shortLabel: "OR",
    defaultUrl: "https://openrouter.ai/api/v1",
    defaultModel: "auto",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "siliconflow",
    label: "SiliconFlow",
    shortLabel: "SF",
    defaultUrl: "https://api.siliconflow.cn/v1",
    defaultModel: "deepseek-ai/DeepSeek-V3",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "together",
    label: "Together AI",
    shortLabel: "Together",
    defaultUrl: "https://api.together.xyz/v1",
    defaultModel: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "ollama",
    label: "Ollama (Local)",
    shortLabel: "Olla",
    defaultUrl: "http://localhost:11434/v1",
    defaultModel: "llama3.3",
    authMode: "bearer",
    supportsModelList: true,
  },
  {
    value: "custom",
    label: "Custom (OpenAI Compatible)",
    shortLabel: "...",
    defaultUrl: "",
    defaultModel: "",
    authMode: "bearer",
    supportsModelList: true,
  },
];

export function getProviderMeta(id: string): AIProviderMeta | undefined {
  return AI_PROVIDERS.find((p) => p.value === id);
}

export function getDefaultUrl(providerId: string, authType: AuthType = "api"): string {
  const meta = getProviderMeta(providerId);
  if (!meta) return "";
  if (meta.urlsByAuthType?.[authType]) return meta.urlsByAuthType[authType]!;
  return meta.defaultUrl;
}

export function isFetchedModel(model: string, fetched: ModelPreset[]): boolean {
  return fetched.some((m) => m.id === model);
}

export function resolveModelSelection(
  model: string,
  fetched: ModelPreset[]
): { selectValue: string; customModel: string } {
  if (!model.trim() || fetched.length === 0 || !isFetchedModel(model, fetched)) {
    return { selectValue: CUSTOM_MODEL_VALUE, customModel: model };
  }
  return { selectValue: model, customModel: "" };
}

export function providerIconLabel(providerId: string): string {
  return getProviderMeta(providerId)?.shortLabel ?? providerId.slice(0, 4);
}

export function providerSupportsModelList(providerId: string): boolean {
  return getProviderMeta(providerId)?.supportsModelList ?? true;
}
