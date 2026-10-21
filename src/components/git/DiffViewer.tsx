import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronUp, Columns2, Loader2, Maximize2, Rows2, Space, TextWrap, Undo2, X } from "lucide-react";
import { readStoredString, writeStoredString } from "../../core/localState";
import { useDiffView, type BlockAction, type DiffMode } from "./useDiffView";
import type { ChangeBlock } from "../../lib/diffRows";

export type DiffViewTarget = {
  repoPath: string;
  filePath: string;
  staged: boolean;
  /** Untracked: the whole file is new, hunks can't be staged separately. */
  wholeFile: boolean;
};

type Props = {
  active: boolean;
  target: DiffViewTarget | null;
  text: string;
  loading: boolean;
  statusLabel?: string;
  statusClass?: string;
  busy: boolean;
  ignoreWhitespace: boolean;
  onIgnoreWhitespace: (on: boolean) => void;
  onApply: (patch: string) => void;
  /** Open the same file in the large diff dialog. */
  onExpand: () => void;
  /** » / « on a change block (revert / stage that block). */
  onBlock: (action: BlockAction, patch: string, block: ChangeBlock) => void;
  /** Set right after a block revert of this file: undo it. */
  onUndoRevert?: () => void;
  onClose: () => void;
};

/** Per-block actions a diff offers: none for untracked files or a -w view (that patch would not
 *  apply back). Truncated text is blocked one layer down, inside useDiffView. */
export function blockActionsFor(target: DiffViewTarget | null, ignoreWhitespace: boolean) {
  if (!target || target.wholeFile || ignoreWhitespace) return { revert: false, stage: false };
  return { revert: true, stage: !target.staged };
}

export const DIFF_MODE_KEY = "cl-diff-mode";
const WRAP_KEY = "cl-diff-wrap";
/** Side-by-side needs room for two gutters and two text columns. */
const SPLIT_MIN_PX = 640;

export function absolutePath(repoPath: string, relPath: string): string {
  return `${repoPath.replace(/[\\/]+$/, "")}\\${relPath.replace(/\//g, "\\")}`;
}

/** File name, folder, status and staged chip: the left side of both diff headers. */
export function DiffTitle({
  target,
  statusLabel,
  statusClass,
  adds,
  dels,
  showStat,
}: {
  target: DiffViewTarget;
  statusLabel?: string;
  statusClass?: string;
  adds: number;
  dels: number;
  showStat: boolean;
}) {
  const { t } = useTranslation("git");
  const name = target.filePath.slice(target.filePath.lastIndexOf("/") + 1);
  const dir = target.filePath.includes("/") ? target.filePath.slice(0, target.filePath.lastIndexOf("/")) : "";
  return (
    <>
      {statusLabel && <span className={`git-file-badge ${statusClass ?? ""}`}>{statusLabel}</span>}
      <span className="dv-title" title={absolutePath(target.repoPath, target.filePath)}>
        <span className="dv-title-name">{name}</span>
        {dir && <span className="dv-title-dir">{dir}</span>}
      </span>
      <span className={`dv-chip${target.staged ? " is-staged" : ""}`}>
        {target.staged ? t("commit.statusStaged") : t("commit.diffUnstaged")}
      </span>
      {showStat && (
        <span className="dv-stat">
          <span className="dv-add">+{adds}</span> <span className="dv-del">−{dels}</span>
        </span>
      )}
    </>
  );
}

/** F7 / Shift+F7 outside inputs and dialogs. */
export function useF7(enabled: boolean, jump: (dir: 1 | -1) => void, insideModal = false) {
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F7" || e.ctrlKey || e.altKey || e.metaKey) return;
      if ((e.target as HTMLElement | null)?.closest("input, textarea, select, [contenteditable='true']")) return;
      if (!insideModal && document.querySelector(".modal-overlay")) return;
      e.preventDefault();
      jump(e.shiftKey ? -1 : 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
}

export default function DiffViewer({
  active,
  target,
  text,
  loading,
  statusLabel,
  statusClass,
  busy,
  ignoreWhitespace,
  onIgnoreWhitespace,
  onApply,
  onExpand,
  onBlock,
  onUndoRevert,
  onClose,
}: Props) {
  const { t } = useTranslation("git");
  const [mode, setMode] = useState<DiffMode>(() => (readStoredString(DIFF_MODE_KEY, "unified") === "split" ? "split" : "unified"));
  const [wrap, setWrap] = useState(() => readStoredString(WRAP_KEY, "1") !== "0");
  const [roomForSplit, setRoomForSplit] = useState(true);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => writeStoredString(DIFF_MODE_KEY, mode), [mode]);
  useEffect(() => writeStoredString(WRAP_KEY, wrap ? "1" : "0"), [wrap]);

  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => setRoomForSplit(entry.contentRect.width >= SPLIT_MIN_PX));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const view: DiffMode = mode === "split" && roomForSplit ? "split" : "unified";
  const diff = useDiffView({
    text,
    view,
    wrap,
    fold: false,
    words: false,
    // A -w diff no longer matches the file, so it can't be applied as a patch.
    canStage: !!target && !target.wholeFile && !ignoreWhitespace,
    staged: !!target?.staged,
    busy,
    onApply,
    blockActions: blockActionsFor(target, ignoreWhitespace),
    onBlock,
  });
  const { parsed } = diff;
  const hasHunks = parsed.hunks.length > 0;
  const canStage = !!target && !target.wholeFile && hasHunks && !parsed.binary && !ignoreWhitespace;
  useF7(active && !!target, diff.jump);

  const body = () => {
    if (loading) {
      return (
        <div className="cl-diff-empty">
          <Loader2 size={14} className="spin" /> {t("commit.diffLoading")}
        </div>
      );
    }
    if (!target || !text) {
      return (
        <div className="cl-diff-empty">
          <div className="cl-empty-title">{t("commit.changelistDiffIdle")}</div>
          <div className="cl-empty-hint runtime-muted">{t("commit.changelistDiffHint")}</div>
        </div>
      );
    }
    if (!hasHunks) {
      return (
        <div className="cl-diff-empty">
          <div className="cl-empty-hint runtime-muted">{parsed.binary ? t("commit.hunkBinary") : text.trim()}</div>
        </div>
      );
    }
    return diff.body;
  };

  return (
    <div ref={rootRef} className="cl-panel cl-split-diff git-diff-panel dv">
      <div className="cl-diff-head dv-head">
        {target ? (
          <DiffTitle
            target={target}
            statusLabel={statusLabel}
            statusClass={statusClass}
            adds={parsed.adds}
            dels={parsed.dels}
            showStat={hasHunks}
          />
        ) : (
          <span className="dv-title runtime-muted">{t("commit.diffEmpty")}</span>
        )}
        <span className="dv-head-gap" />
        {target && onUndoRevert && (
          <button type="button" className="btn btn-secondary btn-small" disabled={busy} onClick={onUndoRevert}>
            <Undo2 size={13} />
            {t("commit.undoRevert")}
          </button>
        )}
        {target && hasHunks && (
          <div className="dv-tools">
            {canStage && diff.picked.size > 0 && (
              <button type="button" className="btn btn-secondary btn-small" disabled={busy} onClick={diff.applyPicked}>
                {target.staged
                  ? t("commit.diffUnstagePicked", { count: diff.picked.size })
                  : t("commit.diffStagePicked", { count: diff.picked.size })}
              </button>
            )}
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={() => diff.jump(-1)}
              disabled={diff.changes === 0}
              title={t("commit.diffPrevChange")}
              aria-label={t("commit.diffPrevChange")}
            >
              <ChevronUp size={14} />
            </button>
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={() => diff.jump(1)}
              disabled={diff.changes === 0}
              title={t("commit.diffNextChange")}
              aria-label={t("commit.diffNextChange")}
            >
              <ChevronDown size={14} />
            </button>
            <button
              type="button"
              className={`btn commit-icon-btn${view === "split" ? " is-on" : ""}`}
              onClick={() => setMode(mode === "split" ? "unified" : "split")}
              disabled={!roomForSplit}
              aria-pressed={view === "split"}
              title={
                !roomForSplit ? t("commit.diffSplitNarrow") : view === "split" ? t("commit.diffUnified") : t("commit.diffSplit")
              }
            >
              {view === "split" ? <Rows2 size={14} /> : <Columns2 size={14} />}
            </button>
            <button
              type="button"
              className={`btn commit-icon-btn${ignoreWhitespace ? " is-on" : ""}`}
              onClick={() => onIgnoreWhitespace(!ignoreWhitespace)}
              aria-pressed={ignoreWhitespace}
              title={ignoreWhitespace ? t("commit.diffWhitespaceOn") : t("commit.diffWhitespace")}
            >
              <Space size={14} />
            </button>
            <button
              type="button"
              className={`btn commit-icon-btn${wrap || view === "split" ? " is-on" : ""}`}
              onClick={() => setWrap(!wrap)}
              disabled={view === "split"}
              aria-pressed={wrap || view === "split"}
              title={view === "split" ? t("commit.diffWrapSplit") : t("commit.diffWrap")}
            >
              <TextWrap size={14} />
            </button>
          </div>
        )}
        {target && (
          <>
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={onExpand}
              title={t("commit.diffExpand")}
              aria-label={t("commit.diffExpand")}
            >
              <Maximize2 size={14} />
            </button>
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={onClose}
              title={t("commit.diffClose")}
              aria-label={t("commit.diffClose")}
            >
              <X size={14} />
            </button>
          </>
        )}
      </div>
      {target?.wholeFile && hasHunks && <div className="cl-hunk-note runtime-muted">{t("commit.hunkUntracked")}</div>}
      {ignoreWhitespace && target && hasHunks && !target.wholeFile && (
        <div className="cl-hunk-note runtime-muted">{t("commit.diffWhitespaceNote")}</div>
      )}
      {body()}
    </div>
  );
}
