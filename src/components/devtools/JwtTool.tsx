import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Lock, AlertCircle, FileText } from "lucide-react";
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
    <div className="devtools-tool jwt-tool">
      {/* ── Toolbar ── */}
      <div className="jwt-toolbar">
        <button
          type="button"
          className="jwt-action-btn"
          onClick={() => setToken(SAMPLE_JWT)}
          title={t("jwt.sample")}
        >
          <FileText size={14} />
          <span>{t("jwt.sample")}</span>
        </button>
        <div className="jwt-toolbar-spacer" />
        <button
          type="button"
          className="jwt-action-btn"
          onClick={() => setToken("")}
          disabled={!token}
          title={t("jwt.clear")}
        >
          <Eraser size={14} />
          <span>{t("jwt.clear")}</span>
        </button>
      </div>

      {/* ── Status ── */}
      {message && (
        <div className={`jwt-status ${message.type}`}>
          {message.type === "success" ? <Check size={13} /> : <Lock size={13} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── IO ── */}
      <div className="jwt-io">
        <div className="jwt-input-area">
          <div className="jwt-pane-header">
            <span className="jwt-pane-title">{t("jwt.token")}</span>
            {token && (
              <span className="jwt-stats">
                {token.length} chars
              </span>
            )}
          </div>
          <textarea
            className="jwt-textarea"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={t("jwt.placeholder")}
            rows={5}
            spellCheck={false}
          />
        </div>

        <div className="jwt-result">
          {parsed && "error" in parsed && (
            <div className="jwt-status error">
              <Lock size={13} />
              <span>{parsed.error}</span>
            </div>
          )}

          {parsed && !("error" in parsed) && (
            <div className="jwt-parsed">
              <div className="jwt-pane-header">
                <span className="jwt-pane-title">{t("jwt.result")}</span>
              </div>
              <div className="jwt-section">
                <div className="jwt-section-header">
                  <span className="jwt-section-tag tag-header">{t("jwt.header")}</span>
                  <button
                    type="button"
                    className="jwt-section-copy"
                    onClick={() => handleCopy(parsed.header)}
                    title={t("jwt.copy")}
                  >
                    <ClipboardCopy size={11} />
                  </button>
                </div>
                <pre className="jwt-pre">{parsed.header}</pre>
              </div>

              <div className="jwt-section">
                <div className="jwt-section-header">
                  <span className="jwt-section-tag tag-payload">{t("jwt.payload")}</span>
                  <button
                    type="button"
                    className="jwt-section-copy"
                    onClick={() => handleCopy(parsed.payload)}
                    title={t("jwt.copy")}
                  >
                    <ClipboardCopy size={11} />
                  </button>
                </div>
                <pre className="jwt-pre">{parsed.payload}</pre>
              </div>

              <div className="jwt-section">
                <div className="jwt-section-header">
                  <span className="jwt-section-tag tag-signature">{t("jwt.signature")}</span>
                </div>
                <pre className="jwt-pre jwt-pre-raw">{parsed.signature}</pre>
              </div>

              {/* Status badges — moved below sections */}
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
        </div>
      </div>
    </div>
  );
}

export default JwtTool;
