import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { Check, ChevronRight, GitMerge, ListChecks, Loader2, Undo2 } from "lucide-react";
import type { AgentUndoFile } from "../../core/types";
import { useGlobalStore } from "../../core/store";
import { writeStoredString } from "../../core/localState";
import { GIT_SUBTAB_KEY } from "../GitManager";
import {
  APPROVAL_FOCUS_KEY,
  type AgentApproval,
  type AgentApprovalPatch,
  type AgentDecision,
  type AgentPlanStep,
} from "./useAskStream";

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
  retry: "agent.retryNote",
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
  // 只有带 detail 的行值得展开；不到 2 条就不给这个入口，省得白占一行
  const detailed = steps.flatMap((step, i) => (step.detail?.trim() ? [i] : []));
  const allOpen = detailed.length > 0 && detailed.every((i) => open.has(i));
  return (
    <ol className="qa-agent-steps">
      {detailed.length > 1 && (
        <li className="qa-agent-steps-all-row">
          <button
            type="button"
            className="qa-agent-steps-all"
            onClick={() => setOpen(allOpen ? new Set() : new Set(detailed))}
          >
            {allOpen ? t("agent.collapseAll") : t("agent.expandAll")}
          </button>
        </li>
      )}
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

/**
 * The model's own checklist (`update_plan`). Live while the round runs, folded into the
 * finished turn afterwards; it is not the answer, so it never replaces the text.
 */
export function AgentPlanCard({ plan }: { plan: AgentPlanStep[] }) {
  const { t } = useTranslation("aihub");
  if (plan.length === 0) return null;
  const done = plan.filter((s) => s.status === "done").length;
  return (
    <div className="qa-plan" role="group" aria-label={t("agent.planTitle")}>
      <div className="qa-plan-head">
        <ListChecks size={12} />
        <span className="qa-plan-title">{t("agent.planTitle")}</span>
        <span className="qa-plan-count">{t("agent.planProgress", { done, total: plan.length })}</span>
      </div>
      <ol className="qa-plan-steps">
        {plan.map((step, i) => (
          <li key={step.id || `${step.title}-${i}`} className={`qa-plan-step is-${step.status}`}>
            <span className="qa-plan-mark" aria-hidden>
              {step.status === "done" ? (
                <Check size={10} />
              ) : step.status === "in_progress" ? (
                <Loader2 size={10} className="spin" />
              ) : (
                <span className="qa-plan-dot" />
              )}
            </span>
            <span className="qa-plan-text">{step.title}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}

/** Last two segments of a path: enough to recognise the file, short enough for one row. */
function shortPath(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.slice(-2).join("/") || path;
}

/**
 * 「撤销这次改动」: restores files byte for byte from the snapshots the backend took before
 * each change. Clicking it first lists what the round touched, so one file can be restored
 * on its own or the whole round at once — every restored file leaves the list, which is why
 * a partial undo followed by 「全部还原」 only touches the rest. The button reports the
 * outcome in place (this window has no toast).
 */
export function AgentUndoButton({ token }: { token: string }) {
  const { t } = useTranslation("aihub");
  const invokeAgentUndo = useGlobalStore((s) => s.invokeAgentUndo);
  const invokeAgentUndoFiles = useGlobalStore((s) => s.invokeAgentUndoFiles);
  const [state, setState] = useState<"idle" | "picking" | "busy" | "done" | "failed">("idle");
  const [files, setFiles] = useState<AgentUndoFile[]>([]);
  const [detail, setDetail] = useState("");

  const open = async () => {
    setState("busy");
    try {
      const list = await invokeAgentUndoFiles(token);
      setFiles(list);
      setDetail("");
      setState("picking");
    } catch (e) {
      setDetail(String(e));
      setState("failed");
    }
  };

  const run = async (path?: string) => {
    setState("busy");
    try {
      const summary = await invokeAgentUndo(token, path);
      setDetail(summary);
      if (!path) {
        setFiles([]);
        setState("done");
        return;
      }
      const rest = files.filter((f) => f.path !== path);
      setFiles(rest);
      setState(rest.length > 0 ? "picking" : "done");
    } catch (e) {
      setDetail(String(e));
      setState("failed");
    }
  };

  if (state === "done" || state === "failed") {
    return (
      <div className={`qa-undo is-${state}`}>
        <div className="qa-undo-result">
          <span className="qa-undo-title">
            {state === "done" ? t("agent.undoDone") : t("agent.undoFailed")}
          </span>
          <span className="qa-undo-detail">{detail.replace(/^\[[A-Z_]+\]\s*/, "")}</span>
        </div>
      </div>
    );
  }

  if (state === "picking") {
    return (
      <div className="qa-undo is-picking">
        <div className="qa-undo-pick-head">
          <span>{t("agent.undoPick", { count: files.length })}</span>
          <button type="button" className="qa-undo-btn" onClick={() => void run()}>
            <Undo2 size={12} />
            {t("agent.undoAll")}
          </button>
        </div>
        <ul className="qa-undo-list">
          {files.map((file) => (
            <li key={file.path} className="qa-undo-file">
              <span className="qa-undo-kind">{t(`agent.undoKind.${file.kind}`)}</span>
              <span className="qa-undo-path" title={file.to ? `${file.path} → ${file.to}` : file.path}>
                {shortPath(file.path)}
                {file.kind === "move" && file.to ? ` → ${shortPath(file.to)}` : ""}
              </span>
              <button type="button" className="qa-undo-file-btn" onClick={() => void run(file.path)}>
                {t("agent.undoOne")}
              </button>
            </li>
          ))}
        </ul>
      </div>
    );
  }

  return (
    <div className={`qa-undo is-${state}`}>
      <button type="button" className="qa-undo-btn" disabled={state === "busy"} onClick={() => void open()}>
        <Undo2 size={12} />
        {state === "busy" ? t("agent.undoBusy") : t("agent.undo")}
      </button>
    </div>
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
      <span className="qa-approve-dock-acts">
        {left && <span className="qa-approve-dock-count">{left}</span>}
        <button type="button" className="qa-agent-allow" onClick={() => onDecide("once")}>
          {t("agent.allow")}
        </button>
        <button type="button" className="qa-agent-allow-turn" onClick={() => onDecide("turn")} title={t("agent.allowTurnTip")}>
          {t("agent.allowTurn")}
        </button>
        {approval.canRemember && (
          <button
            type="button"
            className="qa-agent-allow-always"
            onClick={() => onDecide("always")}
            title={t("agent.allowAlwaysTip")}
          >
            {t("agent.allowAlways")}
          </button>
        )}
        <button type="button" className="qa-agent-deny" onClick={() => onDecide("deny")}>
          {t("agent.deny")}
        </button>
      </span>
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
  const patches: AgentApprovalPatch[] | null =
    approval.patches && approval.patches.length > 1 ? approval.patches : null;
  return (
    <div className="qa-agent-approve" role="group" aria-label={t("agent.confirm")}>
      <div className="qa-agent-approve-title">{approval.summary}</div>
      {patches ? (
        <ol className="qa-agent-approve-patches">
          {patches.map((patch) => (
            <li key={patch.callId} className="qa-agent-approve-patch">
              <div className="qa-agent-approve-patch-title">{patch.summary}</div>
              {patch.preview && <DetailBlock text={patch.preview} diff={patch.previewKind === "diff"} />}
            </li>
          ))}
        </ol>
      ) : (
        approval.preview && <DetailBlock text={approval.preview} diff={approval.previewKind === "diff"} />
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
        {approval.canRemember && (
          <button
            type="button"
            className="qa-agent-allow-always"
            onClick={() => onDecide("always")}
            title={t("agent.allowAlwaysTip")}
          >
            {t("agent.allowAlways")}
          </button>
        )}
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
