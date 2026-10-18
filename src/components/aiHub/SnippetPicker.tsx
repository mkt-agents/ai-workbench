import { useTranslation } from "react-i18next";
import type { Snippet } from "../../core/types";

type Props = {
  snippetQuery: string;
  setSnippetQuery: (q: string) => void;
  snippetChoices: Snippet[];
  onPick: (s: Snippet) => void;
};

/** Inline snippet search + pick list. */
export default function SnippetPicker(props: Props) {
  const { t } = useTranslation("aihub");
  const { snippetQuery, setSnippetQuery, snippetChoices, onPick } = props;
  return (
    <div className="ai-hub-snippets">
      <input
        className="input-field ai-hub-snippet-search"
        placeholder={t("snippetSearch")}
        value={snippetQuery}
        onChange={(e) => setSnippetQuery(e.target.value)}
        autoFocus
      />
      <div className="ai-hub-snippet-list">
        {snippetChoices.map((s) => (
          <button key={s.id} type="button" className="qa-snippet-item" onClick={() => onPick(s)}>
            {s.name}
          </button>
        ))}
        {snippetChoices.length === 0 && <span className="ai-hub-hint">{t("snippetEmpty")}</span>}
      </div>
    </div>
  );
}
