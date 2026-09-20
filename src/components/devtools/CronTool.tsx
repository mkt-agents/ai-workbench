import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CalendarClock,
  Check,
  ChevronDown,
  ChevronUp,
  ClipboardCopy,
  Clock,
  Copy,
  HelpCircle,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";

/* ───────────────────────── cron parsing ───────────────────────── */

const RANGE = {
  second: { min: 0, max: 59 },
  minute: { min: 0, max: 59 },
  hour: { min: 0, max: 23 },
  dom: { min: 1, max: 31 },
  month: { min: 1, max: 12 },
  dow: { min: 0, max: 6 },
  year: { min: 1970, max: 2099 },
} as const;

type FieldType = "all" | "single" | "list" | "range" | "step" | "step-all";
type ParsedField = { type: FieldType; values: number[]; step?: number; start?: number; end?: number };
type ParsedCron = { fields: ParsedField[]; count: number; raw: string };

const ALL_NULL: ParsedField = { type: "all", values: [] };

function parseField(raw: string, min: number, max: number): { all: boolean; values: number[] } | null {
  if (raw === "*" || raw === "?") return { all: true, values: [] };
  const out = new Set<number>();
  for (const segment of raw.split(",")) {
    const [range, stepStr] = segment.split("/");
    const step = stepStr === undefined ? 1 : parseInt(stepStr, 10);
    if (!Number.isInteger(step) || step < 1) return null;
    let lo = min;
    let hi = max;
    if (range !== "*") {
      const dash = range.indexOf("-");
      if (dash === -1) {
        const v = parseInt(range, 10);
        if (!Number.isInteger(v)) return null;
        lo = hi = v;
      } else {
        const a = parseInt(range.slice(0, dash), 10);
        const b = parseInt(range.slice(dash + 1), 10);
        if (!Number.isInteger(a) || !Number.isInteger(b)) return null;
        lo = a;
        hi = b;
      }
    }
    if (lo < min || hi > max || lo > hi) return null;
    for (let v = lo; v <= hi; v += step) out.add(v);
  }
  return { all: false, values: [...out].sort((a, b) => a - b) };
}

function describeField(values: number[], raw: string, min: number): ParsedField {
  if (raw === "*" || raw === "?") return ALL_NULL;
  if (/^\*\/\d+$/.test(raw)) return { type: "step-all", values, step: Number(raw.slice(2)) };
  if (values.length === 1) return { type: "single", values };
  if (values.length === 2 && raw.includes("-")) return { type: "range", values, start: values[0], end: values[1] };
  if (raw.includes("/")) {
    const [range, stepStr] = raw.split("/");
    const lo = range === "*" ? min : Number(range.split("-")[0]);
    return { type: "step", values, step: Number(stepStr), start: lo };
  }
  return { type: "list", values };
}

function parseCron(expr: string): ParsedCron | null {
  const parts = expr.trim().split(/\s+/).filter(Boolean);
  if (parts.length < 5 || parts.length > 7) return null;
  while (parts.length < 7) {
    if (parts.length === 5) parts.unshift("0");
    else parts.push("*");
  }
  const raws = parts;
  const parsed = parts.map((p, i) => parseField(p, Object.values(RANGE)[i].min, Object.values(RANGE)[i].max));
  if (parsed.some((f) => f === null)) return null;
  const fields = parsed.map((f, i) =>
    f!.all ? ALL_NULL : describeField(f!.values, raws[i], Object.values(RANGE)[i].min)
  );
  return { fields, count: parts.length, raw: expr.trim() };
}

/* ───────────────────────── description ───────────────────────── */

const MONTH_ZH = ["1月","2月","3月","4月","5月","6月","7月","8月","9月","10月","11月","12月"];
const MONTH_EN = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
const DOW_ZH = ["周日","周一","周二","周三","周四","周五","周六"];
const DOW_EN = ["Sun","Mon","Tue","Wed","Thu","Fri","Sat"];

const pad2 = (n: number) => String(n).padStart(2, "0");

function describeCron(c: ParsedCron, t: (k: string) => string, lang: "zh" | "en"): string {
  const [second, minute, hour, dom, month, dow] = c.fields;
  const isAll = (f: ParsedField) => f.type === "all";
  const isSingle = (f: ParsedField) => f.type === "single";
  const isStepAll = (f: ParsedField) => f.type === "step-all";
  const MONTH_NAMES = lang === "en" ? MONTH_EN : MONTH_ZH;
  const DOW_NAMES = lang === "en" ? DOW_EN : DOW_ZH;

  if (c.fields.every(isAll)) return t("cron.everyMinute");

  const segments: string[] = [];

  // Time
  if (isSingle(minute) && isSingle(hour) && isAll(second)) {
    segments.push(`${pad2(hour.values[0])}:${pad2(minute.values[0])}`);
  } else if (isStepAll(minute) && isAll(hour)) {
    segments.push(t("cron.everyNMinutes").replace("{{n}}", String(minute.step)));
  } else {
    if (isSingle(minute) && isAll(hour)) {
      segments.push(t("cron.atMinute").replace("{{n}}", String(minute.values[0])));
    } else if (!isAll(minute)) {
      segments.push(`${t("cron.minute")} ${descValues(minute)}`);
    }
    if (!isAll(hour)) segments.push(`${t("cron.hour")} ${descValues(hour)}`);
  }

  // Date
  if (!isAll(dow)) {
    if (isSingle(dow)) segments.push(DOW_NAMES[dow.values[0]]);
    else if (dow.type === "range") segments.push(`${DOW_NAMES[dow.start!]}–${DOW_NAMES[dow.end!]}`);
    else segments.push(descValues(dow, "dow"));
  }
  if (!isAll(dom)) {
    if (isSingle(dom)) segments.push(t("cron.onDay").replace("{{n}}", String(dom.values[0])));
    else segments.push(`${t("cron.day")} ${descValues(dom)}`);
  }
  if (!isAll(month)) {
    if (isSingle(month)) segments.push(t("cron.inMonth").replace("{{n}}", MONTH_NAMES[month.values[0] - 1]));
    else segments.push(descValues(month, "month"));
  }

  return segments.join(" · ") || t("cron.everyMinute");
}

function descValues(f: ParsedField, kind?: "month" | "dow"): string {
  if (f.type === "step-all") return `*/${f.step}`;
  if (f.type === "step") return `${f.start}/${f.step}`;
  if (f.type === "range") {
    if (kind === "month") return `${MONTH_ZH[f.start! - 1]}–${MONTH_ZH[f.end! - 1]}`;
    if (kind === "dow") return `${DOW_ZH[f.start!]}–${DOW_ZH[f.end!]}`;
    return `${f.start}–${f.end}`;
  }
  return f.values.join(",");
}

/* ───────────────────────── next runs ───────────────────────── */

/** Find the smallest value in a sorted array that is strictly greater than `cur`, or -1 if none. */
function findNext(sorted: number[], cur: number): number {
  for (let i = 0; i < sorted.length; i++) {
    if (sorted[i] > cur) return sorted[i];
  }
  return -1;
}

function nextRuns(c: ParsedCron, from: Date, count: number): Date[] {
  const [, minuteF, hourF, domF, monthF, dowF] = c.fields;

  // Precompute Sets for O(1) lookup
  const minuteAll = minuteF.type === "all";
  const hourAll = hourF.type === "all";
  const domAll = domF.type === "all";
  const monthAll = monthF.type === "all";
  const dowAll = dowF.type === "all";

  const minuteSet = minuteAll ? null : new Set(minuteF.values);
  const hourSet = hourAll ? null : new Set(hourF.values);
  const domSet = domAll ? null : new Set(domF.values);
  const monthSet = monthAll ? null : new Set(monthF.values);
  const dowSet = dowAll ? null : new Set(dowF.values);

  const results: Date[] = [];
  const d = new Date(from);
  d.setSeconds(0, 0);
  d.setMinutes(d.getMinutes() + 1);

  // Cap at 2 years of scanning
  const maxMinutes = 366 * 24 * 60 * 2;
  for (let i = 0; i < maxMinutes && results.length < count; i++) {
    let m = d.getMinutes();
    if (!minuteAll && !minuteSet!.has(m)) {
      const next = findNext(minuteF.values, m);
      if (next === -1) {
        // No more valid minutes this hour → advance to next hour
        d.setHours(d.getHours() + 1);
        d.setMinutes(0);
      } else {
        d.setMinutes(next);
      }
      continue;
    }

    const h = d.getHours();
    if (!hourAll && !hourSet!.has(h)) {
      const next = findNext(hourF.values, h);
      if (next === -1) {
        d.setDate(d.getDate() + 1);
        d.setHours(0);
      } else {
        d.setHours(next);
      }
      d.setMinutes(0);
      continue;
    }

    const day = d.getDate();
    if (!domAll && !domSet!.has(day)) {
      const next = findNext(domF.values, day);
      if (next === -1 || next > daysInMonth(d.getMonth(), d.getFullYear())) {
        // Advance to next month
        d.setMonth(d.getMonth() + 1);
        d.setDate(1);
      } else {
        d.setDate(next);
      }
      d.setHours(0, 0);
      continue;
    }

    const mon = d.getMonth() + 1;
    if (!monthAll && !monthSet!.has(mon)) {
      const next = findNext(monthF.values, mon);
      if (next === -1) {
        d.setFullYear(d.getFullYear() + 1);
        d.setMonth(0);
      } else {
        d.setMonth(next - 1);
      }
      d.setDate(1);
      d.setHours(0, 0);
      continue;
    }

    const w = d.getDay();
    if (!dowAll && !dowSet!.has(w)) {
      const next = findNext(dowF.values, w);
      if (next === -1) {
        d.setDate(d.getDate() + (7 - w + dowF.values[0]));
      } else {
        d.setDate(d.getDate() + (next - w));
      }
      d.setHours(0, 0);
      continue;
    }

    // All fields match
    results.push(new Date(d));
    d.setMinutes(d.getMinutes() + 1);
  }

  return results;
}

function daysInMonth(month: number, year: number): number {
  return new Date(year, month + 1, 0).getDate();
}

function relativeTime(msDiff: number, t: (k: string) => string): { text: string; soon: boolean } {
  const future = msDiff < 0;
  const ms = Math.abs(msDiff);
  const sec = Math.floor(ms / 1000);
  const min = Math.floor(sec / 60);
  const hr = Math.floor(min / 60);
  const day = Math.floor(hr / 24);

  if (sec < 5) return { text: t("cron.justNow"), soon: true };
  let text: string;
  if (sec < 60) text = t("cron.nSeconds").replace("{{n}}", String(sec));
  else if (min < 60) text = t("cron.nMinutes").replace("{{n}}", String(min));
  else if (hr < 24) text = t("cron.nHours").replace("{{n}}", String(hr));
  else text = t("cron.nDays").replace("{{n}}", String(day));

  return { text: future ? t("cron.in").replace("{{t}}", text) : text, soon: min < 30 };
}

/* ───────────────────────── presets ───────────────────────── */

const PRESETS: { labelKey: string; expr: string }[] = [
  { labelKey: "cron.preset.everyMinute", expr: "* * * * *" },
  { labelKey: "cron.preset.every15Min", expr: "*/15 * * * *" },
  { labelKey: "cron.preset.hourly", expr: "0 * * * *" },
  { labelKey: "cron.preset.daily9am", expr: "0 9 * * *" },
  { labelKey: "cron.preset.weekdays9am", expr: "0 9 * * 1-5" },
  { labelKey: "cron.preset.weeklyMon", expr: "0 9 * * 1" },
  { labelKey: "cron.preset.monthly1st", expr: "0 0 1 * *" },
  { labelKey: "cron.preset.yearly", expr: "0 0 1 1 *" },
];

/* ───────────────────────── component ───────────────────────── */

export default function CronTool() {
  const { t, i18n } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);
  const lang = i18n.language.startsWith("en") ? "en" : "zh";

  const [expr, setExpr] = useState("0 9 * * 1-5");
  const [debouncedExpr, setDebouncedExpr] = useState(expr);
  const [nextCount, setNextCount] = useState(5);
  const [showHelp, setShowHelp] = useState(false);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);

  // Debounce expression input to avoid expensive recompute on every keystroke
  useEffect(() => {
    const id = setTimeout(() => setDebouncedExpr(expr), 250);
    return () => clearTimeout(id);
  }, [expr]);

  const parsed = useMemo(() => parseCron(debouncedExpr), [debouncedExpr]);
  const description = useMemo(() => (parsed ? describeCron(parsed, t, lang) : null), [parsed, t, lang]);
  const runs = useMemo(() => (parsed ? nextRuns(parsed, new Date(), nextCount) : []), [parsed, nextCount]);

  const flash = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 2200);
  };

  const handleCopy = async (text: string) => {
    if (!text) return;
    try {
      await copy(text);
      flash("success", t("cron.copied"));
    } catch (e) {
      flash("error", String(e));
    }
  };

  const copyAllRuns = async () => {
    const lines = runs.map((d, i) => `#${i + 1}  ${d.toLocaleString()}`);
    await handleCopy(lines.join("\n"));
  };

  return (
    <div className="cron-tool">
      {/* ── Header card ── */}
      <div className="cron-card cron-input-card">
        <div className="cron-card-head">
          <span className="cron-card-title">
            <Clock size={13} />
            {t("cron.expression")}
          </span>
          <button
            type="button"
            className={`cron-help-btn ${showHelp ? "active" : ""}`}
            onClick={() => setShowHelp((v) => !v)}
          >
            <HelpCircle size={12} />
            {t("cron.formatHelp")}
            {showHelp ? <ChevronUp size={11} /> : <ChevronDown size={11} />}
          </button>
        </div>

        <div className={`cron-input-row ${expr.trim() ? (parsed ? "is-valid" : "is-invalid") : ""}`}>
          <code className="cron-input-prefix">cron</code>
          <input
            className="cron-input"
            value={expr}
            onChange={(e) => setExpr(e.target.value)}
            placeholder={t("cron.placeholder")}
            spellCheck={false}
            autoComplete="off"
          />
          {expr.trim() && (
            <button
              type="button"
              className="cron-clear-input"
              onClick={() => setExpr("")}
              title={t("cron.clear")}
            >
              <XCircle size={14} />
            </button>
          )}
        </div>

        {!parsed && expr.trim() && (
          <p className="cron-error-hint">
            <XCircle size={12} />
            {t("cron.invalidHint")}
          </p>
        )}
      </div>

      {/* ── Format reference ── */}
      {showHelp && (
        <div className="cron-card cron-help">
          <div className="cron-help-row">
            <div className="cron-help-block">
              <div className="cron-block-label">{t("cron.help.fields")}</div>
              <div className="cron-field-visual">
                {(["秒","分","时","日","月","周","年"] as const).map((label, i) => {
                  const names = ["second","minute","hour","day","month","week","year"];
                  const ranges = ["0-59","0-59","0-23","1-31","1-12","0-6", t("cron.help.optional")];
                  const isOptional = i === 6;
                  return (
                    <div key={label} className={`cron-field ${isOptional ? "optional" : ""}`}>
                      <span className="cron-field-name">{names[i]}</span>
                      <span className="cron-field-range">{ranges[i]}</span>
                      <span className="cron-field-tag">{label}</span>
                    </div>
                  );
                })}
              </div>
            </div>

            <div className="cron-help-block">
              <div className="cron-block-label">{t("cron.help.symbols")}</div>
              <div className="cron-symbol-list">
                {[
                  { ch: "*", desc: t("cron.help.any"), ex: "* * * * *" },
                  { ch: ",", desc: t("cron.help.list"), ex: "1,3,5" },
                  { ch: "-", desc: t("cron.help.range"), ex: "1-5" },
                  { ch: "/", desc: t("cron.help.step"), ex: "*/15" },
                ].map((s) => (
                  <div key={s.ch} className="cron-symbol">
                    <span className="cron-symbol-ch">{s.ch}</span>
                    <span className="cron-symbol-desc">{s.desc}</span>
                    <code className="cron-symbol-ex">{s.ex}</code>
                  </div>
                ))}
              </div>
            </div>
          </div>

          <div className="cron-help-examples">
            <div className="cron-block-label">{t("cron.help.examples")}</div>
            <div className="cron-examples-grid">
              {[
                { expr: "* * * * *", key: "everyMinute" },
                { expr: "*/5 * * * *", key: "every5Min" },
                { expr: "0 * * * *", key: "hourly" },
                { expr: "0 0 * * *", key: "daily" },
                { expr: "0 9 * * 1-5", key: "weekdays9" },
                { expr: "0 0 1 * *", key: "monthly" },
                { expr: "0 0 1 1 *", key: "yearly" },
                { expr: "30 2 * * 0,6", key: "weekends" },
              ].map((ex) => (
                <button
                  key={ex.expr}
                  type="button"
                  className={`cron-example-chip ${expr === ex.expr ? "active" : ""}`}
                  onClick={() => setExpr(ex.expr)}
                >
                  <code>{ex.expr}</code>
                  <span>{t(`cron.help.ex.${ex.key}`)}</span>
                </button>
              ))}
            </div>
          </div>
        </div>
      )}

      {/* ── Presets ── */}
      <div className="cron-card cron-presets-card">
        <div className="cron-block-label">{t("cron.presets")}</div>
        <div className="cron-presets">
          {PRESETS.map((p) => (
            <button
              key={p.expr}
              type="button"
              className={`cron-preset-btn ${expr === p.expr ? "active" : ""}`}
              onClick={() => setExpr(p.expr)}
            >
              {t(p.labelKey)}
            </button>
          ))}
        </div>
      </div>

      {/* ── Result ── */}
      {parsed && description && (
        <div className="cron-card cron-result-card">
          {/* Description */}
          <div className="cron-desc">
            <div className="cron-desc-icon">
              <CalendarClock size={16} />
            </div>
            <div className="cron-desc-body">
              <div className="cron-desc-label">{t("cron.meaning")}</div>
              <div className="cron-desc-text">{description}</div>
            </div>
            <button
              type="button"
              className="cron-action-btn"
              onClick={() => handleCopy(expr)}
              title={t("cron.copyExpr")}
            >
              <Copy size={13} />
              <span>{t("cron.copyExpr")}</span>
            </button>
          </div>

          {/* Next runs */}
          <div className="cron-runs">
            <div className="cron-runs-head">
              <span className="cron-runs-title">
                <Clock size={12} />
                {t("cron.nextRuns")}
              </span>
              <div className="cron-runs-controls">
                <div className="cron-segmented">
                  {[3, 5, 10].map((n) => (
                    <button
                      key={n}
                      type="button"
                      className={`cron-seg-btn ${nextCount === n ? "active" : ""}`}
                      onClick={() => setNextCount(n)}
                    >
                      {n}
                    </button>
                  ))}
                </div>
                {runs.length > 0 && (
                  <button type="button" className="cron-action-btn primary" onClick={() => void copyAllRuns()} title={t("cron.copyAll")}>
                    <ClipboardCopy size={13} />
                    <span>{t("cron.copyAll")}</span>
                  </button>
                )}
              </div>
            </div>

            {runs.length === 0 ? (
              <div className="cron-no-runs">
                <HelpCircle size={18} />
                {t("cron.noRuns")}
              </div>
            ) : (
              <ul className="cron-runs-list">
                {runs.map((d, i) => {
                  const rel = relativeTime(Date.now() - d.getTime(), t);
                  return (
                    <li key={i} className={`cron-run-item ${i === 0 ? "next" : ""}`}>
                      <span className="cron-run-dot" />
                      <span className="cron-run-index">{i + 1}</span>
                      <span className="cron-run-date">{d.toLocaleString()}</span>
                      <span className={`cron-run-rel ${rel.soon ? "soon" : ""}`}>{rel.text}</span>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </div>
      )}

      {/* ── Toast ── */}
      {message && (
        <div className={`cron-toast ${message.type}`}>
          {message.type === "success" ? <Check size={13} /> : <XCircle size={13} />}
          <span>{message.text}</span>
        </div>
      )}
    </div>
  );
}
