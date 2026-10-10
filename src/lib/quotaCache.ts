/**
 * 账号额度看板的结果缓存：卡片键（`cursor:3` / `wb:5` …）→ 上次查询的 DTO。
 * 页面切走会卸载，靠它回来时立刻有数；DTO 本身不含任何令牌（后端单测钉住）。
 */

/** 所有额度 DTO 的公共部分；`at` 是后端写入的 unix 秒。 */
export interface QuotaResultBase {
  ok: boolean;
  message: string | null;
  at: number;
  needsReauth?: boolean;
}

/** 一张账号卡的整体状态：右侧栏状态点与工具条摘要共用。 */
export type CardStatus = "idle" | "ok" | "warn" | "bad" | "error" | "reauth";

const KEY = "aq-cache";

export function loadQuotaCache(): Record<string, QuotaResultBase> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, QuotaResultBase>)
      : {};
  } catch {
    return {};
  }
}

export function saveQuotaCache(results: Record<string, QuotaResultBase>): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(results));
  } catch {
    /* private mode / quota — the cache just won't persist */
  }
}

const DS_PEAK_KEY = "aq-ds-peak";

/**
 * DeepSeek 余额接口没有总额，进度条以见过的最高余额为满：键 `deepseek:<id>:<币种>`。
 * 充值后余额上涨，峰值随之上调，所以它近似「最近一次充值后的余额」。
 */
export function loadDsPeaks(): Record<string, number> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(DS_PEAK_KEY) ?? "{}");
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Record<string, number>) : {};
  } catch {
    return {};
  }
}

export function saveDsPeaks(peaks: Record<string, number>): void {
  try {
    localStorage.setItem(DS_PEAK_KEY, JSON.stringify(peaks));
  } catch {
    /* private mode / quota — the baseline just restarts next time */
  }
}

/** 超过该秒数的授权类结果在进页面时自动重查；WorkBuddy 不在此列。 */
export const STALE_AFTER_SECS = 600;

export function isStale(result: QuotaResultBase | undefined, nowMs: number): boolean {
  return !result || nowMs / 1000 - result.at > STALE_AFTER_SECS;
}
