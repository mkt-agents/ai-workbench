import { Activity, ArrowUpRight, RotateCw, Server } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { WecomBotProfile } from "../core/types";
import type { WecomBotRuntimeStatus, WecomStatus } from "../lib/wecomChannel";
import { wecomBotLabel } from "../lib/wecomBots";
import WecomDiagnosticsPage from "./WecomDiagnosticsPage";
import {
  fmtWecomDuration,
  maskWecomBotId,
  reportedCount,
  wecomStatusClass,
} from "./wecomBotView";

export type WecomChannelPageProps = {
  /** 已接入的机器人（含停用的，停用那台只有配置没有运行态） */
  bots: WecomBotProfile[];
  runtimeByBotId: Record<string, WecomBotRuntimeStatus | undefined>;
  status: WecomStatus | null;
  busy: boolean;
  onRestart: () => void;
  /** 点对照表里的一行 → 切到这台机器人的配置页 */
  onOpenBot: (id: string) => void;
};

/**
 * 通道汇总（全部机器人）。
 *
 * 和单台配置彻底分开：这里只出现「所有机器人合起来才成立」的数字——
 * 在线数、并发合计、共享任务合计；最近快照与日志都在运行诊断 tab 里。
 * 单台信息一律回左栏点机器人看，避免再把汇总当成本机配置。
 */
export default function WecomChannelPage({
  bots,
  runtimeByBotId,
  status,
  busy,
  onRestart,
  onOpenBot,
}: WecomChannelPageProps) {
  const { t } = useTranslation("settings");
  /** 汇总页的两块内容：概览（KPI + 对照表）与运行诊断（全通道，含最近快照） */
  const [tab, setTab] = useState<"overview" | "diag">("overview");
  const notReported = t("wecom.diag.notReported");
  const online = status?.botsOnline ?? 0;
  const total = status?.botsTotal ?? bots.filter((bot) => bot.enabled).length;

  const kpis = [
    {
      id: "online",
      label: t("wecom.kpi.online"),
      value: `${online} / ${total}`,
      accent: online > 0,
    },
    {
      id: "active",
      label: t("wecom.kpi.active"),
      value: reportedCount(status?.activeJobs, notReported),
    },
    {
      id: "max",
      label: t("wecom.kpi.max"),
      value: reportedCount(status?.maxJobs, notReported),
    },
    {
      id: "waiting",
      label: t("wecom.kpi.waiting"),
      value: reportedCount(status?.waitingJobs, notReported),
    },
    {
      id: "shared",
      label: t("wecom.kpi.shared"),
      value: reportedCount(status?.sharedTasks, notReported),
    },
  ];

  return (
    <div className="wecom-channel-page">
      <header className="wecom-channel-head">
        <span className="wecom-block-icon" aria-hidden>
          <Server size={14} />
        </span>
        <span className="wecom-channel-title">{t("wecom.channelPage")}</span>
        <span className="wecom-global-tag">{t("wecom.scopeAll")}</span>
        <div className="wecom-channel-actions">
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={busy || bots.length === 0}
            onClick={onRestart}
          >
            <RotateCw size={13} />
            {t("wecom.diag.restartAll")}
          </button>
        </div>
        {/* 第二行：页内 tab 与一句话提示同排，头部只占两行不拖高度 */}
        <div className="wecom-channel-tabs-row">
          <div className="wecom-log-tabs wecom-channel-tabs" role="tablist" aria-label={t("wecom.channelPage")}>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "overview"}
              className={`wecom-log-tab${tab === "overview" ? " is-active" : ""}`}
              onClick={() => setTab("overview")}
            >
              <Server size={13} aria-hidden />
              {t("wecom.channelOverview")}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "diag"}
              className={`wecom-log-tab${tab === "diag" ? " is-active" : ""}`}
              onClick={() => setTab("diag")}
            >
              <Activity size={13} aria-hidden />
              {t("wecom.groupDiag")}
            </button>
          </div>
          <p className="setting-hint wecom-channel-sub">{t("wecom.channelPageHint")}</p>
        </div>
      </header>

      {/* 两个 tab 都常驻挂载、切换只藏不卸：查询结果 / 日志 / 折叠状态不因切 tab 丢失 */}
      <div className="wecom-channel-pane" hidden={tab !== "overview"}>
          <div className="wecom-kpi-grid">
        {kpis.map((kpi) => (
          <div key={kpi.id} className={`wecom-kpi${kpi.accent ? " is-accent" : ""}`}>
            <span className="wecom-kpi-value">{kpi.value}</span>
            <span className="wecom-kpi-label">{kpi.label}</span>
          </div>
        ))}
      </div>

      <section className="wecom-channel-card">
        <div className="wecom-diag-head">
          <div className="wecom-diag-head-main">
            <span className="wecom-diag-icon" aria-hidden>
              <Activity size={13} />
            </span>
            <span>{t("wecom.botTableTitle")}</span>
          </div>
          {/* 页面头部已带「全部机器人」tag，卡内不再重复 */}
        </div>
        {bots.length === 0 ? (
          <p className="setting-hint">{t("wecom.botTableEmpty")}</p>
        ) : (
          <div className="wecom-channel-table-wrap">
            <table className="wecom-channel-table">
              <thead>
                <tr>
                  <th>{t("wecom.colBot")}</th>
                  <th>{t("wecom.colState")}</th>
                  <th>{t("wecom.colEngine")}</th>
                  <th>{t("wecom.colUptime")}</th>
                  <th>{t("wecom.colLoad")}</th>
                  <th>{t("wecom.colShared")}</th>
                  <th aria-label={t("wecom.colOpen")} />
                </tr>
              </thead>
              <tbody>
                {bots.map((bot) => {
                  const runtime = bot.botId ? runtimeByBotId[bot.botId] : undefined;
                  const state = bot.enabled ? runtime?.state : undefined;
                  const stateText = !bot.enabled
                    ? t("wecom.sectionOff")
                    : runtime?.state === "connected"
                      ? t("wecom.railOnline")
                      : runtime?.state
                        ? t(`wecom.state.${runtime.state}`, { defaultValue: runtime.state })
                        : t("wecom.railWaiting");
                  const uptime = fmtWecomDuration(runtime?.connectedMs);
                  return (
                    <tr key={bot.id}>
                      <td>
                        <button
                          type="button"
                          className="wecom-channel-bot-link"
                          title={runtime?.lastError || t("wecom.colOpen")}
                          onClick={() => onOpenBot(bot.id)}
                        >
                          <span>{wecomBotLabel(bot, t("wecom.botUntitled"))}</span>
                          <span className="mono">{maskWecomBotId(bot.botId)}</span>
                        </button>
                      </td>
                      <td>
                        <span className={`wecom-status-pill ${wecomStatusClass(state)}`}>
                          <i className="wecom-status-dot" aria-hidden />
                          {stateText}
                        </span>
                      </td>
                      <td>
                        {(runtime?.engine ?? bot.engine) === "dsh"
                          ? t("wecom.engineDsh")
                          : t("wecom.engineTianji")}
                      </td>
                      <td className="tabular-nums">{uptime || notReported}</td>
                      <td className="tabular-nums">
                        {bot.enabled
                          ? `${reportedCount(runtime?.activeJobs, notReported)} / ${reportedCount(
                              runtime?.maxJobs,
                              notReported,
                            )}`
                          : notReported}
                      </td>
                      <td className="tabular-nums">
                        {bot.enabled ? reportedCount(runtime?.sharedTasks, notReported) : notReported}
                      </td>
                      <td>
                        <button
                          type="button"
                          className="wecom-rail-tool"
                          title={t("wecom.colOpen")}
                          aria-label={t("wecom.colOpen")}
                          onClick={() => onOpenBot(bot.id)}
                        >
                          <ArrowUpRight size={13} />
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </section>
      </div>
      <div className="wecom-channel-pane" hidden={tab !== "diag"}>
        <WecomDiagnosticsPage status={status} />
      </div>
    </div>
  );
}
