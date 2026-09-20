import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Calendar,
  Check,
  ClipboardCopy,
  Clock,
  Copy,
  History,
  Timer,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";

type Unit = "seconds" | "milliseconds" | "microseconds";

function nowTs(): number {
  return Math.floor(Date.now() / 1000);
}

/** Human-readable relative time (e.g. "3 小时前" / "2 hours ago"). */
function relativeTime(msDiff: number, t: (k: string) => string): string {
  const future = msDiff < 0;
  const ms = Math.abs(msDiff);
  const sec = Math.floor(ms / 1000);
  const min = Math.floor(sec / 60);
  const hr = Math.floor(min / 60);
  const day = Math.floor(hr / 24);
  const mon = Math.floor(day / 30);
  const yr = Math.floor(day / 365);

  let text: string;
  if (sec < 5) return t("timestamp.justNow");
  if (sec < 60) text = `${sec}s`;
  else if (min < 60) text = `${min}m ${sec % 60}s`;
  else if (hr < 24) text = `${hr}h ${min % 60}m`;
  else if (day < 30) text = `${day}d ${hr % 24}h`;
  else if (mon < 12) text = `${mon}mo ${day % 30}d`;
  else text = `${yr}y ${mon % 12}mo`;

  return future ? `${text} ${t("timestamp.future")}` : `${text} ${t("timestamp.past")}`;
}

function TimestampTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [tsInput, setTsInput] = useState<string>(String(nowTs()));
  const [unit, setUnit] = useState<Unit>("seconds");
  const [dateInput, setDateInput] = useState<string>(() =>
    new Date().toISOString().slice(0, 19),
  );
  const [message, setMessage] = useState<{
    type: "success" | "error";
    text: string;
  } | null>(null);
  const [now, setNow] = useState(Date.now());

  // Live clock — updates every second
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  // Convert the user's timestamp input → Date
  const dateFromTs = useMemo(() => {
    const n = Number(tsInput);
    if (!Number.isFinite(n) || tsInput.trim() === "")
      return { ok: false as const, result: "", local: "", ms: 0, date: null as Date | null };
    let ms: number;
    if (unit === "microseconds") ms = n / 1000;
    else if (unit === "milliseconds") ms = n;
    else ms = n * 1000;
    const d = new Date(ms);
    if (isNaN(d.getTime())) return { ok: false as const, result: "", local: "", ms: 0, date: null };
    return { ok: true as const, result: d.toISOString(), local: d.toLocaleString(), ms, date: d };
  }, [tsInput, unit]);

  const tsFromDate = useMemo(() => {
    const d = new Date(dateInput);
    if (isNaN(d.getTime())) return { ok: false as const, seconds: "", milliseconds: "", microseconds: "" };
    const ms = d.getTime();
    return {
      ok: true as const,
      seconds: String(Math.floor(ms / 1000)),
      milliseconds: String(ms),
      microseconds: String(ms * 1000),
    };
  }, [dateInput]);

  const flash = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 2500);
  };

  const handleCopy = async (text: string) => {
    if (!text) return;
    try {
      await copy(text);
      flash("success", t("timestamp.copied"));
    } catch (e) {
      flash("error", String(e));
    }
  };

  const setToNow = () => {
    if (unit === "microseconds") setTsInput(String(Math.floor(Date.now() * 1000)));
    else if (unit === "milliseconds") setTsInput(String(Date.now()));
    else setTsInput(String(nowTs()));
    setDateInput(new Date().toISOString().slice(0, 19));
  };

  const currentNowValue = unit === "microseconds"
    ? String(Math.floor(now * 1000))
    : unit === "milliseconds"
      ? String(now)
      : String(Math.floor(now / 1000));

  return (
    <div className="devtools-tool ts-tool">
      {/* ── Live clock bar ── */}
      <div className="ts-live-clock">
        <div className="ts-live-left">
          <div className="ts-live-icon">
            <Timer size={14} />
          </div>
          <span className="ts-live-label">{t("timestamp.now")}</span>
          <code className="ts-live-value">{currentNowValue}</code>
        </div>
        <div className="ts-live-right">
          <button
            type="button"
            className="ts-now-btn"
            onClick={() => void handleCopy(currentNowValue)}
            title={t("timestamp.copy")}
          >
            <ClipboardCopy size={12} />
          </button>
          <div className="ts-segmented">
            {(["seconds", "milliseconds", "microseconds"] as Unit[]).map((u) => (
              <button
                key={u}
                type="button"
                className={`ts-seg-item ${unit === u ? "active" : ""}`}
                onClick={() => setUnit(u)}
              >
                {t(`timestamp.${u}`)}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* ── Status ── */}
      {message && (
        <div className={`ts-status ${message.type}`}>
          {message.type === "success" ? <Check size={13} /> : <Clock size={13} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── IO panes ── */}
      <div className="ts-io">
        {/* Timestamp → Date */}
        <div className="ts-pane">
          <div className="ts-pane-header">
            <div className="ts-pane-title">
              <Timer size={12} />
              {t("timestamp.timestamp")}
            </div>
            <button
              type="button"
              className="ts-now-btn"
              onClick={setToNow}
              title={t("timestamp.now")}
            >
              <Clock size={12} />
              {t("timestamp.now")}
            </button>
          </div>
          <input
            className="ts-input"
            value={tsInput}
            onChange={(e) => setTsInput(e.target.value)}
            placeholder="1700000000"
            spellCheck={false}
          />

          <div className="ts-output-grid">
            <div className="ts-output-row">
              <span className="ts-output-label">{t("timestamp.utc")}</span>
              <div className="ts-output-value-wrap">
                <code className="ts-output-value">
                  {dateFromTs.ok ? dateFromTs.result : t("timestamp.invalid")}
                </code>
                {dateFromTs.ok && (
                  <button
                    type="button"
                    className="ts-icon-btn"
                    onClick={() => void handleCopy(dateFromTs.result)}
                  >
                    <Copy size={11} />
                  </button>
                )}
              </div>
            </div>

            <div className="ts-output-row">
              <span className="ts-output-label">{t("timestamp.local")}</span>
              <div className="ts-output-value-wrap">
                <code className="ts-output-value">
                  {dateFromTs.ok ? dateFromTs.local : t("timestamp.invalid")}
                </code>
                {dateFromTs.ok && (
                  <button
                    type="button"
                    className="ts-icon-btn"
                    onClick={() => void handleCopy(dateFromTs.local)}
                  >
                    <Copy size={11} />
                  </button>
                )}
              </div>
            </div>

            {dateFromTs.ok && dateFromTs.date && (
              <div className="ts-output-row">
                <span className="ts-output-label">RFC 2822</span>
                <div className="ts-output-value-wrap">
                  <code className="ts-output-value">
                    {dateFromTs.date.toUTCString()}
                  </code>
                  <button
                    type="button"
                    className="ts-icon-btn"
                    onClick={() => void handleCopy(dateFromTs.date!.toUTCString())}
                  >
                    <Copy size={11} />
                  </button>
                </div>
              </div>
            )}

            <div className="ts-output-row">
              <span className="ts-output-label">
                <History size={11} />
                {t("timestamp.relative")}
              </span>
              <div className="ts-output-value-wrap">
                <code className="ts-output-value">
                  {dateFromTs.ok ? relativeTime(dateFromTs.ms - now, t) : "—"}
                </code>
              </div>
            </div>
          </div>

          {/* All units at a glance */}
          {dateFromTs.ok && (
            <div className="ts-all-units">
              <div className="ts-all-unit">
                <span>{t("timestamp.seconds")}</span>
                <code>
                  {unit === "seconds"
                    ? tsInput
                    : unit === "milliseconds"
                      ? String(Math.floor(Number(tsInput) / 1000))
                      : String(Math.floor(Number(tsInput) / 1e6))}
                </code>
              </div>
              <div className="ts-all-unit">
                <span>{t("timestamp.milliseconds")}</span>
                <code>
                  {unit === "milliseconds"
                    ? tsInput
                    : unit === "seconds"
                      ? String(Number(tsInput) * 1000)
                      : String(Math.floor(Number(tsInput) / 1000))}
                </code>
              </div>
              <div className="ts-all-unit">
                <span>{t("timestamp.microseconds")}</span>
                <code>
                  {unit === "microseconds"
                    ? tsInput
                    : unit === "milliseconds"
                      ? String(Number(tsInput) * 1000)
                      : String(Number(tsInput) * 1e6)}
                </code>
              </div>
            </div>
          )}
        </div>

        {/* Date → Timestamp */}
        <div className="ts-pane">
          <div className="ts-pane-header">
            <div className="ts-pane-title">
              <Calendar size={12} />
              {t("timestamp.date")}
            </div>
            <button
              type="button"
              className="ts-now-btn"
              onClick={setToNow}
            >
              <Clock size={12} />
              {t("timestamp.now")}
            </button>
          </div>
          <input
            className="ts-input"
            type="datetime-local"
            step="1"
            value={dateInput}
            onChange={(e) => setDateInput(e.target.value)}
          />

          <div className="ts-output-grid">
            <div className="ts-output-row">
              <span className="ts-output-label">{t("timestamp.seconds")}</span>
              <div className="ts-output-value-wrap">
                <code className="ts-output-value">
                  {tsFromDate.ok ? tsFromDate.seconds : t("timestamp.invalid")}
                </code>
                {tsFromDate.ok && (
                  <button
                    type="button"
                    className="ts-icon-btn"
                    onClick={() => void handleCopy(tsFromDate.seconds)}
                  >
                    <Copy size={11} />
                  </button>
                )}
              </div>
            </div>
            <div className="ts-output-row">
              <span className="ts-output-label">{t("timestamp.milliseconds")}</span>
              <div className="ts-output-value-wrap">
                <code className="ts-output-value">
                  {tsFromDate.ok ? tsFromDate.milliseconds : t("timestamp.invalid")}
                </code>
                {tsFromDate.ok && (
                  <button
                    type="button"
                    className="ts-icon-btn"
                    onClick={() => void handleCopy(tsFromDate.milliseconds)}
                  >
                    <Copy size={11} />
                  </button>
                )}
              </div>
            </div>
            <div className="ts-output-row">
              <span className="ts-output-label">{t("timestamp.microseconds")}</span>
              <div className="ts-output-value-wrap">
                <code className="ts-output-value">
                  {tsFromDate.ok ? tsFromDate.microseconds : t("timestamp.invalid")}
                </code>
                {tsFromDate.ok && (
                  <button
                    type="button"
                    className="ts-icon-btn"
                    onClick={() => void handleCopy(tsFromDate.microseconds)}
                  >
                    <Copy size={11} />
                  </button>
                )}
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default TimestampTool;
