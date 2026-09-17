import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Eraser, Shuffle } from "lucide-react";
import { useGlobalStore } from "../../core/store";

function UuidTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [count, setCount] = useState(5);
  const [hyphen, setHyphen] = useState(true);
  const [uppercase, setUppercase] = useState(false);
  const [items, setItems] = useState<string[]>([]);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  const generate = () => {
    const n = Math.max(1, Math.min(100, count || 1));
    const out: string[] = [];
    for (let i = 0; i < n; i++) {
      let u: string = crypto.randomUUID();
      if (!hyphen) u = u.replace(/-/g, "");
      if (uppercase) u = u.toUpperCase();
      out.push(u);
    }
    setItems(out);
  };

  const handleCopyAll = async () => {
    try {
      await copy(items.join("\n"));
      setMessage({ type: "success", text: t("uuid.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  return (
    <div className="devtools-tool">
      <div className="devtools-inline-controls">
        <label className="devtools-field">
          <span>{t("uuid.count")}</span>
          <input
            type="number"
            min={1}
            max={100}
            className="devtools-input devtools-input-sm"
            value={count}
            onChange={(e) => setCount(Number(e.target.value))}
          />
        </label>
        <label className="devtools-checkbox">
          <input type="checkbox" checked={hyphen} onChange={(e) => setHyphen(e.target.checked)} />
          <span>{t("uuid.hyphen")}</span>
        </label>
        <label className="devtools-checkbox">
          <input type="checkbox" checked={uppercase} onChange={(e) => setUppercase(e.target.checked)} />
          <span>{t("uuid.uppercase")}</span>
        </label>
        <div className="devtools-actions">
          <button type="button" className="btn btn-primary btn-small" onClick={generate}>
            <Shuffle size={14} />
            {t("uuid.generate")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={handleCopyAll}
            disabled={items.length === 0}
          >
            <ClipboardCopy size={14} />
            {t("uuid.copyAll")}
          </button>
          <button type="button" className="btn btn-secondary btn-small" onClick={() => setItems([])}>
            <Eraser size={14} />
            {t("uuid.clear")}
          </button>
        </div>
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Shuffle size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      {items.length > 0 && (
        <div className="devtools-uuid-list">
          {items.map((u, i) => (
            <div key={i} className="devtools-uuid-item">
              <code>{u}</code>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={async () => {
                  await copy(u);
                  setMessage({ type: "success", text: t("uuid.copied") });
                }}
              >
                <ClipboardCopy size={12} />
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export default UuidTool;
