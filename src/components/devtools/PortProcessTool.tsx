import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ClipboardCopy,
  ListMinus,
  Loader2,
  RefreshCw,
  Search,
  Skull,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import type { DevtoolsPortEntry, DevtoolsProcessInfo } from "../../core/types";

type ProtoFilter = "all" | "tcp" | "udp";

interface PidMap {
  [pid: number]: DevtoolsProcessInfo | undefined;
}

/** Truncate a path for the compact cell: keep the exe name + parent dir. */
function shortPath(path: string): string {
  if (!path) return "";
  const parts = path.split(/[\\/]/);
  if (parts.length <= 2) return path;
  return `…\\${parts.slice(-2).join("\\")}`;
}

function StateBadge({ state }: { state: string | undefined }) {
  const s = (state || "").toLowerCase().replace(/\s+/g, "_");
  let cls = "ports-state-default";
  if (s === "listening") cls = "ports-state-listening";
  else if (s === "established") cls = "ports-state-established";
  else if (s === "time_wait" || s === "close_wait") cls = "ports-state-time_wait";
  return <span className={`ports-state-badge ${cls}`}>{state || "—"}</span>;
}

function PortProcessTool() {
  const { t } = useTranslation("devtools");
  const confirm = useConfirm();
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);
  const listPorts = useGlobalStore((s) => s.invokeListPorts);
  const resolveProcesses = useGlobalStore((s) => s.invokeResolveProcesses);
  const killProcess = useGlobalStore((s) => s.invokeKillProcess);

  const [ports, setPorts] = useState<DevtoolsPortEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [resolving, setResolving] = useState(false);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [killing, setKilling] = useState<number | null>(null);
  const [pidMap, setPidMap] = useState<PidMap>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [search, setSearch] = useState("");
  const [protoFilter, setProtoFilter] = useState<ProtoFilter>("all");
  const [autoRefresh, setAutoRefresh] = useState(false);
  const [hideSystem, setHideSystem] = useState(true);

  const cancelled = useRef(false);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setMessage(null);
    try {
      const list = await listPorts();
      if (cancelled.current) return;
      setPorts(list);
      setPidMap({});
      setExpanded(new Set());

      // Bulk-resolve all unique PIDs at once
      const uniquePids = Array.from(new Set(list.map((p) => p.pid).filter((p) => p !== 0)));
      if (uniquePids.length > 0) {
        setResolving(true);
        try {
          const infos = await resolveProcesses(uniquePids);
          if (!cancelled.current) {
            const map: PidMap = {};
            for (const info of infos) {
              map[info.pid] = info;
            }
            setPidMap(map);
          }
        } catch (e) {
          console.warn("resolveProcesses failed:", e);
        } finally {
          if (!cancelled.current) setResolving(false);
        }
      }
    } catch (e) {
      if (!cancelled.current) {
        setMessage({ type: "error", text: String(e) });
      }
    } finally {
      if (!cancelled.current) setLoading(false);
    }
  }, [listPorts, resolveProcesses]);

  useEffect(() => {
    cancelled.current = false;
    load();
    return () => {
      cancelled.current = true;
    };
  }, [load]);

  // Auto-refresh timer
  useEffect(() => {
    if (autoRefresh) {
      timerRef.current = setInterval(() => {
        load();
      }, 5000);
    } else if (timerRef.current) {
      clearInterval(timerRef.current);
      timerRef.current = null;
    }
    return () => {
      if (timerRef.current) clearInterval(timerRef.current);
    };
  }, [autoRefresh, load]);

  const toggleRow = (key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  const handleKill = async (pid: number, name: string) => {
    const info = pidMap[pid];
    const label = info?.name || name || `#${pid}`;
    const ok = await confirm({
      title: t("ports.kill"),
      message: t("ports.killConfirm", { pid, name: label }),
      confirmText: t("ports.kill"),
      icon: "danger",
    });
    if (!ok) return;
    setKilling(pid);
    try {
      await killProcess(pid);
      setMessage({ type: "success", text: t("ports.killSuccess") });
      await load();
    } catch (e) {
      setMessage({ type: "error", text: t("ports.killFailed", { error: String(e) }) });
    } finally {
      setKilling(null);
    }
  };

  const handleCopyPath = async (path: string) => {
    try {
      await copy(path);
      setMessage({ type: "success", text: t("ports.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  const handleCopyAll = async () => {
    if (filteredPorts.length === 0) return;
    const lines = filteredPorts.map((p) => {
      const info = pidMap[p.pid];
      return `${p.proto}\t${p.local_addr}:${p.local_port}\t${p.remote_addr}:${p.remote_port || "*"}\t${p.state || "—"}\t${p.pid}\t${info?.name || ""}`;
    });
    const text = `Proto\tLocal\tRemote\tState\tPID\tProcess\n${lines.join("\n")}`;
    try {
      await copy(text);
      setMessage({ type: "success", text: t("ports.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  const resolvingCount = resolving ? Object.keys(pidMap).length === 0 && ports.length > 0 : false;

  // Filter ports based on search, protocol, and system process
  const filteredPorts = useMemo(() => {
    const searchLower = search.toLowerCase().trim();
    return ports.filter((p) => {
      if (hideSystem && p.pid === 4) return false;
      const proto = (p.proto || "").toLowerCase();
      if (protoFilter !== "all" && proto !== protoFilter) return false;
      if (!searchLower) return true;
      const info = pidMap[p.pid];
      return (
        String(p.local_port).includes(searchLower) ||
        String(p.pid).includes(searchLower) ||
        (p.local_addr || "").toLowerCase().includes(searchLower) ||
        (info?.name || "").toLowerCase().includes(searchLower) ||
        (info?.path || "").toLowerCase().includes(searchLower)
      );
    });
  }, [ports, search, protoFilter, pidMap, hideSystem]);

  // Stats
  const stats = useMemo(() => {
    const tcpCount = filteredPorts.filter((p) => (p.proto || "").toLowerCase() === "tcp").length;
    const udpCount = filteredPorts.filter((p) => (p.proto || "").toLowerCase() === "udp").length;
    const uniquePids = new Set(filteredPorts.map((p) => p.pid)).size;
    return { tcpCount, udpCount, uniquePids, total: filteredPorts.length };
  }, [filteredPorts]);

  return (
    <div className="devtools-tool ports-tool">
      {/* ── Toolbar ── */}
      <div className="ports-toolbar">
        <div className="ports-toolbar-group">
          <button
            type="button"
            className="ports-action-btn"
            onClick={load}
            disabled={loading}
            title={t("ports.refresh")}
          >
            {loading ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
            <span>{t("ports.refresh")}</span>
          </button>
          <button
            type="button"
            className={`ports-action-btn ${autoRefresh ? "active" : ""}`}
            onClick={() => setAutoRefresh(!autoRefresh)}
            title={t("ports.autoRefresh")}
          >
            <RefreshCw size={13} />
            <span>{t("ports.autoRefresh")}</span>
          </button>
          <label className="ports-action-btn ports-toggle" title={t("ports.hideSystem")}>
            <input
              type="checkbox"
              checked={hideSystem}
              onChange={(e) => setHideSystem(e.target.checked)}
            />
            <ListMinus size={13} />
            <span>{t("ports.hideSystem")}</span>
          </label>
        </div>

        <div className="ports-toolbar-divider" />

        <div className="ports-toolbar-group">
          <span className="ports-toolbar-label">{t("ports.proto")}</span>
          <div className="ports-segmented">
            {(["all", "tcp", "udp"] as ProtoFilter[]).map((proto) => (
              <button
                key={proto}
                type="button"
                className={`ports-seg-item ${protoFilter === proto ? "active" : ""}`}
                onClick={() => setProtoFilter(proto)}
              >
                {proto === "all" ? t("ports.all") : proto.toUpperCase()}
              </button>
            ))}
          </div>
        </div>

        {loading && ports.length === 0 ? (
          <span className="ports-resolve-hint">
            <Loader2 size={12} className="spin" />
            {t("ports.loading")}
          </span>
        ) : resolvingCount ? (
          <span className="ports-resolve-hint">
            <Loader2 size={12} className="spin" />
            {t("ports.resolving")}
          </span>
        ) : null}

        <div className="ports-toolbar-spacer" />

        {ports.length > 0 && (
          <div className="ports-toolbar-group">
            <div className="ports-search">
              <Search size={14} className="ports-search-icon" />
              <input
                className="devtools-input"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder={t("ports.searchPlaceholder")}
                spellCheck={false}
              />
            </div>
            <button
              type="button"
              className="ports-icon-btn"
              onClick={handleCopyAll}
              title={t("ports.copyAll")}
            >
              <ClipboardCopy size={14} />
            </button>
          </div>
        )}
      </div>

      {/* ── Stats ── */}
      {ports.length > 0 && (
        <div className="ports-stats">
          <span>{t("ports.total")}: <strong>{stats.total}</strong></span>
          <span className="ports-stats-dot" />
          <span>TCP: <strong>{stats.tcpCount}</strong></span>
          <span className="ports-stats-dot" />
          <span>UDP: <strong>{stats.udpCount}</strong></span>
          <span className="ports-stats-dot" />
          <span>{t("ports.processes")}: <strong>{stats.uniquePids}</strong></span>
        </div>
      )}

      {/* ── Status ── */}
      {message && (
        <div className={`ports-status ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <XCircle size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── Content ── */}
      {loading && ports.length === 0 ? (
        <div className="ports-empty">
          <Loader2 size={22} className="spin ports-empty-icon" />
          <span>{t("ports.loading")}</span>
        </div>
      ) : ports.length === 0 ? (
        <div className="ports-empty">
          <Search size={22} className="ports-empty-icon" />
          <span>{t("ports.empty")}</span>
        </div>
      ) : filteredPorts.length === 0 ? (
        <div className="ports-empty">
          <Search size={22} className="ports-empty-icon" />
          <span>{t("ports.noMatch")}</span>
        </div>
      ) : (
        <div className="ports-table">
          <div className="ports-table-header">
            <span>{t("ports.proto")}</span>
            <span>{t("ports.localAddress")}</span>
            <span>{t("ports.remoteAddress")}</span>
            <span>{t("ports.state")}</span>
            <span style={{ textAlign: "right" }}>{t("ports.pid")}</span>
            <span>{t("ports.process")}</span>
            <span>{t("ports.actions")}</span>
          </div>
          <div className="ports-table-body">
            {filteredPorts.map((p, i) => {
              const key = `${p.proto}-${p.local_addr}-${p.local_port}-${p.pid}-${i}`;
              const info = pidMap[p.pid];
              const isOpen = expanded.has(key);
              const hasDetail = p.pid !== 0;
              return (
                <Fragment key={key}>
                  <div
                    className={`ports-table-row ${isOpen ? "open" : ""}`}
                    onClick={hasDetail ? () => toggleRow(key) : undefined}
                    style={hasDetail ? { cursor: "pointer" } : undefined}
                  >
                    <span>
                      <span className={`tag tag-${(p.proto || "").toLowerCase()}`}>
                        {p.proto || "?"}
                      </span>
                    </span>
                    <span className="ports-monospace ports-proc-main">
                      {p.local_addr}:{p.local_port}
                    </span>
                    <span className="ports-monospace ports-proc-main">
                      {p.remote_addr}:{p.remote_port || "*"}
                    </span>
                    <span>
                      <StateBadge state={p.state} />
                    </span>
                    <span className="ports-monospace" style={{ textAlign: "right", color: "var(--text-2, #8b949e)" }}>
                      {p.pid}
                    </span>
                    <span>
                      {!hasDetail ? (
                        <span className="ports-na">—</span>
                      ) : !info ? (
                        <span className="ports-proc-main">#{p.pid}</span>
                      ) : (
                        <>
                          <span className="ports-proc-main">{info.name || `#${p.pid}`}</span>
                          {info.path && (
                            <span className="ports-proc-path">{shortPath(info.path)}</span>
                          )}
                        </>
                      )}
                    </span>
                    <span onClick={(e) => e.stopPropagation()}>
                      {hasDetail && (
                        <button
                          type="button"
                          className="ports-kill-btn"
                          onClick={() => handleKill(p.pid, info?.name || "")}
                          disabled={killing === p.pid}
                          title={t("ports.kill")}
                        >
                          {killing === p.pid ? (
                            <Loader2 size={13} className="spin" />
                          ) : (
                            <Skull size={13} />
                          )}
                        </button>
                      )}
                    </span>
                  </div>

                  {isOpen && info && (
                    <div className="ports-table-row expanded-row">
                      <div style={{ gridColumn: "1 / -1" }}>
                        <div className="ports-detail-grid">
                          <div className="ports-detail-row">
                            <span className="ports-detail-label">{t("ports.process")}</span>
                            <span className="ports-detail-value">{info.name || `#${p.pid}`}</span>
                          </div>
                          <div className="ports-detail-row">
                            <span className="ports-detail-label">{t("ports.localAddress")}</span>
                            <div className="ports-detail-path-wrap">
                              <code className="devtools-pre ports-detail-path">
                                {info.path || <span className="ports-na">—</span>}
                              </code>
                              {info.path && (
                                <button
                                  type="button"
                                  className="ports-icon-btn"
                                  onClick={() => handleCopyPath(info.path)}
                                  title={t("ports.copy")}
                                >
                                  <ClipboardCopy size={13} />
                                </button>
                              )}
                            </div>
                          </div>
                          {info.services && (
                            <div className="ports-detail-row">
                              <span className="ports-detail-label">{t("ports.services")}</span>
                              <div className="ports-services">
                                {info.services.split(", ").map((s, idx) => (
                                  <span key={idx} className="tag tag-service">
                                    {s}
                                  </span>
                                ))}
                              </div>
                            </div>
                          )}
                          <div className="ports-detail-row">
                            <span className="ports-detail-label">{t("ports.memory")}</span>
                            <span className="ports-detail-value">{info.memory || "—"}</span>
                          </div>
                        </div>
                      </div>
                    </div>
                  )}
                </Fragment>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

export default PortProcessTool;
