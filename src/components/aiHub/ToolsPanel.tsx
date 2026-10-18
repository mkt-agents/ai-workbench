import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ExternalLink } from "lucide-react";
import { readStoredString, writeStoredString } from "../../core/localState";
import CalculatorTool from "../devtools/CalculatorTool";
import EncoderTool from "../devtools/EncoderTool";
import HashTool from "../devtools/HashTool";
import JwtTool from "../devtools/JwtTool";
import TimestampTool from "../devtools/TimestampTool";
import UuidTool from "../devtools/UuidTool";

const EMBEDDED = [
  { id: "timestamp", label: "timestamp.title", C: TimestampTool },
  { id: "uuid", label: "uuid.title", C: UuidTool },
  { id: "encoder", label: "encoder.title", C: EncoderTool },
  { id: "hash", label: "hash.title", C: HashTool },
  { id: "jwt", label: "jwt.title", C: JwtTool },
  { id: "calc", label: "calc.title", C: CalculatorTool },
] as const;

/** Editor-class tools need the wide main-window layout — jump instead (carrying the tool id). */
const JUMP_ONLY = [
  { id: "json", label: "json.title" },
  { id: "cron", label: "cron.title" },
  { id: "regex", label: "regex.title" },
] as const;

type EmbeddedId = (typeof EMBEDDED)[number]["id"];

const TOOL_KEY = "ai-workbench.aiHub.tool";

function loadTool(): EmbeddedId {
  const stored = readStoredString(TOOL_KEY);
  return EMBEDDED.some((e) => e.id === stored) ? (stored as EmbeddedId) : "timestamp";
}

export default function ToolsPanel({ onJump }: { onJump: (toolId: string) => void }) {
  const { t: td } = useTranslation("devtools");
  // Survives mode switches — the panel unmounts every time the user leaves tools.
  const [tool, setTool] = useState<EmbeddedId>(loadTool);
  const pickTool = (id: EmbeddedId) => {
    setTool(id);
    writeStoredString(TOOL_KEY, id);
  };
  const Active = EMBEDDED.find((e) => e.id === tool)!.C;
  return (
    <div className="ai-hub-tools">
      <div className="qa-tools-bar">
        <div className="qa-tools-row">
          {EMBEDDED.map((e) => (
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
        <div className="qa-tools-row">
          {JUMP_ONLY.map((tool) => (
            <button
              key={tool.id}
              type="button"
              className="qa-opt-chip"
              onClick={() => onJump(tool.id)}
            >
              <ExternalLink size={11} /> {td(tool.label)}
            </button>
          ))}
        </div>
      </div>
      <div className="qa-tools-body">
        <Active />
      </div>
    </div>
  );
}
