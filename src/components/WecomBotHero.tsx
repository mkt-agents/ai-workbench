import { Pencil, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { WecomBotProfile } from "../core/types";
import type { WecomBotRuntimeStatus } from "../lib/wecomChannel";
import { wecomBotLabel } from "../lib/wecomBots";
import { fmtWecomDuration, wecomStatusClass } from "./wecomBotView";

export type WecomBotHeroProps = {
  bot: WecomBotProfile;
  /** 这台机器人的运行态；停用的机器人没有运行态 */
  runtime?: WecomBotRuntimeStatus;
  /**
   * 只有一台机器人且后端没给运行态时，用状态接口的顶层字段兜底。
   * 多机器人下顶层字段是「第一个在线实例」，绝不能拿来代表某一台。
   */
  fallbackState?: string;
  busy: boolean;
  onToggleEnabled: (enabled: boolean) => void;
  onEdit: () => void;
  onRemove: () => void;
};

/**
 * 右栏身份头：被选中那一台机器人的连接身份 + 作用域声明。
 * 这里是唯一能改「启用」的地方，页签里的表单全部只作用于这台机器人。
 */
export default function WecomBotHero({
  bot,
  runtime,
  fallbackState,
  busy,
  onToggleEnabled,
  onEdit,
  onRemove,
}: WecomBotHeroProps) {
  const { t } = useTranslation("settings");
  const state = bot.enabled ? (runtime?.state ?? fallbackState) : undefined;
  const stateLabel = bot.enabled
    ? t(`wecom.state.${state ?? "disconnected"}`, { defaultValue: state ?? "disconnected" })
    : t("wecom.sectionOff");
  const engine = runtime?.engine ?? bot.engine;
  const dshDetail = runtime?.dsh?.detail;
  const connectedMs = bot.enabled ? runtime?.connectedMs : null;

  return (
    <header className="wecom-bot-hero">
      <div className="wecom-bot-hero-main">
        <span className="wecom-bot-hero-name">
          {wecomBotLabel(bot, t("wecom.botUntitled"))}
        </span>
        <span
          className={`wecom-status-pill ${bot.enabled ? wecomStatusClass(state) : ""}`}
          title={runtime?.detail || undefined}
        >
          <i className="wecom-status-dot" aria-hidden />
          {stateLabel}
        </span>
        <span className="wecom-chip" title={dshDetail || undefined}>
          {engine === "dsh" ? t("wecom.engineDsh") : t("wecom.engineTianji")}
        </span>
        {connectedMs != null ? (
          <span className="wecom-chip">
            {t("wecom.diag.connectedFor", { duration: fmtWecomDuration(connectedMs) })}
          </span>
        ) : null}
        <span className="wecom-scope-tag">{t("wecom.scopeOnly")}</span>
      </div>

      <div className="wecom-bot-hero-actions">
        <span className="wecom-bot-hero-switch">
          <span className="wecom-bot-hero-switch-label">{t("wecom.botEnabled")}</span>
          <label className="toggle" title={t("wecom.botEnabledHint")}>
            <input
              type="checkbox"
              checked={Boolean(bot.enabled)}
              disabled={busy}
              onChange={(e) => onToggleEnabled(e.target.checked)}
            />
            <span className="toggle-slider" />
          </label>
        </span>
        <button type="button" className="btn btn-secondary btn-small" disabled={busy} onClick={onEdit}>
          <Pencil size={13} />
          {t("wecom.botEditCreds")}
        </button>
        <button
          type="button"
          className="btn btn-danger btn-small"
          disabled={busy}
          onClick={onRemove}
        >
          <Trash2 size={13} />
          {t("wecom.botRemove")}
        </button>
      </div>

      {bot.enabled && runtime?.lastError ? (
        <p className="wecom-hero-error" title={runtime.lastError}>
          {t("wecom.diag.lastError", { error: runtime.lastError })}
        </p>
      ) : null}
      {bot.enabled ? null : (
        <p className="setting-hint wecom-bot-hero-hint">{t("wecom.botDisabledHint")}</p>
      )}

      <details className="wecom-hint wecom-hero-hint">
        <summary>
          <span>{t("wecom.hintShort")}</span>
          <span className="wecom-hint-more">{t("wecom.hintMore")}</span>
        </summary>
        <p className="setting-hint">{t("wecom.hint")}</p>
        <p className="setting-hint">{t("wecom.diag.jobsHint")}</p>
        {runtime?.detail ? <p className="setting-hint">{runtime.detail}</p> : null}
      </details>
    </header>
  );
}
