/**
 * Persistent run monitor for the whole test module.
 *
 * Renders every session in the testRuns store: one tab per run, so concurrent
 * batch runs no longer overwrite each other, and the panel floats above modal
 * overlays ("z-index": above .modal-overlay) so a run started from the change
 * report stays visible while the report is open.
 */
import { useEffect, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronUp, Loader2, X } from "lucide-react";
import {
  cancelRun,
  clearFinishedRuns,
  getRunsSnapshot,
  removeRun,
  subscribeRuns,
} from "../../core/testRuns";
import type { RunSession } from "../../core/testRuns";
import OutputPanel from "./OutputPanel";

/** Survives unmount: returning to the page should not fight the user's choice. */
let expandedMemory = true;

const STATUS_DOT: Record<string, string> = {
  running: "is-running",
  success: "is-pass",
  failed: "is-fail",
  error: "is-fail",
  cancelled: "is-idle",
  timeout: "is-fail",
};

export default function RunDock({
  revealRunId,
  onToast,
}: {
  revealRunId?: { runId: string; nonce: number } | null;
  onToast?: (type: "success" | "error", text: string) => void;
}) {
  const { t } = useTranslation("test");
  const sessions = useSyncExternalStore(subscribeRuns, getRunsSnapshot);
  const [expanded, setExpanded] = useState(expandedMemory);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());

  const runningCount = sessions.filter((s) => s.status === "running").length;

  useEffect(() => {
    expandedMemory = expanded;
  }, [expanded]);

  // The elapsed timers only tick while something is actually running.
  useEffect(() => {
    if (runningCount === 0) return;
    const id = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [runningCount]);

  // Newest first: the run you started last is the one you want to see.
  const ordered = [...sessions].reverse();
  const selected: RunSession | undefined =
    ordered.find((s) => s.runId === selectedId) ?? ordered[0];

  // A "view result" request from a project card selects and expands that run.
  useEffect(() => {
    if (!revealRunId) return;
    setSelectedId(revealRunId.runId);
    setExpanded(true);
  }, [revealRunId]);

  // A new run steals the view; a cancel elsewhere drops the selection quietly.
  const firstRunning = ordered.find((s) => s.status === "running");
  useEffect(() => {
    if (firstRunning && firstRunning.runId !== selectedId) {
      setSelectedId(firstRunning.runId);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [firstRunning?.runId]);

  if (sessions.length === 0) return null;

  return (
    <section className={`tm-dock${expanded ? " is-expanded" : ""}`} aria-label={t("dock.title")}>
      <header className="tm-dock-bar">
        <button
          type="button"
          className="tm-dock-toggle"
          onClick={() => setExpanded((v) => !v)}
          title={expanded ? t("dock.collapse") : t("dock.expand")}
        >
          {runningCount > 0 ? <Loader2 size={13} className="spin" /> : <ChevronUp size={13} />}
          <span>
            {runningCount > 0
              ? t("dock.running", { count: runningCount })
              : t("dock.finished", { count: sessions.length })}
          </span>
          {expanded ? <ChevronDown size={13} /> : <ChevronUp size={13} />}
        </button>
        <div className="tm-dock-tabs">
          {ordered.map((s) => (
            <button
              key={s.runId}
              type="button"
              className={`tm-dock-tab${selected?.runId === s.runId ? " is-active" : ""}`}
              onClick={() => {
                setSelectedId(s.runId);
                setExpanded(true);
              }}
              title={s.projectName}
            >
              <span className={`tm-dock-dot ${STATUS_DOT[s.status] ?? "is-idle"}`} aria-hidden />
              {s.projectName}
              {s.status !== "running" && (
                <button
                  type="button"
                  className="tm-dock-close"
                  aria-label={t("dock.close")}
                  onClick={(e) => {
                    e.stopPropagation();
                    removeRun(s.runId);
                  }}
                >
                  <X size={11} />
                </button>
              )}
            </button>
          ))}
        </div>
        {sessions.some((s) => s.status !== "running") && (
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={clearFinishedRuns}
          >
            {t("dock.clear")}
          </button>
        )}
      </header>
      {expanded && selected && (
        <div className="tm-dock-body">
          <OutputPanel
            key={selected.runId}
            session={selected}
            nowMs={nowMs}
            onToast={onToast}
            onCancel={
              selected.status === "running"
                ? () =>
                    void cancelRun(selected.projectId).then(
                      () => onToast?.("success", t("cancelRequested")),
                      (e) => onToast?.("error", String(e))
                    )
                : undefined
            }
          />
        </div>
      )}
    </section>
  );
}
