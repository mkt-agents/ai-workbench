import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import { wbLogStats, type WbStatGroup, type WbStatRow } from "../../lib/workbuddy";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

const RANGES = [1, 7, 30] as const;

/**
 * 用量统计。条形图用纯 CSS 宽度百分比画，不引图表库——这里只需要"哪个桶更大"
 * 这一件事，一套 canvas 依赖换不来更多。
 */
export default function UsageStatsPanel() {
  const { t } = useTranslation("workbuddy");
  const translateError = useInvokeErrorTranslator();
  const [days, setDays] = useState<number>(7);
  const [groupBy, setGroupBy] = useState<WbStatGroup>("day");
  const [rows, setRows] = useState<WbStatRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setRows(await wbLogStats(days, groupBy));
      setError(null);
    } catch (e) {
      setError(translateError(e));
    } finally {
      setLoading(false);
    }
  }, [days, groupBy, translateError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const totals = rows.reduce(
    (acc, r) => ({
      requests: acc.requests + r.requests,
      tokens: acc.tokens + r.promptTokens + r.completionTokens,
      errors: acc.errors + r.errors,
    }),
    { requests: 0, tokens: 0, errors: 0 }
  );
  const maxRequests = Math.max(1, ...rows.map((r) => r.requests));

  return (
    <div className="card wb-card">
      <div className="card-title">
        <span>{t("statsTitle")}</span>
        <div className="wb-log-tools">
          <div className="wb-segmented">
            {RANGES.map((r) => (
              <button
                key={r}
                type="button"
                className={`wb-segment ${days === r ? "is-active" : ""}`}
                onClick={() => setDays(r)}
              >
                {t("statsDays", { count: r })}
              </button>
            ))}
          </div>
          <div className="wb-segmented">
            {(["day", "model", "key"] as const).map((g) => (
              <button
                key={g}
                type="button"
                className={`wb-segment ${groupBy === g ? "is-active" : ""}`}
                onClick={() => setGroupBy(g)}
              >
                {t(`statsGroup${g[0].toUpperCase()}${g.slice(1)}`)}
              </button>
            ))}
          </div>
        </div>
      </div>
      <div className="card-body">
        {error && <div className="wb-inline-error" role="alert">{error}</div>}
        <div className="wb-facts">
          <span>
            {t("statsRequests")}: <strong>{totals.requests.toLocaleString()}</strong>
          </span>
          <span>
            {t("statsTokens")}: <strong>{totals.tokens.toLocaleString()}</strong>
          </span>
          <span>
            {t("statsErrors")}: <strong>{totals.errors}</strong>
          </span>
        </div>

        {loading ? (
          <div className="wb-loading">
            <Loader2 size={14} className="spin" />
          </div>
        ) : rows.length === 0 ? (
          <p className="wb-hint">{t("statsEmpty")}</p>
        ) : (
          <ul className="wb-stats">
            {rows.map((row) => (
              <li key={row.label} className="wb-stat-row">
                <span className="wb-stat-label" title={row.label}>
                  {row.label}
                </span>
                <span className="wb-stat-bar-wrap">
                  <span
                    className="wb-stat-bar"
                    style={{ width: `${Math.max(2, (row.requests / maxRequests) * 100)}%` }}
                  />
                </span>
                <span className="wb-stat-num mono">{row.requests}</span>
                <span className="wb-stat-num mono wb-stat-sub">
                  {(row.promptTokens + row.completionTokens).toLocaleString()}
                </span>
                <span className={`wb-stat-num mono ${row.errors > 0 ? "wb-stat-err" : ""}`}>
                  {row.errors}
                </span>
                <span className="wb-stat-num mono wb-stat-sub">
                  {row.avgLatencyMs != null ? `${row.avgLatencyMs}ms` : "—"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
