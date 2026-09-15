import { useEffect, useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { Check, XCircle, Loader2 } from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../core/store";
import type { AppTheme } from "../core/types";
import {
  APP_RELEASES_URL,
  checkForUpdates,
} from "../lib/version";
import { registerQuickAskShortcut, DEFAULT_SHORTCUT } from "../lib/quickAskShortcut";
import { applyDocumentTheme, APP_THEME_CHANGED_EVENT } from "../lib/theme";
import { useConfirm } from "./ConfirmModal";
import { emit } from "@tauri-apps/api/event";

const THEME_OPTIONS: AppTheme[] = ["ice", "silver", "glass", "light", "dark", "system"];

function Settings() {
  const { t, i18n } = useTranslation("settings");
  const confirm = useConfirm();

  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const invokeSetAutoStart = useGlobalStore((s) => s.invokeSetAutoStart);
  const invokeGetAutoStart = useGlobalStore((s) => s.invokeGetAutoStart);
  const invokeExportData = useGlobalStore((s) => s.invokeExportData);
  const invokeImportData = useGlobalStore((s) => s.invokeImportData);

  const [appVersion, setAppVersion] = useState("…");
  const [checking, setChecking] = useState(false);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [updateUrl, setUpdateUrl] = useState<string | null>(null);
  const [shortcutDraft, setShortcutDraft] = useState(settings.quickAskShortcut || DEFAULT_SHORTCUT);
  const [dataBusy, setDataBusy] = useState(false);

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 5000);
  }, []);

  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion("0.1.0"));
  }, []);

  useEffect(() => {
    setShortcutDraft(settings.quickAskShortcut || DEFAULT_SHORTCUT);
  }, [settings.quickAskShortcut]);

  useEffect(() => {
    invokeGetAutoStart()
      .then((enabled) => {
        if (enabled !== settings.autoStart) {
          setSettings({ autoStart: enabled });
        }
      })
      .catch((e) => {
        showMsg("error", t("general.autoStartReadFailed", { error: String(e) }));
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only on mount
  }, []);

  const handleAutoStartChange = async (enabled: boolean) => {
    setSettings({ autoStart: enabled });
    try {
      await invokeSetAutoStart(enabled);
    } catch (e) {
      setSettings({ autoStart: !enabled });
      showMsg("error", t("general.autoStartUpdateFailed", { error: String(e) }));
    }
  };

  const handleThemeChange = (theme: AppTheme) => {
    setSettings({ theme });
    applyDocumentTheme(theme);
    void emit(APP_THEME_CHANGED_EVENT, theme);
  };

  const handleLanguageChange = (lang: "zh-CN" | "en-US") => {
    setSettings({ language: lang });
    i18n.changeLanguage(lang);
  };

  const handleSidebarToggle = () => {
    setSettings({ sidebarCollapsed: !settings.sidebarCollapsed });
  };

  const handleApplyShortcut = async () => {
    const result = await registerQuickAskShortcut(shortcutDraft);
    if (result.ok) {
      setSettings({ quickAskShortcut: shortcutDraft.trim() || DEFAULT_SHORTCUT });
      showMsg("success", t("quickAsk.shortcutOk", { shortcut: shortcutDraft }));
    } else {
      showMsg("error", t("quickAsk.shortcutFail", { error: result.error || "unknown" }));
    }
  };

  const handleExport = async () => {
    setDataBusy(true);
    try {
      const path = await invokeExportData();
      showMsg("success", t("data.exportOk", { path }));
    } catch (e) {
      const msg = String(e);
      if (!msg.includes("取消")) showMsg("error", t("data.failed", { error: msg }));
    } finally {
      setDataBusy(false);
    }
  };

  const handleImport = async () => {
    const ok = await confirm({
      title: t("data.import"),
      message: `${t("data.importWarn")}\n\n${t("data.confirmImport")}`,
    });
    if (!ok) return;
    setDataBusy(true);
    try {
      const path = await invokeImportData();
      showMsg("success", t("data.importOk", { path }));
      await useGlobalStore.getState().initialize();
    } catch (e) {
      const msg = String(e);
      if (!msg.includes("取消")) showMsg("error", t("data.failed", { error: msg }));
    } finally {
      setDataBusy(false);
    }
  };

  const handleCheckUpdate = async () => {
    setChecking(true);
    setUpdateUrl(null);
    try {
      const local = appVersion === "…" ? await getVersion() : appVersion;
      const result = await checkForUpdates(local);
      if (result.status === "upToDate") {
        showMsg("success", t("about.upToDate", { version: result.local }));
      } else if (result.status === "updateAvailable") {
        setUpdateUrl(result.htmlUrl);
        showMsg(
          "success",
          t("about.updateAvailable", { remote: result.remote, local: result.local })
        );
      } else {
        showMsg("error", t("about.checkFailed", { error: result.message }));
      }
    } catch (e) {
      showMsg("error", t("about.checkFailed", { error: String(e) }));
    } finally {
      setChecking(false);
    }
  };

  const themeLabelKey = (theme: AppTheme) =>
    `general.theme${theme.charAt(0).toUpperCase() + theme.slice(1)}`;

  return (
    <div className="settings-page">
      <div className="card">
        <div className="card-title">{t("general.title")}</div>
        <div className="card-body">
          <div className="setting-row">
            <span className="setting-label">{t("general.language")}</span>
            <select
              className="input-field"
              style={{ width: 140 }}
              value={settings.language}
              onChange={(e) => handleLanguageChange(e.target.value as "zh-CN" | "en-US")}
            >
              <option value="zh-CN">中文</option>
              <option value="en-US">English</option>
            </select>
          </div>
          <div className="setting-stack">
            <span className="setting-label">{t("general.theme")}</span>
            <div className="theme-selector" role="group" aria-label={t("general.theme")}>
              {THEME_OPTIONS.map((theme) => (
                <button
                  key={theme}
                  type="button"
                  className={`theme-btn ${settings.theme === theme ? "active" : ""}`}
                  onClick={() => handleThemeChange(theme)}
                >
                  <span>{t(themeLabelKey(theme))}</span>
                </button>
              ))}
            </div>
          </div>          <div className="setting-row">
            <span className="setting-label">{t("general.autoStart")}</span>
            <label className="toggle">
              <input
                type="checkbox"
                checked={settings.autoStart}
                onChange={(e) => handleAutoStartChange(e.target.checked)}
              />
              <span className="toggle-slider" />
            </label>
          </div>
        </div>
      </div>

      <div className="card">
        <div className="card-title">{t("quickAsk.title")}</div>
        <div className="card-body">
          <div className="setting-stack">
            <span className="setting-label">{t("quickAsk.shortcut")}</span>
            <div className="setting-stack-controls">
              <div className="setting-inline-controls">
                <input
                  className="input-field setting-shortcut-input"
                  value={shortcutDraft}
                  onChange={(e) => setShortcutDraft(e.target.value)}
                />
                <button type="button" className="btn btn-secondary btn-small" onClick={() => void handleApplyShortcut()}>
                  {t("quickAsk.shortcutApply")}
                </button>
              </div>
              <span className="setting-hint">{t("quickAsk.shortcutHint")}</span>
            </div>
          </div>
          <div className="setting-row">
            <span className="setting-label">{t("quickAsk.pasteClipboard")}</span>
            <label className="toggle">
              <input
                type="checkbox"
                checked={settings.quickAskPasteClipboard !== false}
                onChange={(e) => setSettings({ quickAskPasteClipboard: e.target.checked })}
              />
              <span className="toggle-slider" />
            </label>
          </div>
          <div className="setting-row">
            <span className="setting-label">{t("quickAsk.bubble")}</span>
            <label className="toggle">
              <input
                type="checkbox"
                checked={settings.quickAskBubbleEnabled !== false}
                onChange={(e) => {
                  const on = e.target.checked;
                  const pos = settings.quickAskBubblePos;
                  // Legacy HiDPI bug saved top-band coords; let Rust use default instead.
                  const unsafe = pos && Number.isFinite(pos.y) && pos.y < 120;
                  setSettings(
                    unsafe
                      ? { quickAskBubbleEnabled: on, quickAskBubblePos: undefined }
                      : { quickAskBubbleEnabled: on }
                  );
                  void invoke("set_quick_ask_bubble_visible", {
                    visible: on,
                    x: unsafe ? null : (pos?.x ?? null),
                    y: unsafe ? null : (pos?.y ?? null),
                  }).catch(() => {});
                }}
              />
              <span className="toggle-slider" />
            </label>
          </div>
          <p className="setting-hint">{t("quickAsk.bubbleHint")}</p>
        </div>
      </div>

      <div className="card">
        <div className="card-title">{t("data.title")}</div>
        <div className="card-body">
          <p className="setting-hint">{t("data.exportWarn")}</p>
          <p className="setting-hint">{t("data.importWarn")}</p>
          <div className="setting-inline-controls">
            <button type="button" className="btn btn-secondary" disabled={dataBusy} onClick={() => void handleExport()}>
              {t("data.export")}
            </button>
            <button type="button" className="btn btn-secondary" disabled={dataBusy} onClick={() => void handleImport()}>
              {t("data.import")}
            </button>
          </div>
        </div>
      </div>

      <div className="card">
        <div className="card-title">{t("appearance.title")}</div>
        <div className="card-body">
          <div className="setting-row">
            <span className="setting-label">{t("appearance.sidebarCollapsed")}</span>
            <label className="toggle">
              <input
                type="checkbox"
                checked={settings.sidebarCollapsed}
                onChange={handleSidebarToggle}
              />
              <span className="toggle-slider" />
            </label>
          </div>
        </div>
      </div>

      <div className="card settings-card-span">
        <div className="card-title">{t("about.title")}</div>
        <div className="card-body about-card-body">
          <div className="setting-row">
            <span className="setting-label">{t("about.version")}</span>
            <span className="setting-value">{appVersion}</span>
          </div>
          <div className="setting-row">
            <span className="setting-label">{t("about.license")}</span>
            <span className="setting-value">MIT</span>
          </div>
          <p className="setting-hint">{t("about.privacyNote")}</p>
          <div className="setting-inline-controls">
            <button
              className="btn btn-secondary btn-small"
              disabled={checking}
              onClick={handleCheckUpdate}
            >
              {checking ? (
                <>
                  <Loader2 size={12} className="spin" /> {t("about.checking")}
                </>
              ) : (
                t("about.checkUpdate")
              )}
            </button>
            <button
              className="btn btn-secondary btn-small"
              onClick={() => {
                const url = updateUrl || APP_RELEASES_URL;
                // window.open is silently swallowed in the Tauri webview — open the
                // URL in the system browser via our http(s)-validated command instead.
                invoke("open_in_browser", { url }).catch(() => window.open(url, "_blank"));
              }}
            >
              {updateUrl ? t("about.openReleases") : t("about.github")}
            </button>
          </div>
        </div>
      </div>

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <XCircle size={16} />}
          </span>
          <span className="toast-text">{message.text}</span>
        </div>
      )}
    </div>
  );
}

export default Settings;
