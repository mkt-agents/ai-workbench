import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Inbox, PenLine, Plus, Target } from "lucide-react";
import type { WorkTask } from "../../core/types";
import WorkTodoDateChrome from "./WorkTodoDateChrome";
import WorkTodoTaskRow, { type WorkTodoTaskActions } from "./WorkTodoTaskRow";
import { activeDates as computeActiveDates, matchesTaskQuery, overdueOpen } from "./selectors";
import { isEnterSubmit, taskSort } from "./utils";

export type WorkTodoComposeExtras = { tags?: string; notes?: string };

type Props = {
  todayKey: string;
  viewDate: string;
  onViewDate: (key: string) => void;
  locale: string;
  workTasks: WorkTask[];
  query: string;
  showDone: boolean;
  onShowDone: (v: boolean) => void;
  copiedId: string | null;
  expanded: Set<string>;
  onToggleExpand: (id: string) => void;
  actions: WorkTodoTaskActions;
  onAddTask: (title: string, workDate: string, extras?: WorkTodoComposeExtras) => Promise<void>;
  onError: (msg: string) => void;
  dashNavToken?: number;
  highlightOverdue?: boolean;
  onDashNavConsumed?: () => void;
  focusQuickAddToken?: number;
  quickAddRef?: React.RefObject<HTMLInputElement | null>;
};

export default function DailyView({
  todayKey,
  viewDate,
  onViewDate,
  locale,
  workTasks,
  query,
  showDone,
  onShowDone,
  copiedId,
  expanded,
  onToggleExpand,
  actions,
  onAddTask,
  onError,
  dashNavToken = 0,
  highlightOverdue,
  onDashNavConsumed,
  focusQuickAddToken = 0,
  quickAddRef,
}: Props) {
  const { t } = useTranslation("workTodo");
  const [composeTitle, setComposeTitle] = useState("");
  const [composeTags, setComposeTags] = useState("");
  const [composeNotes, setComposeNotes] = useState("");
  const [composeBusy, setComposeBusy] = useState(false);
  const localTitleRef = useRef<HTMLInputElement>(null);
  const titleRef = quickAddRef ?? localTitleRef;
  const overdueRef = useRef<HTMLElement>(null);

  const clearCompose = useCallback(() => {
    setComposeTitle("");
    setComposeTags("");
    setComposeNotes("");
  }, []);

  const handleViewDate = useCallback(
    (key: string) => {
      if (key === viewDate) return;
      clearCompose();
      onViewDate(key);
    },
    [onViewDate, viewDate, clearCompose]
  );

  useEffect(() => {
    clearCompose();
  }, [viewDate, clearCompose]);

  useEffect(() => {
    if (!dashNavToken) return;
    if (highlightOverdue) {
      window.requestAnimationFrame(() => {
        overdueRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
      });
    }
    onDashNavConsumed?.();
  }, [dashNavToken, highlightOverdue, onDashNavConsumed]);

  useEffect(() => {
    if (!focusQuickAddToken) return;
    window.setTimeout(() => titleRef.current?.focus(), 0);
  }, [focusQuickAddToken, titleRef]);

  const marks = useMemo(() => computeActiveDates(workTasks), [workTasks]);

  const dayTasks = useMemo(
    () => workTasks.filter((task) => task.workDate === viewDate).sort(taskSort),
    [workTasks, viewDate]
  );
  const openDayTasks = useMemo(
    () => dayTasks.filter((t) => t.status === "open" && matchesTaskQuery(t, query)),
    [dayTasks, query]
  );
  const doneDayTasks = useMemo(
    () => dayTasks.filter((t) => t.status === "done" && matchesTaskQuery(t, query)),
    [dayTasks, query]
  );
  const backlog = useMemo(
    () =>
      workTasks
        .filter((t) => t.workDate == null && t.status === "open")
        .filter((t) => matchesTaskQuery(t, query))
        .sort(taskSort),
    [workTasks, query]
  );
  const doneBacklog = useMemo(
    () =>
      workTasks
        .filter((t) => t.workDate == null && t.status === "done")
        .filter((t) => matchesTaskQuery(t, query))
        .sort(taskSort),
    [workTasks, query]
  );
  const overdue = useMemo(
    () => (viewDate === todayKey ? overdueOpen(workTasks, todayKey, query) : []),
    [workTasks, todayKey, viewDate, query]
  );

  const dayDoneCount = doneDayTasks.length;
  const dayTotal = openDayTasks.length + dayDoneCount;

  const handleComposeAdd = async () => {
    const title = composeTitle.trim();
    if (!title || composeBusy) return;
    const tags = composeTags.trim();
    const notes = composeNotes.trim();
    setComposeBusy(true);
    clearCompose();
    try {
      await onAddTask(title, viewDate, { tags, notes });
      titleRef.current?.focus();
    } catch (e) {
      setComposeTitle(title);
      setComposeTags(tags);
      setComposeNotes(notes);
      onError(String(e));
    } finally {
      setComposeBusy(false);
    }
  };

  const scheduleAllOverdue = async () => {
    for (const task of overdue) {
      await Promise.resolve(actions.onScheduleToday?.(task));
    }
  };

  const renderList = (list: WorkTask[], opts: { backlog?: boolean; overdue?: boolean }) =>
    list.map((task) => (
      <WorkTodoTaskRow
        key={task.id}
        task={task}
        variant={opts.overdue ? "overdue" : opts.backlog ? "backlog" : "day"}
        showScheduleToday={!!(opts.backlog || opts.overdue) && viewDate === todayKey}
        showMoveBacklog={!opts.backlog && !opts.overdue}
        expanded={expanded.has(task.id)}
        onToggleExpand={() => onToggleExpand(task.id)}
        copied={copiedId === task.id}
        actions={actions}
      />
    ));

  return (
    <div className="work-todo-daily">
      <header className="work-todo-chrome">
        <WorkTodoDateChrome
          todayKey={todayKey}
          viewDate={viewDate}
          onViewDate={handleViewDate}
          activeDates={marks}
          locale={locale}
          stats={
            <div className="work-todo-stats">
              <span className="work-todo-stat is-open">
                <span className="work-todo-stat-num">{openDayTasks.length}</span>
                {t("statOpen")}
              </span>
              {dayTotal > 0 && (
                <span className="work-todo-stat is-progress" title={t("dayProgress")}>
                  <span className="work-todo-stat-num">
                    {dayDoneCount}/{dayTotal}
                  </span>
                  {t("statProgress")}
                </span>
              )}
              {viewDate === todayKey && backlog.length > 0 && (
                <span className="work-todo-stat is-backlog">
                  <span className="work-todo-stat-num">{backlog.length}</span>
                  {t("statBacklog")}
                </span>
              )}
              {viewDate === todayKey && overdue.length > 0 && (
                <span className="work-todo-stat is-overdue">
                  <span className="work-todo-stat-num">{overdue.length}</span>
                  {t("statOverdue")}
                </span>
              )}
            </div>
          }
        />
      </header>

      <section className="card work-todo-compose" aria-label={t("composeHeading")}>
        <div className="work-todo-compose-toolbar">
          <span className="work-todo-compose-icon" aria-hidden>
            <PenLine size={14} />
          </span>
          <input
            ref={titleRef as React.RefObject<HTMLInputElement>}
            className="input-field work-todo-compose-title-input"
            value={composeTitle}
            onChange={(e) => setComposeTitle(e.target.value)}
            onKeyDown={(e) => {
              if (!isEnterSubmit(e)) return;
              e.preventDefault();
              void handleComposeAdd();
            }}
            placeholder={t("composeTitlePh")}
            aria-label={t("title")}
            disabled={composeBusy}
          />
          <input
            className="input-field work-todo-compose-tags-input"
            value={composeTags}
            onChange={(e) => setComposeTags(e.target.value)}
            onKeyDown={(e) => {
              if (!isEnterSubmit(e)) return;
              e.preventDefault();
              void handleComposeAdd();
            }}
            placeholder={t("tagsPlaceholder")}
            aria-label={t("tags")}
            disabled={composeBusy}
          />
          <button
            type="button"
            className="btn btn-primary btn-small work-todo-compose-submit"
            onClick={() => void handleComposeAdd()}
            disabled={composeBusy || !composeTitle.trim()}
          >
            <Plus size={13} />
            {t("composeAdd")}
          </button>
        </div>
        <div className="work-todo-compose-notes-row">
          <textarea
            className="input-field work-todo-compose-notes"
            rows={1}
            value={composeNotes}
            onChange={(e) => setComposeNotes(e.target.value)}
            placeholder={t("notesPlaceholder")}
            aria-label={t("notes")}
            spellCheck
            disabled={composeBusy}
          />
        </div>
      </section>

      <div className="work-todo-daily-main">
        <div
          className={`work-todo-board${viewDate === todayKey ? " has-backlog" : ""}${
            viewDate === todayKey && overdue.length > 0 ? " has-overdue" : ""
          }`}
        >
          <section className="card work-todo-panel is-panel-today">
            <div className="work-todo-panel-head">
              <h3 className="work-todo-section-title work-todo-daily-panel-title">
                <span className="work-todo-daily-panel-icon is-today" aria-hidden>
                  <Target size={14} />
                </span>
                {t("todayTasks")}
              </h3>
              {(doneDayTasks.length > 0 || doneBacklog.length > 0) && (
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={() => onShowDone(!showDone)}
                >
                  {showDone ? t("hideDone") : t("toggleDone")}
                  {!showDone ? ` (${doneDayTasks.length + doneBacklog.length})` : ""}
                </button>
              )}
            </div>
            <div className="work-todo-panel-scroll work-todo-scroll">
              {openDayTasks.length === 0 && (!showDone || doneDayTasks.length === 0) ? (
                <div className="work-todo-empty">
                  <p>{query ? t("noMatch") : t("emptyTasks")}</p>
                  {!query && <span>{t("emptyTasksHint")}</span>}
                </div>
              ) : (
                <ul className="work-todo-list">
                  {renderList(openDayTasks, {})}
                  {showDone && renderList(doneDayTasks, {})}
                </ul>
              )}
            </div>
          </section>

          {viewDate === todayKey && (
            <section className="card work-todo-panel is-panel-backlog" title={t("backlogHint")}>
              <div className="work-todo-panel-head">
                <h3 className="work-todo-section-title work-todo-daily-panel-title">
                  <span className="work-todo-daily-panel-icon is-backlog" aria-hidden>
                    <Inbox size={14} />
                  </span>
                  {t("backlog")}
                  {backlog.length > 0 && (
                    <span className="work-todo-count-badge">{backlog.length}</span>
                  )}
                </h3>
                {doneBacklog.length > 0 && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => onShowDone(!showDone)}
                  >
                    {showDone ? t("hideDone") : t("toggleDone")}
                    {!showDone ? ` (${doneBacklog.length})` : ""}
                  </button>
                )}
              </div>
              <div className="work-todo-panel-scroll work-todo-scroll">
                {backlog.length === 0 && (!showDone || doneBacklog.length === 0) ? (
                  <div className="work-todo-empty">
                    <p>{query ? t("noMatch") : t("emptyBacklog")}</p>
                  </div>
                ) : (
                  <ul className="work-todo-list">
                    {renderList(backlog, { backlog: true })}
                    {showDone && renderList(doneBacklog, { backlog: true })}
                  </ul>
                )}
              </div>
            </section>
          )}

          {viewDate === todayKey && overdue.length > 0 && (
            <section
              ref={overdueRef}
              className={`card work-todo-panel work-todo-overdue-panel is-panel-overdue${
                highlightOverdue ? " is-highlight" : ""
              }`}
            >
              <div className="work-todo-panel-head">
                <h3 className="work-todo-section-title work-todo-daily-panel-title">
                  <span className="work-todo-daily-panel-icon is-overdue" aria-hidden>
                    <AlertTriangle size={14} />
                  </span>
                  {t("overdue")}
                  <span className="work-todo-count-badge">{overdue.length}</span>
                </h3>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={() => void scheduleAllOverdue()}
                >
                  {t("scheduleAllToday")}
                </button>
              </div>
              <div className="work-todo-panel-scroll work-todo-scroll">
                <ul className="work-todo-list">{renderList(overdue, { overdue: true })}</ul>
              </div>
            </section>
          )}
        </div>
      </div>
    </div>
  );
}
