import { useEffect, useMemo, useRef, useState } from "react";
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
  /** Start Menu / Desktop shortcuts (what a user calls "my apps"). */
  installed: InstalledAppInfo[];
  /** Registry + Program Files scan, only listed when the user asks for it. */
  installedAll: InstalledAppInfo[];
  running: DesktopRunningApp[];
  runningLoading: boolean;
  /** The running-scan IPC died: say so instead of implying "nothing is open". */
  runningScanFailed: boolean;
  /** The installed-scan IPC died: same deal, with a retry one click away. */
  installedScanFailed: boolean;
  onRefreshRunning: () => void;
  onRescanInstalled: () => void;
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

/**
 * One place to find an app.
 *
 * The three-way tab split (installed / running / file) made "add an app" a
 * route-planning exercise: the wanted app might sit behind any tab, and the
 * same search had to be retyped per tab. One search box now filters ONE list
 * with two groups — what is running right now first (it can move in
 * instantly), then what is installed — and picking an .exe stays one click
 * away at the bottom for anything the scans missed.
 */
export default function AppLibraryDialog({
  apps,
  installed,
  installedAll,
  running,
  runningLoading,
  runningScanFailed,
  installedScanFailed,
  onRefreshRunning,
  onRescanInstalled,
  onPickInstalled,
  onPickRunning,
  onPickFile,
  onSwitch,
  onClose,
}: AppLibraryDialogProps) {
  const { t } = useTranslation("desktopApps");
  const [query, setQuery] = useState("");
  const [showAll, setShowAll] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  const needle = query.trim().toLowerCase();
  const matches = (fields: Array<string | null | undefined>) =>
    !needle || fields.some((field) => (field ?? "").toLowerCase().includes(needle));

  const installedRows = useMemo(() => {
    const pool = showAll ? [...installed, ...installedAll] : installed;
    const seen = new Set<string>();
    const rows = pool.filter((item) => {
      if (!item.exe_path) return false;
      const key = item.exe_path.toLowerCase();
      if (seen.has(key)) return false;
      seen.add(key);
      return matches([item.display_name, item.publisher, processNameFromExe(item.exe_path)]);
    });
    return rows.sort((a, b) => a.display_name.localeCompare(b.display_name));
  }, [installed, installedAll, showAll, needle]);

  const runningRows = useMemo(
    () =>
      running.filter((item) =>
        matches([item.title, item.processName, item.class])
      ),
    [running, needle]
  );

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
  const bothEmpty = runningRows.length === 0 && installedRows.length === 0;

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
              />
              <button
                type="button"
                className={`dapps-lib-toggle${showAll ? " is-on" : ""}`}
                title={t("library.showAll")}
                onClick={() => setShowAll((value) => !value)}
              >
                <SlidersHorizontal size={13} />
                {t("library.showAll")}
              </button>
            </div>

            <div className="dapps-lib-scroll">
              {bothEmpty ? (
                <div className="dapps-lib-empty">
                  <p>
                    {needle
                      ? t("library.empty")
                      : runningLoading
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
                      const added = hasProcess(apps, item.processName);
                      return (
                        <li key={`${item.processName}-${item.pid}`}>
                          <button
                            type="button"
                            className="dapps-lib-row"
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
                      return (
                        <button
                          key={info.exe_path}
                          type="button"
                          className={`dapps-lib-card${added ? " is-added" : ""}`}
                          title={`${info.display_name} · ${processName}`}
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
