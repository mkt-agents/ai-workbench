import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Braces, Check, ClipboardCopy, Eraser } from "lucide-react";
import { useGlobalStore } from "../../core/store";

function tryRepairJson(input: string): string | null {
  // Replace single quotes with double quotes (common mistake)
  let s = input.replace(/'/g, '"');
  // Remove trailing commas
  s = s.replace(/,(\s*[}\]])/g, "$1");
  // Quote unquoted keys
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
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  const parsed = useMemo(() => {
    if (!input.trim()) return { ok: false as const, result: "", error: "" };
    try {
      const obj = JSON.parse(input);
      return { ok: true as const, result: JSON.stringify(obj, null, 2), error: "" };
    } catch (e) {
      return { ok: false as const, result: "", error: String(e) };
    }
  }, [input]);

  const handleFormat = () => {
    if (parsed.ok) {
      setInput(parsed.result);
      setMessage({ type: "success", text: t("json.valid") });
    } else {
      setMessage({ type: "error", text: t("json.invalid") + `: ${parsed.error}` });
    }
  };

  const handleCompress = () => {
    if (parsed.ok) {
      setInput(JSON.stringify(JSON.parse(input)));
      setMessage({ type: "success", text: t("json.valid") });
    } else {
      setMessage({ type: "error", text: t("json.invalid") + `: ${parsed.error}` });
    }
  };

  const handleRepair = () => {
    const repaired = tryRepairJson(input);
    if (repaired) {
      setInput(JSON.stringify(JSON.parse(repaired), null, 2));
      setMessage({ type: "success", text: t("json.valid") });
    } else {
      setMessage({ type: "error", text: t("json.invalid") });
    }
  };

  const handleValidate = () => {
    if (parsed.ok) {
      setMessage({ type: "success", text: t("json.valid") });
    } else {
      setMessage({ type: "error", text: t("json.invalid") + `: ${parsed.error}` });
    }
  };

  const handleCopy = async () => {
    try {
      await copy(input);
      setMessage({ type: "success", text: t("json.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  return (
    <div className="devtools-tool">
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
        <button type="button" className="btn btn-secondary btn-small" onClick={handleValidate}>
          {t("json.validate")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={handleCopy}>
          <ClipboardCopy size={14} />
          {t("json.copy")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={() => setInput("")}>
          <Eraser size={14} />
          {t("json.clear")}
        </button>
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Braces size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      <div className="devtools-io">
        <div className="devtools-io-pane">
          <label className="devtools-label">{t("json.input")}</label>
          <textarea
            className="devtools-textarea"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={t("json.sample")}
            spellCheck={false}
          />
        </div>
        <div className="devtools-io-pane">
          <label className="devtools-label">{t("json.output")}</label>
          <textarea
            className="devtools-textarea"
            value={parsed.ok ? parsed.result : parsed.error}
            readOnly
            placeholder={t("json.sample")}
            spellCheck={false}
          />
        </div>
      </div>
    </div>
  );
}

export default JsonTool;
