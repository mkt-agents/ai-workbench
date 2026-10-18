import { useState } from "react";
import { useTranslation } from "react-i18next";
import { readStoredString, writeStoredString } from "../../core/localState";
import CalculatorTool from "../devtools/CalculatorTool";
import CronTool from "../devtools/CronTool";
import EncoderTool from "../devtools/EncoderTool";
import HashTool from "../devtools/HashTool";
import JsonTool from "../devtools/JsonTool";
import JwtTool from "../devtools/JwtTool";
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
] as const;

type EmbeddedId = (typeof EMBEDDED)[number]["id"];

const TOOL_KEY = "ai-workbench.aiHub.tool";

function loadTool(): EmbeddedId {
  const stored = readStoredString(TOOL_KEY);
  return EMBEDDED.some((e) => e.id === stored) ? (stored as EmbeddedId) : "timestamp";
}

export default function ToolsPanel() {
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
      <div className="qa-tools-body">
        <Active />
      </div>
    </div>
  );
}
