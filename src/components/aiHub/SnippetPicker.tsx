import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { Snippet } from "../../core/types";

type Props = {
  snippetQuery: string;
  setSnippetQuery: (q: string) => void;
  snippetChoices: Snippet[];
  onPick: (s: Snippet) => void;
  /** Opened by typing `/` in the composer: focus stays there so typing keeps filtering. */
  slashMode: boolean;
  active: number;
  onMoveActive: (delta: number) => void;
  setActiveIndex: (index: number) => void;
  onPickActive: () => void;
};

/** Inline snippet search + pick list; ↑↓/Enter work from both this box and the composer. */
export default function SnippetPicker(props: Props) {
  const { t } = useTranslation("aihub");
  const {
    snippetQuery, setSnippetQuery, snippetChoices, onPick, slashMode,
    active, onMoveActive, setActiveIndex, onPickActive,
  } = props;
  const listRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    listRef.current
      ?.querySelector(".qa-snippet-item.is-active")
      ?.scrollIntoView({ block: "nearest" });
  }, [active, snippetChoices]);

  const onNavKey = (e: React.KeyboardEvent) => {
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (snippetChoices.length === 0) return;
      e.preventDefault();
      onMoveActive(e.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if (e.key !== "Enter") return;
    if (e.shiftKey && !(e.ctrlKey || e.metaKey)) return;
    if (snippetChoices.length === 0) return;
    e.preventDefault();
    onPickActive();
  };

  return (
    <div className="ai-hub-snippets">
      <input
        className="input-field ai-hub-snippet-search"
        placeholder={t(slashMode ? "snippetFilter" : "snippetSearch")}
        value={snippetQuery}
        onChange={(e) => setSnippetQuery(e.target.value)}
        onKeyDown={onNavKey}
        // In slash mode the composer owns the caret; stealing it froze the filter.
        autoFocus={!slashMode}
      />
      <div ref={listRef} className="ai-hub-snippet-list" role="listbox" aria-label={t("snippets")}>
        {snippetChoices.map((s, i) => (
          <button
            key={s.id}
            type="button"
            role="option"
            aria-selected={i === active}
            className={`qa-snippet-item${i === active ? " is-active" : ""}`}
            onMouseEnter={() => setActiveIndex(i)}
            onClick={() => onPick(s)}
          >
            {s.name}
          </button>
        ))}
        {snippetChoices.length === 0 && <span className="ai-hub-hint">{t("snippetEmpty")}</span>}
      </div>
    </div>
  );
}
