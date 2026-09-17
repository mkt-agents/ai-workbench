import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Lock, AlertCircle } from "lucide-react";
import { useGlobalStore } from "../../core/store";

const SAMPLE_JWT =
  "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkFJIFdvcmtiZW5jaCIsImlhdCI6MTcxNjIzOTAyMiwiZXhwIjoxOTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";

function b64UrlDecode(s: string): string {
  const pad = "=".repeat((4 - (s.length % 4)) % 4);
  const norm = (s + pad).replace(/-/g, "+").replace(/_/g, "/");
  return atob(norm);
}

/** Format seconds into human-readable duration. */
function formatDuration(seconds: number): string {
  const abs = Math.abs(seconds);
  if (abs < 60) return `${abs}s`;
  if (abs < 3600) return `${Math.floor(abs / 60)}m ${abs % 60}s`;
  if (abs < 86400) return `${Math.floor(abs / 3600)}h ${Math.floor((abs % 3600) / 60)}m`;
  return `${Math.floor(abs / 86400)}d ${Math.floor((abs % 86400) / 3600)}h`;
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
      formatted: formatDuration(diff),
    };
  }, [parsed]);

  // Check for nbf (not before)
  const nbfInfo = useMemo(() => {
    if (!parsed || "error" in parsed) return null;
    const nbf = (parsed as any).payloadObj?.nbf;
    if (!nbf) return null;
    const now = Math.floor(Date.now() / 1000);
    const diff = now - nbf;
    return {
      valid: diff >= 0,
      nbf,
      formatted: formatDuration(diff),
    };
  }, [parsed]);

  // Get algorithm info
  const algoInfo = useMemo(() => {
    if (!parsed || "error" in parsed) return null;
    return (parsed as any).headerObj?.alg || null;
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
            placeholder={t("jwt.placeholder")}
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
              {/* Status badges */}
              <div className="jwt-badges">
                {algoInfo && (
                  <span className="jwt-badge jwt-badge-algo">
                    {algoInfo}
                  </span>
                )}
                {expInfo && (
                  <span className={`jwt-badge ${expInfo.expired ? "jwt-badge-expired" : "jwt-badge-valid"}`}>
                    {expInfo.expired
                      ? t("jwt.expiredBadge", { time: expInfo.formatted })
                      : t("jwt.expiresInBadge", { time: expInfo.formatted })}
                  </span>
                )}
                {nbfInfo && (
                  <span className={`jwt-badge ${nbfInfo.valid ? "jwt-badge-valid" : "jwt-badge-expired"}`}>
                    {nbfInfo.valid ? t("jwt.active") : t("jwt.notYetActive")}
                  </span>
                )}
              </div>

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
                  <AlertCircle size={14} />
                  <span>
                    {expInfo.expired
                      ? t("jwt.expiredAt", { time: expInfo.formatted })
                      : t("jwt.expiresAt", { time: expInfo.formatted })}
                  </span>
                </div>
              )}
            </div>
          )}

          {!parsed && (
            <div className="runtime-empty">{t("jwt.placeholder")}</div>
          )}
        </div>
      </div>
    </div>
  );
}

export default JwtTool;
