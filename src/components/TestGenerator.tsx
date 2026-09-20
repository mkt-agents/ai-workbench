import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, FileText, Loader2, RefreshCw, Save, XCircle } from "lucide-react";
import { useGlobalStore } from "../core/store";
import TestModal from "./TestModal";
import { stripCodeFence } from "../lib/aiText";
import type { TestGenOptions, TestProject } from "../core/types";

type Props = {
  project: TestProject;
  onClose: () => void;
  onToast: (type: "success" | "error", text: string) => void;
};

const COVERAGE_LEVELS: TestGenOptions["coverageLevel"][] = [
  "comprehensive",
  "basic",
  "boundary",
  "exception",
];
const MOCK_STRATEGIES: TestGenOptions["mockStrategy"][] = ["auto", "manual", "skip"];
const ASSERT_STYLES: TestGenOptions["assertStyle"][] = ["expect", "assert", "should"];

/** `src/a.ts` → `src/a.test.ts` (pytest uses `test_a.py`). */
function suggestedTestPath(filePath: string, framework: string): string {
  const base = filePath.replace(/\\/g, "/");
  const dot = base.lastIndexOf(".");
  const stem = dot > 0 ? base.slice(0, dot) : base;
  const ext = dot > 0 ? base.slice(dot) : ".ts";
  if (framework === "pytest") {
    const slash = base.lastIndexOf("/");
    return `${base.slice(0, slash + 1)}test_${base.slice(slash + 1).replace(/\.py$/, "")}.py`;
  }
  return `${stem}.test${ext}`;
}

export default function TestGenerator({ project, onClose, onToast }: Props) {
  const { t } = useTranslation("test");
  const generateTestCode = useGlobalStore((s) => s.generateTestCode);
  const saveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);
  const readTextFile = useGlobalStore((s) => s.invokeReadTextFile);
  const pickSourceFile = useGlobalStore((s) => s.invokePickSourceFile);

  const [sourceCode, setSourceCode] = useState("");
  const [filePath, setFilePath] = useState("");
  const [coverageLevel, setCoverageLevel] = useState<TestGenOptions["coverageLevel"]>("comprehensive");
  const [mockStrategy, setMockStrategy] = useState<TestGenOptions["mockStrategy"]>("auto");
  const [assertStyle, setAssertStyle] = useState<TestGenOptions["assertStyle"]>("expect");
  const [generated, setGenerated] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handlePickFile = useCallback(async () => {
    try {
      const picked = await pickSourceFile(t("selectFile"));
      if (!picked) return;
      setFilePath(picked);
      setError(null);
      setSourceCode(await readTextFile(picked));
    } catch (e) {
      const text = String(e);
      // A dismissed dialog reports as an error — that is not something to surface.
      if (!/取消|cancel/i.test(text)) setError(text);
    }
  }, [pickSourceFile, readTextFile, t]);

  const handleGenerate = useCallback(async () => {
    if (!sourceCode.trim() || !filePath.trim()) {
      setError(t("formIncomplete"));
      return;
    }
    setBusy(true);
    setError(null);
    setGenerated("");
    try {
      const code = await generateTestCode(
        sourceCode,
        filePath,
        project.framework,
        coverageLevel,
        mockStrategy,
        assertStyle
      );
      setGenerated(stripCodeFence(code));
      onToast("success", t("generateSuccess"));
    } catch (e) {
      setError(String(e));
      onToast("error", t("generateFailed", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  }, [sourceCode, filePath, project.framework, coverageLevel, mockStrategy, assertStyle, generateTestCode, onToast, t]);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(generated);
      onToast("success", t("copied"));
    } catch (e) {
      onToast("error", String(e));
    }
  }, [generated, onToast, t]);

  const handleSave = useCallback(async () => {
    try {
      const path = await saveTextFile(generated, suggestedTestPath(filePath, project.framework).split("/").pop() || "test.test", t("saveFile"));
      onToast("success", t("savedTo", { path }));
    } catch (e) {
      const text = String(e);
      // The native dialog reports a dismissed save as an error.
      if (!/取消|cancel/i.test(text)) onToast("error", text);
    }
  }, [generated, filePath, project.framework, saveTextFile, onToast, t]);

  return (
    <TestModal title={`${t("aiTestGenerator")} · ${project.name}`} onClose={onClose} busy={busy} wide>
      <div className="tm-field">
        <label htmlFor="tm-gen-path">{t("selectFile")}</label>
        <div className="tm-field-row">
          <input
            id="tm-gen-path"
            className="input-field"
            value={filePath}
            readOnly
            placeholder={t("sourcePathPlaceholder")}
          />
          <button
            type="button"
            className="btn btn-secondary"
            onClick={() => void handlePickFile()}
            disabled={busy}
          >
            <FileText size={13} />
            {t("browse")}
          </button>
        </div>
        <p className="tm-hint">{t("pickFileHint")}</p>
      </div>

      <div className="tm-field">
        <label htmlFor="tm-gen-src">{t("sourceCode")}</label>
        <textarea
          id="tm-gen-src"
          className="input-field tm-code-area"
          value={sourceCode}
          onChange={(e) => setSourceCode(e.target.value)}
          placeholder={t("sourceCodePlaceholder")}
          rows={8}
          spellCheck={false}
        />
      </div>

      <div className="tm-field-grid tm-field-grid-3">
        <div className="tm-field">
          <label htmlFor="tm-gen-coverage">{t("coverageLevel")}</label>
          <select
            id="tm-gen-coverage"
            className="input-field"
            value={coverageLevel}
            onChange={(e) => setCoverageLevel(e.target.value as TestGenOptions["coverageLevel"])}
          >
            {COVERAGE_LEVELS.map((level) => (
              <option key={level} value={level}>
                {t(`genCoverage.${level}`)}
              </option>
            ))}
          </select>
        </div>
        <div className="tm-field">
          <label htmlFor="tm-gen-mock">{t("mockStrategy")}</label>
          <select
            id="tm-gen-mock"
            className="input-field"
            value={mockStrategy}
            onChange={(e) => setMockStrategy(e.target.value as TestGenOptions["mockStrategy"])}
          >
            {MOCK_STRATEGIES.map((strategy) => (
              <option key={strategy} value={strategy}>
                {t(`mock.${strategy}`)}
              </option>
            ))}
          </select>
        </div>
        <div className="tm-field">
          <label htmlFor="tm-gen-assert">{t("assertionStyle")}</label>
          <select
            id="tm-gen-assert"
            className="input-field"
            value={assertStyle}
            onChange={(e) => setAssertStyle(e.target.value as TestGenOptions["assertStyle"])}
          >
            {ASSERT_STYLES.map((style) => (
              <option key={style} value={style}>
                {t(`assert.${style}`)}
              </option>
            ))}
          </select>
        </div>
      </div>

      <button
        type="button"
        className="btn btn-primary"
        onClick={() => void handleGenerate()}
        disabled={busy}
      >
        {busy ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
        {busy ? t("generating") : t("generateTest")}
      </button>

      {error && (
        <p className="tm-form-error" role="alert">
          <XCircle size={13} />
          {error}
        </p>
      )}

      {generated && (
        <div className="tm-generated">
          <div className="tm-generated-head">
            <h4>{t("generatedCode")}</h4>
            <div className="tm-toolbar-actions">
              <button type="button" className="btn btn-secondary btn-small" onClick={() => void handleCopy()}>
                <Copy size={12} />
                {t("copy")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => void handleSave()}
                title={t("saveFileHint")}
              >
                <Save size={12} />
                {t("saveFile")}
              </button>
            </div>
          </div>
          <pre className="tm-output tm-output-code">{generated}</pre>
        </div>
      )}
    </TestModal>
  );
}
