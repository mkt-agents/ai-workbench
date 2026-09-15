import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  Copy,
  Download,
  FolderOpen,
  FolderPlus,
  Loader2,
  RefreshCw,
  RotateCw,
  Search,
  Terminal,
  Trash2,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import RuntimeInstallModal from "./RuntimeInstallModal";
import type { RuntimeKind, RuntimeSwitchPlan, RuntimeVersion } from "../core/types";

function sourceLabel(source: string, t: (key: string) => string): string {
  switch (source) {
    case "system":
      return t("sourceSystem");
    case "nvm":
      return t("sourceNvm");
    case "fnm":
      return t("sourceFnm");
    case "volta":
      return t("sourceVolta");
    case "custom":
      return t("sourceCustom");
    case "ide":
      return t("sourceIde");
    case "sdkman":
      return t("sourceSdkman");
    case "env":
      return t("sourceEnv");
    case "path":
      return t("sourcePath");
    case "jvman":
      return t("sourceJvman");
    case "managed":
      return t("sourceManaged");
    default:
      return source;
  }
}

type CacheMap = Partial<Record<RuntimeKind, RuntimeVersion[]>>;

/** Remembered sub-tab, so returning to this page keeps the last runtime kind. */
const SUBTAB_STORAGE_KEY = "workbench-runtime-subtab";

function readStoredSubTab(): RuntimeKind {
  return localStorage.getItem(SUBTAB_STORAGE_KEY) === "jdk" ? "jdk" : "node";
}

function VersionSwitcher() {
  const { t } = useTranslation("runtime");
  const { t: tn } = useTranslation("navigation");
  const confirm = useConfirm();

  const invokeListRuntimeVersions = useGlobalStore((s) => s.invokeListRuntimeVersions);
  const invokeSwitchRuntime = useGlobalStore((s) => s.invokeSwitchRuntime);
  const invokePlanRuntimeSwitch = useGlobalStore((s) => s.invokePlanRuntimeSwitch);
  const invokeAddCustomRuntime = useGlobalStore((s) => s.invokeAddCustomRuntime);
  const invokeRemoveCustomRuntime = useGlobalStore((s) => s.invokeRemoveCustomRuntime);
  const invokePickDirectory = useGlobalStore((s) => s.invokePickDirectory);
  const invokeOpenRuntimeFolder = useGlobalStore((s) => s.invokeOpenRuntimeFolder);
  const invokeOpenRuntimeTerminal = useGlobalStore((s) => s.invokeOpenRuntimeTerminal);
  const invokeUninstallRuntime = useGlobalStore((s) => s.invokeUninstallRuntime);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeListDsh = useGlobalStore((s) => s.invokeListDsh);
  const invokeStopDsh = useGlobalStore((s) => s.invokeStopDsh);
  const invokeStartDsh = useGlobalStore((s) => s.invokeStartDsh);

  const [subTab, setSubTab] = useState<RuntimeKind>(readStoredSubTab);
  const [items, setItems] = useState<RuntimeVersion[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyPath, setBusyPath] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [dshPorts, setDshPorts] = useState<number[]>([]);
  const [restartingDsh, setRestartingDsh] = useState(false);
  const [installOpen, setInstallOpen] = useState(false);

  const cancelled = useRef(false);
  const scanSeq = useRef(0);
  const cacheRef = useRef<CacheMap>({});
  const msgTimer = useRef<number | null>(null);

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    if (msgTimer.current) window.clearTimeout(msgTimer.current);
    msgTimer.current = window.setTimeout(() => setMessage(null), 5000);
  }, []);

  const applyList = useCallback((kind: RuntimeKind, list: RuntimeVersion[]) => {
    cacheRef.current[kind] = list;
    setItems(list);
  }, []);

  const refresh = useCallback(
    async (kind: RuntimeKind = subTab) => {
      const seq = ++scanSeq.current;
      setLoading(true);
      try {
        const list = await invokeListRuntimeVersions(kind);
        if (cancelled.current || seq !== scanSeq.current) return;
        applyList(kind, list);
      } catch (e) {
        if (cancelled.current || seq !== scanSeq.current) return;
        if (!cacheRef.current[kind]) {
          setItems([]);
        }
        showMsg("error", String(e));
      } finally {
        if (!cancelled.current && seq === scanSeq.current) {
          setLoading(false);
        }
      }
    },
    [applyList, invokeListRuntimeVersions, showMsg, subTab]
  );

  useEffect(() => {
    cancelled.current = false;
    const cached = cacheRef.current[subTab];
    if (cached) {
      setItems(cached);
    } else {
      setItems([]);
    }
    setQuery("");
    refresh(subTab);
    return () => {
      cancelled.current = true;
      scanSeq.current += 1;
    };
  }, [subTab, refresh]);

  useEffect(() => {
    return () => {
      if (msgTimer.current) window.clearTimeout(msgTimer.current);
    };
  }, []);

  useEffect(() => {
    localStorage.setItem(SUBTAB_STORAGE_KEY, subTab);
  }, [subTab]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const base = !q
      ? items
      : items.filter(
          (i) =>
            i.version.toLowerCase().includes(q) ||
            i.path.toLowerCase().includes(q) ||
            i.source.toLowerCase().includes(q)
        );
    return [...base].sort((a, b) => {
      if (a.active !== b.active) return a.active ? -1 : 1;
      return 0;
    });
  }, [items, query]);

  const active = items.find((i) => i.active);
  const hasMachineConflict = items.some((i) => i.onMachinePath && !i.active);
  const showFullScan = loading && items.length === 0;

  /** A switch only affects processes started afterwards — offer to bounce the DSH
   *  instances still holding the previous runtime. */
  const offerDshRestart = async () => {
    try {
      const instances = await invokeListDsh();
      setDshPorts(
        instances.map((i) => i.port).filter((p): p is number => typeof p === "number")
      );
    } catch {
      setDshPorts([]);
    }
  };

  const handleRestartDsh = async () => {
    if (restartingDsh || dshPorts.length === 0) return;
    const ok = await confirm({
      title: t("dshRestartTitle"),
      message: t("dshRestartMessage", { count: dshPorts.length }),
      confirmText: t("dshRestartConfirm"),
      icon: "warning",
    });
    if (!ok) return;
    setRestartingDsh(true);
    try {
      for (const port of dshPorts) {
        await invokeStopDsh(port).catch(() => {});
        await invokeStartDsh(port);
      }
      showMsg("success", t("dshRestartOk", { count: dshPorts.length }));
      setDshPorts([]);
    } catch (e) {
      showMsg("error", t("dshRestartFailed", { error: String(e) }));
    } finally {
      setRestartingDsh(false);
    }
  };

  const handleOpenTerminal = async (item: RuntimeVersion) => {
    try {
      await invokeOpenRuntimeTerminal(subTab, item.binPath);
      showMsg("success", t("terminalOpened"));
    } catch (e) {
      showMsg("error", t("openTerminalFailed", { error: String(e) }));
    }
  };

  const handleSwitch = async (item: RuntimeVersion) => {
    if (item.active || busyPath) return;

    // The backend owns the elevation decision: it inspects the *order* of entries
    // in the machine PATH, which the list alone cannot express. Asking it here keeps
    // this prompt consistent with what switch_runtime will actually do.
    let plan: RuntimeSwitchPlan = { needsElevation: false };
    try {
      plan = await invokePlanRuntimeSwitch(subTab, item.path);
    } catch {
      // Fall through: let switch_runtime surface the real error instead of guessing.
    }

    const ok = plan.needsElevation
      ? await confirm({
          title: t("elevateConfirmTitle"),
          message: t("elevateConfirmMessage", {
            reason: plan.reason || t("elevateConfirmWarning"),
          }),
          warning: t("elevateConfirmWarning"),
          confirmText: t("switch"),
          icon: "warning",
        })
      : await confirm({
          title: t("switchConfirmTitle"),
          message: t("switchConfirmMessage", {
            version: subTab === "node" ? `v${item.version}` : item.version,
          }),
          confirmText: t("switch"),
          icon: "info",
        });
    if (!ok) return;

    setBusyPath(item.path);
    try {
      const result = await invokeSwitchRuntime(subTab, item.path);
      showMsg(result.verified ? "success" : "error", result.message || t("switchOk"));
      await refresh();
      // DSH runs on Node.js, so a JDK switch cannot leave it stale.
      if (result.verified && subTab === "node") {
        await offerDshRestart();
      }
    } catch (e) {
      showMsg("error", t("switchFailed", { error: String(e) }));
    } finally {
      setBusyPath(null);
    }
  };

  const handleAdd = async () => {
    try {
      const dir = await invokePickDirectory();
      if (!dir) return;
      const added = await invokeAddCustomRuntime(subTab, dir);
      showMsg("success", t("addOk", { version: added.version }));
      await refresh();
    } catch (e) {
      showMsg("error", t("addFailed", { error: String(e) }));
    }
  };

  const handleRemove = async (item: RuntimeVersion) => {
    const ok = await confirm({
      title: t("removeConfirmTitle"),
      message: t("removeConfirmMessage"),
      confirmText: t("remove"),
      icon: "warning",
    });
    if (!ok) return;
    try {
      await invokeRemoveCustomRuntime(subTab, item.path);
      showMsg("success", t("removeOk"));
      await refresh();
    } catch (e) {
      showMsg("error", t("removeFailed", { error: String(e) }));
    }
  };

  const handleUninstall = async (item: RuntimeVersion) => {
    if (busyPath) return;
    const ok = await confirm({
      title: t("uninstallTitle"),
      message: t("uninstallMessage", { version: item.version, path: item.path }),
      confirmText: t("uninstall"),
      icon: "danger",
    });
    if (!ok) return;
    setBusyPath(item.path);
    try {
      await invokeUninstallRuntime(subTab, item.path);
      showMsg("success", t("uninstallOk"));
      await refresh();
    } catch (e) {
      showMsg("error", t("uninstallFailed", { error: String(e) }));
    } finally {
      setBusyPath(null);
    }
  };

  const handleCopy = async (path: string) => {
    try {
      await invokeCopyToClipboard(path);
      showMsg("success", t("copied"));
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const handleOpen = async (path: string) => {
    try {
      await invokeOpenRuntimeFolder(path);
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  return (
    <div className="git-manager">
      <div className="git-manager-column">
        <div className="git-subnav" role="tablist" aria-label={tn("runtime")}>
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "node"}
            className={`git-subnav-item ${subTab === "node" ? "active" : ""}`}
            onClick={() => setSubTab("node")}
          >
            {t("node")}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={subTab === "jdk"}
            className={`git-subnav-item ${subTab === "jdk" ? "active" : ""}`}
            onClick={() => setSubTab("jdk")}
          >
            {t("jdk")}
          </button>
        </div>

        <div className="git-manager-body" role="tabpanel">
          <div className="page-scrollable runtime-page">
            <div className="runtime-header">
              <div>
                <div className="runtime-subtitle">{t("subtitle")}</div>
                <div className="runtime-current">
                  {loading && !active ? (
                    <>
                      <Loader2 size={14} className="spin" />
                      <span className="runtime-muted">{t("detectingCurrent")}</span>
                    </>
                  ) : active ? (
                    <>
                      <span className="runtime-badge active">{t("active")}</span>
                      <span>
                        {subTab === "node" ? "v" : ""}
                        {active.version}
                      </span>
                      <span className="runtime-current-path" title={active.path}>
                        {active.path}
                      </span>
                    </>
                  ) : (
                    <span className="runtime-muted">{t("currentNone")}</span>
                  )}
                </div>
              </div>
              <div className="runtime-actions">
                <button type="button" className="btn btn-secondary" onClick={() => refresh()} disabled={loading}>
                  {loading ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
                  {t("refresh")}
                </button>
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => setInstallOpen(true)}
                  disabled={busyPath !== null}
                >
                  <Download size={14} />
                  {t("installOpen")}
                </button>
                <button type="button" className="btn btn-primary" onClick={handleAdd} disabled={busyPath !== null}>
                  <FolderPlus size={14} />
                  {t("addPath")}
                </button>
              </div>
            </div>

            {message && (
              <div className={`runtime-msg ${message.type}`}>
                {message.type === "success" ? <Check size={14} /> : <XCircle size={14} />}
                <span>{message.text}</span>
              </div>
            )}

            <div className="runtime-hint">{t("hintRestart")}</div>
            {hasMachineConflict && (
              <div className="runtime-hint runtime-hint-warn">{t("elevateConfirmWarning")}</div>
            )}
            {dshPorts.length > 0 && (
              <div
                className="runtime-hint runtime-hint-warn"
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 8,
                  flexWrap: "wrap",
                }}
              >
                <span>{t("dshStaleHint", { count: dshPorts.length })}</span>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={handleRestartDsh}
                  disabled={restartingDsh}
                >
                  {restartingDsh ? (
                    <Loader2 size={12} className="spin" />
                  ) : (
                    <RotateCw size={12} />
                  )}
                  {t("dshRestart")}
                </button>
              </div>
            )}

            {items.length > 0 && (
              <div className="runtime-toolbar">
                <div className="runtime-search">
                  <Search size={14} />
                  <input
                    type="search"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    placeholder={t("searchPlaceholder")}
                    aria-label={t("searchPlaceholder")}
                  />
                </div>
                <span className="runtime-muted">
                  {loading ? (
                    <span className="runtime-inline-loading">
                      <Loader2 size={12} className="spin" />
                      {t("refreshing")}
                    </span>
                  ) : (
                    t("count", { count: filtered.length })
                  )}
                </span>
              </div>
            )}

            {showFullScan ? (
              <div className="runtime-empty">
                <Loader2 size={20} className="spin" />
                <span>{t("loading")}</span>
              </div>
            ) : items.length === 0 ? (
              <div className="runtime-empty">
                <div>{t("empty")}</div>
                <div className="runtime-muted">{t("emptyHint")}</div>
              </div>
            ) : filtered.length === 0 ? (
              <div className="runtime-empty">
                <div>{t("noMatch")}</div>
              </div>
            ) : (
              <div className={`runtime-list ${loading ? "is-refreshing" : ""}`}>
                {filtered.map((item) => {
                  const busy = busyPath === item.path;
                  return (
                    <div
                      key={item.path}
                      className={`runtime-card ${item.active ? "is-active" : ""}`}
                    >
                      <div className="runtime-card-main">
                        <div className="runtime-card-title">
                          <span className="runtime-ver">
                            {subTab === "node" ? "v" : ""}
                            {item.version}
                          </span>
                          {item.active && (
                            <span className="runtime-badge active">{t("active")}</span>
                          )}
                          {item.onMachinePath && (
                            <span className="runtime-badge machine">{t("machinePath")}</span>
                          )}
                          <span className="runtime-source">{sourceLabel(item.source, t)}</span>
                        </div>
                        <div className="runtime-card-path" title={item.path}>
                          {item.path}
                        </div>
                      </div>
                      <div className="runtime-card-actions">
                        <button
                          type="button"
                          className="btn btn-secondary"
                          title={t("copyPath")}
                          onClick={() => handleCopy(item.path)}
                        >
                          <Copy size={14} />
                        </button>
                        <button
                          type="button"
                          className="btn btn-secondary"
                          title={t("openFolder")}
                          onClick={() => handleOpen(item.path)}
                        >
                          <FolderOpen size={14} />
                        </button>
                        <button
                          type="button"
                          className="btn btn-secondary"
                          title={t("openTerminal")}
                          onClick={() => handleOpenTerminal(item)}
                        >
                          <Terminal size={14} />
                        </button>
                        <button
                          type="button"
                          className="btn btn-primary"
                          disabled={item.active || busy || busyPath !== null}
                          onClick={() => handleSwitch(item)}
                        >
                          {busy ? (
                            <>
                              <Loader2 size={14} className="spin" />
                              {t("switching")}
                            </>
                          ) : (
                            t("switch")
                          )}
                        </button>
                        {item.custom && (
                          <button
                            type="button"
                            className="btn btn-secondary"
                            title={t("remove")}
                            onClick={() => handleRemove(item)}
                            disabled={busyPath !== null}
                          >
                            <Trash2 size={14} />
                          </button>
                        )}
                        {item.source === "managed" && (
                          <button
                            type="button"
                            className="btn btn-secondary"
                            title={t("uninstall")}
                            onClick={() => handleUninstall(item)}
                            disabled={busyPath !== null}
                          >
                            <Trash2 size={14} />
                          </button>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </div>
      </div>

      {installOpen && (
        <RuntimeInstallModal
          kind={subTab}
          onClose={() => setInstallOpen(false)}
          onInstalled={refresh}
        />
      )}
    </div>
  );
}

export default VersionSwitcher;
