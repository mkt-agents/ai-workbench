/**
 * IM 机器人主从布局的展示换算：状态胶囊配色、连接时长、Bot ID 脱敏。
 * 左栏条目与右侧身份头共用这一份，避免两处判定不一致。
 */

/** 连接状态 → 胶囊配色类名（与后端 WecomBotRuntimeStatus.state 对齐）。 */
export function wecomStatusClass(state: string | undefined): string {
  if (state === "connected") return "is-connected";
  if (state === "error") return "is-error";
  if (state === "connecting" || state === "reconnecting") return "is-pending";
  return "";
}

/** 已连接时长（ms）→ 简短时长：2h3m / 5m12s / 38s。 */
export function fmtWecomDuration(ms: number | null | undefined): string {
  if (ms == null || ms < 0) return "";
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h${m}m`;
  if (m > 0) return `${m}m${sec}s`;
  return `${sec}s`;
}

/**
 * 后端上报的计数 → 展示文本：缺字段显示「未上报」，0 就是 0（不从状态推测）。
 * 左栏、身份头、通道汇总页共用。
 */
export function reportedCount(
  value: number | null | undefined,
  notReported = "—",
): string {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? String(value)
    : notReported;
}

/** abcd****wxyz；短 ID 原样返回，空值给占位符。 */
export function maskWecomBotId(botId: string | undefined): string {
  const raw = (botId ?? "").trim();
  if (!raw) return "—";
  return raw.length > 10 ? `${raw.slice(0, 4)}****${raw.slice(-4)}` : raw;
}
