import { useEffect, useRef } from "react";
import { FileText, Sparkles, Terminal } from "lucide-react";
import type { PaletteItem } from "./harnessCommands";

type Props = {
  items: PaletteItem[];
  active: number;
  onHover: (index: number) => void;
  onPick: (item: PaletteItem) => void;
};

const ICONS = {
  command: Terminal,
  skill: Sparkles,
  snippet: FileText,
  file: FileText,
} as const;

/** `/` 命令与 `@` 引用共用的浮层；键盘导航在 AskComposer 里，渲染只认这一种形状。 */
export default function CommandPalette({ items, active, onHover, onPick }: Props) {
  const listRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    listRef.current?.querySelector(".qa-palette-row.is-active")?.scrollIntoView({ block: "nearest" });
  }, [active, items]);

  if (items.length === 0) return null;
  return (
    <div ref={listRef} className="qa-palette" role="listbox">
      {items.map((item, index) => {
        const Icon = ICONS[item.kind];
        return (
          <button
            key={`${item.kind}:${item.id}`}
            type="button"
            role="option"
            aria-selected={index === active}
            className={`qa-palette-row${index === active ? " is-active" : ""}`}
            onMouseEnter={() => onHover(index)}
            onClick={() => onPick(item)}
          >
            <Icon size={13} className="qa-palette-icon" aria-hidden />
            <span className="qa-palette-label">{item.label}</span>
            {item.hint && <span className="qa-palette-hint">{item.hint}</span>}
          </button>
        );
      })}
    </div>
  );
}
