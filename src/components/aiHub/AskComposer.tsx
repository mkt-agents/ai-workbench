import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ClipboardPaste, FilePlus2, FileText, Send, Sparkles, Square, X } from "lucide-react";
import { QUICK_TASKS } from "./config";
import type { OpenSuggestion } from "./localActions";
import PermissionPicker from "./PermissionPicker";
import type { AskPanelProps } from "./AskPanel";

type Props = Pick<
  AskPanelProps,
  | "input" | "setInput" | "inputRef" | "busy" | "lastQuestion" | "suggestion" | "onSuggestionPick"
  | "onSuggestionDismiss" | "task" | "onTask" | "onDaily" | "hasSnippets" | "snippetOpen"
  | "onToggleSnippets" | "onPasteClipboard" | "onAttachFile" | "onSend" | "repoControl" | "modelControl"
  | "permission" | "onPermission" | "permissionMenuOpen" | "onTogglePermissionMenu"
  | "openList" | "onRunOpen" | "onStop"
>;

const INPUT_MAX_HEIGHT = 160;

/** "open …" matches: keyboard-highlighted list above the textarea. */
function OpenSuggestList({
  items, active, setActive, onRun, listRef,
}: {
  items: OpenSuggestion[];
  active: number;
  setActive: (i: number) => void;
  onRun: (item: OpenSuggestion) => void;
  listRef: { current: HTMLDivElement | null };
}) {
  const { t } = useTranslation("aihub");
  return (
    <div ref={listRef} className="qa-open-suggest" role="listbox" aria-label={t("openList")}>
      {items.length === 0 ? (
        <div className="qa-open-empty">{t("openNone")}</div>
      ) : (
        items.map((item, i) => {
          const showHead = i === 0 || items[i - 1].group !== item.group;
          return (
            <div key={item.id}>
              {showHead && <div className="qa-open-head">{t(`openGroup.${item.group}`)}</div>}
              <button
                type="button"
                role="option"
                aria-selected={i === active}
                className={`qa-open-item${i === active ? " is-active" : ""}`}
                onMouseEnter={() => setActive(i)}
                onClick={() => onRun(item)}
              >
                {item.label}
              </button>
            </div>
          );
        })
      )}
    </div>
  );
}

/** Input card: repo picker, open list, next-step suggestion, textarea, tools, route / agent / model / send. */
export default function AskComposer(props: Props) {
  const { t } = useTranslation("aihub");
  const {
    input, setInput, inputRef, busy, lastQuestion, suggestion, onSuggestionPick, onSuggestionDismiss,
    task, onTask, onDaily, hasSnippets, snippetOpen, onToggleSnippets, onPasteClipboard, onAttachFile,
    onSend, repoControl, modelControl, permission, onPermission, permissionMenuOpen, onTogglePermissionMenu,
    openList, onRunOpen, onStop,
  } = props;
  const writes = permission !== "read";
  const [openHi, setOpenHi] = useState(0);
  const openListRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    setOpenHi(0);
  }, [input]);
  useEffect(() => {
    openListRef.current
      ?.querySelector(".qa-open-item.is-active")
      ?.scrollIntoView({ block: "nearest" });
  }, [openHi, openList]);

  // Grow the composer with its content (no height transition — layout props never animate).
  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    const next = Math.min(el.scrollHeight, INPUT_MAX_HEIGHT);
    el.style.height = `${next}px`;
    el.style.overflowY = el.scrollHeight > INPUT_MAX_HEIGHT ? "auto" : "hidden";
  }, [input, inputRef]);

  /** Enter / send button: run the highlighted "open …" match, else send. */
  const submit = () => {
    if (openList) {
      const pick = openList[openHi] ?? openList[0];
      if (pick) onRunOpen(pick);
      return;
    }
    onSend();
  };

  return (
    <div className="qa-input-card">
      <div className="qa-compose-meta">{repoControl}</div>
      {openList && (
        <OpenSuggestList
          items={openList}
          active={openHi}
          setActive={setOpenHi}
          onRun={onRunOpen}
          listRef={openListRef}
        />
      )}
      {suggestion && !busy && !input.trim() && onSuggestionPick && (
        <div className="qa-suggest-row">
          <button
            type="button"
            className="qa-suggest-chip"
            onClick={() => onSuggestionPick(suggestion)}
            title={t("suggestPick")}
          >
            <Sparkles size={11} />
            <span className="qa-suggest-prefix">{t("suggestPrefix")}</span>
            <span className="qa-suggest-text">{suggestion}</span>
          </button>
          <button
            type="button"
            className="qa-suggest-x"
            onClick={onSuggestionDismiss}
            aria-label={t("close")}
            title={t("close")}
          >
            <X size={11} />
          </button>
        </div>
      )}
      <textarea
        ref={inputRef}
        className="ai-hub-input"
        placeholder={
          openList
            ? t("openPlaceholder")
            : task !== "none"
              ? t(`taskPlaceholder.${task}`)
              : writes
                ? t("permission.placeholderWrite")
                : t("placeholder")
        }
        value={input}
        onChange={(e) => setInput(e.target.value)}
        onKeyDown={(e) => {
          // ↑ in an empty composer recalls the last question for editing.
          if (e.key === "ArrowUp" && !input && lastQuestion && !e.shiftKey) {
            e.preventDefault();
            setInput(lastQuestion);
            requestAnimationFrame(() => {
              const el = inputRef.current;
              if (el) el.setSelectionRange(el.value.length, el.value.length);
            });
            return;
          }
          if (openList && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
            e.preventDefault();
            if (openList.length === 0) return;
            const step = e.key === "ArrowDown" ? 1 : -1;
            setOpenHi((i) => (i + step + openList.length) % openList.length);
            return;
          }
          if (e.key !== "Enter") return;
          if (e.shiftKey && !(e.ctrlKey || e.metaKey)) return;
          e.preventDefault();
          if (busy) return;
          submit();
        }}
        rows={2}
      />
      <div className="qa-input-foot">
        <div className="qa-compose-tools">
          <div className="qa-tool-cluster">
            <button
              type="button"
              className="qa-tool-btn"
              onClick={onAttachFile}
              title={t("attach.file")}
              disabled={busy}
            >
              <FilePlus2 size={14} />
            </button>
            <button type="button" className="qa-tool-btn" onClick={onPasteClipboard} title={t("pasteClipboard")}>
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
          </div>
          <div className="qa-task-track">
            {QUICK_TASKS.map((k) => (
              <button
                key={k}
                type="button"
                className={`qa-task-btn${task === k ? " on" : ""}`}
                onClick={() => onTask(task === k ? "none" : k)}
                title={t(`task.${k}`)}
                aria-pressed={task === k}
              >
                {t(`taskShort.${k}`)}
              </button>
            ))}
          </div>
          <button
            type="button"
            className="qa-task-btn qa-daily-btn"
            onClick={onDaily}
            disabled={busy}
            title={t("dailyTip")}
          >
            {t("dailyShort")}
          </button>
        </div>
        <div className="qa-compose-end">
          <PermissionPicker
            permission={permission}
            onPermission={onPermission}
            open={permissionMenuOpen}
            onToggle={onTogglePermissionMenu}
          />
          {modelControl}
          <button
            type="button"
            className={`qa-send-btn${busy ? " is-busy" : ""}${writes ? ` is-${permission}` : ""}`}
            onClick={busy ? onStop : submit}
            disabled={!busy && !input.trim()}
            title={busy ? `${t("stop")} (Esc)` : t("sendHintFull")}
            aria-label={busy ? t("stop") : t("send")}
          >
            <Send size={15} className="qa-send-icon" />
            <Square size={12} className="qa-stop-icon" />
          </button>
        </div>
      </div>
    </div>
  );
}
