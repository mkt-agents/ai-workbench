/** App release identity — bump these three files together: package.json, tauri.conf.json, Cargo.toml */
export const APP_GITHUB_REPO = "mkt-agents/ai-workbench";
export const APP_RELEASES_URL = `https://github.com/${APP_GITHUB_REPO}/releases`;
export const APP_RELEASES_API = `https://api.github.com/repos/${APP_GITHUB_REPO}/releases/latest`;
export const APP_GITEE_URL = `https://gitee.com/${APP_GITHUB_REPO}`;
export const APP_GITEE_RELEASES_URL = `${APP_GITEE_URL}/releases`;
/** Gitee v5 has no /latest endpoint — the list comes newest-first, take [0]. */
export const APP_GITEE_RELEASES_API = `https://gitee.com/api/v5/repos/${APP_GITHUB_REPO}/releases`;

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

async function fetchJson(url: string, timeoutMs: number): Promise<unknown> {
  const res = await fetch(url, {
    headers: { Accept: "application/json" },
    signal: AbortSignal.timeout(timeoutMs),
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

/** Newest release tag on Gitee (list is unordered in practice → max by semver), or null. */
async function fetchGiteeLatest(): Promise<{ tag: string; htmlUrl: string } | null> {
  try {
    const data = (await fetchJson(APP_GITEE_RELEASES_API, 6000)) as Array<{
      tag_name?: string;
      html_url?: string;
    }>;
    if (!Array.isArray(data) || data.length === 0) return null;
    let tag = "";
    let htmlUrl = APP_GITEE_RELEASES_URL;
    for (const r of data) {
      if (r.tag_name && compareVersions(r.tag_name, tag || "0") > 0) {
        tag = r.tag_name;
        htmlUrl = r.html_url || htmlUrl;
      }
    }
    return tag ? { tag, htmlUrl } : null;
  } catch {
    return null;
  }
}

/** Newest release tag on GitHub, or null when unreachable/none. */
async function fetchGithubLatest(): Promise<{ tag: string; htmlUrl: string } | null> {
  try {
    const res = await fetch(APP_RELEASES_API, {
      headers: { Accept: "application/vnd.github+json" },
      signal: AbortSignal.timeout(8000),
    });
    if (!res.ok) return null;
    const data = (await res.json()) as { tag_name?: string; html_url?: string };
    return data.tag_name ? { tag: data.tag_name, htmlUrl: data.html_url || APP_RELEASES_URL } : null;
  } catch {
    return null;
  }
}

/**
 * Multi-source check for the main window's startup banner and the Settings page.
 * Gitee first (reachable on CN networks), GitHub as fallback — a source that
 * answers but lags behind cannot downgrade, because both are queried and the
 * higher tag wins. Both unreachable → error (callers stay silent on startup).
 */
export async function checkForUpdatesMultiSource(localVersion: string): Promise<UpdateCheckResult> {
  const [gitee, github] = await Promise.all([fetchGiteeLatest(), fetchGithubLatest()]);
  const picks = [gitee, github].filter((p): p is { tag: string; htmlUrl: string } => !!p);
  if (picks.length === 0) {
    return { status: "error", local: localVersion, message: "Gitee/GitHub 均不可达" };
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
