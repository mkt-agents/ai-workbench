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

export type SnippetKind = 'text' | 'prompt';

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
  /** 'text' = reusable fragment; 'prompt' = Prompt Studio custom template */
  kind?: SnippetKind;
  /** Prompt Studio scenario key (PROMPT_SCENARIOS) when kind === 'prompt' */
  scenario?: string;
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
  quickAppLaunchers: QuickAppLauncher[];
}

/** A saved application shortcut for quick launch / kill. */
export interface QuickAppLauncher {
  id: string;
  name: string;
  /** Full path to the executable. */
  path: string;
  /** Optional command-line arguments. */
  args: string;
  /** Sort order (lower = first). */
  order: number;
  /** Optional group label for filtering. */
  group?: string;
  /** File version of the executable, auto-detected on add (e.g. "1.2.3"). */
  version?: string;
  createdAt: string;
  updatedAt: string;
}

/** An application discovered by scanning the system. */
export interface InstalledAppInfo {
  display_name: string;
  exe_path: string;
  publisher: string;
  version: string;
  icon?: string; // base64 PNG, optional
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

export interface GlobalStore {
  getState: () => GlobalState;
  setState: (partial: Partial<GlobalState> | ((state: GlobalState) => GlobalState)) => void;
  subscribe: (listener: (state: GlobalState) => void) => () => void;
}
