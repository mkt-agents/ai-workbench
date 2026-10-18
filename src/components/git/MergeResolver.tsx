import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  ArrowRight,
  Check,
  ChevronDown,
  ChevronUp,
  FileExclamationPoint,
  Loader2,
  PencilLine,
  Undo2,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";
import type { GitConflictFile, MergeSegment } from "../../core/types";
import { parseInvokeError } from "../../hooks/useInvokeError";
import {
  assembleResult,
  assembleWithMarkers,
  choiceText,
  conflictIndexes,
  lineCount,
  nextUnresolved,
  resolveRemaining,
  unresolvedCount,
  type ChunkChoice,
  type ConflictSegment,
  type Resolutions,
} from "../../lib/mergeChunks";
import ModalTitleRow from "../ModalTitleRow";
import { useConfirm } from "../ConfirmModal";

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

/** Merged text longer than this folds to its first and last lines. */
const FOLD_LINES = 12;
const FOLD_KEEP = 3;
/** Below this body width the three columns become tabs. */
const NARROW_PX = 760;

type Pane = "ours" | "result" | "theirs";

function errorText(e: unknown): string {
  const raw = (e instanceof Error ? e.message : String(e ?? "")).trim();
  return parseInvokeError(raw).message || raw;
}

function CodeBlock({ text, className }: { text: string; className?: string }) {
  return <pre className={`mr-code${className ? ` ${className}` : ""}`}>{text || "\u00a0"}</pre>;
}

/** Text git merged by itself: dimmed, long runs folded. */
function SameBlock({ text }: { text: string }) {
  const { t } = useTranslation("git");
  const [open, setOpen] = useState(false);
  const lines = text.replace(/\n$/, "").split("\n");
  if (open || lines.length <= FOLD_LINES) return <CodeBlock text={text} className="is-same" />;
  return (
    <div className="mr-fold">
      <CodeBlock text={lines.slice(0, FOLD_KEEP).join("\n")} className="is-same" />
      <button type="button" className="mr-fold-btn" onClick={() => setOpen(true)}>
        {t("merge.foldShow", { count: lines.length - FOLD_KEEP * 2 })}
      </button>
      <CodeBlock text={lines.slice(-FOLD_KEEP).join("\n")} className="is-same" />
    </div>
  );
}

export default function MergeResolver({ repoPath, repoName, initialFile, onToast, onClose, onResolved }: Props) {
  const { t } = useTranslation("git");
  const confirm = useConfirm();
  const invokeGitMergeState = useGlobalStore((s) => s.invokeGitMergeState);
  const invokeGitConflictFile = useGlobalStore((s) => s.invokeGitConflictFile);
  const invokeGitResolveConflict = useGlobalStore((s) => s.invokeGitResolveConflict);

  const [files, setFiles] = useState<string[]>([]);
  const [file, setFile] = useState<string | null>(null);
  const [data, setData] = useState<GitConflictFile | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [res, setRes] = useState<Resolutions>({});
  /** Whole-file hand edit; null = chunk mode. */
  const [manual, setManual] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [focusIdx, setFocusIdx] = useState<number | null>(null);
  const [pane, setPane] = useState<Pane>("result");
  const [narrow, setNarrow] = useState(false);
  const bodyRef = useRef<HTMLDivElement>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  const loadGen = useRef(0);

  const segments: MergeSegment[] = data?.segments ?? [];
  const conflicts = useMemo(() => conflictIndexes(segments), [segments]);
  const open = unresolvedCount(segments, res);
  const dirty = Object.keys(res).length > 0 || manual !== null;
  const labels = {
    ours: data?.oursLabel || t("merge.ours"),
    theirs: data?.theirsLabel || t("merge.theirs"),
  };

  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => setNarrow(entry.contentRect.width < NARROW_PX));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const loadList = useCallback(async () => {
    const state = await invokeGitMergeState(repoPath);
    setFiles(state.conflicted);
    return state.conflicted;
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
    setRes({});
    setManual(null);
    setFocusIdx(null);
    invokeGitConflictFile(repoPath, file)
      .then((next) => {
        if (gen !== loadGen.current) return;
        setData(next);
        setFocusIdx(conflictIndexes(next.segments)[0] ?? null);
      })
      .catch((e) => gen === loadGen.current && setError(errorText(e)))
      .finally(() => gen === loadGen.current && setLoading(false));
  }, [file, invokeGitConflictFile, repoPath]);

  useEffect(() => {
    if (focusIdx === null) return;
    gridRef.current
      ?.querySelector(`[data-seg="${focusIdx}"]`)
      ?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [focusIdx]);

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

  const choose = (idx: number, choice: Exclude<ChunkChoice, "custom">) => {
    const seg = segments[idx] as ConflictSegment;
    const nextRes = { ...res, [idx]: { choice, text: choiceText(seg, choice) } };
    setRes(nextRes);
    setFocusIdx(nextUnresolved(segments, nextRes, idx, 1));
  };

  const editChunk = (idx: number, text: string) => setRes((prev) => ({ ...prev, [idx]: { choice: "custom", text } }));

  const undoChunk = (idx: number) =>
    setRes((prev) => {
      const next = { ...prev };
      delete next[idx];
      return next;
    });

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

  const merged = manual ?? assembleResult(segments, res);
  const canSave = merged !== null;

  const startManual = () => setManual(assembleWithMarkers(segments, res, labels));

  const renderConflictResult = (idx: number, seg: ConflictSegment) => {
    const picked = res[idx];
    if (picked) {
      return (
        <div className="mr-picked">
          <textarea
            className="mr-edit"
            value={picked.text}
            spellCheck={false}
            rows={Math.max(1, lineCount(picked.text))}
            onChange={(e) => editChunk(idx, e.target.value)}
            aria-label={t("merge.editChunk")}
          />
          <button type="button" className="mr-chunk-undo" onClick={() => undoChunk(idx)} title={t("merge.undoChunk")}>
            <Undo2 size={12} />
            {t(`merge.choice.${picked.choice}`)}
          </button>
        </div>
      );
    }
    return (
      <div className="mr-open">
        <div className="mr-open-label">{t("merge.unresolved")}</div>
        {seg.base && <CodeBlock text={seg.base} className="is-base" />}
        <div className="mr-open-actions">
          <button type="button" className="btn btn-secondary btn-small" onClick={() => choose(idx, "ours")}>
            {t("merge.choice.ours")}
          </button>
          <button type="button" className="btn btn-secondary btn-small" onClick={() => choose(idx, "theirs")}>
            {t("merge.choice.theirs")}
          </button>
          <button type="button" className="btn btn-secondary btn-small" onClick={() => choose(idx, "oursTheirs")}>
            {t("merge.choice.oursTheirs")}
          </button>
          <button type="button" className="btn btn-secondary btn-small" onClick={() => choose(idx, "theirsOurs")}>
            {t("merge.choice.theirsOurs")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => choose(idx, "base")}
            title={t("merge.choiceBaseTip")}
          >
            {t("merge.choice.base")}
          </button>
        </div>
      </div>
    );
  };

  const renderChunks = () => (
    <>
      <div className={`mr-cols mr-cols-head${narrow ? ` is-tab is-${pane}` : ""}`}>
        <div className="mr-col-title is-ours">{t("merge.oursTitle", { name: labels.ours })}</div>
        <div className="mr-col-title is-result">{t("merge.resultTitle")}</div>
        <div className="mr-col-title is-theirs">{t("merge.theirsTitle", { name: labels.theirs })}</div>
      </div>
      <div ref={gridRef} className={`mr-grid${narrow ? ` is-tab is-${pane}` : ""}`}>
        {segments.map((seg, idx) =>
          seg.kind === "same" ? (
            <div key={idx} className="mr-cols mr-row is-same">
              <div className="mr-cell is-ours"><SameBlock text={seg.text} /></div>
              <div className="mr-cell is-result"><SameBlock text={seg.text} /></div>
              <div className="mr-cell is-theirs"><SameBlock text={seg.text} /></div>
            </div>
          ) : (
            <div
              key={idx}
              data-seg={idx}
              className={`mr-cols mr-row is-conflict${res[idx] ? " is-done" : ""}${focusIdx === idx ? " is-focus" : ""}`}
              onClick={() => setFocusIdx(idx)}
            >
              <div className="mr-cell is-ours">
                <CodeBlock text={seg.ours} className="is-side" />
                <button
                  type="button"
                  className="mr-take is-ours"
                  onClick={(e) => {
                    e.stopPropagation();
                    choose(idx, "ours");
                  }}
                  title={t("merge.takeOursTip")}
                >
                  {t("merge.takeOurs")}
                  <ArrowRight size={12} />
                </button>
              </div>
              <div className="mr-cell is-result">{renderConflictResult(idx, seg)}</div>
              <div className="mr-cell is-theirs">
                <CodeBlock text={seg.theirs} className="is-side" />
                <button
                  type="button"
                  className="mr-take is-theirs"
                  onClick={(e) => {
                    e.stopPropagation();
                    choose(idx, "theirs");
                  }}
                  title={t("merge.takeTheirsTip")}
                >
                  <ArrowLeft size={12} />
                  {t("merge.takeTheirs")}
                </button>
              </div>
            </div>
          )
        )}
      </div>
    </>
  );

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

  const wholeFile = !!data && (!data.textual || data.kind !== "content");
  const position = focusIdx !== null ? conflicts.indexOf(focusIdx) + 1 : 0;

  return (
    <div className="modal-overlay">
      <div className="modal merge-resolver" role="dialog" aria-label={t("merge.title", { name: repoName })}>
        <ModalTitleRow
          title={t("merge.title", { name: repoName })}
          onClose={() => void requestClose()}
          disabled={saving}
          badge={files.length > 0 ? <span className="card-title-badge">{t("merge.fileCount", { count: files.length })}</span> : undefined}
        />
        <div ref={bodyRef} className="mr-body">
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
                      <span className="mr-count">
                        {open > 0
                          ? t("merge.progress", { done: conflicts.length - open, total: conflicts.length })
                          : t("merge.progressDone", { total: conflicts.length })}
                        {position > 0 && ` · ${t("merge.position", { at: position, total: conflicts.length })}`}
                      </span>
                      <button
                        type="button"
                        className="btn commit-icon-btn"
                        disabled={open === 0}
                        onClick={() => setFocusIdx(nextUnresolved(segments, res, focusIdx ?? -1, -1))}
                        title={t("merge.prev")}
                        aria-label={t("merge.prev")}
                      >
                        <ChevronUp size={14} />
                      </button>
                      <button
                        type="button"
                        className="btn commit-icon-btn"
                        disabled={open === 0}
                        onClick={() => setFocusIdx(nextUnresolved(segments, res, focusIdx ?? -1, 1))}
                        title={t("merge.next")}
                        aria-label={t("merge.next")}
                      >
                        <ChevronDown size={14} />
                      </button>
                      <span className="mr-toolbar-gap" />
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        disabled={open === 0}
                        onClick={() => setRes(resolveRemaining(segments, res, "ours"))}
                      >
                        {t("merge.allOurs")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        disabled={open === 0}
                        onClick={() => setRes(resolveRemaining(segments, res, "theirs"))}
                      >
                        {t("merge.allTheirs")}
                      </button>
                      <button type="button" className="btn btn-secondary btn-small" onClick={startManual}>
                        <PencilLine size={13} />
                        {t("merge.editAll")}
                      </button>
                    </>
                  )}
                  {manual !== null && (
                    <>
                      <span className="mr-count">{t("merge.manualHint")}</span>
                      <span className="mr-toolbar-gap" />
                      <button type="button" className="btn btn-secondary btn-small" onClick={() => setManual(null)}>
                        {t("merge.backToChunks")}
                      </button>
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
                  renderChunks()
                )}
                {!wholeFile && (
                  <div className="mr-foot">
                    <span className="mr-foot-hint">
                      {manual !== null
                        ? t("merge.manualSaveHint")
                        : open > 0
                          ? t("merge.openLeft", { count: open })
                          : t("merge.readyHint")}
                    </span>
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={saving || !canSave || (manual === null && open > 0)}
                      onClick={() => merged !== null && void save({ content: merged })}
                    >
                      {saving ? <Loader2 size={14} className="spin" /> : <Check size={14} />}
                      {t("merge.markResolved")}
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
