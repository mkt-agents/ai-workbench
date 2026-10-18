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
