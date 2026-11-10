/**
 * Shared URL helpers for the 常用站点 page.
 *
 * `matchUrlPattern` used to exist twice — once in `PluginBrowser.tsx` for the editor's
 * "does this URL match" preview and once in `lib/browser.ts` for the filter that decides
 * which scripts get injected. Two copies of a matcher is how a preview starts lying, so
 * there is now exactly one, and the injected in-page guard is generated from it.
 */

/** Windows `D:\path\to\file.html` → `file:///D:/path/to/file.html`. */
function windowsPathToFileUrl(raw: string): string | null {
  const m = /^([A-Za-z]):([\\/].*)$/.exec(raw);
  if (!m) return null;
  const drive = m[1].toUpperCase();
  // Collapse `\\` / doubled separators from pasted Explorer paths.
  const rest = m[2].replace(/\\/g, "/").replace(/\/{2,}/g, "/");
  return `file:///${drive}:${rest}`;
}

/**
 * Normalize a 常用站点 address.
 * - `example.com` → `https://example.com/`
 * - `file:///D:/w_app/querySQL.html` kept as file
 * - `D:\w_app\querySQL.html` → `file:///D:/w_app/querySQL.html`
 * Returns null for other schemes (javascript:, data:, UNC file hosts, …).
 */
export function normalizeSiteUrl(raw: string): URL | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;

  const fromWin = windowsPathToFileUrl(trimmed);
  const withProtocol = fromWin
    ? fromWin
    : /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(trimmed)
      ? trimmed
      : `https://${trimmed}`;

  try {
    const u = new URL(withProtocol);
    if (u.protocol === "http:" || u.protocol === "https:") {
      if (!u.hostname) return null;
      return u;
    }
    if (u.protocol === "file:") {
      // Local only: `file:///D:/...` has empty host. Reject UNC `file://server/share`.
      if (u.hostname && u.hostname.toLowerCase() !== "localhost") return null;
      const path = decodeURIComponent(u.pathname || "").replace(/\\/g, "/");
      if (!path || path === "/") return null;
      // Canonicalize drive letter case for stable keys: /d:/x → /D:/x
      const drive = path.match(/^\/([a-zA-Z]):(\/.*)?$/);
      if (drive) {
        const canon = `/${drive[1].toUpperCase()}:${drive[2] || ""}`;
        return new URL(`file://${canon}`);
      }
      return u;
    }
    return null;
  } catch {
    return null;
  }
}

/** @deprecated Prefer `normalizeSiteUrl` — kept as alias for the same site rules. */
export function normalizeHttpUrl(raw: string): URL | null {
  return normalizeSiteUrl(raw);
}

/** Card / filter label: hostname for http(s), file basename (or short path) for file://. */
export function siteLabel(url: URL): string {
  if (url.protocol === "file:") {
    const path = decodeURIComponent(url.pathname || "").replace(/\\/g, "/");
    const parts = path.split("/").filter(Boolean);
    const base = parts[parts.length - 1] || path;
    if (base) return base;
    return path || "file";
  }
  return url.hostname || "—";
}

/** Default bookmark name from an address (strip www. / use file stem). */
export function siteDefaultName(url: URL): string {
  if (url.protocol === "file:") {
    const label = siteLabel(url);
    const stem = label.replace(/\.[^.]+$/, "");
    return stem || label;
  }
  return url.hostname.replace(/^www\./, "") || "site";
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

const HOTKEY_MODIFIERS = new Set([
  "control",
  "ctrl",
  "command",
  "commandorcontrol",
  "cmd",
  "cmdorcontrol",
  "meta",
  "super",
  "alt",
  "option",
  "altgr",
  "shift",
]);

export type HotkeyValidation =
  | { kind: "ok" }
  | { kind: "empty" }
  | { kind: "need-modifier" }
  | { kind: "unknown-key"; part: string };

/**
 * Tauri global-shortcut format: `Modifier+Key` — `Ctrl+Alt+K`, `CommandOrControl+Shift+1`,
 * `F9`. Structured result; the editor renders the reason with i18n so a broken
 * shortcut never reaches the OS registration layer.
 */
export function validateHotkey(hotkey: string): HotkeyValidation {
  const parts = hotkey
    .split("+")
    .map((p) => p.trim())
    .filter(Boolean);
  if (parts.length === 0) return { kind: "empty" }; // empty = no hotkey, fine
  if (parts.length < 2) return { kind: "need-modifier" };
  let hasModifier = false;
  for (const part of parts) {
    const low = part.toLowerCase();
    if (HOTKEY_MODIFIERS.has(low)) {
      hasModifier = true;
      continue;
    }
    if (/^(key[a-z]|digit\d|f([1-9]|1\d|2[0-4]))$/i.test(part)) continue;
    if (/^[a-z0-9]$/i.test(part)) continue;
    return { kind: "unknown-key", part };
  }
  if (!hasModifier) return { kind: "need-modifier" };
  return { kind: "ok" };
}

/** Same shortcut on two plugins: the second registration silently loses. */
export function findHotkeyConflict(
  hotkey: string,
  plugins: { id: string; name: string; hotkey: string }[],
  editingId?: string | null
): { name: string } | null {
  const norm = hotkey.trim().toLowerCase();
  if (!norm) return null;
  const clash = plugins.find(
    (p) => p.id !== editingId && p.hotkey.trim().toLowerCase() === norm
  );
  return clash ? { name: clash.name } : null;
}
