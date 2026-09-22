import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderGit2, Loader2 } from "lucide-react";
import { useGlobalStore } from "../core/store";
import ModalTitleRow from "./ModalTitleRow";
import type { ScannedProject, TestProject } from "../core/types";

type Props = {
  /** The directory the user picked. */
  rootPath: string;
  /** First pass already done by the caller, so opening the modal is instant. */
  initialProjects: ScannedProject[];
  onClose: () => void;
  onAdded: (addedCount: number) => void;
};

const NOTE_KEYS = ["wrapper-missing", "no-test-sources", "skip-tests-property", "reactor"];

const normPath = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");

/** Folder dimension: which sub-directory a hit lives in, relative to the
 *  scanned root ("." = directly under it). A system is usually one parent
 *  folder of many service modules, so the list groups by this key. */
export function folderKey(path: string, rootPath: string): string {
  const full = normPath(path);
  const parent = full.slice(0, full.lastIndexOf("/"));
  const root = normPath(rootPath);
  if (!parent || parent.toLowerCase() === root.toLowerCase()) return ".";
  return parent.toLowerCase().startsWith(root.toLowerCase() + "/") ? parent.slice(root.length + 1) : parent;
}

export function groupByFolder<T extends { path: string }>(items: T[], rootPath: string): [string, T[]][] {
  const groups = new Map<string, T[]>();
  for (const item of items) {
    const key = folderKey(item.path, rootPath);
    const bucket = groups.get(key);
    if (bucket) bucket.push(item);
    else groups.set(key, [item]);
  }
  return [...groups.entries()].sort((a, b) =>
    a[0] === "." ? -1 : b[0] === "." ? 1 : a[0].localeCompare(b[0])
  );
}

/**
 * Recursive scan results: tick the projects to register, in one batch.
 * Already-registered paths are badged and disabled; "scan deeper" widens the search
 * without dropping the current selection.
 */
function ScanTestProjectsModal({ rootPath, initialProjects, onClose, onAdded }: Props) {
  const { t } = useTranslation("test");
  const { t: tc } = useTranslation("common");

  const testProjects = useGlobalStore((s) => s.testProjects);
  const scanTestProjects = useGlobalStore((s) => s.scanTestProjects);
  const addTestProjects = useGlobalStore((s) => s.addTestProjects);

  const [projects, setProjects] = useState(initialProjects);
  const [depth, setDepth] = useState(2);
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(initialProjects.map((p) => p.path))
  );
  const [filter, setFilter] = useState("");
  const [scanning, setScanning] = useState(false);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState("");

  const registered = useMemo(() => {
    const set = new Set<string>();
    for (const project of testProjects) {
      set.add(project.path.replace(/\\/g, "/").toLowerCase());
    }
    return set;
  }, [testProjects]);

  const isRegistered = (path: string) => registered.has(path.toLowerCase());
  const selectable = projects.filter((p) => !isRegistered(p.path));
  const selectedCount = selectable.filter((p) => selected.has(p.path)).length;

  // A big monorepo scan can list 50+ projects: sort by name, filter by
  // name/path, and park the already-registered ones in a collapsed group so
  // the tick-list shows only what the user can actually act on.
  const needle = filter.trim().toLowerCase();
  const matches = (p: ScannedProject) =>
    !needle || p.name.toLowerCase().includes(needle) || p.path.toLowerCase().includes(needle);
  const byName = (a: ScannedProject, b: ScannedProject) => a.name.localeCompare(b.name);
  const shownSelectable = useMemo(
    () => projects.filter((p) => !registered.has(p.path.toLowerCase()) && matches(p)).sort(byName),
    [projects, registered, needle]
  );
  const shownExisting = useMemo(
    () => projects.filter((p) => registered.has(p.path.toLowerCase()) && matches(p)).sort(byName),
    [projects, registered, needle]
  );

  const toggle = (path: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  };

  const scanDeeper = async () => {
    if (scanning) return;
    setScanning(true);
    setError("");
    try {
      const nextDepth = depth + 1;
      const found = await scanTestProjects(rootPath, nextDepth);
      const known = new Set(projects.map((p) => p.path));
      setProjects([...projects, ...found.filter((p) => !known.has(p.path))]);
      setSelected((prev) => {
        const next = new Set(prev);
        for (const project of found) {
          if (!isRegistered(project.path)) next.add(project.path);
        }
        return next;
      });
      setDepth(nextDepth);
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
    }
  };

  const handleAdd = async () => {
    const chosen = selectable.filter((p) => selected.has(p.path));
    if (adding || chosen.length === 0) return;
    setAdding(true);
    setError("");
    try {
      const rows: Omit<TestProject, "id" | "createdAt" | "updatedAt">[] = chosen.map((p) => ({
        name: p.name,
        path: p.path,
        type: p.projectType as TestProject["type"],
        framework: p.framework,
        testCommand: p.testCommand,
        workingDir: p.workingDir || undefined,
        enabled: true,
      }));
      const added = await addTestProjects(rows);
      onAdded(added);
    } catch (e) {
      setError(String(e));
      setAdding(false);
    }
  };

  const toggleGroup = (group: ScannedProject[], checked: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      for (const p of group) {
        if (checked) next.add(p.path);
        else next.delete(p.path);
      }
      return next;
    });
  };

  // One parent folder (the common monorepo case) would gain nothing from
  // headers; group only when the scan actually spans several directories.
  const groups = groupByFolder(shownSelectable, rootPath);
  const useGroups = groups.length > 1;

  const renderRow = (project: ScannedProject, existing: boolean) => {
    const checked = selected.has(project.path);
    return (
      <li
        key={project.path}
        className={`tm-scan-item${existing ? " is-existing" : ""}`}
        onClick={() => !existing && toggle(project.path)}
      >
        <input
          type="checkbox"
          checked={checked && !existing}
          disabled={existing || adding}
          onChange={() => !existing && toggle(project.path)}
          onMouseDown={(e) => e.stopPropagation()}
        />
        <FolderGit2 size={14} className="tm-scan-icon" />
        <span className="tm-scan-name" title={project.name}>
          {project.name}
        </span>
        <span className={`tm-badge tm-badge-${project.framework}`}>{project.framework}</span>
        <code className="tm-scan-cmd" title={project.workingDir ? `${project.workingDir} · ${project.testCommand}` : project.testCommand}>
          {project.testCommand}
        </code>
        <span className="tm-scan-notes">
          {project.notes
            .filter((note) => NOTE_KEYS.includes(note))
            .map((note) => (
              <span key={note} className={`tm-scan-note tm-scan-note-${note.replace(/[^a-z-]/g, "")}`}>
                {t(`scan.note.${note}`, { defaultValue: note })}
              </span>
            ))}
        </span>
        {existing && <span className="runtime-badge active">{t("scan.alreadyAdded")}</span>}
      </li>
    );
  };

  return (
    <div className="modal-overlay" onMouseDown={(e) => !adding && e.target === e.currentTarget && onClose()}>
      <div className="modal tm-scan-modal" onMouseDown={(e) => e.stopPropagation()}>
        <ModalTitleRow title={t("scan.results", { count: projects.length })} onClose={onClose} disabled={adding} />

        <div className="tm-scan-toolbar">
          <input
            type="search"
            className="input-field tm-scan-filter"
            placeholder={t("scan.filter", { defaultValue: "按名称/路径过滤" })}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            disabled={adding}
          />
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => setSelected(new Set(shownSelectable.map((p) => p.path)))}
            disabled={shownSelectable.length === 0}
          >
            {t("scan.selectAll")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => setSelected(new Set())}
            disabled={selectedCount === 0}
          >
            {t("scan.selectNone")}
          </button>
          <span className="tm-scan-depth">
            {t("scan.depth", { depth })}
            <span className="tm-scan-root" title={rootPath}>
              {rootPath}
            </span>
          </span>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => void scanDeeper()}
            disabled={scanning || adding || depth >= 6}
            title={t("scan.deeperHint")}
          >
            {scanning ? <Loader2 size={12} className="spin" /> : null}
            {t("scan.deeper")}
          </button>
        </div>

        <ul className="tm-scan-list">
          {useGroups
            ? groups.map(([folder, items]) => {
                const allChecked = items.every((p) => selected.has(p.path));
                return (
                  <li key={`g-${folder}`} className="tm-scan-group">
                    <label className="tm-scan-group-head">
                      <input
                        type="checkbox"
                        checked={allChecked}
                        disabled={adding}
                        onChange={(e) => toggleGroup(items, e.target.checked)}
                      />
                      <span className="tm-scan-group-name" title={folder === "." ? rootPath : folder}>
                        {folder === "." ? t("scan.groupRoot", { defaultValue: "（根目录）" }) : folder}
                      </span>
                      <span className="tm-scan-group-count">{items.length}</span>
                    </label>
                    <ul className="tm-scan-list tm-scan-list-nested">{items.map((project) => renderRow(project, false))}</ul>
                  </li>
                );
              })
            : shownSelectable.map((project) => renderRow(project, false))}
          {shownSelectable.length === 0 && !scanning && (
            <li className="tm-scan-empty">{needle ? t("scan.noMatch", { defaultValue: "无匹配项目" }) : t("scan.noneFound")}</li>
          )}
          {shownExisting.length > 0 && (
            <li className="tm-scan-existing">
              <details>
                <summary>{t("scan.existingGroup", { count: shownExisting.length, defaultValue: `已登记 ${shownExisting.length} 个（展开查看）` })}</summary>
                <ul className="tm-scan-list tm-scan-list-nested">{shownExisting.map((project) => renderRow(project, true))}</ul>
              </details>
            </li>
          )}
        </ul>

        {error && <div className="repos-modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn btn-secondary" onClick={onClose} disabled={adding}>
            {tc("actions.cancel")}
          </button>
          <button
            className="btn btn-primary"
            onClick={() => void handleAdd()}
            disabled={adding || selectedCount === 0}
          >
            {adding ? <Loader2 size={14} className="spin" /> : null}
            {t("scan.addSelected", { count: selectedCount })}
          </button>
        </div>
      </div>
    </div>
  );
}

export default ScanTestProjectsModal;
