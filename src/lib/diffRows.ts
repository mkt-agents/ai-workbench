/**
 * Unified diff → rows with old/new line numbers, for the commit page's diff
 * panel (unified and side-by-side views share this parse).
 */
export type DiffRowKind = "ctx" | "add" | "del" | "note";

export type DiffRow = {
  kind: DiffRowKind;
  oldNo: number | null;
  newNo: number | null;
  text: string;
};

export type DiffHunk = {
  /** Raw `@@ -a,b +c,d @@ section` line, needed to rebuild a patch. */
  header: string;
  /** Function / section name git prints after the second `@@`. */
  section: string;
  oldStart: number;
  newStart: number;
  rows: DiffRow[];
  adds: number;
  dels: number;
  /** Header + body lines, verbatim. */
  patch: string;
};

export type ParsedDiff = {
  /** Lines before the first hunk (`diff --git`, `index`, `---`, `+++`, mode lines). */
  preamble: string;
  hunks: DiffHunk[];
  adds: number;
  dels: number;
  binary: boolean;
  /** new file / deleted file / rename notes from the preamble. */
  notes: string[];
};

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@ ?(.*)$/;

export function parseUnifiedDiff(text: string): ParsedDiff {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  const pre: string[] = [];
  const hunks: DiffHunk[] = [];
  let cur: DiffHunk | null = null;
  let body: string[] = [];
  let oldNo = 0;
  let newNo = 0;
  const close = () => {
    if (cur) {
      cur.patch = [cur.header, ...body].join("\n");
      hunks.push(cur);
    }
  };
  for (const line of lines) {
    const m = HUNK_RE.exec(line);
    if (m) {
      close();
      oldNo = Number(m[1]);
      newNo = Number(m[2]);
      cur = { header: line, section: m[3].trim(), oldStart: oldNo, newStart: newNo, rows: [], adds: 0, dels: 0, patch: "" };
      body = [];
      continue;
    }
    if (!cur) {
      pre.push(line);
      continue;
    }
    body.push(line);
    if (line.startsWith("+")) {
      cur.rows.push({ kind: "add", oldNo: null, newNo: newNo++, text: line.slice(1) });
      cur.adds++;
    } else if (line.startsWith("-")) {
      cur.rows.push({ kind: "del", oldNo: oldNo++, newNo: null, text: line.slice(1) });
      cur.dels++;
    } else if (line.startsWith("\\")) {
      cur.rows.push({ kind: "note", oldNo: null, newNo: null, text: line.slice(1).trim() });
    } else {
      cur.rows.push({ kind: "ctx", oldNo: oldNo++, newNo: newNo++, text: line.startsWith(" ") ? line.slice(1) : line });
    }
  }
  close();
  const preamble = pre.join("\n");
  const notes = pre
    .filter((l) => /^(new file mode|deleted file mode|rename from|rename to|similarity index|old mode|new mode)/.test(l))
    .map((l) => l.trim());
  return {
    preamble,
    hunks,
    adds: hunks.reduce((n, h) => n + h.adds, 0),
    dels: hunks.reduce((n, h) => n + h.dels, 0),
    binary: /^Binary files .* differ$/m.test(preamble) || /^GIT binary patch/m.test(preamble),
    notes,
  };
}

export type SplitCell = { no: number | null; text: string; kind: DiffRowKind };

/** One side-by-side line: a deletion pairs with the addition at the same offset. */
export type SplitRow = { left: SplitCell | null; right: SplitCell | null; change: boolean };

export function toSplitRows(hunk: DiffHunk): SplitRow[] {
  const out: SplitRow[] = [];
  const rows = hunk.rows;
  let i = 0;
  while (i < rows.length) {
    const row = rows[i];
    if (row.kind === "ctx") {
      out.push({
        left: { no: row.oldNo, text: row.text, kind: "ctx" },
        right: { no: row.newNo, text: row.text, kind: "ctx" },
        change: false,
      });
      i++;
      continue;
    }
    if (row.kind === "note") {
      out.push({ left: { no: null, text: row.text, kind: "note" }, right: null, change: false });
      i++;
      continue;
    }
    const dels: DiffRow[] = [];
    const adds: DiffRow[] = [];
    while (i < rows.length && rows[i].kind === "del") dels.push(rows[i++]);
    while (i < rows.length && rows[i].kind === "add") adds.push(rows[i++]);
    const n = Math.max(dels.length, adds.length);
    for (let k = 0; k < n; k++) {
      const d = dels[k];
      const a = adds[k];
      out.push({
        left: d ? { no: d.oldNo, text: d.text, kind: "del" } : null,
        right: a ? { no: a.newNo, text: a.text, kind: "add" } : null,
        change: true,
      });
    }
  }
  return out;
}

/** Index (within `rows`) of the first row of every change block, for prev / next change. */
export function changeStarts(kinds: boolean[]): number[] {
  const out: number[] = [];
  kinds.forEach((isChange, i) => {
    if (isChange && (i === 0 || !kinds[i - 1])) out.push(i);
  });
  return out;
}

/** A patch of the chosen hunks that `git apply --cached` accepts. */
export function buildHunkPatch(preamble: string, hunks: DiffHunk[]): string {
  const body = [preamble.trimEnd(), ...hunks.map((h) => h.patch)].filter(Boolean).join("\n");
  return body.endsWith("\n") ? body : `${body}\n`;
}

/** Widest line number, for a stable gutter width. */
export function maxLineNo(parsed: ParsedDiff): number {
  let max = 0;
  for (const h of parsed.hunks) {
    for (const r of h.rows) max = Math.max(max, r.oldNo ?? 0, r.newNo ?? 0);
  }
  return max;
}

/** A run of unchanged rows hidden behind "展开 N 行". */
export type Folded<T> = { item: T } | { fold: { start: number; count: number } };

/**
 * Keep `keep` unchanged rows around every change and fold the rest of each
 * unchanged run (only when at least `minFold` rows would hide).
 */
export function foldUnchanged<T>(rows: T[], isChange: (row: T) => boolean, keep = 3, minFold = 4): Folded<T>[] {
  const out: Folded<T>[] = [];
  let i = 0;
  while (i < rows.length) {
    if (isChange(rows[i])) {
      out.push({ item: rows[i++] });
      continue;
    }
    let end = i;
    while (end < rows.length && !isChange(rows[end])) end++;
    const atStart = i === 0;
    const atEnd = end === rows.length;
    const head = atStart ? 0 : keep;
    const tail = atEnd ? 0 : keep;
    const hidden = end - i - head - tail;
    if (hidden >= minFold) {
      for (let k = i; k < i + head; k++) out.push({ item: rows[k] });
      out.push({ fold: { start: i + head, count: hidden } });
      for (let k = end - tail; k < end; k++) out.push({ item: rows[k] });
    } else {
      for (let k = i; k < end; k++) out.push({ item: rows[k] });
    }
    i = end;
  }
  return out;
}

export type WordPiece = { text: string; changed: boolean };

/** Latin words stay whole; every CJK character and symbol is its own token. */
const TOKEN_RE = /\p{Script=Han}|[A-Za-z0-9_\u00C0-\u024F]+|\s+|[^\s]/gu;
const WORD_LIMIT = 2000;
const TOKEN_LIMIT = 400;

function pieces(tokens: string[], keep: boolean[]): WordPiece[] {
  const out: WordPiece[] = [];
  tokens.forEach((text, i) => {
    const changed = !keep[i];
    const last = out[out.length - 1];
    if (last && last.changed === changed) last.text += text;
    else out.push({ text, changed });
  });
  return out;
}

/**
 * Changed words between a removed and an added line (token LCS). Very long
 * lines come back as one changed piece each: the highlight would not help there.
 */
export function wordDiff(oldLine: string, newLine: string): { left: WordPiece[]; right: WordPiece[] } {
  const whole = () => ({
    left: oldLine ? [{ text: oldLine, changed: true }] : [],
    right: newLine ? [{ text: newLine, changed: true }] : [],
  });
  if (oldLine.length > WORD_LIMIT || newLine.length > WORD_LIMIT) return whole();
  const a = oldLine.match(TOKEN_RE) ?? [];
  const b = newLine.match(TOKEN_RE) ?? [];
  if (a.length > TOKEN_LIMIT || b.length > TOKEN_LIMIT) return whole();
  const dp: number[][] = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const keepA = new Array<boolean>(a.length).fill(false);
  const keepB = new Array<boolean>(b.length).fill(false);
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      keepA[i++] = true;
      keepB[j++] = true;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      i++;
    } else {
      j++;
    }
  }
  return { left: pieces(a, keepA), right: pieces(b, keepB) };
}

/** One run of added / removed lines inside a hunk: `rows[start, end)`. */
export type ChangeBlock = { start: number; end: number; adds: number; dels: number };

/** Contiguous change runs; a "\ No newline" note sticks to the run it follows. */
export function changeBlocks(hunk: DiffHunk): ChangeBlock[] {
  const out: ChangeBlock[] = [];
  let i = 0;
  while (i < hunk.rows.length) {
    if (hunk.rows[i].kind !== "add" && hunk.rows[i].kind !== "del") {
      i++;
      continue;
    }
    const start = i;
    let adds = 0;
    let dels = 0;
    while (i < hunk.rows.length && hunk.rows[i].kind !== "ctx") {
      if (hunk.rows[i].kind === "add") adds++;
      if (hunk.rows[i].kind === "del") dels++;
      i++;
    }
    out.push({ start, end: i, adds, dels });
  }
  return out;
}

/**
 * A patch with only `block`'s changes. Lines of the other blocks become the
 * side the patch is applied against:
 * - `"old"` (applied forward to the index, i.e. stage this block): their removed
 *   lines are still in the index, so they turn into context; added lines go;
 * - `"new"` (applied in reverse, i.e. revert this block): their added lines are
 *   in the file, so they turn into context; removed lines go.
 * Hunk header counts are left to `git apply --recount`.
 */
export function blockPatch(preamble: string, hunk: DiffHunk, block: ChangeBlock, side: "old" | "new"): string {
  const body: string[] = [];
  let lastKept = false;
  hunk.rows.forEach((row, i) => {
    const inside = i >= block.start && i < block.end;
    if (row.kind === "note") {
      if (lastKept) body.push(`\\ ${row.text}`);
      return;
    }
    if (inside) {
      body.push(`${row.kind === "add" ? "+" : row.kind === "del" ? "-" : " "}${row.text}`);
      lastKept = true;
      return;
    }
    const keep = row.kind === "ctx" || (row.kind === "del" && side === "old") || (row.kind === "add" && side === "new");
    if (keep) body.push(` ${row.text}`);
    lastKept = keep;
  });
  const text = [preamble.trimEnd(), hunk.header, ...body].filter(Boolean).join("\n");
  return `${text}\n`;
}