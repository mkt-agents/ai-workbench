import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Check, LayoutDashboard, Columns3, CalendarDays, Search, Trash2, Undo2, X } from "lucide-react";
import { useGlobalStore } from "../core/store";
import type { WorkTask } from "../core/types";
import { readStoredString, writeStoredString } from "../core/localState";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";
import DailyView from "./workTodo/DailyView";
import KanbanView from "./workTodo/KanbanView";
import DashboardView, { type DashNavigate } from "./workTodo/DashboardView";
import type { WorkTodoTaskActions } from "./workTodo/WorkTodoTaskRow";
import { patchForColumnDrop } from "./workTodo/selectors";
import {
  localDateKey,
  taskText,
  UNDO_MS,
  VIEW_STORAGE_KEY,
  type WorkTodoView,
} from "./workTodo/utils";
import type { KanbanColumnId } from "./workTodo/utils";

function readView(): WorkTodoView {
  const v = readStoredString(VIEW_STORAGE_KEY);
  if (v === "kanban" || v === "dashboard" || v === "daily") return v;
  return "dashboard";
}

export default function DraftsManager() {
  const { t, i18n } = useTranslation("workTodo");
  const { t: tNav } = useTranslation("navigation");
  const confirm = useConfirm();
  const [todayKey, setTodayKey] = useState(() => localDateKey());
  const [viewDate, setViewDate] = useState(() => localDateKey());
  const [pageView, setPageView] = useState<WorkTodoView>(() => readView());
  const workTasks = useGlobalStore((s) => s.workTasks);
  const addWorkTask = useGlobalStore((s) => s.addWorkTask);
  const updateWorkTask = useGlobalStore((s) => s.updateWorkTask);
  const deleteWorkTask = useGlobalStore((s) => s.deleteWorkTask);
  const restoreWorkTasks = useGlobalStore((s) => s.restoreWorkTasks);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [query, setQuery] = useState("");
  const [showDone, setShowDone] = useState(false);
  const [editing, setEditing] = useState<WorkTask | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [undo, setUndo] = useState<
    | { kind: "delete"; items: WorkTask[]; text: string }
    | { kind: "complete"; taskId: string; text: string }
    | null
  >(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [highlightOverdue, setHighlightOverdue] = useState(false);
  const [dashNavToken, setDashNavToken] = useState(0);
  const [focusQuickAddToken, setFocusQuickAddToken] = useState(0);

  const msgTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const undoTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const quickRef = useRef<HTMLInputElement>(null);
  const pageRef = useRef<HTMLDivElement>(null);

  const locale = i18n.language || "zh-CN";

  useEffect(() => writeStoredString(VIEW_STORAGE_KEY, pageView), [pageView]);

  useEffect(() => {
    const tick = () => setTodayKey(localDateKey());
    const onVis = () => {
      if (document.visibilityState === "visible") tick();
    };
    document.addEventListener("visibilitychange", onVis);
    const id = window.setInterval(tick, 60_000);
    return () => {
      document.removeEventListener("visibilitychange", onVis);
      window.clearInterval(id);
    };
  }, []);

  const onDashNavConsumed = useCallback(() => {
    setHighlightOverdue(false);
  }, []);

  useEffect(
    () => () => {
      if (msgTimer.current) clearTimeout(msgTimer.current);
      if (undoTimer.current) clearTimeout(undoTimer.current);
    },
    []
  );

  const showMsg = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    if (msgTimer.current) clearTimeout(msgTimer.current);
    msgTimer.current = setTimeout(() => setMessage(null), 3500);
  };

  const offerUndoDelete = (items: WorkTask[], text: string) => {
    setMessage(null);
    setUndo({ kind: "delete", items, text });
    if (undoTimer.current) clearTimeout(undoTimer.current);
    undoTimer.current = setTimeout(() => setUndo(null), UNDO_MS);
  };

  const offerUndoComplete = (taskId: string, text: string) => {
    setMessage(null);
    setShowDone(true);
    setUndo({ kind: "complete", taskId, text });
    if (undoTimer.current) clearTimeout(undoTimer.current);
    undoTimer.current = setTimeout(() => setUndo(null), UNDO_MS);
  };

  const handleUndo = async () => {
    if (!undo) return;
    if (undoTimer.current) clearTimeout(undoTimer.current);
    const current = undo;
    setUndo(null);
    try {
      if (current.kind === "delete") await restoreWorkTasks(current.items);
      else await updateWorkTask(current.taskId, { status: "open" });
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const toggleExpand = useCallback((id: string) => {
    setExpanded((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  }, []);

  const actions: WorkTodoTaskActions = {
    onToggleDone: (task) => {
      const next = task.status === "done" ? "open" : "done";
      void updateWorkTask(task.id, { status: next })
        .then(() => {
          if (next === "done") {
            offerUndoComplete(task.id, t("completedToast", { name: task.title || t("untitled") }));
          }
        })
        .catch((e) => showMsg("error", String(e)));
    },
    onScheduleToday: async (task) => {
      try {
        await updateWorkTask(task.id, { workDate: todayKey });
      } catch (e) {
        showMsg("error", String(e));
      }
    },
    onMoveBacklog: async (task) => {
      try {
        await updateWorkTask(task.id, { workDate: null });
      } catch (e) {
        showMsg("error", String(e));
      }
    },
    onTogglePin: (task) => {
      void updateWorkTask(task.id, { pinned: !task.pinned }).catch((e) => showMsg("error", String(e)));
    },
    onEdit: (task) => setEditing({ ...task }),
    onInsertAiHub: (task) => {
      void invoke("open_ai_hub_with_text", { text: taskText(task) }).catch((e) =>
        showMsg("error", String(e))
      );
    },
    onCopy: (task) => {
      void invokeCopyToClipboard(taskText(task))
        .then(() => {
          setCopiedId(task.id);
          window.setTimeout(() => setCopiedId((cur) => (cur === task.id ? null : cur)), 1500);
        })
        .catch((e) => showMsg("error", t("copyFailed", { error: String(e) })));
    },
    onDelete: (task) => {
      void deleteWorkTask(task.id)
        .then(() => offerUndoDelete([task], t("deletedToast", { name: task.title || t("untitled") })))
        .catch((e) => showMsg("error", String(e)));
    },
    onRename: (task, title) => {
      void updateWorkTask(task.id, { title }).catch((e) => showMsg("error", String(e)));
    },
  };

  const editDirty =
    !!editing &&
    !!workTasks.find((x) => x.id === editing.id) &&
    (editing.title !== workTasks.find((x) => x.id === editing.id)!.title ||
      editing.notes !== workTasks.find((x) => x.id === editing.id)!.notes ||
      (editing.tags || "") !== (workTasks.find((x) => x.id === editing.id)!.tags || ""));

  const handleSaveEdit = async () => {
    if (!editing || !editing.title.trim()) return;
    if (!editDirty) {
      setEditing(null);
      return;
    }
    try {
      await updateWorkTask(editing.id, {
        title: editing.title.trim(),
        notes: editing.notes,
        tags: editing.tags || "",
      });
      setEditing(null);
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  const requestCloseEdit = async () => {
    if (editDirty) {
      const ok = await confirm({
        title: t("discardTitle"),
        message: t("discardConfirm"),
        confirmText: t("discard"),
        icon: "warning",
      });
      if (!ok) return;
    }
    setEditing(null);
  };

  const switchView = (v: WorkTodoView) => {
    setPageView(v);
    setHighlightOverdue(false);
  };

  const onDashNavigate = (target: DashNavigate) => {
    if (target.kind === "kanban") {
      setPageView("kanban");
      return;
    }
    setPageView("daily");
    if (target.date) setViewDate(target.date);
    if (target.showDone) setShowDone(true);
    setHighlightOverdue(!!target.highlightOverdue);
    if (target.focusQuickAdd) setFocusQuickAddToken((n) => n + 1);
    setDashNavToken((n) => n + 1);
  };

  const onMoveToColumn = async (task: WorkTask, column: KanbanColumnId) => {
    try {
      await updateWorkTask(task.id, patchForColumnDrop(column, todayKey, task));
    } catch (e) {
      showMsg("error", String(e));
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing || e.keyCode === 229) return;
      const tag = (e.target as HTMLElement | null)?.tagName;
      const inField =
        tag === "INPUT" || tag === "TEXTAREA" || (e.target as HTMLElement)?.isContentEditable;
      if (document.querySelector(".modal-overlay")) return;

      if (e.key === "/" && !inField) {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (inField) return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === "n" || e.key === "N") {
        e.preventDefault();
        if (pageView !== "daily") setPageView("daily");
        window.setTimeout(() => quickRef.current?.focus(), 0);
        return;
      }
      if (e.key === "t" || e.key === "T") {
        e.preventDefault();
        setViewDate(todayKey);
        setPageView("daily");
        return;
      }
      if (e.key === "1") {
        e.preventDefault();
        switchView("dashboard");
      } else if (e.key === "2") {
        e.preventDefault();
        switchView("daily");
      } else if (e.key === "3") {
        e.preventDefault();
        switchView("kanban");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [pageView, todayKey]);

  return (
    <div className="drafts-page work-todo-page" ref={pageRef}>
      <div
        className={`work-todo-shell-bar${pageView !== "daily" ? " is-no-search" : ""}`}
      >
        <h1 className="work-todo-page-title">{tNav("drafts")}</h1>
        <div className="work-todo-tabs" role="tablist" aria-label={t("views")}>
          <button
            type="button"
            role="tab"
            aria-selected={pageView === "dashboard"}
            className={`work-todo-tab${pageView === "dashboard" ? " is-active" : ""}`}
            onClick={() => switchView("dashboard")}
          >
            <LayoutDashboard size={13} />
            {t("viewDashboard")}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={pageView === "daily"}
            className={`work-todo-tab${pageView === "daily" ? " is-active" : ""}`}
            onClick={() => switchView("daily")}
          >
            <CalendarDays size={13} />
            {t("viewDaily")}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={pageView === "kanban"}
            className={`work-todo-tab${pageView === "kanban" ? " is-active" : ""}`}
            onClick={() => switchView("kanban")}
          >
            <Columns3 size={13} />
            {t("viewKanban")}
          </button>
        </div>
        {pageView === "daily" && (
          <div className="work-todo-search">
            <Search size={13} className="work-todo-search-icon" />
            <input
              ref={searchRef}
              className="input-field"
              placeholder={t("search")}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              aria-label={t("search")}
            />
            {query && (
              <button
                type="button"
                className="snippets-search-clear"
                onClick={() => setQuery("")}
                aria-label={t("clearSearch")}
              >
                <X size={11} />
              </button>
            )}
          </div>
        )}
      </div>

      {pageView === "daily" && (
        <DailyView
          todayKey={todayKey}
          viewDate={viewDate}
          onViewDate={setViewDate}
          locale={locale}
          workTasks={workTasks}
          query={query}
          showDone={showDone}
          onShowDone={setShowDone}
          copiedId={copiedId}
          expanded={expanded}
          onToggleExpand={toggleExpand}
          actions={actions}
          onAddTask={async (title, workDate, extras) => {
            await addWorkTask({
              title,
              notes: extras?.notes ?? "",
              tags: extras?.tags ?? "",
              status: "open",
              workDate,
              pinned: false,
            });
          }}
          onError={(msg) => showMsg("error", msg)}
          dashNavToken={dashNavToken}
          highlightOverdue={highlightOverdue}
          onDashNavConsumed={onDashNavConsumed}
          focusQuickAddToken={focusQuickAddToken}
          quickAddRef={quickRef}
        />
      )}

      {pageView === "kanban" && (
        <KanbanView
          todayKey={todayKey}
          workTasks={workTasks}
          query={query}
          onQueryChange={setQuery}
          searchRef={searchRef}
          copiedId={copiedId}
          expanded={expanded}
          onToggleExpand={toggleExpand}
          actions={actions}
          onMoveToColumn={onMoveToColumn}
        />
      )}

      {pageView === "dashboard" && (
        <DashboardView
          todayKey={todayKey}
          locale={locale}
          workTasks={workTasks}
          onNavigate={onDashNavigate}
          onToggleDone={(task) => actions.onToggleDone(task)}
        />
      )}

      {editing && (
        <div className="modal-overlay">
          <div className="modal drafts-modal">
            <ModalTitleRow title={t("editTask")} onClose={() => void requestCloseEdit()} />
            <div className="work-todo-task-editor">
              <label className="work-todo-edit-field">
                <span className="work-todo-edit-label">{t("title")}</span>
                <input
                  className="input-field work-todo-edit-input"
                  value={editing.title}
                  onChange={(e) => setEditing({ ...editing, title: e.target.value })}
                  placeholder={t("title")}
                  autoFocus
                />
              </label>
              <label className="work-todo-edit-field">
                <span className="work-todo-edit-label">{t("tags")}</span>
                <input
                  className="input-field work-todo-edit-input"
                  value={editing.tags || ""}
                  onChange={(e) => setEditing({ ...editing, tags: e.target.value })}
                  placeholder={t("tagsPlaceholder")}
                />
              </label>
              <label className="work-todo-edit-field work-todo-edit-field-grow">
                <span className="work-todo-edit-label">{t("notes")}</span>
                <textarea
                  className="input-field work-todo-edit-textarea"
                  rows={10}
                  value={editing.notes}
                  onChange={(e) => setEditing({ ...editing, notes: e.target.value })}
                  placeholder={t("notesPlaceholder")}
                  spellCheck
                />
              </label>
            </div>
            <div className="modal-actions">
              <button type="button" className="btn btn-secondary" onClick={() => void requestCloseEdit()}>
                {t("cancel")}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => void handleSaveEdit()}
                disabled={!editing.title.trim()}
              >
                {t("save")}
              </button>
            </div>
          </div>
        </div>
      )}

      {undo && (
        <div
          className={`toast ${undo.kind === "delete" ? "toast-warning" : "toast-success"}`}
          role="status"
        >
          <span className="toast-icon">
            {undo.kind === "delete" ? <Trash2 size={16} /> : <Check size={16} />}
          </span>
          <span className="toast-text">{undo.text}</span>
          <button
            type="button"
            className="btn btn-secondary btn-small toast-action"
            onClick={() => void handleUndo()}
          >
            <Undo2 size={12} /> {t("undo")}
          </button>
        </div>
      )}

      {!undo && message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <X size={16} />}
          </span>
          {message.text}
        </div>
      )}
    </div>
  );
}
