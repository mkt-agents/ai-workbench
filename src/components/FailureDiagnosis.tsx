import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Copy, Loader2, XCircle } from "lucide-react";
import { useGlobalStore } from "../core/store";
import TestModal from "./TestModal";
import { firstFencedBlock, markdownSections } from "../lib/aiText";
import { latestFinishedOf } from "../core/testRuns";
import type { FailureDiagnosis as Diagnosis, TestProject, TestRunResult } from "../core/types";

type Props = {
  project: TestProject;
  onClose: () => void;
  onToast: (type: "success" | "error", text: string) => void;
};

/** The model answers in whichever language it likes, so sections are matched by keyword. */
const SECTION_KEYS: { field: keyof Diagnosis; patterns: RegExp }[] = [
  { field: "rootCause", patterns: /根因|root\s*cause/i },
  { field: "expectedBehavior", patterns: /预期|expected/i },
  { field: "actualBehavior", patterns: /实际|actual/i },
  { field: "fixSuggestion", patterns: /建议|suggestion|recommend/i },
  { field: "fixCode", patterns: /修复代码|fix\s*code/i },
];

function pick(sections: Record<string, string>, patterns: RegExp): string | undefined {
  for (const [title, body] of Object.entries(sections)) {
    if (patterns.test(title)) return body.trim() || undefined;
  }
  return undefined;
}

export default function FailureDiagnosis({ project, onClose, onToast }: Props) {
  const { t } = useTranslation("test");
  const diagnoseTestFailure = useGlobalStore((s) => s.diagnoseTestFailure);
  const getTestHistory = useGlobalStore((s) => s.getTestHistory);
  const getTestRun = useGlobalStore((s) => s.getTestRun);

  const [testName, setTestName] = useState("");
  const [errorMessage, setErrorMessage] = useState("");
  const [testCode, setTestCode] = useState("");
  const [sourceCode, setSourceCode] = useState("");
  const [raw, setRaw] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [prefilled, setPrefilled] = useState(false);

  // The failure output the user is staring at is almost always the last run's —
  // pull it from the live session first, then from history, so diagnosis stops
  // asking people to re-paste what they just watched fail.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        let result: TestRunResult | null = null;
        const session = latestFinishedOf(project.id);
        if (session?.result && (session.status === "failed" || session.status === "error")) {
          result = session.result;
        } else {
          const history = await getTestHistory(project.id);
          const lastBad = history.find(
            (h) => h.runId && (h.status === "failed" || h.status === "error")
          );
          if (lastBad?.runId) result = await getTestRun(lastBad.runId);
        }
        if (!result || cancelled) return;
        const firstFail = result.suites
          .flatMap((suite) => suite.tests)
          .find((c) => c.status === "failed");
        const tail = result.output.split("\n").slice(-80).join("\n");
        const errText =
          [firstFail?.error?.message, firstFail?.error?.stack].filter(Boolean).join("\n") || tail;
        setTestName((prev) => prev || firstFail?.name || "");
        setErrorMessage((prev) => prev || errText);
        setPrefilled(true);
      } catch {
        /* prefill is a convenience; the manual path still works */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [project.id, getTestHistory, getTestRun]);

  const diagnosis = useMemo<Diagnosis | null>(() => {
    if (!raw) return null;
    const sections = markdownSections(raw);
    const parsed: Partial<Diagnosis> = {};
    for (const entry of SECTION_KEYS) {
      const value = pick(sections, entry.patterns);
      if (value) {
        parsed[entry.field] =
          entry.field === "fixCode" ? firstFencedBlock(value) ?? value : value;
      }
    }
    // A model that ignores the requested structure still deserves to be read.
    if (!Object.keys(parsed).length) return { rootCause: raw.trim() } as Diagnosis;
    return parsed as Diagnosis;
  }, [raw]);

  const handleDiagnose = useCallback(async () => {
    if (!testCode.trim() || !errorMessage.trim()) {
      setError(t("diagnosisIncomplete"));
      return;
    }
    setBusy(true);
    setError(null);
    setRaw("");
    try {
      const text = await diagnoseTestFailure(
        testCode,
        sourceCode,
        errorMessage,
        testName.trim() || project.name
      );
      setRaw(text);
    } catch (e) {
      setError(String(e));
      onToast("error", t("diagnoseFailed", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  }, [testCode, sourceCode, errorMessage, testName, project.name, diagnoseTestFailure, onToast, t]);

  const handleCopyFix = useCallback(async () => {
    if (!diagnosis?.fixCode) return;
    try {
      await navigator.clipboard.writeText(diagnosis.fixCode);
      onToast("success", t("copied"));
    } catch (e) {
      onToast("error", String(e));
    }
  }, [diagnosis, onToast, t]);

  const fields: { key: keyof Diagnosis; label: string }[] = [
    { key: "rootCause", label: t("rootCause") },
    { key: "expectedBehavior", label: t("expectedBehavior") },
    { key: "actualBehavior", label: t("actualBehavior") },
    { key: "fixSuggestion", label: t("fixSuggestion") },
  ];

  return (
    <TestModal title={`${t("aiFailureDiagnosis")} · ${project.name}`} onClose={onClose} wide>
      <div className="tm-field">
        <label htmlFor="tm-diag-name">{t("testName")}</label>
        <input
          id="tm-diag-name"
          className="input-field"
          value={testName}
          onChange={(e) => setTestName(e.target.value)}
          placeholder={t("testNamePlaceholder")}
        />
      </div>
      <div className="tm-field">
        <label htmlFor="tm-diag-error">{t("errorMessage")}</label>
        {prefilled && errorMessage && (
          <p className="tm-hint">{t("diagnosisPrefilled")}</p>
        )}
        <textarea
          id="tm-diag-error"
          className="input-field tm-code-area"
          value={errorMessage}
          onChange={(e) => setErrorMessage(e.target.value)}
          placeholder={t("errorMessagePlaceholder")}
          rows={3}
          spellCheck={false}
        />
      </div>
      <div className="tm-field">
        <label htmlFor="tm-diag-test">{t("testCode")}</label>
        <textarea
          id="tm-diag-test"
          className="input-field tm-code-area"
          value={testCode}
          onChange={(e) => setTestCode(e.target.value)}
          placeholder={t("testCodePlaceholder")}
          rows={6}
          spellCheck={false}
        />
      </div>
      <div className="tm-field">
        <label htmlFor="tm-diag-src">{t("sourceCode")}</label>
        <textarea
          id="tm-diag-src"
          className="input-field tm-code-area"
          value={sourceCode}
          onChange={(e) => setSourceCode(e.target.value)}
          placeholder={t("sourceCodePlaceholder")}
          rows={6}
          spellCheck={false}
        />
      </div>

      <button type="button" className="btn btn-primary" onClick={() => void handleDiagnose()} disabled={busy}>
        {busy ? <Loader2 size={13} className="spin" /> : <AlertTriangle size={13} />}
        {busy ? t("diagnosing") : t("aiDiagnosis")}
      </button>

      {error && (
        <p className="tm-form-error" role="alert">
          <XCircle size={13} />
          {error}
        </p>
      )}

      {diagnosis && (
        <div className="tm-diagnosis" aria-live="polite">
          <h4>{t("diagnosisResult")}</h4>
          {fields
            .filter((field) => diagnosis[field.key])
            .map((field) => (
              <div key={field.key} className="tm-diagnosis-section">
                <span className="tm-diagnosis-label">{field.label}</span>
                <p className="tm-diagnosis-text">{diagnosis[field.key] as string}</p>
              </div>
            ))}
          {diagnosis.fixCode && (
            <div className="tm-diagnosis-section">
              <div className="tm-generated-head">
                <span className="tm-diagnosis-label">{t("fixCode")}</span>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={() => void handleCopyFix()}
                >
                  <Copy size={12} />
                  {t("copy")}
                </button>
              </div>
              <pre className="tm-output tm-output-code">{diagnosis.fixCode}</pre>
            </div>
          )}
        </div>
      )}
    </TestModal>
  );
}
