/**
 * Folder picker results for the vulnerability page: scan one directory tree,
 * tick the projects, and the chosen ones are registered (so findings have a
 * project to belong to) and queued for scanning. Reuses the test page's scan
 * results command — discovery is the same question in both places.
 */
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderGit2, Loader2 } from "lucide-react";
import { useGlobalStore } from "../../core/store";
import ModalTitleRow from "../ModalTitleRow";
import type { ScannedProject, TestProject } from "../../core/types";

type Props = {
  rootPath: string;
  initialProjects: ScannedProject[];
  onClose: () => void;
  /** The view registers + queues; the modal only decides *what*. */
  onSubmit: (chosen: ScannedProject[]) => Promise<void>;
};

export default function ScanVulnFolderModal({ rootPath, initialProjects, onClose, onSubmit }: Props) {
  const { t } = useTranslation("test");
  const { t: tv } = useTranslation("vuln");
  const { t: tc } = useTranslation("common");

  const testProjects = useGlobalStore((s) => s.testProjects);
  const scanTestProjects = useGlobalStore((s) => s.scanTestProjects);

  const [projects, setProjects] = useState(initialProjects);
  const [depth, setDepth] = useState(2);
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(initialProjects.map((p) => p.path))
  );
  const [filter, setFilter] = useState("");
  const [scanning, setScanning] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");

  const registered = useMemo(() => {
    const set = new Set<string>();
    for (const project of testProjects) {
      set.add(project.path.replace(/\\/g, "/").toLowerCase());
    }
    return set;
  }, [testProjects]);

  const isRegistered = (path: string) => registered.has(path.toLowerCase());
  const chosen = projects.filter((p) => selected.has(p.path));

  // Same long-list treatment as the test page's scan modal: sorted, filterable.
  const needle = filter.trim().toLowerCase();
  const shown = useMemo(
    () =>
      projects
        .filter(
          (p) => !needle || p.name.toLowerCase().includes(needle) || p.path.toLowerCase().includes(needle)
        )
        .sort((a, b) => a.name.localeCompare(b.name)),
    [projects, needle]
  );

  const toggle = (path: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

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
        for (const project of found) next.add(project.path);
        return next;
      });
      setDepth(nextDepth);
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
    }
  };

  const submit = async () => {
    if (submitting || chosen.length === 0) return;
    setSubmitting(true);
    setError("");
    try {
      await onSubmit(chosen);
    } catch (e) {
      setError(String(e));
      setSubmitting(false);
    }
  };

  return (
    <div className="modal-overlay" onMouseDown={(e) => !submitting && e.target === e.currentTarget && onClose()}>
      <div className="modal tm-scan-modal" onMouseDown={(e) => e.stopPropagation()}>
        <ModalTitleRow title={tv("folder.title", { count: projects.length })} onClose={onClose} disabled={submitting} />

        <div className="tm-scan-toolbar">
          <input
            type="search"
            className="input-field tm-scan-filter"
            placeholder={t("scan.filter", { defaultValue: "按名称/路径过滤" })}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            disabled={submitting}
          />
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => setSelected(new Set(shown.map((p) => p.path)))}
            disabled={shown.length === 0}
          >
            {t("scan.selectAll")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => setSelected(new Set())}
            disabled={selected.size === 0}
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
            disabled={scanning || submitting || depth >= 6}
            title={t("scan.deeperHint")}
          >
            {scanning ? <Loader2 size={12} className="spin" /> : null}
            {t("scan.deeper")}
          </button>
        </div>

        <p className="tm-hint">{tv("folder.note")}</p>

        <ul className="tm-scan-list">
          {shown.map((project) => (
            <li key={project.path} className="tm-scan-item" onClick={() => toggle(project.path)}>
              <input
                type="checkbox"
                checked={selected.has(project.path)}
                disabled={submitting}
                onChange={() => toggle(project.path)}
                onMouseDown={(e) => e.stopPropagation()}
              />
              <FolderGit2 size={14} className="tm-scan-icon" />
              <span className="tm-scan-name" title={project.name}>
                {project.name}
              </span>
              <span className={`tm-badge tm-badge-${project.framework}`}>{project.framework}</span>
              {isRegistered(project.path) && (
                <span className="runtime-badge active">{t("scan.alreadyAdded")}</span>
              )}
            </li>
          ))}
          {shown.length === 0 && (
            <li className="tm-scan-empty">{needle ? t("scan.noMatch", { defaultValue: "无匹配项目" }) : t("scan.noneFound")}</li>
          )}
        </ul>

        {error && <div className="repos-modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn btn-secondary" onClick={onClose} disabled={submitting}>
            {tc("actions.cancel")}
          </button>
          <button className="btn btn-primary" onClick={() => void submit()} disabled={submitting || chosen.length === 0}>
            {submitting ? <Loader2 size={14} className="spin" /> : null}
            {tv("folder.queue", { count: chosen.length })}
          </button>
        </div>
      </div>
    </div>
  );
}

/** Rows for `add_test_projects`; the vuln flow does not care about commands. */
export function toProjectRows(chosen: ScannedProject[]): Omit<TestProject, "id" | "createdAt" | "updatedAt">[] {
  return chosen.map((p) => ({
    name: p.name,
    path: p.path,
    type: p.projectType as TestProject["type"],
    framework: p.framework,
    testCommand: p.testCommand,
    workingDir: p.workingDir || undefined,
    enabled: true,
  }));
}
