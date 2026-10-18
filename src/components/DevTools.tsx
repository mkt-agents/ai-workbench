import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Braces,
  Calculator,
  CalendarClock,
  ChevronLeft,
  ChevronRight,
  Clock,
  Code2,
  Hash,
  Lock,
  Regex,
  Search,
  Shuffle,
  X,
} from "lucide-react";
import JsonTool from "./devtools/JsonTool";
import EncoderTool from "./devtools/EncoderTool";
import TimestampTool from "./devtools/TimestampTool";
import CronTool from "./devtools/CronTool";
import UuidTool from "./devtools/UuidTool";
import JwtTool from "./devtools/JwtTool";
import RegexTool from "./devtools/RegexTool";
import HashTool from "./devtools/HashTool";
import CalculatorTool from "./devtools/CalculatorTool";

/** 当前工具 id；智枢跳转写它（storage 事件跨窗同步），本页监听切换。 */
export const DEVTOOLS_TOOL_KEY = "workbench-devtools-tool";
/** 工具条顺序（可拖动）；智枢工具箱读同一个 key，storage 事件跨窗同步。 */
export const DEVTOOLS_ORDER_KEY = "workbench-devtools-order";

type ToolDef = {
  id: string;
  labelKey: string;
  icon: React.ReactNode;
  descriptionKey: string;
};

type ToolGroup = {
  groupKey: string;
  tools: ToolDef[];
};

const TOOL_GROUPS: ToolGroup[] = [
  {
    groupKey: "group.convert",
    tools: [
      { id: "json", labelKey: "json.title", icon: <Braces size={16} />, descriptionKey: "json.description" },
      { id: "encoder", labelKey: "encoder.title", icon: <Code2 size={16} />, descriptionKey: "encoder.description" },
      { id: "timestamp", labelKey: "timestamp.title", icon: <Clock size={16} />, descriptionKey: "timestamp.description" },
      { id: "cron", labelKey: "cron.title", icon: <CalendarClock size={16} />, descriptionKey: "cron.description" },
    ],
  },
  {
    groupKey: "group.generate",
    tools: [
      { id: "uuid", labelKey: "uuid.title", icon: <Shuffle size={16} />, descriptionKey: "uuid.description" },
      { id: "hash", labelKey: "hash.title", icon: <Hash size={16} />, descriptionKey: "hash.description" },
    ],
  },
  {
    groupKey: "group.parse",
    tools: [
      { id: "jwt", labelKey: "jwt.title", icon: <Lock size={16} />, descriptionKey: "jwt.description" },
      { id: "regex", labelKey: "regex.title", icon: <Regex size={16} />, descriptionKey: "regex.description" },
    ],
  },
  {
    groupKey: "group.calc",
    tools: [
      { id: "calc", labelKey: "calc.title", icon: <Calculator size={16} />, descriptionKey: "calc.description" },
    ],
  },
];

/** id → 工具定义（带所属分组，供分隔线与 tooltip 用）。 */
const TOOL_BY_ID: Map<string, ToolDef & { groupKey: string }> = new Map(
  TOOL_GROUPS.flatMap((g) => g.tools.map((t) => [t.id, { ...t, groupKey: g.groupKey }]))
);

/** 默认平铺顺序：常用文本/数据工具在前，计算器紧跟（不垫底），低频的 Cron/JWT/正则靠后。 */
export const DEFAULT_TOOL_ORDER = ["json", "encoder", "calc", "timestamp", "cron", "uuid", "hash", "jwt", "regex"];

/** 读取持久化顺序：未知 id 丢弃、新工具追加到末尾，保证与注册表一致。 */
export function loadToolOrder(allIds: string[]): string[] {
  try {
    const raw = localStorage.getItem(DEVTOOLS_ORDER_KEY);
    if (!raw) return allIds;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return allIds;
    const known = new Set(allIds);
    const cleaned = parsed.filter((x): x is string => typeof x === "string" && known.has(x));
    const missing = allIds.filter((id) => !cleaned.includes(id));
    return [...cleaned, ...missing];
  } catch {
    return allIds;
  }
}

/** Global 1-based index of a tool across all groups; used for the 1-9 hotkeys. */
function toolHotkeyIndex(allTools: ToolDef[], id: string): number {
  return allTools.findIndex((t) => t.id === id) + 1;
}

function DevTools({ active = true }: { active?: boolean }) {
  const { t } = useTranslation("devtools");

  const [toolOrder, setToolOrder] = useState<string[]>(() => loadToolOrder(DEFAULT_TOOL_ORDER));
  const allTools = useMemo(
    () => toolOrder.map((id) => TOOL_BY_ID.get(id)).filter((t): t is NonNullable<typeof t> => !!t),
    [toolOrder]
  );

  const [activeTool, setActiveTool] = useState<string>(() => {
    const stored = localStorage.getItem(DEVTOOLS_TOOL_KEY);
    return stored && TOOL_BY_ID.has(stored) ? stored : "json";
  });

  const [search, setSearch] = useState("");

  useEffect(() => {
    localStorage.setItem(DEVTOOLS_TOOL_KEY, activeTool);
  }, [activeTool]);

  // 跨窗同步：智枢「跳转」写入该 key 时切到对应工具（storage 事件只在别的窗口触发，
  // 覆盖本页已挂载的场景；未挂载时走 mount 时的 init 读取）。
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key !== DEVTOOLS_TOOL_KEY || e.newValue == null) return;
      if (TOOL_BY_ID.has(e.newValue)) setActiveTool(e.newValue);
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  useEffect(() => {
    localStorage.setItem(DEVTOOLS_ORDER_KEY, JSON.stringify(toolOrder));
  }, [toolOrder]);

  const stripRef = useRef<HTMLElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  // Whether the strip can scroll further in each direction (drives the arrows / edge fades).
  const [stripScroll, setStripScroll] = useState({ left: false, right: false });

  const syncStripArrows = useCallback(() => {
    const el = stripRef.current;
    if (!el) return;
    const max = el.scrollWidth - el.clientWidth;
    const left = el.scrollLeft > 1;
    const right = max > 1 && el.scrollLeft < max - 1;
    // Keep the same object when nothing changed so scrolling doesn't re-render every frame.
    setStripScroll((prev) => (prev.left === left && prev.right === right ? prev : { left, right }));
  }, []);

  useEffect(() => {
    const el = stripRef.current;
    if (!el) return;
    syncStripArrows();
    el.addEventListener("scroll", syncStripArrows, { passive: true });
    const ro = new ResizeObserver(syncStripArrows);
    ro.observe(el);
    // A plain mouse wheel has no horizontal axis — pan the strip horizontally instead.
    const onWheel = (e: WheelEvent) => {
      if (el.scrollWidth <= el.clientWidth) return;
      if (Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return;
      e.preventDefault();
      el.scrollLeft += e.deltaY;
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      el.removeEventListener("scroll", syncStripArrows);
      ro.disconnect();
      el.removeEventListener("wheel", onWheel);
    };
  }, [syncStripArrows]);

  // The overflow distance changes when the filtered chip set changes.
  useEffect(() => {
    syncStripArrows();
  }, [search, syncStripArrows]);

  const scrollStrip = useCallback((dir: -1 | 1) => {
    const el = stripRef.current;
    if (!el) return;
    el.scrollBy({ left: dir * Math.max(140, el.clientWidth * 0.7), behavior: "smooth" });
  }, []);

  // Keyboard: ← / → pan the strip when focus isn't in a text field.
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      const el = stripRef.current;
      if (!el || el.scrollWidth <= el.clientWidth) return;
      e.preventDefault();
      scrollStrip(e.key === "ArrowLeft" ? -1 : 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, scrollStrip]);

  // Keep the active chip in view while leaving room for the overlay arrows.
  useEffect(() => {
    const el = stripRef.current;
    const chip = el?.querySelector<HTMLElement>(`[data-tool-id="${activeTool}"]`);
    if (!el || !chip || !el.clientWidth) return;
    const pad = 34;
    const left = chip.offsetLeft;
    const right = left + chip.offsetWidth;
    if (left < el.scrollLeft + pad) {
      el.scrollTo({ left: Math.max(0, left - pad), behavior: "smooth" });
    } else if (right > el.scrollLeft + el.clientWidth - pad) {
      el.scrollTo({ left: right - el.clientWidth + pad, behavior: "smooth" });
    }
  }, [activeTool]);

  // Keyboard shortcut: 1-9 maps to tools in global order
  useEffect(() => {
    if (!active) return;
    const handler = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      if ((e.target as HTMLElement).isContentEditable) return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === "/") {
        e.preventDefault();
        searchRef.current?.focus();
        return;
      }
      const num = parseInt(e.key, 10);
      if (num >= 1 && num <= 9 && num <= allTools.length) {
        e.preventDefault();
        setActiveTool(allTools[num - 1].id);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [active, allTools]);

  // Filter tools based on search (over the current drag-ordered flat list)
  const filteredTools = useMemo(() => {
    if (!search.trim()) return allTools;
    const searchLower = search.toLowerCase();
    return allTools.filter(
      (tool) =>
        t(tool.labelKey).toLowerCase().includes(searchLower) ||
        t(tool.descriptionKey).toLowerCase().includes(searchLower)
    );
  }, [search, allTools, t]);

  const handleSearchKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Escape") {
      if (search) setSearch("");
      else e.currentTarget.blur();
    } else if (e.key === "Enter") {
      const first = filteredTools[0];
      if (!first) return;
      setActiveTool(first.id);
      setSearch("");
      e.currentTarget.blur();
    }
  };

  // --- 工具条拖动排序（HTML5 DnD；搜索过滤时禁用，避免相对位置歧义） ---
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropHint, setDropHint] = useState<{ id: string; side: "before" | "after" } | null>(null);

  const clearDrag = useCallback(() => {
    setDragId(null);
    setDropHint(null);
  }, []);

  const handleDragStart = (e: React.DragEvent, id: string) => {
    if (search.trim()) {
      e.preventDefault();
      return;
    }
    setDragId(id);
    e.dataTransfer.effectAllowed = "move";
    // Firefox 需要 setData 才会启动拖拽
    e.dataTransfer.setData("text/plain", id);
  };

  const handleDragOver = (e: React.DragEvent, id: string) => {
    if (!dragId || dragId === id) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    const rect = e.currentTarget.getBoundingClientRect();
    const side = e.clientX < rect.left + rect.width / 2 ? "before" : "after";
    setDropHint((prev) => (prev?.id === id && prev.side === side ? prev : { id, side }));
  };

  const handleDrop = (e: React.DragEvent, overId: string) => {
    e.preventDefault();
    const dragging = dragId;
    clearDrag();
    if (!dragging || dragging === overId) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const after = e.clientX >= rect.left + rect.width / 2;
    setToolOrder((prev) => {
      const next = prev.filter((x) => x !== dragging);
      next.splice(next.indexOf(overId) + (after ? 1 : 0), 0, dragging);
      return next;
    });
  };

  const activeToolMeta = useMemo(() => allTools.find((t) => t.id === activeTool), [activeTool, allTools]);

  const renderTool = () => {
    switch (activeTool) {
      case "json":         return <JsonTool />;
      case "encoder":      return <EncoderTool />;
      case "timestamp":    return <TimestampTool />;
      case "cron":         return <CronTool />;
      case "uuid":         return <UuidTool />;
      case "jwt":          return <JwtTool />;
      case "regex":        return <RegexTool />;
      case "hash":         return <HashTool />;
      case "calc":         return <CalculatorTool />;
      default:             return <JsonTool />;
    }
  };

  /** Chip in the top tool strip — group name is carried by the tooltip. */
  const renderToolButton = (tool: ToolDef & { groupKey: string }) => {
    const hotkey = toolHotkeyIndex(allTools, tool.id);
    const isActive = activeTool === tool.id;
    const isFirstMatch = !!search.trim() && filteredTools[0]?.id === tool.id;
    const canDrag = !search.trim();
    return (
      <button
        key={tool.id}
        type="button"
        data-tool-id={tool.id}
        className={`dt-tool-item${isActive ? " active" : ""}${isFirstMatch ? " is-first-match" : ""}${
          dragId === tool.id ? " is-dragging" : ""
        }${dropHint?.id === tool.id ? ` drop-${dropHint.side}` : ""}`}
        onClick={() => setActiveTool(tool.id)}
        draggable={canDrag}
        onDragStart={(e) => handleDragStart(e, tool.id)}
        onDragOver={(e) => handleDragOver(e, tool.id)}
        onDrop={(e) => handleDrop(e, tool.id)}
        onDragEnd={clearDrag}
        title={`${t(tool.groupKey)} · ${t(tool.descriptionKey)}${canDrag ? ` · ${t("common.dragHint")}` : ""}`}
        aria-pressed={isActive}
      >
        <span className="dt-tool-icon">{tool.icon}</span>
        <span className="dt-tool-label">{t(tool.labelKey)}</span>
        {hotkey >= 1 && hotkey <= 9 && (
          <kbd className="dt-tool-kbd">{hotkey}</kbd>
        )}
      </button>
    );
  };

  const noResults = search.trim() !== "" && filteredTools.length === 0;

  return (
    <div className="dt-layout">
      <header className="dt-toolbar">
        <div className="dt-toolbar-inner">
        <div className="dt-strip-wrap">
          <nav ref={stripRef} className="dt-toolstrip" role="navigation" aria-label={t("title")}>
            {filteredTools.map((tool) => renderToolButton(tool))}

            {noResults && (
              <div className="dt-strip-empty">
                <Search size={13} />
                <span>{t("common.noResults")}</span>
                <button type="button" className="dt-empty-clear" onClick={() => setSearch("")}>
                  {t("common.clearSearch")}
                </button>
              </div>
            )}
          </nav>

          {stripScroll.left && (
            <button
              type="button"
              className="dt-strip-arrow left"
              onClick={() => scrollStrip(-1)}
              aria-label={t("common.scrollLeft")}
              title={t("common.scrollLeft")}
              tabIndex={-1}
            >
              <ChevronLeft size={15} />
            </button>
          )}
          {stripScroll.right && (
            <button
              type="button"
              className="dt-strip-arrow right"
              onClick={() => scrollStrip(1)}
              aria-label={t("common.scrollRight")}
              title={t("common.scrollRight")}
              tabIndex={-1}
            >
              <ChevronRight size={15} />
            </button>
          )}
        </div>

        <div className="dt-toolbar-search">
          <Search size={13} />
          <input
            ref={searchRef}
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={handleSearchKeyDown}
            placeholder={t("common.search")}
            title={t("common.searchHint")}
            spellCheck={false}
            aria-label={t("common.search")}
          />
          {search && (
            <button
              type="button"
              className="dt-search-clear"
              onClick={() => setSearch("")}
              title={t("common.clearSearch")}
              aria-label={t("common.clearSearch")}
            >
              <X size={12} />
            </button>
          )}
          {!search && <kbd className="dt-search-kbd">/</kbd>}
        </div>
        </div>
        {activeToolMeta && (
          <p className="dt-toolbar-desc" title={t(activeToolMeta.descriptionKey)}>
            {t(activeToolMeta.descriptionKey)}
          </p>
        )}
      </header>

      <main className="dt-main">
        {/* key on activeTool re-triggers the entrance animation on switch */}
        <div className="dt-content" key={activeTool}>
          {renderTool()}
        </div>
      </main>
    </div>
  );
}

export default DevTools;
