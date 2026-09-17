import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Copy, Eraser, Shuffle, CheckCircle, XCircle } from "lucide-react";
import { useGlobalStore } from "../../core/store";

const UUID_REGEX = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function UuidTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [count, setCount] = useState(5);
  const [hyphen, setHyphen] = useState(true);
  const [uppercase, setUppercase] = useState(false);
  const [items, setItems] = useState<string[]>(() => {
    // Auto-generate on first load
    const out: string[] = [];
    for (let i = 0; i < 5; i++) out.push(crypto.randomUUID() as string);
    return out;
  });
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [validateInput, setValidateInput] = useState("");
  const [validateResult, setValidateResult] = useState<{ valid: boolean; version?: number } | null>(null);

  const generate = () => {
    const n = Math.max(1, Math.min(100, count || 1));
    const out: string[] = [];
    for (let i = 0; i < n; i++) {
      let u = crypto.randomUUID() as string;
      if (!hyphen) u = u.replace(/-/g, "");
      if (uppercase) u = u.toUpperCase();
      out.push(u);
    }
    setItems(out);
    setMessage(null);
  };

  const handleCopyOne = async (text: string, idx: number) => {
    try {
      await copy(text);
      setCopiedIdx(idx);
      setTimeout(() => setCopiedIdx(null), 1200);
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  const handleCopyAll = async () => {
    try {
      await copy(items.join("\n"));
      setMessage({ type: "success", text: t("uuid.copied") });
      setTimeout(() => setMessage(null), 2000);
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  const handleCopyAsArray = async () => {
    try {
      const json = JSON.stringify(items, null, 2);
      await copy(json);
      setMessage({ type: "success", text: t("uuid.copied") });
      setTimeout(() => setMessage(null), 2000);
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  const handleValidate = () => {
    const input = validateInput.trim();
    if (!input) {
      setValidateResult(null);
      return;
    }

    // Check basic format
    if (!UUID_REGEX.test(input)) {
      setValidateResult({ valid: false });
      return;
    }

    // Detect version (char at position 14)
    const versionChar = input.replace(/-/g, "")[12];
    const version = parseInt(versionChar, 16);

    setValidateResult({ valid: true, version: version >= 1 && version <= 8 ? version : undefined });
  };

  return (
    <div className="devtools-tool">
      <div className="devtools-inline-controls">
        <label className="devtools-field">
          <span>{t("uuid.count")}</span>
          <input
            type="number"
            min={1}
            max={100}
            className="devtools-input devtools-input-sm"
            value={count}
            onChange={(e) => setCount(Number(e.target.value))}
          />
        </label>
        <label className="devtools-checkbox">
          <input type="checkbox" checked={hyphen} onChange={(e) => setHyphen(e.target.checked)} />
          <span>{t("uuid.hyphen")}</span>
        </label>
        <label className="devtools-checkbox">
          <input type="checkbox" checked={uppercase} onChange={(e) => setUppercase(e.target.checked)} />
          <span>{t("uuid.uppercase")}</span>
        </label>
        <div className="devtools-actions-spacer" />
        <button type="button" className="btn btn-primary btn-small" onClick={generate}>
          <Shuffle size={14} />
          {t("uuid.generate")}
        </button>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={handleCopyAll}
          disabled={items.length === 0}
        >
          <ClipboardCopy size={14} />
          {t("uuid.copyAll")}
        </button>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={handleCopyAsArray}
          disabled={items.length === 0}
        >
          <Copy size={14} />
          {t("uuid.copyAsJson")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={() => setItems([])}>
          <Eraser size={14} />
          {t("uuid.clear")}
        </button>
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Shuffle size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* UUID List */}
      {items.length > 0 && (
        <div className="devtools-uuid-list">
          {items.map((u, i) => (
            <div key={i} className="devtools-uuid-item">
              <code>{u}</code>
              <button
                type="button"
                className="btn btn-secondary btn-small icon-only"
                onClick={() => handleCopyOne(u, i)}
                title={t("uuid.copy")}
              >
                {copiedIdx === i ? <Check size={12} /> : <Copy size={12} />}
              </button>
            </div>
          ))}
        </div>
      )}

      {items.length > 0 && (
        <div className="uuid-stats">
          {t("uuid.count")}: {items.length} · {items[0].length} chars
        </div>
      )}

      {/* UUID Validator */}
      <div className="uuid-validator">
        <label className="devtools-label">{t("uuid.validate")}</label>
        <div className="uuid-validate-input">
          <input
            className="devtools-input"
            value={validateInput}
            onChange={(e) => {
              setValidateInput(e.target.value);
              setValidateResult(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleValidate();
            }}
            placeholder={t("uuid.validatePlaceholder")}
            spellCheck={false}
          />
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={handleValidate}
          >
            {t("uuid.validate")}
          </button>
        </div>
        {validateResult && (
          <div className={`uuid-validate-result ${validateResult.valid ? "valid" : "invalid"}`}>
            {validateResult.valid ? (
              <>
                <CheckCircle size={14} />
                <span>{t("uuid.validUuid")}</span>
                {validateResult.version && (
                  <span className="uuid-version">v{validateResult.version}</span>
                )}
              </>
            ) : (
              <>
                <XCircle size={14} />
                <span>{t("uuid.invalidUuid")}</span>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

export default UuidTool;
