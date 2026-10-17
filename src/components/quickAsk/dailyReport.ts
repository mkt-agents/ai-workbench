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

/** Group paths so the model sees areas ("src/components ×4") instead of a file list. */
export function summarizeAreas(paths: string[]): string {
  const tops = new Map<string, number>();
  for (const p of paths) {
    const segs = p.split(/[\\/]/).filter(Boolean);
    const top = segs.slice(0, Math.min(2, segs.length)).join("/") || p;
    tops.set(top, (tops.get(top) || 0) + 1);
  }
  return [...tops.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .slice(0, 6)
    .map(([k, n]) => (n > 1 ? `${k} ×${n}` : k))
    .join(", ");
}

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

export const DAILY_REPORT_SYSTEM = [
  "You are writing the user's own daily report from today's git notes.",
  "The notes are private source material. Do not quote them, do not keep their headings, and do not list hashes, branches, or file paths.",
  "Write in the user's language. A Chinese question gets a Chinese report, in the first person, the way someone would paste it into a team chat.",
  "Use the file names to tell what the work actually was. Commit subjects like fix, update, or wip are not enough on their own.",
  "Merge commits that touch the same area into one sentence a person would say. If a subject is vague, describe the area from the file names and do not invent motives or results you cannot see.",
  "Shape: one opening sentence, then a few short sentences or bullets. No numbered headings like 一、二、三. About 150 to 250 Chinese characters, short enough to paste into a team chat.",
  "If something is still uncommitted, add one sentence about that unfinished piece.",
  "Skip a tomorrow section unless that unfinished item clearly continues.",
  "Omit a repo that has no commits today and nothing uncommitted.",
  "If nothing happened, say so in one sentence.",
  "Do not mention the workbench, the model, or that context was attached.",
  "Do not paste diffs, do not add a follow-up question, and do not offer to rewrite it.",
].join(" ");
