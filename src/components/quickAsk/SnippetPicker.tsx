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
  const { t } = useTranslation("quickask");
  const { snippetQuery, setSnippetQuery, snippetChoices, onPick } = props;
  return (
    <div className="quick-ask-snippets">
      <input
        className="input-field quick-ask-snippet-search"
        placeholder={t("snippetSearch")}
        value={snippetQuery}
        onChange={(e) => setSnippetQuery(e.target.value)}
        autoFocus
      />
      <div className="quick-ask-snippet-list">
        {snippetChoices.map((s) => (
          <button key={s.id} type="button" className="qa-snippet-item" onClick={() => onPick(s)}>
            {s.name}
          </button>
        ))}
        {snippetChoices.length === 0 && <span className="quick-ask-hint">{t("snippetEmpty")}</span>}
      </div>
    </div>
  );
}
