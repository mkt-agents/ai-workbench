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

const CHIP_KEYS: (keyof QuickAskChips)[] = ["workspace", "git", "dirty", "clipboard"];

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
        <div className="qa-chip-group" role="group" aria-label={t("contextGroup")}>
          {CHIP_KEYS.map((key) => (
            <button
              key={key}
              type="button"
              className={`qa-chip${chips[key] ? " on" : ""}`}
              aria-pressed={chips[key]}
              onClick={() => setChip(key, !chips[key])}
            >
              {t(`chip.${key}`)}
            </button>
          ))}
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
