import type { CursorAccount } from "./types";

/** Match a saved account to the live Cursor login (case-insensitive email/name). */
export function matchCursorAccount(
  account: CursorAccount,
  live: { email: string; name: string; isLoggedIn?: boolean } | null | undefined
): boolean {
  if (!live || live.isLoggedIn === false) return false;
  const liveEmail = (live.email || "").toLowerCase();
  const liveName = (live.name || "").trim();
  if (!liveEmail && !liveName) return false;

  const email = (account.email || "").toLowerCase();
  const name = (account.name || "").toLowerCase();

  if (liveEmail && (email === liveEmail || name === liveEmail)) return true;
  if (liveName) {
    const liveNameLower = liveName.toLowerCase();
    if (email === liveNameLower || name === liveNameLower) return true;
    if (account.email === liveName || account.name === liveName) return true;
  }
  return false;
}
