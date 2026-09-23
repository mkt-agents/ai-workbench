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
