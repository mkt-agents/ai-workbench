import type { WorkTask } from "../../core/types";
import {
  bucketTask,
  DONE_COLUMN_CAP,
  localDateKeyFromIso,
  shiftDateKey,
  taskSort,
  type KanbanColumnId,
} from "./utils";

export function matchesTaskQuery(task: WorkTask, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    task.title.toLowerCase().includes(q) ||
    task.notes.toLowerCase().includes(q) ||
    (task.tags || "").toLowerCase().includes(q)
  );
}

export function activeDates(tasks: WorkTask[]): Set<string> {
  const set = new Set<string>();
  for (const task of tasks) {
    if (task.workDate && task.status === "open") set.add(task.workDate);
  }
  return set;
}

export function overdueOpen(tasks: WorkTask[], todayKey: string, query = ""): WorkTask[] {
  return tasks
    .filter(
      (t) =>
        t.status === "open" &&
        t.workDate != null &&
        t.workDate < todayKey &&
        matchesTaskQuery(t, query)
    )
    .sort(taskSort);
}

export function openCountByDate(tasks: WorkTask[]): Map<string, number> {
  const map = new Map<string, number>();
  for (const t of tasks) {
    if (t.status !== "open" || !t.workDate) continue;
    map.set(t.workDate, (map.get(t.workDate) ?? 0) + 1);
  }
  return map;
}

export type DashboardCounts = {
  openTotal: number;
  todayOpen: number;
  todayDone: number;
  todayTotal: number;
  backlog: number;
  overdue: number;
};

export function countsForDashboard(tasks: WorkTask[], todayKey: string): DashboardCounts {
  let openTotal = 0;
  let todayOpen = 0;
  let todayDone = 0;
  let backlog = 0;
  let overdue = 0;
  for (const t of tasks) {
    if (t.status === "open") {
      openTotal += 1;
      if (t.workDate == null) backlog += 1;
      else if (t.workDate === todayKey) todayOpen += 1;
      else if (t.workDate < todayKey) overdue += 1;
    } else {
      const doneDay = localDateKeyFromIso(t.doneAt || t.updatedAt);
      if (doneDay === todayKey) todayDone += 1;
    }
  }
  return {
    openTotal,
    todayOpen,
    todayDone,
    todayTotal: todayOpen + todayDone,
    backlog,
    overdue,
  };
}

/** Local-date buckets of completions for the last `days` days ending today. */
export function completedInRange(
  tasks: WorkTask[],
  todayKey: string,
  days: number
): { date: string; count: number }[] {
  const start = shiftDateKey(todayKey, -(days - 1));
  const counts = new Map<string, number>();
  for (let i = 0; i < days; i++) {
    counts.set(shiftDateKey(start, i), 0);
  }
  for (const t of tasks) {
    if (t.status !== "done" || !t.doneAt) continue;
    const day = localDateKeyFromIso(t.doneAt);
    if (!counts.has(day)) continue;
    counts.set(day, (counts.get(day) ?? 0) + 1);
  }
  return Array.from(counts.entries()).map(([date, count]) => ({ date, count }));
}

export function recentDone(tasks: WorkTask[], limit = 10): WorkTask[] {
  return tasks
    .filter((t) => t.status === "done")
    .sort((a, b) => (b.doneAt || b.updatedAt).localeCompare(a.doneAt || a.updatedAt))
    .slice(0, limit);
}

export function completedCountByDate(tasks: WorkTask[]): Map<string, number> {
  const map = new Map<string, number>();
  for (const t of tasks) {
    if (t.status !== "done") continue;
    const day = localDateKeyFromIso(t.doneAt || t.updatedAt);
    if (!day) continue;
    map.set(day, (map.get(day) ?? 0) + 1);
  }
  return map;
}

export function sumCompletedInRange(
  tasks: WorkTask[],
  todayKey: string,
  days: number
): number {
  return completedInRange(tasks, todayKey, days).reduce((n, d) => n + d.count, 0);
}

/** Open tasks scheduled for today, pinned first. */
export function todayOpenPreview(
  tasks: WorkTask[],
  todayKey: string,
  limit?: number
): WorkTask[] {
  const list = tasks
    .filter((t) => t.status === "open" && t.workDate === todayKey)
    .sort(taskSort);
  if (limit === undefined || limit <= 0) return list;
  return list.slice(0, limit);
}

/** Consecutive calendar days (ending today) with at least one completion. */
export function completionStreak(tasks: WorkTask[], todayKey: string): number {
  const doneOn = (key: string) =>
    tasks.some(
      (t) => t.status === "done" && localDateKeyFromIso(t.doneAt || t.updatedAt) === key
    );
  let streak = 0;
  let key = todayKey;
  for (let i = 0; i < 400; i++) {
    if (!doneOn(key)) break;
    streak += 1;
    key = shiftDateKey(key, -1);
  }
  return streak;
}

export function kanbanColumns(
  tasks: WorkTask[],
  todayKey: string,
  query: string
): Record<KanbanColumnId, WorkTask[]> {
  const cols: Record<KanbanColumnId, WorkTask[]> = {
    backlog: [],
    today: [],
    scheduled: [],
    overdue: [],
    done: [],
  };
  for (const t of tasks) {
    if (!matchesTaskQuery(t, query)) continue;
    cols[bucketTask(t, todayKey)].push(t);
  }
  for (const id of Object.keys(cols) as KanbanColumnId[]) {
    if (id === "done") {
      cols.done.sort((a, b) => (b.doneAt || b.updatedAt).localeCompare(a.doneAt || a.updatedAt));
      if (!query.trim()) cols.done = cols.done.slice(0, DONE_COLUMN_CAP);
    } else {
      cols[id].sort(taskSort);
    }
  }
  return cols;
}

export function patchForColumnDrop(
  column: KanbanColumnId,
  todayKey: string,
  current?: WorkTask
): Partial<WorkTask> {
  switch (column) {
    case "backlog":
      return { workDate: null, status: "open" };
    case "today":
      return { workDate: todayKey, status: "open" };
    case "scheduled": {
      if (current?.workDate && current.workDate > todayKey && current.status === "open") {
        return { status: "open", workDate: current.workDate };
      }
      return { workDate: shiftDateKey(todayKey, 1), status: "open" };
    }
    case "overdue": {
      if (current?.workDate && current.workDate < todayKey) {
        return { status: "open", workDate: current.workDate };
      }
      return { workDate: shiftDateKey(todayKey, -1), status: "open" };
    }
    case "done":
      return { status: "done" };
  }
}
