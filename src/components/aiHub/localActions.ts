import type { QuickAppLauncher } from "../../core/types";

export type LocalAction =
  | { kind: "tab"; tab: string; label: string; gitSub?: "repos" | "commit" | "report"; toolId?: string }
  | { kind: "app"; label: string; path: string; args: string };

type Alias = {
  names: string[];
  label: string;
  tab: string;
  gitSub?: "repos" | "commit" | "report";
  toolId?: string;
};

/** Pages the main window can open, plus the names people actually say. */
const PAGES: Alias[] = [
  { tab: "git", gitSub: "commit", label: "提交", names: ["提交页", "提交", "git 提交"] },
  { tab: "git", gitSub: "report", label: "报告", names: ["变更报告", "git 报告", "报告"] },
  { tab: "git", gitSub: "repos", label: "仓库", names: ["仓库页", "git 仓库", "仓库"] },
  { tab: "git", label: "Git 管理", names: ["git 管理", "git管理", "工程管理", "版本管理", "git"] },
  { tab: "ai-chat", label: "Harness", names: ["harness", "deepseek", "ai 对话"] },
  { tab: "ai-models", label: "模型配置", names: ["模型配置", "模型"] },
  { tab: "snippets", label: "模板片段", names: ["模板片段", "片段页", "snippets"] },
  { tab: "drafts", label: "工作暂存", names: ["工作暂存", "暂存"] },
  { tab: "cursor-accounts", label: "Cursor", names: ["cursor 账号", "cursor"] },
  { tab: "account-quota", label: "账号配额", names: ["账号配额", "账号额度", "配额", "额度", "签到"] },
  { tab: "runtime", label: "环境变量", names: ["环境变量", "runtime"] },
  { tab: "plugins", label: "网页工具", names: ["网页工具"] },
  { tab: "cloudflared", label: "内网穿透", names: ["内网穿透", "cloudflared", "穿透"] },
  { tab: "hosts", label: "Hosts 管理", names: ["hosts 管理", "hosts"] },
  { tab: "devtools", label: "工具箱", names: ["工具箱", "小工具", "devtools"] },
  { tab: "app-launcher", label: "快捷应用", names: ["快捷应用", "应用启动器"] },
  { tab: "settings", label: "设置", names: ["设置", "settings"] },
];

const TOOLS: Alias[] = [
  { tab: "devtools", toolId: "json", label: "JSON", names: ["json", "json 格式化"] },
  { tab: "devtools", toolId: "encoder", label: "编解码", names: ["编解码", "base64"] },
  { tab: "devtools", toolId: "timestamp", label: "时间戳", names: ["时间戳"] },
  { tab: "devtools", toolId: "cron", label: "Cron", names: ["cron"] },
  { tab: "devtools", toolId: "uuid", label: "UUID", names: ["uuid"] },
  { tab: "devtools", toolId: "hash", label: "Hash", names: ["hash", "哈希"] },
  { tab: "devtools", toolId: "jwt", label: "JWT", names: ["jwt"] },
  { tab: "devtools", toolId: "regex", label: "正则", names: ["正则", "regex"] },
  { tab: "devtools", toolId: "calc", label: "计算器", names: ["计算器"] },
  { tab: "devtools", toolId: "ports", label: "端口查杀", names: ["端口查杀", "端口", "port"] },
];

const VERB_RE =
  /^(?:请|帮我|麻烦)?(打开|进入|跳到|跳转|切到|去|启动|运行)(?:一下|下)?\s*(.*)$/i;
const BA_RE = /^(?:请|帮我|麻烦)?把\s*(.*?)\s*(?:打开|启动|运行)(?:一下|下)?$/i;

export type OpenSuggestion = {
  id: string;
  label: string;
  group: "page" | "tool" | "app" | "installed";
  action: LocalAction;
};

/** A program found in the Start Menu / Desktop shortcuts (`scan_desktop_apps`). */
export type InstalledApp = { name: string; path: string };

const INSTALLED_LIMIT = 6;

/** A real question that happens to start with 去/打开, not a name to open. */
const SENTENCE_RE = /[？?]|然后|并且|为什么|怎么|报错|解释|日报|改了|看看|什么/;

export function parseOpenInput(text: string): { launching: boolean; target: string } | null {
  const trimmed = text.trim();
  const verb = trimmed.match(VERB_RE);
  const ba = verb ? null : trimmed.match(BA_RE);
  if (!verb && !ba) return null;
  const raw = (verb ? verb[2] : ba?.[1]) || "";
  const target = cleanTarget(raw);
  if (target.length > 16 || SENTENCE_RE.test(raw)) return null;
  const word = verb?.[1] || "";
  return { launching: word === "启动" || word === "运行" || /启动|运行/.test(trimmed), target };
}

function norm(s: string): string {
  return s.trim().toLowerCase().replace(/\s+/g, "");
}

function cleanTarget(raw: string): string {
  return raw
    .replace(/[。.!！?？]+$/g, "")
    .replace(/(?:页面|功能|工具|应用|程序|一下|吧|啊)$/g, "")
    .trim();
}

function scoreName(target: string, alias: string): number {
  const n = norm(target);
  const a = norm(alias);
  if (!n || !a) return 0;
  if (n === a) return 100 + a.length;
  if (a.length >= 2 && n.includes(a)) return 70 + a.length;
  if (n.length >= 2 && a.includes(n)) return 50 + n.length;
  return 0;
}

function pageSuggestion(page: Alias): OpenSuggestion {
  return {
    id: `tab:${page.tab}:${page.gitSub || ""}:${page.toolId || ""}:${page.label}`,
    label: page.label,
    group: page.toolId ? "tool" : "page",
    action: {
      kind: "tab",
      tab: page.tab,
      label: page.label,
      gitSub: page.gitSub,
      toolId: page.toolId,
    },
  };
}

function appSuggestion(launcher: QuickAppLauncher): OpenSuggestion {
  return {
    id: `app:${launcher.id}`,
    label: launcher.name,
    group: "app",
    action: { kind: "app", label: launcher.name, path: launcher.path, args: launcher.args || "" },
  };
}

/**
 * How well a spoken name fits an installed program's display name (spaces and case
 * ignored): exact, name contains it, it contains the name, then its characters in
 * order ("360浏览器" → "360安全浏览器"). Mirrors `match_app_name` in agent.rs.
 */
export function installedScore(target: string, name: string): number {
  const n = norm(target);
  const a = norm(name);
  if (!n || !a) return 0;
  if (n === a) return 100;
  if (a.includes(n)) return 80;
  if (a.length >= 2 && n.includes(a)) return 70;
  if ([...n].length >= 3 && inOrder(n, a)) return 60;
  return 0;
}

function inOrder(needle: string, hay: string): boolean {
  let at = 0;
  for (const ch of needle) {
    const found = hay.indexOf(ch, at);
    if (found < 0) return false;
    at = found + ch.length;
  }
  return true;
}

function installedSuggestion(app: InstalledApp): OpenSuggestion {
  return {
    id: `installed:${app.path}`,
    label: app.name,
    group: "installed",
    action: { kind: "app", label: app.name, path: app.path, args: "" },
  };
}

function bestAliasScore(target: string, page: Alias): number {
  return page.names.reduce((best, name) => Math.max(best, scoreName(target, name)), 0);
}

/**
 * Suggestions while the composer is an "open …" phrase.
 * `null` means this text is a normal question. An empty array means nothing matched.
 */
export function openSuggestions(
  text: string,
  launchers: QuickAppLauncher[],
  installed: InstalledApp[] = [],
): OpenSuggestion[] | null {
  const parsed = parseOpenInput(text);
  if (!parsed) return null;
  const pages = [...TOOLS, ...PAGES].map((page) => ({
    item: pageSuggestion(page),
    score: parsed.target ? bestAliasScore(parsed.target, page) : 1,
  }));
  const apps = launchers.map((launcher) => ({
    item: appSuggestion(launcher),
    score: parsed.target ? scoreName(parsed.target, launcher.name) : 1,
  }));
  const rows = parsed.launching ? [...apps, ...pages] : [...pages, ...apps];
  if (!parsed.target) return rows.map((row) => row.item);
  const local = rows
    .filter((row) => row.score >= 70)
    .sort((a, b) => b.score - a.score)
    .map((row) => row.item);
  // Installed programs come after pages and quick apps; skip ones already saved as quick apps.
  const saved = new Set(launchers.map((l) => l.path.toLowerCase()));
  const programs = installed
    .filter((app) => !saved.has(app.path.toLowerCase()))
    .map((app) => ({ app, score: installedScore(parsed.target, app.name) }))
    .filter((row) => row.score > 0)
    .sort((a, b) => b.score - a.score || a.app.name.length - b.app.name.length)
    .slice(0, INSTALLED_LIMIT)
    .map((row) => installedSuggestion(row.app));
  return [...local, ...programs];
}
