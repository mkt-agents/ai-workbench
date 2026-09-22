import { useTranslation } from "react-i18next";
import { ClipboardPaste, Eye, FileText } from "lucide-react";
import type { QuickAskChips } from "../../core/types";

type Props = {
  chips: QuickAskChips;
  setChip: (key: keyof QuickAskChips, value: boolean) => void;
  onFillClipboard: () => void;
  hasSnippets: boolean;
  snippetOpen: boolean;
  onToggleSnippets: () => void;
  ctxOpen: boolean;
  onToggleCtx: () => void;
  ctxLoading: boolean;
  ctxPreview: string;
  ctxTruncated: boolean;
};

/** Context chips + quick tools (paste/snippets/preview) + context preview. */
export default function ContextBar(props: Props) {
  const { t } = useTranslation("quickask");
  const {
    chips, setChip, onFillClipboard, hasSnippets, snippetOpen, onToggleSnippets,
    ctxOpen, onToggleCtx, ctxLoading, ctxPreview, ctxTruncated,
  } = props;
  return (
    <>
      <div className="quick-ask-chips">
        <div className="qa-chip-group">
          <label className={`qa-chip ${chips.workspace ? "on" : ""}`}>
            <input
              type="checkbox"
              checked={chips.workspace}
              onChange={(e) => setChip("workspace", e.target.checked)}
            />
            {t("chip.workspace")}
          </label>
          <label className={`qa-chip ${chips.git ? "on" : ""}`}>
            <input type="checkbox" checked={chips.git} onChange={(e) => setChip("git", e.target.checked)} />
            {t("chip.git")}
          </label>
          <label className={`qa-chip ${chips.dirty ? "on" : ""}`}>
            <input type="checkbox" checked={chips.dirty} onChange={(e) => setChip("dirty", e.target.checked)} />
            {t("chip.dirty")}
          </label>
          <label className={`qa-chip ${chips.clipboard ? "on" : ""}`}>
            <input
              type="checkbox"
              checked={chips.clipboard}
              onChange={(e) => setChip("clipboard", e.target.checked)}
            />
            {t("chip.clipboard")}
          </label>
        </div>
        <div className="qa-tool-group">
          <button
            type="button"
            className="qa-tool-btn"
            onClick={onFillClipboard}
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
          <button
            type="button"
            className={`qa-tool-btn${ctxOpen ? " on" : ""}${ctxLoading ? " is-loading" : ""}`}
            onClick={onToggleCtx}
            disabled={!ctxPreview && !ctxLoading}
            title={ctxOpen ? t("hideContext") : t("showContext")}
          >
            {ctxLoading ? <span className="qa-spinner" /> : <Eye size={14} />}
          </button>
        </div>
      </div>

      {ctxTruncated && <div className="quick-ask-hint">{t("truncated")}</div>}
      {ctxOpen && ctxPreview && <pre className="quick-ask-ctx-preview">{ctxPreview}</pre>}
    </>
  );
}
