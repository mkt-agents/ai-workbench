import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronRight } from "lucide-react";
import type { AgentApproval, AgentDecision } from "./useAskStream";

type StepLike = { tool: string; status: string; summary: string; detail?: string };

/** Tools that drive the screen; the hub shows a "operating the computer" banner while they run. */
export const COMPUTER_TOOL_NAMES = ["screenshot", "mouse", "keyboard", "wait"];

function StepList({ steps }: { steps: StepLike[] }) {
  const { t } = useTranslation("aihub");
  const [open, setOpen] = useState<Set<number>>(new Set());
  const toggle = (i: number) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  return (
    <ol className="qa-agent-steps">
      {steps.map((step, i) => {
        const detail = step.detail?.trim();
        const isOpen = open.has(i);
        return (
          <li key={`${step.tool}-${i}`} className={`qa-agent-step is-${step.status}`}>
            <button
              type="button"
              className="qa-agent-step-row"
              onClick={() => detail && toggle(i)}
              disabled={!detail}
              aria-expanded={detail ? isOpen : undefined}
              title={detail ? t("agent.detailToggle") : undefined}
            >
              <ChevronRight size={11} className={`qa-agent-caret${isOpen ? " is-open" : ""}${detail ? "" : " is-empty"}`} />
              <span className="qa-agent-tool">{t(`agent.tool.${step.tool}`, { defaultValue: step.tool })}</span>
              <span className="qa-agent-summary">{step.summary}</span>
            </button>
            {detail && isOpen && <DetailBlock text={detail} />}
          </li>
        );
      })}
    </ol>
  );
}

/** Unified diff lines get +/- colours; anything else renders as plain monospace. */
function DetailBlock({ text, diff }: { text: string; diff?: boolean }) {
  const looksDiff = diff ?? /^(--- |\+\+\+ |@@ )/m.test(text);
  if (!looksDiff) return <pre className="qa-agent-detail">{text}</pre>;
  return (
    <pre className="qa-agent-detail is-diff">
      {text.split("\n").map((line, i) => {
        const kind = line.startsWith("@@")
          ? "hunk"
          : line.startsWith("+++") || line.startsWith("---")
            ? "meta"
            : line.startsWith("+")
              ? "add"
              : line.startsWith("-")
                ? "del"
                : "ctx";
        return (
          <span key={i} className={`qa-diff-line is-${kind}`}>
            {line || " "}
            {"\n"}
          </span>
        );
      })}
    </pre>
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

/** Approval card for a change that needs the user: diff preview + allow once / for this turn / deny. */
export function AgentApprovalCard({
  approval,
  onDecide,
}: {
  approval: AgentApproval;
  onDecide: (decision: AgentDecision) => void;
}) {
  const { t } = useTranslation("aihub");
  return (
    <div className="qa-agent-approve" role="group" aria-label={t("agent.confirm")}>
      <div className="qa-agent-approve-title">{approval.summary}</div>
      {approval.preview && (
        <DetailBlock text={approval.preview} diff={approval.previewKind === "diff"} />
      )}
      <div className="qa-agent-approve-actions">
        <button type="button" className="qa-agent-allow" onClick={() => onDecide("once")}>
          {t("agent.allow")}
        </button>
        <button
          type="button"
          className="qa-agent-allow-turn"
          onClick={() => onDecide("turn")}
          title={t("agent.allowTurnTip")}
        >
          {t("agent.allowTurn")}
        </button>
        <button type="button" className="qa-agent-deny" onClick={() => onDecide("deny")}>
          {t("agent.deny")}
        </button>
      </div>
    </div>
  );
}
