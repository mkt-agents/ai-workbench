import { useState, type ReactNode, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  CalendarDays,
  CheckCircle2,
  Inbox,
  Search,
  Target,
  X,
} from "lucide-react";
import type { WorkTask } from "../../core/types";
import WorkTodoTaskRow, { type WorkTodoTaskActions } from "./WorkTodoTaskRow";
import { kanbanColumns } from "./selectors";
import { KANBAN_COLUMN_ORDER, type KanbanColumnId } from "./utils";

const COLUMN_ICON: Record<KanbanColumnId, ReactNode> = {
  overdue: <AlertTriangle size={13} aria-hidden />,
  today: <Target size={13} aria-hidden />,
  scheduled: <CalendarDays size={13} aria-hidden />,
  backlog: <Inbox size={13} aria-hidden />,
  done: <CheckCircle2 size={13} aria-hidden />,
};

const DND_MIME = "application/x-work-todo-id";

type Props = {
  todayKey: string;
  workTasks: WorkTask[];
  query: string;
  onQueryChange: (value: string) => void;
  searchRef?: RefObject<HTMLInputElement>;
  copiedId: string | null;
  expanded: Set<string>;
  onToggleExpand: (id: string) => void;
  actions: WorkTodoTaskActions;
  onMoveToColumn: (task: WorkTask, column: KanbanColumnId) => Promise<void>;
};

export default function KanbanView({
  todayKey,
  workTasks,
  query,
  onQueryChange,
  searchRef,
  copiedId,
  expanded,
  onToggleExpand,
  actions,
  onMoveToColumn,
}: Props) {
  const { t } = useTranslation("workTodo");
  const cols = kanbanColumns(workTasks, todayKey, query);
  const [dragOver, setDragOver] = useState<KanbanColumnId | null>(null);
  const [draggingId, setDraggingId] = useState<string | null>(null);

  const filtered = query.trim().length > 0;
  const total = KANBAN_COLUMN_ORDER.reduce((n, id) => n + cols[id].length, 0);

  const columnTitle = (id: KanbanColumnId) => {
    switch (id) {
      case "backlog":
        return t("colBacklog");
      case "today":
        return t("colToday");
      case "scheduled":
        return t("colScheduled");
      case "overdue":
        return t("colOverdue");
      case "done":
        return t("colDone");
    }
  };

  return (
    <div className="work-todo-kanban">
      <div className="work-todo-kanban-toolbar">
        <div className="work-todo-search work-todo-kanban-search">
          <Search size={13} className="work-todo-search-icon" />
          <input
            ref={searchRef}
            className="input-field"
            placeholder={t("search")}
            value={query}
            onChange={(e) => onQueryChange(e.target.value)}
            aria-label={t("search")}
          />
          {query && (
            <button
              type="button"
              className="snippets-search-clear"
              onClick={() => onQueryChange("")}
              aria-label={t("clearSearch")}
            >
              <X size={11} />
            </button>
          )}
        </div>
        <span className="work-todo-kanban-stat">
          {filtered ? t("kanbanMatched", { count: total }) : t("kanbanTotal", { count: total })}
        </span>
        <p className="work-todo-kanban-hint">{t("kanbanHint")}</p>
      </div>

      <div className="work-todo-kanban-board work-todo-scroll">
        {KANBAN_COLUMN_ORDER.map((colId) => {
          const list = cols[colId];
          return (
            <section
              key={colId}
              className={`card work-todo-kanban-col is-col-${colId}${
                dragOver === colId ? " is-drop-target" : ""
              }${list.length === 0 ? " is-empty" : ""}`}
              aria-label={columnTitle(colId)}
              onDragOver={(e) => {
                if (colId === "overdue") {
                  e.dataTransfer.dropEffect = "none";
                  return;
                }
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (dragOver !== colId) setDragOver(colId);
              }}
              onDragLeave={() => {
                if (dragOver === colId) setDragOver(null);
              }}
              onDrop={(e) => {
                e.preventDefault();
                setDragOver(null);
                if (colId === "overdue") {
                  setDraggingId(null);
                  return;
                }
                const id = e.dataTransfer.getData(DND_MIME) || e.dataTransfer.getData("text/plain");
                const task = workTasks.find((x) => x.id === id);
                if (!task) return;
                void onMoveToColumn(task, colId);
                setDraggingId(null);
              }}
            >
              <header className="work-todo-kanban-col-head">
                <h3 className="work-todo-section-title work-todo-kanban-col-title">
                  <span className={`work-todo-kanban-col-icon is-${colId}`}>
                    {COLUMN_ICON[colId]}
                  </span>
                  {columnTitle(colId)}
                </h3>
                <span
                  className={`work-todo-count-badge work-todo-kanban-count${
                    list.length === 0 ? " is-zero" : ""
                  }`}
                >
                  {list.length}
                </span>
              </header>
              <div className="work-todo-kanban-body work-todo-scroll">
                {list.length === 0 ? (
                  <div className="work-todo-kanban-empty">
                    {filtered ? t("kanbanNoMatch") : t("kanbanEmpty")}
                  </div>
                ) : (
                  <ul className="work-todo-list work-todo-kanban-list">
                    {list.map((task) => (
                      <WorkTodoTaskRow
                        key={task.id}
                        task={task}
                        variant={
                          colId === "backlog" ? "backlog" : colId === "overdue" ? "overdue" : "day"
                        }
                        showScheduleToday={colId === "backlog" || colId === "overdue"}
                        showMoveBacklog={colId === "today" || colId === "scheduled"}
                        expanded={expanded.has(task.id)}
                        onToggleExpand={() => onToggleExpand(task.id)}
                        copied={copiedId === task.id}
                        actions={actions}
                        compact
                        draggable
                        dragging={draggingId === task.id}
                        onDragStart={(e) => {
                          e.dataTransfer.setData(DND_MIME, task.id);
                          e.dataTransfer.setData("text/plain", task.id);
                          e.dataTransfer.effectAllowed = "move";
                          setDraggingId(task.id);
                        }}
                        onDragEnd={() => {
                          setDraggingId(null);
                          setDragOver(null);
                        }}
                      />
                    ))}
                  </ul>
                )}
              </div>
            </section>
          );
        })}
      </div>
    </div>
  );
}
