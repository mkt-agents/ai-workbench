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

/** Parent folder that groups multiple git repos (e.g. D:\\dev\\projects\\workspace) */
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

/** Result of `git_list_branches`: local heads, plus remote-tracking refs with
 *  the remote segment stripped (`origin/feature/x` → `feature/x`) so both
 *  lists key on the same plain branch name. No network. */
export interface GitBranchList {
  current: string;
  local: string[];
  remote: string[];
}

export type AppTheme = 'light' | 'dark' | 'system' | 'glass' | 'ice' | 'silver';

export interface AiHubChips {
  workspace: boolean;
  git: boolean;
  dirty: boolean;
}

/** AI call sites that can bind a preferred model (else fall back to default). */
export type AiScene = 'aiHub' | 'report' | 'commit' | 'optimize';

export type AiSceneModels = Partial<Record<AiScene, string>>;

export interface AppSettings {
  theme: AppTheme;
  language: 'zh-CN' | 'en-US';
  sidebarCollapsed: boolean;
  autoStart: boolean;
  /** Last selected git repo path for workbench/commit */
  currentGitRepo?: string;
  /** Global shortcut for ai-hub (e.g. Ctrl+Alt+K) */
  aiHubShortcut?: string;
  /** When opening ai-hub, offer clipboard fill */
  aiHubPasteClipboard?: boolean;
  /** Persisted context chip toggles for ai-hub */
  aiHubChips?: AiHubChips;
  /** Repos given full-diff context in ai-hub (empty = auto = current repo) */
  aiHubFocusRepos?: string[];
  /** Last used ai-hub task (encoded, see aiHub/config encodeTask) */
  aiHubLastTask?: string;
  /** Show desktop floating bubble for ai-hub */
  aiHubBubbleEnabled?: boolean;
  /** Physical screen position of the bubble */
  aiHubBubblePos?: { x: number; y: number };
  /** One-shot tip after first close-to-tray */
  trayHintShown?: boolean;
  /** Preferred model id per AI scene (missing → global default). */
  aiSceneModels?: AiSceneModels;
  /** User-defined AI Hub profiles (builtins are merged at runtime). */
  aiHubProfiles?: AiHubProfile[];
  /** Active AI Hub profile id (builtin:* or custom). */
  aiHubProfileId?: string;
}

/** Named AI Hub preset: role + optional temperature/model. */
export interface AiHubProfile {
  id: string;
  name: string;
  /** i18n key for built-in display names (aihub namespace). */
  nameKey?: string;
  /** Extra system guidance appended after the mode prompt. */
  systemRole?: string;
  temperature?: number;
  modelId?: string;
  builtin?: boolean;
}

export type SnippetKind = 'text' | 'prompt';

/** AI Hub one-shot tasks a snippet can auto-bind to (not `none` / `optimize`). */
export type SnippetTaskBind =
  | 'debug'
  | 'explain'
  | 'polish'
  | 'translate'
  | 'summarize';

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
  /** When set, picking this AI Hub task inserts the snippet if the composer is empty. */
  taskBind?: SnippetTaskBind | '';
}

/** Mid-task scratch note (prompts / scripts); promote to Snippet when lasting. */
export interface Draft {
  id: string;
  /** Optional; list falls back to first line of content when empty. */
  title: string;
  content: string;
  pinned: boolean;
  createdAt: string;
  updatedAt: string;
}

/** One Q&A exchange inside a ai-hub session. */
export interface AiHubTurn {
  role: 'user' | 'assistant';
  content: string;
  /** Copy and save keep the whole reply (polish, translate, summary, daily report). */
  keep?: 'whole';
  /** Tool steps from an agent turn. Not sent as the visible answer. */
  steps?: { tool: string; status: string; summary: string }[];
  /** Folded notes for rounds that fell outside the verbatim window. */
  digest?: boolean;
}

/** A persisted ai-hub conversation (multi-turn). */
export interface AiHubSession {
  id: string;
  /** Short label derived from the first question. */
  title: string;
  /** TaskKind snapshot ('none' | 'debug' | 'explain' | 'polish'). */
  task: string;
  turns: AiHubTurn[];
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
  drafts: Draft[];
  aiHubSessions: AiHubSession[];
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

/** One process holding a queried port. Rust: `port_commands::PortListener` (camelCase). */
export interface PortListener {
  queriedPort: number;
  pid: number;
  proto: "TCP" | "UDP" | string;
  /** e.g. ["0.0.0.0", "[::]"] — a `[` prefix means IPv6. */
  localAddrs: string[];
  state: string;
  processName: string;
  exePath: string;
  /** "" | "system-pid" | "critical-process" — non-empty means the backend refuses to kill it. */
  protection: string;
}

export interface PortKillResult {
  port: number;
  killedPids: number[];
  failed: string[];
  stillListening: PortListener[];
  portReleased: boolean;
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
  /** 来自内置预设模板时的预设 id（如 nexus-batch），用于一键更新 */
  presetId?: string;
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

export type RuntimeKind = "node" | "jdk" | "maven" | "python";

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
  /** Version list after the switch, so the page needn't rescan. */
  list?: RuntimeVersion[];
}

/** A runtime release that can be downloaded and installed by the app. */
export interface InstallableVersion {
  version: string;
  lts: boolean;
  installed: boolean;
}

/** 应用内升级：某个 release 的更新日志与安装包资产（后端 `update_fetch_release_info`）。 */
export interface ReleaseInfo {
  /** 实际取到的版本，可能与请求的不同（tag 命名不一致时）。 */
  version: string;
  notes: string | null;
  setupUrl: string | null;
  setupSize: number;
  msiUrl: string | null;
  htmlUrl: string;
  /** true = GitHub API 没答上来（多半是匿名配额 403），日志缺失但直链兜底拿到了安装包。 */
  degraded: boolean;
}

export interface DownloadedInstaller {
  path: string;
  size: number;
}

/**
 * `app:update_progress` 事件负载。与 runtime/dsh 的 `{stage, percent, message}` 刻意不同形：
 * 进度文案一律由前端按 stage + 字节数用 i18n 拼，后端不再下发中文字串（en-US 会漏中文）。
 */
export interface UpdateProgress {
  stage: string;
  percent: number;
  downloaded: number;
  /** 0 = 服务端没给 Content-Length，只能显示已下载字节数。 */
  total: number;
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
