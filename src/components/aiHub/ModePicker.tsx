import { useTranslation } from "react-i18next";
import { Check, Code2, FileText, Globe, MessageSquare, Table2, Wrench, type LucideIcon } from "lucide-react";
import { HARNESS_MODES, type HarnessMode } from "./harness";

const ICONS: Record<HarnessMode, LucideIcon> = {
  chat: MessageSquare,
  code: Code2,
  doc: FileText,
  data: Table2,
  research: Globe,
  ops: Wrench,
};

type Props = {
  mode: HarnessMode;
  onMode: (next: HarnessMode) => void;
  open: boolean;
  onToggle: () => void;
};

/**
 * 任务模式胶囊：决定这一轮带哪些工具、要不要采项目上下文。
 * 比正则猜「这是不是代码问题」可靠——用户选什么就是什么。
 */
export default function ModePicker({ mode, onMode, open, onToggle }: Props) {
  const { t } = useTranslation("aihub");
  const Icon = ICONS[mode];
  return (
    <div className="qa-focus qa-mode">
      <button
        type="button"
        className={`qa-mode-pill is-${mode}`}
        onClick={onToggle}
        title={t(`mode.${mode}Desc`)}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <Icon size={12} />
        <span className="qa-mode-label">{t(`mode.${mode}`)}</span>
        <span className="qa-focus-caret">▾</span>
      </button>
      {open && (
        <div className="qa-focus-pop is-left is-up qa-mode-pop" role="listbox">
          {HARNESS_MODES.map((id) => {
            const ItemIcon = ICONS[id];
            const on = id === mode;
            return (
              <button
                key={id}
                type="button"
                role="option"
                aria-selected={on}
                className={`qa-focus-item qa-mode-item${on ? " on" : ""}`}
                onClick={() => {
                  onMode(id);
                  onToggle();
                }}
              >
                <ItemIcon size={13} className="qa-perm-item-icon" />
                <span className="qa-perm-item-text">
                  <span className="qa-perm-item-name">{t(`mode.${id}`)}</span>
                  <span className="qa-perm-item-desc">{t(`mode.${id}Desc`)}</span>
                </span>
                {on && <Check size={12} className="qa-perm-item-check" />}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
