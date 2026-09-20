import { useMemo, useState, useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ClipboardCopy,
  Code2,
  Eraser,
  FileJson,
  History,
  Route,
  Trash2,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { storage } from "../../core/storage";
import type { JsonToolHistoryItem } from "../../core/types";

const SAMPLE_JSON = `{
  "name": "ai-workbench",
  "version": "1.0.0",
  "active": true,
  "themes": ["dark", "light", "ice"],
  "stats": {
    "users": 1280,
    "uptime": 99.97
  },
  "tags": []
}`;

let historyId = 1;
const HISTORY_MAX = 20;

/** Minimal JSONPath: supports $.a.b[0].c style paths. */
function evalJsonPath(obj: unknown, path: string): unknown {
  const p = path.trim();
  if (!p || p === "$") return obj;
  let cur: unknown = obj;
  const re = /\[(\d+)\]|([A-Za-z_][\w-]*)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(p)) !== null) {
    if (cur == null || typeof cur !== "object") return undefined;
    const key = m[1] !== undefined ? Number(m[1]) : m[2];
    cur = (cur as Record<string, unknown>)[key as string];
  }
  return cur;
}

function countNodes(value: unknown): number {
  if (Array.isArray(value)) {
    return 1 + value.reduce((acc, v) => acc + countNodes(v), 0);
  }
  if (value && typeof value === "object") {
    return (
      1 +
      Object.values(value).reduce((acc: number, v) => acc + countNodes(v), 0)
    );
  }
  return 1;
}

/**
 * Attempt to repair common JSON syntax problems.
 *
 * Tries a pipeline of increasingly aggressive normalizations; the first one
 * that yields valid JSON wins. Returns null if nothing worked.
 *
 * Handled cases:
 *  1. JSON string literal wrapping an object or array (unwrap and unescape)
 *  2. Single quotes to double quotes
 *  3. Trailing commas before } or ]
 *  4. Unquoted keys to quoted keys
 *  5. JavaScript literals such as undefined or NaN to null
 *  6. Line and block comments (JSONC style)
 *  7. Hex literals to decimal
 *  8. Wrapped JSON string literal (strip outer quotes and unescape)
 */
function tryRepairJson(input: string): string | null {
  const attempts: Array<(s: string) => string> = [
    // 0) No-op: maybe it's already valid after the caller's preprocessing
    (s) => s,

    // 1) Wrapped string literal: {"a":1} -> {"a":1}
    (s) => {
      const t = s.trim();
      if (t.length >= 2 && t.startsWith('"') && t.endsWith('"')) {
        try {
          const inner = JSON.parse(t);
          if (typeof inner === "string") return inner;
        } catch { /* not a string literal */ }
      }
      return s;
    },

    // 2) Single quotes -> double quotes (ignore escaped ')
    (s) => s.replace(/(^|[^\\])'/g, '$1"'),

    // 3) Trailing commas: ,} -> }  and  ,] -> ]
    (s) => s.replace(/,(\s*[}\]])/g, '$1'),

    // 4) Unquoted object keys:  {a:1}  ->  {"a":1}
    (s) => s.replace(/([{,]\s*)([A-Za-z_$][\w$-]*)\s*:/g, '$1"$2":'),

    // 5) JavaScript-only literals -> null
    (s) => s.replace(
      /(?<=[\s:,[])(?:undefined|NaN|Infinity|-Infinity)(?=[\s,}\]])/g,
      "null"
    ),

    // 6) Strip JSONC comment (line and block)
    (s) => s.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/[^\n\r]*/g, ""),

    // 7) Hex literals -> decimal
    (s) => s.replace(/(?<=[\s:,[])(0x[0-9a-fA-F]+)(?=[\s,}\]])/g, (_m, n) =>
      String(parseInt(n, 16))
    ),

    // 8) Wrapped JSON string literal: strip outer quotes + unescape
    (s) => {
      const t = s.trim();
      if (t.length >= 2 && t.startsWith('"') && t.endsWith('"')) {
        try {
          const inner = JSON.parse(t);
          if (typeof inner === "string") return inner;
        } catch { /* not a string literal */ }
      }
      return s;
    },
  ];

  // Try each repair pass in order. Each pass builds on the result of the
  // previous one, so fixes compose (e.g. single quotes + trailing comma).
  let current = input;
  for (const repair of attempts) {
    current = repair(current);
    try {
      JSON.parse(current);
      return current;
    } catch { /* try next */ }
  }

  return null;
}

/**
 * Escape raw control characters *inside* JSON string literals only.
 *
 * When users paste a JSON object that has been "stringified" (wrapped in quotes),
 * raw newlines/tabs inside the string are illegal in JSON - they must appear as
 * \n / \t. This walks the raw text and escapes any raw control char that occurs
 * between an unescaped pair of double quotes, leaving the rest untouched.
 */
function escapeControlsInJsonStrings(raw: string): string {
  let out = "";
  let inString = false;
  let escaped = false;
  for (let i = 0; i < raw.length; i++) {
    const ch = raw[i];
    if (inString) {
      if (escaped) {
        out += ch;
        escaped = false;
        continue;
      }
      if (ch === "\\") {
        out += ch;
        escaped = true;
        continue;
      }
      if (ch === '"') {
        out += ch;
        inString = false;
        continue;
      }
      if (ch === "\n") { out += "\\n"; continue; }
      if (ch === "\r") { out += "\\r"; continue; }
      if (ch === "\t") { out += "\\t"; continue; }
      if (ch === "\b") { out += "\\b"; continue; }
      if (ch === "\f") { out += "\\f"; continue; }
      if (ch.charCodeAt(0) < 0x20) {
        out += "\\u" + ch.charCodeAt(0).toString(16).padStart(4, "0");
        continue;
      }
      out += ch;
    } else {
      if (ch === '"') inString = true;
      out += ch;
    }
  }
  return out;
}

/**
 * Strip invisible/problematic characters: BOM, zero-width spaces, soft hyphen,
 * non-breaking space -> regular space, stray C0 control chars outside strings.
 */
function cleanInput(raw: string): string {
  return raw
    .replace(/[﻿​-‍⁠­]/g, "")
    .replace(/ /g, " ")
    .replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/g, "");
}

/** Full pre-process pipeline for lenient JSON parsing. */
function preprocessJsonInput(raw: string): string {
  return cleanInput(escapeControlsInJsonStrings(raw));
}

function truncate(text: string, max = 60): string {
  const oneLine = text.replace(/\s+/g, " ").trim();
  return oneLine.length > max ? oneLine.slice(0, max) + "…" : oneLine;
}

/** Pretty-print JSON text for display; returns the original if it isn't valid JSON. */
function prettyForDisplay(text: string, maxChars = 1000): string {
  const trimmed = text.trim();
  if (!trimmed) return trimmed;
  try {
    const parsed = JSON.parse(trimmed);
    const formatted = JSON.stringify(parsed, null, 2);
    return formatted.length > maxChars ? formatted.slice(0, maxChars) + "…" : formatted;
  } catch {
    // Not valid JSON (e.g. an error message) - just truncate the raw text
    return trimmed.length > maxChars ? trimmed.slice(0, maxChars) + "…" : trimmed;
  }
}

function JsonTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [input, setInput] = useState("");
  const [path, setPath] = useState("");
  const [message, setMessage] = useState<{
    type: "success" | "error";
    text: string;
  } | null>(null);
  const [history, setHistory] = useState<JsonToolHistoryItem[]>([]);
  const [showHistory, setShowHistory] = useState(false);
  const [compareId, setCompareId] = useState<number | null>(null);

  // Load persisted history from SQLite on mount
  useEffect(() => {
    let cancelled = false;
    storage.jsonToolHistory.load().then((saved) => {
      if (cancelled) return;
      setHistory(saved.slice(0, HISTORY_MAX));
      const maxId = saved.reduce((m, h) => Math.max(m, h.id), 0);
      if (maxId > historyId) historyId = maxId + 1;
    }).catch(() => {/* ignore */});
    return () => { cancelled = true; };
  }, []);

  // Persist history to SQLite whenever it changes
  useEffect(() => {
    if (history.length > 0) {
      storage.jsonToolHistory.save(history).catch(() => {/* ignore */});
    }
  }, [history]);

  const parsed = useMemo(() => {
    const raw = preprocessJsonInput(input).trim();
    if (!raw)
      return { ok: false as const, result: "", value: null as unknown, error: "", unwrapped: false, cleaned: false };

    const tryParse = (text: string) => {
      try {
        return { ok: true as const, value: JSON.parse(text) };
      } catch (e) {
        return { ok: false as const, error: String(e) };
      }
    };

    // 1) Direct parse
    const direct = tryParse(raw);
    if (direct.ok) {
      // If it's a plain string, it might be a wrapped JSON object/array - try unwrapping.
      if (typeof direct.value === "string") {
        const inner = tryParse(direct.value);
        if (inner.ok && inner.value && typeof inner.value === "object") {
          return {
            ok: true as const,
            result: JSON.stringify(inner.value, null, 2),
            value: inner.value,
            error: "",
            unwrapped: true,
            cleaned: raw !== input,
          };
        }
      }
      return {
        ok: true as const,
        result: JSON.stringify(direct.value, null, 2),
        value: direct.value,
        error: "",
        unwrapped: false,
        cleaned: raw !== input,
      };
    }

    // 2) Not valid JSON - check if it's a JSON string literal wrapping an object/array.
    if (raw.length >= 2 && raw.startsWith('"') && raw.endsWith('"')) {
      const inner = tryParse(raw);
      if (inner.ok && typeof inner.value === "string") {
        const obj = tryParse(inner.value);
        if (obj.ok && obj.value && typeof obj.value === "object") {
          return {
            ok: true as const,
            result: JSON.stringify(obj.value, null, 2),
            value: obj.value,
            error: "",
            unwrapped: true,
            cleaned: raw !== input,
          };
        }
      }
    }

    return { ok: false as const, result: "", value: null, error: direct.error, unwrapped: false, cleaned: raw !== input };
  }, [input]);

  const queryResult = useMemo(() => {
    if (!parsed.ok || !path.trim()) return null;
    try {
      const result = evalJsonPath(parsed.value, path);
      const text =
        typeof result === "string"
          ? result
          : result === undefined
            ? undefined
            : JSON.stringify(result, null, 2);
      return { ok: true as const, text };
    } catch (e) {
      return { ok: false as const, text: String(e) };
    }
  }, [parsed, path]);

  const stats = useMemo(() => {
    if (!parsed.ok) return null;
    return {
      chars: input.length,
      nodes: countNodes(parsed.value),
    };
  }, [parsed, input]);

  const outputText = queryResult
    ? queryResult.ok
      ? queryResult.text ?? t("json.empty")
      : `${t("json.pathError")}: ${queryResult.text}`
    : parsed.ok
      ? parsed.result
      : parsed.error;

  // Compare item
  const compareItem = compareId
    ? history.find((h) => h.id === compareId)
    : null;

  const compareOutput = useMemo(() => {
    if (!compareItem) return "";
    if (compareItem.path) {
      try {
        const obj = JSON.parse(compareItem.input);
        const result = evalJsonPath(obj, compareItem.path);
        return typeof result === "string"
          ? result
          : result === undefined
            ? t("json.empty")
            : JSON.stringify(result, null, 2);
      } catch {
        return "";
      }
    }
    return compareItem.output;
  }, [compareItem, t]);

  const flash = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 2500);
  };

  const saveToHistory = useCallback(() => {
    if (!input.trim()) return;
    const item: JsonToolHistoryItem = {
      id: historyId++,
      timestamp: Date.now(),
      input,
      output: parsed.ok ? parsed.result : parsed.error,
      path,
      ok: parsed.ok,
      nodes: stats?.nodes ?? 0,
      chars: stats?.chars ?? 0,
    };
    setHistory((prev) => [item, ...prev].slice(0, HISTORY_MAX));
  }, [input, path, parsed, stats]);

  const handleFormat = () => {
    if (parsed.ok) {
      saveToHistory();
      setInput(parsed.result);
      const msg = parsed.unwrapped
        ? t("json.unwrapped")
        : parsed.cleaned
          ? t("json.cleaned")
          : t("json.valid");
      flash("success", msg);
    } else {
      flash("error", `${t("json.invalid")}: ${parsed.error}`);
    }
  };

  const handleCompress = () => {
    if (parsed.ok) {
      saveToHistory();
      setInput(JSON.stringify(parsed.value));
      flash("success", t("json.valid"));
    } else {
      flash("error", `${t("json.invalid")}: ${parsed.error}`);
    }
  };

  const handleRepair = () => {
    const repaired = tryRepairJson(input);
    if (repaired) {
      saveToHistory();
      setInput(JSON.stringify(JSON.parse(repaired), null, 2));
      flash("success", t("json.valid"));
    } else {
      flash("error", t("json.invalid"));
    }
  };

  const handleEscape = () => {
    if (!input.trim()) return;
    saveToHistory();
    setInput(JSON.stringify(input).slice(1, -1));
    flash("success", t("json.valid"));
  };

  const handleUnescape = () => {
    const trimmed = input.trim();
    if (!trimmed) return;
    const wrapped = trimmed.startsWith('"') ? trimmed : `"${trimmed}"`;
    try {
      saveToHistory();
      setInput(JSON.parse(wrapped));
      flash("success", t("json.valid"));
    } catch (e) {
      flash("error", `${t("json.invalid")}: ${e}`);
    }
  };

  const handleCopy = async () => {
    try {
      await copy(outputText);
      flash("success", t("json.copied"));
    } catch (e) {
      flash("error", String(e));
    }
  };

  const handleCopyInput = async () => {
    try {
      await copy(input);
      flash("success", t("json.copied"));
    } catch (e) {
      flash("error", String(e));
    }
  };

  const loadFromHistory = (item: JsonToolHistoryItem) => {
    setInput(item.input);
    setPath(item.path);
    setCompareId(null);
    setShowHistory(false);
  };

  const deleteHistory = (id: number) => {
    setHistory((prev) => prev.filter((h) => h.id !== id));
    if (compareId === id) setCompareId(null);
  };

  const clearHistory = () => {
    setHistory([]);
    setCompareId(null);
  };

  const formatTime = (ts: number) => {
    const d = new Date(ts);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  };

  return (
    <div className="devtools-tool json-tool-with-history json-tool">
      {/* ── Toolbar ── */}
      <div className="json-toolbar">
        <div className="json-toolbar-group">
          <button type="button" className="json-action-btn" onClick={handleFormat}>
            {t("json.format")}
          </button>
          <button type="button" className="json-action-btn" onClick={handleCompress}>
            {t("json.compress")}
          </button>
          <button type="button" className="json-action-btn" onClick={handleRepair}>
            {t("json.repair")}
          </button>
        </div>

        <div className="json-toolbar-divider" />

        <div className="json-toolbar-group">
          <button type="button" className="json-action-btn" onClick={handleEscape}>
            {t("json.escape")}
          </button>
          <button type="button" className="json-action-btn" onClick={handleUnescape}>
            {t("json.unescape")}
          </button>
        </div>

        <div className="json-toolbar-divider" />

        <div className="json-toolbar-group">
          <button
            type="button"
            className="json-action-btn"
            onClick={() => setInput(SAMPLE_JSON)}
          >
            <FileJson size={13} />
            <span>{t("json.sample")}</span>
          </button>
        </div>

        <div className="json-toolbar-spacer" />

        <button
          type="button"
          className={`json-action-btn ${showHistory ? "active" : ""}`}
          onClick={() => setShowHistory(!showHistory)}
        >
          <History size={13} />
          <span>{t("json.history")}</span>
          <span className="json-history-count">{history.length}</span>
        </button>
        <button
          type="button"
          className="json-action-btn"
          onClick={handleCopyInput}
          title={t("json.copyInput")}
        >
          <ClipboardCopy size={14} />
          <span>{t("json.copyInput")}</span>
        </button>
        <button
          type="button"
          className="json-action-btn primary"
          onClick={handleCopy}
          title={t("json.copy")}
        >
          <ClipboardCopy size={14} />
          <span>{t("json.copy")}</span>
        </button>
        <button
          type="button"
          className="json-action-btn"
          onClick={() => {
            setInput("");
            setPath("");
          }}
          title={t("json.clear")}
        >
          <Eraser size={14} />
          <span>{t("json.clear")}</span>
        </button>
      </div>

      {/* ── Status ── */}
      {message && (
        <div className={`json-status ${message.type}`}>
          {message.type === "success" ? (
            <Check size={13} />
          ) : (
            <Code2 size={13} />
          )}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── JSONPath query bar ── */}
      <div className="json-path-bar">
        <Route size={14} className="json-path-icon" />
        <input
          className="json-input"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          placeholder={t("json.pathHint")}
          spellCheck={false}
        />
        {path && (
          <button
            type="button"
            className="json-icon-btn"
            onClick={() => setPath("")}
            title={t("json.clear")}
          >
            <Eraser size={12} />
          </button>
        )}
      </div>

      {/* ── Main area: IO panes + optional history sidebar ── */}
      <div className="json-main-area">
        <div className={`json-io-area ${showHistory ? "with-sidebar" : ""}`}>
          {/* IO panes */}
          <div className="json-io">
            <div className="json-pane">
              <div className="json-pane-header">
                <span className="json-pane-title">{t("json.input")}</span>
                {stats && (
                  <span className="json-stats">
                    {t("json.stats")
                      .replace("{{chars}}", String(stats.chars))
                      .replace("{{nodes}}", String(stats.nodes))}
                  </span>
                )}
              </div>
              <textarea
                className="json-textarea"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                placeholder={t("json.samplePlaceholder")}
                spellCheck={false}
              />
            </div>
            <div className="json-pane">
              <div className="json-pane-header">
                <span className="json-pane-title">
                  {queryResult ? `${t("json.output")} · ${t("json.path")}` : t("json.output")}
                </span>
                {parsed.ok && queryResult && (
                  <span className="json-stats tag tag-tcp">JSONPath</span>
                )}
              </div>
              <pre className={`json-output ${parsed.ok ? "ok" : input.trim() ? "err" : ""}`}>
                <code>{outputText}</code>
              </pre>
            </div>
          </div>

          {/* Compare panel */}
          {compareItem && (
            <div className="json-compare-panel">
              <div className="json-compare-header">
                <span className="json-compare-title">
                  <History size={12} />
                  {t("json.comparing")}: #{compareItem.id} · {formatTime(compareItem.timestamp)}
                </span>
                <button
                  type="button"
                  className="json-icon-btn"
                  onClick={() => setCompareId(null)}
                  title={t("json.clear")}
                >
                  <Eraser size={12} />
                </button>
              </div>
              <div className="json-compare-io">
                <div className="json-pane">
                  <span className="json-pane-title">{t("json.input")}</span>
                  <pre className="json-output ok compare-pre">
                    <code>{prettyForDisplay(compareItem.input)}</code>
                  </pre>
                </div>
                <div className="json-pane">
                  <span className="json-pane-title">{t("json.output")}</span>
                  <pre className="json-output ok compare-pre">
                    <code>{prettyForDisplay(compareOutput)}</code>
                  </pre>
                </div>
              </div>
            </div>
          )}
        </div>

        {/* History sidebar */}
        {showHistory && (
          <div className="json-history">
            <div className="json-history-header">
              <span className="json-history-title">
                <History size={13} />
                {t("json.history")}
              </span>
              {history.length > 0 && (
                <button
                  type="button"
                  className="json-icon-btn"
                  onClick={clearHistory}
                  title={t("json.clearHistory")}
                >
                  <Trash2 size={12} />
                </button>
              )}
            </div>
            {history.length === 0 ? (
              <div className="json-history-empty">{t("json.noHistory")}</div>
            ) : (
              <div className="json-history-list">
                {history.map((item) => (
                  <div
                    key={item.id}
                    className={`json-history-item ${compareId === item.id ? "comparing" : ""}`}
                  >
                    <div className="json-history-item-main" onClick={() => loadFromHistory(item)}>
                      <div className="json-history-meta">
                        <span className="json-history-id">#{item.id}</span>
                        <span className="json-history-time">{formatTime(item.timestamp)}</span>
                        <span className={`json-history-status ${item.ok ? "ok" : "err"}`}>
                          {item.ok ? "✓" : "✗"}
                        </span>
                      </div>
                      <div className="json-history-preview">{truncate(item.input, 40)}</div>
                      <div className="json-history-stats">
                        {item.chars} chars · {item.nodes} nodes
                        {item.path && ` · ${item.path}`}
                      </div>
                    </div>
                    <div className="json-history-item-actions">
                      <button
                        type="button"
                        className="json-history-icon-btn"
                        onClick={() => setCompareId(compareId === item.id ? null : item.id)}
                        title={t("json.compare")}
                      >
                        <Code2 size={11} />
                      </button>
                      <button
                        type="button"
                        className="json-history-icon-btn danger"
                        onClick={() => deleteHistory(item.id)}
                        title={t("json.delete")}
                      >
                        <Trash2 size={11} />
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

export default JsonTool;
