/**
 * 端口查杀面板（快捷应用页的第二个视图）。
 *
 * 后端 `port_commands.rs` 负责 netstat 解析、系统关键进程保护与杀前复检；这里只做
 * 展示与确认——保护判定不信任前端，受保护的进程后端同样会拒绝。
 */
import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Ban, ChevronRight, Loader2, RefreshCw, Search, ShieldAlert } from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import type { PortListener } from "../../core/types";

const MAX_PORTS = 64;

/** `"3000, 5432 8080-8090"` → 去重后的合法端口；范围展开后总量封顶。 */
export function parsePortList(text: string): number[] {
  const out: number[] = [];
  for (const chunk of text.split(/[,，;；\s]+/)) {
    if (!chunk) continue;
    const range = chunk.match(/^(\d+)-(\d+)$/);
    const values = range
      ? Array.from({ length: Math.min(Number(range[2]) - Number(range[1]) + 1, MAX_PORTS) }, (_, i) => Number(range[1]) + i)
      : [Number(chunk)];
    for (const value of values) {
      if (Number.isInteger(value) && value >= 1 && value <= 65535 && !out.includes(value)) {
        out.push(value);
        if (out.length >= MAX_PORTS) return out;
      }
    }
  }
  return out;
}

type ProtoFilter = "tcp" | "udp" | "all";

export default function PortKillerPanel() {
  const { t } = useTranslation("devtools");
  const confirm = useConfirm();
  const invokeFindPortListeners = useGlobalStore((s) => s.invokeFindPortListeners);
  const invokeKillPortListeners = useGlobalStore((s) => s.invokeKillPortListeners);

  const [portText, setPortText] = useState("");
  const [queriedPorts, setQueriedPorts] = useState<number[]>([]);
  const [listeners, setListeners] = useState<PortListener[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  /** 杀完端口仍未释放是"要人接着处理的事实"，不能像提示那样几秒就消失。 */
  const [heldWarning, setHeldWarning] = useState<string | null>(null);
  const [proto, setProto] = useState<ProtoFilter>("tcp");
  const [hideSystem, setHideSystem] = useState(false);
  const [killingPorts, setKillingPorts] = useState<Set<number>>(new Set());
  const [openPids, setOpenPids] = useState<Set<number>>(new Set());

  const runQuery = useCallback(
    async (ports: number[]) => {
      setLoading(true);
      setError(null);
      setNotice(null);
      try {
        const result = await invokeFindPortListeners(ports);
        setListeners(result);
        setQueriedPorts(ports);
      } catch (e) {
        setListeners([]);
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [invokeFindPortListeners]
  );

  const handleQuery = useCallback(() => {
    const ports = parsePortList(portText);
    if (ports.length === 0) {
      setError(t("ports.needPort"));
      return;
    }
    setHeldWarning(null);
    void runQuery(ports);
  }, [portText, runQuery, t]);

  const visible = useMemo(
    () =>
      (listeners ?? []).filter(
        (l) => (proto === "all" || l.proto === proto.toUpperCase()) && (!hideSystem || l.protection === "")
      ),
    [listeners, proto, hideSystem]
  );

  /** 按端口分组：多端口查询时每组一个端口，"结束全部"也按组给。 */
  const groups = useMemo(() => {
    const byPort = new Map<number, PortListener[]>();
    for (const listener of visible) {
      const bucket = byPort.get(listener.queriedPort);
      if (bucket) bucket.push(listener);
      else byPort.set(listener.queriedPort, [listener]);
    }
    return [...byPort.entries()].sort((a, b) => a[0] - b[0]);
  }, [visible]);

  const protectionReason = useCallback(
    (l: PortListener) =>
      l.protection === "system-pid"
        ? t("ports.protectedSystem")
        : t("ports.protectedCritical", { name: l.processName || l.pid }),
    [t]
  );

  const killTargets = useCallback(
    async (port: number, targets: PortListener[]) => {
      const impact = targets
        .map(
          (l) =>
            `· ${l.processName || t("ports.unknown")}  PID ${l.pid}  ·  ${l.localAddrs.join(" + ")}${
              l.exePath ? `  ·  ${l.exePath}` : ""
            }`
        )
        .join("\n");
      const ok = await confirm({
        title: t("ports.killTitle", { port }),
        message: `${t("ports.killWillRelease", { port })}\n${impact}`,
        warning: t("ports.killWarning"),
        confirmText: t("ports.kill"),
        icon: "danger",
      });
      if (!ok) return;

      setKillingPorts((prev) => new Set(prev).add(port));
      setHeldWarning(null);
      try {
        const result = await invokeKillPortListeners(port, targets.map((l) => l.pid));
        // runQuery clears the messages, so refresh first and report afterwards.
        await runQuery(queriedPorts.length > 0 ? queriedPorts : [port]);
        if (result.failed.length > 0) setError(result.failed.join("；"));
        // Only complain about listeners this tool is actually able to kill — a port
        // shared with a protected system process stays occupied by design, and listing
        // PID 4 as "still holding it" reads like a failure to the user.
        const stillKillable = result.stillListening.filter((l) => l.proto === "TCP" && l.protection === "");
        if (result.portReleased) {
          setNotice(t("ports.killed", { count: result.killedPids.length, port }));
        } else if (stillKillable.length === 0) {
          setHeldWarning(t("ports.stillHeldSystem", { port }));
        } else {
          setHeldWarning(
            t("ports.stillHeld", {
              port,
              who: stillKillable.map((l) => `${l.processName || t("ports.unknown")} · PID ${l.pid}`).join(" / "),
            })
          );
        }
      } catch (e) {
        setError(String(e));
      } finally {
        setKillingPorts((prev) => {
          const next = new Set(prev);
          next.delete(port);
          return next;
        });
      }
    },
    [confirm, invokeKillPortListeners, queriedPorts, runQuery, t]
  );

  const toggleDetail = (pid: number) =>
    setOpenPids((prev) => {
      const next = new Set(prev);
      if (next.has(pid)) next.delete(pid);
      else next.add(pid);
      return next;
    });

  const tcpCount = visible.reduce((n, l) => n + (l.proto === "TCP" ? 1 : 0), 0);

  return (
    <div className="ports-panel">
      <div className="devports-filter-bar">
        <div className="devports-search">
          <Search size={14} className="devports-search-icon" />
          <input
            className="devtools-input"
            value={portText}
            onChange={(e) => setPortText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleQuery();
            }}
            placeholder={t("ports.placeholder")}
            spellCheck={false}
            autoFocus
          />
        </div>
        <button type="button" className="al-btn al-btn-primary" onClick={handleQuery} disabled={loading}>
          {loading ? <Loader2 size={14} className="spin" /> : <Search size={14} />}
          <span>{t("ports.query")}</span>
        </button>
        <div className="devports-proto-filter">
          {(["tcp", "udp", "all"] as ProtoFilter[]).map((key) => (
            <button
              key={key}
              type="button"
              className={`al-btn al-btn-ghost ${proto === key ? "al-btn-active" : ""}`}
              onClick={() => setProto(key)}
            >
              {t(`ports.proto_${key}`)}
            </button>
          ))}
        </div>
        <label className="ports-check">
          <input type="checkbox" checked={hideSystem} onChange={(e) => setHideSystem(e.target.checked)} />
          <span>{t("ports.hideSystem")}</span>
        </label>
        <button
          type="button"
          className="al-icon-btn"
          onClick={() => void runQuery(queriedPorts)}
          disabled={loading || queriedPorts.length === 0}
          title={t("ports.refresh")}
        >
          <RefreshCw size={14} />
        </button>
      </div>

      <p className="ports-hint">{t("ports.hint")}</p>

      {error && (
        <div className="al-toast error" role="alert">
          <span>{error}</span>
        </div>
      )}
      {heldWarning && (
        <div className="al-toast error" role="alert">
          <span>{heldWarning}</span>
        </div>
      )}
      {notice && (
        <div className="al-toast success">
          <span>{notice}</span>
        </div>
      )}

      {loading && listeners === null ? (
        <div className="al-scan-loading">
          <Loader2 size={28} className="spin" />
          <span>{t("ports.loading")}</span>
        </div>
      ) : listeners === null ? (
        <div className="al-empty al-empty-sm">
          <Search size={22} className="al-empty-icon" />
          <span>{t("ports.emptyIntro")}</span>
        </div>
      ) : visible.length === 0 ? (
        <div className="al-empty al-empty-sm">
          <ShieldAlert size={22} className="al-empty-icon" />
          <span>
            {listeners.length === 0
              ? t("ports.emptyNoHit", { ports: queriedPorts.join(", ") })
              : t("ports.emptyFiltered")}
          </span>
        </div>
      ) : (
        <div className="devports-list">
          {groups.map(([port, items]) => {
            const killableItems = items.filter((l) => l.protection === "");
            const killing = killingPorts.has(port);
            return (
              <div className="ports-group" key={port}>
                <div className="ports-group-head">
                  <span className="ports-group-title">
                    {t("ports.portHeading", { port })}
                    <strong>{items.length}</strong>
                  </span>
                  {killableItems.length > 1 && (
                    <button
                      type="button"
                      className="al-action al-action-kill is-active"
                      disabled={killing}
                      onClick={() => void killTargets(port, killableItems)}
                    >
                      {killing ? <Loader2 size={12} className="spin" /> : <Ban size={12} />}
                      <span>{t("ports.killAll", { count: killableItems.length })}</span>
                    </button>
                  )}
                </div>
                {items.map((l) => {
                  const open = openPids.has(l.pid);
                  return (
                    <div className={`devports-card ${open ? "open" : ""}`} key={`${l.proto}-${l.pid}`}>
                      <div className="devports-card-main">
                        <button
                          type="button"
                          className="devports-card-chevron"
                          onClick={() => toggleDetail(l.pid)}
                          title={t("ports.detail")}
                        >
                          <ChevronRight size={14} className={open ? "ports-chev-open" : ""} />
                        </button>
                        <span className="devports-card-addr">{l.queriedPort}</span>
                        <span className="devports-card-remote">{l.localAddrs.join(" + ")}</span>
                        <span className="devports-card-state">
                          {l.proto}
                          {l.state ? ` · ${l.state}` : ""}
                        </span>
                        <span className="devports-card-pid">{l.pid}</span>
                        <span className="devports-card-process">
                          <span className="devports-proc-name">{l.processName || t("ports.unknown")}</span>
                          {l.exePath && <span className="devports-proc-path">{l.exePath}</span>}
                        </span>
                        <span className="devports-card-actions">
                          {l.protection === "" ? (
                            <button
                              type="button"
                              className="al-action al-action-kill is-active"
                              disabled={killing}
                              onClick={() => void killTargets(port, [l])}
                            >
                              {killing ? <Loader2 size={12} className="spin" /> : <Ban size={12} />}
                              <span>{t("ports.kill")}</span>
                            </button>
                          ) : (
                            <span className="ports-protected" title={protectionReason(l)}>
                              <ShieldAlert size={13} />
                              <span>{t("ports.protected")}</span>
                            </span>
                          )}
                        </span>
                      </div>
                      {open && (
                        <div className="devports-card-detail">
                          <div className="devports-detail-grid">
                            {(
                              [
                                { label: t("ports.detail_addr"), value: `${l.localAddrs.join(" + ")}:${l.queriedPort}` },
                                { label: t("ports.detail_proto"), value: `${l.proto}${l.state ? ` · ${l.state}` : ""}` },
                                { label: t("ports.detail_pid"), value: String(l.pid) },
                                { label: t("ports.detail_name"), value: l.processName || t("ports.unknown") },
                                { label: t("ports.detail_path"), value: l.exePath || "—", grow: true },
                                ...(l.protection ? [{ label: t("ports.detail_protected"), value: protectionReason(l) }] : []),
                              ] as { label: string; value: string; grow?: boolean }[]
                            ).map((row) => (
                              <div className="devports-detail-row" key={row.label}>
                                <span className="devports-detail-label">{row.label}</span>
                                <span className={`devports-detail-value ${row.grow ? "devports-detail-grow" : ""}`}>
                                  {row.value}
                                </span>
                              </div>
                            ))}
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>
      )}

      {visible.length > 0 && (
        <div className="devports-stats">
          <span>{t("ports.stats", { total: visible.length, tcp: tcpCount, udp: visible.length - tcpCount })}</span>
        </div>
      )}
    </div>
  );
}
