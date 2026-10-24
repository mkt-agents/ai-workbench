/**
 * 输入语法层：`/` 命令、`@` 文件引用共用一个面板。
 *
 * 以前 `/` 只是片段检索（纯文本模板），没有任何「命令」语义。这里把三类条目
 * （内置命令 / 技能 / 片段）统一成 `PaletteItem`，键盘与渲染只认识这一种形状；
 * 真正执行什么由 `AiHubApp` 注入的回调决定。
 */

export type PaletteItem =
  | { kind: "command"; id: string; label: string; hint: string }
  | { kind: "skill"; id: string; name: string; label: string; hint: string; source: string; allowedTools: string[] }
  | { kind: "snippet"; id: string; label: string; hint: string }
  | { kind: "file"; id: string; label: string; hint: string; path: string };

/** 内置命令（不需要项目配置就能用）。 */
export const BUILTIN_COMMANDS: { id: string; slash: string; labelKey: string; hintKey: string }[] = [
  { id: "clear", slash: "/clear", labelKey: "cmd.clear", hintKey: "cmd.clearHint" },
  { id: "plan", slash: "/plan", labelKey: "cmd.plan", hintKey: "cmd.planHint" },
  { id: "daily", slash: "/daily", labelKey: "cmd.daily", hintKey: "cmd.dailyHint" },
  { id: "memory", slash: "/memory", labelKey: "cmd.memory", hintKey: "cmd.memoryHint" },
  { id: "config", slash: "/config", labelKey: "cmd.config", hintKey: "cmd.configHint" },
];

/** 行内 `/xxx`：只在空白或行首之后触发，避免把 `a/b` 当成命令。 */
const SLASH_RE = /(?:^|\s)\/([^\s/]*)$/;
/** 行内 `@xxx`。 */
const AT_RE = /(?:^|\s)@([^\s@]*)$/;

export function slashQuery(input: string): string | null {
  const hit = SLASH_RE.exec(input);
  return hit ? hit[1] : null;
}

export function atQuery(input: string): string | null {
  const hit = AT_RE.exec(input);
  return hit ? hit[1] : null;
}

/**
 * 把正在输入的 token 换成选中的内容。
 * `keepTrailingSpace`：命令与引用后面补一个空格，方便继续打字。
 */
export function applyCompletion(input: string, token: "slash" | "at", replacement: string): string {
  const re = token === "slash" ? SLASH_RE : AT_RE;
  const hit = re.exec(input);
  if (!hit) return input;
  const start = hit.index + hit[0].length - hit[1].length - 1;
  return `${input.slice(0, start)}${replacement} `;
}

/** 片段/技能的名字是否匹配输入（大小写不敏感、子串即可）。 */
export function matchesQuery(name: string, query: string): boolean {
  return name.toLowerCase().includes(query.trim().toLowerCase());
}
