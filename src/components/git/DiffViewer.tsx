import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronDown,
  ChevronRight,
  ChevronUp,
  Columns2,
  Loader2,
  Rows2,
  Space,
  TextWrap,
  X,
} from "lucide-react";
import { readStoredString, writeStoredString } from "../../core/localState";
import {
  buildHunkPatch,
  changeStarts,
  maxLineNo,
  parseUnifiedDiff,
  toSplitRows,
  type DiffHunk,
  type DiffRow,
  type SplitRow,
} from "../../lib/diffRows";

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
  onClose: () => void;
};

const MODE_KEY = "cl-diff-mode";
const WRAP_KEY = "cl-diff-wrap";
/** Rendering more rows than this as individual nodes gets sluggish. */
const RENDER_ROWS = 2000;
/** Side-by-side needs room for two gutters and two text columns. */
const SPLIT_MIN_PX = 640;

type Mode = "unified" | "split";

/** One rendered line; changeId >= 0 marks the first line of a change block. */
type ViewRow = { change: boolean; changeId: number; split?: SplitRow; unified?: DiffRow };

function absolutePath(repoPath: string, relPath: string): string {
  return `${repoPath.replace(/[\\/]+$/, "")}\\${relPath.replace(/\//g, "\\")}`;
}

const SIGN: Record<string, string> = { add: "+", del: "-", ctx: " ", note: "" };

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
  onClose,
}: Props) {
  const { t } = useTranslation("git");
  const [mode, setMode] = useState<Mode>(() => (readStoredString(MODE_KEY, "unified") === "split" ? "split" : "unified"));
  const [wrap, setWrap] = useState(() => readStoredString(WRAP_KEY, "1") !== "0");
  const [picked, setPicked] = useState<Set<number>>(() => new Set());
  const [folded, setFolded] = useState<Set<number>>(() => new Set());
  const [showAll, setShowAll] = useState(false);
  const [navAt, setNavAt] = useState(-1);
  const [roomForSplit, setRoomForSplit] = useState(true);
  const rootRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);

  useEffect(() => writeStoredString(MODE_KEY, mode), [mode]);
  useEffect(() => writeStoredString(WRAP_KEY, wrap ? "1" : "0"), [wrap]);

  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => setRoomForSplit(entry.contentRect.width >= SPLIT_MIN_PX));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const parsed = useMemo(() => parseUnifiedDiff(text), [text]);
  useEffect(() => {
    setPicked(new Set());
    setFolded(new Set());
    setShowAll(false);
    setNavAt(-1);
  }, [text]);

  const view: Mode = mode === "split" && roomForSplit ? "split" : "unified";
  const hasHunks = parsed.hunks.length > 0;
  // A -w diff no longer matches the file, so it can't be applied as a patch.
  const canStage = !!target && !target.wholeFile && hasHunks && !parsed.binary && !ignoreWhitespace;
  const gutter = `${Math.max(2, String(maxLineNo(parsed)).length) + 1}ch`;

  /** Rows per hunk for the current view, cut at RENDER_ROWS unless expanded. */
  const layout = useMemo(() => {
    let budget = showAll ? Number.POSITIVE_INFINITY : RENDER_ROWS;
    let hidden = 0;
    let changeIndex = 0;
    const hunks = parsed.hunks.map((hunk, hi) => {
      const rows: ViewRow[] =
        view === "split"
          ? toSplitRows(hunk).map((r) => ({ split: r, change: r.change, changeId: -1 }))
          : hunk.rows.map((r) => ({ unified: r, change: r.kind === "add" || r.kind === "del", changeId: -1 }));
      const starts = new Set(changeStarts(rows.map((r) => r.change)));
      const marked = rows.map((r, i) => ({ ...r, changeId: starts.has(i) ? changeIndex++ : -1 }));
      if (folded.has(hi)) return { hunk, rows: [] as typeof marked };
      const take = Math.max(0, Math.min(marked.length, budget));
      budget -= take;
      hidden += marked.length - take;
      return { hunk, rows: marked.slice(0, take) };
    });
    return { hunks, hidden, changes: changeIndex };
  }, [parsed, view, folded, showAll]);

  const jump = (dir: 1 | -1) => {
    if (layout.changes === 0) return;
    const next = (navAt + dir + layout.changes) % layout.changes;
    setNavAt(next);
    bodyRef.current?.querySelector(`[data-change="${next}"]`)?.scrollIntoView({ block: "center" });
  };

  useEffect(() => {
    if (!active || !target) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F7" || e.ctrlKey || e.altKey || e.metaKey) return;
      if ((e.target as HTMLElement | null)?.closest("input, textarea, select, [contenteditable='true']")) return;
      if (document.querySelector(".modal-overlay")) return;
      e.preventDefault();
      jump(e.shiftKey ? -1 : 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const togglePick = (i: number) =>
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });

  const toggleFold = (i: number) =>
    setFolded((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });

  const applyHunks = (hunks: DiffHunk[]) => onApply(buildHunkPatch(parsed.preamble, hunks));

  const name = target ? target.filePath.slice(target.filePath.lastIndexOf("/") + 1) : "";
  const dir = target && target.filePath.includes("/") ? target.filePath.slice(0, target.filePath.lastIndexOf("/")) : "";

  const renderHunkHead = (hunk: DiffHunk, hi: number) => {
    const isFolded = folded.has(hi);
    const Chevron = isFolded ? ChevronRight : ChevronDown;
    return (
      <div className="dv-hunk">
        <button
          type="button"
          className="dv-hunk-fold"
          onClick={() => toggleFold(hi)}
          aria-expanded={!isFolded}
          title={isFolded ? t("commit.diffUnfold") : t("commit.diffFold")}
        >
          <Chevron size={13} />
        </button>
        {canStage && (
          <input
            type="checkbox"
            checked={picked.has(hi)}
            disabled={busy}
            onChange={() => togglePick(hi)}
            aria-label={t("commit.diffPickHunk")}
          />
        )}
        <code className="dv-hunk-range">
          -{hunk.oldStart} +{hunk.newStart}
        </code>
        {hunk.section && <span className="dv-hunk-section">{hunk.section}</span>}
        <span className="dv-hunk-count">
          <span className="dv-add">+{hunk.adds}</span> <span className="dv-del">−{hunk.dels}</span>
        </span>
        {canStage && (
          <button type="button" className="dv-hunk-stage" disabled={busy} onClick={() => applyHunks([hunk])}>
            {target?.staged ? t("commit.diffUnstageHunk") : t("commit.diffStageHunk")}
          </button>
        )}
      </div>
    );
  };

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
          <div className="cl-empty-hint runtime-muted">
            {parsed.binary ? t("commit.hunkBinary") : text.trim()}
          </div>
        </div>
      );
    }
    return (
      <div
        ref={bodyRef}
        className={`dv-body is-${view}${wrap ? "" : " is-nowrap"}`}
        style={{ ["--dv-gutter" as string]: gutter }}
      >
        {parsed.notes.length > 0 && <div className="dv-notes">{parsed.notes.join(" · ")}</div>}
        {layout.hunks.map(({ hunk, rows }, hi) => (
          <section key={hi} className="dv-hunk-block">
            {renderHunkHead(hunk, hi)}
            <div className="dv-rows">
              {rows.map((row, ri) =>
                row.split ? (
                  <div
                    key={ri}
                    className={`dv-srow${row.change ? " is-change" : ""}${row.changeId === navAt && navAt >= 0 ? " is-nav" : ""}`}
                    data-change={row.changeId >= 0 ? row.changeId : undefined}
                  >
                    <span className={`dv-no is-${row.split.left?.kind ?? "empty"}`}>{row.split.left?.no ?? ""}</span>
                    <span className={`dv-text is-${row.split.left?.kind ?? "empty"}`}>{row.split.left?.text || "\u00a0"}</span>
                    <span className={`dv-no is-${row.split.right?.kind ?? "empty"}`}>{row.split.right?.no ?? ""}</span>
                    <span className={`dv-text is-${row.split.right?.kind ?? "empty"}`}>{row.split.right?.text || "\u00a0"}</span>
                  </div>
                ) : row.unified ? (
                  <div
                    key={ri}
                    className={`dv-row is-${row.unified.kind}${row.changeId === navAt && navAt >= 0 ? " is-nav" : ""}`}
                    data-change={row.changeId >= 0 ? row.changeId : undefined}
                  >
                    <span className="dv-no">{row.unified.oldNo ?? ""}</span>
                    <span className="dv-no">{row.unified.newNo ?? ""}</span>
                    <span className="dv-sign">{SIGN[row.unified.kind]}</span>
                    <span className="dv-text">{row.unified.text || "\u00a0"}</span>
                  </div>
                ) : null
              )}
            </div>
          </section>
        ))}
        {layout.hidden > 0 && (
          <div className="cl-diff-more">
            <button type="button" className="btn btn-secondary btn-small" onClick={() => setShowAll(true)}>
              {t("commit.diffShowAll", { count: layout.hidden })}
            </button>
          </div>
        )}
      </div>
    );
  };

  return (
    <div ref={rootRef} className="cl-panel cl-split-diff git-diff-panel dv">
      <div className="cl-diff-head dv-head">
        {target ? (
          <>
            {statusLabel && <span className={`git-file-badge ${statusClass ?? ""}`}>{statusLabel}</span>}
            <span className="dv-title" title={absolutePath(target.repoPath, target.filePath)}>
              <span className="dv-title-name">{name}</span>
              {dir && <span className="dv-title-dir">{dir}</span>}
            </span>
            <span className={`dv-chip${target.staged ? " is-staged" : ""}`}>
              {target.staged ? t("commit.statusStaged") : t("commit.diffUnstaged")}
            </span>
            {hasHunks && (
              <span className="dv-stat">
                <span className="dv-add">+{parsed.adds}</span> <span className="dv-del">−{parsed.dels}</span>
              </span>
            )}
          </>
        ) : (
          <span className="dv-title runtime-muted">{t("commit.diffEmpty")}</span>
        )}
        <span className="dv-head-gap" />
        {target && hasHunks && (
          <div className="dv-tools">
            {canStage && picked.size > 0 && (
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={busy}
                onClick={() => applyHunks(parsed.hunks.filter((_, i) => picked.has(i)))}
              >
                {target.staged
                  ? t("commit.diffUnstagePicked", { count: picked.size })
                  : t("commit.diffStagePicked", { count: picked.size })}
              </button>
            )}
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={() => jump(-1)}
              disabled={layout.changes === 0}
              title={t("commit.diffPrevChange")}
              aria-label={t("commit.diffPrevChange")}
            >
              <ChevronUp size={14} />
            </button>
            <button
              type="button"
              className="btn commit-icon-btn"
              onClick={() => jump(1)}
              disabled={layout.changes === 0}
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
                !roomForSplit
                  ? t("commit.diffSplitNarrow")
                  : view === "split"
                    ? t("commit.diffUnified")
                    : t("commit.diffSplit")
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
              className={`btn commit-icon-btn${wrap ? " is-on" : ""}`}
              onClick={() => setWrap(!wrap)}
              aria-pressed={wrap}
              title={t("commit.diffWrap")}
            >
              <TextWrap size={14} />
            </button>
          </div>
        )}
        {target && (
          <button
            type="button"
            className="btn commit-icon-btn"
            onClick={onClose}
            title={t("commit.diffClose")}
            aria-label={t("commit.diffClose")}
          >
            <X size={14} />
          </button>
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
