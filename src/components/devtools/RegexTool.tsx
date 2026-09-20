import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Regex, Replace } from "lucide-react";
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
  const [replaceStr, setReplaceStr] = useState("");
  const [showReplace, setShowReplace] = useState(false);
  const [flags, setFlags] = useState({ g: true, i: false, m: false, s: false, u: false, y: false });

  const flagLabels: Record<keyof typeof flags, string> = {
    g: "global",
    i: "ignoreCase",
    m: "multiline",
    s: "dotAll",
    u: "unicode",
    y: "sticky",
  };
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

  // Compute replacement result
  const replaceResult = useMemo(() => {
    if (!result.ok || !showReplace) return null;
    try {
      const flagStr =
        (flags.g ? "g" : "") +
        (flags.i ? "i" : "") +
        (flags.m ? "m" : "") +
        (flags.s ? "s" : "") +
        (flags.u ? "u" : "") +
        (flags.y ? "y" : "");
      const re = new RegExp(pattern, flagStr);
      const replaced = testStr.replace(re, replaceStr);
      return replaced;
    } catch {
      return null;
    }
  }, [result.ok, showReplace, pattern, testStr, flags, replaceStr]);

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

  // Common regex patterns
  const commonPatterns = [
    { label: "Email", pattern: "[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}" },
    { label: "URL", pattern: "https?://[\\w.-]+(?:/[\\w.-]*)*" },
    { label: "IPv4", pattern: "\\b(?:\\d{1,3}\\.){3}\\d{1,3}\\b" },
    { label: "Phone (CN)", pattern: "1[3-9]\\d{9}" },
    { label: "Date", pattern: "\\d{4}-\\d{2}-\\d{2}" },
  ];

  return (
    <div className="devtools-tool regex-tool">
      {/* ── Toolbar ── */}
      <div className="regex-toolbar">
        <div className="regex-toolbar-group">
          <span className="regex-toolbar-label">{t("regex.pattern")}</span>
          <input
            className="regex-input"
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            placeholder="(\\d+)"
            spellCheck={false}
          />
        </div>

        <div className="regex-toolbar-divider" />

        <div className="regex-toolbar-group">
          <span className="regex-toolbar-label">{t("regex.flags")}</span>
          <div className="regex-flags">
            {(Object.keys(flags) as (keyof typeof flags)[]).map((key) => (
              <label
                key={key}
                className={`regex-flag ${flags[key] ? "active" : ""}`}
                title={t(`regex.${flagLabels[key]}`)}
              >
                <input type="checkbox" checked={flags[key]} onChange={() => toggleFlag(key)} />
                <span>{key}</span>
              </label>
            ))}
          </div>
        </div>

        <div className="regex-toolbar-spacer" />

        <button
          type="button"
          className={`regex-action-btn ${showReplace ? "active" : ""}`}
          onClick={() => setShowReplace(!showReplace)}
          title={t("regex.replace")}
        >
          <Replace size={14} />
          <span>{t("regex.replace")}</span>
        </button>
        <button
          type="button"
          className="regex-action-btn primary"
          onClick={handleCopy}
          disabled={result.matches.length === 0}
          title={t("regex.copy")}
        >
          <ClipboardCopy size={14} />
          <span>{t("regex.copy")}</span>
        </button>
        <button
          type="button"
          className="regex-action-btn"
          onClick={() => {
            setPattern("");
            setTestStr("");
          }}
          title={t("regex.clear")}
        >
          <Eraser size={14} />
          <span>{t("regex.clear")}</span>
        </button>
      </div>

      {/* ── Presets ── */}
      <div className="regex-presets">
        {commonPatterns.map((preset) => (
          <button
            key={preset.label}
            type="button"
            className="regex-preset-btn"
            onClick={() => setPattern(preset.pattern)}
          >
            {preset.label}
          </button>
        ))}
      </div>

      {/* ── Status ── */}
      {message && (
        <div className={`regex-status ${message.type}`}>
          {message.type === "success" ? <Check size={13} /> : <Regex size={13} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── IO ── */}
      <div className="regex-main">
        <div className="regex-pane">
          {showReplace && (
            <>
              <label className="regex-pane-title">{t("regex.replaceWith")}</label>
              <input
                className="regex-input"
                value={replaceStr}
                onChange={(e) => setReplaceStr(e.target.value)}
                placeholder={t("regex.replacePlaceholder")}
                spellCheck={false}
              />
            </>
          )}
          <div className="regex-pane-header">
            <span className="regex-pane-title">{t("regex.test")}</span>
          </div>
          <textarea
            className="regex-textarea"
            value={testStr}
            onChange={(e) => setTestStr(e.target.value)}
            placeholder=""
            rows={5}
            spellCheck={false}
          />
        </div>

        <div className="regex-pane regex-result-pane">
          <div className="regex-pane-header">
            <span className="regex-pane-title">
              {t("regex.matches")} ({result.matches.length})
            </span>
          </div>

          {!result.ok && result.error && (
            <div className="regex-status error">
              <Regex size={13} />
              <span>
                {t("regex.invalid")}: {result.error}
              </span>
            </div>
          )}

          <div className="regex-result-scroll">
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
              <div className="regex-empty">{t("regex.noMatch")}</div>
            ) : (
              <div className="regex-matches">
                {result.matches.map((m, i) => (
                  <div key={i} className="regex-match">
                    <span className="regex-match-index">@{m.index}</span>
                    <span className="regex-match-value">{m.match}</span>
                    {m.groups.length > 0 && (
                      <span className="regex-groups">
                        {m.groups.map((g, j) => (
                          <span key={j} className="regex-group">
                            {t("regex.group")} {j + 1}: {g || "undefined"}
                          </span>
                        ))}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            )}

            {replaceResult && (
              <div className="regex-replace-result">
                <label className="regex-pane-title">{t("regex.replaceResult")}</label>
                <div className="regex-replace-preview">
                  <pre className="devtools-pre">{replaceResult}</pre>
                  <button
                    type="button"
                    className="regex-icon-btn"
                    onClick={() => {
                      copy(replaceResult);
                      flash("success", t("regex.copied"));
                    }}
                    title={t("regex.copy")}
                  >
                    <ClipboardCopy size={12} />
                  </button>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export default RegexTool;
