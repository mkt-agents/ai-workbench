import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import {
  AppWindow,
  Check,
  FileSearch,
  Loader2,
  RefreshCw,
  Search,
  SlidersHorizontal,
} from "lucide-react";
import ModalTitleRow from "../ModalTitleRow";
import type { DesktopRunningApp, InstalledAppInfo } from "../../core/types";
import { hasProcess, processNameFromExe, type DesktopAppDef } from "../../lib/desktopApps";
import { cachedAppIcon, loadAppIcon, warmAppIcon } from "../../lib/appIcons";

/** The grid stops being readable past a few hundred tiles; search narrows it. */
const MAX_TILES = 120;

export interface AppLibraryDialogProps {
  /** Apps already in the bar — shown with an "added" badge, click = switch. */
  apps: DesktopAppDef[];
  /** Recently used app ids (newest first) — resolved against `apps`. */
  recentIds: string[];
  /** Start Menu / Desktop shortcuts (what a user calls "my apps"). */
  installed: InstalledAppInfo[];
  /** Registry + Program Files scan, only listed when the user asks for it. */
  installedAll: InstalledAppInfo[];
  /** True while the deferred full scan is in flight. */
  fullScanLoading?: boolean;
  running: DesktopRunningApp[];
  runningLoading: boolean;
  /** The running-scan IPC died: say so instead of implying "nothing is open". */
  runningScanFailed: boolean;
  /** The installed-scan IPC died: same deal, with a retry one click away. */
  installedScanFailed: boolean;
  onRefreshRunning: () => void;
  onRescanInstalled: () => void;
  /** User toggled “all programs” or typed a query that needs the full pool. */
  onNeedFullScan: () => void;
  /** Add a scanned app (and open it). */
  onPickInstalled: (info: InstalledAppInfo) => void;
  /** Add a running window's app (and open it). */
  onPickRunning: (running: DesktopRunningApp) => void;
  /** Fall back to the file picker. */
  onPickFile: () => void;
  /** The app is already in the bar: focus it instead of duplicating it. */
  onSwitch: (app: DesktopAppDef) => void;
  onClose: () => void;
}

/**
 * An app icon: the scan's base64 when it has one (it usually does not), else an
 * on-demand extraction shared with the tab strip, else a neutral placeholder.
 */
function IconTile({
  path,
  icon,
  size = 28,
}: {
  path?: string | null;
  icon?: string;
  size?: number;
}) {
  const [src, setSrc] = useState<string | null>(() => cachedAppIcon(path) ?? icon ?? null);

  useEffect(() => {
    if (icon) {
      warmAppIcon(path, icon);
      setSrc(icon);
      return;
    }
    if (!path) return;
    const cached = cachedAppIcon(path);
    if (cached) {
      setSrc(cached);
      return;
    }
    let cancelled = false;
    void loadAppIcon(path).then((value) => {
      if (!cancelled && value) setSrc(value);
    });
    return () => {
      cancelled = true;
    };
  }, [path, icon]);

  if (src) {
    return (
      <img
        className="dapps-lib-icon"
        style={{ width: size, height: size }}
        src={`data:image/png;base64,${src}`}
        alt=""
        draggable={false}
      />
    );
  }
  return (
    <span className="dapps-lib-icon is-empty" style={{ width: size, height: size }}>
      <AppWindow size={Math.round(size * 0.55)} />
    </span>
  );
}

type Selectable =
  | { kind: "app"; key: string; app: DesktopAppDef }
  | { kind: "running"; key: string; item: DesktopRunningApp }
  | { kind: "installed"; key: string; info: InstalledAppInfo };

/**
 * One place to find an app.
 *
 * Opens from cache, keeps running windows first, then installed shortcuts.
 * Full registry scan stays behind “全部程序” / a search that needs more hits.
 * Arrow keys + Enter pick from a flat selectable list without leaving search.
 */
export default function AppLibraryDialog({
  apps,
  recentIds,
  installed,
  installedAll,
  fullScanLoading = false,
  running,
  runningLoading,
  runningScanFailed,
  installedScanFailed,
  onRefreshRunning,
  onRescanInstalled,
  onNeedFullScan,
  onPickInstalled,
  onPickRunning,
  onPickFile,
  onSwitch,
  onClose,
}: AppLibraryDialogProps) {
  const { t } = useTranslation("desktopApps");
  const [query, setQuery] = useState("");
  const [showAll, setShowAll] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  const needle = query.trim().toLowerCase();
  const wantsFullPool = showAll || needle.length > 0;

  useEffect(() => {
    if (wantsFullPool) onNeedFullScan();
  }, [wantsFullPool, onNeedFullScan]);

  const matches = (fields: Array<string | null | undefined>) =>
    !needle || fields.some((field) => (field ?? "").toLowerCase().includes(needle));

  const installedRows = useMemo(() => {
    const pool = wantsFullPool ? [...installed, ...installedAll] : installed;
    const seen = new Set<string>();
    const rows = pool.filter((item) => {
      if (!item.exe_path) return false;
      const key = item.exe_path.toLowerCase();
      if (seen.has(key)) return false;
      seen.add(key);
      return matches([item.display_name, item.publisher, processNameFromExe(item.exe_path)]);
    });
    return rows.sort((a, b) => a.display_name.localeCompare(b.display_name));
  }, [installed, installedAll, wantsFullPool, needle]);

  const runningRows = useMemo(
    () =>
      running.filter((item) => matches([item.title, item.processName, item.class])),
    [running, needle]
  );

  const recentApps = useMemo(() => {
    const byId = new Map(apps.map((app) => [app.id, app]));
    return recentIds
      .map((id) => byId.get(id))
      .filter((app): app is DesktopAppDef => !!app)
      .filter((app) =>
        matches([app.label, app.labelKey, app.processName, app.exePath ?? ""])
      );
  }, [apps, recentIds, needle]);

  /** Existing entry for this process name, if the bar already has it. */
  const existing = (processName: string) =>
    apps.find(
      (app) => app.processName.trim().toLowerCase() === processName.trim().toLowerCase()
    ) ?? null;

  const addInstalled = (info: InstalledAppInfo) => {
    const found = existing(processNameFromExe(info.exe_path));
    if (found) onSwitch(found);
    else onPickInstalled(info);
  };

  const addRunning = (item: DesktopRunningApp) => {
    const found = existing(item.processName);
    if (found) onSwitch(found);
    else onPickRunning(item);
  };

  const shownTiles = installedRows.slice(0, MAX_TILES);
  const hiddenTiles = installedRows.length - shownTiles.length;

  const selectables = useMemo((): Selectable[] => {
    const items: Selectable[] = [];
    const seenProcess = new Set<string>();
    const mark = (processName: string) => {
      const key = processName.trim().toLowerCase();
      if (!key || seenProcess.has(key)) return false;
      seenProcess.add(key);
      return true;
    };
    for (const app of recentApps) {
      if (!mark(app.processName)) continue;
      items.push({ kind: "app", key: `recent:${app.id}`, app });
    }
    for (const item of runningRows) {
      if (!mark(item.processName)) continue;
      items.push({
        kind: "running",
        key: `run:${item.processName}-${item.pid}`,
        item,
      });
    }
    for (const info of shownTiles) {
      const processName = processNameFromExe(info.exe_path);
      if (!mark(processName)) continue;
      items.push({ kind: "installed", key: `inst:${info.exe_path}`, info });
    }
    return items;
  }, [recentApps, runningRows, shownTiles]);

  useEffect(() => {
    setActiveIndex(-1);
  }, [query, showAll, runningRows.length, installedRows.length, recentApps.length]);

  useEffect(() => {
    if (activeIndex < 0) return;
    const rows = listRef.current?.querySelectorAll<HTMLElement>("[data-lib-index]");
    rows?.[activeIndex]?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  const activate = (item: Selectable) => {
    if (item.kind === "app") onSwitch(item.app);
    else if (item.kind === "running") addRunning(item.item);
    else addInstalled(item.info);
  };

  const onSearchKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Enter") return;
    if (selectables.length === 0) return;
    if (event.key === "Enter") {
      if (event.nativeEvent.isComposing || event.keyCode === 229) return;
      if (activeIndex < 0 || activeIndex >= selectables.length) return;
      event.preventDefault();
      activate(selectables[activeIndex]);
      return;
    }
    event.preventDefault();
    setActiveIndex((prev) => {
      if (event.key === "ArrowDown") {
        return prev < 0 ? 0 : Math.min(prev + 1, selectables.length - 1);
      }
      return prev < 0 ? selectables.length - 1 : Math.max(prev - 1, 0);
    });
  };

  const selectableIndex = (key: string) => selectables.findIndex((item) => item.key === key);

  const bothEmpty =
    recentApps.length === 0 && runningRows.length === 0 && installedRows.length === 0;

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div
        className="modal dapps-lib"
        role="dialog"
        aria-modal="true"
        aria-label={t("library.title")}
        onClick={(event) => event.stopPropagation()}
      >
        <ModalTitleRow title={t("library.title")} onClose={onClose} />

        <div className="dapps-lib-body">
          <div className="dapps-lib-main">
            <div className="dapps-lib-searchbar">
              <Search size={14} />
              <input
                ref={searchRef}
                type="search"
                value={query}
                placeholder={t("library.search")}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={onSearchKeyDown}
              />
              <button
                type="button"
                className={`dapps-lib-toggle${showAll ? " is-on" : ""}`}
                title={t("library.showAll")}
                disabled={fullScanLoading && !showAll}
                onClick={() => setShowAll((value) => !value)}
              >
                {fullScanLoading && showAll ? (
                  <Loader2 size={13} className="dapps-spin" />
                ) : (
                  <SlidersHorizontal size={13} />
                )}
                {t("library.showAll")}
              </button>
            </div>

            <div className="dapps-lib-scroll" ref={listRef}>
              {bothEmpty ? (
                <div className="dapps-lib-empty">
                  <p>
                    {needle
                      ? t("library.empty")
                      : runningLoading || fullScanLoading
                        ? t("library.loading")
                        : installedScanFailed || runningScanFailed
                          ? t("library.scanFailed")
                          : t("library.emptyInstalled")}
                  </p>
                  {needle ? null : (
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={onRescanInstalled}
                    >
                      <RefreshCw size={12} />
                      {t("library.rescan")}
                    </button>
                  )}
                </div>
              ) : null}

              {recentApps.length > 0 ? (
                <section className="dapps-lib-group">
                  <p className="dapps-lib-group-head">
                    <span>{t("library.recent")}</span>
                    <span className="dapps-lib-count">{recentApps.length}</span>
                  </p>
                  <ul className="dapps-lib-rows">
                    {recentApps.map((app) => {
                      const key = `recent:${app.id}`;
                      const index = selectableIndex(key);
                      const title =
                        app.label ??
                        (app.labelKey ? t(app.labelKey) : app.processName);
                      return (
                        <li key={key}>
                          <button
                            type="button"
                            data-lib-index={index}
                            className={`dapps-lib-row${
                              index === activeIndex ? " is-active" : ""
                            }`}
                            onMouseEnter={() => setActiveIndex(index)}
                            onClick={() => onSwitch(app)}
                          >
                            <IconTile path={app.exePath} size={26} />
                            <span className="dapps-lib-row-main">
                              <span className="dapps-lib-row-title">{title}</span>
                              <span className="dapps-lib-row-sub">{app.processName}</span>
                            </span>
                            <span className="dapps-lib-row-action is-added">
                              {t("library.switch")}
                            </span>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </section>
              ) : null}

              {runningRows.length > 0 ? (
                <section className="dapps-lib-group">
                  <p className="dapps-lib-group-head">
                    <span>{t("library.tabs.running")}</span>
                    <span className="dapps-lib-count">{runningRows.length}</span>
                    <button
                      type="button"
                      className="dapps-lib-refresh"
                      disabled={runningLoading}
                      title={t("library.refresh")}
                      onClick={onRefreshRunning}
                    >
                      {runningLoading ? (
                        <Loader2 size={12} className="dapps-spin" />
                      ) : (
                        <RefreshCw size={12} />
                      )}
                    </button>
                  </p>
                  <p className="dapps-lib-note">{t("library.runningHint")}</p>
                  <ul className="dapps-lib-rows">
                    {runningRows.map((item) => {
                      const key = `run:${item.processName}-${item.pid}`;
                      const index = selectableIndex(key);
                      const added = hasProcess(apps, item.processName);
                      return (
                        <li key={key}>
                          <button
                            type="button"
                            data-lib-index={index}
                            className={`dapps-lib-row${
                              index === activeIndex ? " is-active" : ""
                            }`}
                            onMouseEnter={() => setActiveIndex(index)}
                            onClick={() => addRunning(item)}
                          >
                            <IconTile path={item.exePath} size={26} />
                            <span className="dapps-lib-row-main">
                              <span className="dapps-lib-row-title">
                                {item.title || item.processName}
                              </span>
                              <span className="dapps-lib-row-sub">
                                {item.processName}
                                {item.visible ? "" : ` · ${t("library.hidden")}`}
                              </span>
                            </span>
                            <span
                              className={`dapps-lib-row-action${added ? " is-added" : ""}`}
                            >
                              {added ? t("library.switch") : t("library.add")}
                            </span>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </section>
              ) : null}

              {shownTiles.length > 0 ? (
                <section className="dapps-lib-group">
                  <p className="dapps-lib-group-head">
                    <span>{t("library.tabs.installed")}</span>
                    <span className="dapps-lib-count">{installedRows.length}</span>
                  </p>
                  <div className="dapps-lib-grid">
                    {shownTiles.map((info) => {
                      const processName = processNameFromExe(info.exe_path);
                      const added = hasProcess(apps, processName);
                      const key = `inst:${info.exe_path}`;
                      const index = selectableIndex(key);
                      return (
                        <button
                          key={info.exe_path}
                          type="button"
                          data-lib-index={index}
                          className={`dapps-lib-card${added ? " is-added" : ""}${
                            index === activeIndex ? " is-active" : ""
                          }`}
                          title={`${info.display_name} · ${processName}`}
                          onMouseEnter={() => setActiveIndex(index)}
                          onClick={() => addInstalled(info)}
                        >
                          <IconTile path={info.exe_path} icon={info.icon} />
                          <span className="dapps-lib-card-name">{info.display_name}</span>
                          <span className="dapps-lib-card-sub">
                            {info.publisher || processName}
                          </span>
                          {added ? (
                            <span className="dapps-lib-badge" title={t("library.added")}>
                              <Check size={10} />
                            </span>
                          ) : null}
                        </button>
                      );
                    })}
                  </div>
                  {hiddenTiles > 0 ? (
                    <p className="dapps-lib-more">
                      {t("library.more", { total: hiddenTiles })}
                    </p>
                  ) : null}
                </section>
              ) : null}
            </div>

            {/* The manual .exe pick stays reachable from the same flow: one
                slim row, not a third tab to discover. */}
            <div className="dapps-lib-file-inline">
              <FileSearch size={15} />
              <span className="dapps-lib-file-hint">{t("library.fileHint")}</span>
              <button type="button" className="btn btn-secondary btn-small" onClick={onPickFile}>
                {t("library.fileButton")}
              </button>
            </div>
          </div>
        </div>

        <p className="dapps-lib-footer">{t("library.footer")}</p>
      </div>
    </div>
  );
}
