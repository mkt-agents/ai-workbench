import { useEffect, useDeferredValue, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowDown, BookmarkPlus, Check, Copy, Download, FolderGit2, Gauge, MessageCircle, MessageSquarePlus,
  Pencil, RefreshCw, RotateCcw,
} from "lucide-react";
import type { AiHubTurn } from "../../core/types";
import MarkdownView from "../MarkdownView";
import { stripThinking } from "./thinking";
import { MAX_HISTORY_TURNS } from "./config";
import { keepableAnswer } from "./keepable";
import { describeSource, describeTurn } from "./answerSource";
import {
  AgentApprovalBar, AgentApprovalCard, AgentLiveSteps, AgentPlanCard, AgentStepLog, AgentUndoButton,
} from "./AgentActivity";
import type { AskPanelProps } from "./AskPanel";

type Props = Pick<
  AskPanelProps,
  | "turns" | "pending" | "answer" | "busy" | "phase" | "modelName" | "projectLabel" | "source"
  | "canRegenerate" | "copied" | "snippetSaved" | "exportDone" | "selectedText" | "setSelectedText"
  | "agentSteps" | "agentPlan" | "agentApproval" | "stopping" | "onDecideAgent" | "onContinue" | "onRegenerate"
  | "onCopyAnswer" | "onSaveSnippet" | "onExport" | "onQuote" | "onEditQuestion" | "onRestartFrom"
>;

/** Within this many px of the bottom the thread keeps following new output. */
const STICK_THRESHOLD = 80;

/** Copy / save / continue / regenerate under one assistant message. */
function MessageActions({
  turn, index, isLast, props,
}: {
  turn: AiHubTurn;
  index: number;
  isLast: boolean;
  props: Props;
}) {
  const { t } = useTranslation("aihub");
  const { busy, canRegenerate, copied, snippetSaved, onContinue, onRegenerate, onCopyAnswer, onSaveSnippet } = props;
  const code = keepableAnswer(turn.content, turn.keep === "whole").kind === "code";
  return (
    <div className="qa-msg-actions">
      <button
        type="button"
        className="qa-mini-btn"
        onClick={() => onCopyAnswer(index)}
        title={code ? t("copyCode") : t("copy")}
      >
        {copied === index ? <Check size={12} className="qa-pop" /> : <Copy size={12} />}{" "}
        {copied === index ? t("copied") : code ? t("copyCode") : t("copy")}
      </button>
      <button
        type="button"
        className="qa-mini-btn"
        onClick={() => onSaveSnippet(index)}
        title={code ? t("saveCode") : t("saveSnippet")}
      >
        {snippetSaved === index ? <Check size={12} className="qa-pop" /> : <BookmarkPlus size={12} />}{" "}
        {snippetSaved === index ? t("savedSnippet") : code ? t("saveCode") : t("saveSnippet")}
      </button>
      {isLast && turn.limited && (
        <button type="button" className="qa-mini-btn" disabled={busy} onClick={onContinue}>
          {t("agent.continue")}
        </button>
      )}
      {isLast && (
        <button
          type="button"
          className="qa-mini-btn"
          disabled={busy || !canRegenerate}
          onClick={onRegenerate}
          title={t("regenerate")}
        >
          <RefreshCw size={12} /> {t("regenerate")}
        </button>
      )}
    </div>
  );
}

function SourceLine({ line, mode }: { line: string; mode: string | undefined }) {
  return (
    <div className={`qa-answer-source is-${mode ?? "restored"}`} title={line}>
      {/* 从会话里读回来的那一条只有数字，没有模式：用仪表盘图标区分 */}
      {mode === "project" ? (
        <FolderGit2 size={11} />
      ) : mode === "chat" ? (
        <MessageCircle size={11} />
      ) : (
        <Gauge size={11} />
      )}
      <span>{line}</span>
    </div>
  );
}

/** Transcript card: toolbar, messages, the round in flight, quote bar, jump-to-latest. */
export default function AskThread(props: Props) {
  const { t } = useTranslation("aihub");
  const {
    turns, pending, answer, busy, phase, modelName, projectLabel, source, exportDone, selectedText,
    setSelectedText, agentSteps, agentPlan, agentApproval, stopping, onDecideAgent, onExport, onQuote,
    onCopyAnswer, copied, onEditQuestion, onRestartFrom,
  } = props;
  const answerRef = useRef<HTMLDivElement | null>(null);
  /** Re-rendering a long streamed answer is expensive: let typing stay responsive. */
  const shownAnswer = useDeferredValue(answer);
  const rounds = Math.ceil(turns.filter((x) => !x.digest).length / 2) + (pending ? 1 : 0);
  const compressed = turns.some((x) => x.digest);
  /** Follow new output unless the user scrolled up to read. */
  const stickRef = useRef(true);
  const lastAssistant = turns.length - 1;
  const sourceLine = describeSource(source, t);
  /** A session read back from disk has numbers but no live `source`. */
  const lastTurn = turns[lastAssistant];
  const restoredLine =
    !sourceLine && lastTurn && lastTurn.role === "assistant" && !lastTurn.digest
      ? describeTurn(lastTurn, t)
      : null;
  /** Shown under the newest answer: live source if any, else the restored numbers. */
  const tailLine = sourceLine ?? restoredLine;

  const [showJump, setShowJump] = useState(false);
  /** Whether the newest output is in view — decides if a pending approval needs the dock. */
  const [atBottom, setAtBottom] = useState(true);

  // Only messages added while this chat is open play the entrance animation:
  // a loaded / replaced transcript (no question in flight) starts above the line.
  const prevTurnsRef = useRef(turns);
  const prevPendingRef = useRef(pending);
  const animateFromRef = useRef(turns.length);
  if (turns !== prevTurnsRef.current && !prevPendingRef.current) {
    animateFromRef.current = turns.length;
  }
  prevTurnsRef.current = turns;
  prevPendingRef.current = pending;
  const isNew = (i: number) => i >= animateFromRef.current;

  /** True while a smooth "jump to latest" scroll is running (its scroll events must not unstick). */
  const jumpingRef = useRef(false);

  const onThreadScroll = () => {
    const el = answerRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < STICK_THRESHOLD;
    setAtBottom(nearBottom);
    if (jumpingRef.current) {
      if (nearBottom) jumpingRef.current = false;
      return;
    }
    stickRef.current = nearBottom;
    setShowJump(busy && !nearBottom);
  };

  const jumpToLatest = () => {
    const el = answerRef.current;
    if (!el) return;
    stickRef.current = true;
    jumpingRef.current = true;
    setShowJump(false);
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    // Output may keep growing during the smooth scroll: settle at the real bottom.
    window.setTimeout(() => {
      if (!jumpingRef.current) return;
      jumpingRef.current = false;
      if (answerRef.current) answerRef.current.scrollTop = answerRef.current.scrollHeight;
    }, 600);
  };

  // A new question (or a loaded session) always jumps to the bottom.
  useLayoutEffect(() => {
    stickRef.current = true;
    setShowJump(false);
    const el = answerRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [pending, turns]);

  useEffect(() => {
    if (!busy) setShowJump(false);
  }, [busy]);

  /**
   * 内容长高之后跟着滚到底，并**重新判定是否真的在底部**。
   * 以前只依赖 scroll 事件更新 atBottom：审批卡 / 计划卡是插进去的，内容变高时不会产生 scroll 事件，
   * atBottom 就停在旧值 true —— 于是既不自动下滑，底部的吸底审批条也不出现，按钮全在视口外。
   */
  useLayoutEffect(() => {
    const el = answerRef.current;
    if (!el) return;
    if (stickRef.current && !jumpingRef.current) el.scrollTop = el.scrollHeight;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < STICK_THRESHOLD;
    setAtBottom(nearBottom);
    // 依赖用 `shownAnswer`（正文真正渲染的那份）：用 `answer` 会在文本变高之前就滚到底，
    // 随后内容长高又不在底部，`atBottom` 抖动、"回到底部"按钮闪一下。
  }, [shownAnswer, agentSteps, agentPlan, agentApproval]);

  useEffect(() => {
    const onSelect = () => {
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed || !answerRef.current) {
        setSelectedText("");
        return;
      }
      const node = sel.getRangeAt(0).commonAncestorContainer;
      if (!answerRef.current.contains(node)) {
        setSelectedText("");
        return;
      }
      setSelectedText(sel.toString());
    };
    document.addEventListener("selectionchange", onSelect);
    return () => document.removeEventListener("selectionchange", onSelect);
  }, [setSelectedText]);

  return (
    <div className={`qa-answer-card${busy ? " is-streaming" : ""}`}>
      <div className="qa-answer-toolbar">
        <span className="qa-thread-meta">
          {t(compressed ? "threadMetaCompressed" : "threadMeta", { n: rounds, max: MAX_HISTORY_TURNS })}
        </span>
        {turns.length > 0 && (
          <button type="button" className="qa-mini-btn" onClick={onExport} title={t("exportSession")}>
            {exportDone ? <Check size={13} /> : <Download size={13} />}{" "}
            {exportDone ? t("exported") : t("exportSession")}
          </button>
        )}
      </div>
      <div ref={answerRef} className="qa-thread" onScroll={onThreadScroll}>
        {turns.map((turn, i) => {
          if (turn.digest) {
            return (
              <div key={i} className="qa-digest" aria-label={t("digestLabel")}>
                <div className="qa-digest-label">{t("digestLabel")}</div>
                {turn.content}
              </div>
            );
          }
          if (turn.role === "user") {
            // Questions are actionable too: 复制 / 编辑 / 从这里重新开始 (the files the
            // agent already changed stay changed; only the conversation rewinds).
            return (
              <div key={i} className={`qa-msg is-user${isNew(i) ? " is-new" : ""}`} aria-label={t("you")}>
                <div className="qa-msg-text">{turn.content}</div>
                <div className="qa-msg-actions is-question">
                  <button
                    type="button"
                    className="qa-mini-btn"
                    onClick={() => onCopyAnswer(i)}
                    title={t("copyQuestion")}
                  >
                    {copied === i ? <Check size={12} className="qa-pop" /> : <Copy size={12} />}{" "}
                    {copied === i ? t("copied") : t("copyQuestion")}
                  </button>
                  <button
                    type="button"
                    className="qa-mini-btn"
                    disabled={busy}
                    onClick={() => onEditQuestion?.(i)}
                    title={t("editQuestion")}
                  >
                    <Pencil size={12} /> {t("edit")}
                  </button>
                  <button
                    type="button"
                    className="qa-mini-btn"
                    disabled={busy}
                    onClick={() => onRestartFrom?.(i)}
                    title={t("restartFrom")}
                  >
                    <RotateCcw size={12} /> {t("restartFromShort")}
                  </button>
                </div>
              </div>
            );
          }
          const isLast = i === lastAssistant && !pending;
          return (
            <div
              key={i}
              className={`qa-msg is-ai${isNew(i) ? " is-new" : ""}${isLast ? " is-last" : ""}`}
              aria-label={t("assistant")}
            >
              <MarkdownView text={stripThinking(turn.content)} />
              <MessageActions turn={turn} index={i} isLast={isLast} props={props} />
              {turn.plan && turn.plan.length > 0 && <AgentPlanCard plan={turn.plan} />}
              {turn.undoToken && <AgentUndoButton token={turn.undoToken} />}
              {turn.steps && turn.steps.length > 0 && <AgentStepLog steps={turn.steps} />}
              {isLast && tailLine && <SourceLine line={tailLine} mode={source?.mode} />}
            </div>
          );
        })}
        {/* Live turn keys match the transcript slots it lands in, so the DOM nodes
            survive the hand-off and the entrance animation doesn't replay. */}
        {pending && (
          <div key={turns.length} className="qa-msg is-user is-new" aria-label={t("you")}>
            {pending.question}
          </div>
        )}
        {pending && (
          <div key={turns.length + 1} className="qa-msg is-ai is-new is-live" aria-label={t("assistant")}>
            {answer ? (
              <>
                <MarkdownView text={shownAnswer} />
                {busy && <span className="qa-caret" aria-hidden />}
              </>
            ) : (
              busy && agentSteps.length === 0 && agentPlan.length === 0 && !agentApproval && (
                <div className="qa-thinking" role="status">
                  <span className="qa-thinking-dots" aria-hidden>
                    <i />
                    <i />
                    <i />
                  </span>
                  <span className="qa-thinking-text">
                    {stopping
                      ? t("phase.stopping")
                      : phase === "context"
                      ? t("phase.context", { repo: projectLabel || t("ctx.noRepo") })
                      : t("phase.waiting", { model: modelName })}
                  </span>
                </div>
              )
            )}
            {stopping && (answer || agentSteps.length > 0 || agentPlan.length > 0 || agentApproval) && (
              <div className="qa-thinking" role="status">
                <span className="qa-thinking-text">{t("phase.stopping")}</span>
              </div>
            )}
            <AgentPlanCard plan={agentPlan} />
            <AgentLiveSteps steps={agentSteps} />
            {agentApproval && <AgentApprovalCard approval={agentApproval} onDecide={onDecideAgent} />}
            {tailLine && answer && <SourceLine line={tailLine} mode={source?.mode} />}
          </div>
        )}
        {selectedText && !busy && (
          <div className="qa-selection-bar">
            <span className="qa-selection-text">{selectedText.trim().slice(0, 60)}</span>
            <button type="button" className="btn btn-secondary btn-small" onClick={onQuote}>
              <MessageSquarePlus size={13} /> {t("followUp")}
            </button>
          </div>
        )}
      </div>
      {agentApproval && !atBottom && (
        <AgentApprovalBar approval={agentApproval} onDecide={onDecideAgent} />
      )}
      {showJump && !agentApproval && (
        <button type="button" className="qa-jump-latest" onClick={jumpToLatest}>
          <ArrowDown size={12} /> {t("jumpLatest")}
        </button>
      )}
    </div>
  );
}
