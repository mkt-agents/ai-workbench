import { useCallback, useState, useMemo } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ChevronDown,
  ChevronRight,
  ClipboardCopy,
  History,
  Loader2,
  Plus,
  Send,
  Trash2,
  Wrench,
  XCircle,
  FileInput,
  X,
  Bookmark,
  BookmarkCheck,
  Search,
  Download,
  Upload,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";

type Tab = "params" | "headers" | "body" | "response";

type BodyType = "none" | "json" | "text" | "form";

interface HeaderRow {
  id: number;
  key: string;
  value: string;
}

interface ParamRow {
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

interface SavedRequest {
  id: string;
  name: string;
  method: string;
  url: string;
  headers: { key: string; value: string }[];
  params: { key: string; value: string }[];
  body: string;
  bodyType: BodyType;
  savedAt: number;
}

let headerId = 1;
function newHeader(key = "", value = ""): HeaderRow {
  return { id: headerId++, key, value };
}

let paramId = 1;
function newParam(key = "", value = ""): ParamRow {
  return { id: paramId++, key, value };
}

let historyId = 1;

const METHODS = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"] as const;

const COMMON_HEADERS: { label: string; key: string; value: string }[] = [
  { label: "JSON", key: "Content-Type", value: "application/json" },
  { label: "Form", key: "Content-Type", value: "application/x-www-form-urlencoded" },
  { label: "Text", key: "Content-Type", value: "text/plain" },
  { label: "Auth", key: "Authorization", value: "Bearer " },
  { label: "Basic", key: "Authorization", value: "Basic " },
  { label: "Accept", key: "Accept", value: "application/json" },
];

const SAVED_REQUESTS_KEY = "ai-workbench.http-saved-requests";

function loadSavedRequests(): SavedRequest[] {
  try {
    const raw = localStorage.getItem(SAVED_REQUESTS_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
}

function saveSavedRequests(items: SavedRequest[]) {
  localStorage.setItem(SAVED_REQUESTS_KEY, JSON.stringify(items));
}

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

interface ParsedCurl {
  method: string;
  url: string;
  headers: { key: string; value: string }[];
  body: string;
}

function parseCurl(input: string): ParsedCurl | null {
  const result: ParsedCurl = {
    method: "GET",
    url: "",
    headers: [],
    body: "",
  };

  const text = input.trim();
  if (!text.toLowerCase().startsWith("curl")) return null;

  // Tokenize respecting single/double quotes
  const tokens: string[] = [];
  let cur = "";
  let quote: string | null = null;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quote) {
      if (ch === quote) {
        quote = null;
      } else {
        cur += ch;
      }
    } else if (ch === "'" || ch === '"') {
      quote = ch;
    } else if (/\s/.test(ch)) {
      if (cur) {
        tokens.push(cur);
        cur = "";
      }
    } else {
      cur += ch;
    }
  }
  if (cur) tokens.push(cur);

  // Known curl flags (long + short) that take an argument
  const valueFlags = new Set([
    "-X", "--request",
    "-H", "--header",
    "-d", "--data", "--data-raw", "--data-binary", "--data-ascii",
    "-u", "--user",
    "-A", "--user-agent",
    "-e", "--referer",
    "-F", "--form",
    "--url",
  ]);
  // Boolean flags we can safely ignore
  const boolFlags = new Set([
    "-k", "--insecure",
    "-L", "--location",
    "--compressed",
    "-s", "--silent",
    "-i", "--include",
    "-v", "--verbose",
  ]);

  let i = 1; // skip "curl"
  while (i < tokens.length) {
    const tok = tokens[i];
    if (tok.startsWith("-")) {
      const flag = tok;
      const next = i + 1 < tokens.length ? tokens[i + 1] : undefined;
      if (valueFlags.has(flag) && next !== undefined) {
        const val = next;
        i += 2;
        switch (flag) {
          case "-X":
          case "--request":
            result.method = val.toUpperCase();
            break;
          case "-H":
          case "--header": {
            const colonIdx = val.indexOf(":");
            if (colonIdx > 0) {
              const k = val.slice(0, colonIdx).trim();
              const v = val.slice(colonIdx + 1).trim();
              // Skip pseudo-headers and content-length (curl manages those)
              if (!k.startsWith(":") && k.toLowerCase() !== "content-length") {
                result.headers.push({ key: k, value: v });
              }
            }
            break;
          }
          case "-d":
          case "--data":
          case "--data-raw":
          case "--data-binary":
          case "--data-ascii":
            // Multiple -d flags: join with &
            result.body = result.body ? result.body + "&" + val : val;
            if (result.method === "GET") result.method = "POST";
            break;
          case "-u":
          case "--user": {
            // user:pass → Authorization: Basic base64
            const encoded = (() => {
              try {
                return btoa(val);
              } catch {
                return "";
              }
            })();
            if (encoded) {
              result.headers.push({ key: "Authorization", value: `Basic ${encoded}` });
            }
            break;
          }
          case "-A":
          case "--user-agent":
            result.headers.push({ key: "User-Agent", value: val });
            break;
          case "-e":
          case "--referer":
            result.headers.push({ key: "Referer", value: val });
            break;
          case "--url":
            result.url = val;
            break;
          case "-F":
          case "--form":
            // Form data — append to body as urlencoded-ish
            {
              const eqIdx = val.indexOf("=");
              if (eqIdx > 0) {
                const fk = val.slice(0, eqIdx);
                const fv = val.slice(eqIdx + 1);
                result.body = result.body ? result.body + "&" + `${fk}=${fv}` : `${fk}=${fv}`;
              }
            }
            if (result.method === "GET") result.method = "POST";
            break;
          default:
            break;
        }
      } else if (boolFlags.has(flag)) {
        i += 1;
      } else if (flag === "-G" || flag === "--get") {
        result.method = "GET";
        i += 1;
      } else {
        // Unknown flag with possible value — skip
        i += 1;
      }
    } else {
      // Positional argument = URL (skip the literal "://" fragment if it leaked)
      if (tok !== "://" && !result.url) {
        result.url = tok;
      }
      i += 1;
    }
  }

  if (!result.url) return null;
  return result;
}

function HttpClientTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);
  const httpRequest = useGlobalStore((s) => s.invokeHttpRequest);

  const [method, setMethod] = useState<(typeof METHODS)[number]>("GET");
  const [url, setUrl] = useState("");
  const [baseUrl, setBaseUrl] = useState(""); // URL without query params
  const [params, setParams] = useState<ParamRow[]>([]);
  const [headers, setHeaders] = useState<HeaderRow[]>([newHeader("Accept", "*/*")]);
  const [body, setBody] = useState("");
  const [bodyType, setBodyType] = useState<BodyType>("none");
  const [tab, setTab] = useState<Tab>("params");
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
  const [showImportCurl, setShowImportCurl] = useState(false);
  const [importCurlText, setImportCurlText] = useState("");
  const [searchText, setSearchText] = useState("");
  const [savedRequests, setSavedRequests] = useState<SavedRequest[]>(loadSavedRequests);
  const [showSaved, setShowSaved] = useState(false);
  const [saveName, setSaveName] = useState("");
  const [showSaveDialog, setShowSaveDialog] = useState(false);

  const headerCount = headers.filter((h) => h.key.trim()).length;
  const paramCount = params.filter((p) => p.key.trim()).length;

  // Build full URL with query params
  const fullUrl = useMemo(() => {
    const base = baseUrl.trim();
    if (!base) return "";
    const searchParams = new URLSearchParams();
    for (const p of params) {
      if (p.key.trim()) {
        searchParams.append(p.key.trim(), p.value);
      }
    }
    const qs = searchParams.toString();
    return qs ? `${base}?${qs}` : base;
  }, [baseUrl, params]);

  // Sync full URL to url state when sending
  const getSendUrl = useCallback(() => {
    if (fullUrl) return fullUrl;
    return url.trim();
  }, [fullUrl, url]);

  // Parse URL when user types in the main input
  const handleUrlChange = (value: string) => {
    setUrl(value);
    // Try to parse query params
    try {
      const qIndex = value.indexOf("?");
      if (qIndex > 0) {
        const base = value.slice(0, qIndex);
        const qs = value.slice(qIndex + 1);
        setBaseUrl(base);
        const sp = new URLSearchParams(qs);
        const newParams: ParamRow[] = [];
        sp.forEach((v, k) => {
          newParams.push(newParam(k, v));
        });
        setParams(newParams.length > 0 ? newParams : [newParam()]);
      } else {
        setBaseUrl(value);
      }
    } catch {
      setBaseUrl(value);
    }
  };

  const updateParam = (id: number, field: "key" | "value", val: string) => {
    setParams((ps) => ps.map((p) => (p.id === id ? { ...p, [field]: val } : p)));
  };

  const addParam = () => {
    setParams((ps) => [...ps, newParam()]);
  };

  const removeParam = (id: number) => {
    setParams((ps) => (ps.length <= 1 ? ps : ps.filter((p) => p.id !== id)));
  };

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

  // Auto-set Content-Type when body type changes
  const handleBodyTypeChange = (newType: BodyType) => {
    setBodyType(newType);
    const ctMap: Record<BodyType, string> = {
      none: "",
      json: "application/json",
      text: "text/plain",
      form: "application/x-www-form-urlencoded",
    };
    const newCT = ctMap[newType];
    if (newCT) {
      // Remove existing Content-Type
      const withoutCT = headers.filter((h) => h.key.toLowerCase() !== "content-type");
      setHeaders([...withoutCT, newHeader("Content-Type", newCT)]);
    }
  };

  const flash = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 3000);
  }, []);

  const handleSend = async () => {
    const sendUrl = getSendUrl();
    const trimmed = sendUrl.trim();
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
    if (formatOnSend && body.trim() && bodyType === "json") {
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
        body: method !== "GET" && method !== "HEAD" && bodyType !== "none" ? finalBody || undefined : undefined,
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

  const handleSaveRequest = () => {
    if (!saveName.trim()) return;
    const sendUrl = getSendUrl();
    const newReq: SavedRequest = {
      id: `req-${Date.now()}`,
      name: saveName.trim(),
      method,
      url: sendUrl.trim(),
      headers: headers.filter((h) => h.key.trim()).map((h) => ({ key: h.key, value: h.value })),
      params: params.filter((p) => p.key.trim()).map((p) => ({ key: p.key, value: p.value })),
      body,
      bodyType,
      savedAt: Date.now(),
    };
    const updated = [newReq, ...savedRequests.filter((r) => r.name !== newReq.name)];
    setSavedRequests(updated);
    saveSavedRequests(updated);
    setShowSaveDialog(false);
    setSaveName("");
    flash("success", t("http.saved"));
  };

  const handleLoadRequest = (req: SavedRequest) => {
    setMethod(req.method as (typeof METHODS)[number]);
    setUrl(req.url);
    try {
      const qIndex = req.url.indexOf("?");
      if (qIndex > 0) {
        setBaseUrl(req.url.slice(0, qIndex));
        const sp = new URLSearchParams(req.url.slice(qIndex + 1));
        const newParams: ParamRow[] = [];
        sp.forEach((v, k) => newParams.push(newParam(k, v)));
        setParams(newParams.length > 0 ? newParams : [newParam()]);
      } else {
        setBaseUrl(req.url);
        setParams([newParam()]);
      }
    } catch {
      setBaseUrl(req.url);
      setParams([newParam()]);
    }
    setHeaders(req.headers.length > 0 ? req.headers.map((h) => newHeader(h.key, h.value)) : [newHeader("Accept", "*/*")]);
    setBody(req.body);
    setBodyType(req.bodyType || "none");
    setShowSaved(false);
  };

  const handleDeleteSaved = (id: string) => {
    const updated = savedRequests.filter((r) => r.id !== id);
    setSavedRequests(updated);
    saveSavedRequests(updated);
  };

  const handleExportSaved = async () => {
    const data = JSON.stringify(savedRequests, null, 2);
    try {
      await copy(data);
      flash("success", t("http.copied"));
    } catch {
      flash("error", "Export failed");
    }
  };

  const handleImportSaved = () => {
    // Trigger file input
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".json";
    input.onchange = async (e) => {
      const file = (e.target as HTMLInputElement).files?.[0];
      if (!file) return;
      try {
        const text = await file.text();
        const imported = JSON.parse(text);
        if (Array.isArray(imported)) {
          const merged = [...imported, ...savedRequests];
          setSavedRequests(merged);
          saveSavedRequests(merged);
          flash("success", t("http.imported", { count: imported.length }));
        }
      } catch {
        flash("error", t("http.importFailed"));
      }
    };
    input.click();
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
    const curl = generateCurl(method, getSendUrl(), hdrs, body);
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
    // Parse URL for params
    try {
      const qIndex = item.url.indexOf("?");
      if (qIndex > 0) {
        setBaseUrl(item.url.slice(0, qIndex));
        const sp = new URLSearchParams(item.url.slice(qIndex + 1));
        const newParams: ParamRow[] = [];
        sp.forEach((v, k) => newParams.push(newParam(k, v)));
        setParams(newParams.length > 0 ? newParams : [newParam()]);
      } else {
        setBaseUrl(item.url);
        setParams([newParam()]);
      }
    } catch {
      setBaseUrl(item.url);
      setParams([newParam()]);
    }
    setShowHistory(false);
  };

  const handleClearHistory = () => {
    setHistory([]);
    setShowHistory(false);
  };

  const handleImportCurl = () => {
    const parsed = parseCurl(importCurlText);
    if (!parsed) {
      flash("error", t("http.curlInvalid"));
      return;
    }
    const validMethod = (METHODS as readonly string[]).includes(parsed.method)
      ? parsed.method
      : "GET";
    setMethod(validMethod as (typeof METHODS)[number]);
    setUrl(parsed.url);
    // Parse URL for params
    try {
      const qIndex = parsed.url.indexOf("?");
      if (qIndex > 0) {
        setBaseUrl(parsed.url.slice(0, qIndex));
        const sp = new URLSearchParams(parsed.url.slice(qIndex + 1));
        const newParams: ParamRow[] = [];
        sp.forEach((v, k) => newParams.push(newParam(k, v)));
        setParams(newParams.length > 0 ? newParams : [newParam()]);
      } else {
        setBaseUrl(parsed.url);
        setParams([newParam()]);
      }
    } catch {
      setBaseUrl(parsed.url);
      setParams([newParam()]);
    }
    setHeaders(
      parsed.headers.length > 0
        ? parsed.headers.map((h) => newHeader(h.key, h.value))
        : [newHeader("Accept", "*/*")]
    );
    setBody(parsed.body);
    setShowImportCurl(false);
    setImportCurlText("");
    flash("success", t("http.curlImported"));
  };

  const statusClass =
    response && response.status >= 200 && response.status < 300
      ? "status-ok"
      : response && response.status >= 400
        ? "status-err"
        : response
          ? "status-other"
          : "";

  const isJsonResponse = response?.contentType?.includes("application/json");

  // Response search
  const responseMatches = useMemo(() => {
    if (!response || !searchText.trim()) return [];
    const regex = new RegExp(searchText.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi");
    const matches: number[] = [];
    let m;
    while ((m = regex.exec(response.body)) !== null) {
      matches.push(m.index);
    }
    return matches;
  }, [response, searchText]);

  return (
    <div className="devtools-tool http-tool">
      {/* ── Toolbar ── */}
      <div className="http-toolbar">
        <select
          className="http-method-select"
          value={method}
          onChange={(e) => setMethod(e.target.value as (typeof METHODS)[number])}
        >
          {METHODS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
        <div className="http-url-bar">
          <input
            className="devtools-input"
            value={fullUrl || url}
            onChange={(e) => handleUrlChange(e.target.value)}
            placeholder="https://httpbin.org/get"
            spellCheck={false}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleSend();
            }}
          />
        </div>
        <button
          type="button"
          className="btn btn-primary btn-small http-send-btn"
          onClick={handleSend}
          disabled={sending}
        >
          {sending ? <Loader2 size={14} className="spin" /> : <Send size={14} />}
          {sending ? t("http.sending") : t("http.send")}
        </button>

        <div className="http-toolbar-spacer" />

        <button
          type="button"
          className="http-action-btn"
          onClick={() => setShowSaveDialog(true)}
          title={t("http.save")}
        >
          <Bookmark size={14} />
          <span>{t("http.save")}</span>
        </button>

        <button
          type="button"
          className="http-action-btn"
          onClick={() => setShowImportCurl(true)}
          title={t("http.importCurl")}
        >
          <FileInput size={14} />
          <span>{t("http.importCurl")}</span>
        </button>

        <div className="http-history">
          <button
            type="button"
            className="http-action-btn"
            onClick={() => {
              setShowHistory(!showHistory);
              if (!showHistory) setShowSaved(false);
            }}
            title={t("http.history")}
          >
            <History size={14} />
            <span>{t("http.history")}</span>
            {showHistory ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
          </button>
          {showHistory && (
            <div className="http-history-menu">
              {history.length === 0 ? (
                <div className="http-history-empty">{t("http.noHistory")}</div>
              ) : (
                <>
                  <div className="http-history-header">
                    <span><History size={12} style={{ verticalAlign: "middle", marginRight: 6 }} />{t("http.history")}</span>
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
                      className="http-history-item"
                      onClick={() => handleLoadHistory(item)}
                    >
                      <span className={`http-method-badge ${item.method}`}>
                        {item.method}
                      </span>
                      <span className="http-history-url">{item.url}</span>
                    </button>
                  ))}
                </>
              )}
            </div>
          )}
        </div>

        <div className="http-saved">
          <button
            type="button"
            className="http-action-btn"
            onClick={() => {
              setShowSaved(!showSaved);
              if (!showSaved) setShowHistory(false);
            }}
            title={t("http.saved")}
          >
            <BookmarkCheck size={14} />
            <span>{t("http.saved")}</span>
            {showSaved ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
          </button>
          {showSaved && (
            <div className="http-history-menu http-saved-menu">
              <div className="http-history-header">
                <span><BookmarkCheck size={12} style={{ verticalAlign: "middle", marginRight: 6 }} />{t("http.saved")}</span>
                <div className="http-saved-actions">
                  <button
                    type="button"
                    className="btn btn-secondary btn-small icon-only"
                    onClick={handleImportSaved}
                    title={t("http.import")}
                  >
                    <Upload size={12} />
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small icon-only"
                    onClick={handleExportSaved}
                    title={t("http.export")}
                    disabled={savedRequests.length === 0}
                  >
                    <Download size={12} />
                  </button>
                </div>
              </div>
              {savedRequests.length === 0 ? (
                <div className="http-history-empty">{t("http.noSaved")}</div>
              ) : (
                savedRequests.map((req) => (
                  <div key={req.id} className="http-saved-item">
                    <button
                      type="button"
                      className="http-saved-load"
                      onClick={() => handleLoadRequest(req)}
                    >
                      <span className={`http-method-badge ${req.method}`}>
                        {req.method}
                      </span>
                      <span className="http-saved-name">{req.name}</span>
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small icon-only http-saved-delete"
                      onClick={() => handleDeleteSaved(req.id)}
                      title={t("http.delete")}
                    >
                      <Trash2 size={11} />
                    </button>
                  </div>
                ))
              )}
            </div>
          )}
        </div>
      </div>

      {/* ── Status ── */}
      {message && (
        <div className={`http-status ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <XCircle size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── Tabs ── */}
      <div className="http-tabs">
        <button
          type="button"
          className={`http-tab ${tab === "params" ? "active" : ""}`}
          onClick={() => setTab("params")}
        >
          {t("http.params")} ({paramCount})
        </button>
        <button
          type="button"
          className={`http-tab ${tab === "headers" ? "active" : ""}`}
          onClick={() => setTab("headers")}
        >
          {t("http.headers")} ({headerCount})
        </button>
        <button
          type="button"
          className={`http-tab ${tab === "body" ? "active" : ""}`}
          onClick={() => setTab("body")}
        >
          {t("http.body")}
        </button>
        <button
          type="button"
          className={`http-tab ${tab === "response" ? "active" : ""}`}
          onClick={() => setTab("response")}
        >
          {t("http.response")} {response ? `· ${response.status}` : ""}
        </button>
      </div>

      {/* ── Params tab ── */}
      {tab === "params" && (
        <div className="http-section">
          <div className="http-section-title">
            <Search size={13} />
            {t("http.params")}
          </div>
          <div className="http-headers">
            {params.map((p) => (
              <div key={p.id} className="http-header-row">
                <input
                  className="devtools-input"
                  value={p.key}
                  onChange={(e) => updateParam(p.id, "key", e.target.value)}
                  placeholder={t("http.paramKey")}
                  spellCheck={false}
                />
                <input
                  className="devtools-input"
                  value={p.value}
                  onChange={(e) => updateParam(p.id, "value", e.target.value)}
                  placeholder={t("http.paramValue")}
                  spellCheck={false}
                />
                <button
                  type="button"
                  className="btn btn-secondary btn-small icon-only"
                  onClick={() => removeParam(p.id)}
                  disabled={params.length <= 1}
                >
                  <Trash2 size={12} />
                </button>
              </div>
            ))}
            <button type="button" className="btn btn-secondary btn-small" onClick={addParam}>
              <Plus size={14} />
              {t("http.addParam")}
            </button>
          </div>
        </div>
      )}

      {/* ── Headers tab ── */}
      {tab === "headers" && (
        <div className="http-section">
          <div className="http-section-title">
            <Plus size={13} />
            {t("http.headers")}
          </div>
          <div className="http-headers">
            <div className="http-headers-presets">
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
              <div key={h.id} className="http-header-row">
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
        </div>
      )}

      {/* ── Body tab ── */}
      {tab === "body" && (
        <div className="http-section">
          <div className="http-section-title">
            <Wrench size={13} />
            {t("http.body")}
          </div>
          <div className="http-body">
            <div className="http-body-toolbar">
              <div className="http-body-type-group">
                {(["none", "json", "text", "form"] as BodyType[]).map((bt) => (
                  <button
                    key={bt}
                    type="button"
                    className={`http-body-type-btn ${bodyType === bt ? "active" : ""}`}
                    onClick={() => handleBodyTypeChange(bt)}
                  >
                    {bt === "none" ? t("http.bodyNone") : bt.toUpperCase()}
                  </button>
                ))}
              </div>
              {bodyType === "json" && (
                <>
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
                </>
              )}
            </div>
            {bodyType !== "none" ? (
              <textarea
                className="devtools-textarea"
                value={body}
                onChange={(e) => setBody(e.target.value)}
                placeholder={bodyType === "json" ? '{"key": "value"}' : bodyType === "form" ? "key1=value1&key2=value2" : "Raw text…"}
                rows={10}
                spellCheck={false}
              />
            ) : (
              <div className="http-body-disabled">{t("http.bodyDisabled")}</div>
            )}
          </div>
        </div>
      )}

      {/* ── Response tab ── */}
      {tab === "response" && (
        <div className="http-response">
          {!response ? (
            <div className="http-hint">
              <Send size={16} style={{ opacity: 0.5 }} />
              {t("http.sendHint")}
            </div>
          ) : (
            <>
              <div className="http-response-meta">
                <span className={`http-response-status ${statusClass}`}>
                  {response.status}
                </span>
                <span className="http-response-stat">
                  <strong>{response.duration_ms}</strong> ms
                </span>
                <span className="http-response-stat">
                  {formatBytes(response.size)}
                </span>
                <div className="http-response-actions-spacer" />
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

              {/* Response search */}
              <div className="http-response-search">
                <Search size={13} className="http-response-search-icon" />
                <input
                  className="devtools-input http-response-search-input"
                  value={searchText}
                  onChange={(e) => setSearchText(e.target.value)}
                  placeholder={t("http.searchResponse")}
                  spellCheck={false}
                />
                {searchText.trim() && (
                  <span className="http-response-search-count">
                    {responseMatches.length} {t("http.matches")}
                  </span>
                )}
              </div>

              <div className="http-response-body">
                <label className="devtools-label">{t("http.responseBody")}</label>
                <pre className="devtools-pre">
                  {searchText.trim() && responseMatches.length > 0 ? (
                    highlightText(response.body, searchText)
                  ) : (
                    response.body || " "
                  )}
                </pre>
              </div>

              <div className="http-response-headers-section">
                <label className="devtools-label">
                  {t("http.responseHeaders")} ({response.headers.length})
                </label>
                <div className="http-response-headers-list">
                  {response.headers.map((h, i) => (
                    <div key={i} className="http-response-header">
                      <strong>{h.key}:</strong> {h.value}
                    </div>
                  ))}
                </div>
              </div>

              <div className="http-curl-section">
                <label className="devtools-label">{t("http.curlLabel")}</label>
                <div className="http-curl-bar">
                  <code className="devtools-pre http-curl-preview">
                    {generateCurl(
                      method,
                      getSendUrl(),
                      headers.filter((h) => h.key.trim()),
                      body
                    ).slice(0, 200)}
                    {generateCurl(
                      method,
                      getSendUrl(),
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

      {/* ── Import cURL modal ── */}
      {showImportCurl && (
        <div className="modal-overlay" onClick={() => setShowImportCurl(false)}>
          <div className="modal http-import-modal" onClick={(e) => e.stopPropagation()}>
            <div className="plugin-edit-modal-header">
              <FileInput size={16} />
              {t("http.importCurlTitle")}
              <button
                type="button"
                className="modal-close"
                onClick={() => setShowImportCurl(false)}
                aria-label={t("common.clear")}
              >
                <X size={16} />
              </button>
            </div>
            <div className="http-import-body">
              <p className="http-import-hint">{t("http.curlHint")}</p>
              <textarea
                className="devtools-textarea"
                value={importCurlText}
                onChange={(e) => setImportCurlText(e.target.value)}
                placeholder={t("http.importCurlPlaceholder")}
                rows={6}
                spellCheck={false}
                autoFocus
              />
            </div>
            <div className="modal-actions">
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setShowImportCurl(false)}
              >
                {t("common.clear")}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={handleImportCurl}
                disabled={!importCurlText.trim()}
              >
                <FileInput size={14} />
                {t("http.importCurl")}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* ── Save request modal ── */}
      {showSaveDialog && (
        <div className="modal-overlay" onClick={() => setShowSaveDialog(false)}>
          <div className="modal http-import-modal" onClick={(e) => e.stopPropagation()}>
            <div className="plugin-edit-modal-header">
              <Bookmark size={16} />
              {t("http.saveRequest")}
              <button
                type="button"
                className="modal-close"
                onClick={() => setShowSaveDialog(false)}
                aria-label={t("common.clear")}
              >
                <X size={16} />
              </button>
            </div>
            <div className="http-import-body">
              <div className="input-group">
                <label className="input-label">{t("http.requestName")}</label>
                <input
                  className="input-field"
                  value={saveName}
                  onChange={(e) => setSaveName(e.target.value)}
                  placeholder={t("http.requestNamePlaceholder")}
                  autoFocus
                  onKeyDown={(e) => {
                    if (e.key === "Enter") handleSaveRequest();
                    if (e.key === "Escape") setShowSaveDialog(false);
                  }}
                />
              </div>
            </div>
            <div className="modal-actions">
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setShowSaveDialog(false)}
              >
                {t("common.clear")}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={handleSaveRequest}
                disabled={!saveName.trim()}
              >
                <BookmarkCheck size={14} />
                {t("http.save")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// Helper to highlight search matches in response
function highlightText(text: string, query: string): React.ReactNode {
  if (!query.trim()) return text;
  const regex = new RegExp(`(${query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, "gi");
  const parts = text.split(regex);
  return parts.map((part, i) =>
    regex.test(part) ? (
      <mark key={i} className="http-search-highlight">
        {part}
      </mark>
    ) : (
      <span key={i}>{part}</span>
    )
  );
}

export default HttpClientTool;
