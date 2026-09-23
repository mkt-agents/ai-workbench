/**
 * Frontend types for the repo-card regression report. Field names mirror the
 * Rust `#[serde(rename_all = "camelCase")]` structs in change_report.rs and
 * report_commands.rs — keep them in lockstep when either side changes.
 */

export interface FileChange {
  path: string;
  oldPath?: string | null;
  status: string;
  adds: number;
  dels: number;
  module: string;
  layer: string;
  risks: string[];
  testPath?: string | null;
  hasTest: boolean;
  untracked: boolean;
  commits: string[];
  hints: string[];
}

export interface CommitBrief {
  sha: string;
  shortSha: string;
  author: string;
  date: string;
  subject: string;
  body: string;
}

export interface ModuleGroup {
  name: string;
  files: number;
  adds: number;
  dels: number;
  layers: string[];
  risks: string[];
  untested: string[];
}

export interface ApiChange {
  kind: string;
  name: string;
  path: string;
}

export interface ChangeStats {
  files: number;
  adds: number;
  dels: number;
  modules: number;
  codeFiles: number;
  untested: number;
  testFiles: number;
  ignored: number;
}

export interface ChangeReport {
  base: string;
  source: string;
  commits: string[];
  commitDetails: CommitBrief[];
  files: FileChange[];
  groups: ModuleGroup[];
  stats: ChangeStats;
  apiChanges: ApiChange[];
  scope: string[];
  truncated: boolean;
}

export interface TestReportBundle {
  report: ChangeReport;
  reportId: string;
  branch: string;
  head: string;
  repoRoot: string;
  repoName: string;
  subdir: boolean;
}

export interface AiResult {
  markdown: string;
  warnings: string[];
}

/** One repo's rollup line inside a folder (multi-repo) report. */
export interface FolderRepoRow {
  name: string;
  files: number;
  adds: number;
  dels: number;
  untested: number;
  error?: string | null;
}

/** The merged report over every repo under one folder, plus its per-repo rollup. */
export interface FolderReportBundle {
  report: ChangeReport;
  reportId: string;
  folderName: string;
  repos: FolderRepoRow[];
}

/** What the report panel is pointed at: one repo, or a whole folder merged. */
export type ReportTarget =
  | { kind: "repo"; path: string }
  | { kind: "folder"; name: string; paths: string[] };

/** The backend prefixes cancellable errors so the UI can tell a cancel from a failure. */
export function isCancelledError(error: unknown): boolean {
  return String(error).includes("[E_CANCELLED]");
}

/** Strip the internal `[E_*]` marker before showing an error to the user. */
export function cleanErrorMessage(error: unknown): string {
  return String(error).replace(/^\[E_[A-Z0-9_]+\]\s*/, "").trim() || String(error);
}

/**
 * One saved AI answer. The report itself is never persisted — it is recomputed
 * from the working tree on every selection, so a stored copy would only go stale.
 * The AI answer is the part worth keeping: it costs a model round trip and it is
 * what a tester actually works from. See `src-tauri/src/report_history.rs`.
 */
export interface ReportAiHistoryEntry {
  id: number;
  /** `repo` | `folder`. */
  targetKind: string;
  targetLabel: string;
  /** How the change set was described, e.g. `HEAD~5` or `2026-09-01 ~ 2026-09-23`. */
  baseline: string;
  model: string;
  /** The requirement box as it read when this answer was generated. */
  requirement: string;
  markdown: string;
  /** Unix milliseconds. */
  createdAt: number;
}

/** What the panel hands over when a generation succeeds. */
export type ReportAiHistoryInput = Omit<ReportAiHistoryEntry, "id" | "createdAt">;
