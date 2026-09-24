import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeftRight,
  Braces,
  CalendarClock,
  Clock,
  Code2,
  FileSearch,
  Globe,
  Hash,
  History,
  Lock,
  Network,
  Regex,
  Search,
  Shuffle,
  Sparkles,
  Wrench,
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
import PortProcessTool from "./devtools/PortProcessTool";
import HttpClientTool from "./devtools/HttpClientTool";

const STORAGE_KEY = "workbench-devtools-tool";
const RECENT_KEY = "workbench-devtools-recent";

type ToolDef = {
  id: string;
  labelKey: string;
  icon: React.ReactNode;
  descriptionKey: string;
};

type ToolGroup = {
  groupKey: string;
  icon: React.ReactNode;
  tools: ToolDef[];
};

const TOOL_GROUPS: ToolGroup[] = [
  {
    groupKey: "group.convert",
    icon: <ArrowLeftRight size={10} />,
    tools: [
      { id: "json", labelKey: "json.title", icon: <Braces size={16} />, descriptionKey: "json.description" },
      { id: "encoder", labelKey: "encoder.title", icon: <Code2 size={16} />, descriptionKey: "encoder.description" },
      { id: "timestamp", labelKey: "timestamp.title", icon: <Clock size={16} />, descriptionKey: "timestamp.description" },
      { id: "cron", labelKey: "cron.title", icon: <CalendarClock size={16} />, descriptionKey: "cron.description" },
    ],
  },
  {
    groupKey: "group.generate",
    icon: <Sparkles size={10} />,
    tools: [
      { id: "uuid", labelKey: "uuid.title", icon: <Shuffle size={16} />, descriptionKey: "uuid.description" },
      { id: "hash", labelKey: "hash.title", icon: <Hash size={16} />, descriptionKey: "hash.description" },
    ],
  },
  {
    groupKey: "group.parse",
    icon: <FileSearch size={10} />,
    tools: [
      { id: "jwt", labelKey: "jwt.title", icon: <Lock size={16} />, descriptionKey: "jwt.description" },
      { id: "regex", labelKey: "regex.title", icon: <Regex size={16} />, descriptionKey: "regex.description" },
    ],
  },
  {
    groupKey: "group.network",
    icon: <Network size={10} />,
    tools: [
      { id: "ports", labelKey: "ports.title", icon: <Network size={16} />, descriptionKey: "ports.description" },
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
  const [recent, setRecent] = useState<string[]>(() => {
    try {
      const stored = localStorage.getItem(RECENT_KEY);
      return stored ? JSON.parse(stored) : [];
    } catch {
      return [];
    }
  });

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, activeTool);
    setRecent((prev) => {
      const filtered = prev.filter((id) => id !== activeTool);
      return [activeTool, ...filtered].slice(0, 3);
    });
  }, [activeTool]);

  useEffect(() => {
    localStorage.setItem(RECENT_KEY, JSON.stringify(recent));
  }, [recent]);

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

  const recentTools = useMemo(() => {
    return recent
      .map((id) => allTools.find((tool) => tool.id === id))
      .filter(Boolean) as typeof allTools;
  }, [recent, allTools]);

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
      case "ports":        return <PortProcessTool />;
      case "http":         return <HttpClientTool />;
      default:             return <JsonTool />;
    }
  };

  /** Tool list button — shared by recent + grouped lists. */
  const renderToolButton = (tool: ToolDef) => {
    const hotkey = toolHotkeyIndex(allTools, tool.id);
    const isActive = activeTool === tool.id;
    return (
      <button
        key={tool.id}
        type="button"
        className={`dt-tool-item ${isActive ? "active" : ""}`}
        onClick={() => setActiveTool(tool.id)}
        title={t(tool.descriptionKey)}
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

  return (
    <div className="dt-layout">
      <aside className="dt-sidebar" role="navigation" aria-label={t("title")}>
        {/* Sidebar header: title + search */}
        <div className="dt-sidebar-header">
          <div className="dt-sidebar-title">
            <span className="dt-sidebar-title-icon">
              <Wrench size={13} />
            </span>
            <span>{t("title")}</span>
          </div>
          <div className="dt-sidebar-search">
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
        </div>

        <nav className="dt-toollist">
          {/* Recent tools (hide when searching) */}
          {search.trim() === "" && recentTools.length > 0 && (
            <div className="dt-toolgroup">
              <div className="dt-toolgroup-label">
                <History size={10} />
                <span>{t("common.recent")}</span>
              </div>
              {recentTools.map((tool) => renderToolButton(tool))}
            </div>
          )}

          {/* Tool groups */}
          {filteredGroups.map((group) => (
            <div key={group.groupKey} className="dt-toolgroup">
              <div className="dt-toolgroup-label">
                {group.icon}
                <span>{t(group.groupKey)}</span>
              </div>
              {group.tools.map((tool) => renderToolButton(tool))}
            </div>
          ))}

          {/* Empty state when search has no results */}
          {search.trim() !== "" && filteredGroups.length === 0 && (
            <div className="dt-empty-state">
              <Search size={18} />
              <span>{t("common.noResults")}</span>
              <button type="button" className="dt-empty-clear" onClick={() => setSearch("")}>
                {t("common.clearSearch")}
              </button>
            </div>
          )}
        </nav>
      </aside>

      <main className="dt-main">
        {/* Active tool header — icon + label + description + hotkey hint */}
        <div className="dt-header">
          {activeToolMeta && (
            <>
              <span className="dt-header-icon">{activeToolMeta.icon}</span>
              <div className="dt-header-text">
                <span className="dt-header-label">{t(activeToolMeta.labelKey)}</span>
                <span className="dt-header-desc">{t(activeToolMeta.descriptionKey)}</span>
              </div>
            </>
          )}
          <span className="dt-header-hint" title={t("common.shortcutHint")}>
            {t("common.shortcutHint")}
          </span>
        </div>

        {/* key on activeTool re-triggers the entrance animation on switch */}
        <div className="dt-content" key={activeTool}>
          {renderTool()}
        </div>
      </main>
    </div>
  );
}

export default DevTools;
