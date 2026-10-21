import { useTranslation } from "react-i18next";
import type { MouseEvent } from "react";
import {
  History, Loader2, Maximize2, MessageSquarePlus, Minimize2, Pin, PinOff, Sparkles, Square, Terminal, X,
} from "lucide-react";
import type { AgentApproval, AgentDecision } from "./useAskStream";

export type AiHubMode = "ask" | "tools";

type Props = {
  mode: AiHubMode;
  onMode: (mode: AiHubMode) => void;
  busy: boolean;
  agentApproval: AgentApproval | null;
  onDecideAgent: (decision: AgentDecision) => void;
  onStop: () => void;
  historyOpen: boolean;
  onToggleHistory: () => void;
  onNewChat: () => void;
  pinned: boolean;
  onTogglePinned: () => void;
  maximized: boolean;
  onToggleMaximized: () => void;
  onClose: () => void;
};

/** Drag-region title bar: mode tabs, background-run pill, session buttons, pin / close. */
export default function AiHubHeader(props: Props) {
  const { t } = useTranslation("aihub");
  const {
    mode, onMode, busy, agentApproval, onDecideAgent, onStop, historyOpen, onToggleHistory, onNewChat,
    pinned, onTogglePinned, maximized, onToggleMaximized, onClose,
  } = props;
  // Double-click on blank title bar (drag regions only, never a button) toggles size.
  const onHeaderDoubleClick = (e: MouseEvent<HTMLElement>) => {
    const target = e.target as HTMLElement;
    if (target.closest("button, input, a, [role=tab]")) return;
    if (!target.closest("[data-tauri-drag-region]")) return;
    onToggleMaximized();
  };
  return (
    <header className="ai-hub-header" data-tauri-drag-region onDoubleClick={onHeaderDoubleClick}>
      <div className="ai-hub-header-lead" data-tauri-drag-region>
        <div className="ai-hub-title" data-tauri-drag-region>
          <Sparkles size={15} strokeWidth={1.75} />
          <span>{t("title")}</span>
        </div>
        <div className="qa-mode-tabs" role="tablist" aria-label={t("title")}>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "ask"}
            className={`qa-mode-btn${mode === "ask" ? " active" : ""}`}
            onClick={() => onMode("ask")}
            title={`${t("modeAsk")} (Alt+1)`}
          >
            <Sparkles size={11} /> {t("modeAsk")}
            {/* 切到工具箱后执行仍在后台跑：在页签上留一个在跑的标记 */}
            {busy && <span className="qa-mode-dot" aria-hidden />}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "tools"}
            className={`qa-mode-btn${mode === "tools" ? " active" : ""}`}
            onClick={() => onMode("tools")}
            title={`${t("modeTools")} (Alt+2)`}
          >
            <Terminal size={11} /> {t("modeTools")}
          </button>
        </div>
        {/* 问答回合在后台执行时切到了工具箱：这里必须有停止入口，
            否则 agent_run 会一直跑完（停止按钮原本只在问答视图的输入框里） */}
        {mode === "tools" && busy && agentApproval && (
          <div className="qa-busy-pill is-approval" role="group" aria-label={t("busyApproval")}>
            <Loader2 size={11} className="spin" />
            <span>{t("busyApproval")}</span>
            <button type="button" className="qa-busy-act is-allow" onClick={() => onDecideAgent("once")}>
              {t("agent.allow")}
            </button>
            <button
              type="button"
              className="qa-busy-act"
              onClick={() => onDecideAgent("turn")}
              title={t("agent.allowTurnTip")}
            >
              {t("agent.allowTurn")}
            </button>
            <button type="button" className="qa-busy-act" onClick={() => onDecideAgent("deny")}>
              {t("agent.deny")}
            </button>
          </div>
        )}
        {mode === "tools" && busy && !agentApproval && (
          <button type="button" className="qa-busy-pill" onClick={onStop} title={t("busyOtherTab")}>
            <Loader2 size={11} className="spin" />
            <span>{t("busyOtherTab")}</span>
            <span className="qa-busy-stop">
              <Square size={9} />
              {t("stop")}
            </span>
          </button>
        )}
      </div>
      <div className="ai-hub-header-actions">
        {mode === "ask" && (
          <div className="qa-header-sessions">
            <button
              type="button"
              className="qa-session-btn qa-session-btn-primary"
              onClick={onNewChat}
              disabled={busy}
              title={`${t("newChat")} (Ctrl+N)`}
            >
              <MessageSquarePlus size={13} />
              {t("newChat")}
            </button>
            <button
              type="button"
              className={`qa-session-btn qa-history-btn${historyOpen ? " on" : ""}`}
              onClick={onToggleHistory}
              title={t("history")}
              aria-expanded={historyOpen}
            >
              <History size={13} />
              {t("historyShort")}
            </button>
          </div>
        )}
        <button
          type="button"
          className={`btn-icon qa-pin-btn${pinned ? " is-active" : ""}`}
          onClick={onTogglePinned}
          title={pinned ? t("unpin") : t("pin")}
          aria-pressed={pinned}
        >
          {pinned ? <Pin size={14} /> : <PinOff size={14} />}
        </button>
        <button
          type="button"
          className="btn-icon qa-max-btn"
          onClick={onToggleMaximized}
          title={maximized ? t("restoreWindow") : t("maximizeWindow")}
          aria-label={maximized ? t("restoreWindow") : t("maximizeWindow")}
        >
          {maximized ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
        </button>
        <button type="button" className="btn-icon" onClick={onClose} title={t("close")}>
          <X size={16} />
        </button>
      </div>
    </header>
  );
}
