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
    setFlags({ ...flags, [key]: !flags[key] });
  };

  const flash = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 2500);
  };

  const handleCopy = async () => {
    try {
      const text = result.matches.map((m) => `${m.index}: ${m.match}`).join("\n");
      await copy(text);
      flash("success", t("regex.copied"));
    } catch (e) {
      flash("error", String(e));
    }
  };

  // Build highlighted segments for the test string
  const segments = useMemo(() => {
    if (!result.ok || result.matches.length === 0 || !testStr) return null;
    const parts: { text: string; highlight: boolean }[] = [];
    let lastIdx = 0;
    for (const m of result.matches) {
      if (m.index > lastIdx) {
        parts.push({ text: testStr.slice(lastIdx, m.index), highlight: false });
      }
      parts.push({ text: m.match, highlight: true });
      lastIdx = m.index + m.match.length;
    }
    if (lastIdx < testStr.length) {
      parts.push({ text: testStr.slice(lastIdx), highlight: false });
    }
    return parts;
  }, [result, testStr]);

  return (
    <div className="devtools-tool">
      <div className="devtools-actions">
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

      <div className="devtools-io">
        <div className="devtools-io-pane">
          <label className="devtools-label">{t("regex.pattern")}</label>
          <input
            className="devtools-input"
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            placeholder="(\\d+)"
            spellCheck={false}
          />

          <div className="devtools-flags">
            {(Object.keys(flags) as (keyof typeof flags)[]).map((key) => (
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
            rows={5}
            spellCheck={false}
          />
        </div>

        <div className="devtools-io-pane">
          <div className="io-header">
            <label className="devtools-label">
              {t("regex.matches")} ({result.matches.length})
            </label>
          </div>

          {message && (
            <div className={`runtime-msg ${message.type}`}>
              {message.type === "success" ? <Check size={14} /> : <Regex size={14} />}
              <span>{message.text}</span>
            </div>
          )}

          {!result.ok && result.error && (
            <div className="runtime-msg error">
              <Regex size={14} />
              <span>
                {t("regex.invalid")}: {result.error}
              </span>
            </div>
          )}

          {result.ok && segments && (
            <div className="regex-highlight-box">
              <div className="regex-highlight-text">
                {segments.map((seg, i) =>
                  seg.highlight ? (
                    <mark key={i} className="regex-match-mark">
                      {seg.text}
                    </mark>
                  ) : (
                    <span key={i}>{seg.text}</span>
                  ),
                )}
              </div>
            </div>
          )}

          {result.ok && result.matches.length === 0 ? (
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
      </div>
    </div>
  );
}

export default RegexTool;
