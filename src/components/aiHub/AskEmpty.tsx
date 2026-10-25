import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import type { HarnessMode, HarnessTrack } from "./harness";

/** Code 轨空态：git / diff / 提交类。 */
const CODE_EXAMPLE_KEYS = [
  "recent",
  "impact",
  "uncommitted",
  "commitMsg",
  "error",
  "open",
  "model",
];

/** Work 轨空态：改写 / 翻译 / 表格 / 日报 / 提炼。 */
const WORK_EXAMPLE_KEYS = [
  "polish",
  "translate",
  "daily",
  "summarize",
  "tableAsk",
  "rewriteMail",
  "model",
];

/** 对话钉住：偏纯问答，不推「改文件」。 */
const CHAT_EXAMPLE_KEYS = ["polish", "translate", "summarize", "error", "model"];

export default function AskEmpty({
  onExample,
  track = "code",
  pinnedMode,
}: {
  onExample: (text: string) => void;
  track?: HarnessTrack;
  /** `/mode` 钉住；chat 时用纯问答口径，避免仍说「改文件」。 */
  pinnedMode?: HarnessMode | null;
}) {
  const { t } = useTranslation("aihub");
  const chatPinned = pinnedMode === "chat";
  const keys = chatPinned
    ? CHAT_EXAMPLE_KEYS
    : track === "work"
      ? WORK_EXAMPLE_KEYS
      : CODE_EXAMPLE_KEYS;
  const titleKey = chatPinned ? "emptyHintChat" : track === "work" ? "emptyHintWork" : "emptyHint";
  const subKey = chatPinned
    ? "emptyHintSubChat"
    : track === "work"
      ? "emptyHintSubWork"
      : "emptyHintSub";
  return (
    <div className="qa-empty">
      <Sparkles size={22} strokeWidth={1.5} className="qa-empty-icon" />
      <p className="qa-empty-title">{t(titleKey)}</p>
      <p className="qa-empty-sub">{t(subKey)}</p>
      <div className="qa-examples">
        {keys.map((key) => (
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
