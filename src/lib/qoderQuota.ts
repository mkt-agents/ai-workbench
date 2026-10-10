/** Qoder（国内版）额度 — IPC 封装。令牌由后端存储并自动续期，这里只拿用量。 */
import { invoke } from "@tauri-apps/api/core";
import type { QuotaAccount } from "./quotaAccounts";

export interface QoderQuota {
  total: number;
  used: number;
  remaining: number;
  /** 已用百分比 0-100。 */
  pct: number;
  unit: string;
}

/** 每日领积分活动的当前一轮。 */
export interface QoderCampaign {
  campaignId: string;
  claimable: boolean;
  amount: number;
  /** 本轮截止（epoch 秒），0 表示上游没给。 */
  endAt: number;
  validityDays: number | null;
}

export interface QoderQuotaDto {
  /** quota_accounts 行 id。 */
  id: number;
  ok: boolean;
  message: string | null;
  /** 邮箱，取不到时是昵称。 */
  label: string | null;
  userType: string | null;
  /** 套餐到期（epoch 毫秒）。 */
  expiresAtMs: number | null;
  upgradeUrl: string | null;
  /** 本期个人 Credits。 */
  user: QoderQuota | null;
  /** 加油包。 */
  addOn: QoderQuota | null;
  /** 团队共享资源包。 */
  org: QoderQuota | null;
  /** 企业版：上游只给详情页链接。 */
  enterpriseUrl: string | null;
  /** 旧缓存里没有这个字段。 */
  campaign?: QoderCampaign | null;
  needsReauth: boolean;
  at: number;
}

export interface QoderClaimResult {
  ok: boolean;
  errorCode: string | null;
  message: string | null;
  /** 被风控拒绝，需要回 Qoder 客户端领取。 */
  risk: boolean;
  needsReauth: boolean;
}

/** 打开 qoder.cn 登录页并轮询结果（最长 5 分钟）；取消与 Cursor 共用 `cursorOauthCancel`。 */
export const qoderOauthStart = () => invoke<QuotaAccount>("qoder_oauth_start");
export const qoderQuotaFetch = (id: number) => invoke<QoderQuotaDto>("qoder_quota_fetch", { id });
/** 直调一次领取接口，不重试。 */
export const qoderCampaignClaim = (id: number, campaignId: string) =>
  invoke<QoderClaimResult>("qoder_campaign_claim", { id, campaignId });
/** 启动本机 Qoder CN 客户端。 */
export const qoderLaunchApp = () => invoke<string>("qoder_launch_app");
