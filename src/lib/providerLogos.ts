/**
 * 账号额度页服务商品牌图标（本地打包，不依赖外网）。
 * 侧栏优先用本机安装包提取的图标，缺省时回落到这里。
 */
import chatgpt from "../assets/providers/chatgpt.png";
import claude from "../assets/providers/claude.png";
import cursor from "../assets/providers/cursor.png";
import deepseek from "../assets/providers/deepseek.svg";
import qoder from "../assets/providers/qoder.png";
import workbuddy from "../assets/providers/workbuddy.png";
import workbuddyAi from "../assets/providers/workbuddy-ai.png";
import antigravity from "../assets/providers/antigravity.png";
import zcode from "../assets/providers/zcode.png";
import xai from "../assets/providers/xai.png";

/** `QUOTA_PROVIDERS[].id` → 资源 URL */
const BY_PROVIDER: Record<string, string> = {
  chatgpt,
  "claude-code": claude,
  "claude-desktop": claude,
  deepseek,
  workbuddy,
  cursor,
  "grok-bot": xai,
  qoder,
  "workbuddy-ai": workbuddyAi,
  antigravity,
  zcode,
  "grok-build": xai,
};

/** 看板卡片 `brand`（`QuotaCardShell`）→ 资源 URL */
const BY_CARD_BRAND: Record<string, string> = {
  wb: workbuddy,
  workbuddy,
  cursor,
  grok: xai,
  "grok-bot": xai,
  codex: chatgpt,
  deepseek,
  qoder,
  claude,
  zcode,
};

export function providerLogoSrc(providerId: string): string | null {
  return BY_PROVIDER[providerId] ?? null;
}

export function cardBrandLogoSrc(brand: string): string | null {
  return BY_CARD_BRAND[brand] ?? null;
}
