import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ChevronDown,
  ChevronRight,
  ClipboardCopy,
  Loader2,
  RefreshCw,
  Skull,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import type { DevtoolsPortEntry, DevtoolsProcessInfo } from "../../core/types";

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

  const cancelled = useRef(false);

  const load = useCallback(async () => {
    setLoading(true);
    setMessage(null);
    try {
      const list = await listPorts();
      if (cancelled.current) return;
      setPorts(list);
      setPidMap({});
      setExpanded(new Set());

      // Bulk-resolve all unique PIDs at once (3 commands total, regardless of count)
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
          // non-fatal: rows will show PID only
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

  const resolvingCount = resolving ? Object.keys(pidMap).length === 0 && ports.length > 0 : false;

  return (
    <div className="devtools-tool">
      <div className="devtools-row">
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={load}
          disabled={loading}
        >
          {loading ? <Loader2 size={14} className="spin" /> : <RefreshCw size={14} />}
          {t("ports.refresh")}
        </button>
        {resolvingCount && (
          <span className="devports-resolve-hint">
            <Loader2 size={12} className="spin" />
            {t("ports.resolving")}
          </span>
        )}
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <XCircle size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      {loading && ports.length === 0 ? (
        <div className="runtime-empty">
          <Loader2 size={20} className="spin" />
          <span>{t("ports.loading")}</span>
        </div>
      ) : ports.length === 0 ? (
        <div className="runtime-empty">{t("ports.empty")}</div>
      ) : (
        <div className="devports-list">
          {ports.map((p, i) => {
            const key = `${p.proto}-${p.local_addr}-${p.local_port}-${p.pid}-${i}`;
            const info = pidMap[p.pid];
            const isOpen = expanded.has(key);
            return (
              <PortRow
                key={key}
                port={p}
                info={info}
                isOpen={isOpen}
                killing={killing}
                onToggle={() => toggleRow(key)}
                onKill={() => handleKill(p.pid, info?.name || "")}
                onCopyPath={handleCopyPath}
                t={t}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

interface PortRowProps {
  port: DevtoolsPortEntry;
  info: DevtoolsProcessInfo | undefined;
  isOpen: boolean;
  killing: number | null;
  onToggle: () => void;
  onKill: () => void;
  onCopyPath: (path: string) => void;
  t: (key: string) => string;
}

function PortRow({ port, info, isOpen, killing, onToggle, onKill, onCopyPath, t }: PortRowProps) {
  const hasDetail = port.pid !== 0;

  return (
    <div className={`devports-card ${isOpen ? "open" : ""}`}>
      <div
        className="devports-card-main"
        onClick={hasDetail ? onToggle : undefined}
        style={hasDetail ? { cursor: "pointer" } : undefined}
      >
        <span className="devports-card-chevron">
          {hasDetail ? (
            isOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />
          ) : null}
        </span>
        <span className={`tag tag-${port.proto.toLowerCase()}`}>{port.proto}</span>
        <span className="devports-card-addr">
          {port.local_addr}:{port.local_port}
        </span>
        <span className="devports-card-remote">
          {port.remote_addr}:{port.remote_port || "*"}
        </span>
        <span className="devports-card-state">{port.state || "—"}</span>
        <span className="devports-card-pid">{port.pid}</span>
        <span className="devports-card-process">
          {!hasDetail ? (
            "—"
          ) : !info ? (
            <span className="devports-proc-name">#{port.pid}</span>
          ) : (
            <>
              <span className="devports-proc-name">{info.name || `#${port.pid}`}</span>
              {info.path && (
                <span className="devports-proc-path">{shortPath(info.path)}</span>
              )}
            </>
          )}
        </span>
        <span className="devports-card-actions" onClick={(e) => e.stopPropagation()}>
          {hasDetail && (
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={onKill}
              disabled={killing === port.pid}
              title={t("ports.kill")}
            >
              {killing === port.pid ? (
                <Loader2 size={12} className="spin" />
              ) : (
                <Skull size={12} />
              )}
            </button>
          )}
        </span>
      </div>

      {isOpen && info && (
        <div className="devports-card-detail">
          <div className="devports-detail-grid">
            <div className="devports-detail-row">
              <span className="devports-detail-label">{t("ports.process")}</span>
              <span className="devports-detail-value">{info.name || `#${port.pid}`}</span>
            </div>
            <div className="devports-detail-row devports-detail-grow">
              <span className="devports-detail-label">{t("ports.localAddress")}</span>
              <div className="devports-detail-path-wrap">
                <code className="devtools-pre devports-detail-path">
                  {info.path || <span className="devports-na">—</span>}
                </code>
                {info.path && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => onCopyPath(info.path)}
                    title={t("ports.copy")}
                  >
                    <ClipboardCopy size={12} />
                  </button>
                )}
              </div>
            </div>
            {info.services && (
              <div className="devports-detail-row devports-detail-grow">
                <span className="devports-detail-label">{t("ports.proto")}</span>
                <div className="devports-services">
                  {info.services.split(", ").map((s, idx) => (
                    <span key={idx} className="tag tag-service">
                      {s}
                    </span>
                  ))}
                </div>
              </div>
            )}
            <div className="devports-detail-row">
              <span className="devports-detail-label">{t("ports.memory")}</span>
              <span className="devports-detail-value">{info.memory || "—"}</span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default PortProcessTool;
