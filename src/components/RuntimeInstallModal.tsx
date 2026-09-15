import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { Check, Download, Loader2, RefreshCw, Search, XCircle } from "lucide-react";
import { useGlobalStore } from "../core/store";
import ModalTitleRow from "./ModalTitleRow";
import type { InstallableVersion, RuntimeKind } from "../core/types";

type Props = {
  kind: RuntimeKind;
  onClose: () => void;
  /** Called after a successful install so the caller can refresh its list. */
  onInstalled: () => Promise<void>;
};

type InstallProgress = { stage: string; percent: number; message: string };

/** Download and install a new Node.js / JDK build into the app-managed runtimes dir. */
function RuntimeInstallModal({ kind, onClose, onInstalled }: Props) {
  const { t } = useTranslation("runtime");
  const { t: tc } = useTranslation("common");

  const invokeListInstallableRuntimes = useGlobalStore((s) => s.invokeListInstallableRuntimes);
  const invokeInstallRuntime = useGlobalStore((s) => s.invokeInstallRuntime);

  const [items, setItems] = useState<InstallableVersion[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [busyVersion, setBusyVersion] = useState<string | null>(null);
  const [progress, setProgress] = useState<InstallProgress | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setItems(await invokeListInstallableRuntimes(kind));
    } catch (e) {
      setError(String(e));
      setItems([]);
    } finally {
      setLoading(false);
    }
  }, [invokeListInstallableRuntimes, kind]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const unlisten = listen<InstallProgress>("runtime:install_progress", (event) => {
      setProgress(event.payload);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return items;
    return items.filter((i) => i.version.toLowerCase().includes(q));
  }, [items, query]);

  const handleInstall = async (item: InstallableVersion) => {
    if (busyVersion) return;
    setBusyVersion(item.version);
    setError("");
    setDone(null);
    setProgress({ stage: "download", percent: 0, message: "" });
    try {
      const installed = await invokeInstallRuntime(kind, item.version);
      setItems((prev) =>
        prev.map((v) => (v.version === item.version ? { ...v, installed: true } : v))
      );
      setDone(installed.version);
      await onInstalled();
    } catch (e) {
      setError(t("installFailed", { error: String(e) }));
    } finally {
      setBusyVersion(null);
      setProgress(null);
    }
  };

  const stageLabel = progress
    ? progress.stage === "extract"
      ? t("installExtracting")
      : t("installDownloading")
    : "";

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 520 }}>
        <ModalTitleRow
          title={t("installTitle", { name: kind === "node" ? "Node.js" : "JDK" })}
          onClose={onClose}
          disabled={busyVersion !== null}
        />

        <div className="runtime-search" style={{ marginBottom: 10 }}>
          <Search size={14} />
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("installSearch")}
            aria-label={t("installSearch")}
          />
        </div>

        {error && (
          <div className="runtime-msg error">
            <XCircle size={14} />
            <span>{error}</span>
          </div>
        )}
        {done && (
          <div className="runtime-msg success">
            <Check size={14} />
            <span>{t("installDone", { version: done })}</span>
          </div>
        )}

        {loading ? (
          <div className="runtime-empty">
            <Loader2 size={20} className="spin" />
            <span>{t("installLoading")}</span>
          </div>
        ) : items.length === 0 ? (
          <div className="runtime-empty">
            <div>{error ? t("installLoadFailed") : t("installEmpty")}</div>
          </div>
        ) : filtered.length === 0 ? (
          <div className="runtime-empty">
            <div>{t("noMatch")}</div>
          </div>
        ) : (
          <ul
            style={{
              listStyle: "none",
              margin: "10px 0 0",
              padding: 0,
              maxHeight: 320,
              overflowY: "auto",
            }}
          >
            {filtered.map((item) => (
              <li
                key={item.version}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  padding: "7px 2px",
                  borderBottom: "1px solid var(--border)",
                }}
              >
                <span className="runtime-ver">{item.version}</span>
                {item.lts && <span className="runtime-badge active">{t("lts")}</span>}
                <span style={{ flex: 1 }} />
                {item.installed ? (
                  <span className="runtime-muted">{t("installInstalled")}</span>
                ) : (
                  <button
                    type="button"
                    className="btn btn-primary btn-small"
                    disabled={busyVersion !== null}
                    onClick={() => void handleInstall(item)}
                  >
                    {busyVersion === item.version ? (
                      <Loader2 size={12} className="spin" />
                    ) : (
                      <Download size={12} />
                    )}
                    {busyVersion === item.version ? t("installRunning") : t("installAction")}
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}

        {progress && (
          <div style={{ marginTop: 12 }}>
            <div className="runtime-muted">
              {stageLabel}
              {progress.stage === "download" && progress.percent > 0
                ? ` ${progress.percent}%`
                : ""}
            </div>
            <div
              style={{
                marginTop: 6,
                height: 4,
                borderRadius: 2,
                background: "var(--border)",
                overflow: "hidden",
              }}
            >
              <div
                style={{
                  width: `${progress.stage === "download" ? progress.percent : 100}%`,
                  height: "100%",
                  background: "var(--accent)",
                  transition: "width 0.3s",
                }}
              />
            </div>
          </div>
        )}

        <div className="modal-actions">
          <button
            type="button"
            className="btn btn-secondary"
            onClick={() => void load()}
            disabled={loading || busyVersion !== null}
            title={t("refresh")}
          >
            {loading ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
            {t("refresh")}
          </button>
          <button
            type="button"
            className="btn btn-secondary"
            onClick={onClose}
            disabled={busyVersion !== null}
          >
            {tc("actions.close")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default RuntimeInstallModal;
