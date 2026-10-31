import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { TrendingUp } from "lucide-react";

type Point = { date: string; count: number };

type Props = {
  days: Point[];
  todayKey: string;
  onDayClick?: (date: string) => void;
};

/** Catmull-Rom → cubic Bézier（与设置页用量走势一致）。 */
function smoothPath(pts: { x: number; y: number }[]): string {
  if (pts.length < 2) return "";
  if (pts.length < 3) {
    return `M ${pts[0].x.toFixed(2)} ${pts[0].y.toFixed(2)} L ${pts
      .slice(1)
      .map((p) => `${p.x.toFixed(2)} ${p.y.toFixed(2)}`)
      .join(" L ")}`;
  }
  let d = `M ${pts[0].x.toFixed(2)} ${pts[0].y.toFixed(2)}`;
  const clampY = (y: number) => Math.min(100, Math.max(0, y));
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] ?? pts[i];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    const c1x = p1.x + (p2.x - p0.x) / 6;
    const c1y = clampY(p1.y + (p2.y - p0.y) / 6);
    const c2x = p2.x - (p3.x - p1.x) / 6;
    const c2y = clampY(p2.y - (p3.y - p1.y) / 6);
    d += ` C ${c1x.toFixed(2)} ${c1y.toFixed(2)}, ${c2x.toFixed(2)} ${c2y.toFixed(2)}, ${p2.x.toFixed(
      2
    )} ${p2.y.toFixed(2)}`;
  }
  return d;
}

export default function WorkTodoTrend({ days, todayKey, onDayClick }: Props) {
  const { t } = useTranslation("workTodo");
  const max = Math.max(...days.map((d) => d.count), 1);
  const peak = Math.max(...days.map((d) => d.count), 0);
  const weekSum = useMemo(
    () => days.slice(-7).reduce((n, d) => n + d.count, 0),
    [days]
  );
  const [hoverDate, setHoverDate] = useState<string | null>(null);

  const pts = useMemo(
    () =>
      days.map((d, i) => ({
        date: d.date,
        count: d.count,
        x: ((i + 0.5) / days.length) * 100,
        y: 100 - Math.min((d.count / max) * 100, 100),
        isToday: d.date === todayKey,
      })),
    [days, max, todayKey]
  );

  const linePath = useMemo(() => smoothPath(pts.map((p) => ({ x: p.x, y: p.y }))), [pts]);
  const hoverPoint = hoverDate ? pts.find((p) => p.date === hoverDate) : null;

  return (
    <div className="work-todo-trend card">
      <div className="work-todo-trend-head">
        <div className="work-todo-trend-head-main">
          <h3 className="work-todo-section-title">
            <TrendingUp size={14} className="work-todo-dash-section-icon" aria-hidden />
            {t("dashTrendTitle")}
          </h3>
          <span className="work-todo-trend-sub">{t("dashTrendWeekSum", { count: weekSum })}</span>
        </div>
        <div className="work-todo-trend-head-meta">
          {hoverPoint ? (
            <span className="work-todo-trend-hover">
              {hoverPoint.date.slice(5)} · {t("dashTrendDayCount", { count: hoverPoint.count })}
            </span>
          ) : peak > 0 ? (
            <span className="work-todo-trend-peak">{t("dashTrendPeak", { count: peak })}</span>
          ) : (
            <span className="work-todo-trend-peak">{t("dashTrendHint")}</span>
          )}
        </div>
      </div>
      <div className="work-todo-trend-plot" role="img" aria-label={t("dashTrendTitle")}>
        {days.length >= 2 ? (
          <svg
            className="work-todo-trend-line is-area"
            viewBox="0 0 100 100"
            preserveAspectRatio="none"
            aria-hidden
          >
            <path className="work-todo-trend-fill" d={`${linePath} L 100 100 L 0 100 Z`} />
            <path className="work-todo-trend-stroke" d={linePath} />
            {pts.map((p) => (
              <circle
                key={p.date}
                className={`work-todo-trend-dot${p.isToday ? " is-today" : ""}${
                  p.count === 0 ? " is-empty" : ""
                }${hoverDate === p.date ? " is-hover" : ""}`}
                cx={p.x}
                cy={p.y}
                r={p.isToday ? 2.2 : 1.6}
              />
            ))}
            {days.map((d, i) => (
              <rect
                key={d.date}
                className="work-todo-trend-hit"
                x={(i / days.length) * 100}
                y="0"
                width={100 / days.length}
                height="100"
                fill="transparent"
                onMouseEnter={() => setHoverDate(d.date)}
                onMouseLeave={() => setHoverDate(null)}
                onClick={() => onDayClick?.(d.date)}
              />
            ))}
          </svg>
        ) : (
          <div className="work-todo-trend-fallback" aria-hidden />
        )}
      </div>
      <div className="work-todo-trend-days">
        {days.map((d) => (
          <button
            key={d.date}
            type="button"
            className={`work-todo-trend-day${d.date === todayKey ? " is-today" : ""}${
              hoverDate === d.date ? " is-hover" : ""
            }`}
            onMouseEnter={() => setHoverDate(d.date)}
            onMouseLeave={() => setHoverDate(null)}
            onClick={() => onDayClick?.(d.date)}
          >
            {d.date.slice(8)}
          </button>
        ))}
      </div>
    </div>
  );
}
