/**
 * Codex 额度 — IPC 封装。账号走应用内链接授权（浏览器登录 ChatGPT → 回调本机），
 * 令牌只存后端 SQLite，这里只拿到身份与用量。
 */
import { invoke } from "@tauri-apps/api/core";

export interface CodexAccount {
  id: number;
  email: string | null;
  planType: string | null;
  createdAt: number;
}

export interface CodexWindow {
  /** 已用百分比（0-100）。 */
  usedPercent: number;
  /** 窗口长度（秒）：18000 = 5 小时，604800 = 每周。 */
  windowSeconds: number | null;
  /** 重置时刻（epoch 毫秒）。 */
  resetAtMs: number | null;
}

export interface CodexQuotaDto {
  id: number;
  ok: boolean;
  message: string | null;
  email: string | null;
  planType: string | null;
  primary: CodexWindow | null;
  secondary: CodexWindow | null;
  limitReached: boolean | null;
  creditsBalance: number | null;
  creditsUnlimited: boolean | null;
  /** 令牌失效且续期失败：卡片给出「重新授权」。 */
  needsReauth: boolean;
  at: number;
}

/** 打开浏览器授权页并等待回调（最长 5 分钟）；成功返回新增 / 更新的账号。 */
export const codexOauthStart = () => invoke<CodexAccount>("codex_oauth_start");
export const codexOauthCancel = () => invoke<void>("codex_oauth_cancel");
export const codexListAccounts = () => invoke<CodexAccount[]>("codex_list_accounts");
export const codexDeleteAccount = (id: number) => invoke<void>("codex_delete_account", { id });
export const codexQuotaFetch = (id: number) => invoke<CodexQuotaDto>("codex_quota_fetch", { id });
