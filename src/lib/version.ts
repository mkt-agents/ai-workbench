/** App release identity — bump these three files together: package.json, tauri.conf.json, Cargo.toml */
export const APP_GITHUB_REPO = "mkt-agents/ai-workbench";
export const APP_RELEASES_URL = `https://github.com/${APP_GITHUB_REPO}/releases`;
export const APP_RELEASES_API = `https://api.github.com/repos/${APP_GITHUB_REPO}/releases/latest`;
export const APP_GITEE_URL = `https://gitee.com/${APP_GITHUB_REPO}`;
export const APP_GITEE_RELEASES_URL = `${APP_GITEE_URL}/releases`;
/** Gitee v5 has no /latest endpoint — the list comes newest-first, take [0]. */
export const APP_GITEE_RELEASES_API = `https://gitee.com/api/v5/repos/${APP_GITHUB_REPO}/releases`;
/**
 * jsDelivr 的 GitHub 包索引：按 git tag 列版本，带 `Access-Control-Allow-Origin: *`
 * 且不占 GitHub API 配额（未登录只有 60 次/小时·IP，超限即 403），所以作为兜底源。
 */
export const APP_JSDELIVR_TAGS_API = `https://data.jsdelivr.com/v1/packages/gh/${APP_GITHUB_REPO}`;

export function normalizeVersion(v: string): string {
  return v.replace(/^v/i, "").split("-")[0].trim();
}

/**
 * Semver ordering: negative when `a` < `b`, positive when `a` > `b`, else 0.
 *
 * Prereleases are compared properly — `0.1.5-rc.2` > `0.1.5-rc.1`, `0.1.5-rc.1` <
 * `0.1.5`, and numeric prerelease identifiers rank below alphabetic ones
 * (`0.1.5-alpha.2` < `0.1.5-rc.1`). Dropping the suffix (the previous behaviour) made
 * every DSH release look identical, since DSH only ships `-rc.N` builds.
 */
export function compareVersions(a: string, b: string): number {
  const parse = (v: string) => {
    const [release = "", ...pre] = v
      .trim()
      .replace(/^v/i, "")
      .split("+")[0]
      .split("-");
    return {
      nums: release.split(".").map((n) => Number(n) || 0),
      pre: pre.join("-"),
    };
  };

  const left = parse(a);
  const right = parse(b);
  const len = Math.max(left.nums.length, right.nums.length);
  for (let i = 0; i < len; i++) {
    const x = left.nums[i] || 0;
    const y = right.nums[i] || 0;
    if (x !== y) return x < y ? -1 : 1;
  }

  // Same release number: a version without a prerelease is the greater one.
  if (!left.pre && !right.pre) return 0;
  if (!left.pre) return 1;
  if (!right.pre) return -1;

  const l = left.pre.split(".");
  const r = right.pre.split(".");
  for (let i = 0; i < Math.max(l.length, r.length); i++) {
    const x = l[i];
    const y = r[i];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    if (x === y) continue;
    const xNum = /^\d+$/.test(x);
    const yNum = /^\d+$/.test(y);
    if (xNum && yNum) return Number(x) < Number(y) ? -1 : 1;
    if (xNum) return -1;
    if (yNum) return 1;
    return x < y ? -1 : 1;
  }
  return 0;
}

/** Returns true if remote is newer than local. */
export function isNewerVersion(local: string, remote: string): boolean {
  return compareVersions(remote, local) > 0;
}

export type UpdateCheckResult =
  | { status: "upToDate"; local: string; remote: string }
  | { status: "updateAvailable"; local: string; remote: string; htmlUrl: string }
  | { status: "error"; local: string; message: string };

/** 每个源的返回：拿到 tag，或者带上"为什么没拿到"的原因。 */
type Latest = { tag: string; htmlUrl: string };
type SourceResult = Latest | { reason: string };
const isLatest = (r: SourceResult): r is Latest => "tag" in r;

/** HTTP 状态要带在异常上：403/404 是"答了但没用"，与"网络不可达"必须区分开报给用户。 */
class HttpError extends Error {
  constructor(readonly status: number) {
    super(`HTTP ${status}`);
  }
}

function describeFailure(e: unknown): string {
  if (e instanceof HttpError) {
    if (e.status === 404) return "HTTP 404（无该仓库或未发布 release）";
    if (e.status === 403) return "HTTP 403（限流：未登录 60 次/小时·IP，稍后再试）";
    return `HTTP ${e.status}`;
  }
  const name = (e as { name?: string })?.name;
  if (name === "TimeoutError" || name === "AbortError") return "请求超时";
  return "网络不可达";
}

async function fetchJson(url: string, timeoutMs: number): Promise<unknown> {
  const res = await fetch(url, {
    headers: { Accept: "application/json" },
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!res.ok) throw new HttpError(res.status);
  return res.json();
}

/** Newest release tag on Gitee (list is unordered in practice → max by semver). */
async function fetchGiteeLatest(): Promise<SourceResult> {
  try {
    const data = (await fetchJson(APP_GITEE_RELEASES_API, 6000)) as Array<{
      tag_name?: string;
      html_url?: string;
    }>;
    if (!Array.isArray(data) || data.length === 0) return { reason: "没有任何 release" };
    let tag = "";
    let htmlUrl = APP_GITEE_RELEASES_URL;
    for (const r of data) {
      if (r.tag_name && compareVersions(r.tag_name, tag || "0") > 0) {
        tag = r.tag_name;
        htmlUrl = r.html_url || htmlUrl;
      }
    }
    return tag ? { tag, htmlUrl } : { reason: "release 都没有 tag" };
  } catch (e) {
    return { reason: describeFailure(e) };
  }
}

/** Newest release tag on GitHub. */
async function fetchGithubLatest(): Promise<SourceResult> {  try {
    const res = await fetch(APP_RELEASES_API, {
      headers: { Accept: "application/vnd.github+json" },
      signal: AbortSignal.timeout(8000),
    });
    if (!res.ok) throw new HttpError(res.status);
    const data = (await res.json()) as { tag_name?: string; html_url?: string };
    return data.tag_name
      ? { tag: data.tag_name, htmlUrl: data.html_url || APP_RELEASES_URL }
      : { reason: "没有 latest release" };
  } catch (e) {
    return { reason: describeFailure(e) };
  }
}

/** 兜底源：jsDelivr 的 tag 索引（无配额、可跨域）。版本号不带 v 前缀，下载链接按 v<version> 拼。 */
async function fetchJsDelivrLatest(): Promise<SourceResult> {
  try {
    const data = (await fetchJson(APP_JSDELIVR_TAGS_API, 6000)) as {
      versions?: Array<{ version?: string }>;
    };
    const list = Array.isArray(data?.versions) ? data.versions : [];
    let tag = "";
    for (const v of list) {
      if (v.version && compareVersions(v.version, tag || "0") > 0) tag = v.version;
    }
    return tag ? { tag, htmlUrl: `${APP_RELEASES_URL}/tag/v${tag}` } : { reason: "没有已发布的 tag" };
  } catch (e) {
    return { reason: describeFailure(e) };
  }
}

/**
 * Multi-source check for the main window's startup banner and the Settings page.
 * 三个源并发查，取最高 tag —— 任何一个"答了但落后"都不会造成降级；全失败才报错，
 * 且报错里逐源给出原因（限流 403 / 仓库 404 / 超时 / 网络不通不再被统称为"不可达"）。
 * jsDelivr 是不占 GitHub 配额、允许跨域的兜底，GitHub API 被限流时它仍能给出新版本。
 */
export async function checkForUpdatesMultiSource(localVersion: string): Promise<UpdateCheckResult> {
  const [gitee, github, jsdelivr] = await Promise.all([
    fetchGiteeLatest(),
    fetchGithubLatest(),
    fetchJsDelivrLatest(),
  ]);
  const picks = [gitee, github, jsdelivr].filter(isLatest);
  if (picks.length === 0) {
    const why = (r: SourceResult) => ("reason" in r ? r.reason : "无版本号");
    return {
      status: "error",
      local: localVersion,
      message: `Gitee ${why(gitee)}；GitHub ${why(github)}；jsDelivr ${why(jsdelivr)}`,
    };
  }
  const best = picks.reduce((a, b) => (compareVersions(b.tag, a.tag) > 0 ? b : a));
  if (isNewerVersion(localVersion, best.tag)) {
    return {
      status: "updateAvailable",
      local: localVersion,
      remote: normalizeVersion(best.tag),
      htmlUrl: best.htmlUrl,
    };
  }
  return { status: "upToDate", local: localVersion, remote: normalizeVersion(best.tag) };
}
