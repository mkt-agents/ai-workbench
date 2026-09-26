import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { KeyRound, Activity, Network, Plus, ScanLine, Settings2, Users } from "lucide-react";
import AccountPoolTable from "./workbuddy/AccountPoolTable";
import ApiKeyTable from "./workbuddy/ApiKeyTable";
import CheckinPanel from "./workbuddy/CheckinPanel";
import GatewayLogPanel from "./workbuddy/GatewayLogPanel";
import GatewayPanel from "./workbuddy/GatewayPanel";
import GrowthPanel from "./workbuddy/GrowthPanel";
import IpControlPanel from "./workbuddy/IpControlPanel";
import ManualAddModal from "./workbuddy/ManualAddModal";
import ProtocolPanel from "./workbuddy/ProtocolPanel";
import ScanAddFlow from "./workbuddy/ScanAddFlow";
import UsageStatsPanel from "./workbuddy/UsageStatsPanel";
import { wbGetSettings, wbListAccounts, type WbAccount, type WbSettings } from "../lib/workbuddy";
import { useInvokeErrorTranslator } from "../hooks/useInvokeError";
import "./WorkBuddyManager.css";

type Section = "pool" | "keys" | "ip" | "logs";

/**
 * WorkBuddy Manager — CodeBuddy 账号池控制台。
 *
 * 账号与协议都不走全局 store：凭证表刻意不进 db_load/db_save 白名单（整表覆盖
 * 写会连带冲掉凭证），所以页面自己持有列表状态，每次变更重新拉取。
 */
export default function WorkBuddyManager() {
  const { t } = useTranslation("workbuddy");
  const translateError = useInvokeErrorTranslator();

  const [accounts, setAccounts] = useState<WbAccount[]>([]);
  const [settings, setSettings] = useState<WbSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState(false);
  const [editTarget, setEditTarget] = useState<WbAccount | null>(null);
  const [scanOpen, setScanOpen] = useState(false);
  const [protocolOpen, setProtocolOpen] = useState(false);
  const [section, setSection] = useState<Section>("pool");
  /// 网关启停后自增，让日志面板跟着刷新。
  const [logPulse, setLogPulse] = useState(0);
  const [toast, setToast] = useState<{ kind: "success" | "error"; text: string } | null>(null);

  const flash = useCallback((kind: "success" | "error", text: string) => {
    setToast({ kind, text });
    setTimeout(() => setToast(null), 4000);
  }, []);

  const refresh = useCallback(async () => {
    try {
      setAccounts(await wbListAccounts());
      setLoadError(null);
    } catch (e) {
      setLoadError(t("loadFailed", { message: translateError(e) }));
    } finally {
      setLoading(false);
    }
  }, [t, translateError]);

  const refreshSettings = useCallback(async () => {
    try {
      setSettings(await wbGetSettings());
    } catch {
      /* 保留上一次可用配置即可 */
    }
  }, []);

  useEffect(() => {
    void refresh();
    void refreshSettings();
  }, [refresh, refreshSettings]);

  const protocolConfigured = useMemo(
    () => Boolean(settings?.adapterJson && settings.adapterJson.trim() && settings.adapterJson !== "{}"),
    [settings]
  );

  return (
    <div className="page-panel is-active workbuddy-page">
      <div className="page-scrollable">
        <GatewayPanel
          onChanged={() => {
            void refresh();
            setLogPulse((n) => n + 1);
          }}
        />

        <div className="wb-tabs">
          <div className="wb-tabs-nav" role="tablist">
            {(
              [
                { id: "pool", label: t("accountPool"), icon: <Users size={13} /> },
                { id: "keys", label: t("keysTitle"), icon: <KeyRound size={13} /> },
                { id: "ip", label: t("ipTitle"), icon: <Network size={13} /> },
                { id: "logs", label: t("logsTab"), icon: <Activity size={13} /> },
              ] as const
            ).map((tab) => (
              <button
                key={tab.id}
                type="button"
                role="tab"
                aria-selected={section === tab.id}
                className={`wb-tab ${section === tab.id ? "is-active" : ""}`}
                onClick={() => setSection(tab.id)}
              >
                {tab.icon}
                <span>{tab.label}</span>
              </button>
            ))}
          </div>

          {section === "pool" && (
            <div className="wb-toolbar-actions">
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setScanOpen(true)}
                title={protocolConfigured ? t("scanTooltip") : t("scanNeedsProtocol")}
              >
                <ScanLine size={14} />
                <span>{t("scanAdd")}</span>
                {!protocolConfigured && <span className="wb-dot-warn" aria-hidden />}
              </button>
              <button type="button" className="btn btn-secondary" onClick={() => setAddOpen(true)}>
                <Plus size={14} />
                <span>{t("manualAdd")}</span>
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => setProtocolOpen(true)}
                disabled={!settings}
              >
                <Settings2 size={14} />
                <span>{t("protocol")}</span>
              </button>
            </div>
          )}
        </div>

        {loadError && (
          <div className="wb-inline-error" role="alert">
            {loadError}
          </div>
        )}

        {section === "pool" && (
          <>
            <AccountPoolTable
              accounts={accounts}
              loading={loading}
              protocolConfigured={protocolConfigured}
              onChanged={refresh}
              onFlash={flash}
              onAdd={() => setAddOpen(true)}
              onEdit={setEditTarget}
            />
            <CheckinPanel
              settings={settings}
              accountCount={accounts.filter((a) => a.enabled).length}
              accounts={accounts}
              onSettingsChanged={() => void refreshSettings()}
              onAccountsChanged={refresh}
              onFlash={flash}
            />
            <GrowthPanel accounts={accounts} />
          </>
        )}
        {section === "keys" && <ApiKeyTable onChanged={refresh} />}
        {section === "ip" && (
          <IpControlPanel lanEnabled={settings?.lanEnabled ?? false} port={settings?.port ?? 8787} />
        )}
        {section === "logs" && (
          <>
            <UsageStatsPanel />
            <GatewayLogPanel refreshSignal={logPulse} />
          </>
        )}
      </div>

      {scanOpen && (
        <ScanAddFlow
          onClose={() => setScanOpen(false)}
          onSaved={async (label, status) => {
            await refresh();
            flash(
              status === "active" ? "success" : "error",
              status === "active" ? t("savedToast", { label }) : t("scanSavedUnverified", { label })
            );
          }}
          onFallBackToPaste={() => {
            setScanOpen(false);
            setAddOpen(true);
          }}
          onOpenProtocol={
            settings
              ? () => {
                  setScanOpen(false);
                  setProtocolOpen(true);
                }
              : undefined
          }
        />
      )}

      {addOpen && (
        <ManualAddModal
          onClose={() => setAddOpen(false)}
          onSaved={async (label) => {
            setAddOpen(false);
            await refresh();
            flash("success", t("savedToast", { label }));
          }}
        />
      )}

      {editTarget && (
        <ManualAddModal
          editAccount={editTarget}
          onClose={() => setEditTarget(null)}
          onSaved={async (label) => {
            setEditTarget(null);
            await refresh();
            flash("success", t("updatedToast", { label }));
          }}
        />
      )}

      {protocolOpen && settings && (
        <ProtocolPanel
          settings={settings}
          onClose={() => setProtocolOpen(false)}
          onSaved={async (next) => {
            setSettings(next);
            setProtocolOpen(false);
            flash("success", t("protocolSaved"));
          }}
        />
      )}

      {toast && (
        <div className={`toast toast-${toast.kind}`} role="status">
          <span className="toast-text">{toast.text}</span>
        </div>
      )}
    </div>
  );
}
