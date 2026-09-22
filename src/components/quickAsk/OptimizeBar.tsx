import { useTranslation } from "react-i18next";
import { PROMPT_GOALS, PROMPT_SCENARIOS } from "../../lib/promptOptimize";
import type { PromptGoal } from "../../lib/promptOptimize";
import type { OptimizeConfig } from "./config";

type Props = {
  value: OptimizeConfig;
  onChange: (v: OptimizeConfig) => void;
};

/** Scenario + goal chips shown above the input when task = optimize. */
export default function OptimizeBar({ value, onChange }: Props) {
  const { t: tAi } = useTranslation("ai");
  const toggleGoal = (g: PromptGoal) => {
    const on = value.goals.includes(g);
    onChange({ ...value, goals: on ? value.goals.filter((x) => x !== g) : [...value.goals, g] });
  };
  return (
    <div className="qa-optimize-bar">
      <div className="qa-opt-row">
        {PROMPT_SCENARIOS.map((s) => (
          <button
            key={s}
            type="button"
            className={`qa-opt-chip${value.scenario === s ? " active" : ""}`}
            onClick={() => onChange({ ...value, scenario: s })}
          >
            {tAi(`prompts.scenarios.${s}`)}
          </button>
        ))}
      </div>
      <div className="qa-opt-row">
        {PROMPT_GOALS.map((g) => (
          <button
            key={g}
            type="button"
            className={`qa-opt-chip is-goal${value.goals.includes(g) ? " active" : ""}`}
            aria-pressed={value.goals.includes(g)}
            onClick={() => toggleGoal(g)}
          >
            {tAi(`prompts.goal.${g}`)}
          </button>
        ))}
      </div>
    </div>
  );
}
