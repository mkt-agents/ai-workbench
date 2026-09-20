import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  ClipboardCopy,
  Eraser,
  FileCheck,
  Key,
  Copy,
} from "lucide-react";
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
    <div className="devtools-tool hash-tool">
      {/* ── Toolbar ── */}
      <div className="hash-toolbar">
        <div className="hash-toolbar-group">
          <span className="hash-toolbar-label">{t("hash.algorithm")}</span>
          <select
            className="hash-select"
            value={algo}
            onChange={(e) => setAlgo(e.target.value as Algo)}
            disabled={multiAlgo}
          >
            {ALGOS.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
        </div>

        <label className="hash-checkbox">
          <input
            type="checkbox"
            checked={multiAlgo}
            onChange={(e) => setMultiAlgo(e.target.checked)}
          />
          <span>{t("hash.allAlgos")}</span>
        </label>

        <div className="hash-toolbar-divider" />

        <div className="hash-toolbar-group">
          <span className="hash-toolbar-label">{t("encoder.mode")}</span>
          <div className="hash-segmented">
            <button
              type="button"
              className={`hash-seg-item ${encoding === "hex" ? "active" : ""}`}
              onClick={() => setEncoding("hex")}
            >
              Hex
            </button>
            <button
              type="button"
              className={`hash-seg-item ${encoding === "base64" ? "active" : ""}`}
              onClick={() => setEncoding("base64")}
            >
              Base64
            </button>
          </div>
        </div>

        <div className="hash-toolbar-spacer" />

        <button
          type="button"
          className="hash-action-btn primary"
          onClick={() => void handleCopyAll()}
          disabled={!outputLength}
          title={t("encoder.copy")}
        >
          <Copy size={14} />
          <span>{t("encoder.copy")}</span>
        </button>
        <button
          type="button"
          className="hash-action-btn"
          onClick={() => setInput("")}
          disabled={!input}
          title={t("encoder.clear")}
        >
          <Eraser size={14} />
          <span>{t("encoder.clear")}</span>
        </button>
      </div>

      {/* ── Status ── */}
      {message && (
        <div className={`hash-status ${message.type}`}>
          {message.type === "success" ? <Check size={13} /> : <Key size={13} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── IO ── */}
      <div className="hash-io">
        <div className="hash-pane">
          <div className="hash-pane-header">
            <span className="hash-pane-title">{t("hash.input")}</span>
            {input && (
              <span className="hash-stats">
                {input.length} {t("encoder.chars")}
              </span>
            )}
          </div>
          <textarea
            className="hash-textarea"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={t("hash.sample")}
            rows={6}
            spellCheck={false}
          />

          {/* Hash comparison */}
          <div className="hash-compare">
            <div className="hash-compare-header">
              <FileCheck size={12} />
              <span>{t("hash.compare")}</span>
            </div>
            <input
              className="hash-input"
              value={compareHash}
              onChange={(e) => setCompareHash(e.target.value)}
              placeholder={t("hash.comparePlaceholder")}
              spellCheck={false}
            />
            {compareResult !== null && (
              <div className={`hash-compare-result ${compareResult ? "match" : "mismatch"}`}>
                {compareResult ? (
                  <>
                    <Check size={13} />
                    <span>{t("hash.match")}</span>
                  </>
                ) : (
                  <>
                    <Eraser size={13} />
                    <span>{t("hash.mismatch")}</span>
                  </>
                )}
              </div>
            )}
          </div>
        </div>

        <div className="hash-pane">
          <div className="hash-pane-header">
            <span className="hash-pane-title">{t("hash.output")}</span>
            {outputLength > 0 && (
              <span className="hash-stats">
                {outputLength} {t("encoder.chars")}
              </span>
            )}
          </div>

          {multiAlgo ? (
            <div className="hash-multi-list">
              {ALGOS.map((a) => (
                <div key={a} className="hash-multi-item">
                  <span className="hash-algo-name">{a}</span>
                  <code className="hash-algo-value">
                    {multiHashes[a] || <span className="hash-na">—</span>}
                  </code>
                  <button
                    type="button"
                    className="hash-icon-btn"
                    onClick={() => void handleCopy(multiHashes[a])}
                    disabled={!multiHashes[a]}
                    title={t("encoder.copy")}
                  >
                    <ClipboardCopy size={12} />
                  </button>
                </div>
              ))}
            </div>
          ) : (
            <div className="hash-single-output">
              <textarea
                className="hash-textarea output"
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
