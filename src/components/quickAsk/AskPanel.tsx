import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  BookmarkPlus, Check, Copy, Download, ExternalLink, MessageSquarePlus,
  RefreshCw, Send, Sparkles, Square,
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
  onQuote: () => void;
  onCopySelection: () => void;
};

/** Answer stage (flex) + composer card; answer actions live with the answer. */
export default function AskPanel(props: Props) {
  const { t } = useTranslation("quickask");
  const {
    input, setInput, inputRef, busy, error, answer, historyCount, canRegenerate,
    copied, snippetSaved, exportDone, selectedText, setSelectedText,
    onSend, onStop, onRegenerate, onCopyAnswer, onSaveSnippet, onOpenDeepSeek,
    onExport, onQuote, onCopySelection,
  } = props;
  const answerRef = useRef<HTMLDivElement | null>(null);
  const showAnswer = Boolean(answer || busy);

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
    <div className="qa-stage">
      {showAnswer ? (
        <div className={`qa-answer-card${busy ? " is-streaming" : ""}`}>
          <div className="qa-answer-toolbar">
            <button
              type="button"
              className="qa-mini-btn"
              disabled={busy || !canRegenerate}
              onClick={onRegenerate}
              title={t("regenerate")}
            >
              <RefreshCw size={13} /> {t("regenerate")}
            </button>
            <button
              type="button"
              className="qa-mini-btn"
              disabled={!answer}
              onClick={onCopyAnswer}
              title={t("copy")}
            >
              {copied ? <Check size={13} /> : <Copy size={13} />}{" "}
              {copied ? t("copied") : t("copy")}
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
            <button
              type="button"
              className="qa-mini-btn"
              onClick={onOpenDeepSeek}
              title={t("openDeepseek")}
            >
              <ExternalLink size={13} /> {t("openDeepseek")}
            </button>
          </div>
          <div ref={answerRef} className="quick-ask-answer">
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
        </div>
      ) : (
        <div className="qa-empty">
          <Sparkles size={22} strokeWidth={1.5} className="qa-empty-icon" />
          <p className="qa-empty-title">{t("emptyHint")}</p>
          <p className="qa-empty-sub">{t("emptyHintSub")}</p>
        </div>
      )}

      {error && <div className="quick-ask-error">{error}</div>}

      <div className="qa-input-card">
        <textarea
          ref={inputRef}
          className="quick-ask-input"
          placeholder={t("placeholder")}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key !== "Enter") return;
            if (e.shiftKey && !(e.ctrlKey || e.metaKey)) return;
            e.preventDefault();
            if (!busy) onSend();
          }}
          rows={3}
        />
        <div className="qa-input-foot">
          <span className="qa-input-hint">{t("sendHint")}</span>
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
    </div>
  );
}
