import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertTriangle,
  CheckCircle,
  Clock,
  Copy,
  Edit,
  FlaskConical,
  FolderOpen,
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
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import TestModal from "./TestModal";
import TestGenerator from "./TestGenerator";
import FailureDiagnosis from "./FailureDiagnosis";
import CoverageReportView from "./CoverageReport";
import { mapPool, TEST_RUN_CONCURRENCY } from "../core/asyncPool";
import type {
  ProjectDetectionResult,
  TestHistoryEntry,
  TestProject,
  TestRunOutcome,
  TestRunResult,
} from "../core/types";
import "./TestManager.css";

type Toast = { type: "success" | "error"; text: string };
type ToastFn = (type: Toast["type"], text: string) => void;

type Draft = {
  name: string;
  path: string;
  type: TestProject["type"];
  framework: string;
  testCommand: string;
  args: string;
  workingDir: string;
};

const EMPTY_DRAFT: Draft = {
  name: "",
  path: "",
  type: "frontend",
  framework: "",
  testCommand: "",
  args: "",
  workingDir: "",
};

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

/** Long logs freeze the webview, so the panel renders the tail first. */
const OUTPUT_PREVIEW_LINES = 300;

const toKey = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

const draftOf = (project: TestProject): Draft => ({
  name: project.name,
  path: project.path,
  type: project.type,
  framework: project.framework,
  testCommand: project.testCommand,
  args: project.args ?? "",
  workingDir: project.workingDir ?? "",
});

export default function TestManager() {
  const { t } = useTranslation("test");
  const confirm = useConfirm();

  const testProjects = useGlobalStore((s) => s.testProjects);
  const loadTestProjects = useGlobalStore((s) => s.loadTestProjects);
  const addTestProject = useGlobalStore((s) => s.addTestProject);
  const updateTestProject = useGlobalStore((s) => s.updateTestProject);
  const deleteTestProject = useGlobalStore((s) => s.deleteTestProject);
  const detectProjectType = useGlobalStore((s) => s.detectProjectType);
  const scanTestProjects = useGlobalStore((s) => s.scanTestProjects);
  const runTest = useGlobalStore((s) => s.runTest);
  const cancelTestRun = useGlobalStore((s) => s.cancelTestRun);
  const getTestHistory = useGlobalStore((s) => s.getTestHistory);
  const getTestRun = useGlobalStore((s) => s.getTestRun);
  const pickDirectory = useGlobalStore((s) => s.invokePickDirectory);

  const [loading, setLoading] = useState(true);
  // projectId -> epoch ms, which also feeds the live elapsed timer.
  const [runs, setRuns] = useState<Record<string, number>>({});
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [results, setResults] = useState<Record<string, TestRunResult>>({});
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [batch, setBatch] = useState<{ done: number; total: number } | null>(null);
  const [outputExpanded, setOutputExpanded] = useState(false);
  const [onlyFailed, setOnlyFailed] = useState(false);
  const [caseSearch, setCaseSearch] = useState("");

  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const [draftOpen, setDraftOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [detecting, setDetecting] = useState(false);

  const [scanBase, setScanBase] = useState("");
  const [scanResults, setScanResults] = useState<ProjectDetectionResult[]>([]);
  const [scanOpen, setScanOpen] = useState(false);
  const [scanning, setScanning] = useState(false);

  const [history, setHistory] = useState<TestHistoryEntry[]>([]);
  const [historyFor, setHistoryFor] = useState<TestProject | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);

  const [generatorFor, setGeneratorFor] = useState<TestProject | null>(null);
  const [diagnosisFor, setDiagnosisFor] = useState<TestProject | null>(null);
  const [coverageFor, setCoverageFor] = useState<TestProject | null>(null);

  const [toast, setToast] = useState<Toast | null>(null);
  const toastTimer = useRef<number | null>(null);

  const showMsg = useCallback<ToastFn>((type, text) => {
    setToast({ type, text });
    if (toastTimer.current) window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 4000);
  }, []);

  useEffect(
    () => () => {
      if (toastTimer.current) window.clearTimeout(toastTimer.current);
    },
    []
  );

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        await loadTestProjects();
      } catch (e) {
        if (alive) showMsg("error", String(e));
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [loadTestProjects, showMsg]);

  const runningCount = Object.keys(runs).length;
  useEffect(() => {
    if (runningCount === 0) return;
    const id = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [runningCount]);

  const outcomeLabel = useCallback(
    (status: TestRunOutcome): string => {
      switch (status) {
        case "success":
          return t("success");
        case "failed":
          return t("failed");
        case "cancelled":
          return t("statusCancelled");
        case "timeout":
          return t("statusTimeout");
        default:
          return t("error");
      }
    },
    [t]
  );

  const runOne = useCallback(
    async (project: TestProject, quiet = false): Promise<TestRunResult | null> => {
      if (runs[project.id]) return null;
      setRuns((prev) => ({ ...prev, [project.id]: Date.now() }));
      setFocusedId(project.id);
      setOutputExpanded(false);
      let finished: TestRunResult | null = null;
      try {
        const result = await runTest(project.id);
        finished = result;
        setResults((prev) => ({ ...prev, [project.id]: result }));
        if (!quiet) {
          showMsg(
            result.status === "success" ? "success" : "error",
            `${project.name} · ${outcomeLabel(result.status)} · ${t("runSummary", {
              passed: result.passed,
              failed: result.failed,
              skipped: result.skipped,
            })}`
          );
        }
      } catch (e) {
        if (!quiet) showMsg("error", `${project.name}: ${String(e)}`);
      } finally {
        setRuns((prev) => {
          const next = { ...prev };
          delete next[project.id];
          return next;
        });
        try {
          await loadTestProjects();
        } catch {
          /* the result already on screen is what matters */
        }
      }
      return finished;
    },
    [runs, runTest, loadTestProjects, showMsg, outcomeLabel, t]
  );

  const handleCancel = useCallback(
    async (project: TestProject) => {
      try {
        await cancelTestRun(project.id);
        showMsg("success", t("cancelRequested"));
      } catch (e) {
        showMsg("error", String(e));
      }
    },
    [cancelTestRun, showMsg, t]
  );

  const handleBatchRun = useCallback(async () => {
    const targets = testProjects.filter((p) => selected.has(p.id) && !runs[p.id]);
    if (targets.length === 0) {
      showMsg("error", t("selectAtLeastOne"));
      return;
    }
    setBatch({ done: 0, total: targets.length });
    const statuses: TestRunOutcome[] = [];
    let done = 0;
    await mapPool(targets, TEST_RUN_CONCURRENCY, async (project) => {
      const result = await runOne(project, true);
      statuses.push(result?.status ?? "error");
      done += 1;
      setBatch({ done, total: targets.length });
    });
    setBatch(null);
    setSelected(new Set());
    const ok = statuses.filter((s) => s === "success").length;
    showMsg(
      ok === statuses.length ? "success" : "error",
      t("batchDone", { ok, fail: statuses.length - ok })
    );
  }, [testProjects, selected, runs, runOne, showMsg, t]);

  const removeProjects = useCallback(
    async (ids: string[]) => {
      let failed = 0;
      for (const id of ids) {
        try {
          await deleteTestProject(id);
          setResults((prev) => {
            const next = { ...prev };
            delete next[id];
            return next;
          });
        } catch {
          failed += 1;
        }
      }
      showMsg(
        failed === 0 ? "success" : "error",
        failed === 0 ? t("deleted") : t("deletePartial", { count: failed })
      );
    },
    [deleteTestProject, showMsg, t]
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
      showMsg("error", t("selectAtLeastOne"));
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
  }, [selected, confirm, removeProjects, showMsg, t]);

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
      showMsg("error", String(e));
    } finally {
      setDetecting(false);
    }
  }, [pickDirectory, detectProjectType, showMsg]);

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
      const payload = {
        name: draft.name.trim(),
        path: draft.path.trim(),
        type: draft.type,
        framework: draft.framework.trim() || "custom",
        testCommand: draft.testCommand.trim(),
        args: draft.args.trim() || undefined,
        workingDir: draft.workingDir.trim() || undefined,
      };
      if (editingId) {
        await updateTestProject(editingId, { ...payload, enabled: true });
        showMsg("success", t("saved"));
      } else {
        await addTestProject({ ...payload, enabled: true });
        showMsg("success", t("added"));
      }
      setDraftOpen(false);
      setEditingId(null);
    } catch (e) {
      setFormError(String(e));
    } finally {
      setSaving(false);
    }
  }, [draft, editingId, updateTestProject, addTestProject, showMsg, t]);

  const handleScan = useCallback(async () => {
    const dir = await pickDirectory();
    if (!dir) return;
    setScanBase(dir);
    setScanOpen(true);
    setScanning(true);
    setScanResults([]);
    try {
      setScanResults(await scanTestProjects(dir));
    } catch (e) {
      showMsg("error", String(e));
      setScanOpen(false);
    } finally {
      setScanning(false);
    }
  }, [pickDirectory, scanTestProjects, showMsg]);

  const addFromScan = useCallback(
    async (result: ProjectDetectionResult) => {
      if (!result.testCommand) return;
      try {
        await addTestProject({
          name: result.path.split(/[/\\]/).pop() || result.path,
          path: result.path,
          type: (result.projectType ?? "custom") as TestProject["type"],
          framework: result.framework || "custom",
          testCommand: result.testCommand,
          enabled: true,
        });
        setScanResults((prev) => prev.filter((r) => r.path !== result.path));
        showMsg("success", t("added"));
      } catch (e) {
        showMsg("error", String(e));
      }
    },
    [addTestProject, showMsg, t]
  );

  const openHistory = useCallback(
    async (project: TestProject) => {
      setHistoryFor(project);
      setHistory([]);
      setHistoryOpen(true);
      try {
        setHistory(await getTestHistory(project.id));
      } catch (e) {
        showMsg("error", String(e));
      }
    },
    [getTestHistory, showMsg]
  );

  /** Re-open a stored run from the history list: same panel, same truncation rules. */
  const openStoredRun = useCallback(
    async (entry: TestHistoryEntry) => {
      if (!entry.runId) return;
      try {
        const result = await getTestRun(entry.runId);
        setOnlyFailed(false);
        setOutputExpanded(false);
        setResults((prev) => ({ ...prev, [result.projectId]: result }));
        setFocusedId(result.projectId);
        setHistoryOpen(false);
      } catch (e) {
        showMsg("error", String(e));
      }
    },
    [getTestRun, showMsg]
  );

  const knownPaths = useMemo(() => new Set(testProjects.map((p) => toKey(p.path))), [testProjects]);  const focused = focusedId ? testProjects.find((p) => p.id === focusedId) ?? null : null;
  const focusedResult = focusedId ? results[focusedId] ?? null : null;

  const outputView = useMemo(() => {
    if (!focusedResult) return null;
    const lines = focusedResult.output.split("\n");
    const trimmed = !outputExpanded && lines.length > OUTPUT_PREVIEW_LINES;
    return {
      totalLines: lines.length,
      text: trimmed ? lines.slice(-OUTPUT_PREVIEW_LINES).join("\n") : focusedResult.output,
      trimmed,
    };
  }, [focusedResult, outputExpanded]);

  // Failures first, both across suites and within a suite — the thing you are most
  // likely to act on should be the first thing you see.
  const sortedSuites = useMemo(() => {
    if (!focusedResult) return [];
    const byStatus = (s: string) => (s === "failed" ? 0 : s === "skipped" ? 1 : 2);
    return [...focusedResult.suites]
      .map((suite) => ({
        ...suite,
        tests: [...suite.tests].sort((a, b) => byStatus(a.status) - byStatus(b.status)),
      }))
      .sort((a, b) => {
        const aFail = a.tests.some((c) => c.status === "failed") ? 0 : 1;
        const bFail = b.tests.some((c) => c.status === "failed") ? 0 : 1;
        return aFail - bFail;
      });
  }, [focusedResult]);

  if (loading) {
    return (
      <div className="tm-page">
        <div className="tm-loading">
          <Loader2 size={16} className="spin" />
          <span>{t("loading")}</span>
        </div>
      </div>
    );
  }

  return (
    <div className="tm-page">
      <div className="tm-toolbar">
        <div className="tm-toolbar-info">
          <span className="tm-count">{t("projectsCount", { count: testProjects.length })}</span>
          {batch && (
            <span className="tm-batch-progress" role="status">
              {t("batchProgress", { done: batch.done, total: batch.total })}
            </span>
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
            onClick={() => void loadTestProjects().catch((e) => showMsg("error", String(e)))}
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
      ) : (
        <div className="tm-list">
          {testProjects.map((project) => {
            const startedAt = runs[project.id];
            const isRunning = startedAt !== undefined;
            const hasResult = !!results[project.id];
            return (
              <div
                key={project.id}
                className={`tm-card${focusedId === project.id ? " is-focused" : ""}${
                  isRunning ? " is-running" : ""
                }`}
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
                      onClick={() => setFocusedId(project.id)}
                      disabled={!hasResult}
                      title={hasResult ? t("viewResult") : undefined}
                    >
                      {project.name}
                    </button>
                    <span className={`tm-badge tm-badge-${project.framework}`}>
                      {project.framework}
                    </span>
                    {project.lastStatus && (
                      <span
                        className={`tm-chip tm-chip-${project.lastStatus}`}
                        title={t("lastStatus")}
                      >
                        {project.lastStatus === "success" ? (
                          <CheckCircle size={11} />
                        ) : (
                          <XCircle size={11} />
                        )}
                        {outcomeLabel(project.lastStatus)}
                      </span>
                    )}
                    {isRunning && (
                      <span className="tm-chip tm-chip-running" role="status">
                        <Loader2 size={11} className="spin" />
                        {t("elapsed", {
                          sec: Math.max(0, Math.round((nowMs - startedAt) / 1000)),
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
                  {isRunning ? (
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => void handleCancel(project)}
                      title={t("cancelRunHint")}
                    >
                      <Square size={12} />
                      {t("cancelRun")}
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="btn btn-primary btn-small"
                      onClick={() => void runOne(project)}
                      disabled={!!batch}
                      title={t("runHint")}
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
                    onClick={() => setCoverageFor(project)}
                    title={t("viewCoverage")}
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

      {focusedResult && focused && outputView && (
        <section className="tm-result" aria-label={t("testResult")}>
          <div className="tm-result-head">
            <h3>
              {t("testResult")} · {focused.name}
            </h3>
            <span className={`tm-chip tm-chip-${focusedResult.status}`}>
              {outcomeLabel(focusedResult.status)}
            </span>
            <div className="tm-result-metrics">
              <span>
                {t("duration")} <strong>{(focusedResult.durationMs / 1000).toFixed(2)}s</strong>
              </span>
              <span className="tm-m-pass">
                {t("passed")} <strong>{focusedResult.passed}</strong>
              </span>
              <span className="tm-m-fail">
                {t("failed")} <strong>{focusedResult.failed}</strong>
              </span>
              <span className="tm-m-skip">
                {t("skipped")} <strong>{focusedResult.skipped}</strong>
              </span>
              <span>
                {t("total")} <strong>{focusedResult.totalTests}</strong>
              </span>
            </div>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => void runOne(focused)}
              disabled={!!batch || !!runs[focused.id]}
              title={t("rerunHint")}
            >
              <RefreshCw size={12} />
              {t("rerun")}
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-icon"
              onClick={() => {
                setResults((prev) => {
                  const next = { ...prev };
                  delete next[focused.id];
                  return next;
                });
                setFocusedId(null);
              }}
              title={t("close")}
              aria-label={t("close")}
            >
              <XCircle size={14} />
            </button>
          </div>
          <div className="tm-result-toolbar">
            <span className="tm-output-label">{t("output")}</span>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() =>
                void navigator.clipboard
                  .writeText(focusedResult.output)
                  .then(() => showMsg("success", t("copied")))
                  .catch((e) => showMsg("error", String(e)))
              }
            >
              <Copy size={12} />
              {t("copy")}
            </button>
            {outputView.totalLines > OUTPUT_PREVIEW_LINES && (
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => setOutputExpanded((v) => !v)}
              >
                {outputView.trimmed
                  ? t("showAllLines", { count: outputView.totalLines })
                  : t("collapse")}
              </button>
            )}
          </div>
          {focusedResult.suites.length > 0 ? (
            <>
              <div className="tm-result-toolbar">
                <span className="tm-output-label">
                  {t("caseDetail", {
                    count: sortedSuites.reduce((n, s) => n + s.tests.length, 0),
                    files: sortedSuites.length,
                  })}
                </span>
                <input
                  type="search"
                  className="input-field tm-case-search"
                  value={caseSearch}
                  onChange={(e) => setCaseSearch(e.target.value)}
                  placeholder={t("caseSearchPlaceholder")}
                />
                <button
                  type="button"
                  className={`btn btn-small ${onlyFailed ? "btn-primary" : "btn-secondary"}`}
                  onClick={() => setOnlyFailed((v) => !v)}
                >
                  {t("onlyFailed")}
                </button>
              </div>
              <div className="tm-suites">
                {sortedSuites.map((suite) => {
                  const passedCases = suite.tests.filter((c) => c.status === "passed").length;
                  const failedCases = suite.tests.filter((c) => c.status === "failed").length;
                  const skippedCases = suite.tests.filter((c) => c.status === "skipped").length;
                  const search = caseSearch.trim().toLowerCase();
                  const cases = suite.tests.filter((c) => {
                    if (onlyFailed && c.status !== "failed") return false;
                    if (search && !c.name.toLowerCase().includes(search)) return false;
                    return true;
                  });
                  if (cases.length === 0) return null;
                  return (
                    <details key={suite.path} className="tm-suite" open={failedCases > 0}>
                      <summary className="tm-suite-head">
                        <span
                          className={`tm-suite-dot ${failedCases > 0 ? "is-fail" : "is-pass"}`}
                          aria-hidden
                        />
                        <span className="tm-suite-name" title={suite.path}>
                          {suite.name}
                        </span>
                        <span className="tm-suite-stat">
                          {suite.tests.length} · ✓{passedCases} ✗{failedCases} ○{skippedCases} ·{" "}
                          {suite.duration}ms
                        </span>
                      </summary>
                      <ul className="tm-cases">
                        {cases.map((entry) => (
                          <li key={entry.id} className={`tm-case tm-case-${entry.status}`}>
                            <span className="tm-case-mark" aria-hidden>
                              {entry.status === "passed" ? "✓" : entry.status === "failed" ? "✗" : "○"}
                            </span>
                            <span className="tm-case-name">{entry.name}</span>
                            <span className="tm-case-dur">{entry.duration}ms</span>
                            {entry.error && (
                              <pre className="tm-case-error">
                                {entry.error.stack
                                  ? `${entry.error.message}\n${entry.error.stack}`
                                  : entry.error.message}
                              </pre>
                            )}
                          </li>
                        ))}
                      </ul>
                    </details>
                  );
                })}
              </div>
            </>
          ) : (
            <p className="tm-hint">{t("noCaseDetail")}</p>
          )}

          <pre className="tm-output">{outputView.text}</pre>
        </section>
      )}

      {draftOpen && (
        <TestModal
          title={editingId ? t("editProject") : t("addProject")}
          onClose={() => setDraftOpen(false)}
          busy={saving}
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
                {["jest", "vitest", "cargo", "pytest", "gotest", "custom"].map((f) => (
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
          <p className="tm-hint">{t("workingDirHint")}</p>
          {formError && (
            <p className="tm-form-error" role="alert">
              {formError}
            </p>
          )}
        </TestModal>
      )}

      {scanOpen && (
        <TestModal title={t("scanResults")} onClose={() => setScanOpen(false)} wide busy={scanning}>
          <p className="tm-scan-base" title={scanBase}>
            {scanBase}
          </p>
          {scanning ? (
            <div className="tm-loading">
              <Loader2 size={16} className="spin" />
              <span>{t("scanning")}</span>
            </div>
          ) : scanResults.length === 0 ? (
            <div className="tm-empty">{t("noProjectsFound")}</div>
          ) : (
            <ul className="tm-scan-list">
              {scanResults.map((result) => {
                const known = knownPaths.has(toKey(result.path));
                return (
                  <li key={result.path} className="tm-scan-row">
                    <div className="tm-scan-info">
                      <span className="tm-scan-path" title={result.path}>
                        {result.path}
                      </span>
                      <span className="tm-scan-tags">
                        {result.framework && (
                          <span className={`tm-badge tm-badge-${result.framework}`}>
                            {result.framework}
                          </span>
                        )}
                        {result.testCommand && <code className="tm-cmd">{result.testCommand}</code>}
                        <span className="tm-evidence">
                          {result.reason !== "none" ? result.reason : ""}
                        </span>
                      </span>
                    </div>
                    <button
                      type="button"
                      className="btn btn-secondary btn-small"
                      onClick={() => void addFromScan(result)}
                      disabled={known || !result.testCommand}
                    >
                      {known ? t("alreadyAdded") : t("add")}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </TestModal>
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
                        {outcomeLabel(entry.status)}
                      </span>
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

      {generatorFor && (
        <TestGenerator
          project={generatorFor}
          onClose={() => setGeneratorFor(null)}
          onToast={showMsg}
        />
      )}
      {diagnosisFor && (
        <FailureDiagnosis
          project={diagnosisFor}
          onClose={() => setDiagnosisFor(null)}
          onToast={showMsg}
        />
      )}
      {coverageFor && (
        <CoverageReportView project={coverageFor} onClose={() => setCoverageFor(null)} />
      )}

      {toast && (
        <div className={`toast toast-${toast.type}`} role="status">
          <span className="toast-icon">
            {toast.type === "success" ? <CheckCircle size={14} /> : <XCircle size={14} />}
          </span>
          <span className="toast-text">{toast.text}</span>
        </div>
      )}
    </div>
  );
}
