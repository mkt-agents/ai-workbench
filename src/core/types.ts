export interface GitAccount {
  id: string;
  name: string;
  email: string;
  color: string;
  /** Optional label e.g. work / personal */
  note?: string;
  createdAt: string;
  updatedAt: string;
}

export interface GitRepoConfig {
  path: string;
  name: string;
  userName: string;
  email: string;
  accountId?: string;
  createdAt: string;
  updatedAt: string;
}

/** Maps a git host (domain) to an account — auto-applies when a repo's origin matches. */
export interface GitHostConfig {
  id: string;
  host: string;
  accountId: string;
  note?: string;
  createdAt: string;
  updatedAt: string;
}

export interface RecentProject {
  id: number;
  path: string;
  name?: string;
  lastOpenedAt: string;
}

/** Parent folder that groups multiple git repos (e.g. D:\\d_project\\tct\\p3) */
export interface GitWorkspace {
  id: number;
  path: string;
  name: string;
  createdAt: string;
}

export interface GitScannedRepo {
  path: string;
  name: string;
}

export type AppTheme = 'light' | 'dark' | 'system' | 'glass' | 'ice' | 'silver';

export interface QuickAskChips {
  workspace: boolean;
  git: boolean;
  dirty: boolean;
  clipboard: boolean;
}

export interface AppSettings {
  theme: AppTheme;
  language: 'zh-CN' | 'en-US';
  sidebarCollapsed: boolean;
  autoStart: boolean;
  /** Last selected git repo path for workbench/commit */
  currentGitRepo?: string;
  /** Global shortcut for quick-ask (e.g. Ctrl+Alt+K) */
  quickAskShortcut?: string;
  /** When opening quick-ask, offer clipboard fill */
  quickAskPasteClipboard?: boolean;
  /** Persisted context chip toggles for quick-ask */
  quickAskChips?: QuickAskChips;
  /** Repos given full-diff context in quick-ask (empty = auto = current repo) */
  quickAskFocusRepos?: string[];
  /** Last used quick-ask task (encoded, see quickAsk/config encodeTask) */
  quickAskLastTask?: string;
  /** Show desktop floating bubble for quick-ask */
  quickAskBubbleEnabled?: boolean;
  /** Physical screen position of the bubble */
  quickAskBubblePos?: { x: number; y: number };
  /** One-shot tip after first close-to-tray */
  trayHintShown?: boolean;
}

export interface Snippet {
  id: string;
  name: string;
  content: string;
  tags: string;
  /** JSON array of param names or comma-separated */
  params: string;
  useCount: number;
  createdAt: string;
  updatedAt: string;
}

/** One Q&A exchange inside a quick-ask session. */
export interface QuickAskTurn {
  role: 'user' | 'assistant';
  content: string;
}

/** A persisted quick-ask conversation (multi-turn). */
export interface QuickAskSession {
  id: string;
  /** Short label derived from the first question. */
  title: string;
  /** TaskKind snapshot ('none' | 'debug' | 'explain' | 'polish'). */
  task: string;
  turns: QuickAskTurn[];
  createdAt: string;
  updatedAt: string;
}

/** A persisted JSON tool history record (input/output snapshot). */
export interface JsonToolHistoryItem {
  id: number;
  timestamp: number;
  input: string;
  output: string;
  path: string;
  ok: boolean;
  nodes: number;
  chars: number;
}

export interface GitRepoSummary {
  path: string;
  name: string;
  branch: string;
  dirtyCount: number;
  ahead: number;
  behind: number;
  hasUpstream: boolean;
  userName: string;
  userEmail: string;
  isGit: boolean;
  error?: string | null;
}

/**
 * One repo from the batched `git_summarize_repos` call: everything the repo
 * cards and the changelist need, gathered with two git subprocesses.
 * `status` is only filled when the caller asked for it (see gitCache modes).
 */
export interface RepoBatchItem extends GitRepoSummary {
  hasCommits: boolean;
  originUrl: string;
  status: GitStatusEntry[];
}

export interface GitStatusEntry {
  path: string;
  indexStatus: string;
  workTreeStatus: string;
  group: 'staged' | 'unstaged' | 'untracked' | string;
}

export interface GlobalState {
  settings: AppSettings;
  git: {
    accounts: GitAccount[];
    repoConfigs: GitRepoConfig[];
    hostConfigs: GitHostConfig[];
  };
  recentProjects: RecentProject[];
  gitWorkspaces: GitWorkspace[];
  webPlugins: WebPlugin[];
  userScripts: UserScript[];
  hostProfiles: HostProfile[];
  cursorAccounts: CursorAccount[];
  aiModels: AIModelConfig[];
  cloudflaredProfiles: CloudflaredNamedProfile[];
  snippets: Snippet[];
  quickAskSessions: QuickAskSession[];
  testProjects: TestProject[];
}

export interface HostProfile {
  id: string;
  name: string;
  content: string;
  createdAt: string;
  updatedAt: string;
}

export interface WebPlugin {
  id: string;
  name: string;
  url: string;
  addedAt: string;
  group: string;
  tags: string;
  order: number;
  lastOpenedAt: string;
  openCount: number;
  hotkey: string;
  isPreset: boolean;
}

export interface UserScript {
  id: string;
  name: string;
  description: string;
  matchPatterns: string[];  // 支持多个匹配模式
  code: string;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
  /** 从网址导入时记录的原始链接，本地手写的脚本为空 */
  sourceUrl?: string;
}

export interface CursorAccount {
  id: string;
  name: string;
  email: string;
  color: string;
  backupPath: string;
  profileDir?: string;
  profileInitialized?: boolean;
  gitUserName?: string;
  gitEmail?: string;
  /** Local memo only — not used for auto-login */
  password?: string;
  notes?: string;
  isLoggedIn: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface CloudflaredNamedProfile {
  id: string;
  name: string;
  hostname: string;
  localUrl: string;
  /** token | config — how to authenticate the named tunnel */
  authMode: "token" | "config";
  /** Required when authMode is token */
  token?: string;
  /** Required when authMode is config — path to config.yml */
  configPath?: string;
  createdAt: string;
  updatedAt: string;
}

export type AuthType = 'api' | 'token_plan';

export interface AIModelConfig {
  id: string;
  name: string;
  provider:
    | 'openai'
    | 'anthropic'
    | 'deepseek'
    | 'ollama'
    | 'longcat'
    | 'agnes'
    | 'openrouter'
    | 'google'
    | 'groq'
    | 'mistral'
    | 'xai'
    | 'moonshot'
    | 'mimo'
    | 'zhipu'
    | 'qwen'
    | 'siliconflow'
    | 'together'
    | 'custom';
  apiKey: string;
  authType: AuthType;
  baseUrl: string;
  model: string;
  temperature: number;
  maxTokens: number;
  isDefault: boolean;
  createdAt: string;
  updatedAt: string;
  /** Last connection test, persisted so the verdict survives a restart. */
  lastTest?: AIModelTestResult | null;
}

export interface AIModelTestResult {
  ok: boolean;
  /** ISO timestamp of the test. */
  at: string;
  /** Backend message: empty when it passed, otherwise the failure reason. */
  message: string;
}

export interface DshInstance {
  pid: number;
  port: number;
  auth_url?: string;
  auth_patch_warning?: string;
}

export type RuntimeKind = "node" | "jdk";

export interface RuntimeVersion {
  kind: string;
  version: string;
  path: string;
  binPath: string;
  source: string;
  active: boolean;
  custom: boolean;
  onMachinePath?: boolean;
}

export interface RuntimeSwitchPlan {
  needsElevation: boolean;
  reason?: string | null;
}

export interface RuntimeSwitchResult {
  message: string;
  version: string;
  verified: boolean;
  verifiedVersion?: string | null;
  elevated: boolean;
}

/** A runtime release that can be downloaded and installed by the app. */
export interface InstallableVersion {
  version: string;
  lts: boolean;
  installed: boolean;
}

export interface DevtoolsPortEntry {
  proto: string;
  local_addr: string;
  local_port: number;
  remote_addr: string;
  remote_port: number;
  state: string;
  pid: number;
}

export interface DevtoolsProcessInfo {
  pid: number;
  name: string;
  memory: string;
  path: string;
  services: string;
}

export interface DevtoolsHeaderPair {
  key: string;
  value: string;
}

export interface DevtoolsHttpRequest {
  method: string;
  url: string;
  headers?: DevtoolsHeaderPair[];
  body?: string;
  timeout_sec?: number;
}

export interface DevtoolsHttpResponse {
  status: number;
  duration_ms: number;
  headers: DevtoolsHeaderPair[];
  body: string;
}

export interface CursorUpdateState {
  installDir: string;
  exePresent: boolean;
  stagingPresent: boolean;
  backupBytes: number;
  files: string[];
  stage: string;
  attempt: number;
  /** New build already in place, but the "update in progress" markers survived. */
  stuck: boolean;
  running: boolean;
  message: string;
}

export interface CursorCleanupResult {
  removedFiles: number;
  freedBytes: number;
  message: string;
}

// Test Automation Types
/** Mirrors the `status` strings test_commands.rs writes. */
export type TestRunOutcome = 'success' | 'failed' | 'error' | 'cancelled' | 'timeout';

export interface TestProject {
  id: string;
  name: string;
  path: string;
  type: 'frontend' | 'backend' | 'rust' | 'python' | 'go' | 'java' | 'csharp' | 'custom';
  framework: string;
  testCommand: string;
  args?: string;
  workingDir?: string;
  env?: Record<string, string>;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
  lastRunAt?: string;
  lastStatus?: TestRunOutcome;
  /** Why the last run failed; see `errorKind.*` in the `test` i18n namespace. */
  lastErrorKind?: string;
}

/** One project found by the recursive directory scan. */
export interface ScannedProject {
  path: string;
  name: string;
  projectType: string;
  framework: string;
  testCommand: string;
  reason: string;
  /** Where the command must run; `..` for a Maven module of a larger reactor. */
  workingDir: string;
  reactorRoot: string;
  reactorModule: string;
  artifactId: string;
  notes: string[];
}

export interface TestCase {
  id: string;
  name: string;
  status: 'passed' | 'failed' | 'skipped';
  duration: number;
  error?: TestCaseError;
}

export interface TestCaseError {
  message: string;
  stack: string;
  expected?: unknown;
  actual?: unknown;
}

export interface TestSuite {
  name: string;
  path: string;
  status: 'passed' | 'failed';
  duration: number;
  tests: TestCase[];
}

export interface TestRunResult {
  projectId: string;
  id: string;
  startedAt: string;
  completedAt: string;
  durationMs: number;
  status: TestRunOutcome;
  totalTests: number;
  passed: number;
  failed: number;
  skipped: number;
  output: string;
  suites: TestSuite[];
  /** `""` when the run did not fail in a classifiable way. */
  errorKind: string;
}

export interface TestHistoryEntry {
  id: number;
  projectId: string;
  runId?: string;
  timestamp: string;
  status: TestRunOutcome;
  total?: number;
  passed?: number;
  failed?: number;
  errorKind?: string;
}

export interface ProjectDetectionResult {
  path: string;
  detected: boolean;
  projectType?: string;
  framework?: string;
  testCommand?: string;
  reason: string;
}

// AI Test Generation Types
export interface TestGenOptions {
  coverageLevel: 'comprehensive' | 'basic' | 'boundary' | 'exception';
  mockStrategy: 'auto' | 'manual' | 'skip';
  assertStyle: 'expect' | 'assert' | 'should';
}

/** Sections the diagnosis prompt asks for; filled in by parseDiagnosis(). */
export interface FailureDiagnosis {
  rootCause: string;
  expectedBehavior: string;
  actualBehavior: string;
  fixSuggestion: string;
  fixCode?: string;
}

// Coverage Report Types
export interface CoverageMetric {
  total: number;
  covered: number;
  percentage: number;
}

export interface CoverageFile {
  path: string;
  lines: CoverageMetric;
  statements: CoverageMetric;
  branches: CoverageMetric;
  functions: CoverageMetric;
}

export interface CoverageReport {
  lines: CoverageMetric;
  statements: CoverageMetric;
  branches: CoverageMetric;
  functions: CoverageMetric;
  files: CoverageFile[];
}

/** One changed file in a regression report, as classified by the backend. */
export interface ChangeFile {
  path: string;
  oldPath?: string | null;
  status: string;
  adds: number;
  dels: number;
  module: string;
  layer: string;
  risks: string[];
  /** Weak (substring-only) risk matches, shown grey and never counted. */
  hints?: string[];
  testPath?: string | null;
  hasTest: boolean;
  untracked: boolean;
  /** Short shas of the commits in range that touched this file. */
  commits?: string[];
}

/** One commit in the analysed range, with enough detail to attribute a file. */
export interface CommitBrief {
  sha: string;
  shortSha: string;
  author: string;
  date: string;
  subject: string;
  body: string;
}

export interface ChangeModuleGroup {
  name: string;
  files: number;
  adds: number;
  dels: number;
  layers: string[];
  risks: string[];
  untested: string[];
}

export interface ApiChange {
  kind: 'added' | 'removed' | 'changed' | string;
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
  /** Full commit briefs when the range had commits (absent on old reports). */
  commitDetails?: CommitBrief[];
  files: ChangeFile[];
  groups: ChangeModuleGroup[];
  stats: ChangeStats;
  apiChanges: ApiChange[];
  scope: string[];
  truncated: boolean;
}

/** Incremental (diff-line) coverage: what the change touched vs what ran. */
export interface DeltaFile {
  path: string;
  changedLines: number;
  covered: number;
  missed: number;
  /** Changed lines the coverage artifact says nothing about — never counted missed. */
  unknown: number;
  ratio: number;
}

export interface CoverageHotspot {
  path: string;
  lines: number[];
}

export interface DeltaCoverage {
  files: DeltaFile[];
  changedLines: number;
  covered: number;
  missed: number;
  unknown: number;
  ratio: number;
  uncoveredHotspots: CoverageHotspot[];
}

export interface ChangeReportBundle {
  report: ChangeReport;
  branch: string;
  head: string;
  repoRoot: string;
  subdir: boolean;
  reportId: string | null;
}

export interface ChangeReportSummary {
  id: string;
  projectId: string;
  base: string;
  source: string;
  createdAt: string;
  branch: string;
  files: number;
  adds: number;
  dels: number;
  untested: number;
  hasAi: boolean;
}

export interface TestTarget {
  name: string;
  /** `class` (surefire/jest suite), `file` (a path) or `filter` (positional pattern). */
  kind: string;
  /** The changed source file this target was derived from. */
  from: string;
  /** `true` when the test file itself was edited, so it is its own target. */
  changed: boolean;
}

export interface TestSelection {
  framework: string;
  targets: TestTarget[];
  /** Changed code files with no test we can point at. */
  gaps: string[];
  /** Append to the project's test command (`run_test`'s `args`). */
  args: string;
  truncated: boolean;
  /** `false` when the framework has no filter syntax we trust: run everything. */
  selectable: boolean;
}

export interface Scenario {
  id: number;
  reportId: string;
  projectId: string;
  runId: string | null;
  title: string;
  detail: string | null;
  priority: string;
  /** `pending` | `passed` | `failed` | `blocked` */
  status: string;
  note: string | null;
  sort: number;
  source: string;
  createdAt: string;
  updatedAt: string;
}

export interface ScenarioSummary {
  total: number;
  passed: number;
  failed: number;
  blocked: number;
  pending: number;
  /** 0..=100 passed share, computed by the backend. */
  percent: number;
}

export interface ChangeRunLink {
  runId: string;
  projectId: string;
  createdAt: string;
  status: string;
  totalTests: number;
  passed: number;
  failed: number;
  errorKind: string;
}

export interface StoredChangeReport {
  summary: ChangeReportSummary;
  report: ChangeReport | null;
  ai: string | null;
  scenarios: Scenario[];
  scenarioSummary: ScenarioSummary;
  runs: ChangeRunLink[];
  /** Cached incremental coverage; `null` until first computed. */
  deltaCoverage: DeltaCoverage | null;
  /** Set once the tester accepted the report; `null` while in flight. */
  acceptedAt: string | null;
  /** Identifiers the AI mentioned that the static report cannot back up. */
  aiWarnings: string[];
}

/* ---------------------------------- vuln scan ---------------------------------- */

/** One OSV advisory as stored on a dependency finding. */
export interface OsvVuln {
  id: string;
  summary: string;
  severity: string;
  fixedVersions: string[];
  aliases: string[];
  package: string;
  version: string;
}

/** One secret hit; the raw value never reaches the frontend either. */
export interface SecretHit {
  rule: string;
  file: string;
  line: number;
  preview: string;
  digest: string;
}

/** What one `scan_project_vulns` call answered. */
export interface VulnScanOutcome {
  status: string;
  errorKind: string;
  error: string | null;
  /** e.g. maven: dependency audit unsupported — a note, not a failure. */
  unsupported: string | null;
  depsChecked: number;
  filesChecked: number;
  filesSkipped: number;
  critical: number;
  high: number;
  total: number;
  vulns: OsvVuln[];
  secrets: SecretHit[];
}

/** `open | fixed | ignored | false_positive` (gated in the Rust store). */
export interface VulnFinding {
  id: number;
  projectId: string;
  scanId: string;
  kind: string;
  dedupKey: string;
  ecosystem: string;
  package: string;
  version: string;
  vulnId: string;
  severity: string;
  summary: string;
  fixedVersions: string[];
  aliases: string[];
  file: string;
  line: number;
  preview: string;
  rule: string;
  status: string;
  firstSeen: string;
  lastSeen: string;
}

export interface VulnTotals {
  total: number;
  open: number;
  critical: number;
  high: number;
  medium: number;
  low: number;
  unknown: number;
  secrets: number;
}

export interface VulnFindingsPage {
  findings: VulnFinding[];
  totals: VulnTotals;
  totalCount: number;
}

export interface VulnScanSummary {
  id: string;
  projectId: string;
  startedAt: string;
  completedAt: string | null;
  status: string;
  errorKind: string;
  depsChecked: number;
  filesChecked: number;
  findingsCritical: number;
  findingsHigh: number;
  findingsTotal: number;
}

/** Change-report link: open findings sitting on one package. */
export interface PackageExposure {
  package: string;
  count: number;
  critical: number;
}

export interface GlobalStore {
  getState: () => GlobalState;
  setState: (partial: Partial<GlobalState> | ((state: GlobalState) => GlobalState)) => void;
  subscribe: (listener: (state: GlobalState) => void) => () => void;
}
