import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ClipboardCopy,
  Code2,
  Eraser,
  FileJson,
  Route,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";

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

function tryRepairJson(input: string): string | null {
  let s = input.replace(/(^|[^\\])'/g, '$1"');
  s = s.replace(/,(\s*[}\]])/g, "$1");
  s = s.replace(/([{,]\s*)([A-Za-z_][\w-]*)\s*:/g, '$1"$2":');
  try {
    JSON.parse(s);
    return s;
  } catch {
    return null;
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

  const parsed = useMemo(() => {
    if (!input.trim())
      return { ok: false as const, result: "", value: null as unknown, error: "" };
    try {
      const obj = JSON.parse(input);
      return { ok: true as const, result: JSON.stringify(obj, null, 2), value: obj, error: "" };
    } catch (e) {
      return { ok: false as const, result: "", value: null, error: String(e) };
    }
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

  const flash = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 2500);
  };

  const handleFormat = () => {
    if (parsed.ok) {
      setInput(parsed.result);
      flash("success", t("json.valid"));
    } else {
      flash("error", `${t("json.invalid")}: ${parsed.error}`);
    }
  };

  const handleCompress = () => {
    if (parsed.ok) {
      setInput(JSON.stringify(parsed.value));
      flash("success", t("json.valid"));
    } else {
      flash("error", `${t("json.invalid")}: ${parsed.error}`);
    }
  };

  const handleRepair = () => {
    const repaired = tryRepairJson(input);
    if (repaired) {
      setInput(JSON.stringify(JSON.parse(repaired), null, 2));
      flash("success", t("json.valid"));
    } else {
      flash("error", t("json.invalid"));
    }
  };

  const handleEscape = () => {
    if (!input.trim()) return;
    setInput(JSON.stringify(input).slice(1, -1));
    flash("success", t("json.valid"));
  };

  const handleUnescape = () => {
    const trimmed = input.trim();
    if (!trimmed) return;
    const wrapped = trimmed.startsWith('"') ? trimmed : `"${trimmed}"`;
    try {
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

  return (
    <div className="devtools-tool">
      {/* Toolbar */}
      <div className="devtools-actions">
        <button type="button" className="btn btn-secondary btn-small" onClick={handleFormat}>
          {t("json.format")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={handleCompress}>
          {t("json.compress")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={handleRepair}>
          {t("json.repair")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={handleEscape}>
          {t("json.escape")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={handleUnescape}>
          {t("json.unescape")}
        </button>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={() => setInput(SAMPLE_JSON)}
        >
          <FileJson size={14} />
          {t("json.sample")}
        </button>
        <div className="devtools-actions-spacer" />
        <button type="button" className="btn btn-secondary btn-small" onClick={handleCopyInput}>
          <ClipboardCopy size={14} />
          {t("json.copyInput")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={handleCopy}>
          <ClipboardCopy size={14} />
          {t("json.copy")}
        </button>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={() => {
            setInput("");
            setPath("");
          }}
        >
          <Eraser size={14} />
          {t("json.clear")}
        </button>
      </div>

      {/* Message */}
      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? (
            <Check size={14} />
          ) : (
            <Code2 size={14} />
          )}
          <span>{message.text}</span>
        </div>
      )}

      {/* JSONPath query bar */}
      <div className="json-path-bar">
        <Route size={14} className="json-path-icon" />
        <input
          className="devtools-input"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          placeholder={t("json.pathHint")}
          spellCheck={false}
        />
        {path && (
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => setPath("")}
          >
            <Eraser size={12} />
          </button>
        )}
      </div>

      {/* IO panes */}
      <div className="devtools-io">
        <div className="devtools-io-pane">
          <div className="io-header">
            <label className="devtools-label">{t("json.input")}</label>
            {stats && (
              <span className="json-stats">
                {t("json.stats")
                  .replace("{{chars}}", String(stats.chars))
                  .replace("{{nodes}}", String(stats.nodes))}
              </span>
            )}
          </div>
          <textarea
            className="devtools-textarea"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={t("json.samplePlaceholder")}
            spellCheck={false}
          />
        </div>
        <div className="devtools-io-pane">
          <div className="io-header">
            <label className="devtools-label">
              {queryResult ? `${t("json.output")} · ${t("json.path")}` : t("json.output")}
            </label>
            {parsed.ok && queryResult && (
              <span className="json-stats tag tag-tcp">JSONPath</span>
            )}
          </div>
          <pre className={`devtools-pre json-output ${parsed.ok ? "ok" : input.trim() ? "err" : ""}`}>
            <code>{outputText}</code>
          </pre>
        </div>
      </div>
    </div>
  );
}

export default JsonTool;
