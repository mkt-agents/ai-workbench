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
  // 当日桶跟总量一起记：算不出真实 token 时退回估算值，宁可保守也不当 0
  const tokens =
    (input.promptTokens ?? 0) + (input.completionTokens ?? 0) || (input.estimatedTokens ?? 0);
  const day = readAiUsageDay();
  try {
    localStorage.setItem(
      DAY_KEY,
      JSON.stringify({ date: day.date, calls: day.calls + 1, tokens: day.tokens + tokens })
    );
    recordHistory(tokens, input.modelId);
    window.dispatchEvent(new CustomEvent("ai-usage-changed"));
  } catch {
    /* ignore */
  }
}

/** 当日桶：预算按「今天用了多少 token」算，跨天自动归零（不保留历史）。 */
export type AiUsageDay = { date: string; calls: number; tokens: number };

const DAY_KEY = "workbench-ai-usage-day";

/** 本地日期键：不能用 toISOString（UTC），否则东八区的「今天」要到早上 8 点才翻篇 */
function localDateKey(d: Date): string {
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}-${m}-${day}`;
}

function todayKey(): string {
  return localDateKey(new Date());
}

export function readAiUsageDay(): AiUsageDay {
  const empty: AiUsageDay = { date: todayKey(), calls: 0, tokens: 0 };
  try {
    const raw = localStorage.getItem(DAY_KEY);
    if (!raw) return empty;
    const parsed = JSON.parse(raw) as AiUsageDay;
    // 换了日期就当没记录：不迁移、不累计
    if (!parsed || parsed.date !== empty.date) return empty;
    return {
      date: parsed.date,
      calls: Number(parsed.calls) || 0,
      tokens: Number(parsed.tokens) || 0,
    };
  } catch {
    return empty;
  }
}

export function clearAiUsage(): void {
  writeAiUsage({});
  try {
    localStorage.removeItem(DAY_KEY);
    localStorage.removeItem(HISTORY_KEY);
  } catch {
    /* ignore */
  }
  window.dispatchEvent(new CustomEvent("ai-usage-changed"));
}

/** 逐日用量（走势图用）：当日桶跨天归零，历史要留下来，只保留最近 [HISTORY_KEEP_DAYS] 天。
 *  `byModel` 记每个模型当天用了多少 token，走势图按模型各画一条曲线。 */
export type AiUsageHistoryPoint = {
  date: string;
  calls: number;
  tokens: number;
  byModel: Record<string, number>;
  /** 分模型调用次数：比 byModel 晚加的字段，旧数据没有，读取时按缺省 0 处理 */
  callsByModel?: Record<string, number>;
};

const HISTORY_KEY = "workbench-ai-usage-history";
const HISTORY_KEEP_DAYS = 30;

function readHistoryMap(): Record<string, AiUsageHistoryPoint> {
  try {
    const raw = localStorage.getItem(HISTORY_KEY);
    const parsed = raw ? (JSON.parse(raw) as Record<string, AiUsageHistoryPoint>) : null;
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

function recordHistory(tokens: number, modelId: string): void {
  const map = readHistoryMap();
  const key = todayKey();
  const prev = map[key] || { date: key, calls: 0, tokens: 0, byModel: {} };
  map[key] = {
    date: key,
    calls: prev.calls + 1,
    tokens: prev.tokens + tokens,
    byModel: { ...prev.byModel, [modelId]: (prev.byModel?.[modelId] ?? 0) + tokens },
    callsByModel: { ...prev.callsByModel, [modelId]: (prev.callsByModel?.[modelId] ?? 0) + 1 },
  };
  // 只留最近 30 天：按日期倒序砍掉旧的
  const kept = Object.values(map)
    .sort((a, b) => b.date.localeCompare(a.date))
    .slice(0, HISTORY_KEEP_DAYS);
  localStorage.setItem(HISTORY_KEY, JSON.stringify(Object.fromEntries(kept.map((d) => [d.date, d]))));
}

/** 最近 `days` 天的逐日用量，按日期升序；没有记录的天补 0，最后一天固定是今天。 */
export function readAiUsageHistory(days = 14): AiUsageHistoryPoint[] {
  const map = readHistoryMap();
  const out: AiUsageHistoryPoint[] = [];
  const now = new Date();
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(now.getFullYear(), now.getMonth(), now.getDate() - i);
    const key = localDateKey(d);
    const hit = map[key];
    out.push({
      date: key,
      calls: Number(hit?.calls) || 0,
      tokens: Number(hit?.tokens) || 0,
      byModel: hit?.byModel ?? {},
    });
  }
  return out;
}

export function usageList(map: AiUsageMap = readAiUsage()): AiUsageEntry[] {
  return Object.values(map).sort(
    (a, b) => b.calls - a.calls || (b.lastAt || "").localeCompare(a.lastAt || "")
  );
}
