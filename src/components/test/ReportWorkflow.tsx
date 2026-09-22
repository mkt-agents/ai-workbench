/**
 * The change-verification workflow, as a first-class panel instead of the old
 * 7-block modal. Five steps (采集 → AI → 选测 → 运行 → 验收) whose state is
 * derived from the stored report, so any step can be redone independently and
 * a reload lands you where the data says you are.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  Copy,
  Download,
  GitCommitHorizontal,
  Loader2,
  ShieldAlert,
  Sparkles,
  Trash2,
} from "lucide-react";
import { useGlobalStore } from "../../core/store";
import { isCancelledError, cleanErrorMessage } from "../../core/errorCodes";
import { useConfirm } from "../ConfirmModal";
import { listen } from "@tauri-apps/api/event";
import { markdownSections } from "../../lib/aiText";
import { STEP_ORDER, deriveSteps } from "../../core/reportSteps";
import type { StepId, StepState } from "../../core/reportSteps";
import { activeRunOf, startRun } from "../../core/testRuns";
import ChangeTestSelection from "../ChangeTestSelection";
import ChangeScenarioChecklist from "../ChangeScenarioChecklist";
import type {
  ChangeFile,
  ChangeReport,
  ChangeReportBundle,
  DeltaCoverage,
  Scenario,
  ScenarioSummary,
  StoredChangeReport,
  TestProject,
  TestSelection,
} from "../../core/types";

type Props = {
  project: TestProject;
  /** Report selected in the left list; null means "compose a new one". */
  reportId: string | null;
  onReportCreated: (id: string) => void;
  onToast: (type: "success" | "error", text: string) => void;
  /** Fired after a delete so the parent can drop the row and reselect. */
  onReportDeleted?: (id: string) => void;
  /** Jump to the vulnerability page from the dependency-exposure badge. */
  onOpenVuln?: () => void;
};

type Mode = "uncommitted" | "base" | "commits";
/** How the file list is grouped: by module (default) or by the commit that touched it. */
type GroupMode = "module" | "commit";

/** The panel renders a bounded list; the full set still counts towards the totals. */
const MAX_VISIBLE_FILES = 150;

const EMPTY_SUMMARY: ScenarioSummary = {
  total: 0,
  passed: 0,
  failed: 0,
  blocked: 0,
  pending: 0,
  percent: 0,
};

export default function ReportWorkflow({
  project,
  reportId,
  onReportCreated,
  onToast,
  onReportDeleted,
  onOpenVuln,
}: Props) {
  const { t } = useTranslation("test");
  const confirm = useConfirm();

  const collectChangeReport = useGlobalStore((s) => s.collectChangeReport);
  const getChangeReport = useGlobalStore((s) => s.getChangeReport);
  const deleteChangeReport = useGlobalStore((s) => s.deleteChangeReport);
  const generateChangeReportAi = useGlobalStore((s) => s.generateChangeReportAi);
  const cancelChangeAi = useGlobalStore((s) => s.cancelChangeAi);
  const selectChangeTests = useGlobalStore((s) => s.selectChangeTests);
  const generateChangeScenarios = useGlobalStore((s) => s.generateChangeScenarios);
  const addChangeScenario = useGlobalStore((s) => s.addChangeScenario);
  const setScenarioStatus = useGlobalStore((s) => s.setScenarioStatus);
  const deleteChangeScenario = useGlobalStore((s) => s.deleteChangeScenario);
  const computeIncrementalCoverage = useGlobalStore((s) => s.computeIncrementalCoverage);
  const setChangeReportAccepted = useGlobalStore((s) => s.setChangeReportAccepted);
  const listVulnFindings = useGlobalStore((s) => s.listVulnFindings);
  const saveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);
  const loadTestProjects = useGlobalStore((s) => s.loadTestProjects);

  const [mode, setMode] = useState<Mode>("uncommitted");
  const [base, setBase] = useState("");
  const [commits, setCommits] = useState(5);
  const [busy, setBusy] = useState(false);
  const [aiBusy, setAiBusy] = useState(false);
  const [aiProgress, setAiProgress] = useState<{ done: number; total: number } | null>(null);
  const [scenarioBusy, setScenarioBusy] = useState(false);
  const [error, setError] = useState("");
  const [bundle, setBundle] = useState<ChangeReportBundle | null>(null);
  const [ai, setAi] = useState("");
  const [onlyUntested, setOnlyUntested] = useState(false);
  const [groupMode, setGroupMode] = useState<GroupMode>("module");
  const [delta, setDelta] = useState<DeltaCoverage | null>(null);
  const [deltaBusy, setDeltaBusy] = useState(false);
  const [deltaError, setDeltaError] = useState("");
  const [acceptedAt, setAcceptedAt] = useState<string | null>(null);
  const [createdAt, setCreatedAt] = useState<string | null>(null);
  const [aiWarnings, setAiWarnings] = useState<string[]>([]);
  const [depExposure, setDepExposure] = useState<{ open: number; critical: number } | null>(null);
  const [selection, setSelection] = useState<TestSelection | null>(null);
  const [selectionLoading, setSelectionLoading] = useState(false);
  const [ticked, setTicked] = useState<string[]>([]);
  const [scenarios, setScenarios] = useState<Scenario[]>([]);
  const [summary, setSummary] = useState<ScenarioSummary>(EMPTY_SUMMARY);
  const [runs, setRuns] = useState<StoredChangeReport["runs"]>([]);
  const sectionRefs = useRef<Partial<Record<StepId, HTMLElement | null>>>({});

  const report: ChangeReport | null = bundle?.report ?? null;
  const viewingId = bundle?.reportId ?? null;
  const liveSession = activeRunOf(project.id);
  const running = Boolean(liveSession);
  const commitDetails = report?.commitDetails ?? [];

  const applyStored = useCallback((stored: StoredChangeReport) => {
    setScenarios(stored.scenarios ?? []);
    setSummary(stored.scenarioSummary ?? EMPTY_SUMMARY);
    setRuns(stored.runs ?? []);
    setDelta(stored.deltaCoverage ?? null);
    setAcceptedAt(stored.acceptedAt ?? null);
    setAiWarnings(stored.aiWarnings ?? []);
    setCreatedAt(stored.summary?.createdAt ?? null);
  }, []);

  const refreshStored = useCallback(
    async (id: string) => {
      if (!id) return;
      try {
        applyStored(await getChangeReport(id));
      } catch {
        /* the visible state is still what the last mutation returned */
      }
    },
    [applyStored, getChangeReport]
  );

  const loadSelection = useCallback(
    async (id: string) => {
      if (!id) {
        setSelection(null);
        setTicked([]);
        return;
      }
      setSelectionLoading(true);
      try {
        const next = await selectChangeTests(id);
        setSelection(next);
        setTicked(next.targets.map((target) => target.name));
      } catch {
        // Selection is an aid, not a gate: the static report below still stands.
        setSelection(null);
        setTicked([]);
      } finally {
        setSelectionLoading(false);
      }
    },
    [selectChangeTests]
  );

  // One load path for "a report was picked on the left" and "back to new".
  useEffect(() => {
    let cancelled = false;
    if (!reportId) {
      setBundle(null);
      setAi("");
      setScenarios([]);
      setSummary(EMPTY_SUMMARY);
      setRuns([]);
      setDelta(null);
      setAcceptedAt(null);
      setAiWarnings([]);
      setSelection(null);
      setTicked([]);
      setError("");
      setDeltaError("");
      return;
    }
    setBusy(true);
    setError("");
    (async () => {
      try {
        const stored = await getChangeReport(reportId);
        if (cancelled) return;
        if (stored.report) {
          setBundle({
            report: stored.report,
            branch: stored.summary.branch,
            head: "",
            repoRoot: "",
            subdir: false,
            reportId: stored.summary.id,
          });
          setAi(stored.ai ?? "");
          applyStored(stored);
          await loadSelection(stored.summary.id);
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setBusy(false);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reportId, project.id]);

  const generate = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      const result = await collectChangeReport(
        project.id,
        mode === "base" ? base.trim() : undefined,
        mode === "commits" ? commits : undefined
      );
      setBundle(result);
      setAi("");
      setScenarios([]);
      setSummary(EMPTY_SUMMARY);
      setRuns([]);
      setDelta(null);
      setAcceptedAt(null);
      setAiWarnings([]);
      setDeltaError("");
      await loadSelection(result.reportId ?? "");
      if (result.reportId) onReportCreated(result.reportId);
      if (result.report.stats.files === 0) {
        onToast("success", t("cr.empty"));
      }
    } catch (e) {
      setError(String(e));
      onToast("error", t("cr.failed", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const runAi = async () => {
    if (aiBusy || !viewingId) return;
    setAiBusy(true);
    setAiProgress(null);
    setError("");
    try {
      const markdown = await generateChangeReportAi(viewingId);
      setAi(markdown);
      try {
        // The backend parses the acceptance sections on the way to storing the AI text.
        const stored = await getChangeReport(viewingId);
        applyStored(stored);
        if ((stored.scenarios?.length ?? 0) > 0) {
          onToast("success", t("cr.sc.parsed", { n: stored.scenarios.length }));
        }
      } catch {
        /* the AI text itself arrived; a missing checklist is recoverable by regenerating */
      }
    } catch (e) {
      // A cancel is a user action, not a failure; anything else leaves the
      // static report usable — only the AI section failed.
      if (isCancelledError(e)) {
        onToast("success", t("cr.aiCancelled", { defaultValue: "AI 生成已取消" }));
      } else {
        setError(cleanErrorMessage(e));
        onToast("error", t("cr.aiFailed", { error: cleanErrorMessage(e) }));
      }
    } finally {
      setAiBusy(false);
      setAiProgress(null);
    }
  };

  // The map-reduce pass emits one event per finished chunk; show which module
  // the model is on, and stop mid-flight with `cancel_change_ai` if asked.
  useEffect(() => {
    if (!aiBusy || !viewingId) return;
    const un = listen<{ reportId: string; done: number; total: number }>(
      "change-ai-progress",
      (event) => {
        if (event.payload.reportId !== viewingId) return;
        setAiProgress({ done: event.payload.done, total: event.payload.total });
      }
    );
    return () => {
      void un.then((dispose) => dispose());
    };
  }, [aiBusy, viewingId]);

  const cancelAi = async () => {
    if (!viewingId) return;
    try {
      await cancelChangeAi(viewingId);
      onToast("success", t("cr.aiCancelSent", { defaultValue: "已发送取消指令，将在当前模型调用结束后停止" }));
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const computeDelta = async () => {
    if (!viewingId || deltaBusy) return;
    setDeltaBusy(true);
    setDeltaError("");
    try {
      setDelta(await computeIncrementalCoverage(viewingId));
    } catch (e) {
      setDeltaError(String(e));
    } finally {
      setDeltaBusy(false);
    }
  };

  const toggleAccept = async () => {
    if (!viewingId) return;
    try {
      const stamp = await setChangeReportAccepted(viewingId, !acceptedAt);
      setAcceptedAt(stamp);
      onToast("success", stamp ? t("cr.accept.done") : t("cr.accept.undone"));
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const runSelected = async () => {
    if (!viewingId || running) return;
    let args = "";
    try {
      // The backend rebuilds the filter from the ticked names, so the syntax stays
      // in Rust. Nothing ticked means the plain suite run, so no filter at all.
      if (ticked.length > 0) args = (await selectChangeTests(viewingId, ticked)).args;
    } catch (e) {
      onToast("error", String(e));
      return;
    }
    const { done } = startRun({
      projectId: project.id,
      projectName: project.name,
      source: "change-report",
      reportId: viewingId,
      args: args || undefined,
    });
    void done.then(async () => {
      await refreshStored(viewingId ?? "");
      try {
        await loadTestProjects();
      } catch {
        /* the run itself already reached the dock */
      }
    });
  };

  const generateScenarios = async () => {
    if (!viewingId || scenarioBusy) return;
    setScenarioBusy(true);
    try {
      const added = await generateChangeScenarios(viewingId);
      await refreshStored(viewingId);
      onToast(
        added > 0 ? "success" : "error",
        added > 0 ? t("cr.sc.added", { n: added }) : t("cr.sc.noNew")
      );
    } catch (e) {
      onToast("error", String(e));
    } finally {
      setScenarioBusy(false);
    }
  };

  const addScenario = async (title: string) => {
    if (!viewingId) return;
    try {
      await addChangeScenario(viewingId, title);
      await refreshStored(viewingId);
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const setScenario = async (scenario: Scenario, patch: { status?: string; note?: string }) => {
    try {
      const next = await setScenarioStatus(
        scenario.id,
        patch.status ?? scenario.status,
        // `undefined` means "leave the note alone"; an empty string clears it.
        patch.note,
        scenario.runId ?? undefined
      );
      setScenarios((prev) =>
        prev.map((row) =>
          row.id === scenario.id
            ? {
                ...row,
                status: patch.status ?? row.status,
                note: patch.note !== undefined ? patch.note : row.note,
              }
            : row
        )
      );
      setSummary(next);
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const removeScenario = async (scenario: Scenario) => {
    try {
      await deleteChangeScenario(scenario.id);
      await refreshStored(viewingId ?? "");
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const removeReport = async () => {
    if (!viewingId) return;
    const removed = viewingId;
    const ok = await confirm({
      title: t("cr.delete"),
      message: t("cr.deleteConfirm", {
        when: createdAt ? new Date(createdAt).toLocaleString() : "-",
        files: report?.stats.files ?? 0,
      }),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await deleteChangeReport(removed);
      setBundle(null);
      onToast("success", t("cr.deleted"));
      // The parent owns the list; it picks the neighbour or clears the selection.
      onReportDeleted?.(removed);
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const markdown = useMemo(() => {
    if (!report) return "";
    const lines: string[] = [];
    lines.push(`# ${t("cr.title")} · ${project.name}`);
    lines.push("");
    lines.push(`- ${t("cr.baseline")}: ${report.base || t("cr.modeUncommitted")}`);
    if (bundle?.branch)
      lines.push(`- ${t("cr.branch")}: ${bundle.branch}${bundle.head ? ` @ ${bundle.head}` : ""}`);
    lines.push(
      `- ${t("cr.stats")}: ${report.stats.files} ${t("cr.files")}, +${report.stats.adds} -${report.stats.dels}, ` +
        `${report.stats.modules} ${t("cr.modules")}, ${report.stats.untested} ${t("cr.untested")}` +
        (report.stats.ignored ? `, ${t("cr.ignored")} ${report.stats.ignored}` : "")
    );
    if (report.scope.length)
      lines.push(
        `- ${t("cr.scopeTitle")}: ${report.scope
          .map((s) => t(`cr.scope.${s}`, { defaultValue: s }))
          .join(" / ")}`
      );
    if (report.commits.length) {
      lines.push("", `## ${t("cr.commits")}`);
      for (const c of report.commits) lines.push(`- ${c}`);
    }
    for (const group of report.groups) {
      lines.push("", `## ${group.name} (${group.files} ${t("cr.files")}, +${group.adds} -${group.dels})`);
      if (group.risks.length)
        lines.push(
          `- ${t("cr.risks")}: ${group.risks
            .map((r) => t(`cr.risk.${r}`, { defaultValue: r }))
            .join(" / ")}`
        );
      for (const file of report.files.filter((f) => f.module === group.name).slice(0, MAX_VISIBLE_FILES)) {
        if (onlyUntested && !file.risks.includes("untested")) continue;
        const flags = [
          file.layer && t(`cr.layer.${file.layer}`, { defaultValue: file.layer }),
          ...file.risks.map((r) => t(`cr.risk.${r}`, { defaultValue: r })),
          (file.commits ?? []).length ? `@${(file.commits ?? []).join("/")}` : null,
        ]
          .filter(Boolean)
          .join(" · ");
        lines.push(`- [${file.status}] ${file.path} (+${file.adds} -${file.dels}) ${flags}`);
      }
    }
    if (delta) {
      lines.push(
        "",
        `## ${t("cr.delta.title")}`,
        `- ${t("cr.delta.summary", {
            pct: (delta.ratio * 100).toFixed(1),
            covered: delta.covered,
            missed: delta.missed,
            unknown: delta.unknown,
            defaultValue: `增量覆盖 ${(delta.ratio * 100).toFixed(1)}%（命中 ${delta.covered} / 未命中 ${delta.missed} / 无数据 ${delta.unknown}）`,
          })}`
      );
      for (const spot of delta.uncoveredHotspots.slice(0, 10)) {
        lines.push(`- ${spot.path}: ${spot.lines.slice(0, 20).join(", ")}`);
      }
    }
    if (report.apiChanges.length) {
      lines.push("", `## ${t("cr.apiChanges")}`);
      for (const change of report.apiChanges) {
        lines.push(
          `- [${t(`cr.api.${change.kind}`, { defaultValue: change.kind })}] ${change.name} @ ${change.path}`
        );
      }
    }
    if (selection && selection.targets.length) {
      lines.push("", `## ${t("cr.sel.title")}`);
      const checked = new Set(ticked);
      for (const target of selection.targets) {
        lines.push(
          `- [${checked.has(target.name) ? "x" : " "}] ${target.name}` +
            (target.from && target.from !== target.name ? ` ← ${target.from}` : "")
        );
      }
      if (selection.gaps.length) {
        lines.push(`- ${t("cr.sel.gapCount", { n: selection.gaps.length })}`);
        for (const gap of selection.gaps.slice(0, 30)) lines.push(`  - ${gap}`);
      }
      if (selection.args) lines.push("", "`" + selection.args + "`");
    }
    if (ai) {
      lines.push("", `## ${t("cr.aiSection")}`, "", ai.trim());
    }
    if (scenarios.length) {
      lines.push(
        "",
        `## ${t("cr.sc.title")} (${summary.passed}/${summary.total} · ${summary.percent}%)`
      );
      for (const item of scenarios) {
        lines.push(
          `- [${item.status === "passed" ? "x" : " "}] ` +
            `${[item.priority, item.title].filter(Boolean).join(" ")} — ` +
            `${t(`cr.sc.status.${item.status}`, { defaultValue: item.status })}` +
            (item.note ? ` · ${item.note}` : "")
        );
        if (item.detail) lines.push(`  ${item.detail}`);
      }
    }
    if (runs.length) {
      lines.push("", `## ${t("cr.runs")}`);
      for (const link of runs) {
        lines.push(
          `- ${link.createdAt} · ${t(`cr.runStatus.${link.status}`, { defaultValue: link.status })} · ` +
            `${link.passed}/${link.totalTests}` +
            (link.errorKind ? ` · ${t(`errorKind.${link.errorKind}`, { defaultValue: link.errorKind })}` : "")
        );
      }
    }
    if (acceptedAt) {
      lines.push("", `## ${t("cr.accept.title")}`, `- ${new Date(acceptedAt).toLocaleString()}`);
    }
    return lines.join("\n");
  }, [report, bundle, ai, project.name, onlyUntested, selection, ticked, scenarios, summary, runs, delta, acceptedAt, t]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(markdown);
      onToast("success", t("cr.copied"));
    } catch (e) {
      onToast("error", String(e));
    }
  };

  const exportMd = async () => {
    try {
      const name = `change-report-${project.name}-${new Date().toISOString().slice(0, 10)}`;
      const path = await saveTextFile(markdown, name, t("cr.export"));
      onToast("success", t("cr.savedTo", { path }));
    } catch (e) {
      const text = String(e);
      if (!/cancel/i.test(text)) onToast("error", text);
    }
  };

  const steps = deriveSteps({
    hasReport: Boolean(report),
    fileCount: report?.stats.files ?? 0,
    ai,
    selectionTargets: selection?.targets.length ?? 0,
    linkedRuns: runs.length,
    running,
    summary,
    acceptedAt,
  });

  const aiSections = ai ? Object.entries(markdownSections(ai)) : [];
  const fileFilter = (files: ChangeFile[]) =>
    files.filter((f) => !onlyUntested || f.risks.includes("untested")).slice(0, MAX_VISIBLE_FILES);
  const visibleFiles = (module: string) =>
    fileFilter((report?.files ?? []).filter((f) => f.module === module));
  // Files grouped by the commit that touched them; a file in several commits
  // appears under each, which is exactly what "what did this commit ship" asks.
  const commitGroups = useMemo(() => {
    if (!report || commitDetails.length === 0) return [];
    return commitDetails
      .map((commit) => ({
        commit,
        files: fileFilter(
          report.files.filter((f) => (f.commits ?? []).includes(commit.shortSha))
        ),
      }))
      .filter((group) => group.files.length > 0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [report, commitDetails, onlyUntested]);
  // Commits whose files all got filtered out (or rename-only paths) still matter.
  const ungroupedCommits = commitDetails.filter(
    (commit) => report?.files.length && !commitGroups.some((g) => g.commit.shortSha === commit.shortSha)
  );

  // Dependency exposure link: a report that touches a lock file shows how many
  // unresolved advisories the project carries right now (read-only in v1).
  const hasDependencyChange = Boolean(report?.files.some((f) => f.risks.includes("dependency")));
  useEffect(() => {
    if (!hasDependencyChange) {
      setDepExposure(null);
      return;
    }
    let cancelled = false;
    listVulnFindings(project.id, { status: "open", kind: "dependency", limit: 1 })
      .then((page) => {
        if (cancelled) return;
        setDepExposure(page.totals.open > 0 ? { open: page.totals.open, critical: page.totals.critical } : null);
      })
      .catch(() => {
        if (!cancelled) setDepExposure(null);
      });
    return () => {
      cancelled = true;
    };
  }, [hasDependencyChange, listVulnFindings, project.id, viewingId]);

  const scrollTo = (step: StepId) => {
    sectionRefs.current[step]?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  const renderFile = (file: ChangeFile) => (
    <li key={`${groupMode}-${file.path}`} className="tm-cr-file">
      <span className={`tm-cr-file-status tm-cr-status-${file.status}`}>{file.status}</span>
      <span
        className="tm-cr-file-path"
        title={`${file.path}${file.testPath ? ` → ${file.testPath}` : file.hasTest ? "" : ` · ${t("cr.noTestHint")}`}`}
      >
        {file.path}
      </span>
      <span className="tm-cr-file-layer">
        {t(`cr.layer.${file.layer}`, { defaultValue: file.layer })}
      </span>
      <span className="tm-cr-file-delta">
        +{file.adds} -{file.dels}
      </span>
      <span className="tm-cr-file-flags">
        {file.hasTest ? <span className="tm-cr-ok">✓</span> : <span className="tm-cr-warn">—</span>}
        {file.risks
          .filter((r) => r !== "untested" && r !== "newFile")
          .map((r) => (
            <span key={r} className={`tm-cr-risk tm-cr-risk-${r}`}>
              {t(`cr.risk.${r}`, { defaultValue: r })}
            </span>
          ))}
        {/* Substring-only matches: shown for context, deliberately grey and uncounted. */}
        {(file.hints ?? []).map((h) => (
          <span
            key={h}
            className="tm-cr-risk tm-cr-risk-hint"
            title={t("cr.hintWhy", { defaultValue: "仅文件名弱匹配，不计入风险" })}
          >
            {t(`cr.risk.${h}`, { defaultValue: h })}
          </span>
        ))}
        {(file.commits ?? []).map((sha) => (
          <span key={sha} className="tm-cr-file-sha" title={sha}>
            {sha}
          </span>
        ))}
      </span>
    </li>
  );

  return (
    <div className="tm-wf">
      <nav className="tm-wf-steps" aria-label={t("wf.steps")}>
        {STEP_ORDER.map((step) => {
          const state: StepState = steps[step];
          return (
            <button
              key={step}
              type="button"
              className={`tm-wf-step is-${state}`}
              onClick={() => scrollTo(step)}
              disabled={state === "empty"}
            >
              <span className="tm-wf-step-dot" aria-hidden>
                {state === "done" ? <Check size={10} /> : state === "running" ? <Loader2 size={10} className="spin" /> : null}
              </span>
              {t(`wf.step.${step}`)}
            </button>
          );
        })}
        <span className="tm-wf-spacer" />
        {report && (
          <>
            <button type="button" className="btn btn-secondary btn-small" onClick={() => void copy()}>
              <Copy size={12} />
              {t("cr.copy")}
            </button>
            <button type="button" className="btn btn-secondary btn-small" onClick={() => void exportMd()}>
              <Download size={12} />
              {t("cr.export")}
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-small tm-danger"
              onClick={() => void removeReport()}
              title={t("cr.deleteHint")}
            >
              <Trash2 size={12} />
              {t("cr.delete")}
            </button>
          </>
        )}
      </nav>

      <div className="tm-cr-toolbar" ref={(el) => void (sectionRefs.current.collect = el)}>
        <select
          className="input-field"
          value={mode}
          onChange={(e) => setMode(e.target.value as Mode)}
          disabled={busy || aiBusy}
          aria-label={t("cr.mode")}
        >
          <option value="uncommitted">{t("cr.modeUncommitted")}</option>
          <option value="base">{t("cr.modeBase")}</option>
          <option value="commits">{t("cr.modeCommits")}</option>
        </select>
        {mode === "base" && (
          <input
            className="input-field"
            value={base}
            placeholder={t("cr.basePlaceholder")}
            onChange={(e) => setBase(e.target.value)}
            disabled={busy || aiBusy}
          />
        )}
        {mode === "commits" && (
          <input
            className="input-field tm-cr-count"
            type="number"
            min={1}
            max={50}
            value={commits}
            onChange={(e) => setCommits(Math.max(1, Math.min(50, Number(e.target.value) || 1)))}
            disabled={busy || aiBusy}
          />
        )}
        <button
          type="button"
          className="btn btn-primary btn-small"
          onClick={() => void generate()}
          disabled={busy || aiBusy}
        >
          {busy ? <Loader2 size={12} className="spin" /> : null}
          {report ? t("cr.regenerate") : t("cr.generate")}
        </button>
        {report && (
          <label className="tm-cr-toggle">
            <input type="checkbox" checked={onlyUntested} onChange={(e) => setOnlyUntested(e.target.checked)} />
            {t("cr.onlyUntested")}
          </label>
        )}
      </div>

      {error && <div className="repos-modal-error">{error}</div>}

      {!report && !busy && <p className="tm-hint">{t("cr.intro")}</p>}

      {busy && !report && (
        <p className="tm-hint">
          <Loader2 size={13} className="spin" /> {t("loading")}
        </p>
      )}

      {report && (
        <>
          <div className="tm-cr-stats">
            <span className="tm-cr-stat">
              {report.stats.files} {t("cr.files")}
            </span>
            <span className="tm-cr-stat is-add">+{report.stats.adds}</span>
            <span className="tm-cr-stat is-del">-{report.stats.dels}</span>
            <span className="tm-cr-stat">{report.stats.modules} {t("cr.modules")}</span>
            {report.stats.untested > 0 && (
              <span className="tm-cr-stat is-warn">
                {report.stats.untested} {t("cr.untested")}
              </span>
            )}
            {report.stats.testFiles > 0 && (
              <span className="tm-cr-stat">{report.stats.testFiles} {t("cr.testFiles")}</span>
            )}
            {report.stats.ignored > 0 && (
              <span className="tm-cr-stat is-muted" title={t("cr.ignoredHint")}>
                {t("cr.ignored")} {report.stats.ignored}
              </span>
            )}
            {bundle?.branch && (
              <span className="tm-cr-stat is-muted">
                {bundle.branch}
                {bundle.head ? ` @ ${bundle.head}` : ""}
              </span>
            )}
            {depExposure && (
              <button
                type="button"
                className="tm-cr-stat is-bad tm-cr-dep-link"
                onClick={onOpenVuln}
                title={t("cr.depOpenHint", {
                  defaultValue: `本次改动涉及依赖清单；点击前往漏洞页处理（${depExposure.open} 条未处理，critical×${depExposure.critical}）`,
                })}
              >
                {t("cr.depOpen", {
                  n: depExposure.open,
                  c: depExposure.critical,
                  defaultValue: `依赖漏洞 ${depExposure.open} 条未处理（critical×${depExposure.critical}）`,
                })}
              </button>
            )}
          </div>

          {report.stats.files === 0 && <p className="tm-hint">{t("cr.empty")}</p>}

          {report.scope.length > 0 && (
            <div className="tm-cr-scope">
              <span className="tm-cr-label">{t("cr.scopeTitle")}</span>
              {report.scope.map((s) => (
                <span key={s} className={`tm-cr-scope-chip tm-cr-scope-${s}`}>
                  {t(`cr.scope.${s}`, { defaultValue: s })}
                </span>
              ))}
            </div>
          )}

          {report.truncated && <p className="tm-hint">{t("cr.truncated")}</p>}

          {report.commits.length > 0 && (
            <div className="tm-cr-commits">
              <span className="tm-cr-label">{t("cr.commits")}</span>
              <ul>
                {(commitDetails.length > 0
                  ? commitDetails.slice(0, 20).map((c) => (
                      <li key={c.sha} className="tm-cr-commit-line">
                        <code className="tm-cr-file-sha">{c.shortSha}</code>
                        <span title={c.body.trim() || undefined}>{c.subject}</span>
                        <span className="tm-cr-commit-meta">
                          {c.author} · {c.date.slice(0, 10)}
                        </span>
                      </li>
                    ))
                  : report.commits.slice(0, 20).map((c, index) => (
                      <li key={`${c}-${index}`}>{c}</li>
                    ))
                )}
              </ul>
            </div>
          )}

          {report.apiChanges.length > 0 && (
            <div className="tm-cr-api">
              <span className="tm-cr-label">{t("cr.apiChanges")}</span>
              <ul>
                {report.apiChanges.slice(0, 40).map((c) => (
                  <li key={`${c.kind}-${c.name}-${c.path}`}>
                    <span className={`tm-cr-api-kind tm-cr-api-${c.kind}`}>
                      {t(`cr.api.${c.kind}`, { defaultValue: c.kind })}
                    </span>
                    <code>{c.name}</code>
                    <span className="tm-cr-api-path">{c.path}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {commitDetails.length > 0 && (
            <div className="tm-cr-groupbar" role="group" aria-label={t("cr.groupBy")}>
              <button
                type="button"
                className={`btn btn-small ${groupMode === "module" ? "btn-primary" : "btn-secondary"}`}
                onClick={() => setGroupMode("module")}
              >
                {t("cr.groupModule")}
              </button>
              <button
                type="button"
                className={`btn btn-small ${groupMode === "commit" ? "btn-primary" : "btn-secondary"}`}
                onClick={() => setGroupMode("commit")}
              >
                <GitCommitHorizontal size={11} />
                {t("cr.groupCommit")}
              </button>
              <span className="tm-cr-groupbar-hint">{t("cr.groupCommitHint")}</span>
            </div>
          )}

          {groupMode === "module" || commitDetails.length === 0
            ? report.groups.map((group) => {
                const files = visibleFiles(group.name);
                if (files.length === 0) return null;
                return (
                  <details key={group.name} className="tm-cr-group" open>
                    <summary className="tm-cr-group-head">
                      <span className="tm-cr-group-name">{group.name}</span>
                      <span className="tm-cr-group-stat">
                        {group.files} · +{group.adds} -{group.dels}
                      </span>
                      {group.risks.map((risk) => (
                        <span key={risk} className={`tm-cr-risk tm-cr-risk-${risk}`}>
                          {t(`cr.risk.${risk}`, { defaultValue: risk })}
                        </span>
                      ))}
                    </summary>
                    <ul className="tm-cr-files">{files.map(renderFile)}</ul>
                  </details>
                );
              })
            : (
              <>
                {commitGroups.map(({ commit, files }) => (
                  <details key={commit.sha} className="tm-cr-group" open>
                    <summary className="tm-cr-group-head">
                      <span className="tm-cr-group-name tm-cr-commit-subject" title={commit.subject}>
                        {commit.subject}
                      </span>
                      <code className="tm-cr-file-sha">{commit.shortSha}</code>
                      <span className="tm-cr-group-stat">
                        {files.length} · {commit.author} · {commit.date.slice(0, 10)}
                      </span>
                    </summary>
                    {commit.body.trim() && <p className="tm-cr-commit-body">{commit.body.trim()}</p>}
                    <ul className="tm-cr-files">{files.map(renderFile)}</ul>
                  </details>
                ))}
                {ungroupedCommits.map((commit) => (
                  <div key={commit.sha} className="tm-cr-commit-empty">
                    <code className="tm-cr-file-sha">{commit.shortSha}</code>
                    <span title={commit.subject}>{commit.subject}</span>
                    <span className="tm-cr-groupbar-hint">{t("cr.commitNoFiles")}</span>
                  </div>
                ))}
              </>
            )}

          <div className="tm-cr-delta">
            <div className="tm-cr-ai-head">
              <span className="tm-cr-label">{t("cr.delta.title")}</span>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => void computeDelta()}
                disabled={deltaBusy || !viewingId || report.stats.files === 0}
              >
                {deltaBusy ? <Loader2 size={12} className="spin" /> : <ShieldAlert size={12} />}
                {delta ? t("cr.delta.recompute") : t("cr.delta.compute")}
              </button>
            </div>
            {deltaError && <p className="tm-cr-delta-error">{deltaError}</p>}
            {!delta && !deltaError && !deltaBusy && <p className="tm-hint">{t("cr.delta.hint")}</p>}
            {delta && (
              <>
                <div className="tm-cr-stats">
                  <span
                    className={`tm-cr-stat ${
                      delta.ratio >= 0.8 ? "is-ok" : delta.ratio >= 0.5 ? "is-warn" : "is-bad"
                    }`}
                  >
                    {(delta.ratio * 100).toFixed(1)}%
                  </span>
                  <span className="tm-cr-stat">
                    {t("cr.delta.line", { covered: delta.covered, missed: delta.missed })}
                  </span>
                  {delta.unknown > 0 && (
                    <span className="tm-cr-stat is-muted" title={t("cr.delta.unknownHint")}>
                      {t("cr.delta.unknown", { n: delta.unknown })}
                    </span>
                  )}
                </div>
                {delta.uncoveredHotspots.length > 0 && (
                  <details className="tm-cr-group" open={delta.ratio < 0.8}>
                    <summary className="tm-cr-group-head">
                      <span className="tm-cr-group-name">{t("cr.delta.hotspots")}</span>
                      <span className="tm-cr-group-stat">{delta.uncoveredHotspots.length}</span>
                    </summary>
                    <ul className="tm-cr-files">
                      {delta.uncoveredHotspots.map((spot) => (
                        <li key={spot.path} className="tm-cr-file">
                          <span className="tm-cr-file-path" title={spot.path}>
                            {spot.path}
                          </span>
                          <span className="tm-cr-hotspot-lines">
                            {spot.lines.slice(0, 40).join(", ")}
                            {spot.lines.length > 40 ? " …" : ""}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </details>
                )}
              </>
            )}
          </div>

          <div ref={(el) => void (sectionRefs.current.select = el)}>
            {viewingId && (
              <ChangeTestSelection
                selection={selection}
                loading={selectionLoading}
                running={running}
                ticked={ticked}
                onTicked={setTicked}
                onRun={() => void runSelected()}
              />
            )}
          </div>

          <div className="tm-cr-ai" ref={(el) => void (sectionRefs.current.ai = el)}>
            <div className="tm-cr-ai-head">
              <span className="tm-cr-label">{t("cr.aiSection")}</span>
              {aiBusy && (aiProgress?.total ?? 1) > 1 && (
                <span className="tm-cr-ai-progress">
                  {t("cr.aiProgress", {
                    defaultValue: "分块生成中 {{done}}/{{total}}",
                    done: aiProgress?.done ?? 0,
                    total: aiProgress?.total ?? 1,
                  })}
                </span>
              )}
              {aiBusy ? (
                <button type="button" className="btn btn-secondary btn-small" onClick={() => void cancelAi()}>
                  {t("cr.aiCancel", { defaultValue: "取消生成" })}
                </button>
              ) : null}
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => void runAi()}
                disabled={aiBusy || !viewingId || report.stats.files === 0}
              >
                {aiBusy ? <Loader2 size={12} className="spin" /> : <Sparkles size={12} />}
                {ai ? t("cr.aiRegenerate") : t("cr.aiGenerate")}
              </button>
            </div>
            {!ai && !aiBusy && (
              <p className="tm-hint">{viewingId ? t("cr.aiHint") : t("cr.aiUnavailable")}</p>
            )}
            {aiSections.length > 0 && (
              <div className="tm-cr-ai-body">
                {aiSections.map(([title, body]) => (
                  <section key={title} className="tm-cr-ai-block">
                    <h4>{title}</h4>
                    <ul>
                      {body
                        .split("\n")
                        .map((line) => line.replace(/^[-*]\s*/, "").trim())
                        .filter(Boolean)
                        .map((item, index) => (
                          <li key={`${title}-${index}`}>{item}</li>
                        ))}
                    </ul>
                  </section>
                ))}
              </div>
            )}
            {ai && aiWarnings.length > 0 && (
              <p className="tm-cr-ai-warn">
                <ShieldAlert size={12} />
                {t("cr.aiWarn", {
                  list: aiWarnings.join("、"),
                  defaultValue: `AI 提到了报告中不存在的标识符：${aiWarnings.join("、")}`,
                })}
              </p>
            )}
          </div>

          <div ref={(el) => void (sectionRefs.current.run = el)}>
            {runs.length > 0 && (
              <div className="tm-cr-runs">
                <span className="tm-cr-label">{t("cr.runs")}</span>
                <ul>
                  {runs.slice(0, 10).map((link) => (
                    <li key={link.runId} className="tm-cr-run">
                      <span className={`tm-cr-run-status tm-cr-run-${link.status}`}>
                        {t(`cr.runStatus.${link.status}`, { defaultValue: link.status })}
                      </span>
                      <span className="tm-cr-run-counts">
                        {link.passed}/{link.totalTests}
                      </span>
                      <span className="tm-cr-run-time">{new Date(link.createdAt).toLocaleString()}</span>
                      {link.errorKind && (
                        <span className="tm-cr-risk">
                          {t(`errorKind.${link.errorKind}`, { defaultValue: link.errorKind })}
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>

          <div ref={(el) => void (sectionRefs.current.accept = el)}>
            {viewingId && (
              <>
                <div className="tm-cr-accept-head">
                  <span className="tm-cr-label">{t("cr.accept.title")}</span>
                  {acceptedAt && (
                    <span className="tm-cr-accept-stamp">
                      {t("cr.accept.at", { time: new Date(acceptedAt).toLocaleString() })}
                    </span>
                  )}
                  <button
                    type="button"
                    className={`btn btn-small ${acceptedAt ? "btn-secondary" : "btn-primary"}`}
                    onClick={() => void toggleAccept()}
                  >
                    {acceptedAt ? t("cr.accept.undo") : <Check size={12} />}
                    {acceptedAt ? t("cr.accept.undo") : t("cr.accept.mark")}
                  </button>
                </div>
                  <ChangeScenarioChecklist
                  scenarios={scenarios}
                  summary={summary}
                  busy={scenarioBusy}
                  canGenerate={Boolean(ai)}
                  onGenerate={() => void generateScenarios()}
                  onAdd={(title) => void addScenario(title)}
                  onStatus={(scenario, status) => void setScenario(scenario, { status })}
                  onNote={(scenario, note) => void setScenario(scenario, { note })}
                  onDelete={(scenario) => void removeScenario(scenario)}
                />
              </>
            )}
          </div>
        </>
      )}
    </div>
  );
}
