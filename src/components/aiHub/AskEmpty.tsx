import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import type { TaskKind } from "./config";

/** Empty-state starters: filled into the input (never auto-sent). `task` pre-switches the composer task pill. */
const EXAMPLES: { key: string; task?: TaskKind }[] = [
  { key: "recent" },
  { key: "impact" },
  { key: "uncommitted" },
  { key: "commitMsg" },
  { key: "error", task: "debug" },
  { key: "polish", task: "polish" },
  { key: "translate", task: "translate" },
  { key: "open" },
  { key: "model" },
];

export default function AskEmpty({ onExample }: { onExample: (text: string, task?: TaskKind) => void }) {
  const { t } = useTranslation("aihub");
  return (
    <div className="qa-empty">
      <Sparkles size={22} strokeWidth={1.5} className="qa-empty-icon" />
      <p className="qa-empty-title">{t("emptyHint")}</p>
      <p className="qa-empty-sub">{t("emptyHintSub")}</p>
      <div className="qa-examples">
        {EXAMPLES.map((ex) => (
          <button
            key={ex.key}
            type="button"
            className="qa-example"
            onClick={() => onExample(t(`examples.${ex.key}`), ex.task)}
          >
            {t(`examples.${ex.key}`)}
          </button>
        ))}
      </div>
    </div>
  );
}
