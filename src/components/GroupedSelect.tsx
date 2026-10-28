import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, ChevronDown, Search } from "lucide-react";

export type GroupedSelectOption = {
  value: string;
  label: string;
  /** 次要说明（模型描述这类），有就显示第二行小字 */
  hint?: string | null;
};

export type GroupedSelectGroup = {
  key: string;
  label: string;
  options: GroupedSelectOption[];
};

type Row =
  | { type: "group"; key: string; label: string; count: number }
  | { type: "option"; key: string; option: GroupedSelectOption; group: string };

type Props = {
  value: string;
  groups: GroupedSelectGroup[];
  /** 空值那一项的文字（如「自动…」）；不给就没有空值项 */
  fallbackLabel?: string;
  /** 当前值已经不在候选里时，顶部补一条说明项 */
  staleLabel?: string;
  searchPlaceholder: string;
  /** 搜索无结果 */
  emptyLabel: string;
  disabled?: boolean;
  className?: string;
  onChange: (value: string) => void;
};

const MENU_GAP = 6;
const MENU_MARGIN = 8;
const MENU_MIN_WIDTH = 320;
const MENU_MAX_HEIGHT = 360;

/**
 * 分组单选（自绘列表）。
 *
 * 为什么不用原生 `<select>`：WebView2 的弹层里选项底色是我们自己的，
 * 但「选中 / hover」那一条是系统画的浅色高亮，白色文字压上去直接看不见；
 * `optgroup` 标题也会被系统压成近灰。原生弹层没有可用的覆盖点，
 * 所以列表、高亮、选中态全部自己画。
 */
export default function GroupedSelect({
  value,
  groups,
  fallbackLabel,
  staleLabel,
  searchPlaceholder,
  emptyLabel,
  disabled,
  className,
  onChange,
}: Props) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(-1);
  const [pos, setPos] = useState<
    { left: number; top?: number; bottom?: number; width: number; maxHeight: number } | null
  >(null);

  const rootRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const optionRefs = useRef(new Map<string, HTMLButtonElement>());
  const updatePosRef = useRef<() => void>(() => {});

  const known = useMemo(
    () => groups.some((group) => group.options.some((option) => option.value === value)),
    [groups, value],
  );

  const sections = useMemo(() => {
    const head: GroupedSelectOption[] = [];
    if (fallbackLabel) head.push({ value: "", label: fallbackLabel });
    if (value && !known && staleLabel) head.push({ value, label: staleLabel });
    const out: { label: string; options: GroupedSelectOption[] }[] = [];
    if (head.length) out.push({ label: "", options: head });
    for (const group of groups) out.push({ label: group.label, options: group.options });
    return out;
  }, [fallbackLabel, groups, known, staleLabel, value]);

  const flat = useMemo(
    () => sections.flatMap((section) => section.options),
    [sections],
  );

  const trimmed = query.trim().toLowerCase();
  const rows = useMemo(() => {
    const out: Row[] = [];
    sections.forEach((section, index) => {
      const matched = trimmed
        ? section.options.filter((option) =>
            `${option.label} ${option.hint ?? ""} ${section.label}`.toLowerCase().includes(trimmed),
          )
        : section.options;
      if (!matched.length) return;
      // 分组一律保留：搜索命中的选项仍然挂在自己的 provider 下面
      if (section.label) out.push({ type: "group", key: `g${index}`, label: section.label, count: matched.length });
      for (const option of matched) {
        out.push({ type: "option", key: `o${index}:${option.value}`, option, group: section.label });
      }
    });
    return out;
  }, [sections, trimmed]);

  // 打开时把高亮放到当前值上；搜索词变了就落到第一条
  useEffect(() => {
    if (!open) return;
    const hit = rows.findIndex((row) => row.type === "option" && row.option.value === value);
    const first = rows.findIndex((row) => row.type === "option");
    setActive(hit >= 0 ? hit : first);
    // rows 只由搜索词和候选集合派生，这里按 q 重算就够
  }, [open, trimmed]);

  useEffect(() => {
    if (!open) return;
    const id = window.requestAnimationFrame(() => searchRef.current?.focus({ preventScroll: true }));
    return () => window.cancelAnimationFrame(id);
  }, [open]);

  useLayoutEffect(() => {
    if (!open) {
      setPos(null);
      return;
    }
    const trigger = triggerRef.current;
    if (!trigger) return;
    const update = () => {
      const rect = trigger.getBoundingClientRect();
      const below = window.innerHeight - rect.bottom - MENU_GAP - MENU_MARGIN;
      const above = rect.top - MENU_GAP - MENU_MARGIN;
      const up = below < 200 && above > below;
      const width = Math.max(rect.width, MENU_MIN_WIDTH);
      const left = Math.min(
        Math.max(MENU_MARGIN, rect.left),
        Math.max(MENU_MARGIN, window.innerWidth - width - MENU_MARGIN),
      );
      const maxHeight = Math.max(140, Math.min(MENU_MAX_HEIGHT, up ? above : below));
      setPos(
        up
          ? { left, bottom: window.innerHeight - rect.top + MENU_GAP, width, maxHeight }
          : { left, top: rect.bottom + MENU_GAP, width, maxHeight },
      );
    };
    updatePosRef.current = update;
    update();
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, [open]);

  // 页面滚动时跟随触发器；列表自己滚动不算
  useEffect(() => {
    if (!open) return;
    const onScroll = (event: Event) => {
      const target = event.target;
      if (target instanceof Node && menuRef.current?.contains(target)) return;
      updatePosRef.current();
    };
    window.addEventListener("scroll", onScroll, true);
    return () => window.removeEventListener("scroll", onScroll, true);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (rootRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => document.removeEventListener("pointerdown", onPointerDown, true);
  }, [open]);

  useEffect(() => {
    if (open) return;
    setQuery("");
  }, [open]);

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  useEffect(() => {
    if (!open || active < 0) return;
    const row = rows[active];
    if (!row || row.type !== "option") return;
    optionRefs.current.get(row.key)?.scrollIntoView({ block: "nearest" });
  }, [active, open, rows]);

  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) triggerRef.current?.focus({ preventScroll: true });
  };

  const choose = (next: string) => {
    close(true);
    if (next !== value) onChange(next);
  };

  const current = flat.find((option) => option.value === value);
  const label = current?.label ?? (value ? (staleLabel ?? value) : (fallbackLabel ?? ""));
  const isFallback = value === "" && Boolean(fallbackLabel);

  const onMenuKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      close(true);
      return;
    }
    if (event.key === "Tab") {
      close(false);
      return;
    }
    const stops = rows.map((row, index) => (row.type === "option" ? index : -1)).filter((i) => i >= 0);
    if (!stops.length) return;
    const at = stops.indexOf(active);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      setActive(at < 0 ? stops[0] : stops[(at + step + stops.length) % stops.length]);
      return;
    }
    if (event.key === "Home") {
      event.preventDefault();
      setActive(stops[0]);
      return;
    }
    if (event.key === "End") {
      event.preventDefault();
      setActive(stops[stops.length - 1]);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const row = rows[active];
      if (row && row.type === "option") choose(row.option.value);
    }
  };

  const menu =
    open && pos
      ? createPortal(
          <div
            ref={menuRef}
            className="mp-menu"
            style={{
              left: pos.left,
              top: pos.top,
              bottom: pos.bottom,
              width: pos.width,
              maxHeight: pos.maxHeight,
            }}
            onKeyDown={onMenuKeyDown}
          >
            <label className="mp-search">
              <Search size={12} aria-hidden />
              <input
                ref={searchRef}
                value={query}
                placeholder={searchPlaceholder}
                onChange={(event) => setQuery(event.target.value)}
                spellCheck={false}
                autoComplete="off"
                aria-label={searchPlaceholder}
              />
            </label>
            <div className="mp-list" role="listbox" aria-label={searchPlaceholder}>
              {rows.length === 0 ? <p className="mp-empty">{emptyLabel}</p> : null}
              {rows.map((row, index) => {
                if (row.type === "group") {
                  return (
                    <div key={row.key} className="mp-group">
                      <span className="mp-group-name">{row.label}</span>
                      <span className="mp-group-count">{row.count}</span>
                    </div>
                  );
                }
                const selected = row.option.value === value;
                const classes = ["mp-option", index === active ? "is-active" : "", selected ? "is-selected" : ""]
                  .filter(Boolean)
                  .join(" ");
                return (
                  <button
                    key={row.key}
                    type="button"
                    role="option"
                    aria-selected={selected}
                    className={classes}
                    title={row.option.hint || row.option.label}
                    ref={(node) => {
                      if (node) optionRefs.current.set(row.key, node);
                      else optionRefs.current.delete(row.key);
                    }}
                    onMouseEnter={() => setActive(index)}
                    onClick={() => choose(row.option.value)}
                  >
                    <span className="mp-option-copy">
                      <span className="mp-option-label">{row.option.label}</span>
                      {row.option.hint ? <span className="mp-option-hint">{row.option.hint}</span> : null}
                    </span>
                    {selected ? <Check size={13} className="mp-check" aria-hidden /> : null}
                  </button>
                );
              })}
            </div>
          </div>,
          document.body,
        )
      : null;

  return (
    <div ref={rootRef} className={["mp", className].filter(Boolean).join(" ")}>
      <button
        ref={triggerRef}
        type="button"
        className="mp-trigger"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={label}
        onClick={() => {
          if (disabled) return;
          setOpen((prev) => !prev);
        }}
        onKeyDown={(event) => {
          if (disabled) return;
          if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            setOpen(true);
          }
        }}
      >
        <span className={`mp-trigger-label${isFallback ? " is-empty" : ""}`}>{label}</span>
        <ChevronDown size={13} className="mp-chevron" aria-hidden />
      </button>
      {menu}
    </div>
  );
}