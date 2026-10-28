import { useTranslation } from "react-i18next";
import WecomSettingsCard from "./WecomSettingsCard";

/**
 * 智能互联 → IM 机器人。
 *
 * 通道标签在最上面（目前只有企业微信，微信占位待做），下面就是该通道的接入设置。
 * 复用设置页那套卡片与表单样式（`.settings-page.is-solo` 只保留单栏布局，
 * 所有 `.settings-page .wecom-*` 规则照旧生效）。
 */
export default function ImBotPage() {
  const { t } = useTranslation("settings");

  return (
    <div className="settings-page is-solo">
      <div className="card">
        {/* 通道标签占卡片标题位：页面大标题已经是「IM 机器人」，不重复 */}
        <div className="card-title is-tabs">
          <div className="hub-tabs is-channel" role="tablist" aria-label={t("imBot.title")}>
            <button type="button" role="tab" aria-selected className="hub-tab is-active">
              {t("imBot.channelWecom")}
            </button>
            {/* 占位：微信通道还没做，先摆一颗带「开发中」标记的禁用标签 */}
            <button
              type="button"
              role="tab"
              aria-selected={false}
              aria-disabled="true"
              disabled
              className="hub-tab is-soon"
            >
              {t("imBot.channelWechat")}
              <span className="hub-tab-soon">{t("imBot.soon")}</span>
            </button>
          </div>
        </div>
        <div className="card-body">
          <section className="hub-group">
            <WecomSettingsCard />
          </section>
        </div>
      </div>
    </div>
  );
}
