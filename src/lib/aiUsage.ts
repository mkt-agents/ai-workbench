/** Local-only AI usage ledger (localStorage). No cloud billing. */

const KEY = "workbench-ai-usage";

export type AiUsageEntry = {
  modelId: string;
  modelName: string;
  calls: number;
  promptTokens: number;
  completionTokens: number;
  estimatedTokens: number;
  lastAt: string;
};

export type AiUsageMap = Record<string, AiUsageEntry>;

export function readAiUsage(): AiUsageMap {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as AiUsageMap;
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

export function writeAiUsage(map: AiUsageMap): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(map));
    window.dispatchEvent(new CustomEvent("ai-usage-changed"));
  } catch {
    /* ignore */
  }
}

export function recordAiUsage(input: {
  modelId: string;
  modelName: string;
  promptTokens?: number;
  completionTokens?: number;
  estimatedTokens?: number;
}): void {
  if (!input.modelId) return;
  const map = readAiUsage();
  const prev = map[input.modelId] || {
    modelId: input.modelId,
    modelName: input.modelName,
    calls: 0,
    promptTokens: 0,
    completionTokens: 0,
    estimatedTokens: 0,
    lastAt: "",
  };
  map[input.modelId] = {
    ...prev,
    modelName: input.modelName || prev.modelName,
    calls: prev.calls + 1,
    promptTokens: prev.promptTokens + (input.promptTokens ?? 0),
    completionTokens: prev.completionTokens + (input.completionTokens ?? 0),
    estimatedTokens: prev.estimatedTokens + (input.estimatedTokens ?? 0),
    lastAt: new Date().toISOString(),
  };
  writeAiUsage(map);
}

export function clearAiUsage(): void {
  writeAiUsage({});
}

export function usageList(map: AiUsageMap = readAiUsage()): AiUsageEntry[] {
  return Object.values(map).sort(
    (a, b) => b.calls - a.calls || (b.lastAt || "").localeCompare(a.lastAt || "")
  );
}
