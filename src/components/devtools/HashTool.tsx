import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Key } from "lucide-react";
import { useGlobalStore } from "../../core/store";

type Algo = "SHA-1" | "SHA-256" | "SHA-384" | "SHA-512";

async function hashText(text: string, algo: Algo): Promise<string> {
  const data = new TextEncoder().encode(text);
  const buf = await crypto.subtle.digest(algo, data);
  return Array.from(new Uint8Array(buf))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function HashTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [algo, setAlgo] = useState<Algo>("SHA-256");
  const [input, setInput] = useState("");
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  const [hashed, setHashed] = useState("");

  useEffect(() => {
    if (!input) {
      setHashed("");
      return;
    }
    let active = true;
    hashText(input, algo)
      .then((h) => {
        if (active) setHashed(h);
      })
      .catch(() => {
        if (active) setHashed("");
    });
    return () => {
      active = false;
    };
  }, [input, algo]);

  const handleCopy = async () => {
    try {
      await copy(hashed);
      setMessage({ type: "success", text: t("hash.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  return (
    <div className="devtools-tool">
      <div className="devtools-inline-controls">
        <label className="devtools-field">
          <span>{t("hash.algorithm")}</span>
          <select
            className="devtools-select"
            value={algo}
            onChange={(e) => setAlgo(e.target.value as Algo)}
          >
            <option value="SHA-1">{t("hash.sha1")}</option>
            <option value="SHA-256">{t("hash.sha256")}</option>
            <option value="SHA-384">{t("hash.sha384")}</option>
            <option value="SHA-512">{t("hash.sha512")}</option>
          </select>
        </label>
        <div className="devtools-actions">
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={handleCopy}
            disabled={!hashed}
          >
            <ClipboardCopy size={14} />
            {t("hash.copy")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => setInput("")}
          >
            <Eraser size={14} />
            {t("hash.clear")}
          </button>
        </div>
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Key size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      <div className="devtools-io">
        <div className="devtools-io-pane">
          <label className="devtools-label">{t("hash.input")}</label>
          <textarea
            className="devtools-textarea"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={t("hash.sample")}
            rows={6}
            spellCheck={false}
          />
        </div>
        <div className="devtools-io-pane">
          <label className="devtools-label">{t("hash.output")}</label>
          <textarea
            className="devtools-textarea"
            value={hashed}
            readOnly
            placeholder=""
            rows={6}
            spellCheck={false}
          />
        </div>
      </div>
    </div>
  );
}

export default HashTool;
