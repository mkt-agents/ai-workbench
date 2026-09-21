import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ClipboardList, Loader2, Plus, Sparkles, Trash2 } from "lucide-react";
import type { Scenario, ScenarioSummary } from "../core/types";

const STATUSES = ["pending", "passed", "failed", "blocked"] as const;

type Props = {
  scenarios: Scenario[];
  summary: ScenarioSummary;
  busy: boolean;
  /** `false` until the AI section exists: the checklist is parsed out of it. */
  canGenerate: boolean;
  onGenerate: () => void;
  onAdd: (title: string) => void;
  onStatus: (scenario: Scenario, status: string) => void;
  onNote: (scenario: Scenario, note: string) => void;
  onDelete: (scenario: Scenario) => void;
};

/**
 * Acceptance items the tester ticks off. Stored per report, so the checklist survives a
 * reload and the exported markdown can carry its state.
 */
export default function ChangeScenarioChecklist({
  scenarios,
  summary,
  busy,
  canGenerate,
  onGenerate,
  onAdd,
  onStatus,
  onNote,
  onDelete,
}: Props) {
  const { t } = useTranslation("test");
  const [draft, setDraft] = useState("");
  const [noteFor, setNoteFor] = useState<number | null>(null);

  const submit = () => {
    const title = draft.trim();
    if (!title) return;
    onAdd(title);
    setDraft("");
  };

  return (
    <div className="tm-cr-sc">
      <div className="tm-cr-ai-head">
        <span className="tm-cr-label">
          <ClipboardList size={12} />
          {t("cr.sc.title")}
          {summary.total > 0 ? ` (${summary.total})` : ""}
        </span>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={onGenerate}
          disabled={busy || !canGenerate}
          title={canGenerate ? t("cr.sc.generateHint") : t("cr.sc.needAi")}
        >
          {busy ? <Loader2 size={12} className="spin" /> : <Sparkles size={12} />}
          {t("cr.sc.generate")}
        </button>
      </div>

      {summary.total > 0 && (
        <div className="tm-cr-sc-progress" title={t("cr.sc.progress", { passed: summary.passed, total: summary.total })}>
          <div className="tm-cr-sc-bar">
            <span
              className="tm-cr-sc-bar-fill"
              style={{ width: `${Math.max(0, Math.min(100, summary.percent))}%` }}
            />
          </div>
          <span className="tm-cr-sc-percent">{summary.percent}%</span>
          <span className="tm-cr-sc-counts">
            {t("cr.sc.status.passed")} {summary.passed} · {t("cr.sc.status.failed")} {summary.failed}
            {summary.blocked > 0 ? ` · ${t("cr.sc.status.blocked")} ${summary.blocked}` : ""}
            {` · ${t("cr.sc.status.pending")} ${summary.pending}`}
          </span>
        </div>
      )}

      {scenarios.length === 0 && (
        <p className="tm-hint">{canGenerate ? t("cr.sc.emptyHint") : t("cr.sc.needAi")}</p>
      )}

      {scenarios.length > 0 && (
        <ul className="tm-cr-sc-list">
          {scenarios.map((scenario) => (
            <li key={scenario.id} className={`tm-cr-sc-row is-${scenario.status}`}>
              <div className="tm-cr-sc-main">
                {scenario.priority && (
                  <span className={`tm-cr-sc-prio tm-cr-sc-prio-${scenario.priority}`}>{scenario.priority}</span>
                )}
                <div className="tm-cr-sc-text">
                  <span className="tm-cr-sc-title">{scenario.title}</span>
                  {scenario.detail && <span className="tm-cr-sc-detail">{scenario.detail}</span>}
                  {scenario.source === "manual" && <span className="tm-cr-sc-flag">{t("cr.sc.manual")}</span>}
                  {scenario.note && scenario.id !== noteFor && (
                    <span className="tm-cr-sc-note">{scenario.note}</span>
                  )}
                </div>
                <div className="tm-cr-sc-actions">
                  {STATUSES.map((status) => (
                    <button
                      key={status}
                      type="button"
                      className={`tm-cr-sc-status is-${status}${scenario.status === status ? " is-active" : ""}`}
                      onClick={() => onStatus(scenario, status)}
                      title={t(`cr.sc.status.${status}`, { defaultValue: status })}
                    >
                      {t(`cr.sc.status.${status}`, { defaultValue: status })}
                    </button>
                  ))}
                  <button
                    type="button"
                    className="tm-cr-sc-icon"
                    onClick={() => setNoteFor(noteFor === scenario.id ? null : scenario.id)}
                    title={t("cr.sc.noteHint")}
                  >
                    #
                  </button>
                  <button
                    type="button"
                    className="tm-cr-sc-icon is-danger"
                    onClick={() => onDelete(scenario)}
                    title={t("cr.sc.delete")}
                  >
                    <Trash2 size={11} />
                  </button>
                </div>
              </div>
              {scenario.id === noteFor && (
                <input
                  className="input-field tm-cr-sc-input"
                  defaultValue={scenario.note ?? ""}
                  placeholder={t("cr.sc.notePlaceholder")}
                  autoFocus
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      onNote(scenario, (e.target as HTMLInputElement).value);
                      setNoteFor(null);
                    }
                    if (e.key === "Escape") setNoteFor(null);
                  }}
                  onBlur={(e) => {
                    onNote(scenario, e.target.value);
                    setNoteFor(null);
                  }}
                />
              )}
            </li>
          ))}
        </ul>
      )}

      <div className="tm-cr-sc-add">
        <input
          className="input-field"
          value={draft}
          placeholder={t("cr.sc.addPlaceholder")}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
          }}
        />
        <button type="button" className="btn btn-secondary btn-small" onClick={submit} disabled={!draft.trim()}>
          <Plus size={12} />
          {t("cr.sc.add")}
        </button>
      </div>
    </div>
  );
}
