import { useCallback } from "react";
import { useTranslation } from "react-i18next";

/**
 * Parse a Tauri invoke error message that may carry a structured error code.
 *
 * Rust returns errors as a JSON string when using WorkbenchError, or as a
 * plain string for legacy errors. This helper extracts the code so the UI can
 * show a localized message.
 */
export interface ParsedInvokeError {
  /** Stable error code if present, e.g. "INVALID_ACCOUNT_ID". */
  code: string | null;
  /** Human-readable detail / fallback message. */
  message: string;
}

export function parseInvokeError(raw: unknown): ParsedInvokeError {
  const text = typeof raw === "string" ? raw : String(raw ?? "");
  if (!text) return { code: null, message: "" };

  // Try to parse as WorkbenchError JSON: {"code":"...","detail":"..."}
  try {
    const obj = JSON.parse(text);
    if (obj && typeof obj === "object" && typeof obj.code === "string") {
      return { code: obj.code, message: obj.detail ?? text };
    }
  } catch {
    // not JSON — fall through
  }

  // Legacy: "[CODE] message" format from Display impl
  const bracket = text.match(/^\[([A-Z_]+)\]\s*(.*)$/s);
  if (bracket) {
    return { code: bracket[1], message: bracket[2] || text };
  }

  return { code: null, message: text };
}

/**
 * Hook that returns a function to translate an invoke error into a localized
 * string. Falls back to the raw detail message when no translation exists.
 *
 * The identity is stable per language on purpose: callers keep it in
 * `useCallback`/`useEffect` dependency lists to define their own refresh
 * function, and a fresh closure every render would re-fire those effects and
 * silently wipe any error message they just set.
 */
export function useInvokeErrorTranslator() {
  const { t } = useTranslation("common");

  return useCallback(
    (raw: unknown): string => {
      const { code, message } = parseInvokeError(raw);
      if (code) {
        const key = `errors.${code}`;
        const translated = t(key);
        // i18next returns the key when missing — fall back to detail
        if (translated !== key) return translated;
      }
      return message;
    },
    [t]
  );
}

export default useInvokeErrorTranslator;
