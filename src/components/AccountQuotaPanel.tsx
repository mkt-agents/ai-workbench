import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type DragEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import {
  Bot,
  Clock,
  ExternalLink,
  EyeOff,
  GripVertical,
  KeyRound,
  LogIn,
  RefreshCw,
  Trash2,
  UserPlus,
  CalendarCheck,
  CalendarClock,
} from "lucide-react";
import useTauriEvent from "../hooks/useTauriEvent";
import {
  wbCheckinNow,
  wbDeleteAccount,
  wbGetSettings,
  wbListAccounts,
  wbQuotaStatus,
  wbUpdateSettings,
  hasCheckinEndpoint,
  WB_CHECKIN_EVENT,
  WB_QUOTA_EVENT,
  type WbAccount,
  type WbCheckinResult,
  type WbSettings,
  type WbCreditPackage,
  type WbQuotaStatus,
} from "../lib/workbuddy";
import { cursorQuotaFetch, type CursorQuotaDto } from "../lib/cursorQuota";
import { grokBotQuotaFetch, type GrokQuotaDto } from "../lib/grokBotQuota";
import {
  qoderCampaignClaim,
  qoderLaunchApp,
  qoderOauthStart,
  qoderQuotaFetch,
  type QoderCampaign,
  type QoderClaimResult,
  type QoderQuota,
  type QoderQuotaDto,
} from "../lib/qoderQuota";
import { invoke } from "@tauri-apps/api/core";
import {
  codexDeleteAccount,
  codexListAccounts,
  codexOauthCancel,
  codexOauthStart,
  codexQuotaFetch,
  type CodexAccount,
  type CodexQuotaDto,
  type CodexWindow,
} from "../lib/codexQuota";
import {
  cursorOauthCancel,
  cursorOauthStart,
  deepseekAddKey,
  deepseekBalanceFetch,
  quotaAccountDelete,
  quotaAccountsList,
  type DeepSeekQuotaDto,
  type QuotaAccount,
} from "../lib/quotaAccounts";
import { useConfirm } from "./ConfirmModal";
import { cardBrandLogoSrc } from "../lib/providerLogos";
import ScanAddFlow from "./workbuddy/ScanAddFlow";
import ManualAddModal from "./workbuddy/ManualAddModal";
import {
  loadCardOrder,
  loadEnabledCards,
  loadHiddenCards,
  saveCardOrder,
  saveEnabledCards,
  saveHiddenCards,
  sortByOrder,
  type QuotaCardId,
} from "../lib/quotaProviders";
import {
  isStale,
  loadDsPeaks,
  loadQuotaCache,
  saveDsPeaks,
  saveQuotaCache,
  type CardStatus,
  type QuotaResultBase,
} from "../lib/quotaCache";
import QuotaProviderRail, { type RailAccount } from "./QuotaProviderRail";
import "./AccountQuotaPanel.css";

/**
 * 账号额度看板 —— 「账号管理 → 账号额度」。
 *
 * 一个账号一张紧凑卡，所有进度条都按「剩余」填充。查询结果按卡片键缓存在
 * localStorage，切页回来立刻有数；授权类账号（Cursor / Grok Bot / Codex /
 * DeepSeek）进页面时自动重查超过 10 分钟的结果。WorkBuddy 的 creditSummary
 * 端点挂风控盾，与签到状态卡同约定只手动触发；后端逐账号串行（2s 间隔），
 * 事件 `wb-quota-done` 让卡片逐张亮起。
 */

type Tone = "ok" | "warn" | "bad";

/** 卡片键前缀；只有 WorkBuddy 的前缀（wb）和服务商 id（workbuddy）不同。 */
type CardPrefix = "wb" | "cursor" | "grok-bot" | "codex" | "deepseek" | "qoder";
type AuthProvider = "codex" | "cursor" | "grok-bot" | "qoder";

const PREFIX_CARD: Record<CardPrefix, QuotaCardId> = {
  wb: "workbuddy",
  cursor: "cursor",
  "grok-bot": "grok-bot",
  codex: "codex",
  deepseek: "deepseek",
  qoder: "qoder",
};

const RAIL_COLLAPSED_KEY = "aq-rail-collapsed";

const AUTH_PRODUCT: Record<AuthProvider, string> = {
  codex: "Codex",
  cursor: "Cursor",
  "grok-bot": "Grok Bot",
  qoder: "Qoder",
};

/** 一张卡的外框绑定：定位用的卡片键、高亮，以及拖拽（手柄按下才 armed）。 */
type CardFrame = {
  cardKey: string;
  flash: boolean;
  armed: boolean;
  dragging: boolean;
  over: boolean;
  onHandleDown: () => void;
  onDragStart: (e: DragEvent<HTMLDivElement>) => void;
  onDragOver: (e: DragEvent<HTMLDivElement>) => void;
  onDragLeave: () => void;
  onDrop: (e: DragEvent<HTMLDivElement>) => void;
  onDragEnd: () => void;
};

const STATUS_KEYS: Record<WbAccount["status"], string> = {
  active: "statusActive",
  unverified: "statusUnverified",
  expired: "statusExpired",
  banned: "statusBanned",
};

/** 倒计时与「N 分钟前」共用的分钟时钟，整页只有一个定时器。 */
const NowContext = createContext(Date.now());

function useNow(intervalMs: number): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(timer);
  }, [intervalMs]);
  return now;
}

/** 上游容量是字符串数字（可能带小数，如 43.81）；整数给千分位，小数留 1 位。 */
function formatCredits(n: number): string {
  return Number.isInteger(n) ? n.toLocaleString() : n.toFixed(1);
}

/** 上游 totalSpend / limit 是美分（Pro 的 limit=2000 即 $20）。 */
function formatSpend(cents: number | null): string {
  if (cents == null) return "—";
  const dollars = cents / 100;
  return `$${dollars.toLocaleString(undefined, {
    minimumFractionDigits: Number.isInteger(dollars) ? 0 : 2,
    maximumFractionDigits: 2,
  })}`;
}

function formatDate(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, { month: "2-digit", day: "2-digit" });
}

function formatAge(atSecs: number, nowMs: number, t: TFunction<"quota">): string {
  const secs = Math.max(0, nowMs / 1000 - atSecs);
  if (secs < 60) return t("ageJustNow");
  if (secs < 3_600) return t("ageMinutes", { n: Math.floor(secs / 60) });
  if (secs < 86_400) return t("ageHours", { n: Math.floor(secs / 3_600) });
  return t("ageDays", { n: Math.floor(secs / 86_400) });
}

function clampPct(v: number): number {
  return Math.max(0, Math.min(100, Math.round(v)));
}

/** 剩余百分比 → 色调；进度条、状态点、工具条摘要用同一套阈值。 */
function toneOf(remainPct: number): Tone {
  return remainPct <= 10 ? "bad" : remainPct <= 30 ? "warn" : "ok";
}

/** 终点时刻 → `21d10h` / `5h` 紧凑倒计时；≤7 天黄、<24 小时红。 */
function compactCountdown(endMs: number | null, nowMs: number): { text: string; tone: Tone } | null {
  if (endMs == null) return null;
  const hours = (endMs - nowMs) / 3_600_000;
  if (hours <= 0) return { text: "0h", tone: "bad" };
  const days = Math.floor(hours / 24);
  const rest = Math.floor(hours % 24);
  const text = days > 0 ? `${days}d${rest}h` : `${Math.max(1, Math.floor(hours))}h`;
  const tone: Tone = hours < 24 ? "bad" : days <= 7 ? "warn" : "ok";
  return { text, tone };
}

/** WorkBuddy 积分包：上游只给 PackageCode，按实测编码段映射中文名，基础包排前。 */
function packageKind(code: string | null): "base" | "reward" | null {
  if (!code) return null;
  if (code.includes("code_008")) return "base";
  if (code.includes("code_007")) return "reward";
  return null;
}

function sortPackages(list: WbCreditPackage[]): WbCreditPackage[] {
  const rank = (p: WbCreditPackage) => {
    const k = packageKind(p.code);
    return k === "base" ? 0 : k === "reward" ? 1 : 2;
  };
  return [...list].sort((a, b) => rank(a) - rank(b));
}

function packagePct(p: WbCreditPackage): number {
  return p.total > 0 ? clampPct((p.remain / p.total) * 100) : p.remain > 0 ? 100 : 0;
}

function wbRemainPct(q: WbQuotaStatus): number | null {
  if (q.packages.length > 0) return Math.min(...q.packages.map(packagePct));
  return q.creditsLeft != null ? (q.creditsLeft > 0 ? 100 : 0) : null;
}

/**
 * 主读数与 Cursor 设置页「Included in Pro / Team」的 Total 一致：totalPercentUsed，个人与团队同一口径。
 * （编辑器底部用量条与「used N%」提示用的是套餐花费 / 限额，两者在 Cursor 里本来就不同。）
 * 旧响应没有该字段时退回较满的那个模型池，再退回套餐花费口径。
 */
/** Cursor 设置页的取整：用过一点（0~1%）就显示 1%，不会因四舍五入显示成没用。 */
function cursorUsedPct(v: number): number {
  return v > 0 && v < 1 ? 1 : Math.min(Math.max(v, 0), 100);
}

function cursorRemainPct(q: CursorQuotaDto): number | null {
  if (q.totalPercentUsed != null) return clampPct(100 - cursorUsedPct(q.totalPercentUsed));
  const pools = [q.autoPercentUsed, q.apiPercentUsed].filter((v): v is number => v != null);
  if (pools.length > 0) return clampPct(100 - cursorUsedPct(Math.max(...pools)));
  const included = q.includedSpend ?? q.totalSpend ?? null;
  return included != null && q.limit ? clampPct(100 - (included / q.limit) * 100) : null;
}

function grokRemainPct(q: GrokQuotaDto): number | null {
  return q.usagePercent != null ? clampPct(100 - q.usagePercent) : null;
}

/** 可用 Credits = 本期套餐 + 加油包（只算有总量的）；免费 / 用完的账号本期总量是 0，额度全在加油包里。 */
function qoderPool(q: QoderQuotaDto): { remaining: number; total: number; unit: string } | null {
  const parts = [q.user, q.addOn].filter((x): x is QoderQuota => x != null && x.total > 0);
  if (parts.length === 0) return null;
  return {
    remaining: parts.reduce((s, x) => s + Math.min(x.remaining, x.total), 0),
    total: parts.reduce((s, x) => s + x.total, 0),
    unit: parts[0].unit,
  };
}

/** 企业版上游不给数字。 */
function qoderRemainPct(q: QoderQuotaDto): number | null {
  const pool = qoderPool(q);
  return pool ? clampPct((pool.remaining / pool.total) * 100) : null;
}

function codexRemainPct(q: CodexQuotaDto): number | null {
  const used = [q.primary, q.secondary].filter((w): w is CodexWindow => w != null).map((w) => w.usedPercent);
  return used.length > 0 ? clampPct(100 - Math.max(...used)) : null;
}

/** 单币种余额占峰值的百分比；没有峰值记录时按「有余额即满」。 */
function balancePct(total: number, peak: number | undefined): number {
  if (total <= 0) return 0;
  return peak && peak > 0 ? clampPct((total / peak) * 100) : 100;
}

/** DeepSeek 只有余额没有总额：以见过的最高余额为满（见 quotaCache 的峰值记录），多币种取最低。 */
function deepseekRemainPct(key: string, q: DeepSeekQuotaDto, peaks: Record<string, number>): number | null {
  if (q.isAvailable === false) return 0;
  if (!q.balances?.length) return null;
  return Math.min(...q.balances.map((b) => balancePct(b.total, peaks[`${key}:${b.currency}`])));
}

function remainPctFor(key: string, r: QuotaResultBase, peaks: Record<string, number>): number | null {
  const prefix = key.split(":")[0] as CardPrefix;
  switch (prefix) {
    case "wb":
      return wbRemainPct(r as WbQuotaStatus);
    case "cursor":
      return cursorRemainPct(r as CursorQuotaDto);
    case "grok-bot":
      return grokRemainPct(r as GrokQuotaDto);
    case "codex":
      return codexRemainPct(r as CodexQuotaDto);
    case "deepseek":
      return deepseekRemainPct(key, r as DeepSeekQuotaDto, peaks);
    case "qoder":
      return qoderRemainPct(r as QoderQuotaDto);
  }
}

function cardStatus(key: string, r: QuotaResultBase | undefined, peaks: Record<string, number>): CardStatus {
  if (!r) return "idle";
  if (!r.ok) return r.needsReauth ? "reauth" : "error";
  const pct = remainPctFor(key, r, peaks);
  return pct == null ? "ok" : toneOf(pct);
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** reqwest 原文 / 旧缓存里的长 URL 错误 → 短提示；已是友好文案则原样返回。 */
function friendlyQuotaMessage(msg: string | null | undefined, networkLabel: string): string | null {
  if (!msg) return null;
  const lower = msg.toLowerCase();
  if (
    lower.includes("error sending request")
    || lower.includes("connect error")
    || lower.includes("connection reset")
    || lower.includes("connection refused")
    || lower.includes("connection timed out")
    || lower.includes("timed out")
    || lower.includes("timeout")
    || lower.includes("dns error")
    || lower.includes("failed to lookup address")
    || lower.includes("network unreachable")
    || lower.includes("no route to host")
    || lower.includes("tls handshake")
    || /\bssl\b/.test(lower)
    || lower.includes("certificate")
  ) {
    return networkLabel;
  }
  return msg;
}

/** invoke 本身 reject（不是后端返回的 ok:false）时，补一份失败结果让卡片显示错误而不是停在旧数据。 */
function failedDto(e: unknown, extra: Record<string, unknown> = {}): QuotaResultBase {
  return { ok: false, message: errorText(e), at: Math.floor(Date.now() / 1000), ...extra };
}

function failedWb(account: WbAccount, e: unknown): WbQuotaStatus {
  return {
    accountId: account.id,
    label: account.label,
    configured: true,
    ok: false,
    message: errorText(e),
    planName: null,
    paidUser: null,
    packages: [],
    creditsLeft: null,
    creditsTotal: null,
    at: Math.floor(Date.now() / 1000),
  };
}

/** 与后端整轮签到 / 额度查询的账号间隔一致（风控端点族）。 */
const WB_SPACING_MS = 2_000;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

function isSameLocalDay(aMs: number, bMs: number): boolean {
  return new Date(aMs).toDateString() === new Date(bMs).toDateString();
}

/** WorkBuddy 后端的时间戳（账号表、签到结果、额度结果）都是毫秒；看板其余卡片是秒。 */
function wbSecs(at: number): number {
  return at > 1e12 ? Math.floor(at / 1000) : at;
}

function wbResult(q: WbQuotaStatus): WbQuotaStatus {
  return { ...q, at: wbSecs(q.at) };
}

/** 升级前缓存里的 WorkBuddy 结果是毫秒，读进来时一并换算。 */
function loadNormalizedCache(): Record<string, QuotaResultBase> {
  const cache = loadQuotaCache();
  for (const [key, r] of Object.entries(cache)) {
    if (key.startsWith("wb:") && typeof r.at === "number") cache[key] = { ...r, at: wbSecs(r.at) };
  }
  return cache;
}

/** 今天是否已签到：只看账号表的 lastCheckin*（毫秒），不调上游 checkinStatus 端点。 */
function checkedInToday(account: WbAccount, nowMs: number): boolean {
  return (
    account.lastCheckinStatus === "ok" &&
    account.lastCheckinAt != null &&
    isSameLocalDay(account.lastCheckinAt, nowMs)
  );
}

function isAccountKey(key: string): boolean {
  return !key.endsWith(":empty") && !key.endsWith(":new");
}

function AccountQuotaPanel() {
  const { t } = useTranslation("quota");
  const confirm = useConfirm();
  const now = useNow(60_000);
  const [accounts, setAccounts] = useState<WbAccount[]>([]);
  const [authAccounts, setAuthAccounts] = useState<QuotaAccount[]>([]);
  const [codexAccounts, setCodexAccounts] = useState<CodexAccount[]>([]);
  const [loaded, setLoaded] = useState(false);
  /** 卡片键（`cursor:3` / `wb:5` …）→ 最近一次查询结果；整表持久化到 aq-cache。 */
  const [results, setResults] = useState<Record<string, QuotaResultBase>>(loadNormalizedCache);
  const [loadingKeys, setLoadingKeys] = useState<string[]>([]);
  const inflight = useRef(new Set<string>());
  /** 同一时刻只跑一个浏览器授权；新开的会先取消旧的。 */
  const [login, setLogin] = useState<AuthProvider | null>(null);
  const [loginError, setLoginError] = useState<Partial<Record<AuthProvider, string>>>({});
  const [dsFormOpen, setDsFormOpen] = useState(false);
  const [dsReplaceId, setDsReplaceId] = useState<number | null>(null);
  const [wbScanOpen, setWbScanOpen] = useState(false);
  const [wbManualOpen, setWbManualOpen] = useState(false);
  const [enabled, setEnabled] = useState<QuotaCardId[]>(loadEnabledCards);
  const [hidden, setHidden] = useState<string[]>(loadHiddenCards);
  const [flashKey, setFlashKey] = useState<string | null>(null);
  const [railCollapsed, setRailCollapsed] = useState(() => localStorage.getItem(RAIL_COLLAPSED_KEY) === "1");
  const toggleRail = useCallback(() => {
    setRailCollapsed((prev) => {
      try {
        localStorage.setItem(RAIL_COLLAPSED_KEY, prev ? "0" : "1");
      } catch {
        /* private mode / quota — the rail just reopens expanded */
      }
      return !prev;
    });
  }, []);
  const cursorAccounts = authAccounts.filter((a) => a.provider === "cursor");
  const grokAccounts = authAccounts.filter((a) => a.provider === "grok-bot");
  const dsAccounts = authAccounts.filter((a) => a.provider === "deepseek");
  const qoderAccounts = authAccounts.filter((a) => a.provider === "qoder");

  useEffect(() => saveQuotaCache(results), [results]);

  const [dsPeaks, setDsPeaks] = useState<Record<string, number>>(loadDsPeaks);
  useEffect(() => saveDsPeaks(dsPeaks), [dsPeaks]);

  // 余额上涨（充值）即上调峰值；缓存里恢复的结果也参与，首次升级后不必等重查。
  useEffect(() => {
    setDsPeaks((prev) => {
      let next = prev;
      for (const [key, r] of Object.entries(results)) {
        if (!key.startsWith("deepseek:") || !r.ok) continue;
        for (const b of (r as DeepSeekQuotaDto).balances ?? []) {
          const peakKey = `${key}:${b.currency}`;
          if (b.total > (next[peakKey] ?? 0)) next = { ...next, [peakKey]: b.total };
        }
      }
      return next;
    });
  }, [results]);

  const setCardEnabled = useCallback((id: QuotaCardId, on: boolean) => {
    setEnabled((prev) => {
      if (prev.includes(id) === on) return prev;
      const next = on ? [...prev, id] : prev.filter((c) => c !== id);
      saveEnabledCards(next);
      return next;
    });
  }, []);

  const setCardHidden = useCallback((key: string, hide: boolean) => {
    setHidden((prev) => {
      if (prev.includes(key) === hide) return prev;
      const next = hide ? [...prev, key] : prev.filter((k) => k !== key);
      saveHiddenCards(next);
      return next;
    });
  }, []);

  /** 把卡片拉回看板（取消隐藏、打开服务商），滚动过去并短暂高亮。 */
  const focusCard = useCallback(
    (key: string) => {
      setCardHidden(key, false);
      setCardEnabled(PREFIX_CARD[key.split(":")[0] as CardPrefix], true);
      setFlashKey(null);
      window.setTimeout(() => {
        setFlashKey(key);
        document
          .querySelector(`[data-card-key="${CSS.escape(key)}"]`)
          ?.scrollIntoView({ behavior: "smooth", block: "nearest" });
      }, 60);
    },
    [setCardHidden, setCardEnabled],
  );

  useEffect(() => {
    if (!flashKey) return;
    const timer = window.setTimeout(() => setFlashKey(null), 1600);
    return () => window.clearTimeout(timer);
  }, [flashKey]);

  const [order, setOrder] = useState<string[]>(loadCardOrder);
  /** 只有按住拖拽手柄时卡片才 draggable，卡片内文字选择和按钮不受影响。 */
  const [armedKey, setArmedKey] = useState<string | null>(null);
  const [dragKey, setDragKey] = useState<string | null>(null);
  const [overKey, setOverKey] = useState<string | null>(null);

  useEffect(() => {
    if (!armedKey) return;
    const disarm = () => setArmedKey(null);
    window.addEventListener("pointerup", disarm);
    return () => window.removeEventListener("pointerup", disarm);
  }, [armedKey]);

  const endDrag = () => {
    setDragKey(null);
    setOverKey(null);
    setArmedKey(null);
  };

  const dropOn = (targetKey: string, visibleKeys: string[]) => {
    if (dragKey && dragKey !== targetKey) {
      const next = [...visibleKeys];
      const from = next.indexOf(dragKey);
      const to = next.indexOf(targetKey);
      if (from !== -1 && to !== -1) {
        next.splice(to, 0, ...next.splice(from, 1));
        // 隐藏中的卡片（已移出看板）保留原记录，重新加回时回到原位附近。
        const merged = [...next, ...order.filter((k) => !next.includes(k))];
        setOrder(merged);
        saveCardOrder(merged);
      }
    }
    endDrag();
  };

  const frameFor = (key: string, visibleKeys: string[]): CardFrame => ({
    cardKey: key,
    flash: flashKey === key,
    armed: armedKey === key,
    dragging: dragKey === key,
    over: overKey === key && dragKey !== null && dragKey !== key,
    onHandleDown: () => setArmedKey(key),
    onDragStart: (e) => {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", key);
      setDragKey(key);
    },
    onDragOver: (e) => {
      if (!dragKey) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";
      if (overKey !== key) setOverKey(key);
    },
    onDragLeave: () => {
      if (overKey === key) setOverKey(null);
    },
    onDrop: (e) => {
      e.preventDefault();
      dropOn(key, visibleKeys);
    },
    onDragEnd: endDrag,
  });

  const reloadWb = useCallback(async () => {
    setAccounts(await wbListAccounts().catch(() => []));
  }, []);

  useEffect(() => {
    let cancelled = false;
    void Promise.all([
      wbListAccounts().catch(() => [] as WbAccount[]),
      quotaAccountsList().catch(() => [] as QuotaAccount[]),
      codexListAccounts().catch(() => [] as CodexAccount[]),
    ]).then(([wb, auth, codex]) => {
      if (cancelled) return;
      setAccounts(wb);
      setAuthAccounts(auth);
      setCodexAccounts(codex);
      // 账号可能在别处被删（WorkBuddy 页、另一窗口），缓存里只留还存在的。
      const live = new Set([
        ...wb.map((a) => `wb:${a.id}`),
        ...auth.map((a) => `${a.provider}:${a.id}`),
        ...codex.map((a) => `codex:${a.id}`),
      ]);
      setResults((prev) => Object.fromEntries(Object.entries(prev).filter(([k]) => live.has(k))));
      setLoaded(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const markLoading = (keys: string[], on: boolean) =>
    setLoadingKeys((prev) => (on ? [...prev, ...keys.filter((k) => !prev.includes(k))] : prev.filter((k) => !keys.includes(k))));

  useTauriEvent<WbQuotaStatus>(WB_QUOTA_EVENT, (e) => {
    const key = `wb:${e.payload.accountId}`;
    setResults((prev) => ({ ...prev, [key]: wbResult(e.payload) }));
    setLoadingKeys((prev) => prev.filter((k) => k !== key));
  }, []);

  /** 进行中的签到账号 id；结果只留本次会话，持久状态以账号表的 lastCheckin* 为准。 */
  const [checkinBusy, setCheckinBusy] = useState<number[]>([]);
  const [checkinResults, setCheckinResults] = useState<Record<number, WbCheckinResult>>({});
  /** 本地设置（不碰上游）：页脚提示与工具条的自动签到弹层。 */
  const [wbSettings, setWbSettings] = useState<WbSettings | null>(null);

  useEffect(() => {
    wbGetSettings().then(setWbSettings, () => undefined);
  }, []);

  useTauriEvent<WbCheckinResult>(WB_CHECKIN_EVENT, (e) => {
    const r = e.payload;
    setCheckinResults((prev) => ({ ...prev, [r.accountId]: r }));
    setCheckinBusy((prev) => prev.filter((id) => id !== r.accountId));
  }, []);

  /**
   * 签到一批 WorkBuddy 账号（前端串行、间隔 2s，与后端整轮一致）。
   * 成功的账号随后再查一次积分，让签到所得立刻可见。
   */
  const checkinWb = useCallback(
    async (ids: number[], wholePool: boolean) => {
      if (ids.length === 0) return;
      setCheckinBusy((prev) => [...prev, ...ids.filter((id) => !prev.includes(id))]);
      const succeeded: number[] = [];
      const record = (list: WbCheckinResult[]) => {
        setCheckinResults((prev) => {
          const next = { ...prev };
          for (const r of list) next[r.accountId] = r;
          return next;
        });
        for (const r of list) if (r.ok) succeeded.push(r.accountId);
      };
      try {
        if (wholePool) {
          record(await wbCheckinNow(null));
        } else {
          for (const [i, id] of ids.entries()) {
            if (i > 0) await sleep(WB_SPACING_MS);
            record(await wbCheckinNow(id));
          }
        }
      } catch (e) {
        const at = Date.now();
        record(ids.map((id) => ({ accountId: id, label: "", ok: false, message: errorText(e), at, disabled: false })));
      } finally {
        setCheckinBusy((prev) => prev.filter((id) => !ids.includes(id)));
        await reloadWb();
      }
      for (const id of succeeded) {
        await sleep(WB_SPACING_MS);
        await refreshWbRef.current([id]);
      }
    },
    [reloadWb],
  );
  const fetchKey = useCallback(async (key: string) => {
    if (inflight.current.has(key)) return;
    inflight.current.add(key);
    const [prefix, rawId] = key.split(":");
    const id = Number(rawId);
    setLoadingKeys((prev) => (prev.includes(key) ? prev : [...prev, key]));
    try {
      let dto: QuotaResultBase;
      if (prefix === "cursor") dto = await cursorQuotaFetch(id).catch((e) => failedDto(e, { id }));
      else if (prefix === "grok-bot") dto = await grokBotQuotaFetch(id).catch((e) => failedDto(e, { id }));
      else if (prefix === "codex") dto = await codexQuotaFetch(id).catch((e) => failedDto(e, { id }));
      else if (prefix === "qoder") dto = await qoderQuotaFetch(id).catch((e) => failedDto(e, { id }));
      else dto = await deepseekBalanceFetch(id).catch((e) => failedDto(e, { id, balances: [] }));
      setResults((prev) => ({ ...prev, [key]: dto }));
    } finally {
      inflight.current.delete(key);
      setLoadingKeys((prev) => prev.filter((k) => k !== key));
    }
  }, []);

  /** WorkBuddy 只走这里；`ids` 为 null 时整池串行，卡片随 wb-quota-done 逐张解除加载。 */
  const refreshWb = useCallback(
    async (ids: number[] | null) => {
      const targets = ids ?? accounts.map((a) => a.id);
      if (targets.length === 0) return;
      const keys = targets.map((id) => `wb:${id}`);
      markLoading(keys, true);
      try {
        const list = await wbQuotaStatus(ids?.length === 1 ? ids[0] : null);
        setResults((prev) => {
          const next = { ...prev };
          for (const item of list) next[`wb:${item.accountId}`] = wbResult(item);
          return next;
        });
      } catch (e) {
        setResults((prev) => {
          const next = { ...prev };
          for (const a of accounts) if (targets.includes(a.id)) next[`wb:${a.id}`] = failedWb(a, e);
          return next;
        });
      } finally {
        markLoading(keys, false);
      }
    },
    [accounts],
  );
  const refreshWbRef = useRef(refreshWb);
  refreshWbRef.current = refreshWb;

  const cancelLogin = useCallback((provider: AuthProvider) => {
    void (provider === "codex" ? codexOauthCancel() : cursorOauthCancel()).catch(() => undefined);
  }, []);

  /**
   * 链接授权：后端打开浏览器并等结果，成功后立刻查一次该账号额度。
   * 同一账号重新授权时后端按身份去重，更新的是原卡片。
   */
  const startLogin = useCallback(
    async (provider: AuthProvider) => {
      if (login && login !== provider) cancelLogin(login);
      setLogin(provider);
      setLoginError((prev) => ({ ...prev, [provider]: undefined }));
      try {
        if (provider === "codex") {
          const account = await codexOauthStart();
          setCodexAccounts(await codexListAccounts());
          void fetchKey(`codex:${account.id}`);
        } else {
          const account = provider === "qoder" ? await qoderOauthStart() : await cursorOauthStart(provider);
          setAuthAccounts(await quotaAccountsList());
          void fetchKey(`${provider}:${account.id}`);
        }
      } catch (e) {
        const message = errorText(e);
        if (!message.includes("已取消")) setLoginError((prev) => ({ ...prev, [provider]: message }));
      } finally {
        setLogin((cur) => (cur === provider ? null : cur));
      }
    },
    [login, cancelLogin, fetchKey],
  );

  const dropResult = (key: string) => {
    setResults((prev) => {
      const next = { ...prev };
      delete next[key];
      return next;
    });
    if (key.startsWith("deepseek:")) {
      setDsPeaks((prev) => Object.fromEntries(Object.entries(prev).filter(([k]) => !k.startsWith(`${key}:`))));
    }
  };

  const addDeepSeekKey = useCallback(
    async (apiKey: string) => {
      const account = await deepseekAddKey(apiKey);
      setAuthAccounts(await quotaAccountsList());
      setDsFormOpen(false);
      void fetchKey(`deepseek:${account.id}`);
    },
    [fetchKey],
  );

  /** 更换 Key：新 Key 验证通过才删旧账号，新卡片接替旧卡片在看板里的位置。 */
  const replaceDeepSeekKey = useCallback(
    async (old: QuotaAccount, apiKey: string) => {
      const account = await deepseekAddKey(apiKey);
      const oldKey = `deepseek:${old.id}`;
      const newKey = `deepseek:${account.id}`;
      if (account.id !== old.id) {
        await quotaAccountDelete(old.id).catch(() => undefined);
        dropResult(oldKey);
        setOrder((prev) => {
          const next = prev.map((k) => (k === oldKey ? newKey : k));
          saveCardOrder(next);
          return next;
        });
      }
      setAuthAccounts(await quotaAccountsList());
      setDsReplaceId(null);
      void fetchKey(newKey);
    },
    [fetchKey],
  );

  const removeAuthAccount = useCallback(
    async (account: QuotaAccount) => {
      const ok = await confirm({
        title: t("removeAccountTitle"),
        message: t("removeAccountMessage", { name: account.label ?? `#${account.id}` }),
        icon: "danger",
      });
      if (!ok) return;
      await quotaAccountDelete(account.id).catch(() => undefined);
      setAuthAccounts(await quotaAccountsList().catch(() => []));
      dropResult(`${account.provider}:${account.id}`);
    },
    [confirm, t],
  );

  const removeCodexAccount = useCallback(
    async (account: CodexAccount) => {
      const ok = await confirm({
        title: t("removeAccountTitle"),
        message: t("removeAccountMessage", { name: account.email ?? `#${account.id}` }),
        icon: "danger",
      });
      if (!ok) return;
      await codexDeleteAccount(account.id).catch(() => undefined);
      setCodexAccounts(await codexListAccounts().catch(() => []));
      dropResult(`codex:${account.id}`);
    },
    [confirm, t],
  );

  const removeWbAccount = useCallback(
    async (account: WbAccount) => {
      const ok = await confirm({
        title: t("removeAccountTitle"),
        message: t("removeAccountMessage", { name: account.label || `#${account.id}` }),
        icon: "danger",
      });
      if (!ok) return;
      await wbDeleteAccount(account.id).catch(() => undefined);
      await reloadWb();
      dropResult(`wb:${account.id}`);
      setCardHidden(`wb:${account.id}`, false);
    },
    [confirm, t, reloadWb, setCardHidden],
  );

  const removeByKey = (key: string) => {
    const [prefix, rawId] = key.split(":") as [CardPrefix, string];
    const id = Number(rawId);
    if (prefix === "wb") {
      const a = accounts.find((x) => x.id === id);
      if (a) void removeWbAccount(a);
    } else if (prefix === "codex") {
      const a = codexAccounts.find((x) => x.id === id);
      if (a) void removeCodexAccount(a);
    } else {
      const a = authAccounts.find((x) => x.provider === prefix && x.id === id);
      if (a) void removeAuthAccount(a);
    }
  };

  const accountKeys: Record<CardPrefix, string[]> = {
    wb: accounts.map((a) => `wb:${a.id}`),
    cursor: cursorAccounts.map((a) => `cursor:${a.id}`),
    "grok-bot": grokAccounts.map((a) => `grok-bot:${a.id}`),
    codex: codexAccounts.map((a) => `codex:${a.id}`),
    deepseek: dsAccounts.map((a) => `deepseek:${a.id}`),
    qoder: qoderAccounts.map((a) => `qoder:${a.id}`),
  };

  /** 服务商关着 → 不出卡；没账号 → 授权空卡；账号全被隐藏 → 什么都不出。 */
  const keysOf = (prefix: CardPrefix) => {
    if (!enabled.includes(PREFIX_CARD[prefix])) return [];
    const keys = accountKeys[prefix];
    return keys.length === 0 ? [`${prefix}:empty`] : keys.filter((k) => !hidden.includes(k));
  };
  const naturalKeys = [
    ...keysOf("wb"),
    ...keysOf("cursor"),
    ...keysOf("grok-bot"),
    ...keysOf("codex"),
    ...keysOf("qoder"),
    ...keysOf("deepseek"),
    ...(enabled.includes("deepseek") && dsFormOpen && dsAccounts.length > 0 ? ["deepseek:new"] : []),
  ];
  const visibleKeys = sortByOrder(
    naturalKeys.map((key) => ({ key })),
    order,
  ).map((x) => x.key);
  const boardAccountKeys = visibleKeys.filter(isAccountKey);
  const visibleAuthKeys = boardAccountKeys.filter((k) => !k.startsWith("wb:"));
  const visibleWbIds = boardAccountKeys.filter((k) => k.startsWith("wb:")).map((k) => Number(k.slice(3)));

  // 进页面只自动跑一次：授权类、在看板上、缓存超过 10 分钟的卡片。
  const autoRefreshed = useRef(false);
  useEffect(() => {
    if (!loaded || autoRefreshed.current) return;
    autoRefreshed.current = true;
    const at = Date.now();
    for (const key of visibleAuthKeys) if (isStale(results[key], at)) void fetchKey(key);
  });

  const refreshAll = () => {
    for (const key of visibleAuthKeys) void fetchKey(key);
  };

  const refreshWbVisible = () => {
    // 全部可见时让后端整池串行一次；部分被隐藏时只查看得见的。
    void refreshWb(visibleWbIds.length === accounts.length ? null : visibleWbIds);
  };

  /** 右侧「添加账号」：+ 永远是加一个新账号，顺带把服务商放回看板。 */
  const addAccount = useCallback(
    (card: QuotaCardId) => {
      setCardEnabled(card, true);
      if (card === "workbuddy") {
        setWbScanOpen(true);
        return;
      }
      if (card === "deepseek") {
        setDsFormOpen(true);
        focusCard(dsAccounts.length > 0 ? "deepseek:new" : "deepseek:empty");
        return;
      }
      void startLogin(card);
      const hasAccounts = card === "codex" ? codexAccounts.length > 0 : authAccounts.some((a) => a.provider === card);
      if (!hasAccounts) focusCard(`${card}:empty`);
    },
    [setCardEnabled, focusCard, startLogin, dsAccounts.length, codexAccounts.length, authAccounts],
  );

  const toggleProvider = useCallback(
    (card: QuotaCardId) => setCardEnabled(card, !enabled.includes(card)),
    [enabled, setCardEnabled],
  );

  const uncheckedWbIds = visibleWbIds.filter((id) => {
    const a = accounts.find((x) => x.id === id);
    return a != null && a.enabled && !checkedInToday(a, now);
  });
  const claimableQoderKeys = boardAccountKeys.filter(
    (k) => k.startsWith("qoder:") && liveQoderCampaign(results[k] as QoderQuotaDto | undefined, now)?.claimable === true,
  );
  const checkinRunning = checkinBusy.length > 0;
  const autoCheckinText = wbSettings
    ? wbSettings.checkinEnabled
      ? t("checkinAuto", { time: wbSettings.checkinTime })
      : t("checkinAutoOff")
    : null;
  const checkinAll = () => {
    const enabledIds = accounts.filter((a) => a.enabled).map((a) => a.id);
    // 看板上可见的未签到账号恰好是整池时交给后端整轮；否则前端逐个。
    const wholePool =
      uncheckedWbIds.length === enabledIds.length && enabledIds.every((id) => uncheckedWbIds.includes(id));
    void checkinWb(uncheckedWbIds, wholePool);
  };

  const railAccounts: RailAccount[] = [
    ...accounts.map((a) => ({
      key: `wb:${a.id}`,
      card: "workbuddy" as const,
      name: a.label,
      checkedIn: checkedInToday(a, now),
    })),
    ...cursorAccounts.map((a) => ({
      key: `cursor:${a.id}`,
      card: "cursor" as const,
      name: (results[`cursor:${a.id}`] as CursorQuotaDto | undefined)?.email ?? a.label ?? `#${a.id}`,
    })),
    ...grokAccounts.map((a) => ({
      key: `grok-bot:${a.id}`,
      card: "grok-bot" as const,
      name: (results[`grok-bot:${a.id}`] as GrokQuotaDto | undefined)?.email ?? a.label ?? `#${a.id}`,
    })),
    ...codexAccounts.map((a) => ({ key: `codex:${a.id}`, card: "codex" as const, name: a.email ?? `#${a.id}` })),
    ...qoderAccounts.map((a) => ({
      key: `qoder:${a.id}`,
      card: "qoder" as const,
      name: (results[`qoder:${a.id}`] as QoderQuotaDto | undefined)?.label ?? a.label ?? `#${a.id}`,
    })),
    ...dsAccounts.map((a) => ({ key: `deepseek:${a.id}`, card: "deepseek" as const, name: a.label ?? `#${a.id}` })),
  ].map((a) => ({
    ...a,
    status: cardStatus(a.key, results[a.key], dsPeaks),
    hidden: hidden.includes(a.key) || !enabled.includes(a.card),
  }));

  const statusOf = (key: string) => cardStatus(key, results[key], dsPeaks);
  const summary = {
    total: boardAccountKeys.length,
    low: boardAccountKeys.filter((k) => ["warn", "bad"].includes(statusOf(k))),
    reauth: boardAccountKeys.filter((k) => statusOf(k) === "reauth"),
    failed: boardAccountKeys.filter((k) => statusOf(k) === "error"),
  };
  const lastAt = boardAccountKeys.reduce<number | null>((max, k) => {
    const at = results[k]?.at;
    return at != null && (max == null || at > max) ? at : max;
  }, null);
  const authLoading = loadingKeys.some((k) => !k.startsWith("wb:"));
  const wbLoading = loadingKeys.some((k) => k.startsWith("wb:"));

  const authPrompt = (provider: AuthProvider) => (
    <AuthPrompt
      hint={t(`authHint.${provider}`)}
      pending={login === provider}
      error={loginError[provider] ?? null}
      onLogin={() => void startLogin(provider)}
      onCancel={() => cancelLogin(provider)}
    />
  );
  const addAnother = (provider: AuthProvider) => (login === provider ? undefined : () => void startLogin(provider));
  const reauthFor = (provider: AuthProvider) => ({
    onReauth: () => void startLogin(provider),
    reauthPending: login === provider,
  });

  const renderCard = (key: string) => {
    const frame = frameFor(key, visibleKeys);
    const [prefix, rawId] = key.split(":") as [CardPrefix, string];
    const id = Number(rawId);
    const quota = results[key];
    const loading = loadingKeys.includes(key);
    const hideProvider = () => setCardEnabled(PREFIX_CARD[prefix], false);

    if (prefix === "deepseek") {
      const account = dsAccounts.find((a) => a.id === id);
      if (!account) {
        return (
          <QuotaCardShell
            key={key}
            brand="deepseek"
            product="DeepSeek"
            onHide={rawId === "new" ? () => setDsFormOpen(false) : hideProvider}
            frame={frame}
          >
            <DeepSeekKeyForm
              onSubmit={addDeepSeekKey}
              onCancel={rawId === "new" ? () => setDsFormOpen(false) : undefined}
            />
          </QuotaCardShell>
        );
      }
      return (
        <DeepSeekCard
          key={key}
          account={account}
          quota={quota as DeepSeekQuotaDto | undefined}
          loading={loading}
          peaks={dsPeaks}
          replacing={dsReplaceId === account.id}
          onRefresh={() => void fetchKey(key)}
          onDelete={() => void removeAuthAccount(account)}
          onReplace={(apiKey) => replaceDeepSeekKey(account, apiKey)}
          onStartReplace={() => setDsReplaceId(account.id)}
          onCancelReplace={() => setDsReplaceId(null)}
          onAddAnother={dsFormOpen ? undefined : () => setDsFormOpen(true)}
          frame={frame}
        />
      );
    }

    if (prefix === "cursor" || prefix === "grok-bot") {
      const account = authAccounts.find((a) => a.provider === prefix && a.id === id);
      if (!account) {
        return prefix === "cursor" ? (
          <QuotaCardShell key={key} brand="cursor" product="Cursor" onHide={hideProvider} frame={frame}>
            {authPrompt("cursor")}
          </QuotaCardShell>
        ) : (
          <QuotaCardShell key={key} brand="grok" product="Grok Bot" onHide={hideProvider} frame={frame}>
            {authPrompt("grok-bot")}
          </QuotaCardShell>
        );
      }
      const common = {
        account,
        loading,
        onRefresh: () => void fetchKey(key),
        onDelete: () => void removeAuthAccount(account),
        onAddAnother: addAnother(prefix),
        frame,
        ...reauthFor(prefix),
      };
      return prefix === "cursor" ? (
        <CursorCard key={key} quota={quota as CursorQuotaDto | undefined} {...common} />
      ) : (
        <GrokCard key={key} quota={quota as GrokQuotaDto | undefined} {...common} />
      );
    }

    if (prefix === "qoder") {
      const account = qoderAccounts.find((a) => a.id === id);
      if (!account) {
        return (
          <QuotaCardShell key={key} brand="qoder" product="Qoder" onHide={hideProvider} frame={frame}>
            {authPrompt("qoder")}
          </QuotaCardShell>
        );
      }
      return (
        <QoderCard
          key={key}
          account={account}
          quota={quota as QoderQuotaDto | undefined}
          loading={loading}
          onRefresh={() => void fetchKey(key)}
          onDelete={() => void removeAuthAccount(account)}
          onAddAnother={addAnother("qoder")}
          frame={frame}
          {...reauthFor("qoder")}
        />
      );
    }

    if (prefix === "codex") {
      const account = codexAccounts.find((a) => a.id === id);
      if (!account) {
        return (
          <QuotaCardShell key={key} brand="codex" product="Codex" onHide={hideProvider} frame={frame}>
            {authPrompt("codex")}
          </QuotaCardShell>
        );
      }
      return (
        <CodexCard
          key={key}
          account={account}
          quota={quota as CodexQuotaDto | undefined}
          loading={loading}
          onRefresh={() => void fetchKey(key)}
          onDelete={() => void removeCodexAccount(account)}
          onAddAnother={addAnother("codex")}
          frame={frame}
          {...reauthFor("codex")}
        />
      );
    }

    const account = accounts.find((a) => a.id === id);
    if (!account) {
      return (
        <QuotaCardShell key={key} brand="wb" product="WorkBuddy" onHide={hideProvider} frame={frame}>
          <AuthPrompt
            hint={t("authHint.workbuddy")}
            pending={false}
            error={null}
            loginText={t("wbScanLogin")}
            onLogin={() => setWbScanOpen(true)}
            onCancel={() => undefined}
          />
        </QuotaCardShell>
      );
    }
    return (
      <WorkbuddyCard
        key={key}
        account={account}
        quota={quota as WbQuotaStatus | undefined}
        loading={loading}
        onRefresh={() => void refreshWb([account.id])}
        onHide={() => setCardHidden(key, true)}
        onDelete={() => void removeWbAccount(account)}
        onAddAnother={() => setWbScanOpen(true)}
        checkinBusy={checkinBusy.includes(account.id)}
        checkinResult={checkinResults[account.id]}
        autoCheckin={autoCheckinText}
        onCheckin={() => void checkinWb([account.id], false)}
        frame={frame}
      />
    );
  };

  const hasAccountsFor = (provider: AuthProvider) =>
    provider === "codex" ? codexAccounts.length > 0 : authAccounts.some((a) => a.provider === provider);
  const pendingInToolbar = login != null && hasAccountsFor(login);
  const toolbarError = (Object.entries(loginError) as [AuthProvider, string | undefined][]).find(
    ([p, msg]) => msg && p !== login && hasAccountsFor(p),
  );

  return (
    <NowContext.Provider value={now}>
      <div className="page-panel is-active quota-page">
        <div className="aq-shell">
          <div className="page-scrollable aq-main">
            <div className="aq-toolbar">
              <p className="aq-subtitle">{t("subtitle")}</p>
              <div className="aq-toolbar-actions">
                <div className="aq-toolbar-info">
                {pendingInToolbar && login && (
                  <span className="aq-auth-status">
                    {t("authPending", { name: AUTH_PRODUCT[login] })}
                    <button type="button" className="aq-auth-cancel" onClick={() => cancelLogin(login)}>
                      {t("authCancel")}
                    </button>
                  </span>
                )}
                {!pendingInToolbar && toolbarError && (
                  <span className="aq-auth-status aq-tone-bad" title={toolbarError[1]}>
                    {AUTH_PRODUCT[toolbarError[0]]}：{toolbarError[1]}
                  </span>
                )}
                {summary.total > 0 && (
                  <span className="aq-summary">
                    <span>{t("summaryAccounts", { count: summary.total })}</span>
                    <SummarySegment keys={summary.low} tone="warn" text={t("summaryLow", { count: summary.low.length })} onFocus={focusCard} />
                    <SummarySegment
                      keys={summary.reauth}
                      tone="bad"
                      text={t("summaryReauth", { count: summary.reauth.length })}
                      onFocus={focusCard}
                    />
                    <SummarySegment
                      keys={summary.failed}
                      tone="bad"
                      text={t("summaryFailed", { count: summary.failed.length })}
                      onFocus={focusCard}
                    />
                    <SummarySegment
                      keys={uncheckedWbIds.map((id) => `wb:${id}`)}
                      tone="warn"
                      text={t("summaryUnchecked", { count: uncheckedWbIds.length })}
                      onFocus={focusCard}
                    />
                    <SummarySegment
                      keys={claimableQoderKeys}
                      tone="warn"
                      text={t("qoderClaimableCount", { count: claimableQoderKeys.length })}
                      onFocus={focusCard}
                    />
                  </span>
                )}
                <span className="aq-refreshed-at">
                  {lastAt != null
                    ? t("lastRefreshed", { time: new Date(lastAt * 1000).toLocaleTimeString() })
                    : t("neverRefreshed")}
                </span>
                </div>
                <div className="aq-toolbar-buttons">
                {visibleWbIds.length > 0 && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={refreshWbVisible}
                    disabled={wbLoading}
                    title={t("refreshWbHint")}
                  >
                    <Bot size={13} />
                    {wbLoading ? t("refreshing") : t("refreshWb")}
                  </button>
                )}
                {visibleWbIds.length > 0 && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={checkinAll}
                    disabled={checkinRunning || uncheckedWbIds.length === 0}
                    title={[t("checkinAllHint"), autoCheckinText].filter(Boolean).join("\n")}
                  >
                    {checkinRunning ? <RefreshCw size={13} className="spin" /> : <CalendarCheck size={13} />}
                    {uncheckedWbIds.length === 0 && !checkinRunning
                      ? t("checkinAllDone")
                      : t("checkinAll", { count: uncheckedWbIds.length })}
                  </button>
                )}
                {visibleWbIds.length > 0 && <AutoCheckinPopover settings={wbSettings} onSaved={setWbSettings} />}
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={refreshAll}
                  disabled={authLoading || visibleAuthKeys.length === 0}
                  title={t("refreshAllHint")}
                >
                  <RefreshCw size={13} className={authLoading ? "spin" : undefined} />
                  {authLoading ? t("refreshing") : t("refreshAll")}
                </button>
                </div>
              </div>
            </div>

            {visibleKeys.length === 0 ? (
              <div className="aq-board-empty">{t("boardEmpty")}</div>
            ) : (
              <div className="aq-grid">{visibleKeys.map(renderCard)}</div>
            )}
          </div>

          <QuotaProviderRail
            enabled={enabled}
            accounts={railAccounts}
            pending={login}
            collapsed={railCollapsed}
            onToggleCollapsed={toggleRail}
            onAdd={addAccount}
            onCancelPending={() => login && cancelLogin(login)}
            onToggleVisible={toggleProvider}
            onFocusAccount={focusCard}
            onDeleteAccount={removeByKey}
          />
        </div>

        {wbScanOpen && (
          <ScanAddFlow
            onClose={() => {
              setWbScanOpen(false);
              void reloadWb();
            }}
            onSaved={() => void reloadWb()}
            onFallBackToPaste={() => {
              setWbScanOpen(false);
              setWbManualOpen(true);
            }}
          />
        )}
        {wbManualOpen && (
          <ManualAddModal
            onClose={() => setWbManualOpen(false)}
            onSaved={() => {
              setWbManualOpen(false);
              void reloadWb();
            }}
          />
        )}
      </div>
    </NowContext.Provider>
  );
}

/** 工具条摘要里的一段；为 0 时不出现，点击定位到第一张匹配的卡。 */
/** 工具条上的 WorkBuddy 每日自动签到设置；与 WorkBuddy 页签到区是同一份设置。 */
function AutoCheckinPopover({
  settings,
  onSaved,
}: {
  settings: WbSettings | null;
  onSaved: (s: WbSettings) => void;
}) {
  const { t } = useTranslation("quota");
  const { t: tw } = useTranslation("workbuddy");
  const [open, setOpen] = useState(false);
  const [time, setTime] = useState(settings?.checkinTime ?? "09:30");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const configured = hasCheckinEndpoint(settings);
  const enabled = settings?.checkinEnabled ?? false;

  useEffect(() => {
    if (settings) setTime(settings.checkinTime);
  }, [settings]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const save = async (nextEnabled: boolean, nextTime: string) => {
    setSaving(true);
    setError(null);
    try {
      onSaved(await wbUpdateSettings({ checkinEnabled: nextEnabled, checkinTime: nextTime }));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSaving(false);
    }
  };
  const saveTimeIfChanged = () => {
    if (settings && time && time !== settings.checkinTime) void save(enabled, time);
  };

  return (
    <div className="aq-pop-anchor" ref={rootRef}>
      <button
        type="button"
        className="btn btn-secondary btn-small"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        disabled={!settings}
      >
        <CalendarClock size={13} />
        {settings && enabled ? t("autoCheckinButton", { time: settings.checkinTime }) : t("autoCheckinOff")}
      </button>
      {open && settings && (
        <div className="aq-pop" role="dialog" aria-label={tw("checkinDaily")}>
          <div className="aq-pop-row">
            <label className="aq-pop-check">
              <input
                type="checkbox"
                checked={enabled}
                disabled={saving || !configured}
                onChange={(e) => void save(e.target.checked, time)}
              />
              <span>{tw("checkinDaily")}</span>
            </label>
            <input
              className="input-field aq-pop-time"
              type="time"
              value={time}
              disabled={saving || !configured}
              onChange={(e) => setTime(e.target.value)}
              onBlur={saveTimeIfChanged}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveTimeIfChanged();
              }}
            />
            {saving && <RefreshCw size={12} className="spin" />}
          </div>
          <p className={`aq-pop-status${configured ? "" : " aq-tone-bad"}`}>
            {!configured
              ? tw("checkinNoEndpoint")
              : enabled
                ? tw("checkinLastAuto", { date: settings.lastAutoCheckinDate ?? "—" })
                : tw("checkinScheduleOff")}
          </p>
          <p className="aq-pop-hint">{tw("checkinHint")}</p>
          {error && (
            <div className="aq-card-error" role="alert">
              {friendlyQuotaMessage(error, tw("networkFailed")) ?? error}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function SummarySegment({
  keys,
  tone,
  text,
  onFocus,
}: {
  keys: string[];
  tone: Tone;
  text: string;
  onFocus: (key: string) => void;
}) {
  if (keys.length === 0) return null;
  return (
    <button type="button" className={`aq-summary-seg aq-tone-${tone}`} onClick={() => onFocus(keys[0])}>
      {text}
    </button>
  );
}

function QuotaCardShell({
  brand,
  product,
  tag,
  at,
  loading,
  onRefresh,
  onDelete,
  onHide,
  extraAction,
  footLead,
  footNote,
  frame,
  children,
}: {
  brand: "wb" | "cursor" | "grok" | "codex" | "deepseek" | "qoder";
  product: string;
  tag?: ReactNode;
  /** 结果时间（unix 秒）；有值时卡片底部显示「N 分钟前」。 */
  at?: number | null;
  loading?: boolean;
  onRefresh?: () => void;
  /** 授权类账号：删除本机保存的令牌 / Key。 */
  onDelete?: () => void;
  /** 只从看板拿掉，不动账号本身。 */
  onHide?: () => void;
  extraAction?: ReactNode;
  /** 底部行最左侧的状态件（如 WorkBuddy 签到状态），排在附注前。 */
  footLead?: ReactNode;
  /** 底部左侧的附注（如自动签到时间），右侧仍是结果时间。 */
  footNote?: string | null;
  frame?: CardFrame;
  children: ReactNode;
}) {
  const { t } = useTranslation("quota");
  const now = useContext(NowContext);
  const cls = [
    "card aq-card",
    `aq-brand-${brand}`,
    loading && "is-loading",
    frame?.dragging && "is-dragging",
    frame?.over && "is-drop-target",
    frame?.flash && "is-flash",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <div
      className={cls}
      data-card-key={frame?.cardKey}
      draggable={frame?.armed ?? false}
      onDragStart={frame?.onDragStart}
      onDragOver={frame?.onDragOver}
      onDragLeave={frame?.onDragLeave}
      onDrop={frame?.onDrop}
      onDragEnd={frame?.onDragEnd}
    >
      <div className="aq-card-head">
        {frame && (
          <span
            className="aq-drag-handle"
            onPointerDown={frame.onHandleDown}
            title={t("dragToReorder")}
            aria-label={t("dragToReorder")}
            role="button"
          >
            <GripVertical size={13} />
          </span>
        )}
        <span className="aq-card-icon"><img src={cardBrandLogoSrc(brand) ?? undefined} alt="" draggable={false} /></span>
        <span className="aq-card-product">{product}</span>
        {tag}
        <span className="aq-card-actions">
          {extraAction}
          {onRefresh && (
            <button
              type="button"
              className="aq-icon-btn"
              onClick={onRefresh}
              disabled={loading}
              title={t("refresh")}
              aria-label={t("refresh")}
            >
              <RefreshCw size={12} className={loading ? "spin" : undefined} />
            </button>
          )}
          {onHide && (
            <button
              type="button"
              className="aq-icon-btn"
              onClick={onHide}
              title={t("hideCard")}
              aria-label={`${t("hideCard")} ${product}`}
            >
              <EyeOff size={12} />
            </button>
          )}
          {onDelete && (
            <button
              type="button"
              className="aq-icon-btn aq-icon-btn-danger"
              onClick={onDelete}
              title={t("removeAuth")}
              aria-label={`${t("removeAuth")} ${product}`}
            >
              <Trash2 size={12} />
            </button>
          )}
        </span>
      </div>
      {children}
      {(loading || at != null || footNote || footLead) && (
        <div className="aq-card-foot">
          {footLead}
          {footNote && (
            <span className="aq-card-foot-note" title={footNote}>
              {footNote}
            </span>
          )}
          <span className="aq-card-foot-age">
            {loading ? t("refreshing") : at != null ? formatAge(at, now, t) : null}
          </span>
        </div>
      )}
    </div>
  );
}

function IdentityRow({ name, badge, mono }: { name: string | null; badge?: ReactNode; mono?: boolean }) {
  if (!name && !badge) return null;
  return (
    <div className="aq-identity">
      <span className={`aq-identity-name${mono ? " aq-mono" : ""}`} title={name ?? undefined}>
        {name ?? "—"}
      </span>
      {badge && <span className="aq-badge">{badge}</span>}
    </div>
  );
}

type Detail = { label: string; value: string; tone?: Tone };
/** 明细固定三列；超过这个字数（如 `2,900 / 5,950`）的值占两列，窄卡片里也不被截断。 */
const DETAIL_WIDE_CHARS = 11;
/** 一行用量：左名称 + 倒计时，右剩余值，下方剩余条。 */
function MeterRow({
  label,
  countdown,
  value,
  percent,
  hint,
}: {
  label?: string;
  countdown?: { text: string; tone: Tone } | null;
  value: string;
  percent: number;
  hint?: string;
}) {
  const fillTone = toneOf(percent);
  return (
    <div className="aq-meter">
      <div className="aq-meter-head">
        {label && <span className="aq-meter-label">{label}</span>}
        {countdown && (
          <span className={`aq-countdown aq-chip-${countdown.tone}`}>
            <Clock size={10} />
            {countdown.text}
          </span>
        )}
        <span className={`aq-meter-value aq-value-${fillTone}`}>
          {value.endsWith("%") ? (
            <>
              {value.slice(0, -1)}
              <small>%</small>
            </>
          ) : (
            value
          )}
        </span>
      </div>
      <div
        className={`aq-track aq-track-${fillTone}`}
        role="meter"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
      >
        <div
          className={`aq-track-fill${percent <= 0 ? " is-empty" : ""}`}
          style={{ transform: `translateX(${Math.max(percent, percent > 0 ? 2 : 0) - 100}%)` }}
        />
      </div>
      {hint && <div className="aq-meter-hint">{hint}</div>}
    </div>
  );
}

function DetailGrid({ items }: { items: Detail[] }) {
  if (items.length === 0) return null;
  return (
    <dl className="aq-details">
      {items.map((d) => (
        <div className={d.value.length > DETAIL_WIDE_CHARS ? "aq-detail is-wide" : "aq-detail"} key={d.label}>
          <dt>{d.label}</dt>
          <dd className={d.tone ? `aq-tone-${d.tone}` : undefined} title={d.value}>
            {d.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** 卡片的空态 / 错误态；授权失效时错误条里直接给出重新授权（或更换 Key）。 */
function CardMessage({
  quota,
  idleText,
  onReauth,
  reauthPending,
  reauthText,
}: {
  quota: { ok: boolean; message: string | null; needsReauth?: boolean } | null | undefined;
  idleText: string;
  onReauth?: () => void;
  reauthPending?: boolean;
  reauthText?: string;
}) {
  const { t } = useTranslation("quota");
  if (!quota) return <div className="aq-card-idle">{idleText}</div>;
  if (!quota.ok) {
    const action = quota.needsReauth && onReauth;
    return (
      <div className={`aq-card-error${action ? " has-action" : ""}`} role="alert">
        <span>{friendlyQuotaMessage(quota.message, t("networkFailed")) ?? t("queryFailed")}</span>
        {action && (
          <button type="button" className="btn btn-primary btn-small" onClick={onReauth} disabled={reauthPending}>
            {reauthPending ? <RefreshCw size={12} className="spin" /> : <LogIn size={12} />}
            {reauthPending ? t("authPendingButton") : (reauthText ?? t("reauth"))}
          </button>
        )}
      </div>
    );
  }
  return null;
}

/** WorkBuddy 卡的签到条：今日已签 / 未签 / 失败 / 被自动停用，未签与失败时可直接签到。 */
function CheckinRow({
  account,
  busy,
  result,
  onCheckin,
}: {
  account: WbAccount;
  busy: boolean;
  result: WbCheckinResult | undefined;
  onCheckin: () => void;
}) {
  const { t } = useTranslation("quota");
  const now = useContext(NowContext);
  const done = checkedInToday(account, now);
  const failedToday =
    !done &&
    account.lastCheckinStatus === "fail" &&
    account.lastCheckinAt != null &&
    isSameLocalDay(account.lastCheckinAt, now);

  let tone: "ok" | "bad" | "idle" = "idle";
  let text = t("checkinNotToday");
  if (done) {
    tone = "ok";
    const time = new Date(account.lastCheckinAt as number).toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    });
    text = t("checkinDoneToday", { time });
  } else if (result?.disabled || !account.enabled) {
    tone = "bad";
    text = t("checkinDisabled");
  } else if (result && !result.ok) {
    tone = "bad";
    text = t("checkinFailed", { message: result.message });
  } else if (failedToday) {
    tone = "bad";
    text = t("checkinFailedLast");
  }

  return (
    <div className={`aq-checkin aq-checkin-${tone}`}>
      <CalendarCheck size={12} className="aq-checkin-icon" />
      <span className="aq-checkin-text" title={text}>
        {text}
      </span>
      {!done && account.enabled && !result?.disabled && (
        <button type="button" className="aq-checkin-btn" onClick={onCheckin} disabled={busy}>
          {busy ? <RefreshCw size={11} className="spin" /> : null}
          {tone === "bad" ? t("checkinRetry") : t("checkin")}
        </button>
      )}
    </div>
  );
}

function WorkbuddyCard({
  account,
  quota,
  loading,
  onRefresh,
  onHide,
  onDelete,
  onAddAnother,
  checkinBusy,
  checkinResult,
  autoCheckin,
  onCheckin,
  frame,
}: {
  account: WbAccount;
  quota: WbQuotaStatus | undefined;
  loading: boolean;
  onRefresh: () => void;
  onHide: () => void;
  onDelete: () => void;
  onAddAnother: () => void;
  checkinBusy: boolean;
  checkinResult: WbCheckinResult | undefined;
  /** 每日自动签到的提示文案；设置还没读到时为 null。 */
  autoCheckin: string | null;
  onCheckin: () => void;
  frame: CardFrame;
}) {
  const { t } = useTranslation("quota");
  const abnormal = account.status !== "active";
  const failure =
    quota && !quota.ok
      ? { ok: false, message: quota.configured ? `${t("queryFailed")}：${quota.message ?? ""}` : t("notConfigured") }
      : quota;
  const details: Detail[] = [];
  if (quota?.ok && quota.creditsLeft != null) {
    details.push({
      label: t("wbTotal"),
      value:
        quota.creditsTotal != null
          ? `${formatCredits(quota.creditsLeft)} / ${formatCredits(quota.creditsTotal)}`
          : formatCredits(quota.creditsLeft),
    });
  }
  if (quota?.ok && account.expUnix != null) {
    details.push({ label: t("expiry"), value: formatDate(account.expUnix * 1000) });
  }

  return (
    <QuotaCardShell
      brand="wb"
      product="WorkBuddy"
      tag={abnormal ? <span className={`aq-tag aq-tag-${account.status}`}>{t(STATUS_KEYS[account.status])}</span> : null}
      at={quota?.at}
      loading={loading}
      onRefresh={onRefresh}
      onHide={onHide}
      onDelete={onDelete}
      extraAction={<AddAnotherButton onClick={onAddAnother} />}
      footLead={<CheckinRow account={account} busy={checkinBusy} result={checkinResult} onCheckin={onCheckin} />}
      footNote={autoCheckin}
      frame={frame}
    >
      <IdentityRow name={account.label} badge={quota?.planName ?? undefined} />
      <CardMessage quota={failure} idleText={t("wbIdle")} />
      {quota?.ok && quota.packages.length > 0 && (
        <div className="aq-card-body">
          {sortPackages(quota.packages).map((p, i) => {
            const kind = packageKind(p.code);
            const label =
              kind === "base" ? t("packageBase") : kind === "reward" ? t("packageReward") : t("packageLabel", { index: i + 1 });
            return <MeterRow key={p.code ?? i} label={label} value={formatCredits(p.remain)} percent={packagePct(p)} />;
          })}
          <DetailGrid items={details} />
        </div>
      )}
      {quota?.ok && quota.packages.length === 0 && (
        <div className="aq-card-idle">
          {quota.creditsLeft != null ? `${t("creditsLeft")} ${formatCredits(quota.creditsLeft)}` : t("neverRefreshed")}
        </div>
      )}
    </QuotaCardShell>
  );
}

/** 团队 / 个人账号标签；上游没给出判断依据时不显示。 */
function SeatTag({ team, title }: { team: boolean | null | undefined; title?: string }) {
  const { t } = useTranslation("quota");
  if (team == null) return null;
  return (
    <span className={`aq-tag ${team ? "aq-tag-team" : ""}`} title={title}>
      {team ? t("seatTeam") : t("seatPersonal")}
    </span>
  );
}

const CURSOR_PERSONAL_PLANS = ["free", "free_trial", "pro", "pro_plus", "pro_student", "ultra", "express"];

/**
 * 与 Cursor 自己一致按套餐类型判断：enterprise 即团队 / 企业席位。
 * limitType 只说明按量预算按团队还是按人算，团队成员也可能是 user，只在没有套餐类型时兜底。
 */
function cursorIsTeam(quota: CursorQuotaDto | undefined): boolean | null {
  if (!quota?.ok) return null;
  const plan = quota.membershipType?.toLowerCase();
  if (plan === "enterprise" || plan === "team") return true;
  if (plan && CURSOR_PERSONAL_PLANS.includes(plan)) return false;
  return quota.limitType?.toLowerCase() === "team" ? true : null;
}

type AuthCardProps = {
  account: QuotaAccount;
  loading: boolean;
  onRefresh: () => void;
  onDelete: () => void;
  onAddAnother?: () => void;
  onReauth: () => void;
  reauthPending: boolean;
  frame: CardFrame;
};

function CursorCard({
  account,
  quota,
  loading,
  onRefresh,
  onDelete,
  onAddAnother,
  onReauth,
  reauthPending,
  frame,
}: AuthCardProps & { quota: CursorQuotaDto | undefined }) {
  const { t } = useTranslation("quota");
  const now = useContext(NowContext);
  const ok = quota?.ok === true;
  const included = quota?.includedSpend ?? quota?.totalSpend ?? null;
  const remainPct = ok ? cursorRemainPct(quota) : null;
  const details: Detail[] = [];
  if (ok) {
    if (quota.autoPercentUsed != null) {
      details.push({ label: t("cursorAutoPool"), value: `${clampPct(100 - cursorUsedPct(quota.autoPercentUsed))}%` });
    }
    if (quota.apiPercentUsed != null) {
      details.push({ label: t("cursorApiPool"), value: `${clampPct(100 - cursorUsedPct(quota.apiPercentUsed))}%` });
    }
    // totalSpend 含套餐外赠送用量；套餐内花费只看 includedSpend，赠送单列。
    if (quota.limit != null) {
      details.push({
        label: t("cursorSpendLabel"),
        value: t("cursorSpend", { spend: formatSpend(included), limit: formatSpend(quota.limit) }),
        tone: included != null && included >= quota.limit ? "warn" : undefined,
      });
    }
    // 按量付费预算：套餐用完后才开始扣；没开（上限为 0）且没花过时不占位。
    const onDemand = (used: number | null, limit: number | null) =>
      limit != null && limit > 0
        ? t("cursorSpend", { spend: formatSpend(used ?? 0), limit: formatSpend(limit) })
        : used != null && used > 0
          ? formatSpend(used)
          : null;
    const mine = onDemand(quota.individualUsed, quota.individualLimit);
    if (mine) {
      details.push({
        label: t("cursorOnDemand"),
        value: mine,
        tone: quota.individualLimit && (quota.individualUsed ?? 0) >= quota.individualLimit ? "warn" : undefined,
      });
    }
    const team = onDemand(quota.pooledUsed, quota.pooledLimit);
    if (team) details.push({ label: t("cursorTeamPool"), value: team });
    if (quota.bonusSpend != null && quota.bonusSpend > 0) {
      details.push({ label: t("cursorBonus"), value: formatSpend(quota.bonusSpend) });
    }
  }

  return (
    <QuotaCardShell
      brand="cursor"
      product="Cursor"
      tag={<SeatTag team={cursorIsTeam(quota)} title={quota?.teamName ?? undefined} />}
      at={quota?.at}
      loading={loading}
      onRefresh={onRefresh}
      onDelete={onDelete}
      extraAction={<AddAnotherButton onClick={onAddAnother} />}
      frame={frame}
    >
      <IdentityRow name={quota?.email ?? account.label} badge={quota?.membershipType ?? undefined} mono />
      <CardMessage quota={quota} idleText={t("neverRefreshed")} onReauth={onReauth} reauthPending={reauthPending} />
      {ok && remainPct != null && (
        <div className="aq-card-body">
          <MeterRow
            label={t("cursorIncluded")}
            countdown={compactCountdown(quota.cycleEndMs, now)}
            value={`${remainPct}%`}
            percent={remainPct}
          />
          <DetailGrid items={details} />
        </div>
      )}
    </QuotaCardShell>
  );
}

function GrokCard({
  account,
  quota,
  loading,
  onRefresh,
  onDelete,
  onAddAnother,
  onReauth,
  reauthPending,
  frame,
}: AuthCardProps & { quota: GrokQuotaDto | undefined }) {
  const { t } = useTranslation("quota");
  const now = useContext(NowContext);
  const ok = quota?.ok === true;
  const remainPct = ok ? grokRemainPct(quota) : null;
  const plan = quota?.grokPlanLabel ?? quota?.cursorPlanName ?? null;
  const details: Detail[] = [];
  if (ok) {
    if (quota.grokPlanLabel && quota.cursorPlanName) {
      details.push({ label: t("grokCursorPlan"), value: quota.cursorPlanName });
    }
    details.push({
      label: t("grokResetAt"),
      value: quota.nextResetMs != null ? formatDate(quota.nextResetMs) : t("grokResetNone"),
    });
    if (quota.periodStartMs != null) {
      details.push({ label: t("grokPeriodStart"), value: formatDate(quota.periodStartMs) });
    }
  }

  return (
    <QuotaCardShell
      brand="grok"
      product="Grok Bot"
      tag={<SeatTag team={quota?.ok ? quota.isTeamSeat : null} />}
      at={quota?.at}
      loading={loading}
      onRefresh={onRefresh}
      onDelete={onDelete}
      extraAction={<AddAnotherButton onClick={onAddAnother} />}
      frame={frame}
    >
      <IdentityRow name={quota?.email ?? account.label} badge={plan ?? undefined} mono />
      <CardMessage quota={quota} idleText={t("neverRefreshed")} onReauth={onReauth} reauthPending={reauthPending} />
      {ok && remainPct != null && (
        <div className="aq-card-body">
          <MeterRow
            label={t("grokUsage")}
            countdown={compactCountdown(quota.nextResetMs, now)}
            value={`${remainPct}%`}
            percent={remainPct}
            hint={quota.hasAvailableUsage === false && remainPct === 0 ? t("grokExhausted") : undefined}
          />
          <DetailGrid items={details} />
        </div>
      )}
    </QuotaCardShell>
  );
}

/** 上游用 9999-12-31 表示不过期；与后端同一阈值（2100 年），兼顾升级前缓存的旧结果。 */
const QODER_NO_EXPIRY_MS = 4_102_444_800_000;

function qoderAmount(q: QoderQuota): string {
  return `${formatCredits(q.remaining)} / ${formatCredits(q.total)}`;
}

function openExternal(url: string) {
  invoke("open_in_browser", { url }).catch(() => window.open(url, "_blank"));
}

const QODER_SITE = "https://qoder.cn";

/** 本轮还没过期的每日积分活动；缓存里可能留着已结束的一轮。 */
function liveQoderCampaign(quota: QoderQuotaDto | undefined, nowMs: number): QoderCampaign | null {
  const c = quota?.ok ? quota.campaign : null;
  if (!c) return null;
  return c.endAt > 0 && c.endAt * 1000 <= nowMs ? null : c;
}

/**
 * Qoder 卡的每日积分条：先直调一次领取接口；失败（多半是风控要求客户端设备身份）
 * 就启动本机 Qoder CN，由客户端弹出领取页，启动不了再开官网。
 */
function QoderCheckinRow({
  accountId,
  campaign,
  onClaimed,
}: {
  accountId: number;
  campaign: QoderCampaign;
  onClaimed: () => void;
}) {
  const { t } = useTranslation("quota");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<QoderClaimResult | null>(null);
  const [launchFailed, setLaunchFailed] = useState(false);
  const amount = formatCredits(campaign.amount);

  const launch = useCallback(async () => {
    try {
      await qoderLaunchApp();
      setLaunchFailed(false);
    } catch {
      setLaunchFailed(true);
      openExternal(QODER_SITE);
    }
  }, []);

  const claim = useCallback(async () => {
    setBusy(true);
    try {
      const r = await qoderCampaignClaim(accountId, campaign.campaignId).catch(
        (e): QoderClaimResult => ({ ok: false, errorCode: null, message: errorText(e), risk: false, needsReauth: false }),
      );
      if (r.ok) {
        setFailure(null);
        onClaimed();
        return;
      }
      setFailure(r);
      if (!r.needsReauth) await launch();
    } finally {
      setBusy(false);
    }
  }, [accountId, campaign.campaignId, launch, onClaimed]);

  let tone: "ok" | "bad" | "idle" = "idle";
  let text: string;
  if (!campaign.claimable) {
    tone = "ok";
    text = t("qoderClaimed", { amount });
  } else if (failure) {
    tone = "bad";
    text = failure.risk ? t("qoderClaimRisk") : t("qoderClaimFailed", { message: failure.message ?? "" });
    if (launchFailed) text = `${text} · ${t("qoderLaunchFailed")}`;
  } else {
    const end =
      campaign.endAt > 0
        ? new Date(campaign.endAt * 1000).toLocaleString(undefined, {
            month: "2-digit",
            day: "2-digit",
            hour: "2-digit",
            minute: "2-digit",
          })
        : null;
    text = end ? t("qoderClaimable", { amount, end }) : t("qoderClaimableNoEnd", { amount });
  }

  return (
    <div className={`aq-checkin aq-checkin-${tone}`}>
      <CalendarCheck size={12} className="aq-checkin-icon" />
      <span className="aq-checkin-text" title={text}>
        {text}
      </span>
      {campaign.claimable && (
        <button
          type="button"
          className="aq-checkin-btn"
          onClick={() => void (failure && !failure.needsReauth ? launch() : claim())}
          disabled={busy}
        >
          {busy ? <RefreshCw size={11} className="spin" /> : null}
          {failure && !failure.needsReauth ? t("qoderClaimOpen") : t("qoderClaim")}
        </button>
      )}
    </div>
  );
}

function QoderCard({
  account,
  quota,
  loading,
  onRefresh,
  onDelete,
  onAddAnother,
  onReauth,
  reauthPending,
  frame,
}: AuthCardProps & { quota: QoderQuotaDto | undefined }) {
  const { t } = useTranslation("quota");
  const now = useContext(NowContext);
  const ok = quota?.ok === true;
  const expiresAtMs = ok && quota.expiresAtMs != null && quota.expiresAtMs < QODER_NO_EXPIRY_MS ? quota.expiresAtMs : null;
  const campaign = liveQoderCampaign(quota, now);
  // 与 Qoder「我的用量」一致：套餐内 Credits 与资源包各一条；卡片整体状态仍按两池之和。
  // 只要有一池非空，空的套餐池也画成 0% 条，两池上下对齐。
  const allPools = ok
    ? [
        { key: "plan", label: t("qoderCredits"), q: quota.user, countdown: compactCountdown(expiresAtMs, now) },
        { key: "addOn", label: t("qoderAddOn"), q: quota.addOn, countdown: null },
      ].filter((p): p is typeof p & { q: QoderQuota } => p.q != null)
    : [];
  const pools = allPools.some((p) => p.q.total > 0)
    ? allPools.filter((p) => p.key === "plan" || p.q.total > 0)
    : [];
  const details: Detail[] = [];
  if (ok) {
    if (quota.org && quota.org.total > 0) {
      details.push({ label: t("qoderOrg"), value: qoderAmount(quota.org) });
    }
    if (expiresAtMs != null) {
      details.push({ label: t("qoderExpires"), value: formatDate(expiresAtMs) });
    }
  }
  return (
    <QuotaCardShell
      brand="qoder"
      product="Qoder"
      at={quota?.at}
      loading={loading}
      onRefresh={onRefresh}
      onDelete={onDelete}
      extraAction={<AddAnotherButton onClick={onAddAnother} />}
      frame={frame}
    >
      <IdentityRow name={quota?.label ?? account.label} badge={quota?.userType ?? undefined} mono />
      <CardMessage quota={quota} idleText={t("neverRefreshed")} onReauth={onReauth} reauthPending={reauthPending} />
      {ok && quota.enterpriseUrl && (
        <div className="aq-card-idle">
          {t("qoderEnterprise")}{" "}
          <button type="button" className="btn btn-secondary btn-small" onClick={() => openExternal(quota.enterpriseUrl!)}>
            <ExternalLink size={12} />
            {t("qoderEnterpriseLink")}
          </button>
        </div>
      )}
      {ok && pools.length > 0 && (
        <div className="aq-card-body">
          {pools.map(({ key, label, q, countdown }) => {
            const pct = q.total > 0 ? clampPct((Math.min(q.remaining, q.total) / q.total) * 100) : 0;
            return (
              <MeterRow
                key={key}
                label={label}
                countdown={countdown}
                value={`${pct}%`}
                percent={pct}
                hint={t("qoderCreditsHint", {
                  used: formatCredits(q.used),
                  remaining: formatCredits(q.remaining),
                  total: formatCredits(q.total),
                  unit: q.unit,
                })}
              />
            );
          })}
          <DetailGrid items={details} />
        </div>
      )}
      {campaign && <QoderCheckinRow accountId={account.id} campaign={campaign} onClaimed={onRefresh} />}
    </QuotaCardShell>
  );
}

/** 限额窗口名：Codex 固定是 5 小时与每周两档，其他长度按小时 / 天兜底。 */
function codexWindowLabel(w: CodexWindow, fallback: string, t: TFunction<"quota">): string {
  const s = w.windowSeconds;
  if (s == null) return fallback;
  if (s === 18_000) return t("codex5h");
  if (s === 604_800) return t("codexWeekly");
  return s % 86_400 === 0 ? t("codexWindowDays", { n: s / 86_400 }) : t("codexWindowHours", { n: Math.round(s / 3_600) });
}

/** 未授权卡片的内容：说明 + 授权登录按钮；等待浏览器回来时换成取消。 */
function AuthPrompt({
  hint,
  pending,
  error,
  loginText,
  onLogin,
  onCancel,
}: {
  hint: string;
  pending: boolean;
  error: string | null;
  loginText?: string;
  onLogin: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation("quota");
  return (
    <div className="aq-auth">
      <p className="aq-auth-text">{pending ? t("authPendingShort") : hint}</p>
      {error && !pending && (
        <div className="aq-card-error" role="alert">
          {friendlyQuotaMessage(error, t("networkFailed")) ?? error}
        </div>
      )}
      {pending ? (
        <button type="button" className="btn btn-secondary btn-small" onClick={onCancel}>
          <RefreshCw size={13} className="spin" />
          {t("authCancel")}
        </button>
      ) : (
        <button type="button" className="btn btn-primary btn-small" onClick={onLogin}>
          <LogIn size={13} />
          {loginText ?? t("authLogin")}
        </button>
      )}
    </div>
  );
}

/** 卡片头的「授权另一个账号」；授权进行中时不显示。 */
function AddAnotherButton({ onClick }: { onClick?: () => void }) {
  const { t } = useTranslation("quota");
  if (!onClick) return null;
  return (
    <button type="button" className="aq-icon-btn" onClick={onClick} title={t("authAddAnother")} aria-label={t("authAddAnother")}>
      <UserPlus size={12} />
    </button>
  );
}

function DeepSeekKeyForm({
  onSubmit,
  onCancel,
  hint,
  submitText,
}: {
  onSubmit: (apiKey: string) => Promise<void>;
  onCancel?: () => void;
  hint?: string;
  submitText?: string;
}) {
  const { t } = useTranslation("quota");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async () => {
    if (!key.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onSubmit(key.trim());
      setKey("");
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className="aq-auth aq-key-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <p className="aq-auth-text">{hint ?? t("authHint.deepseek")}</p>
      <input
        className="input-field aq-mono"
        type="password"
        autoComplete="off"
        spellCheck={false}
        placeholder="sk-…"
        value={key}
        onChange={(e) => setKey(e.target.value)}
        disabled={busy}
        autoFocus={onCancel != null}
      />
      {error && (
        <div className="aq-card-error" role="alert">
          {friendlyQuotaMessage(error, t("networkFailed")) ?? error}
        </div>
      )}
      <div className="aq-key-actions">
        {onCancel && (
          <button type="button" className="btn btn-ghost btn-small" onClick={onCancel} disabled={busy}>
            {t("authCancel")}
          </button>
        )}
        <button type="submit" className="btn btn-primary btn-small" disabled={busy || !key.trim()}>
          {busy ? <RefreshCw size={13} className="spin" /> : <KeyRound size={13} />}
          {busy ? t("dsVerifying") : (submitText ?? t("dsSaveKey"))}
        </button>
      </div>
    </form>
  );
}

const CURRENCY_SYMBOL: Record<string, string> = { CNY: "¥", USD: "$" };

function formatMoney(amount: number, currency: string): string {
  const symbol = CURRENCY_SYMBOL[currency];
  const n = amount.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  return symbol ? `${symbol}${n}` : `${n} ${currency}`;
}

function DeepSeekCard({
  account,
  quota,
  loading,
  peaks,
  replacing,
  onRefresh,
  onDelete,
  onReplace,
  onStartReplace,
  onCancelReplace,
  onAddAnother,
  frame,
}: {
  account: QuotaAccount;
  quota: DeepSeekQuotaDto | undefined;
  loading: boolean;
  /** `deepseek:<id>:<币种>` → 见过的最高余额，作进度条的满额。 */
  peaks: Record<string, number>;
  replacing: boolean;
  onRefresh: () => void;
  onDelete: () => void;
  onReplace: (apiKey: string) => Promise<void>;
  onStartReplace: () => void;
  onCancelReplace: () => void;
  onAddAnother?: () => void;
  frame: CardFrame;
}) {
  const { t } = useTranslation("quota");
  const ok = quota?.ok === true && !replacing;
  return (
    <QuotaCardShell
      brand="deepseek"
      product="DeepSeek"
      tag={quota?.ok && quota.isAvailable === false ? <span className="aq-tag aq-tag-expired">{t("dsUnavailable")}</span> : null}
      at={replacing ? null : quota?.at}
      loading={loading}
      onRefresh={onRefresh}
      onDelete={onDelete}
      extraAction={<AddAnotherButton onClick={onAddAnother} />}
      frame={frame}
    >
      <IdentityRow name={account.label} badge="API" mono />
      {replacing ? (
        <DeepSeekKeyForm onSubmit={onReplace} onCancel={onCancelReplace} hint={t("dsReplaceHint")} submitText={t("dsReplaceKey")} />
      ) : (
        <CardMessage quota={quota} idleText={t("neverRefreshed")} onReauth={onStartReplace} reauthText={t("dsReplaceKey")} />
      )}
      {ok && quota.balances.length === 0 && <div className="aq-card-idle">{t("dsNoBalance")}</div>}
      {ok && quota.balances.length > 0 && (
        <div className="aq-card-body">
          {quota.balances.map((b) => (
            <MeterRow
              key={b.currency}
              label={quota.balances.length > 1 ? `${t("dsBalance")} ${b.currency}` : t("dsBalance")}
              value={formatMoney(b.total, b.currency)}
              percent={balancePct(b.total, peaks[`${frame.cardKey}:${b.currency}`])}
            />
          ))}
          <DetailGrid
            items={quota.balances.flatMap((b) => {
              const suffix = quota.balances.length > 1 ? ` ${b.currency}` : "";
              const peak = peaks[`${frame.cardKey}:${b.currency}`];
              return [
                { label: `${t("dsToppedUp")}${suffix}`, value: formatMoney(b.toppedUp, b.currency) },
                { label: `${t("dsGranted")}${suffix}`, value: formatMoney(b.granted, b.currency) },
                ...(peak != null ? [{ label: `${t("dsPeak")}${suffix}`, value: formatMoney(peak, b.currency) }] : []),
              ];
            })}
          />
        </div>
      )}
    </QuotaCardShell>
  );
}

function CodexCard({
  account,
  quota,
  loading,
  onRefresh,
  onDelete,
  onAddAnother,
  onReauth,
  reauthPending,
  frame,
}: {
  account: CodexAccount;
  quota: CodexQuotaDto | undefined;
  loading: boolean;
  onRefresh: () => void;
  onDelete: () => void;
  onAddAnother?: () => void;
  onReauth: () => void;
  reauthPending: boolean;
  frame: CardFrame;
}) {
  const { t } = useTranslation("quota");
  const now = useContext(NowContext);
  const ok = quota?.ok === true;
  const windows = ok
    ? [
        { w: quota.primary, fallback: t("codexPrimary") },
        { w: quota.secondary, fallback: t("codexSecondary") },
      ].filter((x): x is { w: CodexWindow; fallback: string } => x.w != null)
    : [];
  const details: Detail[] = [];
  if (ok) {
    if (quota.creditsUnlimited) {
      details.push({ label: t("codexCredits"), value: t("codexCreditsUnlimited") });
    } else if (quota.creditsBalance != null && quota.creditsBalance > 0) {
      details.push({ label: t("codexCredits"), value: formatCredits(quota.creditsBalance) });
    }
  }

  return (
    <QuotaCardShell
      brand="codex"
      product="Codex"
      tag={quota?.limitReached ? <span className="aq-tag aq-tag-expired">{t("codexLimitReached")}</span> : null}
      at={quota?.at}
      loading={loading}
      onRefresh={onRefresh}
      onDelete={onDelete}
      extraAction={<AddAnotherButton onClick={onAddAnother} />}
      frame={frame}
    >
      <IdentityRow
        name={quota?.email ?? account.email}
        badge={quota?.planType ?? account.planType ?? undefined}
        mono
      />
      <CardMessage quota={quota} idleText={t("neverRefreshed")} onReauth={onReauth} reauthPending={reauthPending} />
      {ok && windows.length === 0 && <div className="aq-card-idle">{t("codexNoLimits")}</div>}
      {windows.length > 0 && (
        <div className="aq-card-body">
          {windows.map(({ w, fallback }) => {
            const remainPct = clampPct(100 - w.usedPercent);
            return (
              <MeterRow
                key={fallback}
                label={codexWindowLabel(w, fallback, t)}
                countdown={compactCountdown(w.resetAtMs, now)}
                value={`${remainPct}%`}
                percent={remainPct}
              />
            );
          })}
          <DetailGrid items={details} />
        </div>
      )}
    </QuotaCardShell>
  );
}

export default AccountQuotaPanel;
