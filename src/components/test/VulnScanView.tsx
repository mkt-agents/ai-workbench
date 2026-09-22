/**
 * Vulnerability page: dependency advisories (OSV) and secret hits per project.
 *
 * State discipline follows the test page — everything lives in SQLite or in the
 * module-level scan queue, so switching tabs never loses a running scan. This
 * component only renders and issues commands.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Ban,
  Bug,
  ChevronDown,
  Copy,
  FolderSearch,
  KeyRound,
  Loader2,
  RefreshCw,
  RotateCcw,
  ShieldAlert,
  ShieldCheck,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { cleanErrorMessage } from "../../core/errorCodes";
import {
  cancelActiveScan,
  enqueueScans,
  onScanFinished,
  useVulnScans,
} from "../../core/vulnScans";
import ScanVulnFolderModal, { toProjectRows } from "./ScanVulnFolderModal";
import { useConfirm } from "../ConfirmModal";
import type { TestProject, VulnFinding, VulnScanSummary, VulnTotals } from "../../core/types";
import type { ScannedProject } from "../../core/types";

type Props = {
  projects: TestProject[];
  onToast: (type: "success" | "error", text: string) => void;
};

const PAGE_SIZE = 100;
const SEVERITIES = ["critical", "high", "medium", "low", "unknown"];
const RULE_KEYS = ["awsAccessKey", "githubToken", "gitlabToken", "slackToken", "jwt", "privateKey", "genericSecret", "base64Secret"];

const EMPTY_TOTALS: VulnTotals = { total: 0, open: 0, critical: 0, high: 0, medium: 0, low: 0, unknown: 0, secrets: 0 };

export default function VulnScanView({ projects, onToast }: Props) {
  const { t } = useTranslation("vuln");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();

  const listVulnFindings = useGlobalStore((s) => s.listVulnFindings);
  const listVulnScans = useGlobalStore((s) => s.listVulnScans);
  const setVulnFindingStatus = useGlobalStore((s) => s.setVulnFindingStatus);
  const scanTestProjects = useGlobalStore((s) => s.scanTestProjects);
  const addTestProjects = useGlobalStore((s) => s.addTestProjects);
  const loadTestProjects = useGlobalStore((s) => s.loadTestProjects);
  const pickDirectory = useGlobalStore((s) => s.invokePickDirectory);

  const [projectId, setProjectId] = useState(projects[0]?.id ?? "");
  const [status, setStatus] = useState("open");
  const [kind, setKind] = useState("");
  const [severity, setSeverity] = useState("");
  const [page, setPage] = useState(0);
  const [includeUntracked, setIncludeUntracked] = useState(false);
  const [findings, setFindings] = useState<VulnFinding[]>([]);
  const [totals, setTotals] = useState<VulnTotals>(EMPTY_TOTALS);
  const [totalCount, setTotalCount] = useState(0);
  const [scans, setScans] = useState<VulnScanSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [expanded, setExpanded] = useState<number | null>(null);
  const [folder, setFolder] = useState<{ rootPath: string; projects: ScannedProject[] } | null>(null);
  const [scanningFolder, setScanningFolder] = useState(false);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // Keep the selection valid when the project list changes underneath us.
  useEffect(() => {
    if (projects.length && projectId !== "*" && !projects.some((p) => p.id === projectId)) {
      setProjectId(projects[0].id);
    }
  }, [projects, projectId]);

  const refresh = useCallback(async () => {
    if (!projectId) return;
    setLoading(true);
    try {
      const [pageResult, scanRows] = await Promise.all([
        listVulnFindings(projectId, {
          status: status || undefined,
          kind: kind || undefined,
          severity: severity || undefined,
          limit: PAGE_SIZE,
          offset: page * PAGE_SIZE,
        }),
        // Scan history is per project; the aggregate view has none of its own.
        projectId === "*" ? Promise.resolve([] as VulnScanSummary[]) : listVulnScans(projectId),
      ]);
      if (!alive.current) return;
      setFindings(pageResult.findings);
      setTotals(pageResult.totals);
      setTotalCount(pageResult.totalCount);
      setScans(scanRows);
    } catch (e) {
      onToast("error", String(e));
    } finally {
      if (alive.current) setLoading(false);
    }
  }, [listVulnFindings, listVulnScans, onToast, page, projectId, severity, kind, status]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const queue = useVulnScans();
  // "*" is the folder dimension: one report over every registered project.
  const allProjects = projectId === "*";
  const activeForProject = allProjects ? queue.active : queue.active && queue.active.projectId === projectId ? queue.active : null;
  const projectNameById = useMemo(() => new Map(projects.map((p) => [p.id, p.name])), [projects]);

  // Any scan that ends — from this page, the folder modal or after a tab
  // switch — refreshes what is on screen.
  useEffect(
    () =>
      onScanFinished((event) => {
        if (event.error) {
          onToast("error", `${event.task.projectName}: ${cleanErrorMessage(event.error)}`);
          return;
        }
        const outcome = event.outcome;
        if (outcome) {
          const text =
            outcome.status === "cancelled"
              ? t("scan.cancelled", { project: event.task.projectName })
              : t("scan.done", {
                  project: event.task.projectName,
                  deps: outcome.depsChecked,
                  n: outcome.total,
                });
          // Disclose limitations (maven audit unsupported, OSV 300-cap) rather
          // than presenting a partial answer as complete.
          onToast(
            outcome.status === "error" ? "error" : "success",
            outcome.unsupported ? `${text}；${outcome.unsupported}` : text
          );
        }
        if (event.task.projectId === projectId) void refresh();
      }),
    [onToast, projectId, refresh, t]
  );

  const scanCurrent = () => {
    const project = projects.find((p) => p.id === projectId);
    if (!project) return;
    enqueueScans([{ projectId: project.id, projectName: project.name, includeUntracked }]);
  };

  /** The folder dimension: queue every registered project, one serial scan. */
  const scanAll = () => {
    enqueueScans(projects.map((p) => ({ projectId: p.id, projectName: p.name, includeUntracked })));
  };

  const pickFolder = useCallback(async () => {
    const dir = await pickDirectory();
    if (!dir) return;
    setScanningFolder(true);
    try {
      const found = await scanTestProjects(dir, 2);
      setFolder({ rootPath: dir, projects: found });
      if (found.length === 0) onToast("success", t("folder.noneFound"));
    } catch (e) {
      onToast("error", String(e));
    } finally {
      setScanningFolder(false);
    }
  }, [onToast, pickDirectory, scanTestProjects, t]);

  const queueFolder = useCallback(
    async (chosen: ScannedProject[]) => {
      // Register first (findings need a project to belong to; re-scanning a
      // registered path is free), then queue the scans in one go.
      const known = new Set(projects.map((p) => p.path.replace(/\\/g, "/").toLowerCase()));
      const missing = chosen.filter((p) => !known.has(p.path.toLowerCase()));
      if (missing.length > 0) await addTestProjects(toProjectRows(missing));
      await loadTestProjects();
      const fresh = useGlobalStore.getState().testProjects;
      const byPath = new Map<string, string>();
      for (const p of fresh) byPath.set(p.path.replace(/\\/g, "/").toLowerCase(), p.id);
      const tasks = chosen
        .map((p) => byPath.get(p.path.toLowerCase()))
        .filter((id): id is string => Boolean(id))
        .map((id) => ({
          projectId: id,
          projectName: fresh.find((p) => p.id === id)?.name ?? id,
          includeUntracked,
        }));
      enqueueScans(tasks);
      setFolder(null);
      onToast("success", t("folder.queued", { count: tasks.length }));
    },
    [addTestProjects, includeUntracked, loadTestProjects, onToast, projects, t]
  );

  const decide = async (finding: VulnFinding, next: string) => {
    try {
      await setVulnFindingStatus(finding.id, next);
      await refresh();
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const reopenAll = async () => {
    const ok = await confirm({
      title: t("action.reopenAll"),
      message: t("confirm.reopenAll"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      for (const decided of ["ignored", "false_positive"]) {
        const rows = await listVulnFindings(projectId, { status: decided, limit: 500 });
        for (const row of rows.findings) await setVulnFindingStatus(row.id, "open");
      }
      await refresh();
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      onToast("success", tc("copied", { defaultValue: "已复制" }));
    } catch {
      /* clipboard denied: no-op, the text is on screen anyway */
    }
  };

  const pages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE));

  const totalsChips = useMemo(
    () =>
      [
        { key: "critical", value: totals.critical },
        { key: "high", value: totals.high },
        { key: "medium", value: totals.medium },
        { key: "low", value: totals.low },
        { key: "secrets", value: totals.secrets },
      ].filter((chip) => chip.value > 0),
    [totals]
  );

  return (
    <div className="tm-vuln">
      <div className="tm-vuln-toolbar">
        <select className="input-field" value={projectId} onChange={(e) => { setProjectId(e.target.value); setPage(0); }} aria-label={t("aria.project")}>
          {projects.length === 0 && <option value="">{t("noProjects")}</option>}
          {projects.length > 1 && <option value="*">{t("project.all", { n: projects.length })}</option>}
          {projects.map((project) => (
            <option key={project.id} value={project.id}>
              {project.name}
            </option>
          ))}
        </select>
        <label className="tm-cr-toggle">
          <input type="checkbox" checked={includeUntracked} onChange={(e) => setIncludeUntracked(e.target.checked)} />
          {t("scan.includeUntracked")}
        </label>
        {allProjects ? (
          <button
            type="button"
            className="btn btn-primary btn-small"
            onClick={scanAll}
            disabled={projects.length === 0 || Boolean(queue.active) || queue.queue.length > 0}
          >
            {queue.active ? <Loader2 size={12} className="spin" /> : <ShieldAlert size={12} />}
            {t("scan.all", { n: projects.length })}
          </button>
        ) : (
          <button type="button" className="btn btn-primary btn-small" onClick={scanCurrent} disabled={!projectId || Boolean(activeForProject)}>
            {activeForProject ? <Loader2 size={12} className="spin" /> : <ShieldAlert size={12} />}
            {t("scan.one")}
          </button>
        )}
        <button type="button" className="btn btn-secondary btn-small" onClick={() => void pickFolder()} disabled={scanningFolder}>
          {scanningFolder ? <Loader2 size={12} className="spin" /> : <FolderSearch size={12} />}
          {t("scan.folder")}
        </button>
        {activeForProject && (
          <button type="button" className="btn btn-secondary btn-small" onClick={cancelActiveScan}>
            <Ban size={12} />
            {t("scan.cancel")}
          </button>
        )}
        <button type="button" className="btn btn-secondary btn-small" onClick={() => void refresh()} disabled={loading}>
          {loading ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
          {tc("refresh", { defaultValue: "刷新" })}
        </button>
      </div>

      {activeForProject && (
        <div className="tm-vuln-progress" role="status">
          <span className="tm-vuln-phase">{t(`phase.${activeForProject.phase}`, { defaultValue: activeForProject.phase })}</span>
          <span className="tm-vuln-bar" aria-hidden>
            <span
              className="tm-vuln-bar-fill"
              style={{ width: activeForProject.total > 0 ? `${Math.min(100, (activeForProject.done / activeForProject.total) * 100)}%` : "8%" }}
            />
          </span>
          <span className="tm-vuln-count">
            {activeForProject.total > 0 ? `${activeForProject.done}/${activeForProject.total}` : "…"}
          </span>
          {queue.queue.length > 0 && <span className="tm-vuln-queued">{t("scan.queued", { n: queue.queue.length })}</span>}
        </div>
      )}

      <div className="tm-vuln-totals">
        {totalsChips.length === 0 ? (
          <span className="tm-vuln-clean">
            <ShieldCheck size={13} /> {totals.total > 0 ? t("allHandled") : t("neverScanned")}
          </span>
        ) : (
          totalsChips.map((chip) => (
            <span key={chip.key} className={`tm-vuln-sev tm-vuln-sev-${chip.key === "secrets" ? "high" : chip.key}`}>
              {chip.key === "secrets" ? <KeyRound size={11} /> : null}
              {t(`totals.${chip.key}`, { count: chip.value, defaultValue: `${chip.key} ×${chip.value}` })}
            </span>
          ))
        )}
      </div>

      <div className="tm-vuln-filters">
        <select className="input-field" value={status} onChange={(e) => { setStatus(e.target.value); setPage(0); }} aria-label={t("aria.status")}>
          <option value="open">{t("statusFilter.open")}</option>
          <option value="ignored">{t("statusFilter.ignored")}</option>
          <option value="false_positive">{t("statusFilter.falsePositive")}</option>
          <option value="fixed">{t("statusFilter.fixed")}</option>
          <option value="">{t("statusFilter.all")}</option>
        </select>
        <select className="input-field" value={kind} onChange={(e) => { setKind(e.target.value); setPage(0); }} aria-label={t("aria.kind")}>
          <option value="">{t("kindFilter.all")}</option>
          <option value="dependency">{t("kind.dependency")}</option>
          <option value="secret">{t("kind.secret")}</option>
        </select>
        <select className="input-field" value={severity} onChange={(e) => { setSeverity(e.target.value); setPage(0); }} aria-label={t("aria.severity")}>
          <option value="">{t("sevFilter.all")}</option>
          {SEVERITIES.map((value) => (
            <option key={value} value={value}>
              {t(`sev.${value}`)}
            </option>
          ))}
        </select>
        <span className="tm-vuln-total">{t("count", { n: totalCount })}</span>
        {status === "ignored" && totalCount > 0 && (
          <button type="button" className="btn btn-secondary btn-small" onClick={() => void reopenAll()}>
            <RotateCcw size={12} />
            {t("action.reopenAll")}
          </button>
        )}
      </div>

      {findings.length === 0 && !loading ? (
        <div className="tm-empty">{t("empty")}</div>
      ) : (
        /* The 7-column table needs ~720px; let it scroll sideways instead of
           being clipped by the page's overflow-x:hidden on narrow windows. */
        <div className="tm-vuln-tablewrap">
        <table className="tm-table tm-vuln-table">
          <thead>
            <tr>
              <th aria-label="" />
              <th>{t("col.severity")}</th>
              <th>{t("col.kind")}</th>
              {allProjects && <th>{t("col.project", { defaultValue: "项目" })}</th>}
              <th>{t("col.package")}</th>
              <th>{t("col.advisory")}</th>
              <th>{t("col.status")}</th>
              <th>{t("col.actions")}</th>
            </tr>
          </thead>
          <tbody>
            {findings.map((finding) => (
              <FindingRow
                key={`${projectId}-${finding.id}`}
                finding={finding}
                expanded={expanded === finding.id}
                showProject={allProjects}
                projectName={projectNameById.get(finding.projectId) ?? finding.projectId}
                onToggle={() => setExpanded(expanded === finding.id ? null : finding.id)}
                onDecide={decide}
                onCopy={copy}
              />
            ))}
          </tbody>
        </table>
        </div>
      )}

      {pages > 1 && (
        <div className="tm-vuln-pager">
          <button type="button" className="btn btn-secondary btn-small" disabled={page === 0} onClick={() => setPage(page - 1)}>
            {tc("actions.prev", { defaultValue: "上一页" })}
          </button>
          <span>
            {page + 1} / {pages}
          </span>
          <button type="button" className="btn btn-secondary btn-small" disabled={page + 1 >= pages} onClick={() => setPage(page + 1)}>
            {tc("actions.next", { defaultValue: "下一页" })}
          </button>
        </div>
      )}

      {scans.length > 0 && (
        <details className="tm-vuln-history">
          <summary>{t("history.title")}</summary>
          <ul>
            {scans.map((scan) => (
              <li key={scan.id}>
                <span className={`tm-cr-run-status tm-cr-run-${scan.status}`}>
                  {t(`runStatus.${scan.status}`, { defaultValue: scan.status })}
                </span>
                <span>{new Date(scan.startedAt).toLocaleString()}</span>
                <span>{t("history.deps", { n: scan.depsChecked })}</span>
                <span>{t("history.files", { n: scan.filesChecked })}</span>
                <span>{t("history.findings", { n: scan.findingsTotal })}</span>
                {scan.errorKind && <span className="tm-cr-risk">{t(`errorKind.${scan.errorKind}`, { defaultValue: scan.errorKind })}</span>}
              </li>
            ))}
          </ul>
        </details>
      )}

      {folder && (
        <ScanVulnFolderModal
          rootPath={folder.rootPath}
          initialProjects={folder.projects}
          onClose={() => setFolder(null)}
          onSubmit={queueFolder}
        />
      )}
    </div>
  );
}

function FindingRow({
  finding,
  expanded,
  showProject,
  projectName,
  onToggle,
  onDecide,
  onCopy,
}: {
  finding: VulnFinding;
  expanded: boolean;
  /** The aggregate ("all projects") report says which system a hit belongs to. */
  showProject?: boolean;
  projectName?: string;
  onToggle: () => void;
  onDecide: (finding: VulnFinding, next: string) => Promise<void>;
  onCopy: (text: string) => void;
}) {
  const { t } = useTranslation("vuln");
  const isSecret = finding.kind === "secret";
  return (
    <>
      <tr className={`tm-vuln-row${expanded ? " is-expanded" : ""}`}>
        <td className="tm-vuln-expander" onClick={onToggle}>
          <ChevronDown size={12} className={expanded ? "is-open" : ""} />
        </td>
        <td>{finding.severity && finding.kind !== "secret" ? severityChip(finding.severity) : isSecret ? severityChip("high") : null}</td>
        <td>
          <span className="tm-vuln-kind">
            {isSecret ? <KeyRound size={12} /> : <Bug size={12} />}
            {t(`kind.${finding.kind}`, { defaultValue: finding.kind })}
          </span>
        </td>
        {showProject && (
          <td className="tm-vuln-project" title={projectName}>
            {projectName}
          </td>
        )}
        <td className="tm-vuln-target" title={isSecret ? `${finding.file}:${finding.line}` : `${finding.ecosystem} ${finding.package}@${finding.version}`}>
          {isSecret ? `${finding.file}:${finding.line}` : `${finding.package}@${finding.version}`}
        </td>
        <td className="tm-vuln-advisory" title={isSecret ? finding.preview : finding.summary}>
          {isSecret ? `${t(`rule.${finding.rule}`, { defaultValue: finding.rule })} · ${finding.preview}` : `${finding.vulnId} · ${finding.summary}`}
        </td>
        <td>
          <span className={`tm-vuln-status tm-vuln-status-${finding.status}`}>
            {t(`status.${finding.status}`, { defaultValue: finding.status })}
          </span>
        </td>
        <td className="tm-vuln-actions">
          {!isSecret && (
            <button type="button" className="btn btn-secondary btn-small" onClick={() => void onCopy(finding.vulnId)} title={t("action.copy")}>
              <Copy size={11} />
            </button>
          )}
          {finding.status === "open" && (
            <>
              <button type="button" className="btn btn-secondary btn-small" onClick={() => void onDecide(finding, "ignored")}>
                {t("action.ignore")}
              </button>
              <button type="button" className="btn btn-secondary btn-small" onClick={() => void onDecide(finding, "false_positive")}>
                {t("action.falsePositive")}
              </button>
            </>
          )}
          {finding.status !== "open" && (
            <button type="button" className="btn btn-secondary btn-small" onClick={() => void onDecide(finding, "open")}>
              {t("action.reopen")}
            </button>
          )}
        </td>
      </tr>
      {expanded && (
        <tr className="tm-vuln-detail-row">
          <td colSpan={showProject ? 8 : 7}>
            <div className="tm-vuln-detail">
              {isSecret ? (
                <p>
                  {t("detail.secretLine", { file: finding.file, line: finding.line, preview: finding.preview })}
                  {" · "}
                  {RULE_KEYS.includes(finding.rule) ? t(`ruleHelp.${finding.rule}`, { defaultValue: finding.rule }) : finding.rule}
                </p>
              ) : (
                <>
                  {finding.summary && <p>{finding.summary}</p>}
                  {finding.fixedVersions.length > 0 && (
                    <p>
                      {t("detail.fixed")}: <code>{finding.fixedVersions.join(", ")}</code>
                    </p>
                  )}
                  {finding.aliases.length > 0 && (
                    <p>
                      {t("detail.aliases")}: <code>{finding.aliases.join(", ")}</code>
                    </p>
                  )}
                  <p className="tm-vuln-when">
                    {t("detail.firstSeen")}: {new Date(finding.firstSeen).toLocaleString()} · {t("detail.lastSeen")}:{" "}
                    {new Date(finding.lastSeen).toLocaleString()}
                  </p>
                </>
              )}
            </div>
          </td>
        </tr>
      )}
    </>
  );
}

function severityChip(value: string) {
  const { t } = useTranslation("vuln");
  return <span className={`tm-vuln-sev tm-vuln-sev-${value}`}>{t(`sev.${value}`, { defaultValue: value })}</span>;
}

