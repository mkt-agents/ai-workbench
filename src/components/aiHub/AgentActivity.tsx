import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { ChevronRight, GitMerge } from "lucide-react";
import { writeStoredString } from "../../core/localState";
import { GIT_SUBTAB_KEY } from "../GitManager";
import { APPROVAL_FOCUS_KEY, type AgentApproval, type AgentDecision } from "./useAskStream";

/** Conflicts are resolved by the user on the commit page: jump there from the step. */
async function openCommitPage() {
  try {
    writeStoredString(GIT_SUBTAB_KEY, "commit");
    await invoke("open_main_tab", { tab: "git" });
    await emit("navigate-git-subtab", "commit");
  } catch {
    /* window jump is best-effort */
  }
}

type StepLike = { tool: string; status: string; summary: string; arg?: string; detail?: string };

/** Tools that drive the screen; the hub shows a "operating the computer" banner while they run. */
export const COMPUTER_TOOL_NAMES = ["screenshot", "mouse", "keyboard", "wait"];

/** Non-tool steps whose text is a fixed message → translated here, kept Chinese in `summary`. */
const NOTICE_KEYS: Record<string, string> = {
  conflict: "agent.conflictNote",
  notice: "agent.toolsUnsupported",
};

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
              {/* With the argument the row reads in the UI language; without it (older
                  saved turns) the Chinese summary stored by the backend is all there is.
                  Conflict / notice steps never have an argument — their text is a fixed
                  message, so it lives here instead of as a Rust string. */}
              <span className="qa-agent-summary" title={step.summary}>
                {step.arg?.trim() || (NOTICE_KEYS[step.tool] ? t(NOTICE_KEYS[step.tool]) : step.summary)}
              </span>
            </button>
            {detail && isOpen && <DetailBlock text={detail} />}
            {step.tool === "conflict" && (
              <button type="button" className="qa-agent-conflict-open" onClick={() => void openCommitPage()}>
                <GitMerge size={12} />
                {t("agent.openCommitPage")}
              </button>
            )}
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

/** mm:ss left before the backend auto-denies; "" when no deadline is known. */
function useDeadlineLeft(deadlineAt: number): string {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!deadlineAt) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [deadlineAt]);
  if (!deadlineAt) return "";
  const left = Math.max(0, deadlineAt - now);
  const m = Math.floor(left / 60000);
  const s = Math.floor((left % 60000) / 1000);
  return `${m}:${String(s).padStart(2, "0")}`;
}

/**
 * Sticky copy of the approval card for when it has scrolled out of view: a round
 * waiting on the user must stay actionable at the bottom of the transcript.
 */
export function AgentApprovalBar({
  approval,
  onDecide,
}: {
  approval: AgentApproval;
  onDecide: (decision: AgentDecision) => void;
}) {
  const { t } = useTranslation("aihub");
  const left = useDeadlineLeft(approval.deadlineAt);
  return (
    <div className="qa-approve-dock" role="alert">
      <span className="qa-approve-dock-text">{approval.summary}</span>
      {left && <span className="qa-approve-dock-count">{left}</span>}
      <button type="button" className="qa-agent-allow" onClick={() => onDecide("once")}>
        {t("agent.allow")}
      </button>
      <button type="button" className="qa-agent-allow-turn" onClick={() => onDecide("turn")} title={t("agent.allowTurnTip")}>
        {t("agent.allowTurn")}
      </button>
      <button type="button" className="qa-agent-deny" onClick={() => onDecide("deny")}>
        {t("agent.deny")}
      </button>
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
  const left = useDeadlineLeft(approval.deadlineAt);
  const [raise, setRaise] = useState(() => localStorage.getItem(APPROVAL_FOCUS_KEY) === "1");
  const toggleRaise = (on: boolean) => {
    setRaise(on);
    localStorage.setItem(APPROVAL_FOCUS_KEY, on ? "1" : "0");
    // Turning it on should pay off for the call being answered right now.
    if (on) void invoke("show_ai_hub").catch(() => {});
  };
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
      <div className="qa-agent-approve-foot">
        <span className="qa-agent-approve-count" title={t("agent.autoDenyTip")}>
          {left ? t("agent.autoDenyIn", { time: left }) : t("agent.autoDeny")}
        </span>
        <label className="qa-agent-approve-raise">
          <input
            type="checkbox"
            checked={raise}
            onChange={(e) => toggleRaise(e.target.checked)}
          />
          {t("agent.raiseWindow")}
        </label>
      </div>
    </div>
  );
}
