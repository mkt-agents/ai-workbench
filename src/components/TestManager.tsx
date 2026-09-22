/**
 * Test management page shell.
 *
 * Owns only the page frame: the toast and the floating run dock. All run state
 * lives in src/core/testRuns.ts, so switching tabs mid-run unmounts this page
 * without losing the spinner, the cancel button or the live output.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { CheckCircle, FlaskConical, GitCompare, ShieldAlert, XCircle } from "lucide-react";
import ProjectsView from "./test/ProjectsView";
import ChangeReportsView from "./test/ChangeReportsView";
import VulnScanView from "./test/VulnScanView";
import RunDock from "./test/RunDock";
import { useGlobalStore } from "../core/store";
import "./TestManager.css";

type Toast = { type: "success" | "error"; text: string };
type View = "projects" | "reports" | "vuln";

export default function TestManager() {
  const { t } = useTranslation("test");
  const [toast, setToast] = useState<Toast | null>(null);
  const toastTimer = useRef<number | null>(null);
  const [reveal, setReveal] = useState<{ runId: string; nonce: number } | null>(null);
  const [view, setView] = useState<View>("projects");
  const [reportFocus, setReportFocus] = useState<string | null>(null);
  const testProjects = useGlobalStore((s) => s.testProjects);
  const loadTestProjects = useGlobalStore((s) => s.loadTestProjects);

  useEffect(
    () => () => {
      if (toastTimer.current) window.clearTimeout(toastTimer.current);
    },
    []
  );

  // The report view can be the landing view (via card action) before
  // ProjectsView ever mounted, so project loading lives at the shell.
  useEffect(() => {
    void loadTestProjects().catch(() => {});
  }, [loadTestProjects]);

  const showMsg = useCallback((type: Toast["type"], text: string) => {
    setToast({ type, text });
    if (toastTimer.current) window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 4000);
  }, []);

  return (
    <div className="tm-page">
      <nav className="tm-nav" aria-label={t("nav.aria")}>
        <button
          type="button"
          className={`tm-nav-item${view === "projects" ? " is-active" : ""}`}
          onClick={() => setView("projects")}
        >
          <FlaskConical size={13} />
          {t("nav.projects")}
        </button>
        <button
          type="button"
          className={`tm-nav-item${view === "reports" ? " is-active" : ""}`}
          onClick={() => setView("reports")}
        >
          <GitCompare size={13} />
          {t("nav.reports")}
        </button>
        <button
          type="button"
          className={`tm-nav-item${view === "vuln" ? " is-active" : ""}`}
          onClick={() => setView("vuln")}
        >
          <ShieldAlert size={13} />
          {t("nav.vuln")}
        </button>
      </nav>

      {view === "projects" && (
        <ProjectsView
          onToast={showMsg}
          onRevealRun={(runId) => setReveal({ runId, nonce: Date.now() })}
          onOpenReports={(projectId) => {
            setReportFocus(projectId);
            setView("reports");
          }}
        />
      )}
      {view === "reports" && (
        <ChangeReportsView
          projects={testProjects}
          focusProjectId={reportFocus}
          onToast={showMsg}
          onOpenVuln={() => setView("vuln")}
        />
      )}
      {view === "vuln" && <VulnScanView projects={testProjects} onToast={showMsg} />}

      <RunDock revealRunId={reveal} onToast={showMsg} />

      {toast && (
        <div className={`toast toast-${toast.type}`} role="status">
          <span className="toast-icon">
            {toast.type === "success" ? <CheckCircle size={14} /> : <XCircle size={14} />}
          </span>
          <span className="toast-text">{toast.text}</span>
        </div>
      )}
    </div>
  );
}
