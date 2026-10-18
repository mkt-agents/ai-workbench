import type { ReactNode } from "react";
import type { AiHubTurn } from "../../core/types";
import type { TaskKind } from "./config";
import type { AskMode } from "./route";
import type { AgentPermission, AnswerSource, AskPhase, AgentApproval, AgentStep } from "./useAskStream";
import type { OpenSuggestion } from "./localActions";
import AskThread from "./AskThread";
import AskEmpty from "./AskEmpty";
import AskErrorCard from "./AskErrorCard";
import AskComposer from "./AskComposer";

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
  onExample: (text: string, task?: TaskKind) => void;
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
  agentApproval: AgentApproval | null;
  stopping: boolean;
  onDecideAgent: (allow: boolean) => void;
  onContinue: () => void;
  hasSnippets: boolean;
  snippetOpen: boolean;
  onToggleSnippets: () => void;
  onPasteClipboard: () => void;
  onAttachFile: () => void;
  onSend: () => void;
  /** Project picker, shown at the top-left of the composer. */
  repoControl: ReactNode;
  /** Model picker, shown at the bottom-right, beside send. */
  modelControl: ReactNode;
  /** What the AI may do in the repo for the next message. */
  permission: AgentPermission;
  onPermission: (next: AgentPermission) => void;
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
