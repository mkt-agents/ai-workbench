import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ClipboardPaste, FilePlus2, FileText, Send, Sparkles, Square, X } from "lucide-react";
import type { OpenSuggestion } from "./localActions";
import PermissionPicker from "./PermissionPicker";
import CommandPalette from "./CommandPalette";
import type { AskPanelProps } from "./AskPanel";

type Props = Pick<
  AskPanelProps,
  | "input" | "setInput" | "inputRef" | "busy" | "lastQuestion" | "suggestion" | "onSuggestionPick"
  | "onSuggestionDismiss" | "hasSnippets"
  | "onToggleSnippets" | "onPasteClipboard" | "onPasteFiles" | "onAttachFile" | "onSend" | "repoControl"
  | "modelControl"
  | "modeChip" | "track" | "onTrack"
  | "permission" | "onPermission" | "diffConfirm" | "onDiffConfirm"
  | "permissionMenuOpen" | "onTogglePermissionMenu"
  | "openList" | "onRunOpen" | "onStop"
  | "palette"
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
    hasSnippets, onToggleSnippets, onPasteClipboard, onPasteFiles, onAttachFile,
    onSend, repoControl, modelControl, modeChip, track, onTrack,
    permission, onPermission, diffConfirm, onDiffConfirm, permissionMenuOpen, onTogglePermissionMenu,
    openList, onRunOpen, onStop,
    palette,
  } = props;
  /** `/` 命令与 `@` 文件补全的浮层：有候选时它优先吃掉 ↑↓ 与回车。 */
  const paletteItems = palette?.items ?? null;
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

  /** Enter / send button: run the highlighted "open …" match; nothing local matched → ask the AI. */
  const submit = () => {
    const pick = openList?.[openHi] ?? openList?.[0];
    if (pick) {
      onRunOpen(pick);
      return;
    }
    onSend();
  };

  const trackSwitch = onTrack ? (
    <div className="qa-track-switch" role="group" aria-label={t("track.label")}>
      <button
        type="button"
        className={`qa-track-btn${(track ?? "code") === "code" ? " is-active" : ""}`}
        onClick={() => onTrack("code")}
        title={t("track.codeDesc")}
      >
        {t("track.code")}
      </button>
      <button
        type="button"
        className={`qa-track-btn${track === "work" ? " is-active" : ""}`}
        onClick={() => onTrack("work")}
        title={t("track.workDesc")}
      >
        {t("track.work")}
      </button>
    </div>
  ) : null;

  return (
    <div className="qa-input-card">
      {/* 顶栏只放工作区：「在哪干活」是第一上下文 */}
      <div className="qa-compose-meta">{repoControl}</div>
      {/* 任务钉住单独一行，不与产品轨抢同一视觉层级 */}
      {modeChip ? <div className="qa-mode-pin-row">{modeChip}</div> : null}
      {openList && (
        <OpenSuggestList
          items={openList}
          active={openHi}
          setActive={setOpenHi}
          onRun={onRunOpen}
          listRef={openListRef}
        />
      )}
      {palette && paletteItems && (
        <CommandPalette
          items={paletteItems}
          active={palette.active}
          onHover={palette.onHover}
          onPick={palette.onPick}
          drillTitle={palette.drillTitle}
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
        // 输入框没有可见 label：没有 aria-label 时屏幕阅读器只会念"编辑框"
        aria-label={t("placeholder")}
        aria-controls={palette && paletteItems ? "qa-palette" : undefined}
        aria-activedescendant={
          palette && paletteItems ? `qa-palette-${palette.active}` : undefined
        }
        placeholder={
          openList
            ? t("openPlaceholder")
            : writes
              ? t("permission.placeholderWrite")
              : t("placeholder")
        }
        value={input}
        onChange={(e) => setInput(e.target.value)}
        onPaste={(e) => {
          // 资源管理器里「复制」的文件只有 CF_HDROP，Chromium 交给我们的是没有路径的
          // File 对象，内容读不到。所以必须在粘贴时拦下来，让主进程去读剪贴板文件列表，
          // 再走和拖放同一条附件管线。纯文本粘贴不受影响（交给默认行为）。
          const data = e.clipboardData;
          const hasFiles =
            data.files.length > 0 || Array.from(data.types || []).includes("Files");
          if (!hasFiles) return;
          e.preventDefault();
          onPasteFiles();
        }}
        onKeyDown={(e) => {
          // IME in the middle of composing: Enter / ↑↓ belong to the candidate window,
          // never to send or list navigation (a pinyin commit would fire a whole agent run).
          if (e.nativeEvent.isComposing || e.keyCode === 229) return;
          // 二级列表（`/mode` 的模式清单）：Esc 收起；空输入框退格退回 `/` 命令面板
          if (palette?.drillTitle) {
            if (e.key === "Escape") {
              e.preventDefault();
              palette.onCloseDrill?.();
              return;
            }
            if (e.key === "Backspace" && !input) {
              e.preventDefault();
              palette.onCloseDrill?.();
              setInput("/");
              return;
            }
          }
          // `/` 命令与 `@` 文件补全排在所有列表之前：它盖在输入框上方，
          // ↑↓ / 回车必须归它，否则会误触片段列表或直接发送。
          if (palette && paletteItems && paletteItems.length > 0) {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              palette.onMove(e.key === "ArrowDown" ? 1 : -1);
              return;
            }
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              palette.onPickActive();
              return;
            }
          }
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
                className={`qa-tool-btn${paletteItems ? " on" : ""}`}
                onClick={onToggleSnippets}
                title={t("palette.open")}
              >
                <FileText size={14} />
              </button>
            )}
          </div>
        </div>
        <div className="qa-compose-end">
          {/* 轨与权限同属「这一问能做什么」，成组放在发送钮左侧 */}
          {trackSwitch}
          <PermissionPicker
            permission={permission}
            onPermission={onPermission}
            diffConfirm={diffConfirm}
            onDiffConfirm={onDiffConfirm}
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
