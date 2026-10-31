import { useEffect, useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  CalendarDays,
  CheckCircle2,
  ChevronRight,
  Columns3,
  Inbox,
  LayoutList,
  Pin,
  Plus,
  Flame,
  Target,
} from "lucide-react";
import type { WorkTask } from "../../core/types";
import { WorkTodoCalMonthHead } from "./WorkTodoDateChrome";
import WorkTodoTrend from "./WorkTodoTrend";
import { WorkTodoDoneToggle } from "./WorkTodoTaskRow";
import {
  completedCountByDate,
  completedInRange,
  completionStreak,
  countsForDashboard,
  openCountByDate,
  recentDone,
  sumCompletedInRange,
  todayOpenPreview,
} from "./selectors";
import {
  monthGrid,
  localDateKeyFromIso,
  parseDateKey,
  weekdayShortLabels,
} from "./utils";

export type DashNavigate =
  | {
      kind: "daily";
      date?: string;
      highlightOverdue?: boolean;
      showDone?: boolean;
      focusQuickAdd?: boolean;
    }
  | { kind: "kanban" };

type Props = {
  todayKey: string;
  locale: string;
  workTasks: WorkTask[];
  onNavigate: (target: DashNavigate) => void;
  onToggleDone: (task: WorkTask) => void;
};

function DashProgressRing({ pct }: { pct: number }) {
  const r = 26;
  const c = 2 * Math.PI * r;
  const clamped = Math.min(100, Math.max(0, pct));
  const dash = (clamped / 100) * c;
  return (
    <div className="work-todo-dash-ring" aria-hidden>
      <svg viewBox="0 0 64 64">
        <circle className="work-todo-dash-ring-track" cx="32" cy="32" r={r} fill="none" />
        <circle
          className="work-todo-dash-ring-fill"
          cx="32"
          cy="32"
          r={r}
          fill="none"
          strokeDasharray={`${dash} ${c}`}
          transform="rotate(-90 32 32)"
        />
      </svg>
      <span className="work-todo-dash-ring-pct">{clamped}%</span>
    </div>
  );
}

function DashMetric({
  icon,
  tone,
  num,
  label,
  hint,
  warn,
  onClick,
}: {
  icon: ReactNode;
  tone: "accent" | "green" | "neutral" | "warn";
  num: ReactNode;
  label: string;
  hint?: string;
  warn?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={`work-todo-metric is-${tone}${warn ? " is-warn" : ""}`}
      onClick={onClick}
    >
      <span className="work-todo-metric-icon">{icon}</span>
      <span className="work-todo-metric-body">
        <span className="work-todo-metric-num">{num}</span>
        <span className="work-todo-metric-label">{label}</span>
        {hint ? <span className="work-todo-metric-hint">{hint}</span> : null}
      </span>
      <ChevronRight size={14} className="work-todo-metric-chevron" aria-hidden />
    </button>
  );
}

export default function DashboardView({
  todayKey,
  locale,
  workTasks,
  onNavigate,
  onToggleDone,
}: Props) {
  const { t } = useTranslation("workTodo");
  const counts = useMemo(() => countsForDashboard(workTasks, todayKey), [workTasks, todayKey]);
  const trend = useMemo(() => completedInRange(workTasks, todayKey, 14), [workTasks, todayKey]);
  const weekDone = useMemo(() => sumCompletedInRange(workTasks, todayKey, 7), [workTasks, todayKey]);
  const streak = useMemo(() => completionStreak(workTasks, todayKey), [workTasks, todayKey]);
  const focusTasks = useMemo(
    () => todayOpenPreview(workTasks, todayKey),
    [workTasks, todayKey]
  );
  const focusScroll = focusTasks.length > 5;
  const doneList = useMemo(() => recentDone(workTasks, 30), [workTasks]);
  const openByDate = useMemo(() => openCountByDate(workTasks), [workTasks]);
  const doneByDate = useMemo(() => completedCountByDate(workTasks), [workTasks]);

  const todayParsed = useMemo(() => {
    const [y, m] = todayKey.split("-").map(Number);
    return { y, m: m - 1 };
  }, [todayKey]);
  const [calMonth, setCalMonth] = useState(todayParsed);
  useEffect(() => {
    setCalMonth(todayParsed);
  }, [todayKey, todayParsed]);

  const weekLabels = useMemo(() => weekdayShortLabels(locale), [locale]);
  const cells = useMemo(() => monthGrid(calMonth.y, calMonth.m), [calMonth.y, calMonth.m]);

  const todayPct =
    counts.todayTotal > 0
      ? Math.round((counts.todayDone / counts.todayTotal) * 100)
      : counts.todayDone > 0
        ? 100
        : 0;

  const dateSubtitle = useMemo(() => {
    const d = parseDateKey(todayKey);
    return d
      ? d.toLocaleDateString(locale, {
          month: "short",
          day: "numeric",
          weekday: "short",
        })
      : todayKey;
  }, [todayKey, locale]);

  const jumpTodayCal = () => {
    setCalMonth(todayParsed);
    onNavigate({ kind: "daily", date: todayKey });
  };

  /** 悬停提示：标题被截断时也能看到全文，附带备注摘要与标签。 */
  const focusTip = (task: WorkTask) => {
    const parts: string[] = [task.title.trim() || t("untitled")];
    const notes = (task.notes || "").trim();
    if (notes) parts.push(notes.length > 200 ? `${notes.slice(0, 200)}…` : notes);
    const tags = (task.tags || "").trim();
    if (tags) parts.push(`#${tags.replace(/\s*[,，]\s*/g, " #")}`);
    return parts.join("\n");
  };

  return (
    <div className="work-todo-dash">
      <div className="work-todo-dash-body work-todo-scroll">
        <header className="card work-todo-dash-hero">
          <div className="work-todo-dash-hero-main">
            <DashProgressRing pct={todayPct} />
            <div className="work-todo-dash-hero-text">
              <div className="work-todo-dash-hero-top">
                <h2 className="work-todo-dash-hero-title">{t("dashOverview")}</h2>
                <span className="work-todo-dash-hero-date">{dateSubtitle}</span>
              </div>
              <p className="work-todo-dash-hero-stats">
                {t("dashHeroToday", {
                  done: counts.todayDone,
                  total: counts.todayTotal,
                })}
                <span className="work-todo-dash-hero-sep">·</span>
                {t("dashHeroWeek", { count: weekDone })}
                {streak >= 2 && (
                  <>
                    <span className="work-todo-dash-hero-sep">·</span>
                    <span className="work-todo-dash-streak">
                      <Flame size={12} />
                      {t("dashStreak", { count: streak })}
                    </span>
                  </>
                )}
              </p>
            </div>
          </div>
          <div className="work-todo-dash-quick" role="group" aria-label={t("dashQuickActions")}>
            <button
              type="button"
              className="work-todo-dash-quick-btn is-primary"
              onClick={() =>
                onNavigate({ kind: "daily", date: todayKey, focusQuickAdd: true })
              }
            >
              <Plus size={14} />
              {t("dashQuickAdd")}
            </button>
            <button
              type="button"
              className="work-todo-dash-quick-btn"
              onClick={() => onNavigate({ kind: "kanban" })}
            >
              <Columns3 size={14} />
              {t("dashQuickKanban")}
            </button>
          </div>
        </header>

        <div className="work-todo-dash-metrics">
          <DashMetric
            tone="accent"
            icon={<LayoutList size={16} />}
            num={counts.openTotal}
            label={t("dashOpenTotal")}
            hint={t("dashOpenHint")}
            onClick={() => onNavigate({ kind: "kanban" })}
          />
          <DashMetric
            tone="green"
            icon={<Target size={16} />}
            num={
              <>
                {counts.todayDone}
                <span className="work-todo-metric-slash">/</span>
                {counts.todayTotal}
              </>
            }
            label={t("dashTodayProgress")}
            hint={t("dashTodayHint", { open: counts.todayOpen })}
            onClick={() => onNavigate({ kind: "daily", date: todayKey })}
          />
          <DashMetric
            tone="neutral"
            icon={<Inbox size={16} />}
            num={counts.backlog}
            label={t("dashBacklog")}
            hint={t("dashBacklogHint")}
            onClick={() => onNavigate({ kind: "daily", date: todayKey })}
          />
          <DashMetric
            tone="warn"
            icon={<AlertTriangle size={16} />}
            num={counts.overdue}
            label={t("dashOverdue")}
            hint={counts.overdue > 0 ? t("dashOverdueHint") : t("dashOverdueClear")}
            warn={counts.overdue > 0}
            onClick={() =>
              onNavigate({ kind: "daily", date: todayKey, highlightOverdue: true })
            }
          />
        </div>

        <div className="work-todo-dash-main">
          <WorkTodoTrend
            days={trend}
            todayKey={todayKey}
            onDayClick={(date) => onNavigate({ kind: "daily", date })}
          />

          <section
              className={`card work-todo-dash-focus${focusScroll ? " is-scrollable" : ""}`}
            >
              <div className="work-todo-dash-focus-head">
                <h3 className="work-todo-section-title">
                  <Target size={14} className="work-todo-dash-section-icon" aria-hidden />
                  {t("dashFocus")}
                </h3>
                {counts.todayOpen > 0 && (
                  <span className="work-todo-count-badge">{counts.todayOpen}</span>
                )}
                <button
                  type="button"
                  className="work-todo-dash-focus-all"
                  onClick={() => onNavigate({ kind: "daily", date: todayKey })}
                >
                  {t("dashSeeAll")}
                </button>
              </div>
              <div className="work-todo-dash-focus-body work-todo-scroll">
                {focusTasks.length === 0 ? (
                  <p className="work-todo-dash-focus-empty">{t("dashFocusEmpty")}</p>
                ) : (
                  <ul className="work-todo-dash-focus-list">
                    {focusTasks.map((task) => (
                      <li key={task.id}>
                        <div className="work-todo-dash-focus-row" title={focusTip(task)}>
                          <WorkTodoDoneToggle
                            done={false}
                            title={task.title}
                            onToggle={() => onToggleDone(task)}
                            stopPropagation
                          />
                          <button
                            type="button"
                            className="work-todo-dash-focus-title"
                            onClick={() => onNavigate({ kind: "daily", date: todayKey })}
                          >
                            {task.pinned && <Pin size={11} className="work-todo-dash-focus-pin" />}
                            {task.title || t("untitled")}
                          </button>
                        </div>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
              {counts.overdue > 0 && (
                <div className="work-todo-dash-focus-foot">
                  <button
                    type="button"
                    className="work-todo-dash-focus-overdue"
                    onClick={() =>
                      onNavigate({ kind: "daily", date: todayKey, highlightOverdue: true })
                    }
                  >
                    <AlertTriangle size={12} />
                    {t("dashFocusOverdue", { count: counts.overdue })}
                  </button>
                </div>
              )}
            </section>

          <section className="card work-todo-dash-cal">
              <WorkTodoCalMonthHead
                calMonth={calMonth}
                setCalMonth={setCalMonth}
                locale={locale}
                anchorYear={todayParsed.y}
              />
              <div className="work-todo-cal-weekdays">
                {weekLabels.map((label, i) => (
                  <span key={i}>{label}</span>
                ))}
              </div>
              <div className="work-todo-cal-grid work-todo-dash-cal-grid">
                {cells.map((key, i) =>
                  key ? (
                    <button
                      key={key}
                      type="button"
                      className={`work-todo-cal-day${key === todayKey ? " is-today" : ""}${
                        (openByDate.get(key) ?? 0) > 0 ? " has-open" : ""
                      }${(doneByDate.get(key) ?? 0) > 0 ? " has-done" : ""}`}
                      onClick={() => onNavigate({ kind: "daily", date: key })}
                      title={
                        (openByDate.get(key) ?? 0) > 0
                          ? t("dashCalOpen", { count: openByDate.get(key) })
                          : undefined
                      }
                    >
                      <span>{Number(key.slice(8))}</span>
                      <span className="work-todo-dash-cal-dots" aria-hidden>
                        {(openByDate.get(key) ?? 0) > 0 && <i className="is-open" />}
                        {(doneByDate.get(key) ?? 0) > 0 && <i className="is-done" />}
                      </span>
                    </button>
                  ) : (
                    <span key={`pad-${i}`} className="work-todo-cal-pad" />
                  )
                )}
              </div>
              <div className="work-todo-cal-foot">
                <button type="button" className="btn btn-secondary btn-small" onClick={jumpTodayCal}>
                  <CalendarDays size={12} />
                  {t("goToday")}
                </button>
              </div>
              <p className="work-todo-dash-cal-legend">
                <span>
                  <i className="is-open" /> {t("dashLegendOpen")}
                </span>
                <span>
                  <i className="is-done" /> {t("dashLegendDone")}
                </span>
              </p>
            </section>

            <section className="card work-todo-dash-list">
              <div className="work-todo-dash-list-head">
                <h3 className="work-todo-section-title">
                  <CheckCircle2 size={14} className="work-todo-dash-section-icon" aria-hidden />
                  {t("dashRecentDone")}
                </h3>
                {doneList.length > 0 && (
                  <span className="work-todo-count-badge">{doneList.length}</span>
                )}
              </div>
              <div className="work-todo-dash-list-scroll work-todo-scroll">
                {doneList.length === 0 ? (
                  <div className="work-todo-dash-empty">
                    <span className="work-todo-dash-empty-icon is-done" aria-hidden>
                      <CheckCircle2 size={20} strokeWidth={1.5} />
                    </span>
                    <p>{t("dashRecentDoneEmpty")}</p>
                  </div>
                ) : (
                  <ul className="work-todo-dash-ul">
                    {doneList.map((task) => (
                      <li key={task.id}>
                        <button
                          type="button"
                          className="work-todo-dash-link"
                          onClick={() =>
                            onNavigate({
                              kind: "daily",
                              date: task.workDate || todayKey,
                              showDone: true,
                            })
                          }
                        >
                          <CheckCircle2 size={14} className="work-todo-dash-link-icon is-done" />
                          <span className="work-todo-dash-link-text">
                            <span className="work-todo-dash-link-title">
                              {task.title || t("untitled")}
                            </span>
                            <span className="work-todo-dash-link-meta">
                              {localDateKeyFromIso(task.doneAt || task.updatedAt)}
                            </span>
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </section>
        </div>
      </div>
    </div>
  );
}
