import { Fragment, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ChevronDown,
  ChevronUp,
  ChevronsLeft,
  ChevronsRight,
  FileExclamationPoint,
  FoldVertical,
  Loader2,
  PencilLine,
  RotateCcw,
  WandSparkles,
  X,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";
import type { GitConflictFile } from "../../core/types";
import { parseInvokeError } from "../../hooks/useInvokeError";
import { diff3Chunks, type MergeChunk } from "../../lib/merge3";
import {
  acceptSide,
  applyNonConflicting,
  assembleResult,
  assembleWithMarkers,
  chunkLines,
  chunkResolved,
  counts,
  decide,
  editChunk,
  nextOpen,
  openNonConflicting,
  resetChunk,
  segmentsWithMarkers,
  sidesOf,
  stateOf,
  type MergeState,
  type Side,
} from "../../lib/mergeChunks";
import ModalTitleRow from "../ModalTitleRow";
import { useConfirm, useConfirmChoice } from "../ConfirmModal";

type Props = {
  repoPath: string;
  repoName: string;
  initialFile?: string;
  onToast: (type: "success" | "error", text: string) => void;
  /** Resolver closed (list refresh happens in the parent). */
  onClose: () => void;
  /** A file was marked resolved. */
  onResolved: () => void;
};

/** Unchanged runs longer than this fold to their first and last lines. */
const FOLD_MIN = 8;
const FOLD_KEEP = 3;
/** Below this body width the three panes become tabs. */
const NARROW_PX = 760;

type Pane = "ours" | "result" | "theirs";

function errorText(e: unknown): string {
  const raw = (e instanceof Error ? e.message : String(e ?? "")).trim();
  return parseInvokeError(raw).message || raw;
}

/** Numbered lines of one pane inside one chunk. */
function Lines({ lines, start }: { lines: string[]; start: number }) {
  return (
    <>
      {lines.map((line, i) => (
        <div key={i} className="mr3-line">
          <span className="mr3-no">{start + i}</span>
          <span className="mr3-text">{line || "\u00a0"}</span>
        </div>
      ))}
    </>
  );
}

export default function MergeResolver({ repoPath, repoName, initialFile, onToast, onClose, onResolved }: Props) {
  const { t } = useTranslation("git");
  const confirm = useConfirm();
  const confirmChoice = useConfirmChoice();
  const invokeGitMergeState = useGlobalStore((s) => s.invokeGitMergeState);
  const invokeGitConflictFile = useGlobalStore((s) => s.invokeGitConflictFile);
  const invokeGitResolveConflict = useGlobalStore((s) => s.invokeGitResolveConflict);

  const [files, setFiles] = useState<string[]>([]);
  const [file, setFile] = useState<string | null>(null);
  const [data, setData] = useState<GitConflictFile | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [state, setState] = useState<MergeState>({});
  /** Whole-file hand edit; null = chunk mode. */
  const [manual, setManual] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [focusIdx, setFocusIdx] = useState<number | null>(null);
  const [fold, setFold] = useState(true);
  const [unfolded, setUnfolded] = useState<Set<number>>(() => new Set());
  const [pane, setPane] = useState<Pane>("result");
  const [narrow, setNarrow] = useState(false);
  const bodyRef = useRef<HTMLDivElement>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  const loadGen = useRef(0);

  const labels = {
    ours: data?.oursLabel || t("merge.ours"),
    theirs: data?.theirsLabel || t("merge.theirs"),
  };
  const merge = useMemo(
    () =>
      data && data.textual && data.kind === "content"
        ? diff3Chunks(data.base ?? "", data.ours ?? "", data.theirs ?? "")
        : null,
    [data]
  );
  const chunks: MergeChunk[] = merge?.chunks ?? [];
  const wholeFile = !!data && (!data.textual || data.kind !== "content");
  // Too large for a line diff: hand edit git's own merge with conflict markers.
  const tooLarge = !!data && !wholeFile && !merge;
  const open = counts(chunks, state);
  const pendingChanges = openNonConflicting(chunks, state);
  const dirty = Object.keys(state).length > 0 || (manual !== null && !tooLarge);

  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => setNarrow(entry.contentRect.width < NARROW_PX));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const loadList = useCallback(async () => {
    const st = await invokeGitMergeState(repoPath);
    setFiles(st.conflicted);
    return st.conflicted;
  }, [invokeGitMergeState, repoPath]);

  useEffect(() => {
    void (async () => {
      try {
        const list = await loadList();
        const first = initialFile && list.includes(initialFile) ? initialFile : list[0] ?? null;
        setFile(first);
        if (!first) setLoading(false);
      } catch (e) {
        setError(errorText(e));
        setLoading(false);
      }
    })();
  }, [initialFile, loadList]);

  useEffect(() => {
    if (!file) return;
    const gen = ++loadGen.current;
    setLoading(true);
    setError("");
    setData(null);
    setState({});
    setManual(null);
    setFocusIdx(null);
    setUnfolded(new Set());
    invokeGitConflictFile(repoPath, file)
      .then((next) => gen === loadGen.current && setData(next))
      .catch((e) => gen === loadGen.current && setError(errorText(e)))
      .finally(() => gen === loadGen.current && setLoading(false));
  }, [file, invokeGitConflictFile, repoPath]);

  // First open chunk gets focus; a file too large for the grid opens in hand edit.
  useEffect(() => {
    if (!data) return;
    if (tooLarge) {
      setManual(segmentsWithMarkers(data.segments, labels));
      return;
    }
    setFocusIdx(nextOpen(chunks, {}, -1, 1));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data, tooLarge]);

  useEffect(() => {
    if (focusIdx === null) return;
    gridRef.current?.querySelector(`[data-chunk="${focusIdx}"]`)?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [focusIdx]);

  /** Line number each pane starts at for every chunk (result depends on choices). */
  const starts = useMemo(() => {
    let o = 1;
    let th = 1;
    let r = 1;
    return chunks.map((c, i) => {
      const s = { ours: o, theirs: th, result: r };
      o += c.ours.length;
      th += c.theirs.length;
      r += chunkLines(c, stateOf(state, i)).length;
      return s;
    });
  }, [chunks, state]);

  const confirmDiscard = async () =>
    !dirty ||
    (await confirm({
      title: t("merge.discardTitle"),
      message: t("merge.discardMessage"),
      confirmText: t("merge.discardConfirm"),
      icon: "warning",
    }));

  const requestClose = async () => {
    if (saving) return;
    if (await confirmDiscard()) onClose();
  };

  const switchFile = async (next: string) => {
    if (next === file || saving) return;
    if (await confirmDiscard()) setFile(next);
  };

  const afterSaved = async (text: string) => {
    onToast("success", text);
    onResolved();
    const list = await loadList().catch(() => files.filter((f) => f !== file));
    if (list.length === 0) {
      onToast("success", t("merge.allDone"));
      onClose();
      return;
    }
    const idx = Math.max(0, files.indexOf(file ?? ""));
    setFile(list[Math.min(idx, list.length - 1)]);
  };

  const save = async (resolution: { content: string } | { take: "ours" | "theirs" | "delete" }) => {
    if (!file || saving) return;
    setSaving(true);
    try {
      await afterSaved(await invokeGitResolveConflict(repoPath, file, resolution));
    } catch (e) {
      onToast("error", errorText(e));
    } finally {
      setSaving(false);
    }
  };

  const decideAt = (idx: number, side: Side, action: "apply" | "ignore") => {
    const next = decide(state, idx, side, action);
    setState(next);
    if (chunkResolved(chunks[idx], stateOf(next, idx))) setFocusIdx(nextOpen(chunks, next, idx, 1));
  };

  /** 应用: conflicts must be handled; one-sided changes left open are offered first. */
  const apply = async () => {
    if (!merge) return;
    let final = state;
    if (pendingChanges > 0) {
      const choice = await confirmChoice({
        title: t("merge.pendingTitle"),
        message: t("merge.pendingMessage", { count: pendingChanges }),
        confirmText: t("merge.pendingApplyAll"),
        altConfirmText: t("merge.pendingAsIs"),
        icon: "warning",
      });
      if (choice === "cancel") return;
      if (choice === "confirm") final = applyNonConflicting(chunks, state);
    }
    setState(final);
    await save({ content: assembleResult(chunks, final, merge.trailingNewline) });
  };

  const acceptWhole = async (side: Side) => {
    const ok = await confirm({
      title: side === "left" ? t("merge.acceptOursTitle") : t("merge.acceptTheirsTitle"),
      message: t("merge.acceptMessage", { name: side === "left" ? labels.ours : labels.theirs }),
      confirmText: side === "left" ? t("merge.acceptOurs") : t("merge.acceptTheirs"),
      icon: "warning",
    });
    if (!ok) return;
    setState(acceptSide(chunks, side));
    await save({ take: side === "left" ? "ours" : "theirs" });
  };

  const gutter = (idx: number, chunk: MergeChunk, side: Side): ReactNode => {
    if (!sidesOf(chunk).includes(side)) return null;
    const st = stateOf(state, idx);
    const other: Side = side === "left" ? "right" : "left";
    const appending = chunk.kind === "conflict" && st[other] === "applied";
    if (st.custom !== undefined) return null;
    if (st[side] !== "none") {
      return (
        <span className={`mr3-mark is-${st[side]}`} title={t(`merge.mark.${st[side]}`)}>
          {st[side] === "applied" ? <Check size={11} /> : <X size={11} />}
        </span>
      );
    }
    const Arrow = side === "left" ? ChevronsRight : ChevronsLeft;
    const take = (
      <button
        type="button"
        className="mr3-act is-take"
        onClick={(e) => {
          e.stopPropagation();
          decideAt(idx, side, "apply");
        }}
        title={appending ? t("merge.appendTip") : side === "left" ? t("merge.takeOursTip") : t("merge.takeTheirsTip")}
        aria-label={appending ? t("merge.appendTip") : side === "left" ? t("merge.takeOursTip") : t("merge.takeTheirsTip")}
      >
        {appending ? <span className="mr3-append">+</span> : <Arrow size={13} />}
      </button>
    );
    const ignore = (
      <button
        type="button"
        className="mr3-act is-ignore"
        onClick={(e) => {
          e.stopPropagation();
          decideAt(idx, side, "ignore");
        }}
        title={t("merge.ignoreTip")}
        aria-label={t("merge.ignoreTip")}
      >
        <X size={12} />
      </button>
    );
    return side === "left" ? (
      <>
        {take}
        {ignore}
      </>
    ) : (
      <>
        {ignore}
        {take}
      </>
    );
  };

  const renderResult = (idx: number, chunk: MergeChunk) => {
    const st = stateOf(state, idx);
    const lines = chunkLines(chunk, st);
    if (chunk.kind === "equal" || chunk.kind === "same") return <Lines lines={lines} start={starts[idx].result} />;
    const text = st.custom ?? lines.join("\n");
    const touched = st.custom !== undefined || st.left !== "none" || st.right !== "none";
    return (
      <div className="mr3-edit-wrap">
        <textarea
          className="mr3-edit"
          value={text}
          spellCheck={false}
          rows={Math.max(1, text.split("\n").length)}
          onChange={(e) => setState(editChunk(state, idx, e.target.value))}
          aria-label={t("merge.editChunk")}
        />
        {touched && (
          <button
            type="button"
            className="mr3-reset"
            onClick={() => setState(resetChunk(state, idx))}
            title={t("merge.undoChunk")}
            aria-label={t("merge.undoChunk")}
          >
            <RotateCcw size={11} />
          </button>
        )}
      </div>
    );
  };

  /** One chunk = one grid row per visible piece, so the three panes stay aligned. */
  const renderChunk = (chunk: MergeChunk, idx: number) => {
    const s = starts[idx];
    const longEqual = chunk.kind === "equal" && fold && chunk.base.length > FOLD_MIN && !unfolded.has(idx);
    if (longEqual) {
      const n = chunk.base.length;
      const head = chunk.base.slice(0, idx === 0 ? 0 : FOLD_KEEP);
      const tail = chunk.base.slice(idx === chunks.length - 1 ? n : n - FOLD_KEEP);
      const hidden = n - head.length - tail.length;
      const offset = (start: number, k: number) => start + k;
      const row = (lines: string[], from: number, key: string) =>
        lines.length > 0 && (
          <div key={key} className="mr3-row is-equal">
            <div className="mr3-cell is-ours"><Lines lines={lines} start={offset(s.ours, from)} /></div>
            <div className="mr3-gutter is-left" />
            <div className="mr3-cell is-result"><Lines lines={lines} start={offset(s.result, from)} /></div>
            <div className="mr3-gutter is-right" />
            <div className="mr3-cell is-theirs"><Lines lines={lines} start={offset(s.theirs, from)} /></div>
          </div>
        );
      return (
        <Fragment key={idx}>
          {row(head, 0, `${idx}h`)}
          <button
            type="button"
            className="mr3-fold"
            onClick={() => setUnfolded((prev) => new Set(prev).add(idx))}
          >
            {t("merge.foldShow", { count: hidden })}
          </button>
          {row(tail, n - tail.length, `${idx}t`)}
        </Fragment>
      );
    }
    const st = stateOf(state, idx);
    const resolved = chunkResolved(chunk, st);
    return (
      <div
        key={idx}
        data-chunk={idx}
        className={`mr3-row is-${chunk.kind}${resolved ? " is-done" : ""}${focusIdx === idx ? " is-focus" : ""}`}
        onClick={() => chunk.kind !== "equal" && setFocusIdx(idx)}
      >
        <div className={`mr3-cell is-ours${chunk.kind === "theirs" ? " is-quiet" : ""}`}>
          <Lines lines={chunk.ours} start={s.ours} />
        </div>
        <div className="mr3-gutter is-left">{gutter(idx, chunk, "left")}</div>
        <div className="mr3-cell is-result">{renderResult(idx, chunk)}</div>
        <div className="mr3-gutter is-right">{gutter(idx, chunk, "right")}</div>
        <div className={`mr3-cell is-theirs${chunk.kind === "ours" ? " is-quiet" : ""}`}>
          <Lines lines={chunk.theirs} start={s.theirs} />
        </div>
      </div>
    );
  };

  /** Modify-vs-delete, binary or non-UTF-8 files: only whole-file choices. */
  const renderWholeFile = (info: GitConflictFile) => {
    const reason =
      info.kind === "deletedByThem"
        ? t("merge.kindDeletedByThem", { theirs: labels.theirs })
        : info.kind === "deletedByUs"
          ? t("merge.kindDeletedByUs", { ours: labels.ours })
          : info.kind === "deletedBoth"
            ? t("merge.kindDeletedBoth")
            : t("merge.kindBinary");
    const hasOurs = info.kind !== "deletedByUs" && info.kind !== "deletedBoth";
    const hasTheirs = info.kind !== "deletedByThem" && info.kind !== "deletedBoth";
    return (
      <div className="mr-whole">
        <FileExclamationPoint size={20} aria-hidden />
        <div className="mr-whole-text">{reason}</div>
        <div className="mr-whole-actions">
          {hasOurs && (
            <button type="button" className="btn btn-secondary" disabled={saving} onClick={() => void save({ take: "ours" })}>
              {t("merge.keepOurs", { name: labels.ours })}
            </button>
          )}
          {hasTheirs && (
            <button type="button" className="btn btn-secondary" disabled={saving} onClick={() => void save({ take: "theirs" })}>
              {t("merge.keepTheirs", { name: labels.theirs })}
            </button>
          )}
          {info.kind !== "content" && (
            <button type="button" className="btn btn-secondary" disabled={saving} onClick={() => void save({ take: "delete" })}>
              {t("merge.deleteFile")}
            </button>
          )}
        </div>
      </div>
    );
  };

  const openTotal = open.changes + open.conflicts;
  const tabClass = narrow ? ` is-tab is-${pane}` : "";

  return (
    <div className="modal-overlay">
      <div className="modal merge-resolver" role="dialog" aria-label={t("merge.title", { name: repoName })}>
        <ModalTitleRow
          title={t("merge.title", { name: repoName })}
          onClose={() => void requestClose()}
          disabled={saving}
          badge={files.length > 0 ? <span className="card-title-badge">{t("merge.fileCount", { count: files.length })}</span> : undefined}
        />
        <div ref={bodyRef} className={`mr-body${files.length <= 1 ? " is-single" : ""}`}>
          {files.length > 1 && (
            <ul className="mr-files" aria-label={t("merge.files")}>
              {files.map((f) => (
                <li key={f}>
                  <button
                    type="button"
                    className={`mr-file${f === file ? " is-active" : ""}`}
                    onClick={() => void switchFile(f)}
                    title={f}
                  >
                    <span className="mr-file-name">{f.slice(f.lastIndexOf("/") + 1)}</span>
                    {f.includes("/") && <span className="mr-file-dir">{f.slice(0, f.lastIndexOf("/"))}</span>}
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="mr-main">
            {loading ? (
              <div className="mr-state">
                <Loader2 size={16} className="spin" /> {t("merge.loading")}
              </div>
            ) : error ? (
              <div className="mr-state is-error">{error}</div>
            ) : !data || !file ? (
              <div className="mr-state">{t("merge.none")}</div>
            ) : (
              <>
                <div className="mr-toolbar">
                  <span className="mr-path" title={file}>{file}</span>
                  {!wholeFile && manual === null && (
                    <>
                      <span className={`mr-count${open.conflicts > 0 ? " has-conflicts" : ""}`}>
                        {openTotal > 0
                          ? t("merge.openSummary", { changes: open.changes, conflicts: open.conflicts })
                          : t("merge.allHandled")}
                      </span>
                      <button
                        type="button"
                        className="btn commit-icon-btn"
                        disabled={openTotal === 0}
                        onClick={() => setFocusIdx(nextOpen(chunks, state, focusIdx ?? chunks.length, -1))}
                        title={t("merge.prev")}
                        aria-label={t("merge.prev")}
                      >
                        <ChevronUp size={14} />
                      </button>
                      <button
                        type="button"
                        className="btn commit-icon-btn"
                        disabled={openTotal === 0}
                        onClick={() => setFocusIdx(nextOpen(chunks, state, focusIdx ?? -1, 1))}
                        title={t("merge.next")}
                        aria-label={t("merge.next")}
                      >
                        <ChevronDown size={14} />
                      </button>
                      <span className="mr-toolbar-gap" />
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        disabled={pendingChanges === 0}
                        onClick={() => setState(applyNonConflicting(chunks, state))}
                        title={t("merge.applyNonConflictingTip")}
                      >
                        <WandSparkles size={13} />
                        {t("merge.applyNonConflicting", { count: pendingChanges })}
                      </button>
                      <button
                        type="button"
                        className={`btn btn-secondary btn-small${fold ? " is-on" : ""}`}
                        aria-pressed={fold}
                        onClick={() => setFold(!fold)}
                      >
                        <FoldVertical size={13} />
                        {t("merge.foldUnchanged")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        onClick={() => setManual(assembleWithMarkers(chunks, state, labels, merge?.trailingNewline ?? true))}
                      >
                        <PencilLine size={13} />
                        {t("merge.editAll")}
                      </button>
                    </>
                  )}
                  {manual !== null && (
                    <>
                      <span className="mr-count">{tooLarge ? t("merge.tooLarge") : t("merge.manualHint")}</span>
                      <span className="mr-toolbar-gap" />
                      {!tooLarge && (
                        <button type="button" className="btn btn-secondary btn-small" onClick={() => setManual(null)}>
                          {t("merge.backToChunks")}
                        </button>
                      )}
                    </>
                  )}
                </div>
                {narrow && !wholeFile && manual === null && (
                  <div className="mr-tabs" role="tablist">
                    {(["ours", "result", "theirs"] as Pane[]).map((p) => (
                      <button
                        key={p}
                        type="button"
                        role="tab"
                        aria-selected={pane === p}
                        className={`mr-tab${pane === p ? " is-on" : ""}`}
                        onClick={() => setPane(p)}
                      >
                        {p === "ours" ? labels.ours : p === "theirs" ? labels.theirs : t("merge.resultTitle")}
                      </button>
                    ))}
                  </div>
                )}
                {wholeFile ? (
                  renderWholeFile(data)
                ) : manual !== null ? (
                  <textarea
                    className="mr-manual"
                    value={manual}
                    spellCheck={false}
                    onChange={(e) => setManual(e.target.value)}
                    aria-label={t("merge.editAll")}
                  />
                ) : (
                  <>
                    <div className={`mr3-head${tabClass}`}>
                      <div className="mr3-title is-ours">{t("merge.oursTitle", { name: labels.ours })}</div>
                      <div className="mr3-gutter is-left" />
                      <div className="mr3-title is-result">{t("merge.resultTitle")}</div>
                      <div className="mr3-gutter is-right" />
                      <div className="mr3-title is-theirs">{t("merge.theirsTitle", { name: labels.theirs })}</div>
                    </div>
                    <div ref={gridRef} className={`mr3-grid${tabClass}`}>
                      {chunks.map(renderChunk)}
                    </div>
                  </>
                )}
                {!wholeFile && (
                  <div className="mr-foot">
                    {manual === null && (
                      <>
                        <button
                          type="button"
                          className="btn btn-secondary"
                          disabled={saving}
                          onClick={() => void acceptWhole("left")}
                        >
                          {t("merge.acceptOurs")}
                        </button>
                        <button
                          type="button"
                          className="btn btn-secondary"
                          disabled={saving}
                          onClick={() => void acceptWhole("right")}
                        >
                          {t("merge.acceptTheirs")}
                        </button>
                      </>
                    )}
                    <span className="mr-foot-hint">
                      {manual !== null
                        ? t("merge.manualSaveHint")
                        : open.conflicts > 0
                          ? t("merge.conflictsLeft", { count: open.conflicts })
                          : pendingChanges > 0
                            ? t("merge.changesLeft", { count: pendingChanges })
                            : t("merge.readyHint")}
                    </span>
                    <button type="button" className="btn btn-secondary" disabled={saving} onClick={() => void requestClose()}>
                      {t("merge.cancel")}
                    </button>
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={saving || (manual === null && open.conflicts > 0)}
                      onClick={() => (manual !== null ? void save({ content: manual }) : void apply())}
                    >
                      {saving ? <Loader2 size={14} className="spin" /> : <Check size={14} />}
                      {t("merge.apply")}
                    </button>
                  </div>
                )}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
