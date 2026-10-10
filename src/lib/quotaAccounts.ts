/**
 * 账号额度看板里授权登录得来的账号（Cursor / Grok Bot / DeepSeek / Qoder）— IPC 封装。
 * 令牌与 API Key 只存后端 SQLite，这里只拿到 id + 展示名。Codex 见 `codexQuota.ts`。
 */
import { invoke } from "@tauri-apps/api/core";

export type QuotaAccountProvider = "cursor" | "grok-bot" | "deepseek" | "qoder";

export interface QuotaAccount {
  id: number;
  provider: QuotaAccountProvider;
  /** 邮箱（Cursor 系）或掩码后的 API Key（DeepSeek）。 */
  label: string | null;
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

export const quotaAccountsList = () => invoke<QuotaAccount[]>("quota_accounts_list");
export const quotaAccountDelete = (id: number) => invoke<void>("quota_account_delete", { id });

/** 打开 Cursor 登录页并轮询结果（最长 5 分钟）；Grok Bot 用同一套 Cursor 账号体系。 */
export const cursorOauthStart = (provider: "cursor" | "grok-bot") =>
  invoke<QuotaAccount>("cursor_oauth_start", { provider });
export const cursorOauthCancel = () => invoke<void>("cursor_oauth_cancel");

/** 先查一次余额验证 Key，通过才入库。 */
export const deepseekAddKey = (apiKey: string) => invoke<QuotaAccount>("deepseek_add_key", { apiKey });
export const deepseekBalanceFetch = (id: number) => invoke<DeepSeekQuotaDto>("deepseek_balance_fetch", { id });
