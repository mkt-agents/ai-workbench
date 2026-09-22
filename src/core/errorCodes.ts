/**
 * The single parser for backend error codes.
 *
 * New commands fail with `Err("[E_NETWORK] 说明文字")`: the bracketed code is
 * the stable, machine-readable half (what *kind* of failure it is), the text
 * after it is for humans. UI code must never show the raw `[E_…]` token —
 * route error strings through `errorCode` / `formatError` here instead.
 */
import type { TFunction } from "i18next";

const CODE_RE = /^\[E_([A-Z][A-Z0-9_]*)\]\s*/;

/** Lowercased code without the `E_` prefix, or `null` when the error carries none. */
export function errorCode(err: unknown): string | null {
  const match = CODE_RE.exec(String(err));
  return match ? match[1].toLowerCase() : null;
}

export function isCancelledError(err: unknown): boolean {
  return errorCode(err) === "cancelled";
}

/** The message with the `[E_XXX] ` prefix removed. */
export function cleanErrorMessage(err: unknown): string {
  return String(err).replace(CODE_RE, "");
}

/**
 * Localized label for a code (`""` when the locale has none — callers then
 * fall back to the bare message). Codes map onto the existing `errors.*`
 * table in `common`, keyed `E_NETWORK` style.
 */
export function errorCodeLabel(t: TFunction<"common">, code: string): string {
  return t(`errors.E_${code.toUpperCase()}`, { defaultValue: "" });
}

/** One-line, UI-ready text: localized code label plus the human message. */
export function formatError(t: TFunction<"common">, err: unknown): string {
  const message = cleanErrorMessage(err);
  const code = errorCode(err);
  const label = code ? errorCodeLabel(t, code) : "";
  return label ? `${label}：${message}` : message;
}
