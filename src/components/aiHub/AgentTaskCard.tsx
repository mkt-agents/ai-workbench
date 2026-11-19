import { useId, useState, type ReactNode } from "react";
import { ChevronDown, Play } from "lucide-react";
import { useTranslation } from "react-i18next";
import { AgentPlanCard } from "./AgentActivity";
import type { AgentTask } from "./useAgentTask";

// Simplify canonical Windows paths for display only.
function displayTaskPath(path: string) {
  return path
    .replace(/^\\\\\?\\UNC\\/i, "\\\\")
    .replace(/^\\\\\?\\(?=[a-z]:\\)/i, "");
}

export default function AgentTaskCard({ task, busy, onResume, roundMeta, contextPanel, exportControl }: {
  task: AgentTask;
  busy: boolean;
  onResume: () => void;
  roundMeta: ReactNode;
  contextPanel: ReactNode;
  exportControl: ReactNode;
}) {
  const { t } = useTranslation("aihub");
  const [expanded, setExpanded] = useState(false);
  const detailsId = useId();
  const { verification } = task;
  const needsCheck = task.needsReview || task.actions.some((action) => action.status === "uncertain");
  const interrupted = task.status === "interrupted";
  const stopped = interrupted || task.status === "ended";
  const stopReason = task.reason && task.reason !== "completed" && (
    t(`task.reason.${task.reason}`, { defaultValue: task.reason })
  );
  // The journal contains side effects only. Plain Git reads need no verification badge.
  const showVerification = verification.status !== "unverified" || (stopped && task.actions.length > 0);
  const verificationLabel = t(`task.verification.${verification.status}`);

  return (
    <section className="qa-task-card" aria-label={t("task.title")}>
      <div className="qa-answer-toolbar qa-task-header">
        <div className="qa-task-meta">
          {roundMeta}
          <div className="qa-task-summary" role="status">
            <strong className="qa-task-state" data-task-state={task.status}>
              {t(`task.state.${task.status}`)}
            </strong>
            {needsCheck && <span className="qa-task-review">{t("task.reviewBadge")}</span>}
            {stopReason && !(needsCheck && task.reason === "review_required") && (
              <span className="qa-task-reason">{stopReason}</span>
            )}
            {showVerification && <span data-verification={verification.status}>{verificationLabel}</span>}
          </div>
        </div>
        <div className="qa-task-controls">
          <button
            className="qa-mini-btn qa-task-disclosure"
            type="button"
            aria-expanded={expanded}
            aria-controls={detailsId}
            onClick={() => setExpanded((open) => !open)}
          >
            {t("task.details")} <ChevronDown size={13} aria-hidden="true" />
          </button>
          {interrupted && (
            <button className="qa-mini-btn qa-task-resume" type="button" disabled={busy} onClick={onResume}>
              <Play size={12} aria-hidden="true" /> {t("task.resume")}
            </button>
          )}
          {exportControl}
        </div>
      </div>
      {contextPanel}
      <div
        id={detailsId}
        className="qa-task-details"
        role="region"
        aria-label={t("task.details")}
        hidden={!expanded}
        tabIndex={0}
      >
        {expanded && (
          <>
            {needsCheck && <p className="qa-task-review">{t("task.needsReview")}</p>}
            <dl className="qa-task-facts">
              <dt>{t("task.goal")}</dt>
              <dd className="qa-task-goal">{task.goal}</dd>
              {task.workspace && (
                <>
                  <dt>{t("task.workspace")}</dt>
                  <dd><code>{displayTaskPath(task.workspace)}</code></dd>
                </>
              )}
              {task.branch && (
                <>
                  <dt>{t("task.branch")}</dt>
                  <dd><code>{task.branch}</code></dd>
                </>
              )}
              <dt>{t("task.verificationLabel")}</dt>
              <dd>
                <span data-verification={verification.status}>
                  {verification.status === "unverified" ? t("task.noVerificationCommand") : verificationLabel}
                </span>
                {verification.command && (
                  <div className="qa-task-evidence">
                    <span>{t("task.checkCommand")}</span>
                    <code>{verification.command}</code>
                    {verification.exitCode !== null && <span>{t("task.exitCode", { code: verification.exitCode })}</span>}
                  </div>
                )}
              </dd>
            </dl>
            {task.plan.length > 0 && <AgentPlanCard plan={task.plan} />}
            {task.actions.length > 0 && (
              <ul className="qa-task-actions">
                {task.actions.map((action) => (
                  <li key={action.id}>
                    <span>{t(`task.action.${action.status}`)}</span>
                    <span title={action.detail ?? undefined}>{action.summary || action.name}</span>
                    {action.paths.map((path) => <code key={path}>{displayTaskPath(path)}</code>)}
                  </li>
                ))}
              </ul>
            )}
          </>
        )}
      </div>
    </section>
  );
}
