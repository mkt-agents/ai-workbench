import type { ReactNode } from "react";
import type { AiHubTurn } from "../../core/types";
import type { TaskKind } from "./config";
import type { DailyStyle } from "./dailyReport";
import type { AskMode } from "./route";
import type {
  AgentDecision, AgentPermission, AnswerSource, AskPhase, AgentApproval, AgentPlanStep, AgentStep,
} from "./useAskStream";
import type { OpenSuggestion } from "./localActions";
import type { PaletteItem } from "./harnessCommands";
import AskThread from "./AskThread";
import AskEmpty from "./AskEmpty";
import AskErrorCard from "./AskErrorCard";
import AskComposer from "./AskComposer";

/** Composer-side keyboard bridge to the open snippet list. */
export type SnippetNav = {
  /** False when the list is open but nothing matches — Enter then stays "ask the AI". */
  active: boolean;
  onMove: (delta: number) => void;
  onPick: () => void;
};

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
  task: TaskKind;
  onTask: (task: TaskKind) => void;
  onDaily: () => void;
  agentSteps: AgentStep[];
  /** Checklist the model published with `update_plan` for this round. */
  agentPlan: AgentPlanStep[];
  agentApproval: AgentApproval | null;
  stopping: boolean;
  onDecideAgent: (decision: AgentDecision) => void;
  onContinue: () => void;
  hasSnippets: boolean;
  snippetOpen: boolean;
  snippetNav: SnippetNav | null;
  onToggleSnippets: () => void;
  onPasteClipboard: () => void;
  onAttachFile: () => void;
  onSend: () => void;
  /** Project picker, shown at the top-left of the composer. */
  repoControl: ReactNode;
  /** Model picker, shown at the bottom-right, beside send. */
  modelControl: ReactNode;
  /** Persona / temperature profile picker, left of the permission pill. */
  profileControl?: ReactNode;
  /** 任务模式胶囊（对话 / 代码 / 文档 / 数据 / 调研 / 运维）。 */
  modeControl?: ReactNode;
  /** `/` 命令与 `@` 文件引用共用的浮层；null = 没有候选。 */
  palette?: {
    items: PaletteItem[] | null;
    active: number;
    onMove: (delta: number) => void;
    onHover: (index: number) => void;
    onPick: (item: PaletteItem) => void;
    onPickActive: () => void;
  };
  /** What the AI may do in the repo for the next message. */
  permission: AgentPermission;
  onPermission: (next: AgentPermission) => void;
  /** 编辑档的写入也先看 diff（本地设置）。 */
  diffConfirm: boolean;
  onDiffConfirm: (on: boolean) => void;
  permissionMenuOpen: boolean;
  onTogglePermissionMenu: () => void;
  /** Daily report format, picked from the caret beside 日报. */
  dailyStyle: DailyStyle;
  onDailyStyle: (style: DailyStyle) => void;
  dailyMenuOpen: boolean;
  onToggleDailyMenu: () => void;
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
  const { turns, pending, busy, error, failedMode, onRetry, onDismissError, onExample } = props;
  const showThread = turns.length > 0 || Boolean(pending) || busy;
  return (
    <div className="qa-stage">
      {showThread ? <AskThread {...props} /> : <AskEmpty onExample={onExample} />}
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
