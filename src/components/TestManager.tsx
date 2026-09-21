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
import { useGlobalStore } from "../core/store";
import { listen } from "@tauri-apps/api/event";
import { useConfirm } from "./ConfirmModal";
import TestModal from "./TestModal";
import TestGenerator from "./TestGenerator";
import FailureDiagnosis from "./FailureDiagnosis";
import CoverageReportView from "./CoverageReport";
import ChangeReportModal from "./ChangeReportModal";
import ScanTestProjectsModal from "./ScanTestProjectsModal";
import { mapPool, TEST_RUN_CONCURRENCY } from "../core/asyncPool";
import type {
  ScannedProject,
  TestHistoryEntry,
  TestProject,
  TestRunOutcome,
  TestRunResult,
} from "../core/types";
import "./TestManager.css";

type Toast = { type: "success" | "error"; text: string };
type ToastFn = (type: Toast["type"], text: string) => void;
/** Rolling window of the live tail; the full log arrives with the final result. */
type LiveTail = { projectId: string; lines: string[] };

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

const LIVE_MAX_LINES = 400;

/** Frameworks `read_coverage_report` can actually parse. */
const COVERAGE_FRAMEWORKS = ["jest", "vitest", "mocha", "playwright", "cargo", "pytest", "maven", "gotest"];

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
  const linkChangeRun = useGlobalStore((s) => s.linkChangeRun);
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

  const [scan, setScan] = useState<{ rootPath: string; projects: ScannedProject[] } | null>(null);
  const [scanning, setScanning] = useState(false);

  const [history, setHistory] = useState<TestHistoryEntry[]>([]);
  const [historyFor, setHistoryFor] = useState<TestProject | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);

  const [generatorFor, setGeneratorFor] = useState<TestProject | null>(null);
  const [diagnosisFor, setDiagnosisFor] = useState<TestProject | null>(null);
  const [coverageFor, setCoverageFor] = useState<TestProject | null>(null);
  const [changeReportFor, setChangeReportFor] = useState<TestProject | null>(null);

  const [toast, setToast] = useState<Toast | null>(null);
  const toastTimer = useRef<number | null>(null);
  const [live, setLive] = useState<LiveTail | null>(null);
  const liveRef = useRef<HTMLPreElement>(null);
  const [query, setQuery] = useState("");

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

  // Live tail: the runner emits whatever the pipes produced since the last check.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<{ projectId: string; text: string; done: boolean }>(
      "test-run-output",
      (event) => {
        const { projectId, text, done } = event.payload;
        setLive((prev) => {
          if (done) return null;
          const base = prev && prev.projectId === projectId ? prev.lines : [];
          const next = [
            ...base,
            ...text.split(/\r?\n/).filter((line) => line.trim().length > 0),
          ];
          return {
            projectId,
            lines: next.length > LIVE_MAX_LINES ? next.slice(-LIVE_MAX_LINES) : next,
          };
        });
      }
    ).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const el = liveRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [live]);

  // A string, not TestRunOutcome: rows written by older builds carry values the
  // current runner no longer emits, and they must not be relabelled as errors.
  const outcomeLabel = useCallback(
    (status: string): string => {
      switch (status) {
        case "success":
          return t("success");
        case "failed":
          return t("failed");
        case "cancelled":
          return t("statusCancelled");
        case "timeout":
          return t("statusTimeout");
        case "skipped":
          // Only written by older builds; never relabel it as an error.
          return t("skipped");
        case "error":
          return t("error");
        default:
          return status;
      }
    },
    [t]
  );

  const runOne = useCallback(
    async (project: TestProject, quiet = false, args?: string): Promise<TestRunResult | null> => {
      if (runs[project.id]) return null;
      setRuns((prev) => ({ ...prev, [project.id]: Date.now() }));
      setFocusedId(project.id);
      setOutputExpanded(false);
      setLive({ projectId: project.id, lines: [] });
      let finished: TestRunResult | null = null;
      try {
        const result = await runTest(project.id, args);
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

  /**
   * A run started from a change report: same runner (live output, cancel, elapsed all
   * keep working), plus the report↔run link so the report can show what it already ran.
   */
  const runForReport = useCallback(
    async (project: TestProject, args: string, reportId: string) => {
      const result = await runOne(project, false, args || undefined);
      if (!result) return;
      try {
        await linkChangeRun(reportId, result.id);
      } catch {
        // Linking is bookkeeping; the run itself already reached the panel.
      }
    },
    [runOne, linkChangeRun]
  );

  const handleBatchRun = useCallback(async () => {
    const targets = testProjects.filter(
      (p) => selected.has(p.id) && !runs[p.id] && p.enabled
    );
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
        showMsg("success", t("saved"));
      } else {
        await addTestProject(payload);
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
      if (projects.length === 0) showMsg("success", t("scan.noneFound"));
    } catch (e) {
      showMsg("error", String(e));
    } finally {
      setScanning(false);
    }
  }, [pickDirectory, scanTestProjects, showMsg, t]);

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

  const visibleProjects = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return testProjects;
    return testProjects.filter(
      (p) => p.name.toLowerCase().includes(q) || p.path.toLowerCase().includes(q)
    );
  }, [testProjects, query]);

  const liveProject = live && runs[live.projectId] ? live : null;
  const liveName = liveProject
    ? testProjects.find((p) => p.id === liveProject.projectId)?.name ?? ""
    : "";

  const focused = focusedId ? testProjects.find((p) => p.id === focusedId) ?? null : null;
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
      ) : visibleProjects.length === 0 ? (
        <div className="tm-empty">{t("noMatches", { q: query })}</div>
      ) : (
        <div className="tm-list">
          {visibleProjects.map((project) => {
            const startedAt = runs[project.id];
            const isRunning = startedAt !== undefined;
            const hasResult = !!results[project.id];
            const envCount = Object.keys(project.env ?? {}).length;
            const coverageSupported = COVERAGE_FRAMEWORKS.includes(project.framework);
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
                    {project.lastStatus && (
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
                        {outcomeLabel(project.lastStatus)}
                        {project.lastErrorKind && project.lastStatus !== "success" && (
                          <span className="tm-chip-kind">
                            {t(`errorKind.${project.lastErrorKind}`, {
                              defaultValue: project.lastErrorKind,
                            })}
                          </span>
                        )}
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
                    onClick={() => setChangeReportFor(project)}
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

      {liveProject && (
        <section className="tm-result tm-result-live" aria-live="polite">
          <div className="tm-result-head">
            <h3>
              {t("liveOutput")} · {liveName}
            </h3>
            <span className="tm-chip tm-chip-running" role="status">
              <Loader2 size={11} className="spin" />
              {t("elapsed", {
                sec: Math.max(
                  0,
                  Math.round((nowMs - (runs[liveProject.projectId] ?? nowMs)) / 1000)
                ),
              })}
            </span>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => {
                const project = testProjects.find((p) => p.id === liveProject.projectId);
                if (project) void handleCancel(project);
              }}
              title={t("cancelRunHint")}
            >
              <Square size={12} />
              {t("cancelRun")}
            </button>
          </div>
          <pre className="tm-output tm-output-live" ref={liveRef}>
            {liveProject.lines.length > 0
              ? liveProject.lines.join("\n")
              : t("liveWaiting")}
          </pre>
        </section>
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
            showMsg("success", t("scan.addedN", { count }));
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
      {changeReportFor && (
        <ChangeReportModal
          project={changeReportFor}
          onClose={() => setChangeReportFor(null)}
          onToast={showMsg}
          running={Boolean(runs[changeReportFor.id])}
          onRunSelected={(args, reportId) => runForReport(changeReportFor, args, reportId)}
        />
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
