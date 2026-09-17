import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Braces,
  Code2,
  Clock,
  Globe,
  Hash,
  Lock,
  Network,
  Regex,
  Shuffle,
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

type ToolGroup = {
  groupKey: string;
  tools: {
    id: string;
    labelKey: string;
    icon: React.ReactNode;
    descriptionKey: string;
  }[];
};

function DevTools() {
  const { t } = useTranslation("devtools");

  const groups: ToolGroup[] = useMemo(
    () => [
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
    ],
    [],
  );

  const allTools = useMemo(() => groups.flatMap((g) => g.tools), [groups]);

  const [activeTool, setActiveTool] = useState<string>(() => {
    const stored = localStorage.getItem(STORAGE_KEY);
    return stored && allTools.some((tool) => tool.id === stored) ? stored : "json";
  });

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, activeTool);
  }, [activeTool]);

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
        <nav className="dt-toollist" aria-label={t("title")}>
          {groups.map((group) => (
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
