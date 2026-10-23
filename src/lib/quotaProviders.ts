/** 账号额度看板右侧服务商栏：静态目录 + 已选卡片偏好 + 安装检测缓存。 */
import invocations from "../core/store/invocations";
import { quotaCardOrderLoad, quotaCardOrderSave } from "./quotaAccounts";
import type { InstalledAppInfo } from "../core/types";

/** 能查额度、能加到看板的服务商。 */
export type QuotaCardId = "workbuddy" | "cursor" | "grok-bot" | "codex" | "deepseek" | "qoder" | "claude" | "zcode" | "mimo" | "trae";

export interface QuotaProvider {
  id: string;
  name: string;
  domain: string;
  url: string;
  /** 匹配注册表 display_name 或商店包名（小写包含）；空数组 = 纯网页 / CLI 服务，不显示安装状态。
   *  商店应用用包名前缀（如 `openai.codex`），避免裸词误中同名第三方程序（如 Codex++）。 */
  keywords: string[];
  /** 有额度查询实现时才是卡片 id。Claude Code / Desktop 共用 `claude` 账号池。 */
  card: QuotaCardId | null;
}

/** 顺序即右侧栏顺序（栏内再把有账号的提前，组内保持此序）：
 *  编程 IDE / 插件（Grok Bot 共用 Cursor 账号，紧跟其后）→ 模型订阅 → API 余额。 */
export const QUOTA_PROVIDERS: QuotaProvider[] = [
  { id: "cursor", name: "Cursor", domain: "cursor.com", url: "https://cursor.com", keywords: ["cursor"], card: "cursor" },
  { id: "grok-bot", name: "Grok Bot", domain: "x.ai", url: "https://x.ai", keywords: ["grok bot"], card: "grok-bot" },
  { id: "qoder", name: "Qoder", domain: "qoder.cn", url: "https://qoder.cn", keywords: ["qoder"], card: "qoder" },
  { id: "workbuddy", name: "WorkBuddy", domain: "www.codebuddy.cn", url: "https://www.codebuddy.cn", keywords: ["codebuddy", "workbuddy"], card: "workbuddy" },
  { id: "zcode", name: "ZCode", domain: "bigmodel.cn", url: "https://bigmodel.cn", keywords: ["zcode"], card: "zcode" },
  { id: "chatgpt", name: "ChatGPT / Codex", domain: "openai.com", url: "https://chatgpt.com", keywords: ["openai.codex", "openai.chatgpt"], card: "codex" },
  { id: "claude-code", name: "Claude Code", domain: "claude.ai", url: "https://claude.ai/code", keywords: ["claude code"], card: "claude" },
  { id: "claude-desktop", name: "Claude Desktop", domain: "claude.com", url: "https://claude.com/download", keywords: ["claude desktop"], card: "claude" },
  { id: "deepseek", name: "DeepSeek Harness", domain: "www.deepseek.com", url: "https://www.deepseek.com", keywords: ["deepseek"], card: "deepseek" },
  { id: "mimo", name: "小米 MiMo", domain: "platform.xiaomimimo.com", url: "https://platform.xiaomimimo.com", keywords: ["mimo", "xiaomi mimo"], card: "mimo" },
  { id: "trae", name: "Trae CN", domain: "trae.cn", url: "https://www.trae.cn", keywords: ["trae"], card: "trae" },
  { id: "workbuddy-ai", name: "WorkBuddy AI", domain: "www.workbuddy.ai", url: "https://www.workbuddy.ai", keywords: ["workbuddy ai"], card: null },
  { id: "antigravity", name: "Antigravity", domain: "antigravity.google", url: "https://antigravity.google", keywords: ["antigravity"], card: null },
  { id: "grok-build", name: "Grok Build", domain: "x.ai", url: "https://x.ai", keywords: ["grok build"], card: null },
];

const ALL_CARDS: QuotaCardId[] = ["workbuddy", "cursor", "grok-bot", "codex", "deepseek", "qoder", "claude", "zcode", "mimo", "trae"];
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

/**
 * 看板卡片顺序：卡片键为 `<服务商>:<账号 id>`，未授权时是 `<服务商>:empty`。
 * 权威存储是后端 SQLite 的 app_kv（`quota_card_order_load/save`），重装、清
 * WebView 数据都不丢；localStorage 只是首帧种子与 DB 不可用时的降级回退。
 */
export function loadCardOrder(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(ORDER_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((k): k is string => typeof k === "string") : [];
  } catch {
    return [];
  }
}

/** 每次本地写顺序自增；用于丢弃晚到的 DB 校正回包（拖拽不能被闪回旧顺序）。 */
let orderWrites = 0;

export function saveCardOrder(order: string[]): void {
  orderWrites += 1;
  try {
    localStorage.setItem(ORDER_KEY, JSON.stringify(order));
  } catch {
    /* private mode / quota — the order just won't persist locally */
  }
  void quotaCardOrderSave(order).catch(() => undefined);
}

/**
 * 挂载时取权威顺序：DB 有值用 DB（回写 localStorage 供下次首帧即用）；
 * DB 为空而 localStorage 有旧顺序（升级首启），把 localStorage 迁移进 DB。
 */
export async function loadCardOrderAuthoritative(): Promise<string[]> {
  const myWrite = orderWrites;
  try {
    const dbOrder = await quotaCardOrderLoad();
    if (orderWrites !== myWrite) return loadCardOrder();
    if (dbOrder.length > 0) {
      orderWrites += 1; // 回写本地缓存，不算用户拖拽
      try {
        localStorage.setItem(ORDER_KEY, JSON.stringify(dbOrder));
      } catch {
        /* 同上 */
      }
      return dbOrder;
    }
    const local = loadCardOrder();
    if (local.length > 0) void quotaCardOrderSave(local).catch(() => undefined);
    return local;
  } catch {
    return loadCardOrder();
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
let storeTask: Promise<string[]> | null = null;

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

/** 微软商店包名（小写），同样整个会话只扫一次。 */
function scanStoreOnce(): Promise<string[]> {
  if (!storeTask) {
    storeTask = invocations
      .invokeScanStorePackages()
      .then((names) => names.map((n) => n.toLowerCase()))
      .catch(() => {
        storeTask = null;
        return [];
      });
  }
  return storeTask;
}

/** 按关键字匹配每家服务商的安装状态（注册表显示名 + 商店包名）；纯网页服务（无关键字）不出现在结果里。 */
export async function detectInstalled(): Promise<Record<string, InstallState>> {
  const [apps, storePackages] = await Promise.all([scanOnce(), scanStoreOnce()]);
  const result: Record<string, InstallState> = {};
  for (const p of QUOTA_PROVIDERS) {
    if (p.keywords.length === 0) continue;
    const matches = apps.filter((a) => {
      const name = a.display_name.toLowerCase();
      return p.keywords.some((k) => name.includes(k));
    });
    const inStore = storePackages.some((name) => p.keywords.some((k) => name.includes(k)));
    result[p.id] = {
      installed: matches.length > 0 || inStore,
      icon: matches.find((a) => a.icon)?.icon ?? null,
    };
  }
  return result;
}
