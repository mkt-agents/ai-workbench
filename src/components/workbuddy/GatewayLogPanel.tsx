import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, RefreshCw, Trash2 } from "lucide-react";
import { wbKeyList, wbLogClear, wbLogList, wbLogModels, type WbRequestLog } from "../../lib/workbuddy";
import { useConfirm } from "../ConfirmModal";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

const PAGE = 200;

function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function statusTone(code: number | null): string {
  if (code == null) return "unverified";
  if (code >= 500) return "expired";
  if (code >= 400) return "unverified";
  return "active";
}

/** 网关调用流水。只读展示 + 清空，写入全部发生在 Rust 侧。 */
export default function GatewayLogPanel({ refreshSignal }: { refreshSignal?: number }) {
  const { t } = useTranslation("workbuddy");
  const confirm = useConfirm();
  const translateError = useInvokeErrorTranslator();

  const [rows, setRows] = useState<WbRequestLog[]>([]);
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState("");
  const [keys, setKeys] = useState<{ id: number; label: string }[]>([]);
  const [keyId, setKeyId] = useState<number | null>(null);
  const [range, setRange] = useState<"all" | "today" | "7d">("all");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const sinceMs = (): number | null => {
    if (range === "today") {
      const d = new Date();
      d.setHours(0, 0, 0, 0);
      return d.getTime();
    }
    if (range === "7d") return Date.now() - 7 * 24 * 60 * 60 * 1000;
    return null;
  };

  const refresh = useCallback(async () => {
    try {
      setRows(await wbLogList({ limit: PAGE, model: model || null, keyId, since: sinceMs() }));
      setModels(await wbLogModels());
      setError(null);
    } catch (e) {
      setError(translateError(e));
    } finally {
      setLoading(false);
    }
  }, [model, keyId, range, translateError]);

  useEffect(() => {
    void wbKeyList()
      .then((list) => setKeys(list.map((k) => ({ id: k.id, label: k.label }))))
      .catch(() => setKeys([]));
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh, refreshSignal]);

  const clear = async () => {
    const ok = await confirm({
      title: t("logClear"),
      message: t("logClearConfirm", { count: rows.length }),
      warning: t("logClearWarning"),
      icon: "danger",
    });
    if (!ok) return;
    await wbLogClear().catch((e) => setError(translateError(e)));
    await refresh();
  };

  const totalTokens = rows.reduce(
    (sum, r) => sum + (r.promptTokens ?? 0) + (r.completionTokens ?? 0),
    0
  );
  const failures = rows.filter((r) => (r.statusCode ?? 0) >= 400 || r.error).length;

  return (
    <div className="card wb-card">
      <div className="card-title">
        <span>{t("logTitle")}</span>
        <span className="card-title-badge">{rows.length}</span>
        <div className="wb-log-tools">
          {keys.length > 0 && (
            <select
              className="input-field wb-log-filter"
              value={keyId ?? ""}
              onChange={(e) => setKeyId(e.target.value ? Number(e.target.value) : null)}
            >
              <option value="">{t("logAllKeys")}</option>
              {keys.map((k) => (
                <option key={k.id} value={k.id}>
                  {k.label}
                </option>
              ))}
            </select>
          )}
          {models.length > 0 && (
            <select className="input-field wb-log-filter" value={model} onChange={(e) => setModel(e.target.value)}>
              <option value="">{t("logAllModels")}</option>
              {models.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          )}
          <select
            className="input-field wb-log-filter"
            value={range}
            onChange={(e) => setRange(e.target.value as "all" | "today" | "7d")}
          >
            <option value="all">{t("logRangeAll")}</option>
            <option value="today">{t("logRangeToday")}</option>
            <option value="7d">{t("logRange7d")}</option>
          </select>
          <button type="button" className="btn btn-secondary btn-small" onClick={() => void refresh()} title={t("logRefresh")}>
            <RefreshCw size={12} />
          </button>
          <button
            type="button"
            className="btn btn-danger btn-small"
            onClick={() => void clear()}
            disabled={rows.length === 0}
            title={t("logClear")}
          >
            <Trash2 size={12} />
          </button>
        </div>
      </div>
      <div className="card-body">
        {error && <div className="wb-inline-error" role="alert">{error}</div>}
        <div className="wb-facts">
          <span>
            {t("logTokens")}: <strong>{totalTokens.toLocaleString()}</strong>
          </span>
          <span>
            {t("logFailures")}: <strong>{failures}</strong>
          </span>
          <span className="wb-hint-inline">{t("logCap")}</span>
        </div>

        {loading ? (
          <div className="wb-loading">
            <Loader2 size={14} className="spin" />
          </div>
        ) : rows.length === 0 ? (
          <p className="wb-hint">{t("logEmpty")}</p>
        ) : (
          <div className="wb-log-scroll">
            <table className="wb-table wb-log-table">
              <thead>
                <tr>
                  <th>{t("logTime")}</th>
                  <th>{t("keyLabel")}</th>
                  <th>{t("logModel")}</th>
                  <th>{t("logAccount")}</th>
                  <th>{t("logStatus")}</th>
                  <th className="wb-num">{t("logTokensShort")}</th>
                  <th className="wb-num">{t("logLatency")}</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => (
                  <tr key={row.id} title={row.error ?? undefined}>
                    <td className="wb-cell-time">{formatTime(row.ts)}</td>
                    <td>{row.keyLabel ?? "—"}</td>
                    <td>
                      <span className="mono">{row.model ?? "—"}</span>
                      {row.stream && <span className="wb-tag-stream">SSE</span>}
                    </td>
                    <td>{row.accountLabel ?? "—"}</td>
                    <td>
                      <span className={`wb-status wb-status-${statusTone(row.statusCode)}`}>
                        {row.statusCode ?? "err"}
                      </span>
                    </td>
                    <td className="wb-num mono">
                      {row.promptTokens ?? 0}/{row.completionTokens ?? 0}
                    </td>
                    <td className="wb-num mono">{row.latencyMs != null ? `${row.latencyMs}ms` : "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
