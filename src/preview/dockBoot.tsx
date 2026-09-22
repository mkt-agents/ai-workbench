/**
 * TEMPORARY dock-layout repro — delete after use.
 */
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import ReactDOM from "react-dom/client";
import "../i18n/config";
import "../styles.css";
import "../components/TestManager.css";
import TestManager from "../components/TestManager";
import { ConfirmDialogProvider } from "../components/ConfirmModal";
import { startRun } from "../core/testRuns";
import type { TestProject } from "../core/types";

const now = () => new Date().toISOString();
const projects: TestProject[] = [
  { id: "tp-rs", name: "src-tauri", path: "D:/ai_project/ai-workbench/src-tauri", type: "rust", framework: "cargo", testCommand: "cargo test", enabled: true, createdAt: now(), updatedAt: now() },
];

const LINES = [
  "|",
  " = note: `#[warn(deprecated)]` on by default",
  "   Compiling ai_workbench v0.1.7 (D:\\ai_project\\ai-workbench\\src-tauri)",
  "warning: `ai_workbench` (lib) generated 1 warning",
  "   Building [================>          ] 460/461: ai_work…warning: function `mk_dir` is never used",
  "    --> src\\git_commands.rs:1678:8",
  "     |",
  "1678 |       fn mk_dir(p: &Path) {",
  "     |           ^^^^^^ help: if this is intentional, prefix it with an underscore: `_mk_dir` — this line is intentionally very long so it must scroll inside the pre and never escape the dock box",
];

mockWindows("main");
mockIPC(() => {
  throw new Error("placeholder");
}, { shouldMockEvents: true });
mockIPC((cmd: string, payload?: unknown) => {
  const a = (payload || {}) as Record<string, unknown>;
  switch (cmd) {
    case "load_test_projects": return projects;
    case "get_test_history": return [];
    case "cancel_test_run": return null;
    case "run_test":
      return {
        projectId: a.projectId, id: "run-1", startedAt: now(), completedAt: now(),
        durationMs: 1500, status: "success", totalTests: 3, passed: 3, failed: 0, skipped: 0,
        output: LINES.join("\n"), suites: [], errorKind: "",
      };
    case "db_load": return [];
    case "db_save": return null;
    default: throw new Error(`mock: ${cmd}`);
  }
}, { shouldMockEvents: true });

async function boot() {
  const { runId } = startRun({ projectId: "tp-rs", projectName: "src-tauri" });
  for (const line of LINES) {
    await new Promise((r) => setTimeout(r, 150));
    await emit("test-run-output", { projectId: "tp-rs", runId, text: line, done: false });
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <ConfirmDialogProvider>
    <TestManager />
  </ConfirmDialogProvider>
);
setTimeout(() => void boot(), 400);
