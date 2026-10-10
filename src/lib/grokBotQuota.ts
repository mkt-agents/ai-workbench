/** Grok Bot 额度 — IPC 封装。令牌来自看板里的授权登录（Cursor 账号体系，存后端），这里只拿用量。 */
import { invoke } from "@tauri-apps/api/core";

export interface GrokQuotaDto {
  /** quota_accounts 行 id。 */
  id: number;
  ok: boolean;
  message: string | null;
  /** 授权时取到的账号邮箱；取不到时为 null。 */
  email: string | null;
  grokPlanLabel: string | null;
  cursorPlanName: string | null;
  /** 已用百分比 0-100。 */
  usagePercent: number | null;
  /** 下次重置时间（epoch 毫秒）；未排程时为 null。 */
  nextResetMs: number | null;
  periodStartMs: number | null;
  hasAvailableUsage: boolean | null;
  isTeamSeat: boolean | null;
  /** 令牌失效：卡片给出「重新授权」。 */
  needsReauth: boolean;
  at: number;
}

export const grokBotQuotaFetch = (id: number) => invoke<GrokQuotaDto>("grok_bot_quota_fetch", { id });
