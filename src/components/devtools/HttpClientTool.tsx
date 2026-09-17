import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ClipboardCopy,
  History,
  Loader2,
  Plus,
  Send,
  Trash2,
  Wrench,
  XCircle,
  ChevronDown,
  ChevronRight,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";

type Tab = "headers" | "body" | "response";

interface HeaderRow {
  id: number;
  key: string;
  value: string;
}

interface RequestHistoryItem {
  id: number;
  method: string;
  url: string;
  timestamp: number;
}

let headerId = 1;
function newHeader(key = "", value = ""): HeaderRow {
  return { id: headerId++, key, value };
}

let historyId = 1;

const METHODS = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"] as const;

const COMMON_HEADERS: { label: string; key: string; value: string }[] = [
  { label: "JSON", key: "Content-Type", value: "application/json" },
  { label: "Form", key: "Content-Type", value: "application/x-www-form-urlencoded" },
  { label: "Auth", key: "Authorization", value: "Bearer " },
  { label: "Basic", key: "Authorization", value: "Basic " },
  { label: "Accept", key: "Accept", value: "application/json" },
];

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

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

function generateCurl(
  method: string,
  url: string,
  headers: { key: string; value: string }[],
  body?: string
): string {
  const parts = [`curl -X ${method}`];
  for (const h of headers) {
    if (h.key.trim()) {
      parts.push(`-H '${h.key}: ${h.value}'`);
    }
  }
  if (body && method !== "GET" && method !== "HEAD") {
    parts.push(`-d '${body.replace(/'/g, "'\\''")}'`);
  }
  parts.push(`'${url}'`);
  return parts.join(" \\\n  ");
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
    size: number;
  } | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [history, setHistory] = useState<RequestHistoryItem[]>([]);
  const [showHistory, setShowHistory] = useState(false);
  const [formatOnSend, setFormatOnSend] = useState(true);

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

  const addCommonHeader = (key: string, value: string) => {
    // Check if header already exists
    const existing = headers.find((h) => h.key.toLowerCase() === key.toLowerCase());
    if (existing) {
      updateHeader(existing.id, "value", value);
    } else {
      setHeaders((hs) => [...hs, newHeader(key, value)]);
    }
  };

  const flash = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 3000);
  }, []);

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

    // Auto-format JSON body if enabled
    let finalBody = body;
    if (formatOnSend && body.trim()) {
      const { ok, result } = formatJson(body);
      if (ok) {
        finalBody = result;
        if (finalBody !== body) {
          setBody(finalBody);
        }
      }
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
        body: method !== "GET" && method !== "HEAD" ? finalBody || undefined : undefined,
        timeout_sec: 30,
      });
      const contentType = detectContentType(resp.headers);
      const size = new Blob([resp.body]).size;
      setResponse({
        status: resp.status,
        duration_ms: resp.duration_ms,
        headers: resp.headers,
        body: resp.body,
        contentType,
        size,
      });

      // Add to history
      setHistory((prev) => {
        const newItem: RequestHistoryItem = {
          id: historyId++,
          method,
          url: trimmed,
          timestamp: Date.now(),
        };
        const filtered = prev.filter(
          (h) => !(h.method === method && h.url === trimmed)
        );
        return [newItem, ...filtered].slice(0, 20);
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
      flash("success", t("http.formatted"));
    }
  };

  const handleFormatRequestBody = () => {
    const { ok, result } = formatJson(body);
    if (ok) {
      setBody(result);
      flash("success", t("http.formatted"));
    }
  };

  const handleCopyCurl = async () => {
    const hdrs = headers.filter((h) => h.key.trim()).map((h) => ({ key: h.key, value: h.value }));
    const curl = generateCurl(method, url.trim(), hdrs, body);
    try {
      await copy(curl);
      flash("success", t("http.copied"));
    } catch (e) {
      flash("error", String(e));
    }
  };

  const handleLoadHistory = (item: RequestHistoryItem) => {
    setMethod(item.method as (typeof METHODS)[number]);
    setUrl(item.url);
    setShowHistory(false);
  };

  const handleClearHistory = () => {
    setHistory([]);
    setShowHistory(false);
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
        <div className="devtools-history-dropdown">
          <button
            type="button"
            className="btn btn-secondary btn-small icon-only"
            onClick={() => setShowHistory(!showHistory)}
            title={t("http.history")}
          >
            <History size={14} />
            {showHistory ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
          </button>
          {showHistory && (
            <div className="devtools-history-menu">
              {history.length === 0 ? (
                <div className="devtools-history-empty">{t("http.noHistory")}</div>
              ) : (
                <>
                  <div className="devtools-history-header">
                    <span>{t("http.history")}</span>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small icon-only"
                      onClick={handleClearHistory}
                      title={t("http.clearHistory")}
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                  {history.map((item) => (
                    <button
                      key={item.id}
                      type="button"
                      className="devtools-history-item"
                      onClick={() => handleLoadHistory(item)}
                    >
                      <span className={`tag tag-${item.method === "GET" ? "tcp" : "service"}`}>
                        {item.method}
                      </span>
                      <span className="devtools-history-url">{item.url}</span>
                    </button>
                  ))}
                </>
              )}
            </div>
          )}
        </div>
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
          <div className="devtools-headers-presets">
            {COMMON_HEADERS.map((preset) => (
              <button
                key={preset.label}
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => addCommonHeader(preset.key, preset.value)}
              >
                + {preset.label}
              </button>
            ))}
          </div>
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
          <div className="devtools-body-toolbar">
            <label className="devtools-checkbox">
              <input
                type="checkbox"
                checked={formatOnSend}
                onChange={(e) => setFormatOnSend(e.target.checked)}
              />
              {t("http.autoFormat")}
            </label>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={handleFormatRequestBody}
            >
              <Wrench size={12} />
              {t("http.formatJson")}
            </button>
          </div>
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
            <div className="runtime-hint">{t("http.sendHint")}</div>
          ) : (
            <>
              <div className="devtools-response-meta">
                <span className={`devtools-status ${statusClass}`}>
                  {response.status}
                </span>
                <span className="devtools-duration">
                  <strong>{response.duration_ms}</strong> ms
                </span>
                <span className="devtools-duration">
                  {formatBytes(response.size)}
                </span>
                <div className="devtools-actions-spacer" />
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

              <div className="devtools-curl-section">
                <label className="devtools-label">{t("http.curlLabel")}</label>
                <div className="devtools-curl-bar">
                  <code className="devtools-curl-preview">
                    {generateCurl(
                      method,
                      url.trim(),
                      headers.filter((h) => h.key.trim()),
                      body
                    ).slice(0, 200)}
                    {generateCurl(
                      method,
                      url.trim(),
                      headers.filter((h) => h.key.trim()),
                      body
                    ).length > 200
                      ? "…"
                      : ""}
                  </code>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={handleCopyCurl}
                  >
                    <ClipboardCopy size={14} />
                    {t("http.copyCurl")}
                  </button>
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
