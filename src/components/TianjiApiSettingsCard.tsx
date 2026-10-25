import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff } from "lucide-react";
import { useGlobalStore } from "../core/store";
import type { AppSettings } from "../core/types";
import {
  applyTianjiApi,
  fetchTianjiApiStatus,
  protectTianjiKey,
  revealTianjiKey,
  type TianjiApiStatus,
} from "../lib/tianjiApi";

function useFieldDraft(saved: string) {
  const [value, setValue] = useState(saved);
  const focused = useRef(false);
  useEffect(() => {
    if (!focused.current) setValue(saved);
  }, [saved]);
  return { value, setValue, focused };
}

function statusClass(state: string | undefined): string {
  if (state === "listening") return "is-connected";
  if (state === "error") return "is-error";
  return "";
}

export default function TianjiApiSettingsCard() {
  const { t } = useTranslation("settings");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const mcpServers = useGlobalStore((s) => s.mcpServers);

  const [status, setStatus] = useState<TianjiApiStatus | null>(null);
  const [keyDraft, setKeyDraft] = useState("");
  const [revealedKey, setRevealedKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");

  const port = useFieldDraft(String(settings.tianjiApiPort ?? 8792));
  const repoPath = useFieldDraft(settings.tianjiApiRepoPath ?? "");

  const refresh = useCallback(async () => {
    try {
      setStatus(await fetchTianjiApiStatus());
    } catch (e) {
      setMsg(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = window.setInterval(() => void refresh(), 5000);
    return () => window.clearInterval(id);
  }, [refresh]);

  const push = async (next: Partial<AppSettings> = {}, plainKey?: string) => {
    setBusy(true);
    setMsg("");
    const enabling = next.tianjiApiEnabled === true;
    try {
      let patch: Partial<AppSettings> = { ...next };
      const plain = plainKey?.trim() ?? "";
      if (plain) {
        patch = { ...patch, tianjiApiKey: await protectTianjiKey(plain) };
      }
      if (Object.keys(patch).length) setSettings(patch);
      const merged = { ...useGlobalStore.getState().settings, ...patch };
      const st = await applyTianjiApi({
        settings: merged,
        models: aiModels,
        mcpServers,
        plainKey: plain || undefined,
      });
      setStatus(st);
      if (plain) {
        setKeyDraft("");
        setRevealedKey("");
        setShowKey(false);
      }
      await refresh();
      setMsg(t("tianjiApi.saved"));
    } catch (e) {
      if (enabling) setSettings({ tianjiApiEnabled: false });
      setMsg(String(e));
    } finally {
      setBusy(false);
    }
  };

  const toggleKey = async () => {
    if (showKey) {
      setShowKey(false);
      if (keyDraft.trim() && keyDraft.trim() === revealedKey.trim()) setKeyDraft("");
      return;
    }
    if (!keyDraft.trim() && settings.tianjiApiKey) {
      try {
        const plain = await revealTianjiKey(settings.tianjiApiKey);
        setRevealedKey(plain);
        setKeyDraft(plain);
      } catch (e) {
        setMsg(String(e));
        return;
      }
    }
    setShowKey(true);
  };

  const stateKey = status?.state ?? "disabled";
  const stateLabel = t(`tianjiApi.state.${stateKey}`, { defaultValue: stateKey });
  const baseUrl = status?.baseUrl ?? `http://127.0.0.1:${settings.tianjiApiPort ?? 8792}/v1`;

  return (
    <div className="wecom-settings">
      <p className="setting-hint wecom-lead">{t("tianjiApi.hint")}</p>

      <div className="net-panel">
        <div className="hub-group-title">{t("tianjiApi.groupConnection")}</div>
        <div className="net-grid">
          <div className="net-field">
            <span className="setting-label">{t("tianjiApi.enabled")}</span>
            <label className="toggle">
              <input
                type="checkbox"
                checked={Boolean(settings.tianjiApiEnabled)}
                disabled={busy}
                onChange={(e) => void push({ tianjiApiEnabled: e.target.checked })}
              />
              <span className="toggle-slider" />
            </label>
          </div>

          <div className="net-field">
            <span className="setting-label">{t("tianjiApi.status")}</span>
            <div className="wecom-status">
              <span
                className={`wecom-status-pill ${statusClass(status?.state)}`}
                title={status?.detail || undefined}
              >
                <i className="wecom-status-dot" aria-hidden />
                {stateLabel}
              </span>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={busy || !settings.tianjiApiEnabled}
                onClick={() => void push({ tianjiApiEnabled: true })}
              >
                {t("tianjiApi.restart")}
              </button>
            </div>
          </div>

          {status?.state === "error" && status.detail ? (
            <div className="net-field net-span-2">
              <p className="setting-hint">{status.detail}</p>
            </div>
          ) : null}

          <div className="net-field net-span-2">
            <p className="setting-hint">
              {t("tianjiApi.baseUrlHint", { baseUrl, model: "tianji" })}
            </p>
          </div>

          <div className="net-field">
            <span className="setting-label">{t("tianjiApi.port")}</span>
            <input
              className="input-field"
              value={port.value}
              placeholder="8792"
              onFocus={() => {
                port.focused.current = true;
              }}
              onChange={(e) => port.setValue(e.target.value)}
              onBlur={() => {
                port.focused.current = false;
                const n = Number(port.value.trim()) || 8792;
                if (n !== (settings.tianjiApiPort ?? 8792)) void push({ tianjiApiPort: n });
              }}
            />
          </div>

          <div className="net-field">
            <span className="setting-label">{t("tianjiApi.apiKey")}</span>
            <div className="input-with-toggle">
              <input
                className="input-field"
                type={showKey ? "text" : "password"}
                value={showKey ? keyDraft || revealedKey : keyDraft}
                placeholder={
                  settings.tianjiApiKey
                    ? showKey
                      ? ""
                      : `••••••••  ${t("wecom.secretKept")}`
                    : t("tianjiApi.keyPlaceholder")
                }
                onChange={(e) => setKeyDraft(e.target.value)}
                onBlur={() => {
                  const v = keyDraft.trim();
                  if (!v || v === revealedKey.trim()) {
                    if (!showKey || v === revealedKey.trim()) setKeyDraft("");
                    return;
                  }
                  void push({}, v);
                }}
              />
              <button
                type="button"
                className="input-toggle-btn"
                title={showKey ? t("wecom.hideSecret") : t("wecom.showSecret")}
                aria-pressed={showKey}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => void toggleKey()}
              >
                {showKey ? <EyeOff size={14} /> : <Eye size={14} />}
              </button>
            </div>
          </div>

          <div className="net-field">
            <span className="setting-label">{t("wecom.permission")}</span>
            <select
              className="input-field"
              value={settings.tianjiApiPermission ?? "read"}
              onChange={(e) => void push({ tianjiApiPermission: e.target.value })}
            >
              <option value="read">{t("wecom.perm.read")}</option>
              <option value="workspace">{t("wecom.perm.workspace")}</option>
              <option value="review">{t("wecom.perm.review")}</option>
              <option value="full">{t("wecom.perm.full")}</option>
            </select>
          </div>

          <div className="net-field net-span-2">
            <span className="setting-label">{t("wecom.repoPath")}</span>
            <input
              className="input-field"
              value={repoPath.value}
              placeholder={settings.currentGitRepo || t("wecom.repoPlaceholder")}
              onFocus={() => {
                repoPath.focused.current = true;
              }}
              onChange={(e) => repoPath.setValue(e.target.value)}
              onBlur={() => {
                repoPath.focused.current = false;
                const v = repoPath.value.trim();
                if (v !== (settings.tianjiApiRepoPath ?? "")) void push({ tianjiApiRepoPath: v || undefined });
              }}
            />
          </div>
        </div>
      </div>

      {msg ? <p className="setting-hint wecom-toast">{msg}</p> : null}
    </div>
  );
}
