/**
 * 账号额度看板里授权登录得来的账号（Cursor / Grok Bot / DeepSeek / Qoder / Claude / ZCode / MiMo）— IPC 封装。
 * 令牌与 API Key 只存后端 SQLite，这里只拿到 id + 展示名。Codex 见 `codexQuota.ts`。
 */
import { invoke } from "@tauri-apps/api/core";

export type QuotaAccountProvider = "cursor" | "grok-bot" | "deepseek" | "qoder" | "claude" | "zcode" | "mimo" | "trae";

export interface QuotaAccount {
  id: number;
  provider: QuotaAccountProvider;
  /** 邮箱（Cursor 系）或掩码后的 API Key / 令牌（DeepSeek / Claude / ZCode）。 */
  label: string | null;
  /** 用户自定义短标签；看板区分多账号。 */
  tag: string | null;
  createdAt: number;
}

export interface DeepSeekBalance {
  currency: string;
  total: number;
  granted: number;
  toppedUp: number;
}

export interface DeepSeekQuotaDto {
  id: number;
  ok: boolean;
  message: string | null;
  label: string | null;
  isAvailable: boolean | null;
  balances: DeepSeekBalance[];
  /** Key 被拒：卡片给出「更换 Key」。 */
  needsReauth: boolean;
  at: number;
}

export interface ClaudeWindow {
  label: string;
  /** 已用百分比 0–100。 */
  usedPercent: number;
  /** 重置时刻（unix 秒）。 */
  resetsAt: number | null;
}

export interface ClaudeQuotaDto {
  id: number;
  ok: boolean;
  message: string | null;
  label: string | null;
  plan: string | null;
  windows: ClaudeWindow[];
  needsReauth: boolean;
  at: number;
}

export interface ZcodeLimit {
  label: string;
  usage: number;
  limit: number;
  unit: string | null;
}

export interface ZcodeQuotaDto {
  id: number;
  ok: boolean;
  message: string | null;
  label: string | null;
  limits: ZcodeLimit[];
  needsReauth: boolean;
  at: number;
}

export const quotaAccountsList = () => invoke<QuotaAccount[]>("quota_accounts_list");
export const quotaAccountDelete = (id: number) => invoke<void>("quota_account_delete", { id });
export const quotaAccountSetTag = (id: number, tag: string | null) =>
  invoke<void>("quota_account_set_tag", { id, tag });

/** 打开 Cursor 登录页并轮询结果（最长 5 分钟）；Grok Bot 用同一套 Cursor 账号体系。 */
export const cursorOauthStart = (provider: "cursor" | "grok-bot") =>
  invoke<QuotaAccount>("cursor_oauth_start", { provider });
export const cursorOauthCancel = () => invoke<void>("cursor_oauth_cancel");

/** 先查一次余额验证 Key，通过才入库。 */
export const deepseekAddKey = (apiKey: string) => invoke<QuotaAccount>("deepseek_add_key", { apiKey });
export const deepseekBalanceFetch = (id: number) => invoke<DeepSeekQuotaDto>("deepseek_balance_fetch", { id });

/** 从本机 Claude Code 凭证导入；验证用量接口通过才入库。 */
export const claudeImportLocal = () => invoke<QuotaAccount>("claude_import_local");
/** 粘贴 OAuth access token（可选 refresh）；验证通过才入库。 */
export const claudeAddToken = (accessToken: string, refreshToken?: string) =>
  invoke<QuotaAccount>("claude_add_token", { accessToken, refreshToken: refreshToken ?? null });
export const claudeQuotaFetch = (id: number) => invoke<ClaudeQuotaDto>("claude_quota_fetch", { id });

/** 智谱 BigModel API Key；先查配额验证通过才入库。 */
export const zcodeAddKey = (apiKey: string) => invoke<QuotaAccount>("zcode_add_key", { apiKey });
export const zcodeQuotaFetch = (id: number) => invoke<ZcodeQuotaDto>("zcode_quota_fetch", { id });

export type MimoMode = "payg" | "token_plan";

export interface MimoBalance {
  label: string;
  amount: number;
  currency: string;
}

export interface MimoWindow {
  label: string;
  used: number;
  limit: number;
}

export interface MimoQuotaDto {
  id: number;
  ok: boolean;
  message: string | null;
  label: string | null;
  plan: string | null;
  mode: MimoMode | null;
  balances: MimoBalance[];
  windows: MimoWindow[];
  /** 套餐周期结束（epoch 毫秒）。 */
  periodEndMs: number | null;
  /** 上游标记套餐已过期。 */
  expired: boolean;
  /** 已绑定开放平台 Cookie，可查 Token Plan 用量。 */
  hasSession: boolean;
  needsReauth: boolean;
  at: number;
}

/** 小米 MiMo API Key（sk- 按量 / tp- Token Plan）；先查额度验证通过才入库。 */
export const mimoAddKey = (apiKey: string) => invoke<QuotaAccount>("mimo_add_key", { apiKey });
export const mimoQuotaFetch = (id: number) => invoke<MimoQuotaDto>("mimo_quota_fetch", { id });
/** 粘贴开放平台 Cookie，验证用量后绑定到账号。 */
export const mimoSetCookie = (id: number, cookie: string) =>
  invoke<MimoQuotaDto>("mimo_set_cookie", { id, cookie });
export const mimoOpenLoginWindow = () => invoke<string>("mimo_open_login_window");
export const mimoCloseLoginWindow = () => invoke<void>("mimo_close_login_window");

export interface MimoLoginResult {
  account: QuotaAccount;
  quota: MimoQuotaDto;
}

/**
 * 轮询登录窗：登好了就入库、关窗并返回账号与额度；还没登好返回 null。
 * `id` 省略时按小米 ID 新建扫码账号卡（不需要 API Key）。
 */
export const mimoPollLogin = (id?: number) =>
  invoke<MimoLoginResult | null>("mimo_poll_login", { id: id ?? null });

/** 扫码账号的 label 是「小米 ID …」，Key 账号是掩码后的 sk- / tp- / ttp-。 */
export const isMimoSessionAccount = (label: string | null | undefined) => !/^(sk|t?tp)-/i.test(label ?? "");

export interface TraeWindow {
  plan: string;
  used: number;
  limit: number;
}

export interface TraeQuotaDto {
  id: number;
  ok: boolean;
  message: string | null;
  label: string | null;
  /** 积分余额。 */
  credits: number | null;
  windows: TraeWindow[];
  /** 今日是否已签到。 */
  checkedIn: boolean | null;
  /** 今日签到成功的本地时刻（unix 秒）；未签 / 跨天为 null。 */
  checkedInAt: number | null;
  /** 签到功能是否开放。 */
  checkinEnabled: boolean | null;
  needsReauth: boolean;
  at: number;
}

export interface TraeCheckinResult {
  id: number;
  label: string | null;
  ok: boolean;
  /** 今日已签 / 功能未开放，本轮跳过。 */
  skipped: boolean;
  points: number | null;
  credits: number | null;
  message: string | null;
  needsReauth: boolean;
}

export interface TraeSettings {
  checkinEnabled: boolean;
  checkinTime: string;
  lastAutoCheckinDate: string | null;
}

export interface TraeSettingsPatch {
  checkinEnabled?: boolean;
  checkinTime?: string;
}

/** 粘贴 Trae 客户端 refreshToken 新增账号；设备密钥可选（新版续期需要）。 */
export const traeAddRefreshToken = (
  refreshToken: string,
  keys?: { deviceId?: string; machineId?: string; deviceKeyPem?: string; devicePubPem?: string },
) =>
  invoke<QuotaAccount>("trae_add_refresh_token", {
    refreshToken,
    deviceId: keys?.deviceId ?? null,
    machineId: keys?.machineId ?? null,
    deviceKeyPem: keys?.deviceKeyPem ?? null,
    devicePubPem: keys?.devicePubPem ?? null,
  });

/** 本机 Trae 客户端设备身份（授权登录依赖它）。 */
export interface TraeDeviceIdentity {
  deviceId: string;
  publicKeyPem: string;
  machineId: string;
}
export const traeDeviceIdentity = () => invoke<TraeDeviceIdentity>("trae_device_identity");

/** 打开官网授权页，本机 17388 端口接回调，换取令牌并入库（最长 5 分钟）。 */
export const traeOauthStart = () => invoke<QuotaAccount>("trae_oauth_start");
export const traeQuotaFetch = (id: number) => invoke<TraeQuotaDto>("trae_quota_fetch", { id });
export const traeCheckinNow = (id: number) => invoke<TraeCheckinResult>("trae_checkin_now", { id });
export const traeCheckinAll = () => invoke<TraeCheckinResult[]>("trae_checkin_all");
export const traeGetSettings = () => invoke<TraeSettings>("trae_get_settings");
export const traeUpdateSettings = (patch: TraeSettingsPatch) => invoke<TraeSettings>("trae_update_settings", { patch });

export const TRAE_AUTO_CHECKIN_EVENT = "trae-auto-checkin-done";
