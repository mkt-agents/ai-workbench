import { useState } from "react";
import { useTranslation } from "react-i18next";
import WecomSettingsCard from "./WecomSettingsCard";
import WeixinSettingsCard from "./WeixinSettingsCard";

/**
 * 智能互联 → IM 机器人。
 * 通道标签：企业微信 / 微信。
 */
export default function ImBotPage() {
  const { t } = useTranslation("settings");
  const [channel, setChannel] = useState<"wecom" | "weixin">("wecom");

  return (
    <div className="settings-page is-solo">
      <div className="card">
        <div className="card-title is-tabs">
          <div className="hub-tabs is-channel" role="tablist" aria-label={t("imBot.title")}>
            <button
              type="button"
              role="tab"
              aria-selected={channel === "wecom"}
              className={`hub-tab${channel === "wecom" ? " is-active" : ""}`}
              onClick={() => setChannel("wecom")}
            >
              {t("imBot.channelWecom")}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={channel === "weixin"}
              className={`hub-tab${channel === "weixin" ? " is-active" : ""}`}
              onClick={() => setChannel("weixin")}
            >
              {t("imBot.channelWechat")}
            </button>
          </div>
        </div>
        <div className="card-body">
          <section className="hub-group" hidden={channel !== "wecom"}>
            <WecomSettingsCard />
          </section>
          <section className="hub-group" hidden={channel !== "weixin"}>
            <WeixinSettingsCard />
          </section>
        </div>
      </div>
    </div>
  );
}
