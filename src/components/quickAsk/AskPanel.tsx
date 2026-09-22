import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  BookmarkPlus, Check, Copy, Download, ExternalLink, MessageSquarePlus,
  RefreshCw, Send, Square,
} from "lucide-react";
import MarkdownView from "../MarkdownView";

type Props = {
  input: string;
  setInput: (v: string) => void;
  inputRef: { current: HTMLTextAreaElement | null };
  busy: boolean;
  error: string | null;
  answer: string;
  historyCount: number;
  canRegenerate: boolean;
  copied: boolean;
  snippetSaved: boolean;
  exportDone: boolean;
  selectedText: string;
  setSelectedText: (v: string) => void;
  onSend: () => void;
  onStop: () => void;
  onRegenerate: () => void;
  onCopyAnswer: () => void;
  onSaveSnippet: () => void;
  onOpenDeepSeek: () => void;
  onExport: () => void;
  onNewChat: () => void;
  onQuote: () => void;
  onCopySelection: () => void;
};

/** Input card + action bar + streaming answer area with quote-follow-up bar. */
export default function AskPanel(props: Props) {
  const { t } = useTranslation("quickask");
  const {
    input, setInput, inputRef, busy, error, answer, historyCount, canRegenerate,
    copied, snippetSaved, exportDone, selectedText, setSelectedText,
    onSend, onStop, onRegenerate, onCopyAnswer, onSaveSnippet, onOpenDeepSeek,
    onExport, onNewChat, onQuote, onCopySelection,
  } = props;
  const answerRef = useRef<HTMLDivElement | null>(null);

  // Track text selected inside the answer area for the quote-follow-up bar.
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
    <>
      <div className="qa-input-card">
        <textarea
          ref={inputRef}
          className="quick-ask-input"
          placeholder={t("placeholder")}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== "Enter") return;
            // Ctrl/Cmd+Enter always sends; plain Enter sends too, Shift+Enter = newline.
            if (e.shiftKey && !(e.ctrlKey || e.metaKey)) return;
            e.preventDefault();
            if (!busy) onSend();
          }}
          rows={3}
        />
        <div className="qa-input-foot">
          {busy ? (
            <button
              type="button"
              className="qa-stop-btn"
              onClick={onStop}
              title={t("stop")}
            >
              <Square size={13} />
              <span>{t("stop")}</span>
            </button>
          ) : (
            <button
              type="button"
              className="qa-send-btn"
              onClick={onSend}
              disabled={!input.trim()}
              title={t("send")}
            >
              <Send size={15} />
            </button>
          )}
        </div>
      </div>

      <div className="quick-ask-actions">
        <button
          type="button"
          className="qa-mini-btn"
          disabled={busy || !canRegenerate}
          onClick={onRegenerate}
          title={t("regenerate")}
        >
          <RefreshCw size={13} /> {t("regenerate")}
        </button>
        <button type="button" className="qa-mini-btn" disabled={!answer} onClick={onCopyAnswer}>
          {copied ? <Check size={13} /> : <Copy size={13} />} {copied ? t("copied") : t("copy")}
        </button>
        <button
          type="button"
          className="qa-mini-btn"
          disabled={!answer}
          onClick={onSaveSnippet}
          title={t("saveSnippet")}
        >
          {snippetSaved ? <Check size={13} /> : <BookmarkPlus size={13} />}{" "}
          {snippetSaved ? t("savedSnippet") : t("saveSnippet")}
        </button>
        <button type="button" className="qa-mini-btn" onClick={onOpenDeepSeek}>
          <ExternalLink size={13} /> {t("openDeepseek")}
        </button>
        {historyCount > 0 && (
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
        {(historyCount > 0 || answer) && (
          <button
            type="button"
            className="qa-mini-btn"
            disabled={busy}
            onClick={onNewChat}
            title={t("newChat")}
          >
            <MessageSquarePlus size={13} /> {t("newChat")}
          </button>
        )}
      </div>

      {error && <div className="quick-ask-error">{error}</div>}

      {(answer || busy) && (
        <div ref={answerRef} className={`quick-ask-answer${busy ? " is-streaming" : ""}`}>
          <MarkdownView text={answer} />
          {busy && <span className="qa-caret">▍</span>}
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
      )}
    </>
  );
}
