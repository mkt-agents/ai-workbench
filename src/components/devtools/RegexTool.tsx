import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Regex } from "lucide-react";
import { useGlobalStore } from "../../core/store";

interface RegexMatch {
  match: string;
  index: number;
  groups: string[];
}

function RegexTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [pattern, setPattern] = useState("(\\d{4})-(\\d{2})-(\\d{2})");
  const [testStr, setTestStr] = useState("Date: 2026-09-17, 2025-12-31");
  const [flags, setFlags] = useState({ g: true, i: false, m: false, s: false, u: false, y: false });
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  const result = useMemo(() => {
    if (!pattern) return { ok: false as const, error: "", matches: [] as RegexMatch[] };
    const flagStr =
      (flags.g ? "g" : "") +
      (flags.i ? "i" : "") +
      (flags.m ? "m" : "") +
      (flags.s ? "s" : "") +
      (flags.u ? "u" : "") +
      (flags.y ? "y" : "");
    let re: RegExp;
    try {
      re = new RegExp(pattern, flagStr);
    } catch (e) {
      return { ok: false as const, error: String(e), matches: [] as RegexMatch[] };
    }
    const matches: RegexMatch[] = [];
    let m: RegExpExecArray | null;
    let guard = 0;
    if (flags.g) {
      while ((m = re.exec(testStr)) !== null) {
        matches.push({ match: m[0], index: m.index, groups: m.slice(1) });
        if (m[0] === "") re.lastIndex++;
        if (++guard > 1000) break;
      }
    } else {
      m = re.exec(testStr);
      if (m) matches.push({ match: m[0], index: m.index, groups: m.slice(1) });
    }
    return { ok: true as const, error: "", matches };
  }, [pattern, testStr, flags]);

  const toggleFlag = (key: keyof typeof flags) => {
    if (key === "g") {
      // global is the default, allow toggling
      setFlags({ ...flags, g: !flags.g });
    } else {
      setFlags({ ...flags, [key]: !flags[key] });
    }
  };

  const handleCopy = async () => {
    try {
      const text = result.matches.map((m) => `${m.index}: ${m.match}`).join("\n");
      await copy(text);
      setMessage({ type: "success", text: t("regex.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  return (
    <div className="devtools-tool">
      <div className="devtools-row">
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={handleCopy}
          disabled={result.matches.length === 0}
        >
          <ClipboardCopy size={14} />
          {t("regex.copy")}
        </button>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={() => {
            setPattern("");
            setTestStr("");
          }}
        >
          <Eraser size={14} />
          {t("regex.clear")}
        </button>
      </div>

      <label className="devtools-label">{t("regex.pattern")}</label>
      <input
        className="devtools-input"
        value={pattern}
        onChange={(e) => setPattern(e.target.value)}
        placeholder="(\\d+)"
        spellCheck={false}
      />

      <div className="devtools-flags">
        {(Object.keys(flags) as (keyof typeof flags)[])
          .filter((k) => k !== "g" || true)
          .map((key) => (
            <label key={key} className="devtools-checkbox">
              <input type="checkbox" checked={flags[key]} onChange={() => toggleFlag(key)} />
              <span>{t(`regex.${key}`)}</span>
            </label>
          ))}
      </div>

      <label className="devtools-label">{t("regex.test")}</label>
      <textarea
        className="devtools-textarea"
        value={testStr}
        onChange={(e) => setTestStr(e.target.value)}
        placeholder=""
        rows={4}
        spellCheck={false}
      />

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Regex size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      {!result.ok && result.error && (
        <div className="runtime-msg error">
          <Regex size={14} />
          <span>{t("regex.invalid") + ": " + result.error}</span>
        </div>
      )}

      {result.ok && (
        <div className="devtools-regex-out">
          <label className="devtools-label">
            {t("regex.matches")} ({result.matches.length})
          </label>
          {result.matches.length === 0 ? (
            <div className="runtime-hint">{t("regex.noMatch")}</div>
          ) : (
            <div className="devtools-match-list">
              {result.matches.map((m, i) => (
                <div key={i} className="devtools-match-item">
                  <span className="devtools-match-index">@{m.index}</span>
                  <span className="devtools-match-value">{m.match}</span>
                  {m.groups.length > 0 && (
                    <span className="devtools-match-groups">
                      {m.groups.map((g, j) => (
                        <span key={j} className="devtools-match-group">
                          {t("regex.group")} {j + 1}: {g || "undefined"}
                        </span>
                      ))}
                    </span>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default RegexTool;
