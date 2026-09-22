import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, RefreshCw, XCircle } from "lucide-react";
import { useGlobalStore } from "../core/store";
import TestModal from "./TestModal";
import type { CoverageMetric, CoverageReport, TestProject } from "../core/types";

type Props = {
  project: TestProject;
  onClose: () => void;
  /** One-click "run again with coverage flags"; the flag mapping lives in Rust. */
  onRerunWithCoverage?: () => void;
};

/** Thresholds follow the usual istanbul watermarks (80 / 60). */
const levelClass = (percentage: number) =>
  percentage >= 80 ? "tm-cov-good" : percentage >= 60 ? "tm-cov-warn" : "tm-cov-bad";

const HINTED_FRAMEWORKS = ["jest", "vitest", "cargo", "pytest"];

/** Mirrors `coverage_extra_args` in test_commands.rs — anything else errors in Rust. */
const ONE_CLICK_COVERAGE = ["jest", "vitest", "pytest", "maven"];

export default function CoverageReportView({ project, onClose, onRerunWithCoverage }: Props) {
  const { t } = useTranslation("test");
  const readCoverageReport = useGlobalStore((s) => s.readCoverageReport);

  const [report, setReport] = useState<CoverageReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setReport(await readCoverageReport(project.id));
    } catch (e) {
      setReport(null);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [readCoverageReport, project.id]);

  useEffect(() => {
    void load();
  }, [load]);

  const categories: { key: keyof CoverageReport; label: string }[] = useMemo(
    () => [
      { key: "lines", label: t("lines") },
      { key: "statements", label: t("statements") },
      { key: "branches", label: t("branches") },
      { key: "functions", label: t("functions") },
    ],
    [t]
  );

  // Worst-covered files first: that is where the next test is worth writing.
  const files = useMemo(() => {
    if (!report) return [];
    return [...report.files].sort((a, b) => a.lines.percentage - b.lines.percentage);
  }, [report]);

  const pct = (metric: CoverageMetric) => `${metric.percentage.toFixed(1)}%`;

  return (
    <TestModal
      title={`${t("coverage")} · ${project.name}`}
      onClose={onClose}
      wide
      footer={
        <>
          <span className="tm-hint tm-footer-hint">
            {t(
              HINTED_FRAMEWORKS.includes(project.framework)
                ? `coverageHint.${project.framework}`
                : "coverageHint.other"
            )}
          </span>
          {onRerunWithCoverage && ONE_CLICK_COVERAGE.includes(project.framework) && (
            <button
              type="button"
              className="btn btn-secondary"
              onClick={onRerunWithCoverage}
              title={t("coverageRerunHint")}
            >
              <RefreshCw size={13} />
              {t("coverageRerun")}
            </button>
          )}
          <button type="button" className="btn btn-secondary" onClick={() => void load()} disabled={loading}>
            {loading ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
            {t("refresh")}
          </button>
        </>
      }
    >
      {loading && (
        <div className="tm-loading">
          <Loader2 size={16} className="spin" />
          <span>{t("coverageLoading")}</span>
        </div>
      )}

      {!loading && error && (
        <div className="tm-error-state">
          <div className="tm-error-head">
            <XCircle size={18} className="tm-cov-bad" />
            <span className="tm-error-title">{t("coverageLoadError")}</span>
            <button
              type="button"
              className="btn btn-secondary btn-small tm-error-copy"
              onClick={() => {
                void navigator.clipboard.writeText(error).then(() => {
                  setCopied(true);
                  window.setTimeout(() => setCopied(false), 1500);
                });
              }}
              title={t("copy")}
            >
              {copied ? t("copied") : t("copy")}
            </button>
          </div>
          <p className="tm-error-detail">{error}</p>
        </div>
      )}

      {!loading && !error && report && (
        <>
          <div className="tm-cov-grid">
            {categories.map((category) => {
              const metric = report[category.key] as CoverageMetric;
              return (
                <div key={category.key} className="tm-cov-card">
                  <span className="tm-cov-label">{category.label}</span>
                  <span className={`tm-cov-value ${levelClass(metric.percentage)}`}>
                    {pct(metric)}
                  </span>
                  <span className="tm-cov-bar" aria-hidden>
                    <span
                      className={`tm-cov-bar-fill ${levelClass(metric.percentage)}`}
                      style={{ width: `${Math.min(100, metric.percentage)}%` }}
                    />
                  </span>
                  <span className="tm-cov-detail">
                    {metric.covered}/{metric.total}
                  </span>
                </div>
              );
            })}
          </div>

          <h4 className="tm-section-title">{t("files")}</h4>
          {files.length === 0 ? (
            <div className="tm-empty">{t("noCoverageFiles")}</div>
          ) : (
            <table className="tm-table">
              <thead>
                <tr>
                  <th>{t("file")}</th>
                  <th className="tm-num">{t("lines")}</th>
                  <th className="tm-num">{t("statements")}</th>
                  <th className="tm-num">{t("branches")}</th>
                  <th className="tm-num">{t("functions")}</th>
                </tr>
              </thead>
              <tbody>
                {files.map((file) => (
                  <tr key={file.path}>
                    <td className="tm-file-path" title={file.path}>
                      {file.path}
                    </td>
                    {(["lines", "statements", "branches", "functions"] as const).map((key) => (
                      <td key={key} className={`tm-num ${levelClass(file[key].percentage)}`}>
                        {pct(file[key])}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </TestModal>
  );
}
