/** Normalize path for comparison: unify separators, strip trailing slash (keep drive root). */
export function normalizePath(path: string): string {
  let p = path.replace(/\\/g, "/").trim();
  if (p.length > 1 && p.endsWith("/")) {
    p = p.replace(/\/+$/, "");
  }
  // Windows drive letter: keep case-insensitive compare via lowercase
  return p;
}

export function pathKey(path: string): string {
  return normalizePath(path).toLowerCase();
}

/** True if `child` is strictly under `parent` (not equal). */
export function isPathUnder(child: string, parent: string): boolean {
  const c = pathKey(child);
  const p = pathKey(parent);
  if (!c || !p || c === p) return false;
  return c.startsWith(p.endsWith("/") ? p : `${p}/`);
}

export function projectNameFromPath(path: string): string {
  return normalizePath(path).split("/").filter(Boolean).pop() || path;
}

/**
 * Pick the longest workspace root that contains `repoPath`.
 * Returns workspace id/path or null if none match.
 */
export function findWorkspaceForRepo<T extends { path: string }>(
  repoPath: string,
  workspaces: T[]
): T | null {
  let best: T | null = null;
  let bestLen = -1;
  for (const ws of workspaces) {
    if (!isPathUnder(repoPath, ws.path)) continue;
    const len = pathKey(ws.path).length;
    if (len > bestLen) {
      best = ws;
      bestLen = len;
    }
  }
  return best;
}
