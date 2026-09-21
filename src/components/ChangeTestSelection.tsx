import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Play, Target } from "lucide-react";
import type { TestSelection } from "../core/types";

type Props = {
  selection: TestSelection | null;
  loading: boolean;
  running: boolean;
  /** Names the user left ticked; kept by the parent so the export can reuse them. */
  ticked: string[];
  onTicked: (names: string[]) => void;
  onRun: () => void;
};

/**
 * The tests the change points at, tickable. Only the tick list lives here — the runner
 * arguments are rebuilt by the backend from these names, because the filter syntax is
 * per-framework knowledge that must not be duplicated in the UI.
 */
export default function ChangeTestSelection({ selection, loading, running, ticked, onTicked, onRun }: Props) {
  const { t } = useTranslation("test");
  const targets = selection?.targets ?? [];
  const checked = useMemo(() => new Set(ticked), [ticked]);

  const toggle = (name: string) => {
    const next = new Set(checked);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    // Keep the backend's order so the rebuilt command matches the unfiltered one.
    onTicked(targets.filter((target) => next.has(target.name)).map((target) => target.name));
  };

  const tickedCount = targets.filter((target) => checked.has(target.name)).length;
  // Nothing ticked means "run the whole suite", not "run nothing" — the same command the
  // list page's 运行 button issues.
  const argsText = tickedCount === 0 ? "" : selection?.args || "";
  const runLabel =
    tickedCount === 0 || selection?.selectable === false
      ? t("cr.sel.runAll")
      : t("cr.sel.runSelected", { n: tickedCount });
  const runnable = Boolean(selection) && !loading && !running;

  return (
    <div className="tm-cr-sel">
      <div className="tm-cr-ai-head">
        <span className="tm-cr-label">
          <Target size={12} />
          {t("cr.sel.title")}
          {selection && targets.length > 0 ? ` (${targets.length})` : ""}
        </span>
        <button
          type="button"
          className="btn btn-secondary btn-small"
          onClick={onRun}
          disabled={!runnable}
          title={argsText || t("cr.sel.runAllHint")}
        >
          {running ? <Loader2 size={12} className="spin" /> : <Play size={12} />}
          {runLabel}
        </button>
      </div>

      {loading && <p className="tm-hint">{t("cr.sel.loading")}</p>}

      {!loading && selection && targets.length === 0 && <p className="tm-hint">{t("cr.sel.none")}</p>}

      {!loading && selection && targets.length > 0 && (
        <>
          {selection.selectable === false && <p className="tm-hint">{t("cr.sel.unsupported")}</p>}
          <ul className="tm-cr-sel-list">
            {targets.map((target) => (
              <li key={target.name} className="tm-cr-sel-row">
                <input
                  type="checkbox"
                  checked={checked.has(target.name)}
                  onChange={() => toggle(target.name)}
                  disabled={!selection.selectable || running}
                  aria-label={target.name}
                />
                <code className="tm-cr-sel-name" title={target.kind}>
                  {target.name}
                </code>
                <span className="tm-cr-sel-from" title={target.from}>
                  {target.from}
                </span>
                {target.changed && <span className="tm-cr-sel-flag">{t("cr.sel.changed")}</span>}
              </li>
            ))}
          </ul>
          {selection.truncated && <p className="tm-hint">{t("cr.sel.truncated")}</p>}
        </>
      )}

      {selection && selection.gaps.length > 0 && (
        <details className="tm-cr-sel-gaps">
          <summary>{t("cr.sel.gapCount", { n: selection.gaps.length })}</summary>
          <ul>
            {selection.gaps.slice(0, 50).map((gap) => (
              <li key={gap}>
                <code>{gap}</code>
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
