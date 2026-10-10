/** Cursor 额度 — IPC 封装。令牌来自看板里的授权登录（存后端），这里只拿账期用量。 */
import { invoke } from "@tauri-apps/api/core";

export interface CursorQuotaDto {
  /** quota_accounts 行 id。 */
  id: number;
  ok: boolean;
  message: string | null;
  email: string | null;
  membershipType: string | null;
  teamName: string | null;
  cycleStartMs: number | null;
  cycleEndMs: number | null;
  totalSpend: number | null;
  includedSpend: number | null;
  remaining: number | null;
  limit: number | null;
  /** "Cursor Models" 池已用百分比（含 Cursor Grok 与 Composer）。 */
  autoPercentUsed: number | null;
  /** "Other Models" 池已用百分比。 */
  apiPercentUsed: number | null;
  /** 总已用百分比，Cursor 官方主读数。 */
  totalPercentUsed: number | null;
  /** 套餐外赠送用量（美分）；totalSpend = includedSpend + bonusSpend。 */
  bonusSpend: number | null;
  /** 按量付费（On-Demand）预算按团队（team）还是按人（user）算；不代表席位类型。 */
  limitType: string | null;
  /** 团队级按量预算已用 / 上限（美分）。 */
  pooledUsed: number | null;
  pooledLimit: number | null;
  /** 个人按量预算已用 / 上限（美分）。 */
  individualUsed: number | null;
  individualLimit: number | null;
  displayMessage: string | null;
  /** 令牌失效：卡片给出「重新授权」。 */
  needsReauth: boolean;
  at: number;
}

export const cursorQuotaFetch = (id: number) => invoke<CursorQuotaDto>("cursor_quota_fetch", { id });
