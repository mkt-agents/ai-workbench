import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowLeft, ArrowRight, ChevronDown, ChevronUp, Loader2, Undo2 } from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { readStoredString, writeStoredString } from "../../core/localState";
import { parseInvokeError } from "../../hooks/useInvokeError";
import ModalTitleRow from "../ModalTitleRow";
import { blockActionsFor, DiffTitle, useF7, type DiffViewTarget } from "./DiffViewer";
import { useDiffView, type BlockAction, type DiffMode } from "./useDiffView";
import type { ChangeBlock } from "../../lib/diffRows";

export type DiffFile = DiffViewTarget & { statusLabel: string; statusClass: string };

type Props = {
  /** The changelist in display order; ← / → walk through it. */
  files: DiffFile[];
  startIndex: number;
  busy: boolean;
  /** Stage / unstage hunks of `file`; the dialog reloads the diff afterwards. */
  onApply: (file: DiffFile, patch: string) => Promise<void>;
  /** » / « on a change block; the dialog reloads the diff afterwards. */
  onBlock: (file: DiffFile, action: BlockAction, patch: string, block: ChangeBlock) => Promise<void>;
  /** The file whose last block revert can be undone, if any. */
  undoRevertFor: { repoPath: string; filePath: string } | null;
  onUndoRevert: () => Promise<void>;
  onClose: () => void;
};

const MODE_KEY = "cl-diff-modal-mode";
const FULL_KEY = "cl-diff-modal-full";
const FOLD_KEY = "cl-diff-fold";
const WORDS_KEY = "cl-diff-words";
const WS_KEY = "cl-diff-modal-ws";
const WRAP_KEY = "cl-diff-modal-wrap";
/** `-U` large enough for any file: the whole file becomes one hunk. */
const FULL_CONTEXT = 100_000;

const flag = (key: string, fallback: boolean) => readStoredString(key, fallback ? "1" : "0") === "1";

function errorText(e: unknown): string {
  const raw = (e instanceof Error ? e.message : String(e ?? "")).trim();
  return parseInvokeError(raw).message || raw;
}

/** IDEA-style diff window: change / file navigation, viewer mode, whitespace, words, folding, full file. */
export default function DiffModal({ files, startIndex, busy, onApply, onBlock, undoRevertFor, onUndoRevert, onClose }: Props) {
  const { t } = useTranslation("git");
  const invokeGitDiff = useGlobalStore((s) => s.invokeGitDiff);
  const [index, setIndex] = useState(() => Math.min(Math.max(0, startIndex), Math.max(0, files.length - 1)));
  const [text, setText] = useState("");
  const [loading, setLoading] = useState(true);
  const [mode, setMode] = useState<DiffMode>(() => (readStoredString(MODE_KEY, "split") === "unified" ? "unified" : "split"));
  const [full, setFull] = useState(() => flag(FULL_KEY, true));
  const [fold, setFold] = useState(() => flag(FOLD_KEY, true));
  const [words, setWords] = useState(() => flag(WORDS_KEY, true));
  const [ignoreWs, setIgnoreWs] = useState(() => flag(WS_KEY, false));
  const [wrap, setWrap] = useState(() => flag(WRAP_KEY, false));

  useEffect(() => writeStoredString(MODE_KEY, mode), [mode]);
  useEffect(() => writeStoredString(FULL_KEY, full ? "1" : "0"), [full]);
  useEffect(() => writeStoredString(FOLD_KEY, fold ? "1" : "0"), [fold]);
  useEffect(() => writeStoredString(WORDS_KEY, words ? "1" : "0"), [words]);
  useEffect(() => writeStoredString(WS_KEY, ignoreWs ? "1" : "0"), [ignoreWs]);
  useEffect(() => writeStoredString(WRAP_KEY, wrap ? "1" : "0"), [wrap]);

  const file = files[index];

  const load = useCallback(async () => {
    if (!file) return;
    setLoading(true);
    try {
      setText(
        await invokeGitDiff(file.repoPath, file.filePath, file.staged, {
          ignoreWhitespace: ignoreWs,
          contextLines: full ? FULL_CONTEXT : undefined,
        })
      );
    } catch (e) {
      setText(errorText(e));
    } finally {
      setLoading(false);
    }
  }, [file, full, ignoreWs, invokeGitDiff]);

  useEffect(() => {
    void load();
  }, [load]);

  // Full-file and -w diffs are views only: their hunks don't apply back as patches.
  const canStage = !!file && !file.wholeFile && !ignoreWs && !full;
  const diff = useDiffView({
    text,
    view: mode,
    wrap,
    fold,
    words,
    canStage,
    staged: !!file?.staged,
    busy,
    onApply: (patch) => {
      if (!file) return;
      void onApply(file, patch).then(load);
    },
    blockActions: blockActionsFor(file ?? null, ignoreWs),
    onBlock: (action, patch, block) => {
      if (!file) return;
      void onBlock(file, action, patch, block).then(load);
    },
  });
  const canUndo =
    !!file && !!undoRevertFor && undoRevertFor.repoPath === file.repoPath && undoRevertFor.filePath === file.filePath;
  useF7(!!file, diff.jump, true);

  const go = (dir: 1 | -1) => setIndex((i) => Math.min(files.length - 1, Math.max(0, i + dir)));

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.altKey || (e.key !== "ArrowLeft" && e.key !== "ArrowRight")) return;
      if ((e.target as HTMLElement | null)?.closest("input, textarea, select")) return;
      e.preventDefault();
      go(e.key === "ArrowLeft" ? -1 : 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const { parsed } = diff;
  const hasHunks = parsed.hunks.length > 0;
  const name = file ? file.filePath.slice(file.filePath.lastIndexOf("/") + 1) : "";

  return (
    <div className="modal-overlay">
      <div className="modal diff-modal" role="dialog" aria-label={t("commit.diffModal.title", { name })}>
        <ModalTitleRow
          title={t("commit.diffModal.title", { name })}
          onClose={onClose}
          badge={
            files.length > 1 ? (
              <span className="card-title-badge">{t("commit.diffModal.fileOf", { at: index + 1, total: files.length })}</span>
            ) : undefined
          }
        />
        <div className="dm-toolbar" role="toolbar">
          <button
            type="button"
            className="btn commit-icon-btn"
            onClick={() => diff.jump(-1)}
            disabled={diff.changes === 0}
            title={t("commit.diffPrevChange")}
            aria-label={t("commit.diffPrevChange")}
          >
            <ChevronUp size={15} />
          </button>
          <button
            type="button"
            className="btn commit-icon-btn"
            onClick={() => diff.jump(1)}
            disabled={diff.changes === 0}
            title={t("commit.diffNextChange")}
            aria-label={t("commit.diffNextChange")}
          >
            <ChevronDown size={15} />
          </button>
          <span className="dm-sep" />
          <button
            type="button"
            className="btn commit-icon-btn"
            onClick={() => go(-1)}
            disabled={index === 0}
            title={t("commit.diffModal.prevFile")}
            aria-label={t("commit.diffModal.prevFile")}
          >
            <ArrowLeft size={15} />
          </button>
          <button
            type="button"
            className="btn commit-icon-btn"
            onClick={() => go(1)}
            disabled={index >= files.length - 1}
            title={t("commit.diffModal.nextFile")}
            aria-label={t("commit.diffModal.nextFile")}
          >
            <ArrowRight size={15} />
          </button>
          <span className="dm-sep" />
          <select
            className="input-field dm-select"
            value={mode}
            onChange={(e) => setMode(e.target.value === "unified" ? "unified" : "split")}
            aria-label={t("commit.diffModal.viewer")}
          >
            <option value="split">{t("commit.diffModal.viewerSplit")}</option>
            <option value="unified">{t("commit.diffModal.viewerUnified")}</option>
          </select>
          <select
            className="input-field dm-select"
            value={ignoreWs ? "ws" : "none"}
            onChange={(e) => setIgnoreWs(e.target.value === "ws")}
            aria-label={t("commit.diffWhitespace")}
          >
            <option value="none">{t("commit.diffModal.wsNone")}</option>
            <option value="ws">{t("commit.diffModal.wsIgnore")}</option>
          </select>
          <button
            type="button"
            className={`dm-toggle${words ? " is-on" : ""}`}
            aria-pressed={words}
            onClick={() => setWords(!words)}
          >
            {t("commit.diffModal.words")}
          </button>
          <button
            type="button"
            className={`dm-toggle${fold ? " is-on" : ""}`}
            aria-pressed={fold}
            onClick={() => setFold(!fold)}
          >
            {t("commit.diffModal.fold")}
          </button>
          <button
            type="button"
            className={`dm-toggle${full ? " is-on" : ""}`}
            aria-pressed={full}
            onClick={() => setFull(!full)}
            title={t("commit.diffModal.fullTip")}
          >
            {t("commit.diffModal.full")}
          </button>
          <button
            type="button"
            className={`dm-toggle${wrap || mode === "split" ? " is-on" : ""}`}
            aria-pressed={wrap || mode === "split"}
            disabled={mode === "split"}
            title={mode === "split" ? t("commit.diffWrapSplit") : undefined}
            onClick={() => setWrap(!wrap)}
          >
            {t("commit.diffWrap")}
          </button>
          <span className="dm-gap" />
          {canUndo && (
            <button
              type="button"
              className="btn btn-secondary btn-small"
              disabled={busy}
              onClick={() => void onUndoRevert().then(load)}
            >
              <Undo2 size={13} />
              {t("commit.undoRevert")}
            </button>
          )}
          {canStage && diff.picked.size > 0 && (
            <button type="button" className="btn btn-secondary btn-small" disabled={busy} onClick={diff.applyPicked}>
              {file?.staged
                ? t("commit.diffUnstagePicked", { count: diff.picked.size })
                : t("commit.diffStagePicked", { count: diff.picked.size })}
            </button>
          )}
          <span className="dm-count">{t("commit.diffModal.count", { count: diff.changes })}</span>
        </div>
        {file && (
          <div className="dm-file">
            <DiffTitle
              target={file}
              statusLabel={file.statusLabel}
              statusClass={file.statusClass}
              adds={parsed.adds}
              dels={parsed.dels}
              showStat={hasHunks}
            />
            {!canStage && hasHunks && !file.wholeFile && (
              <span className="dm-note">{t("commit.diffModal.viewOnly")}</span>
            )}
          </div>
        )}
        <div className="dm-body">
          {loading ? (
            <div className="cl-diff-empty">
              <Loader2 size={14} className="spin" /> {t("commit.diffLoading")}
            </div>
          ) : !hasHunks ? (
            <div className="cl-diff-empty">
              <div className="cl-empty-hint runtime-muted">
                {parsed.binary ? t("commit.hunkBinary") : text.trim() || t("commit.diffModal.empty")}
              </div>
            </div>
          ) : (
            diff.body
          )}
        </div>
      </div>
    </div>
  );
}
