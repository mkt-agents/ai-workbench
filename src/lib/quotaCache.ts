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

const CUSTOM_DRAFT_KEY = "aq-custom-draft";

/**
 * 自定义中转站表单的未提交草稿。填到一半要去别的页面翻地址 / 取 Key 时，
 * 卡片会随页面卸载，靠它把输入留在本机（成功入库后立刻删除，不长期留 Key）。
 */
export interface CustomDraft {
  baseUrl: string;
  apiKey: string;
  label: string;
}

export function loadCustomDraft(): CustomDraft | null {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(CUSTOM_DRAFT_KEY) ?? "null");
    if (!parsed || typeof parsed !== "object") return null;
    const d = parsed as Partial<CustomDraft>;
    return {
      baseUrl: typeof d.baseUrl === "string" ? d.baseUrl : "",
      apiKey: typeof d.apiKey === "string" ? d.apiKey : "",
      label: typeof d.label === "string" ? d.label : "",
    };
  } catch {
    return null;
  }
}

export function saveCustomDraft(draft: CustomDraft): void {
  try {
    localStorage.setItem(CUSTOM_DRAFT_KEY, JSON.stringify(draft));
  } catch {
    /* private mode / quota — 草稿只是不保留 */
  }
}

export function clearCustomDraft(): void {
  try {
    localStorage.removeItem(CUSTOM_DRAFT_KEY);
  } catch {
    /* 同上 */
  }
}

/** 草稿里还有内容（三个字段全空就不算，别每次进页面都弹表单）。 */
export function hasCustomDraft(draft: CustomDraft | null): boolean {
  return draft != null && (draft.baseUrl.trim() !== "" || draft.apiKey.trim() !== "" || draft.label.trim() !== "");
}

/** 超过该秒数的结果在进页面时自动重查（授权类逐卡查，WorkBuddy 走整池命令）。 */
export const STALE_AFTER_SECS = 600;

export function isStale(result: QuotaResultBase | undefined, nowMs: number): boolean {
  return !result || nowMs / 1000 - result.at > STALE_AFTER_SECS;
}
