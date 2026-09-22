/**
 * Change-verification reports, as a page view (the old ChangeReportModal lived
 * in a dialog with the history hidden in a footer dropdown; here the history
 * is a first-class list on the left and the workflow occupies the right).
 */
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { GitCompare, History, Trash2 } from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import ReportWorkflow from "./ReportWorkflow";
import type { ChangeReportSummary, TestProject } from "../../core/types";

type Props = {
  projects: TestProject[];
  /** Set when a project card asked to open its reports. */
  focusProjectId: string | null;
  onToast: (type: "success" | "error", text: string) => void;
  /** Jump to the vulnerability page (dependency exposure badge). */
  onOpenVuln?: () => void;
};

export default function ChangeReportsView({ projects, focusProjectId, onToast, onOpenVuln }: Props) {
  const { t } = useTranslation("test");
  const confirm = useConfirm();
  const listChangeReports = useGlobalStore((s) => s.listChangeReports);
  const deleteChangeReport = useGlobalStore((s) => s.deleteChangeReport);

  const [projectId, setProjectId] = useState(focusProjectId ?? projects[0]?.id ?? "");
  const [reports, setReports] = useState<ChangeReportSummary[]>([]);
  const [newestFirst, setNewestFirst] = useState(true);
  const [viewingId, setViewingId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (focusProjectId) setProjectId(focusProjectId);
  }, [focusProjectId]);

  const project = projects.find((p) => p.id === projectId) ?? null;

  const refresh = useCallback(async () => {
    if (!projectId) {
      setReports([]);
      return;
    }
    setLoading(true);
    try {
      const rows = await listChangeReports(projectId);
      setReports(rows);
      // Drop a selection that no longer exists (deleted elsewhere, project switch).
      setViewingId((prev) => (prev && rows.some((r) => r.id === prev) ? prev : null));
    } catch {
      setReports([]);
    } finally {
      setLoading(false);
    }
  }, [listChangeReports, projectId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const ordered = newestFirst ? reports : [...reports].reverse();

  const remove = async (row: ChangeReportSummary) => {
    const ok = await confirm({
      title: t("cr.delete"),
      message: t("cr.deleteConfirm", {
        when: new Date(row.createdAt).toLocaleString(),
        files: row.files,
      }),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await deleteChangeReport(row.id);
      if (viewingId === row.id) setViewingId(null);
      await refresh();
      onToast("success", t("cr.deleted"));
    } catch (e) {
      onToast("error", String(e));
    }
  };

  if (projects.length === 0) {
    return (
      <div className="tm-empty">
        {t("cr.noProjects")}
      </div>
    );
  }

  return (
    <div className="tm-cr-grid">
      <aside className="tm-cr-side">
        <div className="tm-cr-side-head">
          <select
            className="input-field"
            value={projectId}
            onChange={(e) => {
              setProjectId(e.target.value);
              setViewingId(null);
            }}
            aria-label={t("cr.project")}
          >
            {projects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={() => setNewestFirst((v) => !v)}
            title={t("cr.sort")}
          >
            <History size={12} />
            {newestFirst ? "↓" : "↑"}
          </button>
        </div>
        <div className="tm-cr-list">
          {loading && (
            <p className="tm-hint">{t("loading")}</p>
          )}
          {!loading && ordered.length === 0 && (
            <p className="tm-hint">{t("cr.listEmpty")}</p>
          )}
          <button
            type="button"
            className={`tm-cr-item is-new${viewingId === null ? " is-active" : ""}`}
            onClick={() => setViewingId(null)}
          >
            <GitCompare size={12} />
            {t("cr.newReport")}
          </button>
          {ordered.map((row) => (
            <div key={row.id} className={`tm-cr-item${viewingId === row.id ? " is-active" : ""}`}>
              <button
                type="button"
                className="tm-cr-item-main"
                onClick={() => setViewingId(row.id)}
              >
                <span className="tm-cr-item-date">{new Date(row.createdAt).toLocaleString()}</span>
                <span className="tm-cr-item-meta">
                  {row.base || t("cr.modeUncommitted")} · {row.files} {t("cr.files")} · +{row.adds} -{row.dels}
                  {row.untested > 0 ? ` · ${row.untested} ${t("cr.untested")}` : ""}
                  {row.hasAi ? " · AI" : ""}
                </span>
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-icon tm-cr-item-del"
                onClick={() => void remove(row)}
                title={t("cr.delete")}
                aria-label={t("cr.delete")}
              >
                <Trash2 size={12} />
              </button>
            </div>
          ))}
        </div>
      </aside>

      <main className="tm-cr-main">
        {project && (
          <ReportWorkflow
            key={`${project.id}:${viewingId ?? "new"}`}
            project={project}
            reportId={viewingId}
            onReportCreated={(id) => {
              setViewingId(id);
              void refresh();
            }}
            onReportDeleted={() => void refresh()}
            onToast={onToast}
            onOpenVuln={onOpenVuln}
          />
        )}
      </main>
    </div>
  );
}
