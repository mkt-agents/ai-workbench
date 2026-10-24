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

/** One commit a push would take along, with the paths it touched (`git_pending_commits`). */
export interface GitPendingCommit {
  hash: string;
  subject: string;
  when: string;
  files: string[];
}

/** `scope` says which comparison range the backend used: `upstream` (the branch has one),
 *  `remoteTracking` (no upstream but a same-named remote-tracking ref), `unpushed`
 *  (commits on no remote ref at all) or `noCommits` (unborn HEAD). `basis` is the ref name
 *  for the first two; empty for the rest. */
export interface GitPendingCommits {
  branch: string;
  scope: string;
  basis: string;
  commits: GitPendingCommit[];
  /** Real count in the range; `commits` is capped, `truncated` says so. */
  total: number;
  truncated: boolean;
}

export type AppTheme = 'light' | 'dark' | 'system' | 'glass' | 'ice' | 'silver';

/** 桌宠形象 id：换装顺序与帧清单在 lib/petSkins，这里只定设置项能存的取值。 */
export type PetSkinId = 'panda' | 'orb' | 'custom';

export interface AiHubChips {
  workspace: boolean;
  git: boolean;
  dirty: boolean;
}

/** AI call sites that can bind a preferred model (else fall back to default). */
export type AiScene = 'aiHub' | 'report' | 'commit' | 'optimize';

export type AiSceneModels = Partial<Record<AiScene, string>>;

/**
 * 侧栏菜单自定义（菜单管理页写入）。权威导航结构在 `lib/navStructure.tsx`，
 * 这里只存覆盖量；任何字段缺失/损坏都会被 `normalizeNavCustomization` 清洗回默认。
 */
export interface NavCustomization {
  /** 显示名覆盖：导航项用页签 id，分组用 `group:<分组 id>`；删除键 = 恢复默认文案。 */
  renames: Record<string, string>;
  /** 导航项 → 分组 id（覆盖默认分组）。 */
  itemGroups: Record<string, string>;
  /** 导航项完整顺序（新版本新增页签按默认顺序补齐）。 */
  itemOrder: string[];
  /** 分组完整顺序（含自定义分组）。 */
  groupOrder: string[];
  /** 用户新建的分组 id（组名存 renames[`group:<id>`]）。 */
  customGroups: string[];
}

export interface AppSettings {
  theme: AppTheme;
  language: 'zh-CN' | 'en-US';
  sidebarCollapsed: boolean;
  autoStart: boolean;
  /** Last selected git repo path for workbench/commit */
  currentGitRepo?: string;
  /** Global shortcut for ai-hub (e.g. Ctrl+Alt+K) */
  aiHubShortcut?: string;
  /** Persisted context chip toggles for ai-hub */
  aiHubChips?: AiHubChips;
  /** Repos given full-diff context in ai-hub (empty = auto = current repo) */
  aiHubFocusRepos?: string[];
  /** Folders added from the ai-hub workspace picker (persisted; services rescan on boot). */
  aiHubExtraFolders?: string[];
  /** Single repos added from the ai-hub workspace picker. */
  aiHubExtraRepos?: string[];
  /** Sidebar menu customization (menu manager page): renames / order / custom groups. */
  navCustomization?: NavCustomization;
  /** Last used ai-hub task (encoded, see aiHub/config encodeTask) */
  aiHubLastTask?: string;
  /** 任务模式（chat / code / doc / data / research / ops），决定工具子集与上下文采集。 */
  aiHubTaskMode?: string;
  /** Predict the next prompt after every answer (one extra model call per round). */
  aiHubSuggest?: boolean;
  /** 编辑档的写入也先弹 diff 确认（默认关）。 */
  agentDiffConfirm?: boolean;
  /** 「以后都允许」记住的 action_kind（如 `cmd:npm`、`git:commit`），上限 20 条。 */
  agentAlwaysAllow?: string[];
  /** 「以后都允许」按仓库记（key = 工作区根路径），选了仓库时优先用它而不是全局。 */
  agentAlwaysAllowByRepo?: Record<string, string[]>;
  /** 「以后都允许」再细到分支：仓库 → 分支 → action_kind 列表。拿到分支时记这一层。 */
  agentAlwaysAllowByBranch?: Record<string, Record<string, string[]>>;
  /** diff 确认按仓库覆盖（key = 工作区根路径）；缺省回落全局 `agentDiffConfirm`。 */
  agentDiffConfirmByRepo?: Record<string, boolean>;
  /** 天玑可以抓取的域名（空 = 不给 web_fetch 工具）。 */
  agentWebAllow?: string[];
  /** 允许天玑派只读子代理（task 工具，默认关）。 */
  agentSubagents?: boolean;
  /** 只读调用并发执行（默认开；关掉就逐个跑，步骤顺序更好读）。 */
  agentParallelReads?: boolean;
  /** 429 / 5xx 自动重试（默认开；关掉直接报错，避免重复计费）。 */
  agentRetries?: boolean;
  /** 搜索时仍屏蔽 .gitignore 里的文件（默认开）。 */
  agentGrepIgnore?: boolean;
  /** 长任务先让天玑用 update_plan 列步骤（默认关）。 */
  agentPlanFirst?: boolean;
  /** 每轮上下文预算（token，默认 48000；越大多带历史，也越贵）。 */
  agentContextTokens?: number;
  /** 单条工具结果回灌上限（字符，默认 12000）。 */
  agentResultCap?: number;
  /** Anthropic 走流式（默认开；某些网关 SSE 支持不好时可关）。 */
  agentAnthropicStream?: boolean;
  /** MCP 总开关（默认开；关掉这一轮不列任何 MCP 工具）。 */
  agentMcpEnabled?: boolean;
  /** 复用两分钟内的上下文缓存（默认开；关掉每问一次都重新采集 git 数据）。 */
  agentFreshContext?: boolean;
  /** 每日 token 预算（0 / 缺省 = 不限），只用于提醒，不阻断提问。 */
  aiUsageDailyBudget?: number;
  /** Show desktop floating bubble for ai-hub */
  aiHubBubbleEnabled?: boolean;
  /** Physical screen position of the bubble */
  aiHubBubblePos?: { x: number; y: number };
  /** 桌宠形象（缺省时读旧 localStorage 键 qa-bubble-pet）。 */
  aiHubPetSkin?: PetSkinId;
  /** 桌宠窗边长（逻辑 px，96 / 112 / 128 / 160；主窗下发，后端按它算位置）。 */
  aiHubPetSize?: number;
  /** 安静模式：不冒话、不做主动情绪反应，只保留状态动画。 */
  aiHubPetQuiet?: boolean;
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

/** One row of the model's own checklist (the `update_plan` tool). */
export interface AiHubPlanStep {
  /** 后端按位置发的稳定 id（s1…s8）：模型每轮重发整张清单，前端靠它保持行身份。 */
  id?: string;
  title: string;
  status: 'pending' | 'in_progress' | 'done';
}

/** One Q&A exchange inside a ai-hub session. */
export interface AiHubTurn {
  role: 'user' | 'assistant';
  content: string;
  /** Copy and save keep the whole reply (polish, translate, summary, daily report). */
  keep?: 'whole';
  /** Tool steps from an agent turn. Not sent as the visible answer. */
  steps?: { tool: string; status: string; summary: string; arg?: string; detail?: string }[];
  /** The model's plan for this round, kept so the checklist survives a reload. */
  plan?: AiHubPlanStep[];
  /** The agent stopped at its round / action cap; the thread offers「继续」. */
  limited?: boolean;
  /** Wall-clock ms of the streamed round, kept so an exported session shows cost. */
  elapsedMs?: number;
  promptTokens?: number;
  completionTokens?: number;
  /** Folded notes for rounds that fell outside the verbatim window. */
  digest?: boolean;
  /** 这一轮带上的上下文字符数（system + 对话）。 */
  contextChars?: number;
  /** 估算的上下文预算占比 0..1。 */
  budgetRatio?: number;
  /** 有内容被压缩的轮数。 */
  trimmedRounds?: number;
  /** 上游 prompt cache 命中的输入 token。 */
  cacheHitTokens?: number;
  /** 这一轮改过文件时的快照令牌（= requestId），用来「撤销这次改动」。 */
  undoToken?: string;
}

/** One file a turn can still undo (see `agent_undo.rs`). */
export interface AgentUndoFile {
  /** Absolute path as recorded by the turn. */
  path: string;
  /** How the turn touched it; decides what undo does. */
  kind: 'write' | 'create' | 'delete' | 'move';
  /** Destination of a `move` entry. */
  to: string;
  bytes: number;
}

/** A persisted ai-hub conversation (multi-turn). */
export interface AiHubSession {
  id: string;
  /** Short label derived from the first question. */
  title: string;
  /** Encoded TaskKind snapshot ('none' or 'optimize:…', see aiHub/config encodeTask). */
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
  /** merge | rebase | cherry-pick | revert while git is mid-operation. */
  operation?: GitOperation | null;
  /** Unmerged (conflicted) paths right now. */
  conflictCount?: number;
}

export type GitOperation = 'merge' | 'rebase' | 'cherry-pick' | 'revert';

export interface GitStatusEntry {
  path: string;
  indexStatus: string;
  workTreeStatus: string;
  /** `conflicted` = unmerged (UU/AA/DD/AU/UA/DU/UD); only the resolver stages it. */
  group: 'staged' | 'unstaged' | 'untracked' | 'conflicted' | string;
}

export interface GitMergeState {
  operation: GitOperation | null;
  conflicted: string[];
  /** MERGE_MSG without git's comment block. */
  message: string;
  oursLabel: string;
  theirsLabel: string;
}

export type MergeSegment =
  | { kind: 'same'; text: string }
  | { kind: 'conflict'; ours: string; base: string; theirs: string };

export interface GitConflictFile {
  kind: 'content' | 'deletedByThem' | 'deletedByUs' | 'deletedBoth';
  /** All versions are UTF-8 text; otherwise only whole-file choices are offered. */
  textual: boolean;
  ours: string | null;
  base: string | null;
  theirs: string | null;
  segments: MergeSegment[];
  oursLabel: string;
  theirsLabel: string;
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
  scriptTemplates: ScriptTemplate[];
  hostProfiles: HostProfile[];
  cursorAccounts: CursorAccount[];
  aiModels: AIModelConfig[];
  cloudflaredProfiles: CloudflaredNamedProfile[];
  snippets: Snippet[];
  drafts: Draft[];
  aiHubSessions: AiHubSession[];
  quickAppLaunchers: QuickAppLauncher[];
  mcpServers: McpServer[];
}

/**
 * An MCP (Model Context Protocol) server the user configured. The agent starts it over
 * stdio per call (`cmd /c <command> <args>`) and exposes its tools as `mcp_<name>_<tool>`.
 */
export interface McpServer {
  id: string;
  /** Shown in tool names and step lines; keep it short and ASCII. */
  name: string;
  /** Program to run, e.g. `npx` or `D:\tools\mcp-fs.exe`. */
  command: string;
  /** Argument line as typed; quotes group arguments. */
  args: string;
  /** Disabled servers are not offered to the model at all. */
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
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

/** A desktop app window the workbench can host. Rust: `desktop_embed::DesktopWindowInfo`. */
export interface DesktopWindowInfo {
  /** Image name matched on, without the `.exe` suffix. */
  processName: string;
  pid: number;
  title: string;
  /** Win32 class of the adopted window; shown on hover so a wrong pick is visible. */
  class: string;
  /** True when the window is currently parented into the workbench window. */
  attached: boolean;
  /**
   * Diagnostic code when the adopted window shows nothing (empty when fine):
   * `gone` | `not-child` | `minimized` | `invisible` | `size`.
   */
  warning: string;
}

/**
 * Outcome of a relayout: `ok` moved, `reparented` the app had un-parented
 * itself and we took it back, `replaced` the old frame was destroyed and a
 * fresh window was adopted, `lost` nothing to show any more.
 */
export interface DesktopRelayoutResult {
  state: "ok" | "reparented" | "replaced" | "lost";
  title: string;
  class: string;
  warning: string;
}

/**
 * One top-level window seen for a process, hidden and owned ones included.
 * Rust: `desktop_embed::DesktopWindowCandidate`. Shown when adoption fails,
 * and listed in the window picker.
 */
export interface DesktopWindowCandidate {
  /** Image name of the owning process, without the `.exe` suffix. */
  processName: string;
  pid: number;
  /** Window handle as hex (`0x00010123`). */
  hwnd: string;
  /** Win32 window class, e.g. `SunAwtFrame` for JetBrains IDEs. */
  class: string;
  title: string;
  visible: boolean;
  /** Has an owner window (usually a dialog, sometimes a launcher helper). */
  owned: boolean;
  /** Window area in physical pixels; the largest is the main frame. */
  area: number;
  /** Window width in physical pixels, for the picker's "W × H" label. */
  width: number;
  height: number;
  /** This handle is the one the auto-pick would adopt; the picker badges it. */
  recommended: boolean;
  /** This handle is the window embedded right now. */
  current: boolean;
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

/** 用户自定义脚本模板（网页工具-脚本页「我的模板」），内置模板之外的补充 */
export interface ScriptTemplate {
  id: string;
  name: string;
  description: string;
  matchPatterns: string[];
  code: string;
  createdAt: string;
  updatedAt: string;
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
  /** Accepts images: needed for the AI hub to operate the computer (完全权限). */
  vision?: boolean;
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
