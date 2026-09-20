import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowRightLeft,
  Check,
  ChevronRight,
  ClipboardCopy,
  Code2,
  Eraser,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";

type Mode = "base64" | "url" | "base64url";
type Direction = "encode" | "decode";

function encodeBase64(text: string): string {
  return btoa(
    encodeURIComponent(text).replace(/%([0-9A-F]{2})/g, (_, p1) =>
      String.fromCharCode(parseInt(p1, 16)),
    ),
  );
}

function decodeBase64(text: string): string {
  const cleaned = text.replace(/\s+/g, "");
  const raw = atob(cleaned);
  return decodeURIComponent(
    raw
      .split("")
      .map((c) => "%" + ("00" + c.charCodeAt(0).toString(16)).slice(-2))
      .join(""),
  );
}

function encodeBase64Url(text: string): string {
  return encodeBase64(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function decodeBase64Url(text: string): string {
  let b64 = text.replace(/-/g, "+").replace(/_/g, "/");
  while (b64.length % 4) b64 += "=";
  const raw = atob(b64);
  return decodeURIComponent(
    raw
      .split("")
      .map((c) => "%" + ("00" + c.charCodeAt(0).toString(16)).slice(-2))
      .join(""),
  );
}

function EncoderTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [mode, setMode] = useState<Mode>("base64");
  const [direction, setDirection] = useState<Direction>("encode");
  const [input, setInput] = useState("");
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  useEffect(() => {
    setMessage(null);
  }, [mode, direction]);

  const output = useMemo(() => {
    if (!input) return "";
    try {
      if (mode === "base64") {
        return direction === "encode" ? encodeBase64(input) : decodeBase64(input);
      }
      if (mode === "base64url") {
        return direction === "encode" ? encodeBase64Url(input) : decodeBase64Url(input);
      }
      return direction === "encode"
        ? encodeURIComponent(input)
        : decodeURIComponent(input);
    } catch {
      return "";
    }
  }, [input, mode, direction]);

  const hasError = input.length > 0 && output === "";

  const handleCopy = async () => {
    try {
      await copy(output);
      setMessage({ type: "success", text: t("encoder.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  const swap = () => {
    setInput(output);
    setDirection(direction === "encode" ? "decode" : "encode");
  };

  const inputBytes = useMemo(() => new Blob([input]).size, [input]);

  const modeLabel = mode === "base64" ? "Base64" : mode === "base64url" ? "Base64URL" : "URL";

  return (
    <div className="devtools-tool encoder-tool">
      {/* ── Toolbar ── */}
      <div className="encoder-toolbar">
        <div className="encoder-toolbar-group">
          <span className="encoder-toolbar-label">{t("encoder.mode")}</span>
          <div className="encoder-segmented">
            <button
              type="button"
              className={`encoder-seg-item ${mode === "base64" ? "active" : ""}`}
              onClick={() => setMode("base64")}
            >
              Base64
            </button>
            <button
              type="button"
              className={`encoder-seg-item ${mode === "base64url" ? "active" : ""}`}
              onClick={() => setMode("base64url")}
            >
              Base64URL
            </button>
            <button
              type="button"
              className={`encoder-seg-item ${mode === "url" ? "active" : ""}`}
              onClick={() => setMode("url")}
            >
              URL
            </button>
          </div>
        </div>

        <div className="encoder-toolbar-divider" />

        <div className="encoder-toolbar-group">
          <span className="encoder-toolbar-label">{t("encoder.direction")}</span>
          <div className="encoder-segmented">
            <button
              type="button"
              className={`encoder-seg-item ${direction === "encode" ? "active" : ""}`}
              onClick={() => setDirection("encode")}
            >
              {t("encoder.encode")}
            </button>
            <button
              type="button"
              className={`encoder-seg-item ${direction === "decode" ? "active" : ""}`}
              onClick={() => setDirection("decode")}
            >
              {t("encoder.decode")}
            </button>
          </div>
        </div>

        <div className="encoder-toolbar-spacer" />

        <button
          type="button"
          className="encoder-icon-btn"
          onClick={swap}
          title={t("encoder.swap")}
        >
          <ArrowRightLeft size={14} />
        </button>
        <button
          type="button"
          className="encoder-icon-btn"
          onClick={() => void handleCopy()}
          disabled={!output}
          title={t("encoder.copy")}
        >
          <ClipboardCopy size={14} />
        </button>
        <button
          type="button"
          className="encoder-icon-btn"
          onClick={() => setInput("")}
          disabled={!input}
          title={t("encoder.clear")}
        >
          <Eraser size={14} />
        </button>
      </div>

      {/* ── Status ── */}
      {hasError && (
        <div className="encoder-status error">
          <Code2 size={13} />
          <span>{t("encoder.invalid")}</span>
        </div>
      )}
      {message && !hasError && (
        <div className={`encoder-status ${message.type}`}>
          {message.type === "success" ? <Check size={13} /> : <Code2 size={13} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── IO ── */}
      <div className="encoder-io">
        <div className="encoder-pane">
          <div className="encoder-pane-header">
            <span className="encoder-pane-title">{t("encoder.input")}</span>
            {input && (
              <span className="encoder-stats">
                {input.length} {t("encoder.chars")} · {inputBytes} {t("encoder.bytes")}
              </span>
            )}
          </div>
          <textarea
            className="encoder-textarea"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={t("encoder.sample")}
            spellCheck={false}
          />
        </div>

        <div className="encoder-flow-indicator">
          <ChevronRight size={16} />
          <span className="encoder-flow-tag">{modeLabel}</span>
          <ChevronRight size={16} />
        </div>

        <div className="encoder-pane">
          <div className="encoder-pane-header">
            <span className="encoder-pane-title">{t("encoder.output")}</span>
            {output && (
              <span className="encoder-stats">
                {output.length} {t("encoder.chars")}
              </span>
            )}
          </div>
          <textarea
            className="encoder-textarea output"
            value={output}
            readOnly
            placeholder={t("encoder.sample")}
            spellCheck={false}
          />
        </div>
      </div>
    </div>
  );
}

export default EncoderTool;
