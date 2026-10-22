import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  Loader2,
  MousePointer2,
  Check,
  AlertCircle,
  XCircle,
  RefreshCw,
  Circle,
  HardDrive,
  Trash2,
  Eye,
  EyeOff,
  Copy,
  FolderOpen,
  ClipboardList,
  Zap,
} from "lucide-react";
import useTauriEvent from "../hooks/useTauriEvent";
import { useGlobalStore } from "../core/store";
import { matchCursorAccount } from "../core/cursorMatch";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";
import type { CursorAccount, CursorUpdateState } from "../core/types";

/** 香槟金主题友好的头像色：珍珠香槟 / 石灰珍珠 */
const AVATAR_COLORS = [
  "#D8CFC3",
  "#B5AAA0",
  "#C4B8AC",
  "#9A928A",
  "#E0D8CE",
  "#8A847C",
];

const LEGACY_AVATAR_COLORS = new Set([
  "#667eea",
  "#f093fb",
  "#4facfe",
  "#43e97b",
  "#fa709a",
  "#fee140",
  // former sage-mist palette
  "#5f8f72",
  "#6a9a88",
  "#7a9068",
  "#4d7a6c",
  "#8a9e7a",
  "#5a8578",
  // former saturated champagne
  "#b8925a",
  "#c4a06a",
  "#a68b6a",
  "#8e7a5c",
  "#d4af7a",
  "#9a8268",
  // former dusty taupe champagne
  "#a89884",
  "#b5a898",
  "#8e8478",
  "#c5b5a0",
  "#9a9084",
  "#7a7268",
]);

const AVATAR_MIGRATE_KEY = "ai-workbench-cursor-avatar-migrated-v4";
/** Pre-rename key: still read so the one-shot migration does not re-run. */
const LEGACY_AVATAR_MIGRATE_KEY = "wt-cursor-avatar-migrated-v4";
/** One-shot dismissal of the first-time setup wizard. */
const SETUP_DISMISS_KEY = "ai-workbench-cursor-setup-dismissed-v1";

type SnapshotStatus = {
  complete: boolean;
  authEmail: string;
  reason: string;
  warning?: string;
};

/**
 * Last snapshot inspection, kept outside React so re-entering the tab paints
 * what we already know. Without this every visit rendered all accounts as
 * "快照不完整" until the (slow) per-account SQLite scan finished.
 */
let lastSnapshotStatus: Record<string, SnapshotStatus> | null = null;
let lastBackupSizes: Record<string, number> = {};

function avatarColorFor(account: CursorAccount, index: number): string {
  const c = (account.color || "").toLowerCase();
  if (!c || LEGACY_AVATAR_COLORS.has(c)) {
    return AVATAR_COLORS[index % AVATAR_COLORS.length];
  }
  return account.color;
}

/** Rough completion for each backend progress stage. */
const SWITCH_STAGE_PERCENT: Record<string, number> = {
  quit: 15,
  save: 30,
  sync: 45,
  seed: 55,
  restore: 70,
  launch: 88,
  done: 100,
};

/**
 * The usage card: bullet lists, so the copy never carries its own numbering
 * (a pasted "7." in JSX and "1."-"6." in the locale strings drift apart the
 * moment an item is inserted). Keyed by i18n suffix under `cursor.*`.
 */
const USAGE_GROUPS: { titleKey: string; itemKeys: string[] }[] = [
  {
    titleKey: "usageGroupAddTitle",
    itemKeys: ["usageAdd1", "usageAdd2", "usageAdd3"],
  },
  {
    titleKey: "usageGroupSwitchTitle",
    itemKeys: ["usageSwitch1", "usageSwitch2"],
  },
  {
    titleKey: "usageGroupMaintainTitle",
    itemKeys: ["usageMaintain1", "usageMaintain2", "usageMaintain3"],
  },
];

const SETUP_STEP_KEYS = ["setupStep1", "setupStep2", "setupStep3"];

function CursorManager() {
  const { t } = useTranslation("ai");
  const confirm = useConfirm();
  const cursorAccounts = useGlobalStore((s) => s.cursorAccounts);
  const loadCursorAccounts = useGlobalStore((s) => s.loadCursorAccounts);
  const addCursorAccount = useGlobalStore((s) => s.addCursorAccount);
  const finishAccountProfile = useGlobalStore((s) => s.finishAccountProfile);
  const reopenCursorForInit = useGlobalStore((s) => s.reopenCursorForInit);
  const updateCursorAccount = useGlobalStore((s) => s.updateCursorAccount);
  const deleteCursorAccount = useGlobalStore((s) => s.deleteCursorAccount);
  const switchCursorAccount = useGlobalStore((s) => s.switchCursorAccount);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeGetCursorLoginStatus = useGlobalStore((s) => s.invokeGetCursorLoginStatus);
  const invokeIsCursorRunning = useGlobalStore((s) => s.invokeIsCursorRunning);
  const invokeLaunchCursor = useGlobalStore((s) => s.invokeLaunchCursor);
  const invokeQuitCursor = useGlobalStore((s) => s.invokeQuitCursor);
  const invokeListCursorBackups = useGlobalStore((s) => s.invokeListCursorBackups);
  const invokeGetCursorOrphanProfiles = useGlobalStore(
    (s) => s.invokeGetCursorOrphanProfiles
  );
  const invokeCleanupCursorOrphanProfiles = useGlobalStore(
    (s) => s.invokeCleanupCursorOrphanProfiles
  );
  const syncCursorLoggedInFlags = useGlobalStore((s) => s.syncCursorLoggedInFlags);
  const invokeInspectCursorBackup = useGlobalStore((s) => s.invokeInspectCursorBackup);
  const invokeGetCursorDiskUsage = useGlobalStore((s) => s.invokeGetCursorDiskUsage);
  const invokeCleanupCursorFullBackups = useGlobalStore((s) => s.invokeCleanupCursorFullBackups);
  const invokeCleanupCursorSealedBackups = useGlobalStore(
    (s) => s.invokeCleanupCursorSealedBackups
  );
  const invokeGetCursorUpdateState = useGlobalStore((s) => s.invokeGetCursorUpdateState);
  const invokeCleanupCursorUpdateState = useGlobalStore((s) => s.invokeCleanupCursorUpdateState);
  const invokeSlimCursorStateDbs = useGlobalStore((s) => s.invokeSlimCursorStateDbs);
  const invokeReadCursorDiagnostics = useGlobalStore((s) => s.invokeReadCursorDiagnostics);
  const invokeOpenRuntimeFolder = useGlobalStore((s) => s.invokeOpenRuntimeFolder);

  const [showModal, setShowModal] = useState(false);
  const [editingAccount, setEditingAccount] = useState<CursorAccount | null>(null);
  const [formData, setFormData] = useState({
    name: "",
    email: "",
    password: "",
    notes: "",
  });
  const [showPassword, setShowPassword] = useState(false);
  const [revealedPasswordIds, setRevealedPasswordIds] = useState<Set<string>>(
    () => new Set()
  );
  const [message, setMessage] = useState<{
    type: "success" | "error" | "warning";
    text: string;
  } | null>(null);
  const [currentCursor, setCurrentCursor] = useState<{
    email: string;
    name: string;
    isLoggedIn: boolean;
  } | null>(null);
  const [isCursorRunning, setIsCursorRunning] = useState(false);
  const [switchingId, setSwitchingId] = useState<string | null>(null);
  const [switchStep, setSwitchStep] = useState("");
  const [switchStage, setSwitchStage] = useState("");
  const [busy, setBusy] = useState(false);
  const [statusRefreshing, setStatusRefreshing] = useState(false);
  const [backupStatus, setBackupStatus] = useState<Record<string, SnapshotStatus>>(
    () => lastSnapshotStatus ?? {}
  );
  const [showSetupGuide, setShowSetupGuide] = useState(
    () => localStorage.getItem(SETUP_DISMISS_KEY) !== "1"
  );
  const [diskUsage, setDiskUsage] = useState<{
    backupsBytes: number;
    backupsFullDbBytes: number;
    staleDbCount: number;
    sealedBytes: number;
    sealedCount: number;
    liveDbBytes: number;
    backupsPath: string;
    liveDbPath: string;
  } | null>(null);
  const [diskLoading, setDiskLoading] = useState(false);
  const [cleaning, setCleaning] = useState(false);
  const [slimming, setSlimming] = useState(false);
  const [backupSizes, setBackupSizes] = useState<Record<string, number>>(
    () => lastBackupSizes
  );
  const [orphans, setOrphans] = useState<{ count: number; bytes: number } | null>(null);
  const [updateState, setUpdateState] = useState<CursorUpdateState | null>(null);
  const [updateCleaning, setUpdateCleaning] = useState(false);
  const [accountQuery, setAccountQuery] = useState("");
  const [accountSort, setAccountSort] = useState<"created" | "name">("created");

  const visibleAccounts = useMemo(() => {
    const q = accountQuery.trim().toLowerCase();
    const matched = q
      ? cursorAccounts.filter(
          (a) =>
            a.name.toLowerCase().includes(q) ||
            a.email.toLowerCase().includes(q) ||
            (a.notes ?? "").toLowerCase().includes(q)
        )
      : cursorAccounts;
    if (accountSort === "name") {
      return [...matched].sort((a, b) =>
        (a.notes?.trim() || a.name).localeCompare(b.notes?.trim() || b.name)
      );
    }
    // The label promises creation order, so sort for it instead of relying on
    // whatever row order the store happened to hand back.
    return [...matched].sort((a, b) => b.createdAt.localeCompare(a.createdAt));
  }, [cursorAccounts, accountQuery, accountSort]);

  /** Only one job that quits Cursor or holds its files open at a time:
   *  switching, adding, deleting and DB slimming all do, and a VACUUM that
   *  races a launch leaves a half-compacted database behind. */
  const opLocked =
    Boolean(switchingId) || busy || cleaning || slimming || updateCleaning;

  const msgTimer = useRef<number | undefined>(undefined);

  const showMsg = useCallback((type: "success" | "error" | "warning", text: string) => {
    setMessage({ type, text });
    if (msgTimer.current) window.clearTimeout(msgTimer.current);
    msgTimer.current = window.setTimeout(() => setMessage(null), 6000);
  }, []);

  useEffect(
    () => () => {
      if (msgTimer.current) window.clearTimeout(msgTimer.current);
    },
    []
  );

  const copyText = useCallback(
    async (text: string, okMsg: string) => {
      try {
        await invokeCopyToClipboard(text);
        showMsg("success", okMsg);
      } catch (e) {
        showMsg("error", `${t("cursor.copyFailed")}: ${e}`);
      }
    },
    [invokeCopyToClipboard, showMsg, t]
  );

  const togglePasswordReveal = useCallback((id: string) => {
    setRevealedPasswordIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const refreshLiveStatus = useCallback(
    async (opts?: { silent?: boolean }) => {
      setStatusRefreshing(true);
      try {
        try {
          const status = await invokeGetCursorLoginStatus();
          setCurrentCursor(status);
          // This runs on every window focus, so only persist when a stored
          // "logged in" flag would actually change — an unconditional save
          // rewrites the whole table each time.
          const stored = useGlobalStore.getState().cursorAccounts;
          // "Logged in" with neither an address nor a display name is a broken
          // read, not a signed-out state — syncing from it would wipe a real
          // 当前登录 flag off some other row.
          const unusable =
            Boolean(status?.isLoggedIn) && !status?.email && !status?.name;
          const live =
            status?.isLoggedIn && !unusable
              ? { email: status.email, name: status.name }
              : null;
          const changed = unusable
            ? false
            : stored.some((account) => {
                const shouldBeLoggedIn = live
                  ? matchCursorAccount(account, { ...live, isLoggedIn: true })
                  : false;
                return Boolean(account.isLoggedIn) !== shouldBeLoggedIn;
              });
          if (changed) {
            await syncCursorLoggedInFlags(status?.email ?? "", status?.name ?? "");
          }
        } catch (e) {
          setCurrentCursor(null);
          if (!opts?.silent) {
            showMsg("error", t("cursor.loginStatusFailed", { error: e }));
          }
        }
        try {
          setIsCursorRunning(await invokeIsCursorRunning());
        } catch {
          setIsCursorRunning(false);
          if (!opts?.silent) {
            showMsg("error", t("cursor.cursorDetectFailed"));
          }
        }
      } finally {
        setStatusRefreshing(false);
      }
    },
    [invokeGetCursorLoginStatus, invokeIsCursorRunning, syncCursorLoggedInFlags, showMsg, t]
  );

  const refreshDiskUsage = useCallback(async () => {
    setDiskLoading(true);
    try {
      setDiskUsage(await invokeGetCursorDiskUsage());
      setOrphans(await invokeGetCursorOrphanProfiles());
    } catch {
      // Keep the last known numbers: a failed re-read is not "no data", and the
      // alternative is flashing "无法读取占用信息" over good figures.
      setOrphans(null);
    } finally {
      setDiskLoading(false);
    }
  }, [invokeGetCursorDiskUsage, invokeGetCursorOrphanProfiles]);

  /** Read-only probe: is Cursor's updater stuck with stale "in progress" markers? */
  const refreshUpdateState = useCallback(async () => {
    try {
      setUpdateState(await invokeGetCursorUpdateState());
    } catch {
      /* diagnostics only — never block the page on it */
    }
  }, [invokeGetCursorUpdateState]);

  const refresh = useCallback(async () => {
    // Disk scan can walk multi-GB trees — start it now, but never block the
    // account list or first paint on it.
    void refreshDiskUsage();
    void refreshUpdateState();
    await loadCursorAccounts();

    // One-shot legacy avatar color migration
    const loaded = useGlobalStore.getState().cursorAccounts;
    const avatarMigrated =
      localStorage.getItem(AVATAR_MIGRATE_KEY) === "1" ||
      localStorage.getItem(LEGACY_AVATAR_MIGRATE_KEY) === "1";
    if (!avatarMigrated) {
      const toMigrate = loaded.filter((a, i) => avatarColorFor(a, i) !== a.color);
      if (toMigrate.length > 0) {
        await Promise.all(
          loaded.map(async (a, i) => {
            const next = avatarColorFor(a, i);
            if (next !== a.color) {
              await updateCursorAccount(a.id, { color: next });
            }
          })
        );
      }
      localStorage.setItem(AVATAR_MIGRATE_KEY, "1");
    }

    await refreshLiveStatus({ silent: true });

    const accounts = useGlobalStore.getState().cursorAccounts;
    // Publish per account as it lands: the SQLite probe takes ~1s each, and a
    // card that keeps its last known state beats one that flashes "不完整".
    const publish = (id: string, status: SnapshotStatus) => {
      lastSnapshotStatus = { ...(lastSnapshotStatus ?? {}), [id]: status };
      setBackupStatus(lastSnapshotStatus);
    };
    await Promise.all(
      accounts.map(async (a) => {
        try {
          const info = await invokeInspectCursorBackup(a.id);
          publish(a.id, {
            complete: info.complete,
            authEmail: info.authEmail,
            reason: info.reason,
            warning: info.warning,
          });
        } catch {
          publish(a.id, {
            complete: false,
            authEmail: "",
            reason: t("cursor.checkSnapshotFailed"),
          });
        }
      })
    );
    const alive = new Set(accounts.map((a) => a.id));
    lastSnapshotStatus = Object.fromEntries(
      Object.entries(lastSnapshotStatus ?? {}).filter(([id]) => alive.has(id))
    );
    setBackupStatus(lastSnapshotStatus);

    const backups = await invokeListCursorBackups().catch(
      (): Array<{ accountId: string; path: string; sizeBytes: number }> => []
    );
    lastBackupSizes = Object.fromEntries(backups.map((b) => [b.accountId, b.sizeBytes]));
    setBackupSizes(lastBackupSizes);
  }, [
    loadCursorAccounts,
    updateCursorAccount,
    refreshLiveStatus,
    invokeInspectCursorBackup,
    invokeListCursorBackups,
    refreshDiskUsage,
    refreshUpdateState,
    t,
  ]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    const onFocus = () => {
      refreshLiveStatus({ silent: true });
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshLiveStatus]);

  // Keep the "Cursor is running" indicator fresh even when the window stays
  // focused while Cursor quits / starts in the background (focus refresh alone
  // would leave a stale "in use" state on the active account's button).
  useEffect(() => {
    const timer = window.setInterval(async () => {
      if (opLocked) return;
      try {
        setIsCursorRunning(await invokeIsCursorRunning());
      } catch {
        /* silent — probing failures must not spam toasts */
      }
    }, 10_000);
    return () => window.clearInterval(timer);
  }, [invokeIsCursorRunning, opLocked]);

  useTauriEvent<{ stage: string; message: string }>(
    "cursor:switch_progress",
    (event) => {
      setSwitchStep(event.payload.message);
      setSwitchStage(event.payload.stage);
    },
    []
  );

  // Flows emit stages in different orders (finish saves before it quits), so the
  // bar only ever moves forward within one run and resets when it goes idle.
  const maxPercentRef = useRef(0);
  maxPercentRef.current = switchStage
    ? Math.max(maxPercentRef.current, SWITCH_STAGE_PERCENT[switchStage] ?? 0)
    : 0;
  const switchPercent = maxPercentRef.current;

  const formatBytes = (bytes: number) => {
    if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
    if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
    if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
    return `${bytes} B`;
  };

  const handleCleanupFullBackups = async () => {
    if (opLocked) return;
    const stale = diskUsage?.staleDbCount ?? 0;
    const reclaim = diskUsage?.backupsFullDbBytes ?? 0;
    const ok = await confirm({
      title: t("cursor.cleanupTitle"),
      message: t("cursor.cleanupMessage", {
        count: stale,
        size: formatBytes(reclaim),
      }),
      warning: t("cursor.cleanupWarning"),
      confirmText: t("cursor.cleanupConfirm"),
      icon: "warning",
    });
    if (!ok) return;
    setCleaning(true);
    try {
      const result = await invokeCleanupCursorFullBackups();
      showMsg("success", result.message || t("cursor.cleanupOk"));
      await refreshDiskUsage();
    } catch (e) {
      showMsg("error", `${t("cursor.cleanupFailed")}: ${e}`);
    } finally {
      setCleaning(false);
    }
  };

  const handleCleanupSealedBackups = async () => {
    if (opLocked) return;
    const count = diskUsage?.sealedCount ?? 0;
    const size = diskUsage?.sealedBytes ?? 0;
    const ok = await confirm({
      title: t("cursor.sealedCleanupTitle"),
      message: t("cursor.sealedCleanupMessage", { count, size: formatBytes(size) }),
      warning: t("cursor.sealedCleanupWarning"),
      confirmText: t("cursor.sealedCleanupConfirm"),
      icon: "warning",
    });
    if (!ok) return;
    setCleaning(true);
    try {
      const result = await invokeCleanupCursorSealedBackups();
      showMsg("success", result.message || t("cursor.sealedCleanupOk"));
      await refreshDiskUsage();
    } catch (e) {
      showMsg("error", `${t("cursor.sealedCleanupFailed")}: ${e}`);
    } finally {
      setCleaning(false);
    }
  };

  /**
   * Move Cursor's stale "update in progress" markers aside (reversible) so its
   * updater stops re-attempting a move of `Cursor.exe` that can never succeed.
   */
  const handleCleanupUpdateState = async () => {
    if (!updateState?.stuck || opLocked) return;
    if (updateState.running) {
      showMsg("warning", t("cursor.updateCleanupNeedQuit"));
      return;
    }
    const backupNote =
      updateState.backupBytes > 0
        ? `\n${t("cursor.updateCleanupBackup", { size: formatBytes(updateState.backupBytes) })}`
        : "";
    const ok = await confirm({
      title: t("cursor.updateCleanupTitle"),
      message: `${updateState.message}\n${updateState.files
        .map((file) => `• ${file}`)
        .join("\n")}${backupNote}`,
      warning: t("cursor.updateCleanupWarning"),
      confirmText: t("cursor.updateCleanupConfirm"),
      icon: "warning",
    });
    if (!ok) return;
    setUpdateCleaning(true);
    try {
      const result = await invokeCleanupCursorUpdateState();
      showMsg("success", result.message || t("cursor.updateCleanupOk"));
    } catch (e) {
      showMsg("error", `${t("cursor.updateCleanupFailed")}: ${e}`);
    } finally {
      setUpdateCleaning(false);
      await refreshUpdateState();
    }
  };

  const handleSlimDbs = async () => {
    if (opLocked) return;
    const ok = await confirm({
      title: t("cursor.slimTitle"),
      message: t("cursor.slimMessage"),
      warning: t("cursor.slimWarning"),
      confirmText: t("cursor.slimConfirm"),
      icon: "warning",
    });
    if (!ok) return;
    setSlimming(true);
    try {
      const result = await invokeSlimCursorStateDbs();
      const slimLabel = (label: string) =>
        label === "legacy-shared"
          ? t("cursor.diskShared")
          : label === "default"
            ? t("cursor.diskLive")
            : t("cursor.diskProfiles");
      const failed = result.targets.filter((x) => x.action === "failed");
      const reclaimed = result.targets
        .filter((x) => x.beforeBytes > x.afterBytes)
        .map((x) => `${slimLabel(x.label)} −${formatBytes(x.beforeBytes - x.afterBytes)}`);
      showMsg(
        failed.length ? "warning" : "success",
        failed.length
          ? `${result.message}：${failed.map((x) => slimLabel(x.label)).join(", ")}`
          : `${result.message}${reclaimed.length ? `（${reclaimed.join("、")}）` : ""}`
      );
      await refreshDiskUsage();
    } catch (e) {
      showMsg("error", `${t("cursor.slimFailed")}: ${e}`);
    } finally {
      setSlimming(false);
    }
  };

  const openAddModal = () => {
    const incomplete = cursorAccounts.find((account) => {
      if (!account.profileInitialized) return true;
      const snap = backupStatus[account.id];
      return snap ? !snap.complete : false;
    });
    if (incomplete) {
      showMsg(
        "warning",
        t("cursor.pendingInitBlocked", { name: incomplete.notes || incomplete.name })
      );
      return;
    }
    setEditingAccount(null);
    setShowPassword(false);
    setFormData({
      name: currentCursor?.name || "",
      email: currentCursor?.email || "",
      password: "",
      notes: "",
    });
    setShowModal(true);
  };

  const handleEdit = (account: CursorAccount) => {
    setEditingAccount(account);
    setShowPassword(false);
    setFormData({
      name: account.name,
      email: account.email,
      password: account.password || "",
      notes: account.notes || "",
    });
    setShowModal(true);
  };

  const handleDelete = async (id: string) => {
    if (opLocked) return;
    // A running Cursor holds the profile and snapshot files open; a
    // half-deleted account is worse than a refused click, and the page cannot
    // tell which profile the running process owns (that probe is server-side).
    if (isCursorRunning) {
      showMsg("warning", t("cursor.deleteNeedQuit"));
      return;
    }
    const ok = await confirm({
      title: t("cursor.deleteTitle"),
      message: t("cursor.deleteMessage"),
      warning: t("cursor.deleteWarning"),
      confirmText: t("cursor.deleteConfirm"),
      icon: "danger",
    });
    if (ok) {
      try {
        await deleteCursorAccount(id);
        showMsg("success", t("cursor.deleteOk"));
        await refresh();
      } catch (error) {
        showMsg("error", t("cursor.deleteFailed", { error }));
      }
    }
  };

  const handleSubmit = async () => {
    if (opLocked) return;
    if (!formData.name.trim() || !formData.email.trim()) {
      showMsg("error", t("cursor.nameEmailRequired"));
      return;
    }

    const colors = AVATAR_COLORS;

    if (editingAccount) {
      try {
        setBusy(true);
        await updateCursorAccount(editingAccount.id, {
          name: formData.name,
          email: formData.email,
          password: formData.password.trim() || undefined,
          notes: formData.notes || undefined,
        });
        showMsg("success", t("cursor.saveUpdated"));
        setShowModal(false);
        await refresh();
      } catch (error) {
        showMsg("error", t("cursor.saveFailed", { error }));
      } finally {
        setBusy(false);
      }
      return;
    }

    // Two rows for one address would let the second "完成初始化" treat the
    // first as a duplicate slot and delete its snapshot.
    const wanted = formData.email.trim().toLowerCase();
    const duplicate = cursorAccounts.find(
      (a) => a.email.trim().toLowerCase() === wanted
    );
    if (duplicate) {
      showMsg(
        "warning",
        t("cursor.duplicateEmail", { name: duplicate.notes || duplicate.name })
      );
      return;
    }

    if (isCursorRunning) {
      const ok = await confirm({
        title: t("cursor.addConfirmTitle"),
        message: t("cursor.addConfirmMessage"),
        warning: t("cursor.addConfirmWarning"),
        confirmText: t("cursor.addConfirmOk"),
        icon: "warning",
      });
      if (!ok) return;
    }

    setBusy(true);
    setSwitchStep(t("cursor.openingProfile"));
    try {
      const msg = await addCursorAccount({
        name: formData.name,
        email: formData.email,
        color: colors[cursorAccounts.length % colors.length],
        password: formData.password.trim() || undefined,
        notes: formData.notes || undefined,
      });
      showMsg("success", msg);
      setShowModal(false);
      await refresh();
    } catch (error) {
      showMsg("error", t("cursor.addFailed", { error }));
      await refresh();
    } finally {
      setBusy(false);
      setSwitchStep("");
      setSwitchStage("");
    }
  };

  const handleFinishInit = async (account: CursorAccount) => {
    if (opLocked) return;
    const ok = await confirm({
      title: t("cursor.finishInitTitle"),
      message: t("cursor.finishInitMessage", { name: account.notes || account.name }),
      warning: t("cursor.finishInitWarning"),
      confirmText: t("cursor.finishLogin"),
      icon: "warning",
    });
    if (!ok) return;

    setBusy(true);
    setSwitchStep(t("cursor.savingLogin"));
    try {
      const result = await finishAccountProfile(account.id);
      showMsg("success", result || t("cursor.finishInitOk"));
      await refresh();
    } catch (error) {
      showMsg("error", t("cursor.initFailed", { error }));
      await refresh();
    } finally {
      setBusy(false);
      setSwitchStep("");
      setSwitchStage("");
    }
  };

  const handleReopenCursor = async (account: CursorAccount) => {
    if (opLocked) return;
    setBusy(true);
    setSwitchStep(t("cursor.openingProfile"));
    try {
      const msg = await reopenCursorForInit(account.id, account.email);
      showMsg("success", msg || t("cursor.reopenOk"));
      await refresh();
    } catch (error) {
      showMsg("error", t("cursor.reopenFailed", { error }));
      await refresh();
    } finally {
      setBusy(false);
      setSwitchStep("");
      setSwitchStage("");
    }
  };

  const handleOpenProfileFolder = async (account: CursorAccount) => {
    if (!account.profileDir) {
      showMsg("warning", t("cursor.noProfileDir"));
      return;
    }
    try {
      await invokeOpenRuntimeFolder(account.profileDir);
    } catch (error) {
      showMsg("error", t("cursor.openFolderFailed", { error }));
    }
  };

  const handleCopyDiagnostics = async (account: CursorAccount) => {
    try {
      const text = await invokeReadCursorDiagnostics(account.id);
      await invokeCopyToClipboard(text);
      showMsg("success", t("cursor.diagnosticsCopied"));
    } catch (error) {
      showMsg("error", t("cursor.diagnosticsFailed", { error }));
    }
  };

  const handleLaunchCursor = async (account: CursorAccount) => {
    if (opLocked) return;
    setBusy(true);
    setSwitchStep(t("cursor.launching"));
    try {
      const msg = await invokeLaunchCursor(account.id, account.email);
      showMsg("success", msg || t("cursor.launchOk"));
      await refreshLiveStatus({ silent: true });
    } catch (error) {
      showMsg("error", t("cursor.launchFailed", { error }));
    } finally {
      setBusy(false);
      setSwitchStep("");
      setSwitchStage("");
    }
  };

  const handleQuitCursor = async () => {
    if (opLocked) return;
    const ok = await confirm({
      title: t("cursor.quitTitle"),
      message: t("cursor.quitMessage"),
      confirmText: t("cursor.quitConfirm"),
      icon: "warning",
    });
    if (!ok) return;
    setBusy(true);
    setSwitchStep(t("cursor.quitting"));
    try {
      const msg = await invokeQuitCursor();
      showMsg("success", msg || t("cursor.quitOk"));
      await refreshLiveStatus({ silent: true });
    } catch (error) {
      showMsg("error", t("cursor.quitFailed", { error }));
    } finally {
      setBusy(false);
      setSwitchStep("");
      setSwitchStage("");
    }
  };

  const handleCleanupOrphans = async () => {
    if (opLocked) return;
    const count = orphans?.count ?? 0;
    if (count === 0) return;
    const ok = await confirm({
      title: t("cursor.orphanCleanupTitle"),
      message: t("cursor.orphanCleanupMessage", {
        count,
        size: formatBytes(orphans?.bytes ?? 0),
      }),
      warning: t("cursor.orphanCleanupWarning"),
      confirmText: t("cursor.orphanCleanupConfirm"),
      icon: "danger",
    });
    if (!ok) return;
    setCleaning(true);
    try {
      const result = await invokeCleanupCursorOrphanProfiles();
      showMsg("success", result.message || t("cursor.orphanCleanupOk"));
      await refresh();
    } catch (error) {
      showMsg("error", t("cursor.orphanCleanupFailed", { error }));
    } finally {
      setCleaning(false);
    }
  };

  const isLiveAccount = (account: CursorAccount) =>
    matchCursorAccount(account, currentCursor);

  const handleSwitch = async (account: CursorAccount) => {
    if (opLocked) return;
    if (isLiveAccount(account)) {
      showMsg("warning", t("cursor.alreadyCurrent"));
      return;
    }

    const status = backupStatus[account.id];

    if (!account.profileInitialized) {
      const snapOk = status && status.complete;
      if (!snapOk) {
        showMsg("warning", t("cursor.notInitialized"));
        return;
      }
    }

    if (status && !status.complete) {
      showMsg(
        "error",
        t("cursor.snapshotBlocked", {
          name: account.notes || account.name,
          reason: status.reason,
        })
      );
      return;
    }

    const liveSaved = cursorAccounts.find((a) => isLiveAccount(a));
    const currentUnsaved = Boolean(currentCursor?.isLoggedIn && !liveSaved);

    const displayName =
      account.notes?.trim() ||
      (account.email.includes("@") ? account.email : account.name);
    const ok = await confirm({
      title: t("cursor.switchTitle"),
      message: t("cursor.switchMessage", {
        target: displayName,
        action: isCursorRunning
          ? t("cursor.switchActionRunning")
          : t("cursor.switchActionIdle"),
      }),
      warning: currentUnsaved
        ? t("cursor.switchWarningUnsaved")
        : t("cursor.switchWarning"),
      confirmText: t("cursor.switch"),
      icon: "warning",
    });
    if (!ok) return;

    setSwitchingId(account.id);
    setSwitchStep(
      isCursorRunning ? t("cursor.switchStepClosing") : t("cursor.switchStepSwitching")
    );
    try {
      const result = await switchCursorAccount(
        account.id,
        liveSaved && liveSaved.id !== account.id ? liveSaved.id : null
      );
      showMsg("success", result || t("cursor.switchOk", { name: displayName }));
      await refresh();
    } catch (error) {
      const errText = String(error);
      const authish = /Sign Out|Authentication/i.test(errText);
      // The error toast already offers 复制错误信息, so no silent clipboard write;
      // and the next step belongs in i18n, not bolted onto the message in English.
      showMsg(
        "error",
        t("cursor.switchFailed", { error: errText }) +
          (authish ? `\n${t("cursor.switchFailedAuth")}` : "")
      );
      await refresh();
    } finally {
      setSwitchingId(null);
      setSwitchStep("");
      setSwitchStage("");
    }
  };

  return (
    <div className="page-scrollable cursor-manager-page">
      <div className="cursor-toolbar">
        <div className="cursor-toolbar-main">
          <div className="status-label">{t("cursor.currentLogin")}</div>
          <div className="status-value cursor-toolbar-login">
            {currentCursor?.isLoggedIn && (currentCursor.name || currentCursor.email)
              ? `${currentCursor.name || currentCursor.email}${
                  currentCursor.email ? ` <${currentCursor.email}>` : ""
                }`
              : t("cursor.notLoggedIn")}
          </div>
        </div>
        <div className="cursor-toolbar-actions">
          <span className="cursor-run-pill">
            <Circle
              size={10}
              fill={isCursorRunning ? "var(--green)" : "var(--text-3)"}
              color={isCursorRunning ? "var(--green)" : "var(--text-3)"}
            />
            {isCursorRunning ? t("cursor.runningShort") : t("cursor.notRunningShort")}
          </span>
          {isCursorRunning && (
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={handleQuitCursor}
              disabled={opLocked}
              title={t("cursor.quitMessage")}
            >
              {t("cursor.btnQuit")}
            </button>
          )}
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => refreshLiveStatus()}
            disabled={statusRefreshing || Boolean(switchingId) || busy}
            title={t("cursor.refreshStatus")}
          >
            {statusRefreshing ? <Loader2 size={12} className="spin" /> : <RefreshCw size={12} />}
            {t("cursor.refresh")}
          </button>
        </div>
      </div>

      {switchingId && (
        <div className="switch-progress">
          <Loader2 size={14} className="spin" />
          <div style={{ flex: 1 }}>
            <div>{switchStep || t("cursor.switching")}</div>
            {switchPercent > 0 && (
              <div
                style={{
                  marginTop: 4,
                  height: 3,
                  borderRadius: 2,
                  background: "var(--border)",
                  overflow: "hidden",
                }}
              >
                <div
                  style={{
                    width: `${switchPercent}%`,
                    height: "100%",
                    background: "var(--accent)",
                    transition: "width 0.3s",
                  }}
                />
              </div>
            )}
          </div>
        </div>
      )}

      {busy && !switchingId && (
        <div className="switch-progress">
          <Loader2 size={14} className="spin" />
          <div style={{ flex: 1 }}>
            <div>{switchStep || t("cursor.working")}</div>
            {switchPercent > 0 && (
              <div
                style={{
                  marginTop: 4,
                  height: 3,
                  borderRadius: 2,
                  background: "var(--border)",
                  overflow: "hidden",
                }}
              >
                <div
                  style={{
                    width: `${switchPercent}%`,
                    height: "100%",
                    background: "var(--accent)",
                    transition: "width 0.3s",
                  }}
                />
              </div>
            )}
          </div>
        </div>
      )}

      {isCursorRunning && !switchingId && !busy && (
        <div className="runtime-muted cursor-switch-tip">{t("cursor.switchTip")}</div>
      )}

      {showSetupGuide && cursorAccounts.length <= 1 && (
        <div className="card cursor-setup-card">
          <div className="card-title cursor-card-title">
            <span>{t("cursor.setupTitle")}</span>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => {
                localStorage.setItem(SETUP_DISMISS_KEY, "1");
                setShowSetupGuide(false);
              }}
            >
              {t("cursor.setupDismiss")}
            </button>
          </div>
          <ol className="cursor-setup-list">
            {SETUP_STEP_KEYS.map((key) => (
              <li key={key}>{t(`cursor.${key}`)}</li>
            ))}
          </ol>
        </div>
      )}

      <div className="card">
        <div className="card-title cursor-card-title">
          <span>
            {t("cursor.accountsTitle")}
            {cursorAccounts.length > 0 && (
              <span className="card-title-badge">{cursorAccounts.length}</span>
            )}
          </span>
          <button
            type="button"
            className="btn btn-primary btn-small"
            onClick={openAddModal}
            disabled={opLocked}
          >
            + {t("cursor.addAccount")}
          </button>
        </div>
        {cursorAccounts.length > 1 && (
          <div className="cursor-account-tools">
            <input
              className="input-field"
              value={accountQuery}
              onChange={(e) => setAccountQuery(e.target.value)}
              placeholder={t("cursor.searchAccounts")}
            />
            <select
              className="input-field"
              value={accountSort}
              onChange={(e) => setAccountSort(e.target.value as "created" | "name")}
            >
              <option value="created">{t("cursor.sortCreatedDesc")}</option>
              <option value="name">{t("cursor.sortNameAsc")}</option>
            </select>
          </div>
        )}
        {cursorAccounts.length === 0 ? (
          <div className="empty-state">
            <div className="empty-state-icon">
              <MousePointer2 size={32} strokeWidth={1.5} />
            </div>
            <p>{t("cursor.noAccounts")}</p>
            <p className="runtime-muted" style={{ marginTop: 4 }}>
              {t("cursor.noAccountsHint")}
            </p>
          </div>
        ) : visibleAccounts.length === 0 ? (
          <div className="empty-state">
            <p>{t("cursor.noAccountMatch")}</p>
          </div>
        ) : (
          <ul className="account-list">
            {visibleAccounts.map((account) => {
              const index = cursorAccounts.indexOf(account);
              const liveActive = isLiveAccount(account);
              const snap = backupStatus[account.id];
              // "Never inspected yet" only happens on the first load of a session.
              // It must not read as "broken": no red tag, no re-init prompt, and
              // no switch the backend would refuse.
              const snapPending = !snap;
              const incomplete = !!snap && !snap.complete;
              const needsInit = !account.profileInitialized;
              return (
                <li
                  key={account.id}
                  className={`account-item ${liveActive ? "active" : ""}`}
                >
                  <div
                    className="account-avatar"
                    style={{ background: avatarColorFor(account, index) }}
                  >
                    {(account.notes || account.name).charAt(0).toUpperCase()}
                  </div>
                  <div className="account-info">
                    <div className="account-name">
                      {account.notes?.trim() || account.name}
                      {liveActive && (
                        <span className="tag-pill tag-pill-accent">
                          {t("cursor.tagCurrent")}
                        </span>
                      )}
                      {liveActive && isCursorRunning && (
                        <span className="tag-pill tag-pill-green">
                          ● {t("cursor.tagRunning")}
                        </span>
                      )}
                      {needsInit && (
                        <span className="tag-pill tag-pill-amber">
                          {t("cursor.tagNeedsInit")}
                        </span>
                      )}
                      {account.profileInitialized && snap?.complete && (
                        <span className="tag-pill tag-pill-green">
                          {t("cursor.tagReady")}
                        </span>
                      )}
                      {incomplete && (
                        <span className="tag-pill tag-pill-red">
                          {t("cursor.tagSnapshotBad")}
                        </span>
                      )}
                      {snapPending && !needsInit && (
                        <span className="tag-pill">
                          <Loader2 size={10} className="spin" /> {t("cursor.checkingSnapshot")}
                        </span>
                      )}
                    </div>
                    <div className="cursor-acct-meta">
                      <span className="account-email cursor-acct-email">
                        {snap?.authEmail || account.email}
                      </span>
                      <button
                        type="button"
                        className="cursor-icon-btn"
                        onClick={() =>
                          copyText(snap?.authEmail || account.email, t("cursor.emailCopied"))
                        }
                        title={t("cursor.copyEmail")}
                        aria-label={t("cursor.copyEmail")}
                        disabled={Boolean(switchingId) || busy}
                      >
                        <Copy size={11} />
                      </button>
                      {backupSizes[account.id] > 0 && (
                        <span className="cursor-acct-size">
                          {t("cursor.backupSize", {
                            size: formatBytes(backupSizes[account.id]),
                          })}
                        </span>
                      )}
                    </div>
                    {account.password && (
                      <div className="cursor-password-row">
                        <span className="account-email cursor-password-value">
                          {t("cursor.password")}:{" "}
                          {revealedPasswordIds.has(account.id)
                            ? account.password
                            : "••••••••"}
                        </span>
                        <button
                          type="button"
                          className="btn btn-secondary btn-small"
                          onClick={() => togglePasswordReveal(account.id)}
                          title={
                            revealedPasswordIds.has(account.id)
                              ? t("cursor.hidePassword")
                              : t("cursor.showPassword")
                          }
                          disabled={Boolean(switchingId) || busy}
                        >
                          {revealedPasswordIds.has(account.id) ? (
                            <EyeOff size={12} />
                          ) : (
                            <Eye size={12} />
                          )}
                        </button>
                        <button
                          type="button"
                          className="btn btn-secondary btn-small"
                          onClick={() =>
                            copyText(account.password!, t("cursor.passwordCopied"))
                          }
                          title={t("cursor.copyPassword")}
                          disabled={Boolean(switchingId) || busy}
                        >
                          <Copy size={12} />
                        </button>
                      </div>
                    )}
                    {needsInit && (
                      <div className="account-notes cursor-note-warn">
                        {t("cursor.needsInitHint")}
                      </div>
                    )}
                    {incomplete && !needsInit && (
                      <div className="account-notes cursor-note-error">
                        {t("cursor.snapshotReasonHint", { reason: snap?.reason ?? "" })}
                      </div>
                    )}
                    {snap?.warning && (
                      <div className="account-notes cursor-note-warn">
                        {snap.warning}
                      </div>
                    )}
                  </div>
                  <div className="account-actions">
                    {needsInit || incomplete ? (
                      <>
                        <button
                          className="btn btn-primary btn-small"
                          onClick={() => handleFinishInit(account)}
                          disabled={opLocked}
                        >
                          {t("cursor.finishLogin")}
                        </button>
                        <button
                          className="btn btn-secondary btn-small"
                          onClick={() => handleReopenCursor(account)}
                          disabled={opLocked}
                          title={t("cursor.reopenHint")}
                        >
                          {t("cursor.btnReopen")}
                        </button>
                      </>
                    ) : snapPending ? (
                      <button
                        className="btn btn-primary btn-small"
                        disabled
                        title={t("cursor.checkingSnapshot")}
                      >
                        <Loader2 size={12} className="spin" />
                        {t("cursor.checkingSnapshot")}
                      </button>
                    ) : (
                      <button
                        className="btn btn-primary btn-small"
                        onClick={() =>
                          liveActive && !isCursorRunning
                            ? handleLaunchCursor(account)
                            : handleSwitch(account)
                        }
                        disabled={opLocked || (liveActive && isCursorRunning)}
                        title={
                          liveActive && !isCursorRunning
                            ? t("cursor.launchHint")
                            : undefined
                        }
                      >
                        {switchingId === account.id ? (
                          <>
                            <Loader2 size={12} className="spin" />
                            {t("cursor.btnSwitching")}
                          </>
                        ) : liveActive ? (
                          isCursorRunning ? (
                            t("cursor.btnInUse")
                          ) : (
                            t("cursor.btnLaunch")
                          )
                        ) : (
                          t("cursor.switch")
                        )}
                      </button>
                    )}
                    <button
                      className="btn btn-secondary btn-small"
                      onClick={() => handleEdit(account)}
                      disabled={Boolean(switchingId) || busy}
                    >
                      {t("cursor.btnEdit")}
                    </button>
                    <button
                      className="btn btn-danger btn-small"
                      onClick={() => handleDelete(account.id)}
                      disabled={opLocked}
                    >
                      {t("cursor.btnDelete")}
                    </button>
                    <button
                      className="btn btn-secondary btn-small"
                      onClick={() => handleOpenProfileFolder(account)}
                      disabled={busy}
                      title={account.profileDir || t("cursor.noProfileDir")}
                    >
                      <FolderOpen size={12} />
                    </button>
                    <button
                      className="btn btn-secondary btn-small"
                      onClick={() => handleCopyDiagnostics(account)}
                      disabled={busy}
                      title={t("cursor.btnCopyDiagnostics")}
                    >
                      <ClipboardList size={12} />
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      {updateState?.stuck && (
        <div className="cursor-update-alert">
          <AlertCircle size={14} className="cursor-update-alert-icon" />
          <div className="cursor-update-alert-body">
            <div className="cursor-update-alert-title">{t("cursor.updateCleanupDetected")}</div>
            <div className="cursor-update-alert-text">{updateState.message}</div>
            <div className="runtime-muted cursor-update-alert-hint">
              {updateState.running
                ? t("cursor.updateCleanupNeedQuit")
                : t("cursor.updateCleanupHint")}
            </div>
          </div>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={handleCleanupUpdateState}
            disabled={updateState.running || opLocked}
            title={
              updateState.running
                ? t("cursor.updateCleanupNeedQuit")
                : t("cursor.updateCleanupHint")
            }
          >
            {updateCleaning ? <Loader2 size={12} className="spin" /> : <Trash2 size={12} />}
            {t("cursor.updateCleanupAction")}
          </button>
        </div>
      )}

      <div className="cursor-disk-bar">
        <div className="cursor-disk-bar-main">
          <HardDrive size={14} className="cursor-disk-bar-icon" />
          <span className="cursor-disk-bar-title">{t("cursor.diskTitle")}</span>
          {diskUsage ? (
            <span className="cursor-disk-bar-stats">
              <span title={diskUsage.liveDbPath}>
                {t("cursor.diskLive")} {formatBytes(diskUsage.liveDbBytes)}
              </span>
              <span className="cursor-disk-sep">·</span>
              <span title={diskUsage.backupsPath}>
                {t("cursor.diskBackups")} {formatBytes(diskUsage.backupsBytes)}
                {diskUsage.staleDbCount > 0
                  ? `（${t("cursor.diskStaleShort", {
                      count: diskUsage.staleDbCount,
                      size: formatBytes(diskUsage.backupsFullDbBytes),
                    })}）`
                  : ""}
              </span>
              {diskUsage.sealedCount > 0 && (
                <>
                  <span className="cursor-disk-sep">·</span>
                  <span title={t("cursor.diskSealedHint")}>
                    {t("cursor.diskSealed")} {formatBytes(diskUsage.sealedBytes)}
                    {`（${t("cursor.diskSealedShort", { count: diskUsage.sealedCount })}）`}
                  </span>
                </>
              )}
              {orphans && orphans.count > 0 && (
                <>
                  <span className="cursor-disk-sep">·</span>
                  <span>
                    {t("cursor.orphanStale", {
                      count: orphans.count,
                      size: formatBytes(orphans.bytes),
                    })}
                  </span>
                </>
              )}
            </span>
          ) : (
            <span className="runtime-muted">
              {diskLoading ? t("cursor.diskLoading") : t("cursor.diskLoadFailed")}
            </span>
          )}
        </div>
        <div className="cursor-disk-bar-actions">
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => refreshDiskUsage()}
            disabled={diskLoading || cleaning}
            title={t("cursor.diskRefresh")}
          >
            {diskLoading ? <Loader2 size={12} className="spin" /> : <RefreshCw size={12} />}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={opLocked || !diskUsage?.staleDbCount}
            onClick={handleCleanupFullBackups}
            title={
              diskUsage?.staleDbCount
                ? t("cursor.cleanupWarning")
                : t("cursor.diskBackupsClean")
            }
          >
            {cleaning ? <Loader2 size={12} className="spin" /> : <Trash2 size={12} />}
            {t("cursor.cleanupAction")}
            {diskUsage && diskUsage.staleDbCount > 0
              ? ` (${formatBytes(diskUsage.backupsFullDbBytes)})`
              : ""}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={opLocked || !diskUsage}
            onClick={handleSlimDbs}
            title={t("cursor.slimWarning")}
          >
            {slimming ? <Loader2 size={12} className="spin" /> : <Zap size={12} />}
            {t("cursor.slimAction")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            disabled={opLocked || !orphans?.count}
            onClick={handleCleanupOrphans}
            title={
              orphans?.count
                ? t("cursor.orphanCleanupWarning")
                : t("cursor.orphanNone")
            }
          >
            {cleaning ? <Loader2 size={12} className="spin" /> : <Trash2 size={12} />}
            {t("cursor.orphanAction")}
            {orphans && orphans.count > 0 ? ` (${orphans.count})` : ""}
          </button>
          {diskUsage && diskUsage.sealedCount > 0 && (
            <button
              type="button"
              className="btn btn-secondary btn-small"
              disabled={opLocked}
              onClick={handleCleanupSealedBackups}
              title={t("cursor.sealedCleanupWarning")}
            >
              {cleaning ? <Loader2 size={12} className="spin" /> : <Trash2 size={12} />}
              {t("cursor.sealedCleanupAction")} (
              {formatBytes(diskUsage.sealedBytes)})
            </button>
          )}
        </div>
      </div>

      <div className="card cursor-usage-card">
        <div className="card-title">{t("cursor.usageTitle")}</div>
        <p className="cursor-usage-lead">{t("cursor.usageLead")}</p>
        {USAGE_GROUPS.map((group) => (
          <div className="cursor-usage-group" key={group.titleKey}>
            <div className="cursor-usage-group-title">{t(`cursor.${group.titleKey}`)}</div>
            <ul className="cursor-usage-list">
              {group.itemKeys.map((key) => (
                <li key={key}>{t(`cursor.${key}`)}</li>
              ))}
            </ul>
          </div>
        ))}
      </div>

      {showModal && (
        <div className="modal-overlay">
          <div className="modal">
            <ModalTitleRow
              title={editingAccount ? t("cursor.editTitle") : t("cursor.addTitle")}
              onClose={() => setShowModal(false)}
              disabled={busy}
            />
            <div className="input-group">
              <label className="input-label">{t("cursor.nameLabel")}</label>
              <input
                className="input-field"
                value={formData.name}
                onChange={(e) => setFormData({ ...formData, name: e.target.value })}
                placeholder={t("cursor.namePlaceholder")}
                disabled={busy}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("cursor.emailLabel")}</label>
              <input
                className="input-field"
                value={formData.email}
                onChange={(e) => setFormData({ ...formData, email: e.target.value })}
                placeholder={t("cursor.emailPlaceholder")}
                disabled={busy}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("cursor.password")}</label>
              <div className="cursor-password-input-wrap">
                <input
                  className="input-field"
                  type={showPassword ? "text" : "password"}
                  value={formData.password}
                  onChange={(e) => setFormData({ ...formData, password: e.target.value })}
                  placeholder={t("cursor.passwordPlaceholder")}
                  autoComplete="off"
                  disabled={busy}
                />
                <button
                  type="button"
                  className="btn btn-secondary btn-small cursor-password-toggle"
                  onClick={() => setShowPassword((v) => !v)}
                  title={showPassword ? t("cursor.hidePassword") : t("cursor.showPassword")}
                  disabled={busy}
                >
                  {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
              </div>
              <p className="runtime-muted" style={{ marginTop: 4, fontSize: 12 }}>
                {t("cursor.passwordMemoNote")}
              </p>
            </div>
            <div className="input-group">
              <label className="input-label">{t("cursor.notesLabel")}</label>
              <textarea
                className="input-field"
                rows={2}
                value={formData.notes}
                onChange={(e) => setFormData({ ...formData, notes: e.target.value })}
                placeholder={t("cursor.notesPlaceholder")}
                style={{ resize: "vertical" }}
                disabled={busy}
              />
            </div>
            <div className="modal-actions">
              <button
                className="btn btn-secondary"
                onClick={() => setShowModal(false)}
                disabled={busy}
              >
                {t("cursor.cancel")}
              </button>
              <button className="btn btn-primary" onClick={handleSubmit} disabled={busy}>
                {busy ? (
                  <>
                    <Loader2 size={12} className="spin" />
                    {editingAccount ? t("cursor.saving") : t("cursor.workingModal")}
                  </>
                ) : editingAccount ? (
                  t("cursor.save")
                ) : (
                  t("cursor.add")
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" && <Check size={16} />}
            {message.type === "warning" && <AlertCircle size={16} />}
            {message.type === "error" && <XCircle size={16} />}
          </span>
          <span style={{ flex: 1 }}>{message.text}</span>
          {message.type === "error" && (
            <button
              type="button"
              className="btn btn-secondary btn-small"
              style={{ marginLeft: 8 }}
              onClick={() => invokeCopyToClipboard(message.text).catch(() => {})}
            >
              {t("cursor.copyError")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

export default CursorManager;
