import { useTranslation } from "react-i18next";
import { Ban, Check, Slash, X, XCircle } from "lucide-react";

export type BatchRowStatus = "ok" | "fail" | "skip" | "cancel";

export type BatchRow = {
  path: string;
  label: string;
  status: BatchRowStatus;
  /** Failure text, skip reason or "cancelled before it ran"; omitted for a plain success. */
  detail?: string;
};

const ICON = { ok: Check, fail: XCircle, skip: Slash, cancel: Ban } as const;

/**
 * Per-repo outcome of one batch run. The toast only survives 8 seconds and shows
 * three errors, so a 19-repo run left the user guessing which repos were touched;
 * this list stays until dismissed and hands the failed subset back for a retry.
 */
export default function BatchReport({
  title,
  rows,
  busy,
  onClose,
  onRetry,
}: {
  title: string;
  rows: BatchRow[];
  busy: boolean;
  onClose: () => void;
  onRetry: (paths: string[]) => void;
}) {
  const { t } = useTranslation("git");
  const failed = rows.filter((r) => r.status === "fail");
  const cancelled = rows.filter((r) => r.status === "cancel");
  const skipped = rows.filter((r) => r.status === "skip");
  // A cancel is unfinished work, so it joins the failures in the retry subset.
  const unfinished = [...failed, ...cancelled].map((r) => r.path);
  const okCount = rows.length - failed.length - cancelled.length - skipped.length;
  const severity = failed.length > 0 ? "fail" : unfinished.length + skipped.length > 0 ? "skip" : "ok";

  return (
    <div className={`repos-report is-${severity}`} role="status">
      <div className="repos-report-head">
        <span className="repos-report-title">{title}</span>
        <span className="repos-report-sums">
          <span className="repos-report-sum is-ok">{t("batch.report.ok", { count: okCount })}</span>
          {skipped.length > 0 && (
            <span className="repos-report-sum is-skip">{t("batch.report.skip", { count: skipped.length })}</span>
          )}
          {cancelled.length > 0 && (
            <span className="repos-report-sum is-cancel">
              {t("batch.report.cancel", { count: cancelled.length })}
            </span>
          )}
          {failed.length > 0 && (
            <span className="repos-report-sum is-fail">{t("batch.report.fail", { count: failed.length })}</span>
          )}
        </span>
        {unfinished.length > 0 && (
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={busy}
            onClick={() => onRetry(unfinished)}
            title={t("batch.report.retryTip")}
          >
            {cancelled.length > 0
              ? t("batch.report.retryUnfinished", { count: unfinished.length })
              : t("batch.report.retry", { count: failed.length })}
          </button>
        )}
        <button
          type="button"
          className="btn commit-icon-btn"
          onClick={onClose}
          title={t("batch.report.close")}
          aria-label={t("batch.report.close")}
        >
          <X size={14} />
        </button>
      </div>
      <ul className="repos-report-list">
        {rows.map((row) => {
          const Icon = ICON[row.status];
          return (
            <li key={row.path} className={`repos-report-row is-${row.status}`}>
              <span className="repos-report-state">
                <Icon size={12} aria-hidden />
                {t(`batch.report.st_${row.status}`)}
              </span>
              <span className="repos-report-name" title={row.path}>
                {row.label}
              </span>
              {row.detail && (
                <span className="repos-report-detail" title={row.detail}>
                  {row.detail}
                </span>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
