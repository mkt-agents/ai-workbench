import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ClipboardCopy,
  Loader2,
  Plus,
  Send,
  Trash2,
  Wrench,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";

type Tab = "headers" | "body" | "response";

interface HeaderRow {
  id: number;
  key: string;
  value: string;
}

let headerId = 1;
function newHeader(key = "", value = ""): HeaderRow {
  return { id: headerId++, key, value };
}

const METHODS = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"] as const;

function formatJson(text: string): { ok: boolean; result: string } {
  try {
    const parsed = JSON.parse(text);
    return { ok: true, result: JSON.stringify(parsed, null, 2) };
  } catch {
    return { ok: false, result: text };
  }
}

function detectContentType(headers: { key: string; value: string }[]): string {
  const ct = headers.find((h) => h.key.toLowerCase() === "content-type");
  return ct?.value || "";
}

function HttpClientTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);
  const httpRequest = useGlobalStore((s) => s.invokeHttpRequest);

  const [method, setMethod] = useState<(typeof METHODS)[number]>("GET");
  const [url, setUrl] = useState("");
  const [headers, setHeaders] = useState<HeaderRow[]>([newHeader("Accept", "*/*")]);
  const [body, setBody] = useState("");
  const [tab, setTab] = useState<Tab>("headers");
  const [sending, setSending] = useState(false);
  const [response, setResponse] = useState<{
    status: number;
    duration_ms: number;
    headers: { key: string; value: string }[];
    body: string;
    contentType: string;
  } | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  const headerCount = headers.filter((h) => h.key.trim()).length;

  const updateHeader = (id: number, field: "key" | "value", val: string) => {
    setHeaders((hs) => hs.map((h) => (h.id === id ? { ...h, [field]: val } : h)));
  };

  const addHeader = () => {
    setHeaders((hs) => [...hs, newHeader()]);
  };

  const removeHeader = (id: number) => {
    setHeaders((hs) => (hs.length <= 1 ? hs : hs.filter((h) => h.id !== id)));
  };

  const flash = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 3000);
  };

  const handleSend = async () => {
    const trimmed = url.trim();
    if (!trimmed) {
      flash("error", t("http.invalidUrl"));
      return;
    }
    if (!/^https?:\/\//i.test(trimmed)) {
      flash("error", t("http.invalidUrl"));
      return;
    }
    setSending(true);
    setMessage(null);
    try {
      const hdrs = headers
        .filter((h) => h.key.trim())
        .map((h) => ({ key: h.key, value: h.value }));
      const resp = await httpRequest({
        method,
        url: trimmed,
        headers: hdrs,
        body: method !== "GET" && method !== "HEAD" ? body || undefined : undefined,
        timeout_sec: 30,
      });
      const contentType = detectContentType(resp.headers);
      setResponse({
        status: resp.status,
        duration_ms: resp.duration_ms,
        headers: resp.headers,
        body: resp.body,
        contentType,
      });
      setTab("response");
    } catch (e) {
      flash("error", String(e));
    } finally {
      setSending(false);
    }
  };

  const handleCopyBody = async () => {
    if (!response) return;
    try {
      await copy(response.body);
      flash("success", t("http.copied"));
    } catch (e) {
      flash("error", String(e));
    }
  };

  const handleFormatBody = () => {
    if (!response) return;
    const { ok, result } = formatJson(response.body);
    if (ok) {
      setResponse({ ...response, body: result });
      flash("success", "JSON formatted");
    }
  };

  const statusClass =
    response && response.status >= 200 && response.status < 300
      ? "status-ok"
      : response && response.status >= 400
        ? "status-err"
        : "";

  const isJsonResponse = response?.contentType?.includes("application/json");

  return (
    <div className="devtools-tool">
      <div className="devtools-http-bar">
        <select
          className="devtools-select devtools-select-method"
          value={method}
          onChange={(e) => setMethod(e.target.value as (typeof METHODS)[number])}
        >
          {METHODS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
        <input
          className="devtools-input devtools-input-url"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://httpbin.org/get"
          spellCheck={false}
          onKeyDown={(e) => {
            if (e.key === "Enter") handleSend();
          }}
        />
        <button
          type="button"
          className="btn btn-primary btn-small"
          onClick={handleSend}
          disabled={sending}
        >
          {sending ? <Loader2 size={14} className="spin" /> : <Send size={14} />}
          {sending ? t("http.sending") : t("http.send")}
        </button>
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <XCircle size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      <div className="devtools-http-tabs">
        <button
          type="button"
          className={`devtools-http-tab ${tab === "headers" ? "active" : ""}`}
          onClick={() => setTab("headers")}
        >
          {t("http.headers")} ({headerCount})
        </button>
        <button
          type="button"
          className={`devtools-http-tab ${tab === "body" ? "active" : ""}`}
          onClick={() => setTab("body")}
        >
          {t("http.body")}
        </button>
        <button
          type="button"
          className={`devtools-http-tab ${tab === "response" ? "active" : ""}`}
          onClick={() => setTab("response")}
        >
          {t("http.response")} {response ? `· ${response.status}` : ""}
        </button>
      </div>

      {tab === "headers" && (
        <div className="devtools-headers">
          {headers.map((h) => (
            <div key={h.id} className="devtools-header-row">
              <input
                className="devtools-input"
                value={h.key}
                onChange={(e) => updateHeader(h.id, "key", e.target.value)}
                placeholder={t("http.headerKey")}
                spellCheck={false}
              />
              <input
                className="devtools-input"
                value={h.value}
                onChange={(e) => updateHeader(h.id, "value", e.target.value)}
                placeholder={t("http.headerValue")}
                spellCheck={false}
              />
              <button
                type="button"
                className="btn btn-secondary btn-small icon-only"
                onClick={() => removeHeader(h.id)}
                disabled={headers.length <= 1}
              >
                <Trash2 size={12} />
              </button>
            </div>
          ))}
          <button type="button" className="btn btn-secondary btn-small" onClick={addHeader}>
            <Plus size={14} />
            {t("http.addHeader")}
          </button>
        </div>
      )}

      {tab === "body" && (
        <div className="devtools-io-pane">
          <textarea
            className="devtools-textarea"
            value={body}
            onChange={(e) => setBody(e.target.value)}
            placeholder='{"key": "value"}'
            rows={10}
            spellCheck={false}
          />
        </div>
      )}

      {tab === "response" && (
        <div className="devtools-response">
          {!response ? (
            <div className="runtime-hint">{t("http.send")}…</div>
          ) : (
            <>
              <div className="devtools-response-meta">
                <span className={`devtools-status ${statusClass}`}>
                  {response.status}
                </span>
                <span className="devtools-duration">{response.duration_ms} ms</span>
                {isJsonResponse && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={handleFormatBody}
                  >
                    <Wrench size={12} />
                    {t("http.formatJson")}
                  </button>
                )}
                <div className="devtools-actions-spacer" />
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={handleCopyBody}
                >
                  <ClipboardCopy size={14} />
                  {t("http.copyBody")}
                </button>
              </div>

              <div className="devtools-response-body">
                <label className="devtools-label">{t("http.responseBody")}</label>
                <pre className="devtools-pre">{response.body || " "}</pre>
              </div>

              <div className="devtools-response-headers-section">
                <label className="devtools-label">
                  {t("http.responseHeaders")} ({response.headers.length})
                </label>
                <div className="devtools-response-headers-list">
                  {response.headers.map((h, i) => (
                    <div key={i} className="devtools-response-header">
                      <strong>{h.key}:</strong> {h.value}
                    </div>
                  ))}
                </div>
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}

export default HttpClientTool;
