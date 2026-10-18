/** Questions that want a standup written from today's commits, not a file dump. */
const DAILY_RE =
  /日报|每日总结|今日总结|今天做了什么|今日改动|今天的改动|今天改了什么|工作汇报|工作总结|standup|daily report|what did i do today/i;

export function isDailyReport(question: string): boolean {
  return DAILY_RE.test(question.trim());
}

export function localDayStart(now: Date = new Date()): string {
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `${y}-${m}-${d} 00:00:00`;
}

export type DailyCommit = { subject: string; when: string; files: string[] };

const REVISE_RE = /短一点|短一些|长一点|口语|正式|自然一点|改得|重写|再写|简洁|详细一点|换种|不要分点|少一点/;

/** A short follow-up that should rewrite the report just produced, not start a new question. */
export function isDailyRevision(question: string): boolean {
  const q = question.trim();
  return q.length > 0 && q.length <= 40 && REVISE_RE.test(q);
}

function fileLine(files: string[]): string {
  if (files.length === 0) return "";
  const shown = files.slice(0, 12);
  const more = files.length > shown.length ? ` +${files.length - shown.length}` : "";
  return `\n  files: ${shown.join(", ")}${more}`;
}
export type DailyRepoNote = {
  name: string;
  commits: DailyCommit[];
  /** Uncommitted paths, used only to name the area still in progress. */
  dirtyPaths: string[];
  unreadable?: boolean;
};

/** Source notes for the model. Not a template for the report itself. */
export function formatDailyNotes(repos: DailyRepoNote[]): string {
  if (repos.length === 0) return "No repository is selected.";
  return repos
    .map((r) => {
      const commits = r.commits.length
        ? r.commits.map((c) => `- ${c.subject} (${c.when})${fileLine(c.files)}`).join("\n")
        : "- none since local midnight";
      if (r.unreadable) return `Repo ${r.name}: git history could not be read.`;
      const dirty = r.dirtyPaths.length
        ? `Uncommitted files: ${fileLine(r.dirtyPaths).replace("\n  files: ", "")}`
        : "Nothing uncommitted.";
      return `Repo ${r.name}:\nCommits today:\n${commits}\n${dirty}`;
    })
    .join("\n\n");
}

/** `chat`: one opening line + a few points, pasteable into a team chat. `sections`: 今日完成 / 进行中 / 明日计划. */
export type DailyStyle = "chat" | "sections";
export const DAILY_STYLES: DailyStyle[] = ["chat", "sections"];

const DAILY_COMMON = [
  "You are writing the user's own daily report from today's git notes.",
  "The notes are private source material. Do not quote them, do not keep their headings, and do not list hashes, branches, or file paths.",
  "Write in the user's language. A Chinese question gets a Chinese report, in the first person.",
  "Use the file names to tell what the work actually was. Commit subjects like fix, update, or wip are not enough on their own.",
  "Merge commits that touch the same area into one point a person would say. If a subject is vague, describe the area from the file names and do not invent motives or results you cannot see.",
  "Omit a repo that has no commits today and nothing uncommitted. If nothing happened, say so in one sentence.",
  "Output only the report text itself, as plain text: no code block, no Markdown headings (#), no tables, no bold, no emoji.",
  "Points start with \"- \". Each point is one short sentence.",
  "Do not mention the workbench, the model, or that context was attached.",
  "Do not paste diffs, do not add a follow-up question, do not offer to rewrite it, and never add a note about the report itself.",
];

const DAILY_SHAPE: Record<DailyStyle, string[]> = {
  chat: [
    "Shape: one opening sentence summing up the day, then 3 to 5 points. About 150 to 250 Chinese characters, short enough to paste into a team chat.",
    "If something is still uncommitted, end with one point about that unfinished piece. No tomorrow section.",
  ],
  sections: [
    "Shape: up to three parts, each a label line followed by its points: 今日完成：, 进行中：, 明日计划： (use the same labels translated if the report is not Chinese).",
    "今日完成 covers committed work. 进行中 covers uncommitted work only. 明日计划 only lists what clearly continues from 进行中; never invent plans.",
    "Leave out a part that would be empty. About 150 to 300 Chinese characters in total.",
  ],
};

export function dailyReportSystem(style: DailyStyle): string {
  return [...DAILY_COMMON, ...DAILY_SHAPE[style]].join(" ");
}

const CJK = /[\u4e00-\u9fff\u3400-\u4dbf]/;
// Pictographic emoji plus the variation selector / joiner that glue them together.
const EMOJI = /[\p{Extended_Pictographic}\u{FE0F}\u{200D}]/gu;
const TABLE_RULE = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;

/**
 * Free models ignore "plain text" and wrap the report in a ```markdown block, add
 * emoji tables and a closing note about the report. Strip all of that so the
 * result pastes cleanly.
 */
export function cleanDailyReport(raw: string): string {
  let text = raw.replace(/\r\n/g, "\n").trim();
  // One fenced block holding the report (possibly followed by commentary): keep its inside.
  const fence = /^([\s\S]*?)```[a-zA-Z]*\n([\s\S]*?)\n```([\s\S]*)$/.exec(text);
  if (fence && fence[2].trim().length > fence[1].trim().length) {
    // Blank lines around the inside keep a trailing note its own paragraph (dropped below).
    text = `${fence[1].trim()}\n\n${fence[2]}\n\n${fence[3].trim()}`.trim();
  }
  const lines: string[] = [];
  for (const rawLine of text.split("\n")) {
    let line = rawLine.replace(EMOJI, "").replace(/\s+$/, "");
    if (TABLE_RULE.test(line)) continue;
    // `| a | b |` table rows become points; a header row of generic words is dropped.
    if (/^\s*\|.*\|\s*$/.test(line)) {
      const cells = line.split("|").map((c) => c.trim()).filter(Boolean);
      if (cells.length === 0 || /^(分类|类别|类型|项目|模块|内容|关键内容|事项|说明)$/.test(cells[0])) continue;
      line = `- ${cells.join("：")}`;
    }
    line = line
      .replace(/^\s*#{1,6}\s*/, "")
      .replace(/\*\*(.+?)\*\*/g, "$1")
      // Inline code ticks read as noise once pasted into a chat.
      .replace(/`([^`\n]+)`/g, "$1")
      .replace(/^(\s*)[*+]\s+/, "$1- ");
    lines.push(line);
  }
  let paragraphs = lines.join("\n").split(/\n{2,}/).map((p) => p.trim()).filter(Boolean);
  // A Chinese report followed by an English "This report captures …" paragraph: drop the tail.
  if (paragraphs.some((p) => CJK.test(p))) {
    while (paragraphs.length > 1 && !CJK.test(paragraphs[paragraphs.length - 1])) {
      paragraphs = paragraphs.slice(0, -1);
    }
  }
  return paragraphs.join("\n\n");
}
