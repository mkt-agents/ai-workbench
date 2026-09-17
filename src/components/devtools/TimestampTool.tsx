import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ClipboardCopy, Clock } from "lucide-react";
import { useGlobalStore } from "../../core/store";

function nowTs(): number {
  return Math.floor(Date.now() / 1000);
}

function TimestampTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [tsInput, setTsInput] = useState<string>(String(nowTs()));
  const [unit, setUnit] = useState<"seconds" | "milliseconds">("seconds");
  const [dateInput, setDateInput] = useState<string>(() => new Date().toISOString().slice(0, 19));
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  useEffect(() => {
    setTsInput(String(nowTs()));
  }, []);

  const dateFromTs = useMemo(() => {
    const n = Number(tsInput);
    if (!Number.isFinite(n)) return { ok: false as const, result: "" };
    // Auto-detect: > 1e12 → milliseconds
    const ms = n > 1e12 ? n : n * 1000;
    const d = new Date(ms);
    if (isNaN(d.getTime())) return { ok: false as const, result: "" };
    return {
      ok: true as const,
      result: d.toISOString(),
      local: d.toLocaleString(),
    };
  }, [tsInput]);

  const tsFromDate = useMemo(() => {
    const d = new Date(dateInput);
    if (isNaN(d.getTime())) return { ok: false as const, result: "" };
    const ms = d.getTime();
    return {
      ok: true as const,
      seconds: String(Math.floor(ms / 1000)),
      milliseconds: String(ms),
    };
  }, [dateInput]);

  const handleCopy = async (text: string) => {
    try {
      await copy(text);
      setMessage({ type: "success", text: t("timestamp.copied") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  return (
    <div className="devtools-tool">
      <div className="devtools-row">
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={() => {
            setTsInput(String(nowTs()));
            setDateInput(new Date().toISOString().slice(0, 19));
          }}
        >
          {t("timestamp.now")}
        </button>
      </div>

      {message && (
        <div className={`runtime-msg ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <Clock size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      <div className="devtools-io">
        <div className="devtools-io-pane">
          <label className="devtools-label">{t("timestamp.timestamp")}</label>
          <div className="devtools-row">
            <input
              className="devtools-input"
              value={tsInput}
              onChange={(e) => setTsInput(e.target.value)}
              placeholder="1700000000"
              spellCheck={false}
            />
            <div className="devtools-segmented">
              <button
                type="button"
                className={`segmented-item ${unit === "seconds" ? "active" : ""}`}
                onClick={() => setUnit("seconds")}
              >
                {t("timestamp.seconds")}
              </button>
              <button
                type="button"
                className={`segmented-item ${unit === "milliseconds" ? "active" : ""}`}
                onClick={() => setUnit("milliseconds")}
              >
                {t("timestamp.milliseconds")}
              </button>
            </div>
          </div>
          <div className="devtools-out-row">
            <div className="devtools-out-label">{t("timestamp.utc")}</div>
            <div className="devtools-out-value">
              {dateFromTs.ok ? dateFromTs.result : t("timestamp.invalid")}
            </div>
          </div>
          <div className="devtools-out-row">
            <div className="devtools-out-label">{t("timestamp.local")}</div>
            <div className="devtools-out-value">
              {dateFromTs.ok ? (dateFromTs as any).local : t("timestamp.invalid")}
            </div>
          </div>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => handleCopy(dateFromTs.ok ? dateFromTs.result : "")}
            disabled={!dateFromTs.ok}
          >
            <ClipboardCopy size={14} />
            {t("timestamp.copy")}
          </button>
        </div>

        <div className="devtools-io-pane">
          <label className="devtools-label">{t("timestamp.date")}</label>
          <input
            className="devtools-input"
            type="datetime-local"
            step="1"
            value={dateInput}
            onChange={(e) => setDateInput(e.target.value)}
          />
          <div className="devtools-out-row">
            <div className="devtools-out-label">{t("timestamp.seconds")}</div>
            <div className="devtools-out-value">{tsFromDate.ok ? tsFromDate.seconds : t("timestamp.invalid")}</div>
          </div>
          <div className="devtools-out-row">
            <div className="devtools-out-label">{t("timestamp.milliseconds")}</div>
            <div className="devtools-out-value">
              {tsFromDate.ok ? tsFromDate.milliseconds : t("timestamp.invalid")}
            </div>
          </div>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => handleCopy(tsFromDate.ok ? tsFromDate.seconds : "")}
            disabled={!tsFromDate.ok}
          >
            <ClipboardCopy size={14} />
            {t("timestamp.copy")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default TimestampTool;
