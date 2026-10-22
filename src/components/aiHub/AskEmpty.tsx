import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";

/** Empty-state starters: filled into the input (never auto-sent). */
const EXAMPLE_KEYS = [
  "recent",
  "impact",
  "uncommitted",
  "commitMsg",
  "error",
  "polish",
  "translate",
  "open",
  "model",
];

export default function AskEmpty({ onExample }: { onExample: (text: string) => void }) {
  const { t } = useTranslation("aihub");
  return (
    <div className="qa-empty">
      <Sparkles size={22} strokeWidth={1.5} className="qa-empty-icon" />
      <p className="qa-empty-title">{t("emptyHint")}</p>
      <p className="qa-empty-sub">{t("emptyHintSub")}</p>
      <div className="qa-examples">
        {EXAMPLE_KEYS.map((key) => (
          <button
            key={key}
            type="button"
            className="qa-example"
            onClick={() => onExample(t(`examples.${key}`))}
          >
            {t(`examples.${key}`)}
          </button>
        ))}
      </div>
    </div>
  );
}
