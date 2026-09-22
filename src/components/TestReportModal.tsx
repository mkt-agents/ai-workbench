/**
 * One-shot regression report launched from a repo card: collect the change set
 * (uncommitted / against a base / last N commits), show the static analysis,
 * then optionally ask a model for the five tester-facing sections. Nothing is
 * persisted — copy or export the assembled Markdown and it's yours.
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { Copy, Download, GitCommitHorizontal, Loader2, ShieldAlert, Sparkles, X } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { readStoredString, writeStoredString } from "../core/localState";
import { markdownSections } from "../lib/aiText";
import ModalTitleRow from "./ModalTitleRow";
import { isCancelledError, cleanErrorMessage } from "./testReportTypes";
import type { AiResult, FileChange, TestReportBundle } from "./testReportTypes";
import type { AIModelConfig } from "../core/types";
import "./TestReportModal.css";

type Props = {
  repoPath: string;
  onClose: () => void;
};

type Mode = "uncommitted" | "base" | "commits";
type GroupMode = "module" | "commit";

/** The panel renders a bounded list; the full set still counts towards the totals. */
const MAX_VISIBLE_FILES = 150;
const MODEL_KEY = "workbench-commit-model";

export default function TestReportModal({ repoPath, onClose }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");

  const collect = useGlobalStore((s) => s.invokeCollectTestReport);
  const generateAi = useGlobalStore((s) => s.invokeGenerateTestReportAi);
  const cancelAi = useGlobalStore((s) => s.invokeCancelTestReportAi);
  const saveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);
  const copyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const loadAIModels = useGlobalStore((s) => s.loadAIModels);

  const [mode, setMode] = useState<Mode>("uncommitted");
  const [base, setBase] = useState("");
  const [commits, setCommits] = useState(5);
  const [busy, setBusy] = useState(false);
  const [bundle, setBundle] = useState<TestReportBundle | null>(null);
  const [error, setError] = useState("");
  const [toast, setToast] = useState<{ type: "success" | "error"; text: string } | null>(null);

  const [ai, setAi] = useState("");
  const [aiWarnings, setAiWarnings] = useState<string[]>([]);
  const [aiBusy, setAiBusy] = useState(false);
  const [aiProgress, setAiProgress] = useState<{ done: number; total: number } | null>(null);

  const [onlyUntested, setOnlyUntested] = useState(false);
  const [groupMode, setGroupMode] = useState<GroupMode>("module");
  const [selectedModelId, setSelectedModelId] = useState<string>(() => readStoredString(MODEL_KEY));

  const report = bundle?.report ?? null;
  const viewingId = bundle?.reportId ?? "";
  const commitDetails = report?.commitDetails ?? [];

  const defaultModel = useMemo(() => aiModels.find((m) => m.isDefault) || aiModels[0] || null, [aiModels]);
  const selectedModel: AIModelConfig | null = useMemo(() => {
    if (selectedModelId) {
      const found = aiModels.find((m) => m.id === selectedModelId);
      if (found) return found;
    }
    return defaultModel;
  }, [selectedModelId, aiModels, defaultModel]);

  useEffect(() => {
    writeStoredString(MODEL_KEY, selectedModelId);
  }, [selectedModelId]);

  useEffect(() => {
    if (aiModels.length === 0) void loadAIModels().catch(() => {});
  }, [aiModels.length, loadAIModels]);

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setToast({ type, text });
    window.setTimeout(() => setToast(null), type === "error" ? 8000 : 3000);
  }, []);

  // A new baseline invalidates any shown report — never let it go stale against
  // the wrong range.
  const resetReport = useCallback(() => {
    setBundle(null);
    setAi("");
    setAiWarnings([]);
    setAiProgress(null);
    setError("");
  }, []);

  const generate = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      const result = await collect(
        repoPath,
        mode === "base" ? base.trim() : undefined,
        mode === "commits" ? commits : undefined
      );
      setBundle(result);
      setAi("");
      setAiWarnings([]);
      if (result.report.stats.files === 0) showMsg("success", t("testReport.empty"));
    } catch (e) {
      setError(cleanErrorMessage(e));
      showMsg("error", t("testReport.failed", { error: cleanErrorMessage(e) }));
    } finally {
      setBusy(false);
    }
  };

  const runAi = async () => {
    if (aiBusy || !viewingId || !selectedModel) return;
    setAiBusy(true);
    setAiProgress(null);
    setError("");
    try {
      const result: AiResult = await generateAi(viewingId, selectedModel);
      setAi(result.markdown);
      setAiWarnings(result.warnings);
    } catch (e) {
      // A cancel is a user action, not a failure; the static report stays usable.
      if (isCancelledError(e)) {
        showMsg("success", t("testReport.aiCancelled"));
      } else {
        setError(cleanErrorMessage(e));
        showMsg("error", t("testReport.aiFailed", { error: cleanErrorMessage(e) }));
      }
    } finally {
      setAiBusy(false);
      setAiProgress(null);
    }
  };

  // The map-reduce pass emits one event per finished chunk.
  useEffect(() => {
    if (!aiBusy || !viewingId) return;
    const un = listen<{ reportId: string; done: number; total: number }>(
      "test-report-ai-progress",
      (event) => {
        if (event.payload.reportId !== viewingId) return;
        setAiProgress({ done: event.payload.done, total: event.payload.total });
      }
    );
    return () => {
      void un.then((dispose) => dispose());
    };
  }, [aiBusy, viewingId]);

  const cancelAiRun = async () => {
    if (!viewingId) return;
    try {
      await cancelAi(viewingId);
      showMsg("success", t("testReport.aiCancelSent"));
    } catch (e) {
      showMsg("error", cleanErrorMessage(e));
    }
  };

  const fileFilter = (files: FileChange[]) =>
    files.filter((f) => !onlyUntested || f.risks.includes("untested")).slice(0, MAX_VISIBLE_FILES);
  const visibleFiles = (moduleName: string) =>
    fileFilter((report?.files ?? []).filter((f) => f.module === moduleName));

  const commitGroups = useMemo(() => {
    if (!report || commitDetails.length === 0) return [];
    return commitDetails
      .map((commit) => ({
        commit,
        files: fileFilter(report.files.filter((f) => (f.commits ?? []).includes(commit.shortSha))),
      }))
      .filter((group) => group.files.length > 0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [report, commitDetails, onlyUntested]);
  const ungroupedCommits = commitDetails.filter(
    (commit) => report?.files.length && !commitGroups.some((g) => g.commit.shortSha === commit.shortSha)
  );

  const aiSections = useMemo(() => Object.entries(markdownSections(ai)), [ai]);

  const markdown = useMemo(() => {
    if (!report) return "";
    const lines: string[] = [];
    lines.push(`# ${t("testReport.title")} · ${bundle?.repoName ?? ""}`);
    lines.push("");
    lines.push(`- ${t("testReport.baseline")}: ${report.base || t("testReport.modeUncommitted")}`);
    if (bundle?.branch)
      lines.push(`- ${t("testReport.branch")}: ${bundle.branch}${bundle.head ? ` @ ${bundle.head}` : ""}`);
    lines.push(
      `- ${t("testReport.stats")}: ${report.stats.files} ${t("testReport.files")}, +${report.stats.adds} -${report.stats.dels}, ` +
        `${report.stats.modules} ${t("testReport.modules")}, ${report.stats.untested} ${t("testReport.untested")}` +
        (report.stats.ignored ? `, ${t("testReport.ignored")} ${report.stats.ignored}` : "")
    );
    if (report.scope.length)
      lines.push(
        `- ${t("testReport.scopeTitle")}: ${report.scope.map((s) => t(`testReport.scope.${s}`, { defaultValue: s })).join(" / ")}`
      );
    if (report.commits.length) {
      lines.push("", `## ${t("testReport.commits")}`);
      for (const c of report.commits) lines.push(`- ${c}`);
    }
    for (const group of report.groups) {
      lines.push("", `## ${group.name} (${group.files} ${t("testReport.files")}, +${group.adds} -${group.dels})`);
      if (group.risks.length)
        lines.push(`- ${t("testReport.risks")}: ${group.risks.map((r) => t(`testReport.risk.${r}`, { defaultValue: r })).join(" / ")}`);
      for (const file of report.files.filter((f) => f.module === group.name).slice(0, MAX_VISIBLE_FILES)) {
        if (onlyUntested && !file.risks.includes("untested")) continue;
        const flags = [
          file.layer && t(`testReport.layer.${file.layer}`, { defaultValue: file.layer }),
          ...file.risks.map((r) => t(`testReport.risk.${r}`, { defaultValue: r })),
          (file.commits ?? []).length ? `@${(file.commits ?? []).join("/")}` : null,
        ]
          .filter(Boolean)
          .join(" · ");
        lines.push(`- [${file.status}] ${file.path} (+${file.adds} -${file.dels}) ${flags}`);
      }
    }
    if (report.apiChanges.length) {
      lines.push("", `## ${t("testReport.apiChanges")}`);
      for (const change of report.apiChanges)
        lines.push(`- [${t(`testReport.api.${change.kind}`, { defaultValue: change.kind })}] ${change.name} @ ${change.path}`);
    }
    if (ai) lines.push("", `## ${t("testReport.aiSection")}`, "", ai.trim());
    return lines.join("\n");
  }, [report, bundle, ai, onlyUntested, t]);

  const copy = async () => {
    try {
      await copyToClipboard(markdown);
      showMsg("success", t("testReport.copied"));
    } catch (e) {
      showMsg("error", cleanErrorMessage(e));
    }
  };

  const exportMd = async () => {
    try {
      const name = `${bundle?.repoName ?? "repo"}-${t("testReport.title")}-${new Date().toISOString().slice(0, 10)}.md`;
      const path = await saveTextFile(markdown, name, t("testReport.export"));
      showMsg("success", t("testReport.savedTo", { path }));
    } catch (e) {
      const text = cleanErrorMessage(e);
      if (!/cancel/i.test(text)) showMsg("error", text);
    }
  };

  const renderFile = (file: FileChange) => (
    <li key={`${groupMode}-${file.path}`} className="tr-file">
      <span className={`tr-file-status tr-status-${file.status}`}>{file.status}</span>
      <span
        className="tr-file-path"
        title={`${file.path}${file.testPath ? ` → ${file.testPath}` : file.hasTest ? "" : ` · ${t("testReport.noTestHint")}`}`}
      >
        {file.path}
      </span>
      <span className="tr-file-layer">{t(`testReport.layer.${file.layer}`, { defaultValue: file.layer })}</span>
      <span className="tr-file-delta">
        +{file.adds} -{file.dels}
      </span>
      <span className="tr-file-flags">
        {file.hasTest ? <span className="tr-ok">✓</span> : <span className="tr-warn">—</span>}
        {file.risks
          .filter((r) => r !== "untested" && r !== "newFile")
          .map((r) => (
            <span key={r} className={`tr-risk tr-risk-${r}`}>
              {t(`testReport.risk.${r}`, { defaultValue: r })}
            </span>
          ))}
        {(file.hints ?? []).map((h) => (
          <span key={h} className="tr-risk tr-risk-hint" title={t("testReport.hintWhy")}>
            {t(`testReport.risk.${h}`, { defaultValue: h })}
          </span>
        ))}
        {(file.commits ?? []).map((sha) => (
          <span key={sha} className="tr-file-sha" title={sha}>
            {sha}
          </span>
        ))}
      </span>
    </li>
  );

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal tr-modal" onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow title={t("testReport.title")} onClose={onClose} disabled={busy || aiBusy} />

        <div className="tr-toolbar">
          <select
            className="input-field"
            value={mode}
            onChange={(e) => {
              setMode(e.target.value as Mode);
              resetReport();
            }}
            disabled={busy || aiBusy}
            aria-label={t("testReport.mode")}
          >
            <option value="uncommitted">{t("testReport.modeUncommitted")}</option>
            <option value="base">{t("testReport.modeBase")}</option>
            <option value="commits">{t("testReport.modeCommits")}</option>
          </select>
          {mode === "base" && (
            <input
              className="input-field"
              value={base}
              placeholder={t("testReport.basePlaceholder")}
              onChange={(e) => {
                setBase(e.target.value);
                resetReport();
              }}
              disabled={busy || aiBusy}
            />
          )}
          {mode === "commits" && (
            <input
              className="input-field tr-count"
              type="number"
              min={1}
              max={50}
              value={commits}
              onChange={(e) => {
                setCommits(Math.max(1, Math.min(50, Number(e.target.value) || 1)));
                resetReport();
              }}
              disabled={busy || aiBusy}
            />
          )}
          <button type="button" className="btn btn-primary btn-small" onClick={() => void generate()} disabled={busy || aiBusy}>
            {busy ? <Loader2 size={12} className="spin" /> : null}
            {report ? t("testReport.regenerate") : t("testReport.generate")}
          </button>
          {report && (
            <label className="tr-toggle">
              <input type="checkbox" checked={onlyUntested} onChange={(e) => setOnlyUntested(e.target.checked)} />
              {t("testReport.onlyUntested")}
            </label>
          )}
        </div>

        {error && <div className="repos-modal-error">{error}</div>}
        {toast && <div className={`toast toast-${toast.type}`}><span className="toast-text">{toast.text}</span></div>}

        {!report && !busy && <p className="tr-hint">{t("testReport.intro")}</p>}
        {busy && !report && (
          <p className="tr-hint">
            <Loader2 size={13} className="spin" /> {tc("loading")}
          </p>
        )}

        {report && (
          <div className="tr-body">
            <div className="tr-stats">
              <span className="tr-stat">
                {report.stats.files} {t("testReport.files")}
              </span>
              <span className="tr-stat is-add">+{report.stats.adds}</span>
              <span className="tr-stat is-del">-{report.stats.dels}</span>
              <span className="tr-stat">
                {report.stats.modules} {t("testReport.modules")}
              </span>
              {report.stats.untested > 0 && (
                <span className="tr-stat is-warn">
                  {report.stats.untested} {t("testReport.untested")}
                </span>
              )}
              {report.stats.testFiles > 0 && (
                <span className="tr-stat">
                  {report.stats.testFiles} {t("testReport.testFiles")}
                </span>
              )}
              {report.stats.ignored > 0 && (
                <span className="tr-stat is-muted" title={t("testReport.ignoredHint")}>
                  {t("testReport.ignored")} {report.stats.ignored}
                </span>
              )}
              {bundle?.branch && (
                <span className="tr-stat is-muted">
                  {bundle.branch}
                  {bundle.head ? ` @ ${bundle.head}` : ""}
                </span>
              )}
            </div>

            {report.stats.files === 0 && <p className="tr-hint">{t("testReport.empty")}</p>}

            {report.scope.length > 0 && (
              <div className="tr-scope">
                <span className="tr-label">{t("testReport.scopeTitle")}</span>
                {report.scope.map((s) => (
                  <span key={s} className={`tr-scope-chip tr-scope-${s}`}>
                    {t(`testReport.scope.${s}`, { defaultValue: s })}
                  </span>
                ))}
              </div>
            )}

            {report.truncated && <p className="tr-hint">{t("testReport.truncated")}</p>}

            {report.commits.length > 0 && (
              <div className="tr-commits">
                <span className="tr-label">{t("testReport.commits")}</span>
                <ul>
                  {commitDetails.length > 0
                    ? commitDetails
                        .slice(0, 20)
                        .map((c) => (
                          <li key={c.sha} className="tr-commit-line">
                            <code className="tr-file-sha">{c.shortSha}</code>
                            <span title={c.body.trim() || undefined}>{c.subject}</span>
                            <span className="tr-commit-meta">
                              {c.author} · {c.date.slice(0, 10)}
                            </span>
                          </li>
                        ))
                    : report.commits.slice(0, 20).map((c, index) => <li key={`${c}-${index}`}>{c}</li>)}
                </ul>
              </div>
            )}

            {report.apiChanges.length > 0 && (
              <div className="tr-api">
                <span className="tr-label">{t("testReport.apiChanges")}</span>
                <ul>
                  {report.apiChanges.slice(0, 40).map((c) => (
                    <li key={`${c.kind}-${c.name}-${c.path}`}>
                      <span className={`tr-api-kind tr-api-${c.kind}`}>
                        {t(`testReport.api.${c.kind}`, { defaultValue: c.kind })}
                      </span>
                      <code>{c.name}</code>
                      <span className="tr-api-path">{c.path}</span>
                    </li>
                  ))}
                </ul>
              </div>
            )}

            {commitDetails.length > 0 && (
              <div className="tr-groupbar" role="group" aria-label={t("testReport.groupBy")}>
                <button
                  type="button"
                  className={`btn btn-small ${groupMode === "module" ? "btn-primary" : "btn-secondary"}`}
                  onClick={() => setGroupMode("module")}
                >
                  {t("testReport.groupModule")}
                </button>
                <button
                  type="button"
                  className={`btn btn-small ${groupMode === "commit" ? "btn-primary" : "btn-secondary"}`}
                  onClick={() => setGroupMode("commit")}
                >
                  <GitCommitHorizontal size={11} />
                  {t("testReport.groupCommit")}
                </button>
                <span className="tr-groupbar-hint">{t("testReport.groupCommitHint")}</span>
              </div>
            )}

            {groupMode === "module" || commitDetails.length === 0 ? (
              report.groups.map((group) => {
                const files = visibleFiles(group.name);
                if (files.length === 0) return null;
                return (
                  <details key={group.name} className="tr-group" open>
                    <summary className="tr-group-head">
                      <span className="tr-group-name">{group.name}</span>
                      <span className="tr-group-stat">
                        {group.files} · +{group.adds} -{group.dels}
                      </span>
                      {group.risks.map((risk) => (
                        <span key={risk} className={`tr-risk tr-risk-${risk}`}>
                          {t(`testReport.risk.${risk}`, { defaultValue: risk })}
                        </span>
                      ))}
                    </summary>
                    <ul className="tr-files">{files.map(renderFile)}</ul>
                  </details>
                );
              })
            ) : (
              <>
                {commitGroups.map(({ commit, files }) => (
                  <details key={commit.sha} className="tr-group" open>
                    <summary className="tr-group-head">
                      <span className="tr-group-name tr-commit-subject" title={commit.subject}>
                        {commit.subject}
                      </span>
                      <code className="tr-file-sha">{commit.shortSha}</code>
                      <span className="tr-group-stat">
                        {files.length} · {commit.author} · {commit.date.slice(0, 10)}
                      </span>
                    </summary>
                    {commit.body.trim() && <p className="tr-commit-body">{commit.body.trim()}</p>}
                    <ul className="tr-files">{files.map(renderFile)}</ul>
                  </details>
                ))}
                {ungroupedCommits.map((commit) => (
                  <div key={commit.sha} className="tr-commit-empty">
                    <code className="tr-file-sha">{commit.shortSha}</code>
                    <span title={commit.subject}>{commit.subject}</span>
                    <span className="tr-groupbar-hint">{t("testReport.commitNoFiles")}</span>
                  </div>
                ))}
              </>
            )}

            <div className="tr-ai">
              <div className="tr-ai-head">
                <span className="tr-label">{t("testReport.aiSection")}</span>
                <select
                  className="input-field tr-model"
                  value={selectedModel?.id ?? ""}
                  onChange={(e) => setSelectedModelId(e.target.value)}
                  disabled={aiBusy}
                  aria-label={t("testReport.aiModel")}
                >
                  {aiModels.length === 0 && <option value="">{t("testReport.aiNeedModel")}</option>}
                  {aiModels.map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.name}
                    </option>
                  ))}
                </select>
                {aiBusy && (aiProgress?.total ?? 1) > 1 && (
                  <span className="tr-ai-progress">
                    {t("testReport.aiProgress", { done: aiProgress?.done ?? 0, total: aiProgress?.total ?? 1 })}
                  </span>
                )}
                {aiBusy && (
                  <button type="button" className="btn btn-secondary btn-small" onClick={() => void cancelAiRun()}>
                    {t("testReport.aiCancel")}
                  </button>
                )}
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={() => void runAi()}
                  disabled={aiBusy || !viewingId || !selectedModel || report.stats.files === 0}
                >
                  {aiBusy ? <Loader2 size={12} className="spin" /> : <Sparkles size={12} />}
                  {ai ? t("testReport.aiRegenerate") : t("testReport.aiGenerate")}
                </button>
              </div>
              {!ai && !aiBusy && <p className="tr-hint">{selectedModel ? t("testReport.aiHint") : t("testReport.aiNeedModel")}</p>}
              {aiSections.length > 0 && (
                <div className="tr-ai-body">
                  {aiSections.map(([title, body]) => (
                    <section key={title} className="tr-ai-block">
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
                <p className="tr-ai-warn">
                  <ShieldAlert size={12} />
                  {t("testReport.aiWarn", { list: aiWarnings.join("、") })}
                </p>
              )}
            </div>
          </div>
        )}

        <div className="modal-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose} disabled={busy || aiBusy}>
            <X size={13} /> {tc("actions.close")}
          </button>
          <span className="tr-actions-spacer" />
          <button type="button" className="btn btn-secondary" onClick={() => void copy()} disabled={!report}>
            <Copy size={13} /> {t("testReport.copy")}
          </button>
          <button type="button" className="btn btn-primary" onClick={() => void exportMd()} disabled={!report}>
            <Download size={13} /> {t("testReport.export")}
          </button>
        </div>
      </div>
    </div>
  );
}
