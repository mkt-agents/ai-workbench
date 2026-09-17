import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Key, FileCheck } from "lucide-react";
import { useGlobalStore } from "../../core/store";

type Algo = "SHA-1" | "SHA-256" | "SHA-384" | "SHA-512";
type Encoding = "hex" | "base64";

const ALGOS: Algo[] = ["SHA-1", "SHA-256", "SHA-384", "SHA-512"];

async function hashText(text: string, algo: Algo): Promise<Uint8Array> {
  const data = new TextEncoder().encode(text);
  const buf = await crypto.subtle.digest(algo, data);
  return new Uint8Array(buf);
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary);
}

function HashTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [algo, setAlgo] = useState<Algo>("SHA-256");
  const [input, setInput] = useState("");
  const [encoding, setEncoding] = useState<Encoding>("hex");
  const [multiAlgo, setMultiAlgo] = useState(false);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [compareHash, setCompareHash] = useState("");
  const [compareResult, setCompareResult] = useState<boolean | null>(null);

  const [hashed, setHashed] = useState("");
  const [multiHashes, setMultiHashes] = useState<Record<Algo, string>>({
    "SHA-1": "",
    "SHA-256": "",
    "SHA-384": "",
    "SHA-512": "",
  });

  useEffect(() => {
    if (!input) {
      setHashed("");
      setMultiHashes({ "SHA-1": "", "SHA-256": "", "SHA-384": "", "SHA-512": "" });
      return;
    }
    let active = true;
    const algosToUse = multiAlgo ? ALGOS : [algo];

    Promise.all(algosToUse.map((a) => hashText(input, a))).then((results) => {
      if (!active) return;
      if (multiAlgo) {
        const map: Record<string, string> = {};
        algosToUse.forEach((a, i) => {
          map[a] = encoding === "hex" ? bytesToHex(results[i]) : bytesToBase64(results[i]);
        });
        setMultiHashes(map as Record<Algo, string>);
      } else {
        const bytes = results[0];
        setHashed(encoding === "hex" ? bytesToHex(bytes) : bytesToBase64(bytes));
      }
    });
    return () => {
      active = false;
    };
  }, [input, algo, encoding, multiAlgo]);

  // Compare hash
  useEffect(() => {
    if (!compareHash.trim()) {
      setCompareResult(null);
      return;
    }
    const currentHash = multiAlgo ? multiHashes[algo] : hashed;
    if (!currentHash) {
      setCompareResult(null);
      return;
    }
    // Case-insensitive comparison, trim whitespace
    setCompareResult(currentHash.toLowerCase() === compareHash.trim().toLowerCase());
  }, [compareHash, hashed, multiHashes, multiAlgo, algo]);

  const outputLength = useMemo(() => {
    if (multiAlgo) {
      const first = Object.values(multiHashes).find((v) => v);
      return first ? first.length : 0;
    }
    return hashed.length;
  }, [hashed, multiHashes, multiAlgo]);

  const handleCopy = async (text: string) => {
    try {
      await copy(text);
      setMessage({ type: "success", text: t("hash.copied") });
      setTimeout(() => setMessage(null), 2000);
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  const handleCopyAll = async () => {
    try {
      if (multiAlgo) {
        const lines = ALGOS.filter((a) => multiHashes[a]).map((a) => `${a}: ${multiHashes[a]}`);
        await copy(lines.join("\n"));
      } else {
        await copy(hashed);
      }
      setMessage({ type: "success", text: t("hash.copied") });
      setTimeout(() => setMessage(null), 2000);
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
            disabled={multiAlgo}
          >
            {ALGOS.map((a) => (
              <option key={a} value={a}>
                {t(`hash.${a.toLowerCase()}`)}
              </option>
            ))}
          </select>
        </label>
        <label className="devtools-checkbox">
          <input
            type="checkbox"
            checked={multiAlgo}
            onChange={(e) => setMultiAlgo(e.target.checked)}
          />
          <span>{t("hash.allAlgos")}</span>
        </label>
        <div className="devtools-segmented">
          <button
            type="button"
            className={`segmented-item ${encoding === "hex" ? "active" : ""}`}
            onClick={() => setEncoding("hex")}
          >
            Hex
          </button>
          <button
            type="button"
            className={`segmented-item ${encoding === "base64" ? "active" : ""}`}
            onClick={() => setEncoding("base64")}
          >
            Base64
          </button>
        </div>
        <div className="devtools-actions-spacer" />
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={handleCopyAll}
          disabled={!outputLength}
        >
          <ClipboardCopy size={14} />
          {t("hash.copy")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={() => setInput("")}>
          <Eraser size={14} />
          {t("hash.clear")}
        </button>
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Key size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      <div className="devtools-io">
        <div className="devtools-io-pane">
          <div className="io-header">
            <label className="devtools-label">{t("hash.input")}</label>
            {input && <span className="json-stats">{input.length} chars</span>}
          </div>
          <textarea
            className="devtools-textarea"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={t("hash.sample")}
            rows={6}
            spellCheck={false}
          />

          {/* Hash comparison */}
          <div className="hash-compare">
            <label className="devtools-label">{t("hash.compare")}</label>
            <input
              className="devtools-input"
              value={compareHash}
              onChange={(e) => setCompareHash(e.target.value)}
              placeholder={t("hash.comparePlaceholder")}
              spellCheck={false}
            />
            {compareResult !== null && (
              <div className={`hash-compare-result ${compareResult ? "match" : "mismatch"}`}>
                {compareResult ? (
                  <>
                    <FileCheck size={14} />
                    <span>{t("hash.match")}</span>
                  </>
                ) : (
                  <>
                    <Key size={14} />
                    <span>{t("hash.mismatch")}</span>
                  </>
                )}
              </div>
            )}
          </div>
        </div>

        <div className="devtools-io-pane">
          <div className="io-header">
            <label className="devtools-label">
              {t("hash.output")}
              {outputLength > 0 && <span className="json-stats">{outputLength} chars</span>}
            </label>
          </div>

          {multiAlgo ? (
            <div className="hash-multi-list">
              {ALGOS.map((a) => (
                <div key={a} className="hash-multi-item">
                  <span className="hash-algo-name">{a}</span>
                  <code className="hash-algo-value">
                    {multiHashes[a] || <span className="devports-na">—</span>}
                  </code>
                  <button
                    type="button"
                    className="btn btn-secondary btn-small icon-only"
                    onClick={() => handleCopy(multiHashes[a])}
                    disabled={!multiHashes[a]}
                  >
                    <ClipboardCopy size={11} />
                  </button>
                </div>
              ))}
            </div>
          ) : (
            <div className="hash-single-output">
              <textarea
                className="devtools-textarea"
                value={hashed}
                readOnly
                placeholder=""
                rows={6}
                spellCheck={false}
              />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export default HashTool;
