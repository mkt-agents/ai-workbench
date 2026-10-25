import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { readStoredString, writeStoredString } from "../../core/localState";
import { DEFAULT_TOOL_ORDER, DEVTOOLS_ORDER_KEY, loadToolOrder } from "../DevTools";
import CalculatorTool from "../devtools/CalculatorTool";
import CronTool from "../devtools/CronTool";
import EncoderTool from "../devtools/EncoderTool";
import HashTool from "../devtools/HashTool";
import JsonTool from "../devtools/JsonTool";
import JwtTool from "../devtools/JwtTool";
import PortKillerTool from "../devtools/PortKillerTool";
import RegexTool from "../devtools/RegexTool";
import TimestampTool from "../devtools/TimestampTool";
import UuidTool from "../devtools/UuidTool";

const EMBEDDED = [
  { id: "timestamp", label: "timestamp.title", C: TimestampTool },
  { id: "uuid", label: "uuid.title", C: UuidTool },
  { id: "encoder", label: "encoder.title", C: EncoderTool },
  { id: "hash", label: "hash.title", C: HashTool },
  { id: "jwt", label: "jwt.title", C: JwtTool },
  { id: "calc", label: "calc.title", C: CalculatorTool },
  { id: "json", label: "json.title", C: JsonTool },
  { id: "cron", label: "cron.title", C: CronTool },
  { id: "regex", label: "regex.title", C: RegexTool },
  { id: "ports", label: "ports.title", C: PortKillerTool },
] as const;

type EmbeddedId = (typeof EMBEDDED)[number]["id"];

const TOOL_KEY = "ai-workbench.aiHub.tool";

function loadTool(): EmbeddedId {
  const stored = readStoredString(TOOL_KEY);
  return EMBEDDED.some((e) => e.id === stored) ? (stored as EmbeddedId) : "timestamp";
}

export default function ToolsPanel() {
  const { t: td } = useTranslation("devtools");
  const [tool, setTool] = useState<EmbeddedId>(loadTool);
  /** 进过的工具挂起不卸，切走再回来输入还在。 */
  const [visited, setVisited] = useState<Set<EmbeddedId>>(() => new Set([loadTool()]));
  const pickTool = (id: EmbeddedId) => {
    setTool(id);
    setVisited((prev) => (prev.has(id) ? prev : new Set(prev).add(id)));
    writeStoredString(TOOL_KEY, id);
  };
  // Same order as the main window's toolbox (drag-sorted there); follows changes live.
  const [order, setOrder] = useState(() => loadToolOrder(DEFAULT_TOOL_ORDER));
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === DEVTOOLS_ORDER_KEY) setOrder(loadToolOrder(DEFAULT_TOOL_ORDER));
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);
  const tools = useMemo(() => {
    const rank = (id: string) => {
      const i = order.indexOf(id);
      return i < 0 ? order.length : i;
    };
    return [...EMBEDDED].sort((a, b) => rank(a.id) - rank(b.id));
  }, [order]);
  return (
    <div className="ai-hub-tools">
      <div className="qa-tools-bar">
        {tools.map((e) => (
          <button
            key={e.id}
            type="button"
            className={`qa-opt-chip${tool === e.id ? " active" : ""}`}
            onClick={() => pickTool(e.id)}
          >
            {td(e.label)}
          </button>
        ))}
      </div>
      <div className="qa-tools-body">
        {EMBEDDED.map((e) => {
          if (!visited.has(e.id)) return null;
          const Comp = e.C;
          return (
            <div
              key={e.id}
              className={`qa-tool-pane${tool === e.id ? " is-active" : ""}`}
              aria-hidden={tool !== e.id}
            >
              <Comp />
            </div>
          );
        })}
      </div>
    </div>
  );
}
