/**
 * Shared URL helpers for the 网页工具 page.
 *
 * `matchUrlPattern` used to exist twice — once in `PluginBrowser.tsx` for the editor's
 * "does this URL match" preview and once in `lib/browser.ts` for the filter that decides
 * which scripts get injected. Two copies of a matcher is how a preview starts lying, so
 * there is now exactly one, and the injected in-page guard is generated from it.
 */

/** `example.com` → `https://example.com/`. Returns null for anything not http(s). */
export function normalizeHttpUrl(raw: string): URL | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const withProtocol = /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(trimmed) ? trimmed : `https://${trimmed}`;
  try {
    const u = new URL(withProtocol);
    if (u.protocol !== "http:" && u.protocol !== "https:") return null;
    if (!u.hostname) return null;
    return u;
  } catch {
    return null;
  }
}

/**
 * Chrome/Tampermonkey-style match pattern: `*` is any run of characters, `?` one
 * character, everything else literal. A pattern is matched against the whole URL.
 */
export function matchUrlPattern(pattern: string, url: string): boolean {
  if (pattern === "<all_urls>") return true;
  const trimmed = pattern.trim();
  if (!trimmed) return false;
  let regex = "";
  for (const ch of trimmed) {
    if (ch === "*") regex += ".*";
    else if (ch === "?") regex += ".";
    else if ("+.^${}()|[]\\".includes(ch)) regex += "\\" + ch;
    else regex += ch;
  }
  try {
    return new RegExp("^" + regex + "$").test(url);
  } catch {
    return false;
  }
}

/**
 * Row ids must be unique: `db_save` clears the table and re-inserts every row in one
 * transaction, so one colliding primary key fails the write for the whole table.
 */
export function newId(prefix?: string): string {
  const id = crypto.randomUUID();
  return prefix ? `${prefix}-${id}` : id;
}
