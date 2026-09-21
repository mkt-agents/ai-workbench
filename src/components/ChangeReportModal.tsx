import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Download, History, Loader2, Sparkles } from "lucide-react";
import TestModal from "./TestModal";
import { useGlobalStore } from "../core/store";
import { markdownSections } from "../lib/aiText";
import type { ChangeReport, ChangeReportBundle, ChangeReportSummary, TestProject } from "../core/types";

type Props = {
  project: TestProject;
  onClose: () => void;
  onToast: (type: "success" | "error", text: string) => void;
};

type Mode = "uncommitted" | "base" | "commits";

/** The panel renders a bounded list; the full set still counts towards the totals. */
const MAX_VISIBLE_FILES = 150;

function ChangeReportModal({ project, onClose, onToast }: Props) {
  const { t } = useTranslation("test");

  const collectChangeReport = useGlobalStore((s) => s.collectChangeReport);
  const listChangeReports = useGlobalStore((s) => s.listChangeReports);
  const getChangeReport = useGlobalStore((s) => s.getChangeReport);
  const generateChangeReportAi = useGlobalStore((s) => s.generateChangeReportAi);
  const saveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);

  const [mode, setMode] = useState<Mode>("uncommitted");
  const [base, setBase] = useState("");
  const [commits, setCommits] = useState(5);
  const [busy, setBusy] = useState(false);
  const [aiBusy, setAiBusy] = useState(false);
  const [error, setError] = useState("");
  const [bundle, setBundle] = useState<ChangeReportBundle | null>(null);
  const [ai, setAi] = useState("");
  const [viewingId, setViewingId] = useState<string | null>(null);
  const [history, setHistory] = useState<ChangeReportSummary[]>([]);
  const [onlyUntested, setOnlyUntested] = useState(false);

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
      await refreshHistory();
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
      setAi(await generateChangeReportAi(viewingId));
      await refreshHistory();
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
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
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
    if (ai) {
      lines.push("", `## ${t("cr.aiSection")}`, "", ai.trim());
    }
    return lines.join("\n");
  }, [report, bundle, ai, project.name, onlyUntested, t]);

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
        </>
      )}
    </TestModal>
  );
}

export default ChangeReportModal;
