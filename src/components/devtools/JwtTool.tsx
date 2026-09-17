import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Lock } from "lucide-react";
import { useGlobalStore } from "../../core/store";

function b64UrlDecode(s: string): string {
  const pad = "=".repeat((4 - (s.length % 4)) % 4);
  const norm = (s + pad).replace(/-/g, "+").replace(/_/g, "/");
  return atob(norm);
}

function JwtTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [token, setToken] = useState("");
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  const parsed = useMemo(() => {
    if (!token.trim()) return null;
    const parts = token.trim().split(".");
    if (parts.length !== 3) return { error: t("jwt.invalid") };
    try {
      const header = JSON.parse(b64UrlDecode(parts[0]));
      const payload = JSON.parse(b64UrlDecode(parts[1]));
      const headerStr = JSON.stringify(header, null, 2);
      const payloadStr = JSON.stringify(payload, null, 2);
      return { header: headerStr, payload: payloadStr, signature: parts[2] };
    } catch (e) {
      return { error: t("jwt.invalid") };
    }
  }, [token, t]);

  const handleCopy = async (text: string) => {
    try {
      await copy(text);
      setMessage({ type: "success", text: t("jwt.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  return (
    <div className="devtools-tool">
      <div className="devtools-row">
        <button type="button" className="btn btn-secondary btn-small" onClick={() => setToken("")}>
          <Eraser size={14} />
          {t("jwt.clear")}
        </button>
      </div>

      <label className="devtools-label">{t("jwt.token")}</label>
      <textarea
        className="devtools-textarea"
        value={token}
        onChange={(e) => setToken(e.target.value)}
        placeholder={t("jwt.sample")}
        rows={4}
        spellCheck={false}
      />

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Lock size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      {parsed && "error" in parsed && (
        <div className="runtime-msg error">
          <Lock size={14} />
          <span>{parsed.error}</span>
        </div>
      )}

      {parsed && !("error" in parsed) && (
        <div className="devtools-jwt-panels">
          <div className="devtools-io-pane">
            <label className="devtools-label">{t("jwt.header")}</label>
            <pre className="devtools-pre">{parsed.header}</pre>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => handleCopy(parsed.header)}
            >
              <ClipboardCopy size={14} />
              {t("jwt.copy")}
            </button>
          </div>
          <div className="devtools-io-pane">
            <label className="devtools-label">{t("jwt.payload")}</label>
            <pre className="devtools-pre">{parsed.payload}</pre>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => handleCopy(parsed.payload)}
            >
              <ClipboardCopy size={14} />
              {t("jwt.copy")}
            </button>
          </div>
          <div className="devtools-io-pane">
            <label className="devtools-label">{t("jwt.signature")}</label>
            <pre className="devtools-pre devtools-pre-raw">{parsed.signature}</pre>
          </div>
        </div>
      )}
    </div>
  );
}

export default JwtTool;
