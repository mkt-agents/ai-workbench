import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown, ChevronLeft, ChevronRight } from "lucide-react";
import {
  formatDayLabel,
  formatMonthTitle,
  monthGrid,
  parseDateKey,
  shiftDateKey,
  weekdayShortLabels,
} from "./utils";

export type CalMonth = { y: number; m: number };

function shiftCalMonth(c: CalMonth, delta: number): CalMonth {
  const d = new Date(c.y, c.m + delta, 1);
  return { y: d.getFullYear(), m: d.getMonth() };
}

type CalMonthHeadProps = {
  calMonth: CalMonth;
  setCalMonth: (next: CalMonth | ((prev: CalMonth) => CalMonth)) => void;
  locale: string;
  anchorYear?: number;
};

export function WorkTodoCalMonthHead({
  calMonth,
  setCalMonth,
  locale,
  anchorYear,
}: CalMonthHeadProps) {
  const { t } = useTranslation("workTodo");
  const [pickOpen, setPickOpen] = useState(false);
  const [yearOpen, setYearOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const yearPickerRef = useRef<HTMLDivElement>(null);

  const monthLabels = useMemo(
    () =>
      Array.from({ length: 12 }, (_, i) =>
        new Date(2000, i, 1).toLocaleDateString(locale, { month: "short" })
      ),
    [locale]
  );

  const years = useMemo(() => {
    const center = anchorYear ?? calMonth.y;
    const from = center - 15;
    const to = center + 5;
    const list: number[] = [];
    for (let y = from; y <= to; y++) list.push(y);
    return list;
  }, [anchorYear, calMonth.y]);

  useEffect(() => {
    if (!pickOpen) setYearOpen(false);
  }, [pickOpen]);

  useEffect(() => {
    if (!pickOpen && !yearOpen) return;
    const onDoc = (e: MouseEvent) => {
      const node = e.target as Node;
      if (wrapRef.current && !wrapRef.current.contains(node)) {
        setPickOpen(false);
        setYearOpen(false);
        return;
      }
      if (
        yearOpen &&
        yearPickerRef.current &&
        !yearPickerRef.current.contains(node)
      ) {
        setYearOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (yearOpen) setYearOpen(false);
        else setPickOpen(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
    };
  }, [pickOpen, yearOpen]);

  return (
    <div className="work-todo-cal-head-wrap" ref={wrapRef}>
      <div className="work-todo-cal-head">
        <button
          type="button"
          className="work-todo-nav-btn"
          title={t("prevMonth")}
          onClick={() => {
            setPickOpen(false);
            setYearOpen(false);
            setCalMonth((c) => shiftCalMonth(c, -1));
          }}
        >
          <ChevronLeft size={14} />
        </button>
        <button
          type="button"
          className={`work-todo-cal-title-btn${pickOpen ? " is-open" : ""}`}
          title={t("pickYearMonth")}
          aria-expanded={pickOpen}
          aria-haspopup="dialog"
          onClick={() => setPickOpen((v) => !v)}
        >
          <span className="work-todo-cal-title">
            {formatMonthTitle(calMonth.y, calMonth.m, locale)}
          </span>
          <ChevronDown size={12} className="work-todo-cal-title-chevron" aria-hidden />
        </button>
        <button
          type="button"
          className="work-todo-nav-btn"
          title={t("nextMonth")}
          onClick={() => {
            setPickOpen(false);
            setYearOpen(false);
            setCalMonth((c) => shiftCalMonth(c, 1));
          }}
        >
          <ChevronRight size={14} />
        </button>
      </div>
      {pickOpen && (
        <div className="work-todo-cal-ym-panel" role="dialog" aria-label={t("pickYearMonth")}>
          <div className="work-todo-cal-year-row">
            <span className="work-todo-cal-year-label">{t("pickYear")}</span>
            <div className="work-todo-cal-year-picker" ref={yearPickerRef}>
              <button
                type="button"
                className={`work-todo-cal-year-trigger${yearOpen ? " is-open" : ""}`}
                aria-expanded={yearOpen}
                aria-haspopup="listbox"
                onClick={() => setYearOpen((v) => !v)}
              >
                <span className="work-todo-cal-year-value">{calMonth.y}</span>
                <ChevronDown size={12} className="work-todo-cal-year-chevron" aria-hidden />
              </button>
              {yearOpen && (
                <ul
                  className="work-todo-cal-year-list work-todo-scroll"
                  role="listbox"
                  aria-label={t("pickYear")}
                >
                  {years.map((y) => (
                    <li key={y}>
                      <button
                        type="button"
                        role="option"
                        aria-selected={y === calMonth.y}
                        className={`work-todo-cal-year-option${
                          y === calMonth.y ? " is-active" : ""
                        }`}
                        onClick={() => {
                          setCalMonth({ y, m: calMonth.m });
                          setYearOpen(false);
                        }}
                      >
                        {y}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </div>
          <div className="work-todo-cal-months" role="listbox" aria-label={t("pickMonth")}>
            {monthLabels.map((label, i) => (
              <button
                key={label}
                type="button"
                role="option"
                aria-selected={i === calMonth.m}
                className={`work-todo-cal-month-chip${i === calMonth.m ? " is-active" : ""}`}
                onClick={() => {
                  setCalMonth({ y: calMonth.y, m: i });
                  setPickOpen(false);
                }}
              >
                {label}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

type Props = {
  todayKey: string;
  viewDate: string;
  onViewDate: (key: string) => void;
  activeDates: Set<string>;
  locale: string;
  /** Extra stats next to the week strip (progress chips). */
  stats?: React.ReactNode;
};

export default function WorkTodoDateChrome({
  todayKey,
  viewDate,
  onViewDate,
  activeDates,
  locale,
  stats,
}: Props) {
  const { t } = useTranslation("workTodo");
  const [calOpen, setCalOpen] = useState(false);
  const parsed = parseDateKey(viewDate) ?? new Date();
  const [calMonth, setCalMonth] = useState({ y: parsed.getFullYear(), m: parsed.getMonth() });
  const dateNavRef = useRef<HTMLDivElement>(null);

  const weekStrip = useMemo(
    () => Array.from({ length: 7 }, (_, i) => shiftDateKey(todayKey, i - 3)),
    [todayKey]
  );
  const weekLabels = useMemo(() => weekdayShortLabels(locale), [locale]);
  const calCells = useMemo(() => monthGrid(calMonth.y, calMonth.m), [calMonth.y, calMonth.m]);

  useEffect(() => {
    const d = parseDateKey(viewDate);
    if (!d) return;
    setCalMonth({ y: d.getFullYear(), m: d.getMonth() });
  }, [viewDate]);

  useEffect(() => {
    if (!calOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (dateNavRef.current && !dateNavRef.current.contains(e.target as Node)) {
        setCalOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setCalOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
    };
  }, [calOpen]);

  return (
    <div className="work-todo-date-nav" ref={dateNavRef} role="group" aria-label={t("pickDate")}>
      <button
        type="button"
        className="work-todo-nav-btn"
        title={t("prevDay")}
        onClick={() => {
          setCalOpen(false);
          onViewDate(shiftDateKey(viewDate, -1));
        }}
      >
        <ChevronLeft size={14} />
      </button>
      <div className="work-todo-date-picker-wrap">
        <button
          type="button"
          className={`work-todo-date-mid${calOpen ? " is-open" : ""}`}
          title={t("pickDate")}
          aria-expanded={calOpen}
          aria-haspopup="dialog"
          onClick={() => setCalOpen((v) => !v)}
        >
          <strong className="work-todo-date-label">{formatDayLabel(viewDate, todayKey, t)}</strong>
          <span className="work-todo-date-iso">
            {viewDate}
            <ChevronDown size={11} className="work-todo-date-chevron" />
          </span>
        </button>
        {calOpen && (
          <div className="work-todo-cal" role="dialog" aria-label={t("pickDate")}>
            <WorkTodoCalMonthHead
              calMonth={calMonth}
              setCalMonth={setCalMonth}
              locale={locale}
              anchorYear={parsed.getFullYear()}
            />
            <div className="work-todo-cal-weekdays">
              {weekLabels.map((label, i) => (
                <span key={i}>{label}</span>
              ))}
            </div>
            <div className="work-todo-cal-grid">
              {calCells.map((key, i) =>
                key ? (
                  <button
                    key={key}
                    type="button"
                    className={`work-todo-cal-day${key === viewDate ? " is-selected" : ""}${
                      key === todayKey ? " is-today" : ""
                    }${activeDates.has(key) ? " has-mark" : ""}`}
                    onClick={() => {
                      onViewDate(key);
                      setCalOpen(false);
                    }}
                  >
                    <span>{Number(key.slice(8))}</span>
                    {activeDates.has(key) && <i className="work-todo-cal-dot" aria-hidden />}
                  </button>
                ) : (
                  <span key={`pad-${i}`} className="work-todo-cal-pad" />
                )
              )}
            </div>
            <div className="work-todo-cal-foot">
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => {
                  onViewDate(todayKey);
                  setCalOpen(false);
                }}
              >
                {t("goToday")}
              </button>
            </div>
          </div>
        )}
      </div>
      <button
        type="button"
        className="work-todo-nav-btn"
        title={t("nextDay")}
        onClick={() => {
          setCalOpen(false);
          onViewDate(shiftDateKey(viewDate, 1));
        }}
      >
        <ChevronRight size={14} />
      </button>
      {viewDate !== todayKey && (
        <button
          type="button"
          className="btn btn-secondary btn-small work-todo-today-btn"
          onClick={() => {
            setCalOpen(false);
            onViewDate(todayKey);
          }}
        >
          {t("goToday")}
        </button>
      )}
      <div className="work-todo-week-strip" role="list" aria-label={t("weekStrip")}>
        {weekStrip.map((key) => {
          const d = parseDateKey(key);
          const dayNum = d ? d.getDate() : key.slice(8);
          const wd = d ? d.toLocaleDateString(locale, { weekday: "narrow" }) : "";
          return (
            <button
              key={key}
              type="button"
              role="listitem"
              className={`work-todo-week-chip${key === viewDate ? " is-selected" : ""}${
                key === todayKey ? " is-today" : ""
              }${activeDates.has(key) ? " has-mark" : ""}`}
              title={key}
              onClick={() => {
                setCalOpen(false);
                onViewDate(key);
              }}
            >
              <span className="work-todo-week-wd">{wd}</span>
              <span className="work-todo-week-num">{dayNum}</span>
              {activeDates.has(key) && <i className="work-todo-cal-dot" aria-hidden />}
            </button>
          );
        })}
      </div>
      {stats}
    </div>
  );
}
