import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Braces,
  Clock,
  Code2,
  Globe,
  Hash,
  History,
  Lock,
  Network,
  Regex,
  Search,
  Shuffle,
  Wrench,
} from "lucide-react";
import JsonTool from "./devtools/JsonTool";
import EncoderTool from "./devtools/EncoderTool";
import TimestampTool from "./devtools/TimestampTool";
import UuidTool from "./devtools/UuidTool";
import JwtTool from "./devtools/JwtTool";
import RegexTool from "./devtools/RegexTool";
import HashTool from "./devtools/HashTool";
import PortProcessTool from "./devtools/PortProcessTool";
import HttpClientTool from "./devtools/HttpClientTool";

const STORAGE_KEY = "workbench-devtools-tool";
const RECENT_KEY = "workbench-devtools-recent";

type ToolGroup = {
  groupKey: string;
  tools: {
    id: string;
    labelKey: string;
    icon: React.ReactNode;
    descriptionKey: string;
  }[];
};

const TOOL_GROUPS: ToolGroup[] = [
  {
    groupKey: "group.convert",
    tools: [
      { id: "json", labelKey: "json.title", icon: <Braces size={16} />, descriptionKey: "json.description" },
      { id: "encoder", labelKey: "encoder.title", icon: <Code2 size={16} />, descriptionKey: "encoder.description" },
      { id: "timestamp", labelKey: "timestamp.title", icon: <Clock size={16} />, descriptionKey: "timestamp.description" },
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
      { id: "ports", labelKey: "ports.title", icon: <Network size={16} />, descriptionKey: "ports.description" },
      { id: "http", labelKey: "http.title", icon: <Globe size={16} />, descriptionKey: "http.description" },
    ],
  },
];

function DevTools() {
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
    // Update recent tools
    setRecent((prev) => {
      const filtered = prev.filter((id) => id !== activeTool);
      return [activeTool, ...filtered].slice(0, 5);
    });
  }, [activeTool]);

  useEffect(() => {
    localStorage.setItem(RECENT_KEY, JSON.stringify(recent));
  }, [recent]);

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

  const renderTool = () => {
    switch (activeTool) {
      case "json":
        return <JsonTool />;
      case "encoder":
        return <EncoderTool />;
      case "timestamp":
        return <TimestampTool />;
      case "uuid":
        return <UuidTool />;
      case "jwt":
        return <JwtTool />;
      case "regex":
        return <RegexTool />;
      case "hash":
        return <HashTool />;
      case "ports":
        return <PortProcessTool />;
      case "http":
        return <HttpClientTool />;
      default:
        return <JsonTool />;
    }
  };

  return (
    <div className="dt-layout">
      <aside className="dt-sidebar">
        <div className="dt-sidebar-header">
          <div className="dt-sidebar-title">
            <Wrench size={16} />
            <span>{t("title")}</span>
          </div>
          <div className="dt-sidebar-search">
            <Search size={14} />
            <input
              type="text"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={t("common.search")}
              spellCheck={false}
            />
          </div>
        </div>
        <nav className="dt-toollist" aria-label={t("title")}>
          {/* Recent tools */}
          {search.trim() === "" && recentTools.length > 0 && (
            <div className="dt-toolgroup">
              <div className="dt-toolgroup-label">
                <History size={10} />
                <span>{t("common.recent")}</span>
              </div>
              {recentTools.map((tool) => (
                <button
                  key={`recent-${tool.id}`}
                  type="button"
                  className={`dt-tool-item ${activeTool === tool.id ? "active" : ""}`}
                  onClick={() => setActiveTool(tool.id)}
                  title={t(tool.descriptionKey)}
                >
                  <span className="dt-tool-icon">{tool.icon}</span>
                  <span className="dt-tool-label">{t(tool.labelKey)}</span>
                </button>
              ))}
            </div>
          )}

          {/* Tool groups */}
          {filteredGroups.map((group) => (
            <div key={group.groupKey} className="dt-toolgroup">
              <div className="dt-toolgroup-label">{t(group.groupKey)}</div>
              {group.tools.map((tool) => (
                <button
                  key={tool.id}
                  type="button"
                  className={`dt-tool-item ${activeTool === tool.id ? "active" : ""}`}
                  onClick={() => setActiveTool(tool.id)}
                  title={t(tool.descriptionKey)}
                >
                  <span className="dt-tool-icon">{tool.icon}</span>
                  <span className="dt-tool-label">{t(tool.labelKey)}</span>
                </button>
              ))}
            </div>
          ))}
        </nav>
      </aside>

      <main className="dt-main">
        <div className="dt-content">{renderTool()}</div>
      </main>
    </div>
  );
}

export default DevTools;
