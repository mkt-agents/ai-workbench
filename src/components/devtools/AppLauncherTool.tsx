import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  Download,
  FolderOpen,
  Loader2,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  Repeat,
  Rocket,
  Search,
  Ban,
  Tags,
  Trash2,
  Upload,
  X,
  XCircle,
} from "lucide-react";
import { useGlobalStore, normalizeAppPath } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import QuickSelect from "../quickAsk/QuickSelect";
import type { InstalledAppInfo, QuickAppLauncher } from "../../core/types";

/** Extract the executable name from a full path for process detection. */
function exeNameFromPath(path: string): string {
  if (!path) return "";
  const parts = path.split(/[\\/]/);
  const last = parts[parts.length - 1] || "";
  return last.replace(/\.exe$/i, "");
}

/** Truncate a path for display: keep the exe name + parent dir. */
function shortPath(path: string): string {
  if (!path) return "";
  const parts = path.split(/[\\/]/);
  if (parts.length <= 2) return path;
  return `…\\${parts.slice(-2).join("\\")}`;
}

/** Get the display name's first letter for avatar fallback. */
function getInitial(name: string): string {
  if (!name) return "?";
  const trimmed = name.trim();
  // Use first non-whitespace character, uppercase.
  const ch = trimmed.charAt(0);
  return ch.toUpperCase() || "?";
}

/** Scan item icon — shows real exe icon if available, falls back to letter tile. */
function ScanItemIcon({ size = 18, path = "", name = "", icon: backendIcon }: { size?: number; path?: string; name?: string; icon?: string }) {
  const [realIcon, setRealIcon] = useState<string | null>(backendIcon || null);
  const [iconLoading, setIconLoading] = useState(false);

  useEffect(() => {
    if (backendIcon) {
      setRealIcon(backendIcon);
      return;
    }
    if (!path) return;

    let cancelled = false;
    setIconLoading(true);

    import("@tauri-apps/api/core")
      .then(({ invoke }) =>
        invoke<string>("extract_app_icon", { path })
      )
      .then((data) => {
        if (!cancelled) setRealIcon(data);
      })
      .catch(() => {
        if (!cancelled) setRealIcon(null);
      })
      .finally(() => {
        if (!cancelled) setIconLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [path, backendIcon]);

  const initial = getInitial(name);

  if (iconLoading) {
    return (
      <span className="al-scan-icon" style={{ width: size, height: size }}>
        <Loader2 size={size} className="spin" />
      </span>
    );
  }

  if (realIcon) {
    return (
      <span className="al-scan-icon" style={{ width: size, height: size }}>
        <img src={`data:image/png;base64,${realIcon}`} alt={name} draggable={false} />
      </span>
    );
  }

  return (
    <span className="al-scan-icon al-tile" style={{ width: size, height: size, fontSize: size * 0.5 }}>
      {initial}
    </span>
  );
}

/** Module-level icon cache shared across cards (survives re-renders). */
const appIconCache = new Map<string, string>();

/** Default app icon — tries to load the real exe icon, falls back to initial letter. */
function AppIcon({ size = 22, path = "", name = "" }: { size?: number; path?: string; name?: string }) {
  const initial = getInitial(name);
  const [realIcon, setRealIcon] = useState<string | null>(() => appIconCache.get(path) ?? null);
  const [iconLoading, setIconLoading] = useState(false);

  // Try to extract the real icon from the exe file.
  useEffect(() => {
    if (!path || !path.toLowerCase().endsWith(".exe")) return;

    const cached = appIconCache.get(path);
    if (cached) {
      setRealIcon(cached);
      return;
    }

    let cancelled = false;
    setIconLoading(true);

    import("@tauri-apps/api/core")
      .then(({ invoke }) =>
        invoke<string>("extract_app_icon", { path })
      )
      .then((data) => {
        if (!cancelled) {
          appIconCache.set(path, data);
          setRealIcon(data);
        }
      })
      .catch(() => {
        // Icon extraction failed — fall back to initial letter.
        if (!cancelled) setRealIcon(null);
      })
      .finally(() => {
        if (!cancelled) setIconLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [path]);

  if (realIcon) {
    return <img className="al-app-icon-img" src={`data:image/png;base64,${realIcon}`} alt={name} draggable={false} />;
  }
  if (iconLoading) {
    return <Loader2 size={size * 0.6} className="spin al-app-icon-loading" />;
  }
  return <span className="al-tile al-app-icon-letter" style={{ fontSize: size * 0.5 }}>{initial}</span>;
}

interface AppFormData {
  name: string;
  path: string;
  args: string;
  group: string;
  version: string;
}

// List ordering. "custom" keeps the manual (drag) order; the rest auto-sort and
// disable dragging. Persisted to localStorage as a UI preference.
type SortMode = "custom" | "name" | "nameDesc" | "group" | "running";

function AppLauncherTool() {
  const { t } = useTranslation("devtools");
  const confirm = useConfirm();
  const launchers = useGlobalStore((s) => s.quickAppLaunchers);
  const addLauncher = useGlobalStore((s) => s.addQuickAppLauncher);
  const updateLauncher = useGlobalStore((s) => s.updateQuickAppLauncher);
  const deleteLauncher = useGlobalStore((s) => s.deleteQuickAppLauncher);
  const invokeLaunchApp = useGlobalStore((s) => s.invokeLaunchApp);
  const invokeKillApp = useGlobalStore((s) => s.invokeKillApp);
  const invokeGetAppVersion = useGlobalStore((s) => s.invokeGetAppVersion);
  const invokeScanInstalledApps = useGlobalStore((s) => s.invokeScanInstalledApps);
  const reorderLaunchers = useGlobalStore((s) => s.reorderQuickAppLaunchers);

  // Batch check running status for multiple processes in one call.
  // `force` bypasses the backend's ~800ms tasklist cache (after kills).
  const invokeCheckAppsRunning = useCallback(async (names: string[], force = false) => {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<boolean[]>("check_apps_running", { processNames: names, force });
  }, []);

  // Scan state
  const [scanDialogOpen, setScanDialogOpen] = useState(false);
  const [scanResults, setScanResults] = useState<InstalledAppInfo[]>([]);
  const [scanLoading, setScanLoading] = useState(false);
  const [scanError, setScanError] = useState<string | null>(null);
  const [selectedApps, setSelectedApps] = useState<Set<string>>(new Set());
  const [scanSearch, setScanSearch] = useState("");

  const [search, setSearch] = useState("");
  const [showForm, setShowForm] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<AppFormData>({ name: "", path: "", args: "", group: "", version: "" });
  const [formError, setFormError] = useState<string | null>(null);
  const [runningPids, setRunningPids] = useState<Set<string>>(new Set());
  const [statusLoading, setStatusLoading] = useState<Set<string>>(new Set());
  const [launching, setLaunching] = useState<Set<string>>(new Set());
  const [killing, setKilling] = useState<Set<string>>(new Set());
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [autoRefresh, setAutoRefresh] = useState(false);
  const [dragId, setDragId] = useState<string | null>(null);
  const [dragOverId, setDragOverId] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [groupFilter, setGroupFilter] = useState<string>('');
  // Which toolbar popover is open — group filter, sort, and the batch-group
  // picker share one slot so opening one closes the others.
  const [openPop, setOpenPop] = useState<"" | "group" | "sort" | "batchGroup">("");
  const [sortBy, setSortBy] = useState<SortMode>(() => {
    const v = typeof localStorage !== "undefined" ? localStorage.getItem("al-sort") : null;
    return v === "name" || v === "nameDesc" || v === "group" || v === "running" ? v : "custom";
  });
  // User-defined group names (the canonical list, so empty groups can exist).
  const [groups, setGroups] = useState<string[]>(() => {
    try {
      const raw = typeof localStorage !== "undefined" ? localStorage.getItem("al-groups") : null;
      const arr = raw ? JSON.parse(raw) : [];
      return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === "string") : [];
    } catch {
      return [];
    }
  });
  const [groupManagerOpen, setGroupManagerOpen] = useState(false);
  const [newGroup, setNewGroup] = useState("");
  const [editingGroup, setEditingGroup] = useState<{ name: string; value: string } | null>(null);
  const autoRefreshRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const toolbarRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    try { localStorage.setItem("al-sort", sortBy); } catch { /* storage disabled */ }
  }, [sortBy]);

  useEffect(() => {
    try { localStorage.setItem("al-groups", JSON.stringify(groups)); } catch { /* storage disabled */ }
  }, [groups]);

  // Close the toolbars' popovers on outside click / Escape.
  useEffect(() => {
    if (!openPop) return;
    const onDown = (e: MouseEvent) => {
      if (!toolbarRef.current?.contains(e.target as Node)) setOpenPop("");
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpenPop("");
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [openPop]);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const cancelled = useRef(false);
  const runningPidsRef = useRef<Set<string>>(new Set());

  // Refresh running status for all launchers.
  const refreshStatus = useCallback(async (force = false) => {
    if (launchers.length === 0) {
      runningPidsRef.current = new Set();
      setRunningPids(new Set());
      setStatusLoading(new Set());
      return;
    }
    // Keep running cards showing their live state; only unknown cards spin.
    setStatusLoading(new Set(launchers.filter((l) => !runningPidsRef.current.has(l.id)).map((l) => l.id)));
    try {
      const processNames = launchers.map((l) => exeNameFromPath(l.path));
      const results = await invokeCheckAppsRunning(processNames, force);
      const nextRunning = new Set<string>();
      launchers.forEach((l, i) => {
        if (results[i]) nextRunning.add(l.id);
      });
      if (!cancelled.current) {
        runningPidsRef.current = nextRunning;
        setRunningPids(nextRunning);
        setStatusLoading(new Set());
      }
    } catch {
      if (!cancelled.current) setStatusLoading(new Set());
    }
  }, [launchers, invokeCheckAppsRunning]);

  // After a launch, poll for a few seconds — slow apps pass the ShellExecute
  // return long before their process actually appears in tasklist.
  const pollRunningAfterLaunch = useCallback(async (id: string) => {
    for (const delay of [800, 1500, 2500, 3500]) {
      await new Promise((r) => setTimeout(r, delay));
      if (cancelled.current) return;
      const l = launchers.find((x) => x.id === id);
      if (!l) return;
      try {
        const results = await invokeCheckAppsRunning([exeNameFromPath(l.path)]);
        if (cancelled.current) return;
        if (results[0]) {
          runningPidsRef.current = new Set(runningPidsRef.current).add(id);
          setRunningPids(runningPidsRef.current);
          setStatusLoading((prev) => { const n = new Set(prev); n.delete(id); return n; });
          return;
        }
      } catch { /* keep polling */ }
    }
    // Gave up: clear this card's spinner without touching running state.
    if (!cancelled.current) {
      setStatusLoading((prev) => { const n = new Set(prev); n.delete(id); return n; });
    }
  }, [launchers, invokeCheckAppsRunning]);

  useEffect(() => {
    cancelled.current = false;
    refreshStatus();
    return () => {
      cancelled.current = true;
    };
  }, [refreshStatus]);

  // Auto-hide status message.
  useEffect(() => {
    if (!message) return;
    const timer = setTimeout(() => setMessage(null), 4000);
    return () => clearTimeout(timer);
  }, [message]);

  const filteredLaunchers = useMemo(() => {
    let result = launchers;
    const q = search.toLowerCase().trim();
    if (q) {
      result = result.filter(
        (l) =>
          l.name.toLowerCase().includes(q) ||
          l.path.toLowerCase().includes(q)
      );
    }
    if (groupFilter) {
      result = result.filter((l) => l.group === groupFilter);
    }
    if (sortBy === "custom") return result;
    const byName = (a: QuickAppLauncher, b: QuickAppLauncher) => a.name.localeCompare(b.name, "zh");
    const arr = [...result];
    if (sortBy === "name") arr.sort(byName);
    else if (sortBy === "nameDesc") arr.sort((a, b) => byName(b, a));
    else if (sortBy === "group")
      arr.sort((a, b) => (a.group || "").localeCompare(b.group || "", "zh") || byName(a, b));
    else if (sortBy === "running")
      arr.sort(
        (a, b) =>
          (runningPids.has(b.id) ? 1 : 0) - (runningPids.has(a.id) ? 1 : 0) || byName(a, b)
      );
    return arr;
  }, [launchers, search, groupFilter, sortBy, runningPids]);

  // All groups to show in filters/pickers: the user-defined list plus any group
  // still used by an app (so a stray/imported group is never invisible).
  const availableGroups = useMemo(() => {
    const set = new Set<string>(groups);
    launchers.forEach((l) => {
      if (l.group) set.add(l.group);
    });
    return Array.from(set).sort((a, b) => a.localeCompare(b, "zh"));
  }, [groups, launchers]);

  // Ensure a group name is in the managed list (used on save / batch / import).
  const ensureGroup = useCallback((name?: string) => {
    const n = name?.trim();
    if (!n) return;
    setGroups((prev) => (prev.some((g) => g.toLowerCase() === n.toLowerCase()) ? prev : [...prev, n]));
  }, []);

  const addGroup = useCallback((name: string) => {
    const n = name.trim();
    if (!n) return;
    setGroups((prev) => (prev.some((g) => g.toLowerCase() === n.toLowerCase()) ? prev : [...prev, n]));
  }, []);

  // Rename a group everywhere: the managed list and every app using it.
  const renameGroup = useCallback(async (oldName: string, rawNew: string) => {
    const newName = rawNew.trim();
    if (!newName || newName === oldName) return;
    setGroups((prev) => {
      const exists = prev.some((g) => g.toLowerCase() === newName.toLowerCase());
      const base = prev.filter((g) => g !== oldName);
      return exists ? base : [...base, newName];
    });
    for (const l of launchers.filter((x) => (x.group || "") === oldName)) {
      await updateLauncher(l.id, { group: newName });
    }
    if (groupFilter === oldName) setGroupFilter(newName);
    setMessage({ type: "success", text: t("appLauncher.groupRenamed") });
  }, [launchers, groupFilter, t, updateLauncher]);

  // Delete a group: drop it from the list and clear it on every app using it.
  const deleteGroup = useCallback(async (name: string) => {
    setGroups((prev) => prev.filter((g) => g !== name));
    for (const l of launchers.filter((x) => (x.group || "") === name)) {
      await updateLauncher(l.id, { group: undefined });
    }
    if (groupFilter === name) setGroupFilter("");
    setMessage({ type: "success", text: t("appLauncher.groupDeleted") });
  }, [launchers, groupFilter, t, updateLauncher]);

  // Assign a group to all selected apps (empty string clears it).
  const batchSetGroup = useCallback(async (name: string) => {
    const targets = launchers.filter((l) => selectedIds.has(l.id));
    if (targets.length === 0) return;
    const group = name.trim() || undefined;
    if (group) ensureGroup(group);
    for (const l of targets) {
      if ((l.group || "") !== (group || "")) await updateLauncher(l.id, { group });
    }
    setOpenPop("");
    setSelectedIds(new Set());
    setMessage({ type: "success", text: t("appLauncher.batchGroupDone", { count: targets.length }) });
  }, [launchers, selectedIds, ensureGroup, updateLauncher, t]);

  const runningCount = useMemo(() => runningPids.size, [runningPids]);

  // Select all / deselect all for the app card list
  const allCardSelected = useMemo(() => {
    if (filteredLaunchers.length === 0) return false;
    return filteredLaunchers.every((l) => selectedIds.has(l.id));
  }, [filteredLaunchers, selectedIds]);

  const toggleAllCards = () => {
    if (allCardSelected) {
      setSelectedIds(new Set());
    } else {
      setSelectedIds(new Set(filteredLaunchers.map((l) => l.id)));
    }
  };

  // ── Scan handlers ──
  const openScanDialog = async () => {
    setScanDialogOpen(true);
    setScanLoading(true);
    setScanError(null);
    setScanResults([]);
    setSelectedApps(new Set());
    setScanSearch("");
    try {
      const results = await invokeScanInstalledApps();
      if (!cancelled.current) {
        setScanResults(results);
      }
    } catch (e) {
      if (!cancelled.current) {
        setScanError(String(e));
      }
    } finally {
      if (!cancelled.current) {
        setScanLoading(false);
      }
    }
  };

  const closeScanDialog = () => {
    setScanDialogOpen(false);
    setScanResults([]);
    setSelectedApps(new Set());
    setScanError(null);
    setScanSearch("");
  };

  const toggleAppSelection = (exePath: string) => {
    setSelectedApps((prev) => {
      const next = new Set(prev);
      if (next.has(exePath)) next.delete(exePath);
      else next.add(exePath);
      return next;
    });
  };

  const addSelectedApps = async () => {
    if (selectedApps.size === 0) return;
    const savedPaths = new Set(launchers.map((l) => normalizeAppPath(l.path)));
    const toAdd = scanResults.filter((app) => selectedApps.has(app.exe_path));
    let added = 0;
    let skipped = 0;
    for (const app of toAdd) {
      if (savedPaths.has(normalizeAppPath(app.exe_path))) { skipped++; continue; }
      try {
        const name = app.display_name || exeNameFromPath(app.exe_path);
        // Registry-scanned apps carry DisplayVersion; Start-Menu/portable ones don't,
        // so fall back to reading the exe's own file version.
        let version = app.version || "";
        if (!version) {
          try { version = (await invokeGetAppVersion(app.exe_path)) || ""; } catch { version = ""; }
        }
        await addLauncher({ name, path: app.exe_path, args: "", version: version || undefined, order: 0 });
        savedPaths.add(normalizeAppPath(app.exe_path));
        added++;
      } catch { /* skip */ }
    }
    const parts = [t("appLauncher.scan.added", { count: added })];
    if (skipped > 0) parts.push(t("appLauncher.skippedDuplicates", { count: skipped }));
    setMessage({ type: "success", text: parts.join(" / ") });
    closeScanDialog();
    // Delay status refresh to avoid blocking the UI during batch add
    setTimeout(() => { void refreshStatus(); }, 100);
  };

  const filteredScanResults = useMemo(() => {
    // Applications already saved (by normalized path) don't need re-adding.
    const savedPaths = new Set(launchers.map((l) => normalizeAppPath(l.path)));
    const q = scanSearch.toLowerCase().trim();
    return scanResults.filter((app) => {
      if (!app.exe_path || savedPaths.has(normalizeAppPath(app.exe_path))) return false;
      if (!q) return true;
      return (
        app.display_name.toLowerCase().includes(q) ||
        app.exe_path.toLowerCase().includes(q) ||
        (app.publisher && app.publisher.toLowerCase().includes(q))
      );
    });
  }, [scanResults, scanSearch, launchers]);

  const allFilteredSelected = useMemo(() => {
    if (filteredScanResults.length === 0) return false;
    return filteredScanResults.every((app) => selectedApps.has(app.exe_path));
  }, [filteredScanResults, selectedApps]);

  const toggleAllFiltered = () => {
    if (allFilteredSelected) {
      setSelectedApps((prev) => {
        const next = new Set(prev);
        for (const app of filteredScanResults) next.delete(app.exe_path);
        return next;
      });
    } else {
      setSelectedApps((prev) => {
        const next = new Set(prev);
        for (const app of filteredScanResults) next.add(app.exe_path);
        return next;
      });
    }
  };

  const openAddForm = () => {
    setEditingId(null);
    setForm({ name: "", path: "", args: "", group: "", version: "" });
    setFormError(null);
    setShowForm(true);
  };

  const openEditForm = (l: QuickAppLauncher) => {
    setEditingId(l.id);
    setForm({ name: l.name, path: l.path, args: l.args, group: l.group || "", version: l.version || "" });
    setFormError(null);
    setShowForm(true);
  };

  const handleSave = async () => {
    const name = form.name.trim();
    const path = form.path.trim();
    if (!name) { setFormError(t("appLauncher.error.nameRequired")); return; }
    if (!path) { setFormError(t("appLauncher.error.pathRequired")); return; }
    // Prefer a user-entered version; otherwise auto-detect from the exe file.
    let version = form.version.trim();
    if (!version) {
      try { version = (await invokeGetAppVersion(path)) || ""; } catch { version = ""; }
    }
    try {
      if (editingId) {
        await updateLauncher(editingId, { name, path, args: form.args.trim(), group: form.group.trim() || undefined, version: version || undefined });
        setMessage({ type: "success", text: t("appLauncher.updated") });
      } else {
        await addLauncher({ name, path, args: form.args.trim(), group: form.group.trim() || undefined, version: version || undefined, order: 0 });
        setMessage({ type: "success", text: t("appLauncher.added") });
      }
      ensureGroup(form.group);
      setShowForm(false);
      setEditingId(null);
      setForm({ name: "", path: "", args: "", group: "", version: "" });
      setFormError(null);
    } catch (e) {
      const msg = String(e);
      setFormError(msg.startsWith("DUPLICATE_APP:") ? t("appLauncher.error.dupPath") : msg);
    }
  };

  const handleDelete = async (l: QuickAppLauncher) => {
    const ok = await confirm({
      title: t("appLauncher.deleteTitle"),
      message: t("appLauncher.deleteConfirm", { name: l.name }),
      confirmText: t("appLauncher.delete"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await deleteLauncher(l.id);
      setMessage({ type: "success", text: t("appLauncher.deleted") });
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    }
  };

  const handleLaunch = async (l: QuickAppLauncher) => {
    setLaunching((prev) => new Set(prev).add(l.id));
    setStatusLoading((prev) => new Set(prev).add(l.id));
    try {
      const result = await invokeLaunchApp(l.path, l.args || undefined);
      setMessage({ type: "success", text: result });
      void pollRunningAfterLaunch(l.id);
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
      setStatusLoading((prev) => { const n = new Set(prev); n.delete(l.id); return n; });
    } finally {
      setLaunching((prev) => { const n = new Set(prev); n.delete(l.id); return n; });
    }
  };

  const handleKill = async (l: QuickAppLauncher) => {
    const exeName = exeNameFromPath(l.path);
    const ok = await confirm({
      title: t("appLauncher.killTitle"),
      message: t("appLauncher.killConfirm", { name: exeName }),
      confirmText: t("appLauncher.kill"),
      icon: "danger",
    });
    if (!ok) return;
    setKilling((prev) => new Set(prev).add(l.id));
    try {
      const result = await invokeKillApp(exeName);
      setMessage({ type: "success", text: result });
      await refreshStatus(true);
    } catch (e) {
      setMessage({ type: "error", text: String(e) });
    } finally {
      setKilling((prev) => { const n = new Set(prev); n.delete(l.id); return n; });
    }
  };

  // Auto-refresh running status every 5s when enabled.
  useEffect(() => {
    if (!autoRefresh) {
      if (autoRefreshRef.current) {
        clearInterval(autoRefreshRef.current);
        autoRefreshRef.current = null;
      }
      return;
    }
    autoRefreshRef.current = setInterval(() => {
      void refreshStatus();
    }, 5000);
    return () => {
      if (autoRefreshRef.current) {
        clearInterval(autoRefreshRef.current);
        autoRefreshRef.current = null;
      }
    };
  }, [autoRefresh, refreshStatus]);

  // ── Browse file ──
  const handleBrowseFile = async () => {
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      const path = await invoke<string>('browse_app_file');
      if (path) {
        setForm((f) => ({
          ...f,
          path,
          name: f.name || exeNameFromPath(path),
        }));
      }
    } catch (e) {
      setFormError(String(e));
    }
  };

  // ── Drag & drop reorder ──
  const handleDragStart = (id: string) => {
    setDragId(id);
  };

  const handleDragOver = (e: React.DragEvent, id: string) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    if (dragId && dragId !== id) {
      setDragOverId(id);
    }
  };

  const handleDragLeave = () => {
    setDragOverId(null);
  };

  const handleDrop = async (targetId: string) => {
    if (sortBy !== "custom") return;
    if (!dragId || dragId === targetId) {
      setDragId(null);
      setDragOverId(null);
      return;
    }
    const currentOrder = launchers.map((l) => l.id);
    const fromIndex = currentOrder.indexOf(dragId);
    const toIndex = currentOrder.indexOf(targetId);
    if (fromIndex < 0 || toIndex < 0) return;
    const newOrder = [...currentOrder];
    const [moved] = newOrder.splice(fromIndex, 1);
    newOrder.splice(toIndex, 0, moved);
    await reorderLaunchers(newOrder);
    setDragId(null);
    setDragOverId(null);
  };

  const handleDragEnd = () => {
    setDragId(null);
    setDragOverId(null);
  };

  // ── Export / Import ──
  const handleExport = () => {
    // Only user-meaningful fields; import regenerates id/order/timestamps.
    const data = JSON.stringify(
      launchers.map((l) => ({ name: l.name, path: l.path, args: l.args, group: l.group ?? "", version: l.version ?? "" })),
      null, 2
    );
    const blob = new Blob([data], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'quick-app-launchers.json';
    a.click();
    URL.revokeObjectURL(url);
  };

  const handleImportFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      const text = await file.text();
      const data = JSON.parse(text);
      if (!Array.isArray(data)) throw new Error('Invalid format');
      const savedPaths = new Set(launchers.map((l) => normalizeAppPath(l.path)));
      let added = 0;
      let skipped = 0;
      for (const item of data) {
        // Accept both camelCase (our export) and snake_case (legacy/external).
        const name = item.name;
        const path = item.path;
        const args = item.args ?? item.cmd_args ?? '';
        const group = item.group ?? '';
        const version = item.version ?? '';
        if (!name || !path) continue;
        if (savedPaths.has(normalizeAppPath(path))) { skipped++; continue; }
        try {
          await addLauncher({ name, path, args, group: group || undefined, version: version || undefined, order: 0 });
          if (group) ensureGroup(group);
          savedPaths.add(normalizeAppPath(path));
          added++;
        } catch { skipped++; }
      }
      const parts = [t('appLauncher.imported', { count: added })];
      if (skipped > 0) parts.push(t('appLauncher.skippedDuplicates', { count: skipped }));
      setMessage({ type: 'success', text: parts.join(' / ') });
      setTimeout(() => { void refreshStatus(); }, 100);
    } catch (err) {
      setMessage({ type: 'error', text: String(err) });
    }
    e.target.value = '';
  };

  // ── Batch operations ──
  const toggleSelect = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const handleBatchLaunch = async () => {
    const targets = launchers.filter((l) => selectedIds.has(l.id) && !runningPids.has(l.id));
    if (targets.length === 0) return;
    let launched = 0;
    let failed = 0;
    const launchedIds: string[] = [];
    // Sequential with a small gap so ShellExecute isn't hammered in parallel.
    for (const l of targets) {
      try {
        await invokeLaunchApp(l.path, l.args || undefined);
        launched++;
        launchedIds.push(l.id);
        setStatusLoading((prev) => new Set(prev).add(l.id));
      } catch {
        failed++;
      }
      await new Promise((r) => setTimeout(r, 150));
    }
    setSelectedIds(new Set());
    const parts = [t("appLauncher.batchLaunchDone", { count: launched })];
    if (failed > 0) parts.push(t("appLauncher.batchLaunchFailed", { count: failed }));
    setMessage({ type: failed > 0 ? "error" : "success", text: parts.join(" / ") });
    launchedIds.forEach((id) => { void pollRunningAfterLaunch(id); });
  };

  const handleBatchDelete = async () => {
    if (selectedIds.size === 0) return;
    const ok = await confirm({
      title: t('appLauncher.deleteTitle'),
      message: t('appLauncher.batchDeleteConfirm', { count: selectedIds.size }),
      confirmText: t('appLauncher.delete'),
      icon: 'danger',
    });
    if (!ok) return;
    for (const id of selectedIds) {
      await deleteLauncher(id);
    }
    setSelectedIds(new Set());
    setMessage({ type: 'success', text: t('appLauncher.deleted') });
  };

  // Close only the selected apps that are currently running.
  const handleBatchKill = async () => {
    const targets = launchers.filter((l) => selectedIds.has(l.id) && runningPids.has(l.id));
    if (targets.length === 0) return;
    const ok = await confirm({
      title: t("appLauncher.killAllTitle"),
      message: t("appLauncher.batchKillConfirm", { count: targets.length }),
      confirmText: t("appLauncher.kill"),
      icon: "danger",
    });
    if (!ok) return;
    const ids = targets.map((l) => l.id);
    setKilling((prev) => {
      const next = new Set(prev);
      ids.forEach((id) => next.add(id));
      return next;
    });
    const results = await Promise.allSettled(
      targets.map((l) => invokeKillApp(exeNameFromPath(l.path)))
    );
    const failed = results.filter((r) => r.status === "rejected").length;
    await refreshStatus(true);
    setKilling((prev) => {
      const next = new Set(prev);
      ids.forEach((id) => next.delete(id));
      return next;
    });
    setSelectedIds(new Set());
    const parts = [t("appLauncher.batchKillDone", { count: targets.length - failed })];
    if (failed > 0) parts.push(t("appLauncher.batchKillFailed", { count: failed }));
    setMessage({ type: failed > 0 ? "error" : "success", text: parts.join(" / ") });
  };

  const selectedRunningCount = launchers.filter((l) => selectedIds.has(l.id) && runningPids.has(l.id)).length;

  return (
    <div className="devtools-tool app-launcher-tool">
      {/* ── Toolbar ── */}
      <div className="al-toolbar" ref={toolbarRef}>
        <div className="al-toolbar-left">
          <button type="button" className="al-btn al-btn-primary" onClick={openScanDialog} title={t("appLauncher.add")}>
            <Plus size={14} />
            <span>{t("appLauncher.add")}</span>
          </button>
          <button
            type="button"
            className="al-btn"
            onClick={() => setGroupManagerOpen(true)}
            title={t("appLauncher.groupManager.title")}
          >
            <Tags size={14} />
            <span>{t("appLauncher.groupManager.title")}</span>
          </button>
        </div>
        <div className="al-toolbar-right">
          {selectedIds.size > 0 ? (
            <>
              <span className="al-selected-count">{t("appLauncher.selected", { count: selectedIds.size })}</span>
              <button type="button" className="al-btn" onClick={handleBatchLaunch} title={t("appLauncher.batchLaunch")}>
                <Play size={14} />
                <span>{t("appLauncher.batchLaunch")}</span>
              </button>
              <button
                type="button"
                className="al-btn al-btn-danger-outline"
                onClick={handleBatchKill}
                disabled={selectedRunningCount === 0}
                title={t("appLauncher.batchKill")}
              >
                <Ban size={14} />
                <span>{t("appLauncher.batchKill")}</span>
              </button>
              <QuickSelect
                className="al-batch-group-select"
                value=""
                placeholder={t("appLauncher.batchSetGroup")}
                title={t("appLauncher.batchSetGroup")}
                options={[{ value: "", label: t("appLauncher.ungrouped") }, ...availableGroups.map((g) => ({ value: g, label: g }))]}
                onChange={batchSetGroup}
                alignLeft
                open={openPop === "batchGroup"}
                onToggle={() => setOpenPop((v) => (v === "batchGroup" ? "" : "batchGroup"))}
              />
              <button type="button" className="al-btn al-btn-danger-outline" onClick={handleBatchDelete} title={t("appLauncher.batchDelete")}>
                <Trash2 size={14} />
                <span>{t("appLauncher.batchDelete")}</span>
              </button>
              <button type="button" className="al-btn al-btn-ghost" onClick={() => setSelectedIds(new Set())} title={t("appLauncher.deselectAll")}>
                <X size={14} />
              </button>
            </>
          ) : (
            <>
              {availableGroups.length > 0 && (
                <QuickSelect
                  className="al-group-select"
                  value={groupFilter}
                  options={[{ value: "", label: t("appLauncher.allGroups") }, ...availableGroups.map((g) => ({ value: g, label: g }))]}
                  onChange={setGroupFilter}
                  alignLeft
                  open={openPop === "group"}
                  onToggle={() => setOpenPop((v) => (v === "group" ? "" : "group"))}
                />
              )}
              {launchers.length > 0 && (
                <QuickSelect
                  className="al-sort-select"
                  value={sortBy}
                  title={t("appLauncher.sort.title")}
                  options={[
                    { value: "custom", label: t("appLauncher.sort.custom"), short: t("appLauncher.sort.customShort") },
                    { value: "name", label: t("appLauncher.sort.name") },
                    { value: "nameDesc", label: t("appLauncher.sort.nameDesc") },
                    { value: "group", label: t("appLauncher.sort.group") },
                    { value: "running", label: t("appLauncher.sort.running") },
                  ]}
                  onChange={(v) => setSortBy(v as SortMode)}
                  alignLeft
                  open={openPop === "sort"}
                  onToggle={() => setOpenPop((v) => (v === "sort" ? "" : "sort"))}
                />
              )}
              {launchers.length > 0 && (
                <div className="al-search">
                  <Search size={14} className="al-search-icon" />
                  <input
                    className="al-search-input"
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                    placeholder={t("appLauncher.searchPlaceholder")}
                    spellCheck={false}
                  />
                </div>
              )}
              {runningCount > 0 && (
                <span className="al-running-badge">
                  <span className="al-running-dot" />
                  {runningCount} {t("appLauncher.running")}
                </span>
              )}
              <div className="al-icon-group">
                <div className="al-divider" />
                <button type="button" className="al-btn al-btn-icon" onClick={() => { void refreshStatus(true); }} title={t("appLauncher.refreshStatus")}>
                  <RefreshCw size={14} />
                </button>
                <button
                  type="button"
                  className={'al-btn al-btn-icon ' + (autoRefresh ? 'al-btn-active' : '')}
                  onClick={() => setAutoRefresh((v) => !v)}
                  title={t("appLauncher.autoRefresh")}
                >
                  <Repeat size={14} className={autoRefresh ? 'spin' : ''} />
                </button>
                <button type="button" className="al-btn al-btn-icon" onClick={handleExport} title={t("appLauncher.export")}>
                  <Upload size={14} />
                </button>
                <button type="button" className="al-btn al-btn-icon" onClick={() => fileInputRef.current?.click()} title={t("appLauncher.import")}>
                  <Download size={14} />
                </button>
              </div>
              <input
                ref={fileInputRef}
                type="file"
                accept=".json"
                style={{ display: 'none' }}
                onChange={handleImportFile}
              />
            </>
          )}
        </div>
      </div>

      {/* ── Status toast ── */}
      {message && (
        <div className={`al-toast ${message.type}`}>
          {message.type === "success" ? <Check size={14} /> : <XCircle size={14} />}
          <span>{message.text}</span>
        </div>
      )}

      {/* ── Content ── */}
      {launchers.length === 0 ? (
        <div className="al-empty">
          <div className="al-empty-icon-wrap">
            <Rocket size={28} />
          </div>
          <span className="al-empty-title">{t("appLauncher.empty")}</span>
          <span className="al-empty-hint">{t("appLauncher.emptyHint")}</span>
          <div className="al-empty-actions">
            <button type="button" className="al-btn al-btn-primary" onClick={openScanDialog}>
              <Download size={14} />
              <span>{t("appLauncher.scan.title")}</span>
            </button>
            <button type="button" className="al-btn" onClick={openAddForm}>
              <Plus size={14} />
              <span>{t("appLauncher.addFirst")}</span>
            </button>
          </div>
        </div>
      ) : filteredLaunchers.length === 0 ? (
        <div className="al-empty al-empty-sm">
          <Search size={22} className="al-empty-icon" />
          <span>{t("appLauncher.noMatch")}</span>
        </div>
      ) : (
        <>
          <div className="al-grid-head">
            <label className="al-select-all">
              <input
                type="checkbox"
                checked={allCardSelected}
                onChange={toggleAllCards}
              />
              <span>{allCardSelected ? t("appLauncher.deselectAll") : t("appLauncher.selectAll")}</span>
            </label>
            <span className="al-count">{filteredLaunchers.length} / {launchers.length}</span>
          </div>
        <div className="al-grid">
          {filteredLaunchers.map((l) => {
            const isRunning = runningPids.has(l.id);
            const isLaunching = launching.has(l.id);
            const isKilling = killing.has(l.id);
            const isStatusLoading = statusLoading.has(l.id);
            return (
              <div
                key={l.id}
                className={'al-card ' + (isRunning ? 'is-running' : '') + (selectedIds.has(l.id) ? ' is-selected' : '') + (dragId === l.id ? ' dragging' : '') + (dragOverId === l.id && dragId !== l.id ? ' drag-over' : '')}
                draggable={sortBy === "custom"}
                onDragStart={() => handleDragStart(l.id)}
                onDragOver={(e) => handleDragOver(e, l.id)}
                onDragLeave={handleDragLeave}
                onDrop={() => handleDrop(l.id)}
                onDragEnd={handleDragEnd}
              >
                <div className="al-card-top">
                  <div className="al-card-checkbox">
                    <input
                      type="checkbox"
                      checked={selectedIds.has(l.id)}
                      onChange={() => toggleSelect(l.id)}
                      onClick={(e) => e.stopPropagation()}
                    />
                  </div>
                  <div className="al-card-icon">
                    {isStatusLoading ? (
                      <Loader2 size={20} className="spin" />
                    ) : (
                      <AppIcon size={20} path={l.path} name={l.name} />
                    )}
                  </div>
                  <div className="al-card-info">
                    <div className="al-card-name-row">
                      <span className="al-card-name">{l.name}</span>
                      {l.version && (
                        <span className="al-card-version" title={t("appLauncher.version")}>{l.version}</span>
                      )}
                      {l.group && (
                        <span className="al-card-group">
                          <span className="al-group-badge">{l.group}</span>
                        </span>
                      )}
                    </div>
                    <span className="al-card-path" title={l.path}>
                      <FolderOpen size={10} />
                      {shortPath(l.path)}
                    </span>
                  </div>
                  {isRunning ? (
                    <span className="al-badge al-badge-green">
                      <span className="al-badge-dot" />
                      {t("appLauncher.running")}
                    </span>
                  ) : (
                    <span className="al-status-dot" title={t("appLauncher.notRunning")} />
                  )}
                </div>
                {l.args && (
                  <div className="al-card-args">
                    <code>{l.args}</code>
                  </div>
                )}
                <div className="al-card-footer">
                  <button
                    type="button"
                    className={`al-action al-action-launch ${isRunning ? "is-active" : ""}`}
                    onClick={() => handleLaunch(l)}
                    disabled={isLaunching || isKilling || isRunning}
                    title={t("appLauncher.launch")}
                  >
                    {isLaunching ? <Loader2 size={14} className="spin" /> : <Play size={14} />}
                    <span>{t("appLauncher.launch")}</span>
                  </button>
                  <button
                    type="button"
                    className={`al-action al-action-kill ${isKilling ? "is-active" : ""}`}
                    onClick={() => handleKill(l)}
                    disabled={isLaunching || isKilling || !isRunning}
                    title={t("appLauncher.kill")}
                  >
                    {isKilling ? <Loader2 size={14} className="spin" /> : <Ban size={14} />}
                    <span>{t("appLauncher.kill")}</span>
                  </button>
                  <div className="al-card-spacer" />
                  <button type="button" className="al-icon-btn" onClick={() => openEditForm(l)} title={t("appLauncher.edit")}>
                    <Pencil size={14} />
                  </button>
                  <button type="button" className="al-icon-btn al-icon-btn-danger" onClick={() => handleDelete(l)} title={t("appLauncher.delete")}>
                    <Trash2 size={14} />
                  </button>
                </div>
              </div>
            );
          })}
        </div>
        </>
      )}

      {/* ── Add/Edit Modal ── */}
      {showForm && (
        <div className="modal-overlay" onClick={() => setShowForm(false)}>
          <div className="modal-card" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">
                {editingId ? t("appLauncher.editTitle") : t("appLauncher.addTitle")}
              </span>
              <button type="button" className="modal-close-btn" onClick={() => setShowForm(false)} aria-label={t("common.clearSearch")}>
                <XCircle size={16} />
              </button>
            </div>
            <div className="modal-body">
              <div className="form-row">
                <label className="form-label">{t("appLauncher.formName")}</label>
                <input type="text" className="devtools-input" value={form.name}
                  onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
                  placeholder={t("appLauncher.namePlaceholder")} autoFocus />
              </div>
              <div className="form-row">
                <label className="form-label">{t("appLauncher.formPath")}</label>
                <div className="al-path-input">
                  <input type="text" className="devtools-input" value={form.path}
                    onChange={(e) => setForm((f) => ({ ...f, path: e.target.value }))}
                    placeholder={t("appLauncher.pathPlaceholder")} spellCheck={false} />
                  <button type="button" className="al-btn" onClick={handleBrowseFile} title={t("appLauncher.browse")}>
                    <FolderOpen size={14} />
                    <span>{t("appLauncher.browse")}</span>
                  </button>
                </div>
              </div>
              <div className="form-row">
                <label className="form-label">{t("appLauncher.version")}</label>
                <input type="text" className="devtools-input" value={form.version}
                  onChange={(e) => setForm((f) => ({ ...f, version: e.target.value }))}
                  placeholder={t("appLauncher.versionPlaceholder")} spellCheck={false} />
              </div>
              <div className="form-row">
                <label className="form-label">{t("appLauncher.group")}</label>
                <input type="text" className="devtools-input" value={form.group}
                  onChange={(e) => setForm((f) => ({ ...f, group: e.target.value }))}
                  list="al-group-options"
                  placeholder={t("appLauncher.groupPlaceholder")} spellCheck={false} />
                <datalist id="al-group-options">
                  {availableGroups.map((g) => <option key={g} value={g} />)}
                </datalist>
              </div>
              <div className="form-row">
                <label className="form-label">{t("appLauncher.formArgs")}</label>
                <input type="text" className="devtools-input" value={form.args}
                  onChange={(e) => setForm((f) => ({ ...f, args: e.target.value }))}
                  placeholder={t("appLauncher.argsPlaceholder")} spellCheck={false} />
              </div>
              {formError && (
                <div className="form-error"><XCircle size={14} /><span>{formError}</span></div>
              )}
            </div>
            <div className="modal-footer">
              <button type="button" className="btn btn-secondary" onClick={() => setShowForm(false)}>{t("appLauncher.cancel")}</button>
              <button type="button" className="btn btn-primary" onClick={handleSave}>{t("appLauncher.save")}</button>
            </div>
          </div>
        </div>
      )}

      {/* ── Scan Modal ── */}
      {scanDialogOpen && (
        <div className="modal-overlay" onClick={closeScanDialog}>
          <div className="modal-card modal-card-wide" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">{t("appLauncher.scan.title")}</span>
              <button type="button" className="modal-close-btn" onClick={closeScanDialog} aria-label={t("common.clearSearch")}>
                <XCircle size={16} />
              </button>
            </div>
            <div className="modal-body">
              {scanLoading ? (
                <div className="al-scan-loading">
                  <Loader2 size={28} className="spin" />
                  <span>{t("appLauncher.scan.scanning")}</span>
                </div>
              ) : scanError ? (
                <div className="al-empty al-empty-sm" style={{ padding: "24px" }}>
                  <XCircle size={22} className="al-empty-icon" />
                  <span>{scanError}</span>
                </div>
              ) : scanResults.length === 0 ? (
                <div className="al-empty al-empty-sm" style={{ padding: "24px" }}>
                  <Rocket size={22} className="al-empty-icon" />
                  <span>{t("appLauncher.scan.empty")}</span>
                </div>
              ) : (
                <>
                  <div className="al-scan-toolbar">
                    <div className="al-search" style={{ flex: 1 }}>
                      <Search size={14} className="al-search-icon" />
                      <input className="al-search-input" value={scanSearch}
                        onChange={(e) => setScanSearch(e.target.value)}
                        placeholder={t("appLauncher.scan.searchPlaceholder")} spellCheck={false} />
                    </div>
                    <button
                      type="button"
                      className={`al-btn ${allFilteredSelected ? "al-btn-primary" : ""}`}
                      onClick={toggleAllFiltered}
                      disabled={filteredScanResults.length === 0}
                    >
                      <Check size={13} />
                      <span>{allFilteredSelected ? t("appLauncher.scan.deselectAll") : t("appLauncher.scan.selectAll")}</span>
                    </button>
                  </div>
                  <div className="al-scan-list">
                    {filteredScanResults.map((app) => {
                      const isSelected = selectedApps.has(app.exe_path);
                      return (
                        <div
                          key={app.exe_path}
                          className={`al-scan-item ${isSelected ? "selected" : ""}`}
                          onClick={() => toggleAppSelection(app.exe_path)}
                        >
                          <span className="al-scan-check">
                            {isSelected && <Check size={12} />}
                          </span>
                          <ScanItemIcon
                            path={app.exe_path}
                            name={app.display_name || exeNameFromPath(app.exe_path)}
                            icon={app.icon}
                          />
                          <span className="al-scan-info">
                            <span className="al-scan-name" title={app.exe_path}>
                              {app.display_name || exeNameFromPath(app.exe_path)}
                            </span>
                            <span className="al-scan-meta">
                              {app.publisher && <span className="al-scan-publisher">{app.publisher}</span>}
                              {app.version && <span className="al-scan-version">{app.version}</span>}
                            </span>
                          </span>
                        </div>
                      );
                    })}
                  </div>
                  <div className="al-scan-footer">
                    {t("appLauncher.scan.selected", { selected: selectedApps.size, total: scanResults.length })}
                  </div>
                </>
              )}
            </div>
            <div className="modal-footer">
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => { closeScanDialog(); openAddForm(); }}
              >
                <span>{t("appLauncher.scan.manualAdd")}</span>
              </button>
              <div className="modal-footer-spacer" />
              <button type="button" className="btn btn-secondary" onClick={closeScanDialog}>{t("appLauncher.cancel")}</button>
              <button type="button" className="btn btn-primary" onClick={addSelectedApps} disabled={selectedApps.size === 0}>
                <Download size={14} />
                <span>{t("appLauncher.scan.addSelected", { count: selectedApps.size })}</span>
              </button>
            </div>
          </div>
        </div>
      )}

      {/* ── Group Manager Modal ── */}
      {groupManagerOpen && (
        <div className="modal-overlay" onClick={() => { setGroupManagerOpen(false); setEditingGroup(null); }}>
          <div className="modal-card" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <span className="modal-title">{t("appLauncher.groupManager.title")}</span>
              <button
                type="button"
                className="modal-close-btn"
                onClick={() => { setGroupManagerOpen(false); setEditingGroup(null); }}
                aria-label={t("common.clearSearch")}
              >
                <XCircle size={16} />
              </button>
            </div>
            <div className="modal-body">
              <div className="al-group-add">
                <input
                  type="text"
                  className="devtools-input"
                  value={newGroup}
                  onChange={(e) => setNewGroup(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter" && newGroup.trim()) { addGroup(newGroup); setNewGroup(""); } }}
                  placeholder={t("appLauncher.groupManager.newPlaceholder")}
                  spellCheck={false}
                />
                <button
                  type="button"
                  className="al-btn al-btn-primary"
                  disabled={!newGroup.trim()}
                  onClick={() => { addGroup(newGroup); setNewGroup(""); }}
                >
                  <Plus size={14} />
                  <span>{t("appLauncher.groupManager.add")}</span>
                </button>
              </div>
              {availableGroups.length === 0 ? (
                <div className="al-empty al-empty-sm">
                  <Tags size={20} className="al-empty-icon" />
                  <span>{t("appLauncher.groupManager.empty")}</span>
                </div>
              ) : (
                <div className="al-group-list">
                  {availableGroups.map((g) => {
                    const count = launchers.filter((l) => (l.group || "") === g).length;
                    const isEditing = editingGroup?.name === g;
                    return (
                      <div key={g} className="al-group-row">
                        {isEditing ? (
                          <>
                            <input
                              type="text"
                              className="devtools-input al-group-rename-input"
                              value={editingGroup.value}
                              autoFocus
                              onChange={(e) => setEditingGroup({ name: g, value: e.target.value })}
                              onKeyDown={(e) => {
                                if (e.key === "Enter") { void renameGroup(g, editingGroup.value); setEditingGroup(null); }
                                else if (e.key === "Escape") setEditingGroup(null);
                              }}
                            />
                            <button
                              type="button"
                              className="al-icon-btn"
                              title={t("appLauncher.save")}
                              onClick={() => { void renameGroup(g, editingGroup.value); setEditingGroup(null); }}
                            >
                              <Check size={14} />
                            </button>
                            <button type="button" className="al-icon-btn" title={t("appLauncher.cancel")} onClick={() => setEditingGroup(null)}>
                              <X size={14} />
                            </button>
                          </>
                        ) : (
                          <>
                            <span className="al-group-row-name" title={g}>{g}</span>
                            <span className="al-group-row-count">{count}</span>
                            <button
                              type="button"
                              className="al-icon-btn"
                              title={t("appLauncher.groupManager.rename")}
                              onClick={() => setEditingGroup({ name: g, value: g })}
                            >
                              <Pencil size={14} />
                            </button>
                            <button
                              type="button"
                              className="al-icon-btn al-icon-btn-danger"
                              title={t("appLauncher.groupManager.delete")}
                              onClick={async () => {
                                const ok = await confirm({
                                  title: t("appLauncher.groupManager.deleteTitle"),
                                  message: t("appLauncher.groupManager.deleteConfirm", { name: g, count }),
                                  confirmText: t("appLauncher.delete"),
                                  icon: "danger",
                                });
                                if (ok) await deleteGroup(g);
                              }}
                            >
                              <Trash2 size={14} />
                            </button>
                          </>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
            <div className="modal-footer">
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => { setGroupManagerOpen(false); setEditingGroup(null); }}
              >
                {t("appLauncher.close")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default AppLauncherTool;
