/** App release identity — bump these three files together: package.json, tauri.conf.json, Cargo.toml */
export const APP_GITHUB_REPO = "mkt-agents/ai-workbench";
export const APP_RELEASES_URL = `https://github.com/${APP_GITHUB_REPO}/releases`;
export const APP_RELEASES_API = `https://api.github.com/repos/${APP_GITHUB_REPO}/releases/latest`;

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

export async function checkForUpdates(localVersion: string): Promise<UpdateCheckResult> {
  try {
    const res = await fetch(APP_RELEASES_API, {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!res.ok) {
      return {
        status: "error",
        local: localVersion,
        message: `GitHub API ${res.status}`,
      };
    }
    const data = (await res.json()) as { tag_name?: string; html_url?: string };
    const remote = data.tag_name || "";
    if (!remote) {
      return { status: "error", local: localVersion, message: "No release tag" };
    }
    if (isNewerVersion(localVersion, remote)) {
      return {
        status: "updateAvailable",
        local: localVersion,
        remote: normalizeVersion(remote),
        htmlUrl: data.html_url || APP_RELEASES_URL,
      };
    }
    return {
      status: "upToDate",
      local: localVersion,
      remote: normalizeVersion(remote),
    };
  } catch (e) {
    return {
      status: "error",
      local: localVersion,
      message: String(e),
    };
  }
}
