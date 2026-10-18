import type { TaskKind } from "./config";
import { isDailyReport } from "./dailyReport";

export type AskMode = "chat" | "project";
export type RouteOverride = "auto" | AskMode;

/** Why a question was routed the way it was — drives the pill tooltip (i18n key suffix). */
export type RouteReason =
  | "override"
  | "meta"
  | "textTask"
  | "projectTask"
  | "signal"
  | "followUp"
  | "default";

export type RouteInput = {
  question: string;
  task: TaskKind;
  /** Mode of the previous exchange in this chat (null = fresh chat). */
  lastMode: AskMode | null;
  override: RouteOverride;
};

export type RouteResult = { mode: AskMode; reason: RouteReason };

/** Tasks that work on the text the user typed — the repo is irrelevant. */
const TEXT_TASKS: TaskKind[] = ["polish", "translate", "summarize", "optimize"];
/** Tasks that are almost always about the user's code. */
const PROJECT_TASKS: TaskKind[] = ["debug"];

/** Questions about the assistant / app itself — answered from the meta block. */
const META_RE =
  /(什么模型|哪个模型|啥模型|模型是|用的.{0,4}模型|你是谁|你是什么|你叫什么|你的名字|谁开发|which model|what model|who are you|what are you|your name)/i;

/** Words that point at the user's repo / code / changes. */
const PROJECT_WORD_RE =
  /(项目|仓库|代码|源码|改动|改了|修改了|变更|提交|commit|diff|分支|branch|merge|rebase|报错|错误|异常|bug|崩溃|堆栈|这个文件|这段|函数|方法|组件|接口|模块|重构|review|代码审查|未提交|工作区|stash|pull request|\bpr\b)/i;

/** File names with a code extension, or common source-dir prefixes. ("AI/ML" must not match.) */
const PATH_RE =
  /\b[\w-]+\.(?:tsx?|jsx?|rs|py|go|java|kt|vue|css|scss|json|toml|ya?ml|sql|sh|ps1|cs|cpp|h)\b|\b(?:src|lib|app|components|pages|tests?|scripts)[\\/]/i;

/** Stack-trace / compiler-error shapes. */
const TRACE_RE =
  /(\bat\s+[\w.$<>]+\s*\(|Traceback \(most recent call last\)|error\[E\d+\]|\bTS\d{4}\b|Exception:|panicked at|\bline \d+\b)/i;

/** Short follow-ups that lean on the previous turn ("那这个呢", "为什么", "继续"). */
const FOLLOW_UP_RE = /^(那|这|它|为什么|为啥|怎么|继续|还有|再|然后|详细|展开|why|how|and|what about|continue|more)/i;
const FOLLOW_UP_MAX_CHARS = 30;

/**
 * Decide whether a ai-hub question needs workbench project context or is a
 * plain AI conversation. Pure and synchronous so the composer can preview the
 * decision live as the user types.
 */
export function routeQuestion(input: RouteInput): RouteResult {
  if (input.override !== "auto") return { mode: input.override, reason: "override" };

  const q = input.question.trim();
  if (META_RE.test(q)) return { mode: "chat", reason: "meta" };
  if (isDailyReport(q)) return { mode: "project", reason: "projectTask" };
  if (TEXT_TASKS.includes(input.task)) return { mode: "chat", reason: "textTask" };
  if (PROJECT_TASKS.includes(input.task)) return { mode: "project", reason: "projectTask" };
  if (PROJECT_WORD_RE.test(q) || PATH_RE.test(q) || TRACE_RE.test(q)) {
    return { mode: "project", reason: "signal" };
  }
  if (
    input.lastMode === "project" &&
    q.length > 0 &&
    q.length <= FOLLOW_UP_MAX_CHARS &&
    FOLLOW_UP_RE.test(q)
  ) {
    return { mode: "project", reason: "followUp" };
  }
  return { mode: "chat", reason: "default" };
}
