import { useEffect, useRef, useState, type DragEvent } from "react";
import { useTranslation } from "react-i18next";
import {
  CalendarDays,
  Check,
  CheckCircle2,
  Circle,
  Copy,
  ListTodo,
  Pin,
  PinOff,
  Pencil,
  Sparkles,
  Trash2,
} from "lucide-react";
import type { WorkTask } from "../../core/types";
import { splitTags } from "../../lib/snippets";
import { isEnterSubmit } from "./utils";

type DoneToggleProps = {
  done: boolean;
  title: string;
  onToggle: () => void;
  className?: string;
  size?: number;
  stopPropagation?: boolean;
};

export function WorkTodoDoneToggle({
  done,
  title,
  onToggle,
  className = "",
  size = 16,
  stopPropagation = false,
}: DoneToggleProps) {
  const { t } = useTranslation("workTodo");
  const label = title.trim() || t("untitled");
  const aria = done
    ? t("markIncomplete", { title: label })
    : t("markComplete", { title: label });
  const cls = ["work-todo-done-toggle", done && "is-done", className].filter(Boolean).join(" ");

  return (
    <button
      type="button"
      className={cls}
      onClick={(e) => {
        if (stopPropagation) e.stopPropagation();
        onToggle();
      }}
      aria-label={aria}
      aria-pressed={done}
      title={done ? t("markIncompleteTip") : t("markCompleteTip")}
    >
      {done ? (
        <CheckCircle2 size={size} strokeWidth={2} aria-hidden />
      ) : (
        <Circle size={size} strokeWidth={2} aria-hidden />
      )}
    </button>
  );
}

export type WorkTodoTaskActions = {
  onToggleDone: (task: WorkTask) => void;
  onScheduleToday?: (task: WorkTask) => void | Promise<void>;
  onMoveBacklog?: (task: WorkTask) => void | Promise<void>;
  onTogglePin: (task: WorkTask) => void;
  onEdit: (task: WorkTask) => void;
  onInsertAiHub: (task: WorkTask) => void;
  onCopy: (task: WorkTask) => void;
  onDelete: (task: WorkTask) => void;
  onRename: (task: WorkTask, title: string) => void;
};

type Props = {
  task: WorkTask;
  variant?: "backlog" | "day" | "overdue";
  showScheduleToday?: boolean;
  showMoveBacklog?: boolean;
  expanded: boolean;
  onToggleExpand: () => void;
  copied: boolean;
  actions: WorkTodoTaskActions;
  /** Compact card for kanban. */
  compact?: boolean;
  draggable?: boolean;
  onDragStart?: (e: DragEvent) => void;
  onDragEnd?: (e: DragEvent) => void;
  dragging?: boolean;
};

export default function WorkTodoTaskRow({
  task,
  variant = "day",
  showScheduleToday,
  showMoveBacklog,
  expanded,
  onToggleExpand,
  copied,
  actions,
  compact,
  draggable,
  onDragStart,
  onDragEnd,
  dragging,
}: Props) {
  const { t } = useTranslation("workTodo");
  const hasNotes = task.notes.trim().length > 0;
  const tagList = splitTags(task.tags || "");
  const [renaming, setRenaming] = useState(false);
  const [titleDraft, setTitleDraft] = useState(task.title);
  const renameRef = useRef<HTMLInputElement>(null);
  const titleClickTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (!renaming) setTitleDraft(task.title);
  }, [task.title, renaming]);

  useEffect(() => {
    if (renaming) renameRef.current?.focus();
  }, [renaming]);

  useEffect(
    () => () => {
      if (titleClickTimer.current) clearTimeout(titleClickTimer.current);
    },
    []
  );

  const commitRename = () => {
    const next = titleDraft.trim();
    setRenaming(false);
    if (!next || next === task.title) {
      setTitleDraft(task.title);
      return;
    }
    actions.onRename(task, next);
  };

  return (
    <li
      className={`work-todo-row${task.pinned ? " is-pinned" : ""}${
        task.status === "done" ? " is-done" : ""
      }${compact ? " is-compact" : ""}${variant === "overdue" ? " is-overdue" : ""}${
        dragging ? " is-dragging" : ""
      }`}
      draggable={draggable}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
    >
      <WorkTodoDoneToggle
        done={task.status === "done"}
        title={task.title}
        onToggle={() => actions.onToggleDone(task)}
      />
      <div className="work-todo-row-main">
        <div className="work-todo-row-title-line">
          {renaming ? (
            <input
              ref={renameRef}
              className="input-field work-todo-rename-input"
              value={titleDraft}
              onChange={(e) => setTitleDraft(e.target.value)}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  setTitleDraft(task.title);
                  setRenaming(false);
                  return;
                }
                if (!isEnterSubmit(e)) return;
                e.preventDefault();
                commitRename();
              }}
              aria-label={t("title")}
            />
          ) : (
            <button
              type="button"
              className={`work-todo-title${task.status === "done" ? " is-done" : ""}`}
              onClick={() => {
                if (titleClickTimer.current) clearTimeout(titleClickTimer.current);
                titleClickTimer.current = setTimeout(() => {
                  titleClickTimer.current = null;
                  if (hasNotes) onToggleExpand();
                  else actions.onEdit(task);
                }, 250);
              }}
              onDoubleClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
                if (titleClickTimer.current) {
                  clearTimeout(titleClickTimer.current);
                  titleClickTimer.current = null;
                }
                setRenaming(true);
              }}
              title={t("renameHint")}
            >
              {task.title || t("untitled")}
              {hasNotes && !expanded && <span className="work-todo-notes-dot" title={t("notes")} />}
            </button>
          )}
          {task.workDate && variant !== "day" && (
            <span className="work-todo-date-badge">{task.workDate}</span>
          )}
          {tagList.length > 0 && (
            <div className="work-todo-tag-row" aria-label={t("tags")}>
              {tagList.map((tag) => (
                <span key={tag} className="work-todo-tag-chip">
                  {tag}
                </span>
              ))}
            </div>
          )}
          <div className="work-todo-row-actions">
            {showScheduleToday && (
              <button
                type="button"
                className="work-todo-schedule-btn"
                title={t("scheduleToday")}
                onClick={() => actions.onScheduleToday?.(task)}
              >
                <CalendarDays size={12} />
                <span>{t("scheduleToday")}</span>
              </button>
            )}
            <div className="work-todo-row-tools">
              {showMoveBacklog && (
                <button
                  type="button"
                  className="snippet-icon-btn"
                  title={t("moveToBacklog")}
                  onClick={() => actions.onMoveBacklog?.(task)}
                >
                  <ListTodo size={14} />
                </button>
              )}
              <button
                type="button"
                className="snippet-icon-btn"
                title={task.pinned ? t("unpin") : t("pin")}
                onClick={() => actions.onTogglePin(task)}
              >
                {task.pinned ? <PinOff size={14} /> : <Pin size={14} />}
              </button>
              <button
                type="button"
                className="snippet-icon-btn"
                title={t("edit")}
                onClick={() => actions.onEdit(task)}
              >
                <Pencil size={14} />
              </button>
              <button
                type="button"
                className="snippet-icon-btn"
                title={t("insertAiHub")}
                onClick={() => actions.onInsertAiHub(task)}
              >
                <Sparkles size={14} />
              </button>
              <button
                type="button"
                className="snippet-icon-btn"
                title={copied ? t("copied") : t("copy")}
                onClick={() => actions.onCopy(task)}
              >
                {copied ? <Check size={14} /> : <Copy size={14} />}
              </button>
              <button
                type="button"
                className="snippet-icon-btn danger"
                title={t("delete")}
                onClick={() => actions.onDelete(task)}
              >
                <Trash2 size={14} />
              </button>
            </div>
          </div>
        </div>
        {hasNotes && expanded && <pre className="work-todo-notes-preview">{task.notes}</pre>}
      </div>
    </li>
  );
}
