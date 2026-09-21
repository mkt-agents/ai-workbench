import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Download, History, Loader2, Sparkles } from "lucide-react";
import TestModal from "./TestModal";
import ChangeTestSelection from "./ChangeTestSelection";
import ChangeScenarioChecklist from "./ChangeScenarioChecklist";
import { useGlobalStore } from "../core/store";
import { markdownSections } from "../lib/aiText";
import type {
  ChangeReport,
  ChangeReportBundle,
  ChangeReportSummary,
  ChangeRunLink,
  Scenario,
  ScenarioSummary,
  StoredChangeReport,
  TestProject,
  TestSelection,
} from "../core/types";

type Props = {
  project: TestProject;
  onClose: () => void;
  onToast: (type: "success" | "error", text: string) => void;
  /** A run started from the list owns the process, so the report defers to it. */
  running?: boolean;
  /** Run the ticked tests and link the run to this report; resolves when it finishes. */
  onRunSelected?: (args: string, reportId: string) => Promise<void>;
};

type Mode = "uncommitted" | "base" | "commits";

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

function ChangeReportModal({ project, onClose, onToast, running = false, onRunSelected }: Props) {
  const { t } = useTranslation("test");

  const collectChangeReport = useGlobalStore((s) => s.collectChangeReport);
  const listChangeReports = useGlobalStore((s) => s.listChangeReports);
  const getChangeReport = useGlobalStore((s) => s.getChangeReport);
  const generateChangeReportAi = useGlobalStore((s) => s.generateChangeReportAi);
  const selectChangeTests = useGlobalStore((s) => s.selectChangeTests);
  const generateChangeScenarios = useGlobalStore((s) => s.generateChangeScenarios);
  const addChangeScenario = useGlobalStore((s) => s.addChangeScenario);
  const setScenarioStatus = useGlobalStore((s) => s.setScenarioStatus);
  const deleteChangeScenario = useGlobalStore((s) => s.deleteChangeScenario);
  const saveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);

  const [mode, setMode] = useState<Mode>("uncommitted");
  const [base, setBase] = useState("");
  const [commits, setCommits] = useState(5);
  const [busy, setBusy] = useState(false);
  const [aiBusy, setAiBusy] = useState(false);
  const [scenarioBusy, setScenarioBusy] = useState(false);
  const [error, setError] = useState("");
  const [bundle, setBundle] = useState<ChangeReportBundle | null>(null);
  const [ai, setAi] = useState("");
  const [viewingId, setViewingId] = useState<string | null>(null);
  const [history, setHistory] = useState<ChangeReportSummary[]>([]);
  const [onlyUntested, setOnlyUntested] = useState(false);
  const [selection, setSelection] = useState<TestSelection | null>(null);
  const [selectionLoading, setSelectionLoading] = useState(false);
  const [ticked, setTicked] = useState<string[]>([]);
  const [scenarios, setScenarios] = useState<Scenario[]>([]);
  const [summary, setSummary] = useState<ScenarioSummary>(EMPTY_SUMMARY);
  const [runs, setRuns] = useState<ChangeRunLink[]>([]);

  const report: ChangeReport | null = bundle?.report ?? null;

  const refreshHistory = useCallback(async () => {
    try {
      setHistory(await listChangeReports(project.id));
    } catch {
      // History is an auxiliary list; an unavailable table must not block the report.
      setHistory([]);
    }
  }, [listChangeReports, project.id]);

  useEffect(() => {
    void refreshHistory();
  }, [refreshHistory]);

  const loadSelection = useCallback(
    async (reportId: string) => {
      if (!reportId) {
        setSelection(null);
        setTicked([]);
        return;
      }
      setSelectionLoading(true);
      try {
        const next = await selectChangeTests(reportId);
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

  /** Bring the checklist and the run links back in line with what the DB holds. */
  const applyStored = useCallback((stored: StoredChangeReport) => {
    setScenarios(stored.scenarios ?? []);
    setSummary(stored.scenarioSummary ?? EMPTY_SUMMARY);
    setRuns(stored.runs ?? []);
  }, []);

  const refreshStored = useCallback(
    async (reportId: string) => {
      if (!reportId) return;
      try {
        applyStored(await getChangeReport(reportId));
      } catch {
        /* the visible state is still what the last mutation returned */
      }
    },
    [applyStored, getChangeReport]
  );

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
      setViewingId(result.reportId);
      setScenarios([]);
      setSummary(EMPTY_SUMMARY);
      setRuns([]);
      await refreshHistory();
      await loadSelection(result.reportId ?? "");
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
    setError("");
    try {
      const markdown = await generateChangeReportAi(viewingId);
      setAi(markdown);
      await refreshHistory();
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
      // The static report stays usable: only the AI section failed.
      setError(String(e));
      onToast("error", t("cr.aiFailed", { error: String(e) }));
    } finally {
      setAiBusy(false);
    }
  };

  const openHistory = async (id: string) => {
    if (!id) return;
    setBusy(true);
    setError("");
    try {
      const stored = await getChangeReport(id);
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
        setViewingId(stored.summary.id);
        applyStored(stored);
        await loadSelection(stored.summary.id);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const runSelected = async () => {
    if (!viewingId || !onRunSelected || running) return;
    let args = "";
    try {
      // The backend rebuilds the filter from the ticked names, so the syntax stays in
      // Rust. Nothing ticked means the plain suite run, so no filter is passed at all.
      if (ticked.length > 0) args = (await selectChangeTests(viewingId, ticked)).args;
    } catch (e) {
      onToast("error", String(e));
      return;
    }
    try {
      await onRunSelected(args, viewingId);
    } finally {
      await refreshStored(viewingId);
    }
  };

  const generateScenarios = async () => {
    if (!viewingId || scenarioBusy) return;
    setScenarioBusy(true);
    try {
      const added = await generateChangeScenarios(viewingId);
      await refreshStored(viewingId);
      onToast(added > 0 ? "success" : "error", added > 0 ? t("cr.sc.added", { n: added }) : t("cr.sc.noNew"));
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
      // The command returns the new progress, so only the row is patched locally.
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

  const markdown = useMemo(() => {
    if (!report) return "";
    const lines: string[] = [];
    lines.push(`# ${t("cr.title")} · ${project.name}`);
    lines.push("");
    lines.push(`- ${t("cr.baseline")}: ${report.base || t("cr.modeUncommitted")}`);
    if (bundle?.branch) lines.push(`- ${t("cr.branch")}: ${bundle.branch}${bundle.head ? ` @ ${bundle.head}` : ""}`);
    lines.push(
      `- ${t("cr.stats")}: ${report.stats.files} ${t("cr.files")}, +${report.stats.adds} -${report.stats.dels}, ` +
        `${report.stats.modules} ${t("cr.modules")}, ${report.stats.untested} ${t("cr.untested")}` +
        (report.stats.ignored ? `, ${t("cr.ignored")} ${report.stats.ignored}` : "")
    );
    if (report.scope.length) lines.push(`- ${t("cr.scope")}: ${report.scope.map((s) => t(`cr.scope.${s}`, { defaultValue: s })).join(" / ")}`);
    if (report.commits.length) {
      lines.push("", `## ${t("cr.commits")}`);
      for (const c of report.commits) lines.push(`- ${c}`);
    }
    for (const group of report.groups) {
      lines.push("", `## ${group.name} (${group.files} ${t("cr.files")}, +${group.adds} -${group.dels})`);
      if (group.risks.length) lines.push(`- ${t("cr.risks")}: ${group.risks.map((r) => t(`cr.risk.${r}`, { defaultValue: r })).join(" / ")}`);
      for (const file of report.files.filter((f) => f.module === group.name).slice(0, MAX_VISIBLE_FILES)) {
        if (onlyUntested && !file.risks.includes("untested")) continue;
        const flags = [
          file.layer && t(`cr.layer.${file.layer}`, { defaultValue: file.layer }),
          ...file.risks.map((r) => t(`cr.risk.${r}`, { defaultValue: r })),
        ]
          .filter(Boolean)
          .join(" · ");
        lines.push(`- [${file.status}] ${file.path} (+${file.adds} -${file.dels}) ${flags}`);
      }
    }
    if (report.apiChanges.length) {
      lines.push("", `## ${t("cr.apiChanges")}`);
      for (const change of report.apiChanges) {
        lines.push(`- [${t(`cr.api.${change.kind}`, { defaultValue: change.kind })}] ${change.name} @ ${change.path}`);
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
      lines.push("", `## ${t("cr.sc.title")} (${summary.passed}/${summary.total} · ${summary.percent}%)`);
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
    return lines.join("\n");
  }, [report, bundle, ai, project.name, onlyUntested, selection, ticked, scenarios, summary, runs, t]);

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

  const aiSections = ai ? Object.entries(markdownSections(ai)) : [];
  const visibleFiles = (module: string) =>
    (report?.files ?? [])
      .filter((f) => f.module === module)
      .filter((f) => !onlyUntested || f.risks.includes("untested"))
      .slice(0, MAX_VISIBLE_FILES);

  return (
    <TestModal
      title={`${t("cr.title")} · ${project.name}`}
      onClose={onClose}
      busy={busy || aiBusy}
      wide
      footer={
        <>
          {history.length > 0 && (
            <label className="tm-cr-history">
              <History size={13} />
              <select
                className="input-field"
                value={viewingId ?? ""}
                onChange={(e) => void openHistory(e.target.value)}
                disabled={busy || aiBusy}
              >
                <option value="">{t("cr.historyPlaceholder")}</option>
                {history.map((row) => (
                  <option key={row.id} value={row.id}>
                    {new Date(row.createdAt).toLocaleString()} · {row.base || t("cr.modeUncommitted")} ·{" "}
                    {row.files} {t("cr.files")}
                    {row.hasAi ? " · AI" : ""}
                  </option>
                ))}
              </select>
            </label>
          )}
          <span className="tm-cr-footer-spacer" />
          <button type="button" className="btn btn-secondary btn-small" onClick={() => void copy()} disabled={!report}>
            <Copy size={12} />
            {t("cr.copy")}
          </button>
          <button type="button" className="btn btn-secondary btn-small" onClick={() => void exportMd()} disabled={!report}>
            <Download size={12} />
            {t("cr.export")}
          </button>
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            {t("cr.close")}
          </button>
        </>
      }
    >
      <div className="tm-cr-toolbar">
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
        <button type="button" className="btn btn-primary btn-small" onClick={() => void generate()} disabled={busy || aiBusy}>
          {busy ? <Loader2 size={12} className="spin" /> : null}
          {t("cr.generate")}
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
              <span className="tm-cr-stat is-warn">{report.stats.untested} {t("cr.untested")}</span>
            )}
            {report.stats.testFiles > 0 && <span className="tm-cr-stat">{report.stats.testFiles} {t("cr.testFiles")}</span>}
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
          </div>

          {report.stats.files === 0 && <p className="tm-hint">{t("cr.empty")}</p>}

          {report.scope.length > 0 && (
            <div className="tm-cr-scope">
              <span className="tm-cr-label">{t("cr.scope")}</span>
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
                {report.commits.slice(0, 20).map((c, index) => (
                  <li key={`${c}-${index}`}>{c}</li>
                ))}
              </ul>
            </div>
          )}

          {report.apiChanges.length > 0 && (
            <div className="tm-cr-api">
              <span className="tm-cr-label">{t("cr.apiChanges")}</span>
              <ul>
                {report.apiChanges.slice(0, 40).map((c) => (
                  <li key={`${c.kind}-${c.name}-${c.path}`}>
                    <span className={`tm-cr-api-kind tm-cr-api-${c.kind}`}>{t(`cr.api.${c.kind}`, { defaultValue: c.kind })}</span>
                    <code>{c.name}</code>
                    <span className="tm-cr-api-path">{c.path}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {report.groups.map((group) => {
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
                <ul className="tm-cr-files">
                  {files.map((file) => (
                    <li key={file.path} className="tm-cr-file">
                      <span className={`tm-cr-file-status tm-cr-status-${file.status}`}>{file.status}</span>
                      <span className="tm-cr-file-path" title={file.testPath ?? (file.hasTest ? "" : t("cr.noTestHint"))}>
                        {file.path}
                      </span>
                      <span className="tm-cr-file-layer">{t(`cr.layer.${file.layer}`, { defaultValue: file.layer })}</span>
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
                      </span>
                    </li>
                  ))}
                </ul>
              </details>
            );
          })}

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

          <div className="tm-cr-ai">
            <div className="tm-cr-ai-head">
              <span className="tm-cr-label">{t("cr.aiSection")}</span>
              <button type="button" className="btn btn-secondary btn-small" onClick={() => void runAi()} disabled={aiBusy || !viewingId || report.stats.files === 0}>
                {aiBusy ? <Loader2 size={12} className="spin" /> : <Sparkles size={12} />}
                {ai ? t("cr.aiRegenerate") : t("cr.aiGenerate")}
              </button>
            </div>
            {!ai && !aiBusy && <p className="tm-hint">{viewingId ? t("cr.aiHint") : t("cr.aiUnavailable")}</p>}
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
          </div>
          {viewingId && (
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
          )}

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
        </>
      )}
    </TestModal>
  );
}

export default ChangeReportModal;
