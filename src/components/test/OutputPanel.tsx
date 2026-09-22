/**
 * The one renderer for a run session: live tail while running, full result
 * (metrics, case detail, output) afterwards. Used by RunDock and nowhere else —
 * every run in the app shares this output path so nothing can hide behind a
 * modal any more.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Loader2, Square } from "lucide-react";
import { outcomeLabel } from "../../core/testStatus";
import type { RunSession } from "../../core/testRuns";

/** Long logs freeze the webview, so the panel renders the tail first. */
const OUTPUT_PREVIEW_LINES = 300;

export default function OutputPanel({
  session,
  nowMs,
  onCancel,
  onToast,
}: {
  session: RunSession;
  nowMs: number;
  onCancel?: () => void;
  onToast?: (type: "success" | "error", text: string) => void;
}) {
  const { t } = useTranslation("test");
  const preRef = useRef<HTMLPreElement>(null);
  const [outputExpanded, setOutputExpanded] = useState(false);
  const [onlyFailed, setOnlyFailed] = useState(false);
  const [caseSearch, setCaseSearch] = useState("");
  const result = session.result;
  const running = session.status === "running";

  const outputView = useMemo(() => {
    if (!result) return null;
    const lines = result.output.split("\n");
    const trimmed = !outputExpanded && lines.length > OUTPUT_PREVIEW_LINES;
    return {
      totalLines: lines.length,
      text: trimmed ? lines.slice(-OUTPUT_PREVIEW_LINES).join("\n") : result.output,
      trimmed,
    };
  }, [result, outputExpanded]);

  // Failures first, both across suites and within a suite — the thing you are
  // most likely to act on should be the first thing you see.
  const sortedSuites = useMemo(() => {
    if (!result) return [];
    const byStatus = (s: string) => (s === "failed" ? 0 : s === "skipped" ? 1 : 2);
    return [...result.suites]
      .map((suite) => ({
        ...suite,
        tests: [...suite.tests].sort((a, b) => byStatus(a.status) - byStatus(b.status)),
      }))
      .sort((a, b) => {
        const aFail = a.tests.some((c) => c.status === "failed") ? 0 : 1;
        const bFail = b.tests.some((c) => c.status === "failed") ? 0 : 1;
        return aFail - bFail;
      });
  }, [result]);

  const liveText = running
    ? session.lines.length > 0
      ? session.lines.join("\n")
      : t("liveWaiting")
    : "";

  useEffect(() => {
    const el = preRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [liveText]);

  return (
    <div className="tm-op">
      <div className="tm-result-head">
        <h3>
          {session.projectName}
          {session.source !== "manual" && (
            <span className="tm-chip tm-op-source">
              {t(`source.${session.source}`, { defaultValue: session.source })}
            </span>
          )}
        </h3>
        <span className={`tm-chip tm-chip-${session.status}`}>
          {outcomeLabel(t, session.status)}
        </span>
        {result && result.errorKind && (
          <span className="tm-chip" title={t("errorKindHint", { defaultValue: "程序判定的失败原因" })}>
            {t(`errorKind.${result.errorKind}`, { defaultValue: result.errorKind })}
          </span>
        )}
        {running && (
          <span className="tm-chip tm-chip-running" role="status">
            <Loader2 size={11} className="spin" />
            {t("elapsed", { sec: Math.max(0, Math.round((nowMs - session.startedAt) / 1000)) })}
          </span>
        )}
        {result && (
          <div className="tm-result-metrics">
            <span>
              {t("duration")} <strong>{(result.durationMs / 1000).toFixed(2)}s</strong>
            </span>
            <span className="tm-m-pass">
              {t("passed")} <strong>{result.passed}</strong>
            </span>
            <span className="tm-m-fail">
              {t("failed")} <strong>{result.failed}</strong>
            </span>
            <span className="tm-m-skip">
              {t("skipped")} <strong>{result.skipped}</strong>
            </span>
            <span>
              {t("total")} <strong>{result.totalTests}</strong>
            </span>
          </div>
        )}
        {running && onCancel && (
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={onCancel}
            title={t("cancelRunHint")}
          >
            <Square size={12} />
            {t("cancelRun")}
          </button>
        )}
        {session.error && (
          <span className="tm-op-error-text" role="alert">
            {session.error}
          </span>
        )}
      </div>

      {running && (
        <pre className="tm-output tm-output-live" ref={preRef}>
          {liveText}
        </pre>
      )}

      {result && (
        <>
          <div className="tm-result-toolbar">
            <span className="tm-output-label">{t("output")}</span>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() =>
                void navigator.clipboard
                  .writeText(result.output)
                  .then(() => onToast?.("success", t("copied")))
                  .catch((e) => onToast?.("error", String(e)))
              }
            >
              <Copy size={12} />
              {t("copy")}
            </button>
            {outputView && outputView.totalLines > OUTPUT_PREVIEW_LINES && (
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => setOutputExpanded((v) => !v)}
              >
                {outputView.trimmed
                  ? t("showAllLines", { count: outputView.totalLines })
                  : t("collapse")}
              </button>
            )}
          </div>
          {result.suites.length > 0 ? (
            <>
              <div className="tm-result-toolbar">
                <span className="tm-output-label">
                  {t("caseDetail", {
                    count: sortedSuites.reduce((n, s) => n + s.tests.length, 0),
                    files: sortedSuites.length,
                  })}
                </span>
                <input
                  type="search"
                  className="input-field tm-case-search"
                  value={caseSearch}
                  onChange={(e) => setCaseSearch(e.target.value)}
                  placeholder={t("caseSearchPlaceholder")}
                />
                <button
                  type="button"
                  className={`btn btn-small ${onlyFailed ? "btn-primary" : "btn-secondary"}`}
                  onClick={() => setOnlyFailed((v) => !v)}
                >
                  {t("onlyFailed")}
                </button>
              </div>
              <div className="tm-suites">
                {sortedSuites.map((suite) => {
                  const passedCases = suite.tests.filter((c) => c.status === "passed").length;
                  const failedCases = suite.tests.filter((c) => c.status === "failed").length;
                  const skippedCases = suite.tests.filter((c) => c.status === "skipped").length;
                  const search = caseSearch.trim().toLowerCase();
                  const cases = suite.tests.filter((c) => {
                    if (onlyFailed && c.status !== "failed") return false;
                    if (search && !c.name.toLowerCase().includes(search)) return false;
                    return true;
                  });
                  if (cases.length === 0) return null;
                  return (
                    <details key={suite.path} className="tm-suite" open={failedCases > 0}>
                      <summary className="tm-suite-head">
                        <span
                          className={`tm-suite-dot ${failedCases > 0 ? "is-fail" : "is-pass"}`}
                          aria-hidden
                        />
                        <span className="tm-suite-name" title={suite.path}>
                          {suite.name}
                        </span>
                        <span className="tm-suite-stat">
                          {suite.tests.length} · ✓{passedCases} ✗{failedCases} ○{skippedCases} ·{" "}
                          {suite.duration}ms
                        </span>
                      </summary>
                      <ul className="tm-cases">
                        {cases.map((entry) => (
                          <li key={entry.id} className={`tm-case tm-case-${entry.status}`}>
                            <span className="tm-case-mark" aria-hidden>
                              {entry.status === "passed"
                                ? "✓"
                                : entry.status === "failed"
                                  ? "✗"
                                  : "○"}
                            </span>
                            <span className="tm-case-name">{entry.name}</span>
                            <span className="tm-case-dur">{entry.duration}ms</span>
                            {entry.error && (
                              <pre className="tm-case-error">
                                {entry.error.stack
                                  ? `${entry.error.message}\n${entry.error.stack}`
                                  : entry.error.message}
                              </pre>
                            )}
                          </li>
                        ))}
                      </ul>
                    </details>
                  );
                })}
              </div>
            </>
          ) : (
            <p className="tm-hint">{t("noCaseDetail")}</p>
          )}
          {outputView && <pre className="tm-output">{outputView.text}</pre>}
        </>
      )}
    </div>
  );
}
