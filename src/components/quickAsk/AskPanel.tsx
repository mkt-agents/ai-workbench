import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { QuickAskTurn } from "../../core/types";
import {
  AlertCircle, ArrowDown, BookmarkPlus, Bot, Check, ClipboardPaste, Copy, Download, ExternalLink,
  FileText, FolderGit2, MessageCircle, MessageSquarePlus, RefreshCw, Send, Sparkles, Square, X,
} from "lucide-react";
import MarkdownView from "../MarkdownView";
import { MAX_HISTORY_TURNS, QUICK_TASKS } from "./config";
import type { TaskKind } from "./config";
import type { AskMode, RouteOverride, RouteResult } from "./route";
import type { AnswerSource, AskPhase, AgentApproval, AgentStep } from "./useAskStream";
import type { OpenSuggestion } from "./localActions";
import { keepableAnswer } from "./keepable";

/** Empty-state starters: filled into the input (never auto-sent). */
const EXAMPLES: { key: string; task?: TaskKind }[] = [
  { key: "daily" },
  { key: "recent" },
  { key: "impact" },
  { key: "error", task: "debug" },
  { key: "polish", task: "polish" },
  { key: "open" },
  { key: "model" },
];
const INPUT_MAX_HEIGHT = 160;

type Props = {
  input: string;
  setInput: (v: string) => void;
  inputRef: { current: HTMLTextAreaElement | null };
  busy: boolean;
  phase: AskPhase;
  /** Selected model name, for the "thinking" line. */
  modelName: string;
  error: string | null;
  /** Mode of the failed request (null = no retry offered). */
  failedMode: AskMode | null;
  onRetry: (asChat: boolean) => void;
  onDismissError: () => void;
  /** Last question sent, recalled with ↑ in an empty input. */
  lastQuestion: string;
  onExample: (text: string, task?: TaskKind) => void;
  /** Streaming text of the in-flight answer. */
  answer: string;
  /** Finished transcript (user / assistant pairs). */
  turns: QuickAskTurn[];
  /** Question being answered right now. */
  pending: { question: string } | null;
  source: AnswerSource | null;
  canRegenerate: boolean;
  /** Transcript index of the assistant message just copied / saved. */
  copied: number | null;
  snippetSaved: number | null;
  exportDone: boolean;
  selectedText: string;
  setSelectedText: (v: string) => void;
  /** Live routing decision for the text currently in the composer. */
  route: RouteResult;
  routeOverride: RouteOverride;
  onRouteOverride: (v: RouteOverride) => void;
  routeMenuOpen: boolean;
  onToggleRouteMenu: () => void;
  /** Repo label used in project mode; null = no repo available at all. */
  projectLabel: string | null;
  task: TaskKind;
  onTask: (task: TaskKind) => void;
  onDaily: () => void;
  agentOn: boolean;
  onToggleAgent: () => void;
  agentSteps: AgentStep[];
  agentApproval: AgentApproval | null;
  stopping: boolean;
  onDecideAgent: (allow: boolean) => void;
  hasSnippets: boolean;
  snippetOpen: boolean;
  onToggleSnippets: () => void;
  onPasteClipboard: () => void;
  onSend: () => void;
  /** Non-null while the composer is an "open …" phrase (empty = nothing matched). */
  openList: OpenSuggestion[] | null;
  onRunOpen: (item: OpenSuggestion) => void;
  onStop: () => void;
  onRegenerate: () => void;
  onCopyAnswer: (index: number) => void;
  onSaveSnippet: (index: number) => void;
  onOpenDeepSeek: () => void;
  onExport: () => void;
  onQuote: () => void;
  onCopySelection: () => void;
};

const ROUTE_OPTIONS: RouteOverride[] = ["auto", "chat", "project"];
/** Within this many px of the bottom the thread keeps following new output. */
const STICK_THRESHOLD = 80;

/** Answer stage (flex) + composer card; answer actions live with the answer. */
export default function AskPanel(props: Props) {
  const { t } = useTranslation("quickask");
  const {
    input, setInput, inputRef, busy, phase, modelName, error, failedMode, onRetry, onDismissError,
    lastQuestion, onExample, answer, turns, pending, source, canRegenerate,
    copied, snippetSaved, exportDone, selectedText, setSelectedText,
    route, routeOverride, onRouteOverride, routeMenuOpen, onToggleRouteMenu, projectLabel,
    task, onTask, onDaily, agentOn, onToggleAgent, agentSteps, agentApproval, stopping, onDecideAgent,
    hasSnippets, snippetOpen, onToggleSnippets, onPasteClipboard,
    onSend, openList, onRunOpen, onStop, onRegenerate, onCopyAnswer, onSaveSnippet, onOpenDeepSeek,
    onExport, onQuote, onCopySelection,
  } = props;
  const answerRef = useRef<HTMLDivElement | null>(null);
  const [openHi, setOpenHi] = useState(0);
  const openListRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    setOpenHi(0);
  }, [input]);
  useEffect(() => {
    openListRef.current
      ?.querySelector(".qa-open-item.is-active")
      ?.scrollIntoView({ block: "nearest" });
  }, [openHi, openList]);
  const showThread = turns.length > 0 || Boolean(pending) || busy;
  const rounds = Math.ceil(turns.length / 2) + (pending ? 1 : 0);
  /** Follow new output unless the user scrolled up to read. */
  const stickRef = useRef(true);
  const lastAssistant = turns.length - 1;

  const [showJump, setShowJump] = useState(false);

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

  // Grow the composer with its content (no height transition — layout props never animate).
  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    const next = Math.min(el.scrollHeight, INPUT_MAX_HEIGHT);
    el.style.height = `${next}px`;
    el.style.overflowY = el.scrollHeight > INPUT_MAX_HEIGHT ? "auto" : "hidden";
  }, [input, inputRef]);

  // Flash the route pill when the live decision flips between chat and project.
  const [flashKey, setFlashKey] = useState(0);
  const prevModeRef = useRef(route.mode);
  useEffect(() => {
    if (prevModeRef.current !== route.mode) setFlashKey((k) => k + 1);
    prevModeRef.current = route.mode;
  }, [route.mode]);

  useLayoutEffect(() => {
    const el = answerRef.current;
    if (el && stickRef.current && !jumpingRef.current) el.scrollTop = el.scrollHeight;
  }, [answer]);

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

  const isProject = route.mode === "project";
  const pillLabel = isProject
    ? projectLabel
      ? t("route.pillProject", { label: projectLabel })
      : t("route.pillProjectNoRepo")
    : t("route.pillChat");
  const pillTip = `${t(`route.reason.${route.reason}`)}${routeOverride === "auto" ? "" : ` · ${t("route.overrideOnce")}`}`;

  const sourceLine = (() => {
    if (!source) return null;
    const bits: string[] = [];
    if (source.mode === "project") {
      bits.push(t("source.project"));
      if (source.summary.repos.length) bits.push(source.summary.repos.join("、"));
      const projectParts = source.summary.parts.filter((p) => p !== "clipboard");
      if (projectParts.length) bits.push(projectParts.map((p) => t(`part.${p}`)).join(" + "));
      else bits.push(t("source.noProjectData"));
    } else {
      bits.push(t("source.chat"));
    }
    if (source.summary.parts.includes("clipboard")) bits.push(t("part.clipboard"));
    if (source.task !== "none") bits.push(t(`taskShort.${source.task}`));
    bits.push(source.modelName);
    if (source.elapsedMs != null) {
      bits.push(
        source.elapsedMs >= 1000
          ? t("source.elapsedSec", { n: (source.elapsedMs / 1000).toFixed(1) })
          : t("source.elapsedMs", { n: source.elapsedMs })
      );
    }
    if (source.promptTokens != null || source.completionTokens != null) {
      const p = source.promptTokens ?? 0;
      const c = source.completionTokens ?? 0;
      bits.push(t("source.tokens", { n: p + c, in: p, out: c }));
    } else if (source.estimatedTokens != null) {
      bits.push(t("source.tokensEst", { n: source.estimatedTokens }));
    }
    return bits.join(" · ");
  })();

  return (
    <div className="qa-stage">
      {showThread ? (
        <div className={`qa-answer-card${busy ? " is-streaming" : ""}`}>
          <div className="qa-answer-toolbar">
            <span className="qa-thread-meta">
              {t("threadMeta", { n: rounds, max: MAX_HISTORY_TURNS })}
            </span>
            {turns.length > 0 && (
              <button
                type="button"
                className="qa-mini-btn"
                onClick={onExport}
                title={t("exportSession")}
              >
                {exportDone ? <Check size={13} /> : <Download size={13} />}{" "}
                {exportDone ? t("exported") : t("exportSession")}
              </button>
            )}
            <button
              type="button"
              className="qa-mini-btn"
              onClick={onOpenDeepSeek}
              title={t("openDeepseek")}
            >
              <ExternalLink size={13} /> {t("openDeepseek")}
            </button>
          </div>
          <div ref={answerRef} className="qa-thread" onScroll={onThreadScroll}>
            {turns.map((turn, i) =>
              turn.role === "user" ? (
                <div key={i} className={`qa-msg is-user${isNew(i) ? " is-new" : ""}`} aria-label={t("you")}>
                  {turn.content}
                </div>
              ) : (
                <div key={i} className={`qa-msg is-ai${isNew(i) ? " is-new" : ""}`} aria-label={t("assistant")}>
                  <MarkdownView text={turn.content} />
                  <div className="qa-msg-actions">
                    {(() => {
                      const kept = keepableAnswer(turn.content, turn.keep === "whole");
                      const code = kept.kind === "code";
                      return (
                        <>
                    <button
                      type="button"
                      className="qa-mini-btn"
                      onClick={() => onCopyAnswer(i)}
                      title={code ? t("copyCode") : t("copy")}
                    >
                      {copied === i ? <Check size={12} className="qa-pop" /> : <Copy size={12} />}{" "}
                      {copied === i ? t("copied") : code ? t("copyCode") : t("copy")}
                    </button>
                    <button
                      type="button"
                      className="qa-mini-btn"
                      onClick={() => onSaveSnippet(i)}
                      title={code ? t("saveCode") : t("saveSnippet")}
                    >
                      {snippetSaved === i ? <Check size={12} className="qa-pop" /> : <BookmarkPlus size={12} />}{" "}
                      {snippetSaved === i ? t("savedSnippet") : code ? t("saveCode") : t("saveSnippet")}
                    </button>
                    {i === lastAssistant && !pending && (
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
                        </>
                      );
                    })()}
                  </div>
                  {i === lastAssistant && !pending && sourceLine && (
                    <div className={`qa-answer-source is-${source?.mode}`} title={sourceLine}>
                      {source?.mode === "project" ? <FolderGit2 size={11} /> : <MessageCircle size={11} />}
                      <span>{sourceLine}</span>
                    </div>
                  )}
                </div>
              ),
            )}
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
                    <MarkdownView text={answer} />
                    {busy && <span className="qa-caret" aria-hidden />}
                  </>
                ) : (
                  busy && agentSteps.length === 0 && !agentApproval && (
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
                {stopping && (answer || agentSteps.length > 0 || agentApproval) && (
                  <div className="qa-thinking" role="status">
                    <span className="qa-thinking-text">{t("phase.stopping")}</span>
                  </div>
                )}
                {agentSteps.length > 0 && (
                  <ol className="qa-agent-steps">
                    {agentSteps.map((step, i) => (
                      <li key={`${step.tool}-${i}`} className={`qa-agent-step is-${step.status}`}>
                        <span className="qa-agent-tool">{t(`agent.tool.${step.tool}`, { defaultValue: step.tool })}</span>
                        <span className="qa-agent-summary">{step.summary}</span>
                      </li>
                    ))}
                  </ol>
                )}
                {agentApproval && (
                  <div className="qa-agent-approve" role="group" aria-label={t("agent.confirm")}>
                    <div className="qa-agent-approve-title">{agentApproval.summary}</div>
                    {agentApproval.preview && (
                      <pre className="qa-agent-preview">{agentApproval.preview}</pre>
                    )}
                    <div className="qa-agent-approve-actions">
                      <button type="button" className="qa-agent-allow" onClick={() => onDecideAgent(true)}>
                        {t("agent.allow")}
                      </button>
                      <button type="button" className="qa-agent-deny" onClick={() => onDecideAgent(false)}>
                        {t("agent.deny")}
                      </button>
                    </div>
                  </div>
                )}
                {sourceLine && answer && (
                  <div className={`qa-answer-source is-${source?.mode}`} title={sourceLine}>
                    {source?.mode === "project" ? <FolderGit2 size={11} /> : <MessageCircle size={11} />}
                    <span>{sourceLine}</span>
                  </div>
                )}
              </div>
            )}
            {selectedText && !busy && (
              <div className="qa-selection-bar">
                <span className="qa-selection-text">{selectedText.trim().slice(0, 60)}</span>
                <button type="button" className="btn btn-secondary btn-small" onClick={onQuote}>
                  <MessageSquarePlus size={13} /> {t("followUp")}
                </button>
                <button type="button" className="btn btn-secondary btn-small" onClick={onCopySelection}>
                  <Copy size={13} /> {t("copySelection")}
                </button>
              </div>
            )}
          </div>
          {showJump && (
            <button type="button" className="qa-jump-latest" onClick={jumpToLatest}>
              <ArrowDown size={12} /> {t("jumpLatest")}
            </button>
          )}
        </div>
      ) : (
        <div className="qa-empty">
          <Sparkles size={22} strokeWidth={1.5} className="qa-empty-icon" />
          <p className="qa-empty-title">{t("emptyHint")}</p>
          <p className="qa-empty-sub">{t("emptyHintSub")}</p>
          <div className="qa-examples">
            {EXAMPLES.map((ex) => (
              <button
                key={ex.key}
                type="button"
                className="qa-example"
                onClick={() => onExample(t(`examples.${ex.key}`), ex.task)}
              >
                {t(`examples.${ex.key}`)}
              </button>
            ))}
          </div>
        </div>
      )}

      {error && (
        <div className="qa-error-card" role="alert">
          <AlertCircle size={14} className="qa-error-icon" />
          <span className="qa-error-text" title={error}>{error}</span>
          {failedMode && !busy && (
            <span className="qa-error-actions">
              <button type="button" className="qa-mini-btn" onClick={() => onRetry(false)}>
                <RefreshCw size={12} /> {t("retry")}
              </button>
              {failedMode === "project" && (
                <button type="button" className="qa-mini-btn" onClick={() => onRetry(true)}>
                  <MessageCircle size={12} /> {t("retryAsChat")}
                </button>
              )}
            </span>
          )}
          <button type="button" className="qa-error-close" onClick={onDismissError} title={t("dismiss")}>
            <X size={12} />
          </button>
        </div>
      )}

      <div className="qa-compose-meta">
        <div className="qa-focus qa-route">
          <button
            type="button"
            className={`qa-route-pill is-${route.mode}${routeOverride !== "auto" ? " is-pinned" : ""}${
              isProject && !projectLabel ? " is-warn" : ""
            }`}
            onClick={onToggleRouteMenu}
            title={pillTip}
            aria-haspopup="listbox"
            aria-expanded={routeMenuOpen}
          >
            {flashKey > 0 && <span key={flashKey} className="qa-route-flash" aria-hidden />}
            {isProject ? <FolderGit2 size={12} /> : <MessageCircle size={12} />}
            <span className="qa-route-label">{pillLabel}</span>
            {routeOverride === "auto" && <span className="qa-route-auto">{t("route.autoTag")}</span>}
            <span className="qa-focus-caret">▾</span>
          </button>
          {routeMenuOpen && (
            <div className="qa-focus-pop is-left is-up qa-route-pop" role="listbox">
              <div className="qa-focus-pop-head">
                <span>{t("route.menuTitle")}</span>
              </div>
              <div className="qa-focus-list">
                {ROUTE_OPTIONS.map((opt) => {
                  const on = opt === routeOverride;
                  return (
                    <button
                      key={opt}
                      type="button"
                      role="option"
                      aria-selected={on}
                      className={`qa-focus-item${on ? " on" : ""}`}
                      onClick={() => {
                        onRouteOverride(opt);
                        onToggleRouteMenu();
                      }}
                    >
                      <span className={`qa-focus-check${on ? " on" : ""}`}>{on && <Check size={11} />}</span>
                      <span className="qa-focus-name">{t(`route.${opt}`)}</span>
                      <span className="qa-route-desc">{t(`route.${opt}Desc`)}</span>
                    </button>
                  );
                })}
              </div>
            </div>
          )}
        </div>
        {task !== "none" && (
          <span className="qa-task-tag">
            {t(`taskShort.${task}`)}
            <button
              type="button"
              className="qa-task-tag-x"
              onClick={() => onTask("none")}
              title={t("taskClear")}
            >
              <X size={11} />
            </button>
          </span>
        )}
      </div>

      <div className="qa-input-card">
        {openList && (
          <div ref={openListRef} className="qa-open-suggest" role="listbox" aria-label={t("openList")}>
            {openList.length === 0 ? (
              <div className="qa-open-empty">{t("openNone")}</div>
            ) : (
              openList.map((item, i) => {
                const showHead = i === 0 || openList[i - 1].group !== item.group;
                return (
                  <div key={item.id}>
                    {showHead && <div className="qa-open-head">{t(`openGroup.${item.group}`)}</div>}
                    <button
                      type="button"
                      role="option"
                      aria-selected={i === openHi}
                      className={`qa-open-item${i === openHi ? " is-active" : ""}`}
                      onMouseEnter={() => setOpenHi(i)}
                      onClick={() => onRunOpen(item)}
                    >
                      {item.label}
                    </button>
                  </div>
                );
              })
            )}
          </div>
        )}
        <textarea
          ref={inputRef}
          className="quick-ask-input"
          placeholder={
            openList
              ? t("openPlaceholder")
              : task !== "none"
                ? t(`taskPlaceholder.${task}`)
                : t("placeholder")
          }
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            // ↑ in an empty composer recalls the last question for editing.
            if (e.key === "ArrowUp" && !input && lastQuestion && !e.shiftKey) {
              e.preventDefault();
              setInput(lastQuestion);
              requestAnimationFrame(() => {
                const el = inputRef.current;
                if (el) el.setSelectionRange(el.value.length, el.value.length);
              });
              return;
            }
            if (openList && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
              e.preventDefault();
              if (openList.length === 0) return;
              const step = e.key === "ArrowDown" ? 1 : -1;
              setOpenHi((i) => (i + step + openList.length) % openList.length);
              return;
            }
            if (e.key !== "Enter") return;
            if (e.shiftKey && !(e.ctrlKey || e.metaKey)) return;
            e.preventDefault();
            if (busy) return;
            if (openList) {
              const pick = openList[openHi] ?? openList[0];
              if (pick) onRunOpen(pick);
              return;
            }
            onSend();
          }}
          rows={2}
        />
        <div className="qa-input-foot">
          <div className="qa-compose-tools">
            <button
              type="button"
              className="qa-tool-btn"
              onClick={onPasteClipboard}
              title={t("pasteClipboard")}
            >
              <ClipboardPaste size={14} />
            </button>
            {hasSnippets && (
              <button
                type="button"
                className={`qa-tool-btn${snippetOpen ? " on" : ""}`}
                onClick={onToggleSnippets}
                title={t("snippets")}
              >
                <FileText size={14} />
              </button>
            )}
            <span className="qa-compose-sep" aria-hidden />
            {QUICK_TASKS.map((k) => (
              <button
                key={k}
                type="button"
                className={`qa-task-btn${task === k ? " on" : ""}`}
                onClick={() => onTask(task === k ? "none" : k)}
                title={t(`task.${k}`)}
                aria-pressed={task === k}
              >
                {t(`taskShort.${k}`)}
              </button>
            ))}
            <span className="qa-compose-sep" aria-hidden />
            <button
              type="button"
              className="qa-task-btn qa-daily-btn"
              onClick={onDaily}
              disabled={busy}
              title={t("dailyTip")}
            >
              {t("dailyShort")}
            </button>
            <button
              type="button"
              className={`qa-task-btn qa-agent-btn${agentOn ? " on" : ""}`}
              onClick={onToggleAgent}
              disabled={busy}
              title={t("agent.tip")}
              aria-pressed={agentOn}
            >
              <Bot size={12} />
              {t("agent.label")}
            </button>
          </div>
          {/* One key, two states: morphs in place so the footer never shifts. */}
          <button
            type="button"
            className={`qa-send-btn${busy ? " is-busy" : ""}`}
            onClick={busy ? onStop : () => {
              if (openList) {
                const pick = openList[openHi] ?? openList[0];
                if (pick) onRunOpen(pick);
                return;
              }
              onSend();
            }}
            disabled={!busy && !input.trim()}
            title={busy ? `${t("stop")} (Esc)` : t("sendHintFull")}
            aria-label={busy ? t("stop") : t("send")}
          >
            <Send size={15} className="qa-send-icon" />
            <Square size={12} className="qa-stop-icon" />
          </button>
        </div>
      </div>
    </div>
  );
}
