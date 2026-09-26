/** WorkBuddy Manager — IPC wrappers and wire types.
 *
 *  Rust 侧字段为 camelCase（serde rename_all）。凭证只回预览串。
 */
import { invoke } from "@tauri-apps/api/core";

export type WbCredentialType = "token" | "cookie";

/** 与 codebuddy_accounts.status 一致。 */
export type WbAccountStatus = "active" | "unverified" | "expired" | "banned";

export interface WbAccount {
  id: number;
  label: string;
  credentialType: WbCredentialType;
  credentialPreview: string;
  email: string | null;
  expUnix: number | null;
  status: WbAccountStatus;
  enabled: boolean;
  lastCheckinAt: number | null;
  lastCheckinStatus: string | null;
  checkinFailCount: number;
  notes: string | null;
  createdAt: number;
}

export interface WbAddAccountInput {
  label: string;
  credentialType: WbCredentialType;
  credentialRaw: string;
  email?: string | null;
  notes?: string | null;
}

export interface WbAccountPatch {
  label?: string;
  notes?: string;
  enabled?: boolean;
}

export interface WbProbeResult {
  ok: boolean;
  expUnix: number | null;
  status: WbAccountStatus;
  reason: string | null;
}

export interface WbSettings {
  port: number;
  lanEnabled: boolean;
  checkinEnabled: boolean;
  checkinTime: string;
  lastAutoCheckinDate: string | null;
  adapterJson: string | null;
}

export interface WbSettingsPatch {
  port?: number;
  lanEnabled?: boolean;
  checkinEnabled?: boolean;
  checkinTime?: string;
  adapterJson?: string;
}

export const wbListAccounts = () => invoke<WbAccount[]>("wb_list_accounts");

export const wbAddAccount = (input: WbAddAccountInput) => invoke<number>("wb_add_account_manual", { input });

export const wbUpdateAccount = (id: number, patch: WbAccountPatch) =>
  invoke<void>("wb_update_account", { id, patch });

export const wbDeleteAccount = (id: number) => invoke<void>("wb_delete_account", { id });

export const wbProbeAccount = (id: number) => invoke<WbProbeResult>("wb_probe_account", { id });

export const wbGetSettings = () => invoke<WbSettings>("wb_get_settings");

export const wbUpdateSettings = (patch: WbSettingsPatch) => invoke<WbSettings>("wb_update_settings", { patch });

/** epoch 秒 → 本地日期串；无 exp 时给中性提示。 */
export function formatExpiry(expUnix: number | null): { text: string; tone: "none" | "ok" | "warn" | "bad" } {
  if (expUnix == null) return { text: "—", tone: "none" };
  const days = (expUnix * 1000 - Date.now()) / 86_400_000;
  const date = new Date(expUnix * 1000).toLocaleDateString();
  if (days <= 0) return { text: `${date} 已过期`, tone: "bad" };
  if (days <= 7) return { text: `${date}（${Math.ceil(days)} 天）`, tone: "warn" };
  return { text: date, tone: "ok" };
}

// ---------------------------------------------------------------------------
// 网关 / 密钥 / IP 规则
// ---------------------------------------------------------------------------

export interface GatewayStatus {
  running: boolean;
  bindAddr: string | null;
  port: number;
  lanEnabled: boolean;
  startedAt: number | null;
  keyCount: number;
  accountTotal: number;
  accountEligible: number;
}

export interface WbApiKey {
  id: number;
  keyMasked: string;
  label: string;
  enabled: boolean;
  createdAt: number;
  rotatedAt: number | null;
  lastUsedAt: number | null;
  callCount: number;
}

export interface WbCreatedKey {
  id: number;
  /** 仅此一次返回明文，之后只能重置。 */
  key: string;
  label: string;
}

export type WbIpRuleKind = "allow" | "deny";

export interface WbIpRule {
  id: string;
  kind: WbIpRuleKind;
  ipOrCidr: string;
  enabled: boolean;
  note: string | null;
}

export const wbGatewayStart = (port?: number, lan?: boolean) =>
  invoke<GatewayStatus>("wb_gateway_start", { port, lan });

export const wbGatewayStop = () => invoke<GatewayStatus>("wb_gateway_stop");

export const wbGatewayStatus = () => invoke<GatewayStatus>("wb_gateway_status");

export const wbSetLanMode = (enabled: boolean) => invoke<GatewayStatus>("wb_set_lan_mode", { enabled });

export const wbKeyCreate = (label: string) => invoke<WbCreatedKey>("wb_key_create", { label });

export const wbKeyList = () => invoke<WbApiKey[]>("wb_key_list");

export const wbKeySetEnabled = (id: number, enabled: boolean) =>
  invoke<void>("wb_key_set_enabled", { id, enabled });

export const wbKeyDelete = (id: number) => invoke<void>("wb_key_delete", { id });

export const wbKeyReset = (id: number) => invoke<string>("wb_key_reset", { id });

export const wbIpList = () => invoke<WbIpRule[]>("wb_ip_list");

export const wbIpAdd = (rule: { kind: WbIpRuleKind; ipOrCidr: string; note?: string | null }) =>
  invoke<WbIpRule>("wb_ip_add", { rule });

export const wbIpDelete = (id: string) => invoke<void>("wb_ip_delete", { id });

export const wbIpSetEnabled = (id: string, enabled: boolean) =>
  invoke<void>("wb_ip_set_enabled", { id, enabled });

// ---------------------------------------------------------------------------
// 调用日志与用量统计
// ---------------------------------------------------------------------------

export interface WbRequestLog {
  id: number;
  ts: number;
  keyId: number | null;
  keyLabel: string | null;
  accountId: number | null;
  accountLabel: string | null;
  model: string | null;
  stream: boolean;
  statusCode: number | null;
  promptTokens: number | null;
  completionTokens: number | null;
  latencyMs: number | null;
  error: string | null;
}

export interface WbStatRow {
  label: string;
  requests: number;
  promptTokens: number;
  completionTokens: number;
  errors: number;
  avgLatencyMs: number | null;
}

export type WbStatGroup = "day" | "model" | "key";

export interface WbLogFilter {
  limit?: number;
  keyId?: number | null;
  model?: string | null;
  since?: number | null;
}

export const wbLogList = (filter: WbLogFilter = {}) =>
  invoke<WbRequestLog[]>("wb_log_list", {
    limit: filter.limit ?? 200,
    keyId: filter.keyId ?? null,
    model: filter.model ?? null,
    since: filter.since ?? null,
  });

export const wbLogClear = (beforeTs?: number | null) =>
  invoke<number>("wb_log_clear", { beforeTs: beforeTs ?? null });

export const wbLogStats = (days: number, groupBy: WbStatGroup) =>
  invoke<WbStatRow[]>("wb_log_stats", { days, groupBy });

export const wbLogModels = () => invoke<string[]>("wb_log_models");

// ---------------------------------------------------------------------------
// 签到
// ---------------------------------------------------------------------------

export interface WbCheckinResult {
  accountId: number;
  label: string;
  ok: boolean;
  message: string;
  at: number;
  /** 本轮失败让该账号连败达到上限并被自动停用。 */
  disabled: boolean;
}

export interface WbCheckinLogEntry {
  id: number;
  accountId: number;
  label: string | null;
  ok: boolean;
  message: string | null;
  createdAt: number;
}

/** Rust 侧串行执行，整轮可能耗时数十秒（每账号间隔 2s）。 */
export const wbCheckinNow = (accountId?: number | null) =>
  invoke<WbCheckinResult[]>("wb_checkin_now", { accountId: accountId ?? null });

export const wbCheckinLogList = (limit = 100) => invoke<WbCheckinLogEntry[]>("wb_checkin_log_list", { limit });

export const wbCheckinLogClear = () => invoke<number>("wb_checkin_log_clear");

export interface WbCheckinStatus {
  accountId: number;
  label: string;
  /** 协议里没有 checkinStatus 端点时为 false。 */
  configured: boolean;
  ok: boolean;
  message: string | null;
  todayCheckedIn: boolean | null;
  streakDays: number | null;
  weekProgress: string | null;
  totalCredits: number | null;
  at: number;
}

/** 只读查询，串行 + 每账号间隔 2s；失败不写账号、不计签到连败。 */
export const wbCheckinStatus = (accountId?: number | null) =>
  invoke<WbCheckinStatus[]>("wb_checkin_status", { accountId: accountId ?? null });

export const WB_CHECKIN_STATUS_EVENT = "wb-checkin-status-done";

// ---------------------------------------------------------------------------
// 成长计划（只读，不自动兑换）
// ---------------------------------------------------------------------------

export interface WbGrowthTier {
  tier: number;
  daysRequired: number;
  claimed: boolean | null;
}

export interface WbGrowthStreak {
  days: number | null;
  tiers: WbGrowthTier[];
}

export interface WbGrowthTask {
  code: string | null;
  title: string | null;
  done: boolean | null;
}

export interface WbGrowthInfo {
  accountId: number;
  label: string;
  ok: boolean;
  /** 各端点失败原因（灰态提示）。 */
  reasons: string[];
  streak: WbGrowthStreak | null;
  tasks: WbGrowthTask[];
  at: number;
}

export const wbGrowthInfo = (accountId: number) =>
  invoke<WbGrowthInfo>("wb_growth_info", { accountId });

/** 后端逐账号推送的事件负载。 */
export const WB_CHECKIN_EVENT = "wb-checkin-done";

// ---------------------------------------------------------------------------
// 扫码纳管（登录窗 + 凭证捕获）
// ---------------------------------------------------------------------------

export interface CapturedCredential {
  batchId: string;
  credentialType: WbCredentialType;
  credentialRaw: string;
  /** 捕获来源：header:authorization / storage:xxx / cookie。 */
  via: string;
  /** 凭证超过回传上限，已不可信，需回落到手动粘贴。 */
  truncated: boolean;
}

export const WB_CAPTURE_EVENT = "wb-credential-captured";

/** 返回实际打开的登录地址；协议未配置 loginUrl 时后端会报错。 */
export const wbOpenCbLoginWindow = (batchId: string) =>
  invoke<string>("wb_open_cb_login_window", { batchId });

export const wbCloseCbLoginWindow = (batchId: string) => invoke<void>("wb_close_cb_login_window", { batchId });

/** 登录窗 Cookie 库快照：控制台用 httpOnly 会话 Cookie 鉴权，页面脚本抓不到，只能读 Cookie 库。 */
export interface WbCookieCapture {
  pageUrl: string;
  /** 窗口是否已离开 /login；没离开时那些 Cookie 只是 CSRF/state 噪声。 */
  loggedIn: boolean;
  cookieHeader: string;
  names: string[];
  truncated: boolean;
}

export const wbCaptureCbLoginCookies = (batchId: string) =>
  invoke<WbCookieCapture>("wb_capture_cb_login_cookies", { batchId });

/** 协议探针：登录窗里页面自己发出的调用（只回方法/路径/状态码/认证头名，不带值）。 */
export interface WbProbeCall {
  m: string;
  u: string;
  s: number;
  /** 逗号分隔的"看起来像凭证"的请求头名；为空说明浏览器是自动带 Cookie 的。 */
  a: string;
  n: number;
}

/** 注入脚本的自诊断计数（__wbCaptureStats）：脚本死没死、页内会话校验通没通。 */
export interface WbCaptureStats {
  installed: number;
  ticks: number;
  calls: number;
  /** 观察到带认证头的请求数；web 端恒为 0（Bearer 只在 miniProgram 模式挂）。 */
  auth: number;
  bodyScans: number;
  tokens: number;
  /** 页内用 Cookie 会话校验 credentialProbe 的 HTTP 状态码；0 = 还没跑/失败。 */
  cookieProbe: number;
  cookieOk: boolean;
  navigated: number;
}

export interface WbProbeReport {
  pageUrl: string;
  calls: WbProbeCall[];
  empty: boolean;
  /** null = 脚本没跑起来（CSP/新文档/读取超时）。 */
  captureStats: WbCaptureStats | null;
}

export const wbReadCbLoginProbe = (batchId: string) =>
  invoke<WbProbeReport>("wb_read_cb_login_probe", { batchId });

/** 登录窗内自检：让页面自己 fetch 一次，看它在浏览器会话下通不通（区分"端点错"和"凭证带不出去"）。 */
export interface WbSelfTestOutcome {
  url: string;
  /** 0 = 没跑成（超时/窗口无响应/fetch 抛错）。 */
  status: number;
  bodyHead: string;
  error: string | null;
}

export const wbCbLoginSelftest = (batchId: string, method: string, url: string) =>
  invoke<WbSelfTestOutcome>("wb_cb_login_selftest", { batchId, method, url });
