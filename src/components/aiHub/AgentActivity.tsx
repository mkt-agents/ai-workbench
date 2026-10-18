import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { AgentApproval } from "./useAskStream";

type StepLike = { tool: string; status: string; summary: string };

function StepList({ steps }: { steps: StepLike[] }) {
  const { t } = useTranslation("aihub");
  return (
    <ol className="qa-agent-steps">
      {steps.map((step, i) => (
        <li key={`${step.tool}-${i}`} className={`qa-agent-step is-${step.status}`}>
          <span className="qa-agent-tool">{t(`agent.tool.${step.tool}`, { defaultValue: step.tool })}</span>
          <span className="qa-agent-summary">{step.summary}</span>
        </li>
      ))}
    </ol>
  );
}

/** Live steps of the round in flight. */
export function AgentLiveSteps({ steps }: { steps: StepLike[] }) {
  if (steps.length === 0) return null;
  return <StepList steps={steps} />;
}

/** Steps of a finished round, folded behind a toggle. */
export function AgentStepLog({ steps }: { steps: StepLike[] }) {
  const { t } = useTranslation("aihub");
  const [open, setOpen] = useState(false);
  if (steps.length === 0) return null;
  return (
    <div className="qa-agent-log">
      <button
        type="button"
        className="qa-agent-log-toggle"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        {t("agent.steps", { n: steps.length })}
      </button>
      {open && <StepList steps={steps} />}
    </div>
  );
}

/** Inline allow / deny card for a write or shell command awaiting the user. */
export function AgentApprovalCard({
  approval,
  onDecide,
}: {
  approval: AgentApproval;
  onDecide: (allow: boolean) => void;
}) {
  const { t } = useTranslation("aihub");
  return (
    <div className="qa-agent-approve" role="group" aria-label={t("agent.confirm")}>
      <div className="qa-agent-approve-title">{approval.summary}</div>
      {approval.preview && <pre className="qa-agent-preview">{approval.preview}</pre>}
      <div className="qa-agent-approve-actions">
        <button type="button" className="qa-agent-allow" onClick={() => onDecide(true)}>
          {t("agent.allow")}
        </button>
        <button type="button" className="qa-agent-deny" onClick={() => onDecide(false)}>
          {t("agent.deny")}
        </button>
      </div>
    </div>
  );
}
