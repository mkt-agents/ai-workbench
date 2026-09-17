import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Code2, Eraser, ArrowRightLeft } from "lucide-react";
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

  return (
    <div className="devtools-tool">
      <div className="devtools-row">
        <div className="devtools-segmented">
          <button
            type="button"
            className={`segmented-item ${mode === "base64" ? "active" : ""}`}
            onClick={() => setMode("base64")}
          >
            {t("encoder.base64")}
          </button>
          <button
            type="button"
            className={`segmented-item ${mode === "base64url" ? "active" : ""}`}
            onClick={() => setMode("base64url")}
          >
            Base64URL
          </button>
          <button
            type="button"
            className={`segmented-item ${mode === "url" ? "active" : ""}`}
            onClick={() => setMode("url")}
          >
            {t("encoder.url")}
          </button>
        </div>
        <div className="devtools-segmented">
          <button
            type="button"
            className={`segmented-item ${direction === "encode" ? "active" : ""}`}
            onClick={() => setDirection("encode")}
          >
            {t("encoder.encode")}
          </button>
          <button
            type="button"
            className={`segmented-item ${direction === "decode" ? "active" : ""}`}
            onClick={() => setDirection("decode")}
          >
            {t("encoder.decode")}
          </button>
        </div>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={swap}
          title={t("encoder.swap")}
        >
          <ArrowRightLeft size={14} />
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={handleCopy}>
          <ClipboardCopy size={14} />
          {t("encoder.copy")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={() => setInput("")}>
          <Eraser size={14} />
          {t("encoder.clear")}
        </button>
      </div>

      {hasError && (
        <div className="runtime-msg error">
          <Code2 size={14} />
          <span>{t("encoder.invalid")}</span>
        </div>
      )}
      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Code2 size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      <div className="devtools-io">
        <div className="devtools-io-pane">
          <div className="io-header">
            <label className="devtools-label">{t("encoder.input")}</label>
            <span className="json-stats">
              {input.length} {t("encoder.chars")} · {inputBytes} {t("encoder.bytes")}
            </span>
          </div>
          <textarea
            className="devtools-textarea"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={t("encoder.sample")}
            spellCheck={false}
          />
        </div>
        <div className="devtools-io-pane">
          <div className="io-header">
            <label className="devtools-label">{t("encoder.output")}</label>
            {output && (
              <span className="json-stats">
                {output.length} {t("encoder.chars")}
              </span>
            )}
          </div>
          <textarea
            className="devtools-textarea"
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
