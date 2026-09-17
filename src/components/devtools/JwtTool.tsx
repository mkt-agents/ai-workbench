import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Lock } from "lucide-react";
import { useGlobalStore } from "../../core/store";

const SAMPLE_JWT =
  "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkFJIFdvcmtiZW5jaCIsImlhdCI6MTcxNjIzOTAyMiwiZXhwIjoxOTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";

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
      return { header: headerStr, payload: payloadStr, signature: parts[2], headerObj: header, payloadObj: payload };
    } catch (e) {
      return { error: t("jwt.invalid") };
    }
  }, [token, t]);

  const flash = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 2500);
  };

  const handleCopy = async (text: string) => {
    try {
      await copy(text);
      flash("success", t("jwt.copied"));
    } catch (e) {
      flash("error", String(e));
    }
  };

  // Check for expiration
  const expInfo = useMemo(() => {
    if (!parsed || "error" in parsed) return null;
    const exp = (parsed as any).payloadObj?.exp;
    if (!exp) return null;
    const now = Math.floor(Date.now() / 1000);
    const diff = exp - now;
    return {
      exp,
      expired: diff < 0,
      in: diff,
    };
  }, [parsed]);

  return (
    <div className="devtools-tool">
      <div className="devtools-actions">
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={() => setToken(SAMPLE_JWT)}
        >
          <Lock size={14} />
          {t("jwt.sample")}
        </button>
        <div className="devtools-actions-spacer" />
        <button type="button" className="btn btn-secondary btn-small" onClick={() => setToken("")}>
          <Eraser size={14} />
          {t("jwt.clear")}
        </button>
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Lock size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      <div className="devtools-io">
        <div className="devtools-io-pane">
          <label className="devtools-label">{t("jwt.token")}</label>
          <textarea
            className="devtools-textarea"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={t("token")}
            rows={5}
            spellCheck={false}
          />
        </div>

        <div className="devtools-io-pane jwt-parse-area">
          {parsed && "error" in parsed && (
            <div className="runtime-msg error">
              <Lock size={14} />
              <span>{parsed.error}</span>
            </div>
          )}

          {parsed && !("error" in parsed) && (
            <div className="jwt-result">
              <div className="jwt-section">
                <div className="jwt-section-header">
                  <span className="jwt-section-tag tag-tcp">{t("jwt.header")}</span>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small icon-only"
                    onClick={() => handleCopy(parsed.header)}
                  >
                    <ClipboardCopy size={11} />
                  </button>
                </div>
                <pre className="devtools-pre">{parsed.header}</pre>
              </div>

              <div className="jwt-section">
                <div className="jwt-section-header">
                  <span className="jwt-section-tag tag-service">{t("jwt.payload")}</span>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small icon-only"
                    onClick={() => handleCopy(parsed.payload)}
                  >
                    <ClipboardCopy size={11} />
                  </button>
                </div>
                <pre className="devtools-pre">{parsed.payload}</pre>
              </div>

              <div className="jwt-section">
                <div className="jwt-section-header">
                  <span className="jwt-section-tag">{t("jwt.signature")}</span>
                </div>
                <pre className="devtools-pre devtools-pre-raw">{parsed.signature}</pre>
              </div>

              {expInfo && (
                <div className={`jwt-expiry ${expInfo.expired ? "expired" : "valid"}`}>
                  {expInfo.expired
                    ? `⏰ Expired ${Math.abs(expInfo.in)}s ago`
                    : `⏰ Expires in ${expInfo.in}s`}
                </div>
              )}
            </div>
          )}

          {!parsed && (
            <div className="runtime-empty">{t("jwt.sample")}</div>
          )}
        </div>
      </div>
    </div>
  );
}

export default JwtTool;
