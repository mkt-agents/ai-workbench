import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Plus, XCircle } from "lucide-react";
import { useGlobalStore } from "../core/store";
import TianjiApiSettingsCard from "./TianjiApiSettingsCard";

/** MCP 服务名会进工具名（`mcp_<name>_<tool>`）：只留字母数字与下划线。 */
function parseServerName(raw: string): string {
  return raw.trim().toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "");
}

/** "docs.rs, *.anthropic.com\nhttps://x.dev/a" → 去重、去协议、只留主机名。 */
function parseDomainList(raw: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const part of raw.split(/[\s,;]+/)) {
    const host = part.trim().toLowerCase().replace(/^https?:\/\//, "").split("/")[0];
    if (!host || seen.has(host)) continue;
    seen.add(host);
    out.push(host);
  }
  return out;
}

/**
 * 联网扩展面板（挂在「天玑助手」页的第二个 Tab 里）。
 *
 * 从设置页整块搬出来的：网页抓取 / 联网搜索 / MCP 服务 / 天玑 OpenAI 兼容端点。
 * 本身不套卡片，外层由 `TianjiAssistantPage` 提供。
 */
export default function NetExtensionPanel() {
  const { t } = useTranslation("settings");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const mcpServers = useGlobalStore((s) => s.mcpServers);
  const addMcpServer = useGlobalStore((s) => s.addMcpServer);
  const updateMcpServer = useGlobalStore((s) => s.updateMcpServer);
  const deleteMcpServer = useGlobalStore((s) => s.deleteMcpServer);

  const [netGroup, setNetGroup] = useState<"web" | "mcp" | "tianjiApi">("web");
  const [mcpDraft, setMcpDraft] = useState({ name: "", command: "", args: "" });
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 5000);
  }, []);

  // 抓取白名单：输入期间不被外部改动覆盖
  const [webAllowDraft, setWebAllowDraft] = useState((settings.agentWebAllow ?? []).join(", "));
  const webAllowFocused = useRef(false);
  const savedWebAllow = (settings.agentWebAllow ?? []).join(", ");
  useEffect(() => {
    if (!webAllowFocused.current) setWebAllowDraft(savedWebAllow);
  }, [savedWebAllow]);

  /** Add the draft MCP server; the name is what shows up inside every tool name. */
  const addServer = useCallback(async () => {
    const name = parseServerName(mcpDraft.name);
    const command = mcpDraft.command.trim();
    if (!name || !command) return;
    try {
      await addMcpServer({ name, command, args: mcpDraft.args.trim(), enabled: true });
      setMcpDraft({ name: "", command: "", args: "" });
      showMsg("success", t("aiHub.mcpAdded", { name }));
    } catch (e) {
      showMsg("error", String(e));
    }
  }, [mcpDraft, addMcpServer, showMsg, t]);

  return (
    <>
          <div className="hub-tabs" role="tablist" aria-label={t("net.title")}>
            {(
              [
                ["web", "tabs.groupWeb"],
                ["mcp", "tabs.groupMcp"],
                ["tianjiApi", "tabs.groupTianjiApi"],
              ] as const
            ).map(([id, labelKey]) => (
              <button
                key={id}
                type="button"
                role="tab"
                aria-selected={netGroup === id}
                className={`hub-tab${netGroup === id ? " is-active" : ""}`}
                onClick={() => setNetGroup(id)}
              >
                {t(labelKey)}
              </button>
            ))}
          </div>

          <section className="hub-group" hidden={netGroup !== "web"}>
            <div className="net-panel">
              <div className="hub-group-title">{t("net.groupFetch")}</div>
              <div className="net-grid is-single">
                <div className="net-field is-compact">
                  <span className="setting-label">{t("aiHub.webTier")}</span>
                  <select
                    className="input-field"
                    value={settings.agentWebTier || "allowlist"}
                    onChange={(e) => setSettings({ agentWebTier: e.target.value })}
                  >
                    <option value="off">{t("aiHub.webTierOff")}</option>
                    <option value="allowlist">{t("aiHub.webTierAllowlist")}</option>
                    <option value="open">{t("aiHub.webTierOpen")}</option>
                  </select>
                  <p className="setting-hint">{t("aiHub.webTierHint")}</p>
                </div>
                <div className="net-field">
                  <span className="setting-label">{t("aiHub.webAllow")}</span>
                  <textarea
                    className="input-field net-textarea"
                    rows={2}
                    spellCheck={false}
                    value={webAllowDraft}
                    placeholder={t("aiHub.webAllowPlaceholder")}
                    onFocus={() => {
                      webAllowFocused.current = true;
                    }}
                    onChange={(e) => setWebAllowDraft(e.target.value)}
                    onBlur={() => {
                      webAllowFocused.current = false;
                      const next = parseDomainList(webAllowDraft);
                      if (next.join(",") !== (settings.agentWebAllow ?? []).join(",")) {
                        setSettings({ agentWebAllow: next });
                      }
                    }}
                  />
                  <p className="setting-hint">{t("aiHub.webAllowHint")}</p>
                </div>
              </div>
            </div>
            <div className="net-panel">
              <div className="hub-group-title">{t("net.groupSearch")}</div>
              <div className="net-grid is-single">
                <div className="net-field is-compact">
                  <span className="setting-label">{t("aiHub.webSearchProvider")}</span>
                  <select
                    className="input-field"
                    value={settings.agentWebSearchProvider || ""}
                    onChange={(e) => setSettings({ agentWebSearchProvider: e.target.value })}
                  >
                    <option value="">{t("aiHub.webSearchNone")}</option>
                    <option value="brave">{t("aiHub.webSearchBrave")}</option>
                    <option value="tavily">{t("aiHub.webSearchTavily")}</option>
                  </select>
                </div>
                <div className="net-field">
                  <span className="setting-label">{t("aiHub.webSearchKey")}</span>
                  <input
                    className="input-field"
                    type="password"
                    autoComplete="off"
                    disabled={!settings.agentWebSearchProvider}
                    value={settings.agentWebSearchKey || ""}
                    placeholder={t("aiHub.webSearchKeyPlaceholder")}
                    onChange={(e) => setSettings({ agentWebSearchKey: e.target.value })}
                  />
                  <p className="setting-hint">{t("aiHub.webSearchHint")}</p>
                </div>
              </div>
            </div>
          </section>

          <section className="hub-group" hidden={netGroup !== "mcp"}>
            <div className="net-panel">
              <div className="hub-group-title">{t("net.groupMcpSwitch")}</div>
              <div className="setting-item">
                <div className="setting-row">
                  <span className="setting-label">{t("aiHub.mcpEnabled")}</span>
                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={settings.agentMcpEnabled !== false}
                      onChange={(e) => setSettings({ agentMcpEnabled: e.target.checked })}
                    />
                    <span className="toggle-slider" />
                  </label>
                </div>
                <p className="setting-hint">{t("aiHub.mcpEnabledHint")}</p>
              </div>
              <div className="hub-group-title">{t("net.groupMcpServers")}</div>
              <div className="setting-item">
                <span className="setting-label">{t("aiHub.mcp")}</span>
                <div className="mcp-list">
                  {mcpServers.length === 0 && <p className="setting-hint">{t("aiHub.mcpEmpty")}</p>}
                  {mcpServers.map((server) => (
                    <div className="mcp-row" key={server.id}>
                      <label className="toggle" title={t("aiHub.mcpEnabled")}>
                        <input
                          type="checkbox"
                          checked={server.enabled}
                          onChange={(e) => void updateMcpServer(server.id, { enabled: e.target.checked })}
                        />
                        <span className="toggle-slider" />
                      </label>
                      <input
                        className="input-field mcp-name"
                        defaultValue={server.name}
                        placeholder={t("aiHub.mcpName")}
                        onBlur={(e) => void updateMcpServer(server.id, { name: parseServerName(e.target.value) })}
                      />
                      <input
                        className="input-field mcp-command"
                        defaultValue={server.command}
                        placeholder={t("aiHub.mcpCommand")}
                        onBlur={(e) => void updateMcpServer(server.id, { command: e.target.value.trim() })}
                      />
                      <input
                        className="input-field mcp-args"
                        defaultValue={server.args}
                        placeholder={t("aiHub.mcpArgs")}
                        onBlur={(e) => void updateMcpServer(server.id, { args: e.target.value })}
                      />
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        onClick={() => void deleteMcpServer(server.id)}
                      >
                        {t("aiHub.mcpDelete")}
                      </button>
                    </div>
                  ))}
                  <div className="mcp-row is-draft">
                    <Plus size={13} className="mcp-add-icon" aria-hidden />
                    <input
                      className="input-field mcp-name"
                      value={mcpDraft.name}
                      placeholder={t("aiHub.mcpName")}
                      onChange={(e) => setMcpDraft({ ...mcpDraft, name: e.target.value })}
                    />
                    <input
                      className="input-field mcp-command"
                      value={mcpDraft.command}
                      placeholder={t("aiHub.mcpCommand")}
                      onChange={(e) => setMcpDraft({ ...mcpDraft, command: e.target.value })}
                    />
                    <input
                      className="input-field mcp-args"
                      value={mcpDraft.args}
                      placeholder={t("aiHub.mcpArgs")}
                      onChange={(e) => setMcpDraft({ ...mcpDraft, args: e.target.value })}
                      onKeyDown={(e) => {
                        if (e.nativeEvent.isComposing || e.keyCode === 229) return;
                        if (e.key === "Enter") void addServer();
                      }}
                    />
                    <button
                      type="button"
                      className="btn btn-primary btn-small"
                      disabled={!mcpDraft.name.trim() || !mcpDraft.command.trim()}
                      onClick={() => void addServer()}
                    >
                      {t("aiHub.mcpAdd")}
                    </button>
                  </div>
                </div>
                <p className="setting-hint">{t("aiHub.mcpHint")}</p>
              </div>
            </div>
          </section>

          <section className="hub-group" hidden={netGroup !== "tianjiApi"}>
            <TianjiApiSettingsCard />
          </section>

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <XCircle size={16} />}
          </span>
          <span className="toast-text">{message.text}</span>
        </div>
      )}
    </>
  );
}
