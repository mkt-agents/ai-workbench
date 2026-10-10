/** 账号额度看板右侧服务商栏：静态目录 + 已选卡片偏好 + 安装检测缓存。 */
import invocations from "../core/store/invocations";
import type { InstalledAppInfo } from "../core/types";

/** 能查额度、能加到看板的服务商。 */
export type QuotaCardId = "workbuddy" | "cursor" | "grok-bot" | "codex" | "deepseek" | "qoder" | "claude" | "zcode";

export interface QuotaProvider {
  id: string;
  name: string;
  domain: string;
  url: string;
  /** 匹配注册表 display_name（小写包含）；空数组 = 纯网页 / CLI 服务，不显示安装状态。 */
  keywords: string[];
  /** 有额度查询实现时才是卡片 id。Claude Code / Desktop 共用 `claude` 账号池。 */
  card: QuotaCardId | null;
}

export const QUOTA_PROVIDERS: QuotaProvider[] = [
  { id: "chatgpt", name: "ChatGPT / Codex", domain: "openai.com", url: "https://chatgpt.com", keywords: [], card: "codex" },
  { id: "claude-code", name: "Claude Code", domain: "claude.ai", url: "https://claude.ai/code", keywords: ["claude code"], card: "claude" },
  { id: "claude-desktop", name: "Claude Desktop", domain: "claude.com", url: "https://claude.com/download", keywords: ["claude desktop"], card: "claude" },
  { id: "deepseek", name: "DeepSeek Harness", domain: "www.deepseek.com", url: "https://www.deepseek.com", keywords: ["deepseek"], card: "deepseek" },
  { id: "workbuddy", name: "WorkBuddy", domain: "www.codebuddy.cn", url: "https://www.codebuddy.cn", keywords: ["codebuddy", "workbuddy"], card: "workbuddy" },
  { id: "cursor", name: "Cursor", domain: "cursor.com", url: "https://cursor.com", keywords: ["cursor"], card: "cursor" },
  { id: "grok-bot", name: "Grok Bot", domain: "x.ai", url: "https://x.ai", keywords: ["grok bot"], card: "grok-bot" },
  { id: "qoder", name: "Qoder", domain: "qoder.cn", url: "https://qoder.cn", keywords: ["qoder"], card: "qoder" },
  { id: "zcode", name: "ZCode", domain: "bigmodel.cn", url: "https://bigmodel.cn", keywords: ["zcode"], card: "zcode" },
  { id: "workbuddy-ai", name: "WorkBuddy AI", domain: "www.workbuddy.ai", url: "https://www.workbuddy.ai", keywords: ["workbuddy ai"], card: null },
  { id: "antigravity", name: "Antigravity", domain: "antigravity.google", url: "https://antigravity.google", keywords: ["antigravity"], card: null },
  { id: "grok-build", name: "Grok Build", domain: "x.ai", url: "https://x.ai", keywords: ["grok build"], card: null },
];

const ALL_CARDS: QuotaCardId[] = ["workbuddy", "cursor", "grok-bot", "codex", "deepseek", "qoder", "claude", "zcode"];
const CARDS_KEY = "aq-cards";

export function loadEnabledCards(): QuotaCardId[] {
  try {
    const raw = localStorage.getItem(CARDS_KEY);
    if (raw == null) return ALL_CARDS;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return ALL_CARDS;
    return ALL_CARDS.filter((id) => parsed.includes(id));
  } catch {
    return ALL_CARDS;
  }
}

export function saveEnabledCards(cards: QuotaCardId[]): void {
  try {
    localStorage.setItem(CARDS_KEY, JSON.stringify(cards));
  } catch {
    /* private mode / quota — the selection just won't persist */
  }
}

const ORDER_KEY = "aq-card-order";

/** 看板卡片顺序：卡片键为 `<服务商>:<账号 id>`，未授权时是 `<服务商>:empty`。 */
export function loadCardOrder(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(ORDER_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((k): k is string => typeof k === "string") : [];
  } catch {
    return [];
  }
}

export function saveCardOrder(order: string[]): void {
  try {
    localStorage.setItem(ORDER_KEY, JSON.stringify(order));
  } catch {
    /* private mode / quota — the order just won't persist */
  }
}

const HIDDEN_KEY = "aq-hidden";

/** 从看板隐藏的单张卡片（目前只有 WorkBuddy 账号卡：账号本身归 WorkBuddy 页管理）。 */
export function loadHiddenCards(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(HIDDEN_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((k): k is string => typeof k === "string") : [];
  } catch {
    return [];
  }
}

export function saveHiddenCards(keys: string[]): void {
  try {
    localStorage.setItem(HIDDEN_KEY, JSON.stringify(keys));
  } catch {
    /* private mode / quota — hidden cards just come back next time */
  }
}

/** 按已存顺序排；没记录过的卡片（新账号等）保持自然顺序排在后面。 */
export function sortByOrder<T extends { key: string }>(items: T[], order: string[]): T[] {
  const rank = (k: string) => {
    const i = order.indexOf(k);
    return i === -1 ? Number.MAX_SAFE_INTEGER : i;
  };
  return items
    .map((item, natural) => ({ item, natural }))
    .sort((a, b) => rank(a.item.key) - rank(b.item.key) || a.natural - b.natural)
    .map((x) => x.item);
}

export interface InstallState {
  installed: boolean;
  icon: string | null;
}

let scanTask: Promise<InstalledAppInfo[]> | null = null;

/** 注册表扫描较慢（含图标提取），整个会话只扫一次；失败时清空缓存，下次进页再试。 */
function scanOnce(): Promise<InstalledAppInfo[]> {
  if (!scanTask) {
    scanTask = invocations.invokeScanInstalledApps().catch(() => {
      scanTask = null;
      return [];
    });
  }
  return scanTask;
}

/** 按关键字匹配每家服务商的安装状态；纯网页服务（无关键字）不出现在结果里。 */
export async function detectInstalled(): Promise<Record<string, InstallState>> {
  const apps = await scanOnce();
  const result: Record<string, InstallState> = {};
  for (const p of QUOTA_PROVIDERS) {
    if (p.keywords.length === 0) continue;
    const matches = apps.filter((a) => {
      const name = a.display_name.toLowerCase();
      return p.keywords.some((k) => name.includes(k));
    });
    result[p.id] = {
      installed: matches.length > 0,
      icon: matches.find((a) => a.icon)?.icon ?? null,
    };
  }
  return result;
}
