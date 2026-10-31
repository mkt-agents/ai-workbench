import type { WorkTask } from "../../core/types";

export type KanbanColumnId = "backlog" | "today" | "scheduled" | "overdue" | "done";

/** Left-to-right on kanban; active buckets follow dashboard metrics (today → backlog → overdue). */
export const KANBAN_COLUMN_ORDER: KanbanColumnId[] = [
  "today",
  "scheduled",
  "backlog",
  "overdue",
  "done",
];

export function localDateKey(d = new Date()): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** Calendar day for an ISO timestamp in local timezone. */
export function localDateKeyFromIso(iso: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso.slice(0, 10);
  return localDateKey(d);
}

export function parseDateKey(key: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(key);
  if (!m) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return Number.isNaN(d.getTime()) ? null : d;
}

export function shiftDateKey(key: string, deltaDays: number): string {
  const d = parseDateKey(key);
  if (!d) return key;
  d.setDate(d.getDate() + deltaDays);
  return localDateKey(d);
}

export function formatDayLabel(key: string, todayKey: string, t: (k: string) => string): string {
  if (key === todayKey) return t("goToday");
  const d = parseDateKey(key);
  return d ? d.toLocaleDateString(undefined, { month: "short", day: "numeric", weekday: "short" }) : key;
}

export function formatMonthTitle(year: number, monthIndex: number, locale: string): string {
  return new Date(year, monthIndex, 1).toLocaleDateString(locale, {
    year: "numeric",
    month: "long",
  });
}

/** Monday-first grid of date keys for a month (null = padding cell). */
export function monthGrid(year: number, monthIndex: number): (string | null)[] {
  const first = new Date(year, monthIndex, 1);
  const startPad = (first.getDay() + 6) % 7;
  const daysInMonth = new Date(year, monthIndex + 1, 0).getDate();
  const cells: (string | null)[] = [];
  for (let i = 0; i < startPad; i++) cells.push(null);
  for (let day = 1; day <= daysInMonth; day++) {
    cells.push(localDateKey(new Date(year, monthIndex, day)));
  }
  while (cells.length % 7 !== 0) cells.push(null);
  return cells;
}

export function weekdayShortLabels(locale: string): string[] {
  return Array.from({ length: 7 }, (_, i) =>
    new Date(2024, 0, 1 + i).toLocaleDateString(locale, { weekday: "narrow" })
  );
}

export function taskSort(a: WorkTask, b: WorkTask): number {
  if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
  if (a.status !== b.status) return a.status === "open" ? -1 : 1;
  return b.updatedAt.localeCompare(a.updatedAt);
}

export function taskText(task: WorkTask): string {
  const body = task.notes.trim();
  return body ? `${task.title}\n\n${body}` : task.title;
}

export function isEnterSubmit(e: { nativeEvent: { isComposing: boolean }; keyCode: number; key: string; shiftKey: boolean }): boolean {
  if (e.nativeEvent.isComposing || e.keyCode === 229) return false;
  return e.key === "Enter" && !e.shiftKey;
}

/** Map a task into a kanban column relative to today. */
export function bucketTask(task: WorkTask, todayKey: string): KanbanColumnId {
  if (task.status === "done") return "done";
  if (task.workDate == null) return "backlog";
  if (task.workDate === todayKey) return "today";
  if (task.workDate > todayKey) return "scheduled";
  return "overdue";
}

export type WorkTodoView = "daily" | "kanban" | "dashboard";

export const VIEW_STORAGE_KEY = "work-todo-view";
export const UNDO_MS = 8000;
export const DONE_COLUMN_CAP = 40;
