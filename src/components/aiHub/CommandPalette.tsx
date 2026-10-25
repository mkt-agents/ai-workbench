import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { ChevronRight, Circle, CircleDot, FileText, Sparkles, Terminal } from "lucide-react";
import type { PaletteItem } from "./harnessCommands";

type Props = {
  items: PaletteItem[];
  active: number;
  onHover: (index: number) => void;
  onPick: (item: PaletteItem) => void;
  /** 二级列表标题；有值就渲染表头与「Esc 返回」。 */
  drillTitle?: string | null;
};

const ICONS = {
  command: Terminal,
  skill: Sparkles,
  snippet: Sparkles,
  file: FileText,
} as const;

/** 二级列表是单选语义：当前值用实心圆点，其余空心。 */
function OptionIcon({ checked }: { checked?: boolean }) {
  return checked ? (
    <CircleDot size={13} className="qa-palette-icon is-on" aria-hidden />
  ) : (
    <Circle size={13} className="qa-palette-icon" aria-hidden />
  );
}

/** `/` 命令与 `@` 引用共用的浮层；键盘导航在 AskComposer 里，渲染只认这一种形状。 */
export default function CommandPalette({ items, active, onHover, onPick, drillTitle }: Props) {
  const { t } = useTranslation("aihub");
  const listRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    listRef.current?.querySelector(".qa-palette-row.is-active")?.scrollIntoView({ block: "nearest" });
  }, [active, items]);

  if (items.length === 0) return null;
  // 行按钮 onMouseDown 阻止默认：点完焦点仍留在输入框，↑↓ 可以接着选
  return (
    <div ref={listRef} className={`qa-palette${drillTitle ? " is-drill" : ""}`} role="listbox">
      {drillTitle && (
        <div className="qa-palette-head" role="presentation">
          <span className="qa-palette-head-title">{drillTitle}</span>
          <span className="qa-palette-head-hint">{t("palette.backHint")}</span>
        </div>
      )}
      {items.map((item, index) => {
        const Icon = item.kind === "option" ? null : ICONS[item.kind];
        // 二级列表是「选一个」：名称 + 说明分两行，比一行挤右侧的说明好读
        return (
          <button
            key={`${item.kind}:${item.id}`}
            type="button"
            role="option"
            aria-selected={index === active}
            className={`qa-palette-row${index === active ? " is-active" : ""}${item.kind === "option" ? " is-option" : ""}`}
            onMouseDown={(e) => e.preventDefault()}
            onMouseEnter={() => onHover(index)}
            onClick={() => onPick(item)}
          >
            {Icon ? (
              <Icon size={13} className="qa-palette-icon" aria-hidden />
            ) : (
              <OptionIcon checked={item.kind === "option" && item.checked} />
            )}
            {item.kind === "option" ? (
              <span className="qa-palette-option">
                <span className="qa-palette-option-name">{item.label}</span>
                {item.hint && <span className="qa-palette-option-desc">{item.hint}</span>}
              </span>
            ) : (
              <>
                <span className="qa-palette-label">{item.label}</span>
                {item.hint && <span className="qa-palette-hint">{item.hint}</span>}
              </>
            )}
            {item.kind === "command" && item.drill && (
              <ChevronRight size={12} className="qa-palette-more" aria-hidden />
            )}
          </button>
        );
      })}
    </div>
  );
}
