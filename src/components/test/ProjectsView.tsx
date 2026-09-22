/**
 * The projects workbench view: registered test projects, their cards, and the
 * modals that operate on one project.
 *
 * All run state (live tail, results, elapsed, cancel) lives in the testRuns
 * module store and is displayed by RunDock at the shell level — this component
 * only starts runs and paints their derived flags, so unmounting on tab switch
 * can no longer "lose" a running suite.
 */
import { useCallback, useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  CheckCircle,
  Clock,
  Edit,
  FlaskConical,
  FolderOpen,
  GitCompare,
  History,
  Loader2,
  Play,
  Plus,
  RefreshCw,
  Sparkles,
  Square,
  Trash2,
  TrendingUp,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { useConfirm } from "../ConfirmModal";
import TestModal from "../TestModal";
import TestGenerator from "../TestGenerator";
import FailureDiagnosis from "../FailureDiagnosis";
import CoverageReportView from "../CoverageReport";
import ScanTestProjectsModal from "../ScanTestProjectsModal";
import OutputPanel from "./OutputPanel";
import { mapPool, TEST_RUN_CONCURRENCY } from "../../core/asyncPool";
import {
  activeRunOf,
  cancelRun,
  getRunsSnapshot,
  latestFinishedOf,
  startRun,
  subscribeRuns,
} from "../../core/testRuns";
import type { RunSession, RunSource } from "../../core/testRuns";
import { outcomeLabel } from "../../core/testStatus";
import type {
  ScannedProject,
  TestHistoryEntry,
  TestProject,
  TestRunOutcome,
} from "../../core/types";
import type { TestRunResult } from "../../core/types";
import "../TestManager.css";

type ToastFn = (type: "success" | "error", text: string) => void;

type Draft = {
  name: string;
  path: string;
  type: TestProject["type"];
  framework: string;
  testCommand: string;
  args: string;
  workingDir: string;
  enabled: boolean;
  env: { key: string; value: string }[];
};

const EMPTY_DRAFT: Draft = {
  name: "",
  path: "",
  type: "frontend",
  framework: "",
  testCommand: "",
  args: "",
  workingDir: "",
  enabled: true,
  env: [],
};

/** Frameworks `read_coverage_report` can actually parse. */
const COVERAGE_FRAMEWORKS = [
  "jest",
  "vitest",
  "mocha",
  "playwright",
  "cargo",
  "pytest",
  "maven",
  "gotest",
];

const PROJECT_TYPES: TestProject["type"][] = [
  "frontend",
  "backend",
  "rust",
  "python",
  "go",
  "java",
  "csharp",
  "custom",
];

const draftOf = (project: TestProject): Draft => ({
  name: project.name,
  path: project.path,
  type: project.type,
  framework: project.framework,
  testCommand: project.testCommand,
  args: project.args ?? "",
  workingDir: project.workingDir ?? "",
  enabled: project.enabled,
  env: Object.entries(project.env ?? {}).map(([key, value]) => ({ key, value })),
});

/** A stored run re-opened from history: shown through the same panel as live ones. */
function sessionOfResult(result: TestRunResult, projectName: string): RunSession {
  return {
    runId: result.id,
    projectId: result.projectId,
    projectName,
    source: "manual",
    startedAt: Date.parse(result.startedAt) || Date.now(),
    endedAt: Date.parse(result.completedAt) || undefined,
    lines: [],
    status: result.status,
    result,
  };
}

export default function ProjectsView({
  onRevealRun,
  onOpenReports,
  onToast,
}: {
  onRevealRun?: (runId: string) => void;
  onOpenReports?: (projectId: string) => void;
  onToast: ToastFn;
}) {
  const { t } = useTranslation("test");
  const confirm = useConfirm();

  const testProjects = useGlobalStore((s) => s.testProjects);
  const loadTestProjects = useGlobalStore((s) => s.loadTestProjects);
  const addTestProject = useGlobalStore((s) => s.addTestProject);
  const updateTestProject = useGlobalStore((s) => s.updateTestProject);
  const deleteTestProject = useGlobalStore((s) => s.deleteTestProject);
  const detectProjectType = useGlobalStore((s) => s.detectProjectType);
  const scanTestProjects = useGlobalStore((s) => s.scanTestProjects);
  const getTestHistory = useGlobalStore((s) => s.getTestHistory);
  const getTestRun = useGlobalStore((s) => s.getTestRun);
  const pickDirectory = useGlobalStore((s) => s.invokePickDirectory);

  const sessions = useSyncExternalStore(subscribeRuns, getRunsSnapshot);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [batch, setBatch] = useState<{ done: number; total: number } | null>(null);
  const [query, setQuery] = useState("");

  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const [draftOpen, setDraftOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [detecting, setDetecting] = useState(false);

  const [scan, setScan] = useState<{ rootPath: string; projects: ScannedProject[] } | null>(null);
  const [scanning, setScanning] = useState(false);

  const [history, setHistory] = useState<TestHistoryEntry[]>([]);
  const [historyFor, setHistoryFor] = useState<TestProject | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [viewRun, setViewRun] = useState<RunSession | null>(null);

  const [generatorFor, setGeneratorFor] = useState<TestProject | null>(null);
  const [diagnosisFor, setDiagnosisFor] = useState<TestProject | null>(null);
  const [coverageFor, setCoverageFor] = useState<TestProject | null>(null);

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        await loadTestProjects();
      } catch (e) {
        if (alive) onToast("error", String(e));
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [loadTestProjects, onToast]);

  const lastSessionOf = useCallback(
    (projectId: string) => latestFinishedOf(projectId),
    [sessions] // eslint-disable-line react-hooks/exhaustive-deps
  );

  const runProject = useCallback(
    async (
      project: TestProject,
      options: { quiet?: boolean; args?: string; source?: RunSource; reportId?: string; coverage?: boolean } = {}
    ): Promise<RunSession | null> => {
      if (activeRunOf(project.id)) return null;
      const { done } = startRun({
        projectId: project.id,
        projectName: project.name,
        source: options.source ?? "manual",
        reportId: options.reportId,
        args: options.args,
        coverage: options.coverage,
      });
      const session = await done;
      if (!options.quiet) {
        if (session.result) {
          onToast(
            session.result.status === "success" ? "success" : "error",
            `${project.name} · ${outcomeLabel(t, session.result.status)} · ${t("runSummary", {
              passed: session.result.passed,
              failed: session.result.failed,
              skipped: session.result.skipped,
            })}`
          );
        } else {
          onToast("error", `${project.name}: ${session.error ?? t("error")}`);
        }
      }
      try {
        await loadTestProjects();
      } catch {
        /* the run itself already reached the dock */
      }
      return session;
    },
    [t, onToast, loadTestProjects]
  );

  const handleCancel = useCallback(
    async (projectId: string) => {
      try {
        await cancelRun(projectId);
        onToast("success", t("cancelRequested"));
      } catch (e) {
        onToast("error", String(e));
      }
    },
    [onToast, t]
  );

  // The coverage dialog closes while the run goes to the dock; re-opening it
  // afterwards is the "one-click rerun" loop the old page never had.
  const rerunWithCoverage = useCallback(
    async (project: TestProject) => {
      setCoverageFor(null);
      const session = await runProject(project, { source: "coverage-rerun", coverage: true });
      if (session && session.status !== "error") setCoverageFor(project);
    },
    [runProject]
  );

  const handleBatchRun = useCallback(async () => {
    const targets = testProjects.filter(
      (p) => selected.has(p.id) && !activeRunOf(p.id) && p.enabled
    );
    if (targets.length === 0) {
      onToast("error", t("selectAtLeastOne"));
      return;
    }
    setBatch({ done: 0, total: targets.length });
    const statuses: TestRunOutcome[] = [];
    let done = 0;
    await mapPool(targets, TEST_RUN_CONCURRENCY, async (project) => {
      const session = await runProject(project, { quiet: true, source: "batch" });
      statuses.push(session?.result?.status ?? "error");
      done += 1;
      setBatch({ done, total: targets.length });
    });
    setBatch(null);
    setSelected(new Set());
    const ok = statuses.filter((s) => s === "success").length;
    onToast(
      ok === statuses.length ? "success" : "error",
      t("batchDone", { ok, fail: statuses.length - ok })
    );
  }, [testProjects, selected, runProject, onToast, t]);

  const removeProjects = useCallback(
    async (ids: string[]) => {
      let failed = 0;
      for (const id of ids) {
        try {
          await deleteTestProject(id);
        } catch {
          failed += 1;
        }
      }
      onToast(
        failed === 0 ? "success" : "error",
        failed === 0 ? t("deleted") : t("deletePartial", { count: failed })
      );
    },
    [deleteTestProject, onToast, t]
  );

  const handleDelete = useCallback(
    async (project: TestProject) => {
      const ok = await confirm({
        title: t("deleteProject"),
        message: t("deleteConfirm", { name: project.name }),
        icon: "danger",
      });
      if (!ok) return;
      await removeProjects([project.id]);
      setSelected((prev) => {
        const next = new Set(prev);
        next.delete(project.id);
        return next;
      });
    },
    [confirm, removeProjects, t]
  );

  const handleBatchDelete = useCallback(async () => {
    const ids = [...selected];
    if (ids.length === 0) {
      onToast("error", t("selectAtLeastOne"));
      return;
    }
    const ok = await confirm({
      title: t("batchDelete"),
      message: t("confirmDeleteMultiple", { count: ids.length }),
      icon: "danger",
    });
    if (!ok) return;
    await removeProjects(ids);
    setSelected(new Set());
  }, [selected, confirm, removeProjects, onToast, t]);

  const toggleSelected = useCallback((id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handlePickPath = useCallback(async () => {
    const dir = await pickDirectory();
    if (!dir) return;
    setDraft((prev) => ({ ...prev, path: dir }));
    setFormError(null);
    setDetecting(true);
    try {
      const detection = await detectProjectType(dir);
      if (detection.detected) {
        setDraft((prev) => ({
          ...prev,
          name: prev.name || dir.split(/[/\\]/).pop() || "",
          type: (detection.projectType ?? prev.type) as TestProject["type"],
          framework: detection.framework || prev.framework,
          testCommand: detection.testCommand || prev.testCommand,
        }));
      }
    } catch (e) {
      onToast("error", String(e));
    } finally {
      setDetecting(false);
    }
  }, [pickDirectory, detectProjectType, onToast]);

  const openAdd = useCallback(() => {
    setDraft(EMPTY_DRAFT);
    setEditingId(null);
    setFormError(null);
    setDraftOpen(true);
  }, []);

  const openEdit = useCallback((project: TestProject) => {
    setDraft(draftOf(project));
    setEditingId(project.id);
    setFormError(null);
    setDraftOpen(true);
  }, []);

  const submitDraft = useCallback(async () => {
    if (!draft.name.trim() || !draft.path.trim() || !draft.testCommand.trim()) {
      setFormError(t("formIncomplete"));
      return;
    }
    setSaving(true);
    try {
      const env = draft.env
        .filter((row) => row.key.trim())
        .reduce<Record<string, string>>((acc, row) => {
          acc[row.key.trim()] = row.value;
          return acc;
        }, {});
      const payload = {
        name: draft.name.trim(),
        path: draft.path.trim(),
        type: draft.type,
        framework: draft.framework.trim() || "custom",
        testCommand: draft.testCommand.trim(),
        args: draft.args.trim() || undefined,
        workingDir: draft.workingDir.trim() || undefined,
        enabled: draft.enabled,
        env: Object.keys(env).length > 0 ? env : undefined,
      };
      if (editingId) {
        await updateTestProject(editingId, payload);
        onToast("success", t("saved"));
      } else {
        await addTestProject(payload);
        onToast("success", t("added"));
      }
      setDraftOpen(false);
      setEditingId(null);
    } catch (e) {
      setFormError(String(e));
    } finally {
      setSaving(false);
    }
  }, [draft, editingId, updateTestProject, addTestProject, onToast, t]);

  const addEnvRow = useCallback(
    () => setDraft((prev) => ({ ...prev, env: [...prev.env, { key: "", value: "" }] })),
    []
  );
  const updateEnvRow = useCallback(
    (index: number, patch: Partial<{ key: string; value: string }>) =>
      setDraft((prev) => ({
        ...prev,
        env: prev.env.map((row, i) => (i === index ? { ...row, ...patch } : row)),
      })),
    []
  );
  const removeEnvRow = useCallback(
    (index: number) =>
      setDraft((prev) => ({ ...prev, env: prev.env.filter((_, i) => i !== index) })),
    []
  );

  const handleScan = useCallback(async () => {
    const dir = await pickDirectory();
    if (!dir) return;
    setScanning(true);
    try {
      // Two levels by default: the common shape is `<picked>/<group>/<module>`.
      const projects = await scanTestProjects(dir, 2);
      setScan({ rootPath: dir, projects });
      if (projects.length === 0) onToast("success", t("scan.noneFound"));
    } catch (e) {
      onToast("error", String(e));
    } finally {
      setScanning(false);
    }
  }, [pickDirectory, scanTestProjects, onToast, t]);

  const openHistory = useCallback(
    async (project: TestProject) => {
      setHistoryFor(project);
      setHistory([]);
      setHistoryOpen(true);
      try {
        setHistory(await getTestHistory(project.id));
      } catch (e) {
        onToast("error", String(e));
      }
    },
    [getTestHistory, onToast]
  );

  const openStoredRun = useCallback(
    async (entry: TestHistoryEntry) => {
      if (!entry.runId) return;
      try {
        const result = await getTestRun(entry.runId);
        setViewRun(sessionOfResult(result, historyFor?.name ?? result.projectId));
        setHistoryOpen(false);
      } catch (e) {
        onToast("error", String(e));
      }
    },
    [getTestRun, onToast, historyFor]
  );

  const visibleProjects = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return testProjects;
    return testProjects.filter(
      (p) => p.name.toLowerCase().includes(q) || p.path.toLowerCase().includes(q)
    );
  }, [testProjects, query]);

  // Card elapsed chips need a heartbeat; the dock has its own.
  const anyRunning = sessions.some((s) => s.status === "running");
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    if (!anyRunning) return;
    const id = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [anyRunning]);

  if (loading) {
    return (
      <div className="tm-loading">
        <Loader2 size={16} className="spin" />
        <span>{t("loading")}</span>
      </div>
    );
  }

  return (
    <>
      <div className="tm-toolbar">
        <div className="tm-toolbar-info">
          <span className="tm-count">{t("projectsCount", { count: testProjects.length })}</span>
          {batch && (
            <span className="tm-batch-progress" role="status">
              {t("batchProgress", { done: batch.done, total: batch.total })}
            </span>
          )}
          {testProjects.length > 0 && (
            <input
              type="search"
              className="input-field tm-search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("searchPlaceholder")}
              aria-label={t("searchPlaceholder")}
            />
          )}
        </div>
        <div className="tm-toolbar-actions">
          {selected.size > 0 && (
            <>
              <span className="tm-selected-count">
                {t("selectedCount", { count: selected.size })}
              </span>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => void handleBatchRun()}
                disabled={!!batch}
              >
                <Play size={12} />
                {t("batchRun")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => void handleBatchDelete()}
              >
                <Trash2 size={12} />
                {t("batchDelete")}
              </button>
            </>
          )}
          <button
            type="button"
            className="btn btn-secondary"
            onClick={() => void handleScan()}
            disabled={scanning}
          >
            {scanning ? <Loader2 size={13} className="spin" /> : <FolderOpen size={13} />}
            {t("scanDirectory")}
          </button>
          <button
            type="button"
            className="btn btn-secondary btn-icon"
            onClick={() => void loadTestProjects().catch((e) => onToast("error", String(e)))}
            title={t("refresh")}
            aria-label={t("refresh")}
          >
            <RefreshCw size={14} />
          </button>
          <button type="button" className="btn btn-primary" onClick={openAdd}>
            <Plus size={13} />
            {t("addProject")}
          </button>
        </div>
      </div>

      {testProjects.length === 0 ? (
        <div className="empty-state">
          <div className="empty-state-icon">
            <FlaskConical size={22} />
          </div>
          <p>{t("noProjects")}</p>
          <button type="button" className="btn btn-primary" onClick={openAdd}>
            {t("addFirstProject")}
          </button>
        </div>
      ) : visibleProjects.length === 0 ? (
        <div className="tm-empty">{t("noMatches", { q: query })}</div>
      ) : (
        <div className="tm-list">
          {visibleProjects.map((project) => {
            const live = activeRunOf(project.id);
            const lastSession = lastSessionOf(project.id);
            const envCount = Object.keys(project.env ?? {}).length;
            const coverageSupported = COVERAGE_FRAMEWORKS.includes(project.framework);
            return (
              <div
                key={project.id}
                className={`tm-card${live ? " is-running" : ""}`}
              >
                <input
                  type="checkbox"
                  className="tm-check"
                  checked={selected.has(project.id)}
                  onChange={() => toggleSelected(project.id)}
                  aria-label={t("selectProject", { name: project.name })}
                />

                <div className="tm-card-main">
                  <div className="tm-card-head">
                    <button
                      type="button"
                      className="tm-name"
                      onClick={() => onRevealRun?.((lastSession ?? live)!.runId)}
                      disabled={!lastSession && !live}
                      title={lastSession ? t("viewResult") : undefined}
                    >
                      {project.name}
                    </button>
                    <span className={`tm-badge tm-badge-${project.framework}`}>
                      {project.framework}
                    </span>
                    {!project.enabled && (
                      <span className="tm-chip" title={t("disabledHint")}>
                        {t("disabled")}
                      </span>
                    )}
                    {envCount > 0 && (
                      <span className="tm-chip" title={t("envVars")}>
                        env {envCount}
                      </span>
                    )}
                    {project.lastStatus && !live && (
                      <span
                        className={`tm-chip tm-chip-${project.lastStatus}`}
                        title={
                          project.lastErrorKind
                            ? `${t("lastStatus")} · ${t(`errorKind.${project.lastErrorKind}`, {
                                defaultValue: project.lastErrorKind,
                              })}`
                            : t("lastStatus")
                        }
                      >
                        {project.lastStatus === "success" ? (
                          <CheckCircle size={11} />
                        ) : (
                          <XCircle size={11} />
                        )}
                        {outcomeLabel(t, project.lastStatus)}
                        {project.lastErrorKind && project.lastStatus !== "success" && (
                          <span className="tm-chip-kind">
                            {t(`errorKind.${project.lastErrorKind}`, {
                              defaultValue: project.lastErrorKind,
                            })}
                          </span>
                        )}
                      </span>
                    )}
                    {live && (
                      <span className="tm-chip tm-chip-running" role="status">
                        <Loader2 size={11} className="spin" />
                        {t("elapsed", {
                          sec: Math.max(0, Math.round((nowMs - live.startedAt) / 1000)),
                        })}
                      </span>
                    )}
                  </div>
                  <div className="tm-card-meta">
                    <span className="tm-path" title={project.path}>
                      {project.path}
                    </span>
                    <code className="tm-cmd">{project.testCommand}</code>
                    {project.args && <code className="tm-cmd">{project.args}</code>}
                  </div>
                  {project.lastRunAt && (
                    <div className="tm-card-time">
                      <Clock size={11} />
                      {t("lastRun")}: {new Date(project.lastRunAt).toLocaleString()}
                    </div>
                  )}
                </div>

                <div className="tm-card-actions">
                  {live ? (
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => void handleCancel(project.id)}
                      title={t("cancelRunHint")}
                    >
                      <Square size={12} />
                      {t("cancelRun")}
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="btn btn-primary btn-small"
                      onClick={() => void runProject(project)}
                      disabled={!!batch || !project.enabled}
                      title={project.enabled ? t("runHint") : t("disabledHint")}
                    >
                      <Play size={12} />
                      {t("runTests")}
                    </button>
                  )}
                  <button
                    type="button"
                    className="btn btn-secondary btn-icon"
                    onClick={() => setGeneratorFor(project)}
                    title={t("aiTestGenerator")}
                    aria-label={t("aiTestGenerator")}
                  >
                    <Sparkles size={14} />
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-icon"
                    onClick={() => setDiagnosisFor(project)}
                    title={t("aiFailureDiagnosis")}
                    aria-label={t("aiFailureDiagnosis")}
                  >
                    <AlertTriangle size={14} />
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-icon"
                    onClick={() => onOpenReports?.(project.id)}
                    title={t("cr.open")}
                    aria-label={t("cr.open")}
                  >
                    <GitCompare size={14} />
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-icon"
                    onClick={() => setCoverageFor(project)}
                    disabled={!coverageSupported}
                    title={coverageSupported ? t("viewCoverage") : t("coverageUnsupported")}
                    aria-label={t("viewCoverage")}
                  >
                    <TrendingUp size={14} />
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-icon"
                    onClick={() => void openHistory(project)}
                    title={t("viewHistory")}
                    aria-label={t("viewHistory")}
                  >
                    <History size={14} />
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-icon"
                    onClick={() => openEdit(project)}
                    title={t("editProject")}
                    aria-label={t("editProject")}
                  >
                    <Edit size={14} />
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary btn-icon tm-danger"
                    onClick={() => void handleDelete(project)}
                    title={t("deleteProject")}
                    aria-label={t("deleteProject")}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {draftOpen && (
        <TestModal
          title={editingId ? t("editProject") : t("addProject")}
          onClose={() => setDraftOpen(false)}
          lockClose={saving}
          footer={
            <>
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setDraftOpen(false)}
                disabled={saving}
              >
                {t("cancel")}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => void submitDraft()}
                disabled={saving}
              >
                {saving ? <Loader2 size={13} className="spin" /> : null}
                {editingId ? t("save") : t("add")}
              </button>
            </>
          }
        >
          <div className="tm-field">
            <label htmlFor="tm-name">{t("projectName")}</label>
            <input
              id="tm-name"
              className="input-field"
              value={draft.name}
              onChange={(e) => setDraft((prev) => ({ ...prev, name: e.target.value }))}
              placeholder={t("projectNamePlaceholder")}
            />
          </div>
          <div className="tm-field">
            <label htmlFor="tm-path">{t("projectPath")}</label>
            <div className="tm-field-row">
              <input
                id="tm-path"
                className="input-field"
                value={draft.path}
                readOnly
                placeholder={t("projectPathPlaceholder")}
              />
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => void handlePickPath()}
                disabled={detecting}
              >
                {detecting ? <Loader2 size={13} className="spin" /> : <FolderOpen size={13} />}
                {t("browse")}
              </button>
            </div>
            {detecting && <p className="tm-hint">{t("detecting")}</p>}
          </div>
          <div className="tm-field-grid">
            <div className="tm-field">
              <label htmlFor="tm-type">{t("projectType")}</label>
              <select
                id="tm-type"
                className="input-field"
                value={draft.type}
                onChange={(e) =>
                  setDraft((prev) => ({ ...prev, type: e.target.value as TestProject["type"] }))
                }
              >
                {PROJECT_TYPES.map((type) => (
                  <option key={type} value={type}>
                    {t(`type.${type}`)}
                  </option>
                ))}
              </select>
            </div>
            <div className="tm-field">
              <label htmlFor="tm-framework">{t("testFramework")}</label>
              <input
                id="tm-framework"
                className="input-field"
                value={draft.framework}
                onChange={(e) => setDraft((prev) => ({ ...prev, framework: e.target.value }))}
                placeholder={t("testFrameworkPlaceholder")}
                list="tm-framework-options"
              />
              <datalist id="tm-framework-options">
                {["jest", "vitest", "cargo", "pytest", "gotest", "maven", "custom"].map((f) => (
                  <option key={f} value={f} />
                ))}
              </datalist>
            </div>
          </div>
          <div className="tm-field">
            <label htmlFor="tm-command">{t("testCommand")}</label>
            <input
              id="tm-command"
              className="input-field"
              value={draft.testCommand}
              onChange={(e) => setDraft((prev) => ({ ...prev, testCommand: e.target.value }))}
              placeholder={t("testCommandPlaceholder")}
            />
          </div>
          <div className="tm-field-grid">
            <div className="tm-field">
              <label htmlFor="tm-args">{t("args")}</label>
              <input
                id="tm-args"
                className="input-field"
                value={draft.args}
                onChange={(e) => setDraft((prev) => ({ ...prev, args: e.target.value }))}
                placeholder={t("argsPlaceholder")}
              />
            </div>
            <div className="tm-field">
              <label htmlFor="tm-workdir">{t("workingDir")}</label>
              <input
                id="tm-workdir"
                className="input-field"
                value={draft.workingDir}
                onChange={(e) => setDraft((prev) => ({ ...prev, workingDir: e.target.value }))}
                placeholder={t("workingDirPlaceholder")}
              />
            </div>
          </div>
          <div className="tm-field">
            <div className="tm-env-head">
              <span className="tm-field-label">{t("envVars")}</span>
              <button type="button" className="btn btn-secondary btn-small" onClick={addEnvRow}>
                <Plus size={12} />
                {t("addEnv")}
              </button>
            </div>
            {draft.env.length === 0 ? (
              <p className="tm-hint">{t("envHint")}</p>
            ) : (
              <div className="tm-env-rows">
                {draft.env.map((row, index) => (
                  <div className="tm-env-row" key={index}>
                    <input
                      className="input-field"
                      value={row.key}
                      placeholder={t("envKey")}
                      aria-label={t("envKey")}
                      onChange={(e) => updateEnvRow(index, { key: e.target.value })}
                    />
                    <input
                      className="input-field"
                      value={row.value}
                      placeholder={t("envValue")}
                      aria-label={t("envValue")}
                      onChange={(e) => updateEnvRow(index, { value: e.target.value })}
                    />
                    <button
                      type="button"
                      className="btn btn-secondary btn-icon tm-danger"
                      onClick={() => removeEnvRow(index)}
                      title={t("removeEnv")}
                      aria-label={t("removeEnv")}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
          <p className="tm-hint">{t("workingDirHint")}</p>
          <label className="tm-check-row">
            <input
              type="checkbox"
              className="tm-check"
              checked={draft.enabled}
              onChange={(e) => setDraft((prev) => ({ ...prev, enabled: e.target.checked }))}
            />
            <span>
              {t("enabledField")}
              <span className="tm-hint"> · {t("disabledHint")}</span>
            </span>
          </label>
          {formError && (
            <p className="tm-form-error" role="alert">
              {formError}
            </p>
          )}
        </TestModal>
      )}

      {scan && (
        <ScanTestProjectsModal
          rootPath={scan.rootPath}
          initialProjects={scan.projects}
          onClose={() => setScan(null)}
          onAdded={(count) => {
            setScan(null);
            onToast("success", t("scan.addedN", { count }));
          }}
        />
      )}

      {historyOpen && historyFor && (
        <TestModal
          title={`${t("testHistory")} · ${historyFor.name}`}
          onClose={() => setHistoryOpen(false)}
          wide
        >
          {history.length === 0 ? (
            <div className="tm-empty">{t("noHistory")}</div>
          ) : (
            <table className="tm-table">
              <thead>
                <tr>
                  <th>{t("date")}</th>
                  <th>{t("status")}</th>
                  <th className="tm-num">{t("total")}</th>
                  <th className="tm-num">{t("passed")}</th>
                  <th className="tm-num">{t("failed")}</th>
                  <th aria-label={t("viewRun")} />
                </tr>
              </thead>
              <tbody>
                {history.map((entry) => (
                  <tr key={entry.id}>
                    <td className="tm-time">{new Date(entry.timestamp).toLocaleString()}</td>
                    <td>
                      <span className={`tm-chip tm-chip-${entry.status}`}>
                        {outcomeLabel(t, entry.status)}
                      </span>
                      {entry.errorKind && (
                        <span className="tm-chip">
                          {t(`errorKind.${entry.errorKind}`, { defaultValue: entry.errorKind })}
                        </span>
                      )}
                    </td>
                    <td className="tm-num">{entry.total ?? "-"}</td>
                    <td className="tm-num tm-m-pass">{entry.passed ?? "-"}</td>
                    <td className="tm-num tm-m-fail">{entry.failed ?? "-"}</td>
                    <td className="tm-num">
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        onClick={() => void openStoredRun(entry)}
                        disabled={!entry.runId}
                        title={entry.runId ? t("viewRunHint") : t("viewRunUnavailable")}
                      >
                        {t("viewRun")}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </TestModal>
      )}

      {viewRun && (
        <TestModal title={`${t("testResult")} · ${historyFor?.name ?? ""}`} onClose={() => setViewRun(null)} wide>
          <OutputPanel session={viewRun} nowMs={Date.now()} />
        </TestModal>
      )}

      {generatorFor && (
        <TestGenerator project={generatorFor} onClose={() => setGeneratorFor(null)} onToast={onToast} />
      )}
      {diagnosisFor && (
        <FailureDiagnosis
          project={diagnosisFor}
          onClose={() => setDiagnosisFor(null)}
          onToast={onToast}
        />
      )}
      {coverageFor && (
        <CoverageReportView
          project={coverageFor}
          onClose={() => setCoverageFor(null)}
          onRerunWithCoverage={() => void rerunWithCoverage(coverageFor)}
        />
      )}
    </>
  );
}
