import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronRight, Plus, RotateCcw } from "lucide-react";
import {
  blockPatch,
  buildHunkPatch,
  changeBlocks,
  changeStarts,
  foldUnchanged,
  maxLineNo,
  parseUnifiedDiff,
  toSplitRows,
  wordDiff,
  type ChangeBlock,
  type DiffHunk,
  type DiffRow,
  type ParsedDiff,
  type SplitRow,
  type WordPiece,
} from "../../lib/diffRows";

export type DiffMode = "unified" | "split";

/** One rendered line; `changeId` >= 0 marks the first line of a change block. */
type ViewRow = {
  change: boolean;
  changeId: number;
  split?: SplitRow;
  unified?: DiffRow;
  /** Word pieces for a modified line (unified: this row; split: left / right). */
  words?: WordPiece[];
  leftWords?: WordPiece[];
  rightWords?: WordPiece[];
  /** Index of the change block (see changeBlocks) this line belongs to. */
  block?: number;
  /** First line of its block: the per-block action buttons sit here. */
  blockStart?: boolean;
};

export type BlockAction = "revert" | "stage";

/** One entry of the change navigator: which change, at which new-side line, saying what. */
export type ChangeSummary = { id: number; line: number; text: string };

type Options = {
  text: string;
  view: DiffMode;
  wrap: boolean;
  /** Fold long unchanged runs into "展开 N 行". */
  fold: boolean;
  /** Highlight the changed words of a modified line. */
  words: boolean;
  /** Hunk checkboxes / 暂存此块 are offered. */
  canStage: boolean;
  staged: boolean;
  busy: boolean;
  onApply: (patch: string) => void;
  /** Which per-block actions a change offers: revert writes the file back, stage only moves it into the index. */
  blockActions?: { revert: boolean; stage: boolean };
  onBlock?: (action: BlockAction, patch: string, block: ChangeBlock) => void;
};

/** Rendering more rows than this as individual nodes gets sluggish. */
const RENDER_ROWS = 2000;

const SIGN: Record<string, string> = { add: "+", del: "-", ctx: " ", note: "" };

/** Navigator label for a change: prefer the new side so a modification reads as "+ 新内容". */
function changeSummary(row: ViewRow): { line: number; text: string } | null {
  const side = row.unified
    ? { kind: row.unified.kind, text: row.unified.text, no: row.unified.newNo ?? row.unified.oldNo }
    : row.split?.right
      ? { kind: row.split.right.kind, text: row.split.right.text, no: row.split.right.no }
      : row.split?.left
        ? { kind: row.split.left.kind, text: row.split.left.text, no: row.split.left.no }
        : null;
  if (!side) return null;
  const body = `${SIGN[side.kind] ?? " "} ${side.text.trim()}`.trim();
  return { line: side.no ?? 0, text: body.length > 56 ? `${body.slice(0, 56)}…` : body };
}

/** Pair the k-th removed line of a block with the k-th added line after it. */
function pairedLines(rows: DiffRow[]): Map<number, number> {
  const pairs = new Map<number, number>();
  let i = 0;
  while (i < rows.length) {
    if (rows[i].kind !== "del") {
      i++;
      continue;
    }
    const dels: number[] = [];
    while (i < rows.length && rows[i].kind === "del") dels.push(i++);
    const adds: number[] = [];
    while (i < rows.length && rows[i].kind === "add") adds.push(i++);
    dels.forEach((d, k) => {
      if (adds[k] !== undefined) pairs.set(d, adds[k]);
    });
  }
  return pairs;
}

function Text({ text, pieces }: { text: string; pieces?: WordPiece[] }) {
  if (!pieces) return <>{text || "\u00a0"}</>;
  return (
    <>
      {pieces.map((p, i) =>
        p.changed ? (
          <span key={i} className="dv-word">
            {p.text}
          </span>
        ) : (
          <span key={i}>{p.text}</span>
        )
      )}
    </>
  );
}

/**
 * Parse + layout + render of a unified diff, shared by the commit page's diff
 * panel and the large diff dialog (each draws its own header and tools).
 */
export function useDiffView({
  text,
  view,
  wrap,
  fold,
  words,
  canStage,
  staged,
  busy,
  onApply,
  blockActions,
  onBlock,
}: Options) {
  const { t } = useTranslation("git");
  const [picked, setPicked] = useState<Set<number>>(() => new Set());
  const [folded, setFolded] = useState<Set<number>>(() => new Set());
  const [opened, setOpened] = useState<Set<string>>(() => new Set());
  const [showAll, setShowAll] = useState(false);
  const [navAt, setNavAt] = useState(-1);
  const bodyRef = useRef<HTMLDivElement>(null);

  const parsed: ParsedDiff = useMemo(() => parseUnifiedDiff(text), [text]);
  useEffect(() => {
    setPicked(new Set());
    setFolded(new Set());
    setOpened(new Set());
    setShowAll(false);
    setNavAt(-1);
  }, [text]);

  const layout = useMemo(() => {
    let budget = showAll ? Number.POSITIVE_INFINITY : RENDER_ROWS;
    let hidden = 0;
    let changeIndex = 0;
    const changeList: ChangeSummary[] = [];
    const hunks = parsed.hunks.map((hunk, hi) => {
      let rows: ViewRow[];
      if (view === "split") {
        rows = toSplitRows(hunk).map((r) => {
          const row: ViewRow = { split: r, change: r.change, changeId: -1 };
          if (words && r.left?.kind === "del" && r.right?.kind === "add") {
            const w = wordDiff(r.left.text, r.right.text);
            row.leftWords = w.left;
            row.rightWords = w.right;
          }
          return row;
        });
      } else {
        const pairs = words ? pairedLines(hunk.rows) : new Map<number, number>();
        const pieces = new Map<number, WordPiece[]>();
        for (const [d, a] of pairs) {
          const w = wordDiff(hunk.rows[d].text, hunk.rows[a].text);
          pieces.set(d, w.left);
          pieces.set(a, w.right);
        }
        rows = hunk.rows.map((r, i) => ({
          unified: r,
          change: r.kind === "add" || r.kind === "del",
          changeId: -1,
          words: pieces.get(i),
        }));
      }
      // Same runs as changeBlocks: a note row ("\ No newline") does not end a run.
      let blk = -1;
      let inRun = false;
      rows.forEach((r) => {
        const isNote = r.split ? r.split.left?.kind === "note" : r.unified?.kind === "note";
        if (r.change) {
          if (!inRun) {
            blk++;
            r.blockStart = true;
          }
          inRun = true;
          r.block = blk;
        } else if (!isNote) {
          inRun = false;
        }
      });
      const starts = new Set(changeStarts(rows.map((r) => r.change)));
      rows.forEach((r, i) => {
        if (!starts.has(i)) return;
        r.changeId = changeIndex;
        const sum = changeSummary(r);
        if (sum) changeList.push({ id: changeIndex, ...sum });
        changeIndex++;
      });
      if (folded.has(hi)) return { hunk, items: [] as ReturnType<typeof foldUnchanged<ViewRow>> };
      const items = fold
        ? foldUnchanged(rows, (r) => r.change).flatMap((f) =>
            "fold" in f && opened.has(`${hi}:${f.fold.start}`)
              ? rows.slice(f.fold.start, f.fold.start + f.fold.count).map((item) => ({ item }))
              : [f]
          )
        : rows.map((item) => ({ item }));
      const take = Math.max(0, Math.min(items.length, budget));
      budget -= take;
      hidden += items.length - take;
      return { hunk, items: items.slice(0, take) };
    });
    return { hunks, hidden, changes: changeIndex, changeList };
  }, [parsed, view, words, fold, folded, opened, showAll]);

  /** Focus one change: mark it as current and bring it into view. */
  const goTo = (id: number) => {
    if (id < 0 || id >= layout.changes) return;
    setNavAt(id);
    bodyRef.current?.querySelector(`[data-change="${id}"]`)?.scrollIntoView({ block: "center" });
  };

  const jump = (dir: 1 | -1) => {
    if (layout.changes === 0) return;
    goTo((navAt + dir + layout.changes) % layout.changes);
  };

  const toggle = <T,>(set: (fn: (prev: Set<T>) => Set<T>) => void, key: T) =>
    set((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  const applyHunks = (hunks: DiffHunk[]) => onApply(buildHunkPatch(parsed.preamble, hunks));
  const blocksOf = useMemo(() => parsed.hunks.map(changeBlocks), [parsed]);
  const actions = !!onBlock && !!blockActions && (blockActions.revert || blockActions.stage);

  /** Revert / stage buttons on the first line of a change block. Icons only, and only on hover. */
  const arrows = (row: ViewRow, hi: number): ReactNode => {
    if (!actions || !row.blockStart || row.block === undefined) return null;
    const block = blocksOf[hi]?.[row.block];
    const hunk = parsed.hunks[hi];
    if (!block || !hunk) return null;
    return (
      <>
        {blockActions!.revert && (
          <button
            type="button"
            className="dv-arrow is-revert"
            disabled={busy}
            title={staged ? t("commit.blockUnstageTip") : t("commit.blockRevertTip")}
            aria-label={staged ? t("commit.blockUnstageTip") : t("commit.blockRevertTip")}
            onClick={() => onBlock!("revert", blockPatch(parsed.preamble, hunk, block, "new"), block)}
          >
            <RotateCcw size={13} />
          </button>
        )}
        {blockActions!.stage && (
          <button
            type="button"
            className="dv-arrow is-stage"
            disabled={busy}
            title={t("commit.blockStageTip")}
            aria-label={t("commit.blockStageTip")}
            onClick={() => onBlock!("stage", blockPatch(parsed.preamble, hunk, block, "old"), block)}
          >
            <Plus size={13} />
          </button>
        )}
      </>
    );
  };
  const gutter = `${Math.max(2, String(maxLineNo(parsed)).length) + 1}ch`;

  const hunkHead = (hunk: DiffHunk, hi: number) => {
    const isFolded = folded.has(hi);
    const Chevron = isFolded ? ChevronRight : ChevronDown;
    return (
      <div className="dv-hunk">
        <button
          type="button"
          className="dv-hunk-fold"
          onClick={() => toggle(setFolded, hi)}
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
            onChange={() => toggle(setPicked, hi)}
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
            {staged ? t("commit.diffUnstageHunk") : t("commit.diffStageHunk")}
          </button>
        )}
      </div>
    );
  };

  const navClass = (row: ViewRow) => (row.changeId === navAt && navAt >= 0 ? " is-nav" : "");

  const renderRow = (row: ViewRow, key: string, hi: number): ReactNode => {
    const gutClass = `dv-gut${row.block !== undefined ? " is-block" : ""}${row.blockStart ? " is-start" : ""}`;
    if (row.split) {
      const { left, right } = row.split;
      return (
        <div
          key={key}
          className={`dv-srow${row.change ? " is-change" : ""}${navClass(row)}`}
          data-change={row.changeId >= 0 ? row.changeId : undefined}
        >
          <span className={`dv-no is-${left?.kind ?? "empty"}`}>{left?.no ?? ""}</span>
          <span className={`dv-text is-${left?.kind ?? "empty"}`}>
            <Text text={left?.text ?? ""} pieces={row.leftWords} />
          </span>
          <span className={gutClass}>{arrows(row, hi)}</span>
          <span className={`dv-no is-${right?.kind ?? "empty"}`}>{right?.no ?? ""}</span>
          <span className={`dv-text is-${right?.kind ?? "empty"}`}>
            <Text text={right?.text ?? ""} pieces={row.rightWords} />
          </span>
        </div>
      );
    }
    const r = row.unified!;
    return (
      <div
        key={key}
        className={`dv-row is-${r.kind}${navClass(row)}`}
        data-change={row.changeId >= 0 ? row.changeId : undefined}
      >
        {actions && <span className={gutClass}>{arrows(row, hi)}</span>}
        <span className="dv-no">{r.oldNo ?? ""}</span>
        <span className="dv-no">{r.newNo ?? ""}</span>
        <span className="dv-sign">{SIGN[r.kind]}</span>
        <span className="dv-text">
          <Text text={r.text} pieces={row.words} />
        </span>
      </div>
    );
  };

  const body = (
    <div
      ref={bodyRef}
      className={`dv-body is-${view}${wrap ? "" : " is-nowrap"}${actions ? " has-actions" : ""}`}
      style={{ ["--dv-gutter" as string]: gutter }}
    >
      {parsed.notes.length > 0 && <div className="dv-notes">{parsed.notes.join(" · ")}</div>}
      {layout.hunks.map(({ hunk, items }, hi) => (
        <section key={hi} className="dv-hunk-block">
          {hunkHead(hunk, hi)}
          <div className="dv-rows">
            {items.map((entry, ri) =>
              "fold" in entry ? (
                <button
                  key={`f${ri}`}
                  type="button"
                  className="dv-fold"
                  onClick={() => toggle(setOpened, `${hi}:${entry.fold.start}`)}
                >
                  {t("commit.diffFoldShow", { count: entry.fold.count })}
                </button>
              ) : (
                renderRow(entry.item, `r${ri}`, hi)
              )
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

  return {
    parsed,
    body,
    changes: layout.changes,
    changeList: layout.changeList,
    /** Index of the change F7 last landed on, or -1 before the first jump. */
    navAt,
    goTo,
    picked,
    applyPicked: () => applyHunks(parsed.hunks.filter((_, i) => picked.has(i))),
    jump,
  };
}
