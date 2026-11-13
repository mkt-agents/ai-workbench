import { Activity, Bot, ChevronRight, Pencil, QrCode, Trash2 } from "lucide-react";
import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { WecomBotProfile } from "../core/types";
import type { WecomBotRuntimeStatus } from "../lib/wecomChannel";
import { wecomBotLabel } from "../lib/wecomBots";
import { maskWecomBotId, wecomStatusClass } from "./wecomBotView";

export type WecomBotRailProps = {
  /** 已接入（有 Bot ID）的机器人列表 */
  bots: WecomBotProfile[];
  activeId?: string;
  /** bot_id → 运行态；只包含已启用并 apply 成功的实例 */
  runtimeByBotId: Record<string, WecomBotRuntimeStatus | undefined>;
  /** bot_id → 待审批配对数量 */
  pendingByBotId: Record<string, number>;
  onlineCount: number;
  totalCount: number;
  busy: boolean;
  /** 右栏当前是否停在「通道汇总（全部机器人）」 */
  channelSelected: boolean;
  onSelectChannel: () => void;
  onSelect: (id: string) => void;
  onEdit: (bot: WecomBotProfile) => void;
  onRemove: (bot: WecomBotProfile) => void;
  onScanAdd: () => void;
  onManualAdd: () => void;
};

/**
 * 左栏：机器人列表是「配置哪一台」的唯一入口（主从布局的主）。
 *
 * 启用开关故意只放在右栏身份头：列表里点击整条是「切过去看」，
 * 不在列表上提供写操作，避免选错人时顺手把生产机器人停掉。
 */
export default function WecomBotRail({
  bots,
  activeId,
  runtimeByBotId,
  pendingByBotId,
  onlineCount,
  totalCount,
  busy,
  channelSelected,
  onSelectChannel,
  onSelect,
  onEdit,
  onRemove,
  onScanAdd,
  onManualAdd,
}: WecomBotRailProps) {
  const { t } = useTranslation("settings");
  const untitled = t("wecom.botUntitled");
  const listRef = useRef<HTMLUListElement | null>(null);

  // 窄窗口下列表横向排布：把竖向滚轮翻译成横向位移（shift+滚轮太隐蔽，触控板横扫才原生）。
  // 只在列表真的溢出时接管，不溢出时滚轮照常滚页面。
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const onWheel = (event: WheelEvent) => {
      if (event.deltaY === 0 || el.scrollWidth <= el.clientWidth + 1) return;
      event.preventDefault();
      el.scrollLeft += event.deltaY;
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [bots.length]);

  return (
    <aside className="wecom-bot-rail" aria-label={t("wecom.railTitle")}>
      {/* 头部三件套贴在一起：标题 / 在线数 / 两个接入入口。
          接入按钮原来钉在栏底，列表短时要跨过整片空白去够，移到标题下面。 */}
      <div className="wecom-rail-top">
        <div className="wecom-rail-head">
          <span className="wecom-block-icon" aria-hidden>
            <Bot size={14} />
          </span>
          <span className="wecom-rail-title">{t("wecom.railTitle")}</span>
          <span className="wecom-chip" title={t("wecom.railOnlineHint")}>
            {t("wecom.botsOnline", { online: onlineCount, total: totalCount })}
          </span>
        </div>
        <div className="wecom-rail-add">
          <button
            type="button"
            className="btn btn-primary btn-small"
            disabled={busy}
            onClick={onScanAdd}
          >
            <QrCode size={13} />
            {t("wecom.botsScanAdd")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={busy}
            onClick={onManualAdd}
          >
            {t("wecom.botsManualAdd")}
          </button>
        </div>
        {/* 看的是「全部机器人」的汇总，不是某一台：跟着接入按钮放头部，
            列表再长也永远在最上面，不用滚到栏底找 */}
        <div className="wecom-rail-nav">
          <button
            type="button"
            className={`wecom-rail-nav-btn${channelSelected ? " is-selected" : ""}`}
            aria-current={channelSelected ? "page" : undefined}
            onClick={onSelectChannel}
          >
            <Activity size={13} aria-hidden />
            <span className="wecom-rail-nav-label">{t("wecom.channelPage")}</span>
            <span className="wecom-rail-nav-tag">{t("wecom.scopeAll")}</span>
            <ChevronRight size={12} className="wecom-rail-nav-arrow" aria-hidden />
          </button>
        </div>
      </div>

      {bots.length === 0 ? (
        <p className="setting-hint wecom-rail-empty">{t("wecom.railEmpty")}</p>
      ) : (
        <ul className="wecom-rail-list" ref={listRef}>
          {bots.map((bot) => {
            const runtime = bot.botId ? runtimeByBotId[bot.botId] : undefined;
            const selected = activeId === bot.id;
            const pending = bot.botId ? (pendingByBotId[bot.botId] ?? 0) : 0;
            const state = bot.enabled ? runtime?.state : undefined;
            const stateText = !bot.enabled
              ? t("wecom.sectionOff")
              : runtime?.state === "connected"
                ? t("wecom.railOnline")
                : runtime?.state
                  ? t(`wecom.state.${runtime.state}`, { defaultValue: runtime.state })
                  : t("wecom.railWaiting");
            return (
              <li
                key={bot.id}
                className={`wecom-rail-item${selected ? " is-selected" : ""}${
                  bot.enabled ? "" : " is-off"
                }`}
              >
                <button
                  type="button"
                  className="wecom-rail-item-btn"
                  aria-current={selected ? "true" : undefined}
                  onClick={() => onSelect(bot.id)}
                >
                  <span className="wecom-rail-item-row">
                    <i
                      className={`wecom-status-dot ${wecomStatusClass(state)}`}
                      aria-hidden
                    />
                    <span className="wecom-rail-item-name">{wecomBotLabel(bot, untitled)}</span>
                    {pending > 0 ? (
                      <span
                        className="wecom-rail-item-badge"
                        title={t("wecom.railPending", { count: pending })}
                      >
                        {pending}
                      </span>
                    ) : null}
                  </span>
                  <span className="wecom-rail-item-meta">
                    <span className="mono">{maskWecomBotId(bot.botId)}</span>
                    <span className={state ? `is-${state}` : undefined}>{stateText}</span>
                  </span>
                  <span className="wecom-rail-item-stats">
                    {bot.enabled
                      ? t("wecom.railJobs", {
                          active: runtime?.activeJobs ?? 0,
                          max: runtime?.maxJobs ?? 0,
                          shared: runtime?.sharedTasks ?? 0,
                        })
                      : t("wecom.railJobsOff")}
                  </span>
                </button>
                <span className="wecom-rail-item-tools">
                  <button
                    type="button"
                    className="wecom-rail-tool"
                    title={t("wecom.botsEdit")}
                    aria-label={t("wecom.botsEdit")}
                    disabled={busy}
                    onClick={() => onEdit(bot)}
                  >
                    <Pencil size={12} />
                  </button>
                  <button
                    type="button"
                    className="wecom-rail-tool is-danger"
                    title={t("wecom.botsRemove")}
                    aria-label={t("wecom.botsRemove")}
                    disabled={busy}
                    onClick={() => onRemove(bot)}
                  >
                    <Trash2 size={12} />
                  </button>
                </span>
              </li>
            );
          })}
        </ul>
      )}
    </aside>
  );
}
