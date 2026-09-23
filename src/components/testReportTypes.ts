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

/** The backend prefixes cancellable errors so the UI can tell a cancel from a failure. */
export function isCancelledError(error: unknown): boolean {
  return String(error).includes("[E_CANCELLED]");
}

/** Strip the internal `[E_*]` marker before showing an error to the user. */
export function cleanErrorMessage(error: unknown): string {
  return String(error).replace(/^\[E_[A-Z0-9_]+\]\s*/, "").trim() || String(error);
}
