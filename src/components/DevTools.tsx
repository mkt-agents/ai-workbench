import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Braces,
  CalendarClock,
  Clock,
  Code2,
  Globe,
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
import HttpClientTool from "./devtools/HttpClientTool";

const STORAGE_KEY = "workbench-devtools-tool";

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
    groupKey: "group.network",
    tools: [
      { id: "http", labelKey: "http.title", icon: <Globe size={16} />, descriptionKey: "http.description" },
    ],
  },
];

/** Global 1-based index of a tool across all groups; used for the 1-9 hotkeys. */
function toolHotkeyIndex(allTools: ToolDef[], id: string): number {
  return allTools.findIndex((t) => t.id === id) + 1;
}

function DevTools({ active = true }: { active?: boolean }) {
  const { t } = useTranslation("devtools");

  const allTools = useMemo(() => TOOL_GROUPS.flatMap((g) => g.tools), []);

  const [activeTool, setActiveTool] = useState<string>(() => {
    const stored = localStorage.getItem(STORAGE_KEY);
    return stored && allTools.some((tool) => tool.id === stored) ? stored : "json";
  });

  const [search, setSearch] = useState("");

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, activeTool);
  }, [activeTool]);

  const stripRef = useRef<HTMLElement>(null);

  // The strip scrolls horizontally on narrow windows — keep the active chip in view.
  useEffect(() => {
    const chip = stripRef.current?.querySelector<HTMLElement>(`[data-tool-id="${activeTool}"]`);
    chip?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [activeTool]);

  // Keyboard shortcut: 1-9 maps to tools in global order
  useEffect(() => {
    if (!active) return;
    const handler = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      const num = parseInt(e.key, 10);
      if (num >= 1 && num <= 9 && num <= allTools.length) {
        e.preventDefault();
        setActiveTool(allTools[num - 1].id);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [active, allTools]);

  // Filter tools based on search
  const filteredGroups = useMemo(() => {
    if (!search.trim()) return TOOL_GROUPS;
    const searchLower = search.toLowerCase();
    return TOOL_GROUPS.map((group) => ({
      ...group,
      tools: group.tools.filter(
        (tool) =>
          t(tool.labelKey).toLowerCase().includes(searchLower) ||
          t(tool.descriptionKey).toLowerCase().includes(searchLower)
      ),
    })).filter((group) => group.tools.length > 0);
  }, [search, t]);

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
      case "http":         return <HttpClientTool />;
      default:             return <JsonTool />;
    }
  };

  /** Chip in the top tool strip — group name is carried by the tooltip. */
  const renderToolButton = (tool: ToolDef, groupKey: string) => {
    const hotkey = toolHotkeyIndex(allTools, tool.id);
    const isActive = activeTool === tool.id;
    return (
      <button
        key={tool.id}
        type="button"
        data-tool-id={tool.id}
        className={`dt-tool-item ${isActive ? "active" : ""}`}
        onClick={() => setActiveTool(tool.id)}
        title={`${t(groupKey)} · ${t(tool.descriptionKey)}`}
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

  const noResults = search.trim() !== "" && filteredGroups.length === 0;

  return (
    <div className="dt-layout">
      <header className="dt-toolbar">
        <nav ref={stripRef} className="dt-toolstrip" role="navigation" aria-label={t("title")}>
          {filteredGroups.map((group, i) => (
            <Fragment key={group.groupKey}>
              {i > 0 && <span className="dt-strip-divider" aria-hidden="true" />}
              <div className="dt-strip-group">{group.tools.map((tool) => renderToolButton(tool, group.groupKey))}</div>
            </Fragment>
          ))}

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

        {activeToolMeta && (
          <span className="dt-toolbar-desc" title={t(activeToolMeta.descriptionKey)}>
            {t(activeToolMeta.descriptionKey)}
          </span>
        )}

        <div className="dt-toolbar-search">
          <Search size={13} />
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setSearch("")}
            placeholder={t("common.search")}
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
        </div>
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
