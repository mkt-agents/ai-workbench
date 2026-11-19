import { useId, useState, type ReactNode } from "react";
import { Check, ChevronDown, Download } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { AiHubContextInfo, AiHubTurn } from "../../core/types";
import type { AskMode } from "./route";
import type {
  AgentDecision, AgentPermission, AnswerSource, AskPhase, AgentApproval, AgentPlanStep, AgentStep,
} from "./useAskStream";
import type { OpenSuggestion } from "./localActions";
import type { PaletteItem } from "./harnessCommands";
import type { HarnessMode, HarnessTrack } from "./harness";
import type { AgentTask } from "./useAgentTask";
import AgentTaskCard from "./AgentTaskCard";
import AskThread from "./AskThread";
import AskEmpty from "./AskEmpty";
import AskErrorCard from "./AskErrorCard";
import AskComposer from "./AskComposer";
import ContextBudgetDetails from "./ContextBudgetDetails";
import { contextMeter } from "./contextUsage";

export type AskPanelProps = {
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
  /** Predicted next-step prompt shown above the composer; null = hidden. */
  suggestion?: string | null;
  onSuggestionPick?: (text: string) => void;
  onSuggestionDismiss?: () => void;
  onExample: (text: string) => void;
  /** Streaming text of the in-flight answer. */
  answer: string;
  /** Finished transcript (user / assistant pairs). */
  turns: AiHubTurn[];
  historyStart: number;
  legacySummary: string;
  contextInfo: AiHubContextInfo | null;
  loadingOlder: boolean;
  onLoadOlder: () => Promise<void>;
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
  /** Repo label used in project mode; null = no repo available at all. */
  projectLabel: string | null;
  agentSteps: AgentStep[];
  /** Checklist the model published with `update_plan` for this round. */
  agentPlan: AgentPlanStep[];
  agentApproval: AgentApproval | null;
  stopping: boolean;
  onDecideAgent: (decision: AgentDecision) => void;
  onContinue: () => void;
  agentTask: AgentTask | null;
  onResumeTask: () => void;
  hasSnippets: boolean;
  /** 插入 `/`：命令、技能、提示词统一由命令面板列出。 */
  onToggleSnippets: () => void;
  onPasteClipboard: () => void;
  /** Ctrl+V 时剪贴板里是文件（资源管理器复制）：走同一条附件管线。 */
  onPasteFiles: () => void;
  onAttachFile: () => void;
  onSend: () => void;
  /** Project picker, shown at the top-left of the composer. */
  repoControl: ReactNode;
  /** 钉住的任务模式 chip：独占一行，点一下解除；不钉时为 null。 */
  modeChip?: ReactNode;
  /** 当前钉住的模式 id（空态文案用）；与 modeChip 同步。 */
  pinnedMode?: HarnessMode | null;
  /** Code | Work 轨切换；与任务模式正交。 */
  track?: HarnessTrack;
  onTrack?: (track: HarnessTrack) => void;
  /** Model picker, shown at the bottom-right, beside send. */
  modelControl: ReactNode;
  /** `/` 命令与 `@` 文件引用共用的浮层；null = 没有候选。 */
  palette?: {
    items: PaletteItem[] | null;
    active: number;
    onMove: (delta: number) => void;
    onHover: (index: number) => void;
    onPick: (item: PaletteItem) => void;
    onPickActive: () => void;
    /** 二级列表标题（如 `/mode`）；有值即表示正在二级列表里。 */
    drillTitle?: string | null;
    onCloseDrill?: () => void;
  };
  /** What the AI may do in the repo for the next message. */
  permission: AgentPermission;
  onPermission: (next: AgentPermission) => void;
  /** 编辑档的写入也先看 diff（本地设置）。 */
  diffConfirm: boolean;
  onDiffConfirm: (on: boolean) => void;
  permissionMenuOpen: boolean;
  onTogglePermissionMenu: () => void;
  /** Non-null while the composer is an "open …" phrase (empty = nothing matched). */
  openList: OpenSuggestion[] | null;
  onRunOpen: (item: OpenSuggestion) => void;
  onStop: () => void;
  onRegenerate: () => void;
  onCopyAnswer: (index: number) => void;
  onSaveSnippet: (index: number) => void;
  onExport: () => void;
  onQuote: () => void;
  /** Put a past question back in the composer (context stays). */
  onEditQuestion?: (index: number) => void;
  /** Rewind the transcript to before this question and put it in the composer. */
  onRestartFrom?: (index: number) => void;
};

/** Answer stage (flex) + composer card; answer actions live with the answer. */
export default function AskPanel(props: AskPanelProps) {
  const { t } = useTranslation("aihub");
  const {
    turns, pending, busy, error, failedMode, onRetry, onDismissError, onExample, track, pinnedMode,
    agentTask, onResumeTask, exportDone, onExport,
  } = props;
  const { historyStart, legacySummary, contextInfo } = props;
  const [contextExpanded, setContextExpanded] = useState(false);
  const contextPanelId = useId();
  const showThread = turns.length > 0 || Boolean(pending) || busy || Boolean(legacySummary) || historyStart > 0;
  const rounds = Math.ceil((historyStart + turns.length) / 2) + (pending ? 1 : 0);
  const compressed = Boolean(legacySummary || contextInfo?.covered);
  const showContext = showThread && (compressed || contextInfo?.estimatedTokens != null || contextInfo?.degraded);
  const contextLabel = t(compressed ? "context.compressed" : "context.usage");
  const meter = contextMeter(contextInfo);
  const contextPercent = meter?.percent ?? null;
  const roundMeta = showThread && (
    <div className="qa-thread-meta">
      <span>{t("threadRound", { n: rounds })}</span>
      {showContext && (
        <button
          type="button"
          className="qa-mini-btn qa-context-toggle"
          aria-expanded={contextExpanded}
          aria-controls={contextPanelId}
          onClick={() => setContextExpanded((expanded) => !expanded)}
        >
          {contextLabel}
          {contextPercent !== null && (
            <span className="qa-context-usage" data-level={meter?.level} title={t("context.windowPercent")}>
              <span className="qa-context-meter" aria-hidden="true">
                <span style={{ width: `${Math.min(contextPercent, 100)}%` }} />
              </span>
              <span className="qa-context-percent">{contextPercent}%</span>
            </span>
          )}
          <ChevronDown size={13} aria-hidden="true" />
        </button>
      )}
    </div>
  );
  const contextPanel = showContext && (
    <div id={contextPanelId} className="qa-context-panel" role="region" aria-label={contextLabel} hidden={!contextExpanded}>
      {contextInfo && <ContextBudgetDetails info={contextInfo} />}
      {Boolean(contextInfo?.covered) && <span>{t("context.covered", { n: Math.floor((contextInfo?.covered ?? 0) / 2) })}</span>}
      {legacySummary && <span>{t("context.legacy")}</span>}
      {contextInfo?.degraded && <span>{t("context.failed")}</span>}
    </div>
  );
  const exportControl = (rounds > 0 || Boolean(legacySummary)) && (
    <button type="button" className="qa-mini-btn" onClick={onExport} title={t("exportSession")}>
      {exportDone ? <Check size={13} aria-hidden="true" /> : <Download size={13} aria-hidden="true" />}
      {exportDone ? t("exported") : t("exportSession")}
    </button>
  );
  return (
    <div className="qa-stage">
      {agentTask ? (
        <AgentTaskCard
          key={`${agentTask.sessionId}:${agentTask.taskId}`}
          task={agentTask}
          busy={busy}
          onResume={onResumeTask}
          roundMeta={roundMeta}
          contextPanel={contextPanel}
          exportControl={exportControl}
        />
      ) : showThread && (
        <div className="qa-answer-header">
          <div className="qa-answer-toolbar">
            {roundMeta}
            {exportControl}
          </div>
          {contextPanel}
        </div>
      )}
      {showThread ? (
        <AskThread {...props} />
      ) : (
        <AskEmpty
          onExample={onExample}
          track={track ?? "code"}
          pinnedMode={pinnedMode}
        />
      )}
      {error && (
        <AskErrorCard
          error={error}
          failedMode={failedMode}
          busy={busy}
          onRetry={onRetry}
          onDismiss={onDismissError}
        />
      )}
      <AskComposer {...props} />
    </div>
  );
}
