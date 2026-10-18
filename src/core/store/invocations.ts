/**
 * Pure pass-through Tauri invoke wrappers.
 *
 * These functions do no business logic — they forward arguments to the
 * corresponding Rust `#[tauri::command]` and return the result. Keeping them
 * separate from the store's business logic makes both easier to read.
 */
import { tauriInvoke } from './helpers';
import type { AIModelConfig, DshInstance, GitBranchList, GitConflictFile, GitMergeState, GitRepoSummary, GitScannedRepo, GitStatusEntry, InstallableVersion, RuntimeKind, RuntimeSwitchPlan, RuntimeSwitchResult, RuntimeVersion, InstalledAppInfo, PortListener, PortKillResult, ReleaseInfo, DownloadedInstaller } from '../types';
import type {
  AiResult,
  FolderReportBundle,
  ReportAiHistoryEntry,
  ReportAiHistoryInput,
  TestReportBundle,
} from '../../components/testReportTypes';

export interface Invocations {
  invokeTestModelConnection: (config: AIModelConfig) => Promise<{ success: boolean; message: string }>;
  invokeGenerateText: (params: { config: AIModelConfig; system: string; user: string }) => Promise<string>;
  invokeListProviderModels: (params: { provider: string; apiKey: string; baseUrl: string }) => Promise<{ success: boolean; models: { id: string; label?: string }[]; message?: string }>;
  invokeDetectLocalAiEndpoints: () => Promise<
    Array<{ kind: string; baseUrl: string; label: string; reachable: boolean; models: string[] }>
  >;
  invokeStartDsh: (port?: number) => Promise<DshInstance>;
  invokeDshSetEmbed: (params: { url: string; x: number; y: number; width: number; height: number; visible: boolean }) => Promise<void>;
  invokeStopDsh: (port: number) => Promise<string>;
  invokeListDsh: () => Promise<DshInstance[]>;
  invokeCheckDshPort: (port: number) => Promise<boolean>;
  invokeCheckDshHttp: (port: number) => Promise<boolean>;
  invokeCheckNodejs: () => Promise<boolean>;
  invokeGetDshVersion: () => Promise<string>;
  invokeGetDshLatestVersion: () => Promise<string>;
  invokeGetDshVersions: () => Promise<string[]>;
  invokeInstallDsh: (version?: string) => Promise<string>;
  invokeUpdateDsh: (version?: string | null) => Promise<string>;
  invokeSyncModelToDsh: (params: { name: string; provider: string; apiKey: string; baseUrl: string; model: string; maxTokens: number }) => Promise<string>;
  invokeCloudflaredStatus: () => Promise<{ installed: boolean; version: string; path: string; message: string; customPath: boolean }>;
  invokeCloudflaredInstall: () => Promise<string>;
  invokeCloudflaredOpenDownload: () => Promise<void>;
  invokeCloudflaredPickBinary: () => Promise<string | null>;
  invokeCloudflaredSetBinaryPath: (path: string) => Promise<string>;
  invokeCloudflaredClearBinaryPath: () => Promise<string>;
  invokeCloudflaredPickConfig: () => Promise<string | null>;
  invokeCloudflaredStartQuickTunnel: (localUrl: string, protocol?: string) => Promise<{ id: string; running: boolean; pid?: number | null; localUrl?: string | null; publicUrl?: string | null; mode?: string | null; profileId?: string | null }>;
  invokeCloudflaredStartNamedTunnel: (params: { profileId: string; hostname: string; token?: string; configPath?: string; localUrl?: string; protocol?: string }) => Promise<{ id: string; running: boolean; pid?: number | null; localUrl?: string | null; publicUrl?: string | null; mode?: string | null; profileId?: string | null }>;
  invokeCloudflaredStopTunnel: (id: string) => Promise<string>;
  invokeCloudflaredStopAllTunnels: () => Promise<string>;
  invokeCloudflaredTunnelStatus: () => Promise<Array<{ id: string; running: boolean; pid?: number | null; localUrl?: string | null; publicUrl?: string | null; mode?: string | null; profileId?: string | null }>>;
  invokeCloudflaredSetupNewDomain: (params: { configPath: string; hostname: string; localUrl: string }) => Promise<string>;
  invokeSetGitConfig: (scope: string, name: string, email: string) => Promise<string>;
  invokeSetRepoGitConfig: (repoPath: string, name: string, email: string) => Promise<string>;
  invokeGetGitConfig: (scope?: string) => Promise<[string, string]>;
  invokeGetRepoGitConfig: (repoPath: string) => Promise<[string, string]>;
  invokePickDirectory: () => Promise<string | null>;
  invokeSetAiHubPinned: (pinned: boolean) => Promise<void>;
  invokeGitRepoSummary: (path: string) => Promise<GitRepoSummary>;
  invokeGitStatus: (path: string) => Promise<GitStatusEntry[]>;
  invokeGitStage: (path: string, files: string[]) => Promise<string>;
  invokeGitUnstage: (path: string, files: string[]) => Promise<string>;
  invokeGitCommit: (path: string, message: string, amend?: boolean) => Promise<string>;
  /** `worktree`: apply to the working file instead of the index (block revert / undo). */
  invokeGitApplyCached: (path: string, patch: string, reverse?: boolean, worktree?: boolean) => Promise<string>;
  invokeGitStashPush: (path: string, message?: string) => Promise<string>;
  invokeGitStashList: (path: string) => Promise<Array<{ index: number; message: string }>>;
  invokeGitStashPop: (path: string, index: number) => Promise<string>;
  invokeGitStashDrop: (path: string, index: number) => Promise<string>;
  invokeGitPush: (path: string) => Promise<string>;
  invokeGitPull: (path: string, merge?: boolean) => Promise<string>;
  invokeGitMergeState: (path: string) => Promise<GitMergeState>;
  invokeGitConflictFile: (path: string, file: string) => Promise<GitConflictFile>;
  /** Record the user's resolution: full `content`, or take `ours` / `theirs` / `delete`. */
  invokeGitResolveConflict: (
    path: string,
    file: string,
    resolution: { content: string } | { take: 'ours' | 'theirs' | 'delete' },
  ) => Promise<string>;
  invokeGitMergeAbort: (path: string) => Promise<string>;
  invokeGitCreateBranch: (path: string, name: string) => Promise<string>;
  invokeGitSwitchBranch: (path: string, name: string) => Promise<string>;
  invokeGitListBranches: (path: string) => Promise<GitBranchList>;
  /** `contextLines` / `ignoreWhitespace` are view options: such a diff can't be staged by hunk. */
  invokeGitDiff: (
    path: string,
    filePath: string,
    staged: boolean,
    opts?: { ignoreWhitespace?: boolean; contextLines?: number },
  ) => Promise<string>;
  invokeGitLog: (path: string, count: number, since?: string) => Promise<string[][]>;
  invokeGitLogFiles: (
    path: string,
    count: number,
    since?: string,
  ) => Promise<Array<{ subject: string; when: string; files: string[] }>>;
  invokeGitDiscard: (
    path: string,
    filePath: string,
    untracked: boolean,
    staged: boolean,
  ) => Promise<string>;
  invokeGitUndoLastCommit: (path: string) => Promise<string>;
  invokeGitCommitContext: (path: string) => Promise<{ files: string[]; diff: string; source: string }>;
  invokeGitIsRepo: (path: string) => Promise<boolean>;
  invokeGitScanRepos: (path: string, maxDepth?: number) => Promise<GitScannedRepo[]>;
  invokeGitRemoteUrl: (path: string) => Promise<string | null>;
  invokeCollectTestReport: (
    repoPath: string,
    base?: string,
    lastCommits?: number,
    since?: string,
    until?: string,
  ) => Promise<TestReportBundle>;
  invokeCollectFolderReport: (
    folderName: string,
    repoPaths: string[],
    base?: string,
    lastCommits?: number,
    since?: string,
    until?: string,
  ) => Promise<FolderReportBundle>;
  invokeGenerateTestReportAi: (
    reportId: string,
    config: AIModelConfig,
    system?: string,
    requirement?: string,
  ) => Promise<AiResult>;
  invokeRegenerateTestReportAiSection: (
    reportId: string,
    config: AIModelConfig,
    section: string,
    system?: string,
    requirement?: string,
  ) => Promise<AiResult>;
  invokeCancelTestReportAi: (reportId: string) => Promise<void>;
  invokeSaveReportAiHistory: (entry: ReportAiHistoryInput) => Promise<number>;
  invokeListReportAiHistory: () => Promise<ReportAiHistoryEntry[]>;
  invokeDeleteReportAiHistory: (id: number) => Promise<void>;
  invokeReadSystemHosts: () => Promise<string>;
  invokeIsAdmin: () => Promise<boolean>;
  invokeRelaunchAsAdmin: () => Promise<void>;
  invokeWriteSystemHosts: (content: string) => Promise<string>;
  invokeListHostBackups: () => Promise<Array<{ path: string; name: string }>>;
  invokeRestoreHostBackup: (path: string) => Promise<string>;
  invokeDeleteHostBackup: (path: string) => Promise<void>;
  invokeSetAutoStart: (enabled: boolean) => Promise<void>;
  invokeGetAutoStart: () => Promise<boolean>;
  invokeCopyToClipboard: (text: string) => Promise<void>;
  invokeReadClipboard: () => Promise<string>;
  invokeExportData: () => Promise<string>;
  invokeImportData: () => Promise<string>;
  invokeSaveTextFile: (content: string, defaultName: string, title?: string) => Promise<string>;
  invokePickTextFile: (title?: string) => Promise<string>;
  invokePickSourceFile: (title?: string) => Promise<string>;
  invokeReadTextFile: (path: string) => Promise<string>;
  invokeListRuntimeVersions: (kind: RuntimeKind) => Promise<RuntimeVersion[]>;
  invokeGetActiveRuntime: (kind: RuntimeKind) => Promise<RuntimeVersion | null>;
  invokeAddCustomRuntime: (kind: RuntimeKind, path: string) => Promise<RuntimeVersion>;
  invokeRemoveCustomRuntime: (kind: RuntimeKind, path: string) => Promise<void>;
  invokeSwitchRuntime: (kind: RuntimeKind, path: string) => Promise<RuntimeSwitchResult>;
  invokePlanRuntimeSwitch: (kind: RuntimeKind, path: string) => Promise<RuntimeSwitchPlan>;
  invokeOpenRuntimeFolder: (path: string) => Promise<void>;
  invokeOpenRuntimeTerminal: (kind: RuntimeKind, binPath?: string | null) => Promise<void>;
  invokeListInstallableRuntimes: (kind: RuntimeKind) => Promise<InstallableVersion[]>;
  invokeInstallRuntime: (kind: RuntimeKind, version: string) => Promise<RuntimeVersion>;
  invokeUninstallRuntime: (kind: RuntimeKind, path: string) => Promise<void>;
  invokeLaunchApp: (path: string, args?: string) => Promise<string>;
  invokeKillApp: (processName: string) => Promise<string>;
  invokeIsAppRunning: (processName: string) => Promise<boolean>;
  invokeScanInstalledApps: () => Promise<InstalledAppInfo[]>;
  invokeScanDesktopApps: () => Promise<InstalledAppInfo[]>;
  invokeScanStorePackages: () => Promise<string[]>;
  invokeGetAppVersion: (path: string) => Promise<string>;
  invokeFindPortListeners: (ports: number[]) => Promise<PortListener[]>;
  invokeKillPortListeners: (port: number, pids: number[]) => Promise<PortKillResult>;
  invokeUpdateReleaseInfo: (version: string) => Promise<ReleaseInfo>;
  invokeUpdateDownload: (requestId: string, url: string) => Promise<DownloadedInstaller>;
  invokeUpdateInstall: (path: string) => Promise<void>;
  invokeCancelRequest: (id: string) => Promise<void>;
  invokeUpdateLatestTag: () => Promise<string>;
  invokeFetchSiteIcon: (url: string) => Promise<string | null>;
}

export const invocations: Invocations = {
  // AI
  invokeTestModelConnection: (config: AIModelConfig) =>
    tauriInvoke<{ success: boolean; message: string }>('test_model_connection', { config }),
  invokeGenerateText: (params: { config: AIModelConfig; system: string; user: string }) =>
    tauriInvoke<string>('generate_text', { req: params }),
  invokeListProviderModels: (params: { provider: string; apiKey: string; baseUrl: string }) =>
    tauriInvoke<{ success: boolean; models: { id: string; label?: string }[]; message?: string }>(
      'list_provider_models',
      { config: params },
    ),
  invokeDetectLocalAiEndpoints: () =>
    tauriInvoke<
      Array<{ kind: string; baseUrl: string; label: string; reachable: boolean; models: string[] }>
    >('detect_local_ai_endpoints'),

  // DeepSeek Harness
  invokeStartDsh: (port?: number) =>
    tauriInvoke<DshInstance>('start_dsh', { port }),
  invokeDshSetEmbed: (params) =>
    tauriInvoke<void>('dsh_set_embed', params),
  invokeStopDsh: (port: number) =>
    tauriInvoke<string>('stop_dsh', { port }),
  invokeListDsh: () =>
    tauriInvoke<DshInstance[]>('list_dsh'),
  invokeCheckDshPort: (port: number) =>
    tauriInvoke<boolean>('check_dsh_port', { port }),
  invokeCheckDshHttp: (port: number) =>
    tauriInvoke<boolean>('check_dsh_http', { port }),
  invokeCheckNodejs: () =>
    tauriInvoke<boolean>('check_nodejs_installed'),
  invokeGetDshVersion: () =>
    tauriInvoke<string>('get_dsh_version'),
  invokeGetDshLatestVersion: () =>
    tauriInvoke<string>('get_dsh_latest_version'),
  invokeGetDshVersions: () =>
    tauriInvoke<string[]>('get_dsh_versions'),
  invokeInstallDsh: (version?: string) =>
    tauriInvoke<string>('install_dsh', { version }),
  // Install exactly the version the UI offered; the backend falls back to `latest`
  // when omitted, which can resolve to the already-installed build.
  invokeUpdateDsh: (version?: string | null) =>
    tauriInvoke<string>('update_dsh', { version: version ?? null }),
  invokeSyncModelToDsh: (params: { name: string; provider: string; apiKey: string; baseUrl: string; model: string; maxTokens: number }) =>
    tauriInvoke<string>('sync_model_to_dsh', params),

  // Cloudflared
  invokeCloudflaredStatus: () =>
    tauriInvoke<{
      installed: boolean;
      version: string;
      path: string;
      message: string;
      customPath: boolean;
    }>('cloudflared_status'),
  invokeCloudflaredInstall: () => tauriInvoke<string>('cloudflared_install'),
  invokeCloudflaredOpenDownload: () =>
    tauriInvoke<void>('cloudflared_open_download'),
  invokeCloudflaredPickBinary: () =>
    tauriInvoke<string | null>('cloudflared_pick_binary'),
  invokeCloudflaredSetBinaryPath: (path: string) =>
    tauriInvoke<string>('cloudflared_set_binary_path', { path }),
  invokeCloudflaredClearBinaryPath: () =>
    tauriInvoke<string>('cloudflared_clear_binary_path'),
  invokeCloudflaredPickConfig: () =>
    tauriInvoke<string | null>('cloudflared_pick_config'),
  invokeCloudflaredStartQuickTunnel: (localUrl: string, protocol?: string) =>
    tauriInvoke<{
      id: string;
      running: boolean;
      pid?: number | null;
      localUrl?: string | null;
      publicUrl?: string | null;
      mode?: string | null;
      profileId?: string | null;
    }>('cloudflared_start_quick_tunnel', { localUrl, protocol }),
  invokeCloudflaredStartNamedTunnel: (params: {
    profileId: string;
    hostname: string;
    token?: string;
    configPath?: string;
    localUrl?: string;
    protocol?: string;
  }) =>
    tauriInvoke<{
      id: string;
      running: boolean;
      pid?: number | null;
      localUrl?: string | null;
      publicUrl?: string | null;
      mode?: string | null;
      profileId?: string | null;
    }>('cloudflared_start_named_tunnel', {
      profileId: params.profileId,
      hostname: params.hostname,
      token: params.token,
      configPath: params.configPath,
      localUrl: params.localUrl,
      protocol: params.protocol,
    }),
  invokeCloudflaredStopTunnel: (id: string) =>
    tauriInvoke<string>('cloudflared_stop_tunnel', { id }),
  invokeCloudflaredStopAllTunnels: () =>
    tauriInvoke<string>('cloudflared_stop_all_tunnels'),
  invokeCloudflaredTunnelStatus: () =>
    tauriInvoke<
      Array<{
        id: string;
        running: boolean;
        pid?: number | null;
        localUrl?: string | null;
        publicUrl?: string | null;
        mode?: string | null;
        profileId?: string | null;
      }>
    >('cloudflared_tunnel_status'),
  invokeCloudflaredSetupNewDomain: (params: {
    configPath: string;
    hostname: string;
    localUrl: string;
  }) =>
    tauriInvoke<string>('cloudflared_setup_new_domain', {
      configPath: params.configPath,
      hostname: params.hostname,
      localUrl: params.localUrl,
    }),

  // Git
  invokeSetGitConfig: (scope: string, name: string, email: string) =>
    tauriInvoke<string>('set_git_config', { config: { scope, name, email } }),
  invokeSetRepoGitConfig: (repoPath: string, name: string, email: string) =>
    tauriInvoke<string>('set_repo_git_config', { config: { repoPath, name, email } }),
  invokeGetGitConfig: (scope?: string) =>
    tauriInvoke<[string, string]>('get_git_config', { scope }),
  invokeGetRepoGitConfig: (repoPath: string) =>
    tauriInvoke<[string, string]>('get_repo_git_config', { repoPath }),
  invokePickDirectory: () =>
    tauriInvoke<string | null>('pick_directory'),
  invokeSetAiHubPinned: (pinned: boolean) =>
    tauriInvoke<void>('set_ai_hub_pinned', { pinned }),
  invokeGitRepoSummary: (path: string) =>
    tauriInvoke<GitRepoSummary>('git_repo_summary', { path }),
  invokeGitStatus: (path: string) =>
    tauriInvoke<GitStatusEntry[]>('git_status', { path }),
  invokeGitStage: (path: string, files: string[]) =>
    tauriInvoke<string>('git_stage', { path, files }),
  invokeGitUnstage: (path: string, files: string[]) =>
    tauriInvoke<string>('git_unstage', { path, files }),
  invokeGitCommit: (path: string, message: string, amend?: boolean) =>
    tauriInvoke<string>('git_commit', { path, message, amend: amend ?? false }),
  invokeGitApplyCached: (path: string, patch: string, reverse?: boolean, worktree?: boolean) =>
    tauriInvoke<string>('git_apply_cached', { path, patch, reverse: reverse ?? false, worktree: worktree ?? false }),
  invokeGitStashPush: (path: string, message?: string) =>
    tauriInvoke<string>('git_stash_push', { path, message: message ?? null }),
  invokeGitStashList: (path: string) =>
    tauriInvoke<Array<{ index: number; message: string }>>('git_stash_list', { path }),
  invokeGitStashPop: (path: string, index: number) =>
    tauriInvoke<string>('git_stash_pop', { path, index }),
  invokeGitStashDrop: (path: string, index: number) =>
    tauriInvoke<string>('git_stash_drop', { path, index }),
  invokeGitPush: (path: string) =>
    tauriInvoke<string>('git_push', { path }),
  invokeGitPull: (path: string, merge?: boolean) =>
    tauriInvoke<string>('git_pull', { path, merge: merge ?? false }),
  invokeGitMergeState: (path: string) =>
    tauriInvoke<GitMergeState>('git_merge_state', { path }),
  invokeGitConflictFile: (path: string, file: string) =>
    tauriInvoke<GitConflictFile>('git_conflict_file', { path, file }),
  invokeGitResolveConflict: (path, file, resolution) =>
    tauriInvoke<string>('git_resolve_conflict', {
      path,
      file,
      content: 'content' in resolution ? resolution.content : null,
      take: 'take' in resolution ? resolution.take : null,
    }),
  invokeGitMergeAbort: (path: string) =>
    tauriInvoke<string>('git_merge_abort', { path }),
  invokeGitCreateBranch: (path: string, name: string) =>
    tauriInvoke<string>('git_create_branch', { path, name }),
  invokeGitSwitchBranch: (path: string, name: string) =>
    tauriInvoke<string>('git_switch_branch', { path, name }),
  invokeGitListBranches: (path: string) =>
    tauriInvoke<GitBranchList>('git_list_branches', { path }),
  invokeGitDiff: (path, filePath, staged, opts) =>
    tauriInvoke<string>('git_diff', {
      path,
      filePath,
      staged,
      ignoreWhitespace: opts?.ignoreWhitespace ?? false,
      contextLines: opts?.contextLines ?? null,
    }),
  invokeGitLog: (path: string, count: number, since?: string) =>
    tauriInvoke<string[][]>('git_log', { path, count, since: since ?? null }),
  invokeGitLogFiles: (path: string, count: number, since?: string) =>
    tauriInvoke<Array<{ subject: string; when: string; files: string[] }>>('git_log_files', {
      path,
      count,
      since: since ?? null,
    }),
  invokeGitDiscard: (path: string, filePath: string, untracked: boolean, staged: boolean) =>
    tauriInvoke<string>('git_discard', { path, filePath, untracked, staged }),
  invokeGitUndoLastCommit: (path: string) =>
    tauriInvoke<string>('git_undo_last_commit', { path }),
  invokeGitCommitContext: (path: string) =>
    tauriInvoke<{ files: string[]; diff: string; source: string }>('git_commit_context', { path }),
  invokeGitIsRepo: (path: string) =>
    tauriInvoke<boolean>('git_is_repo', { path }),
  invokeGitScanRepos: (path: string, maxDepth?: number) =>
    tauriInvoke<GitScannedRepo[]>('git_scan_repos', { path, maxDepth }),
  invokeGitRemoteUrl: (path: string) =>
    tauriInvoke<string | null>('git_remote_url', { path }),

  // Regression report (repo card)
  invokeCollectTestReport: (repoPath: string, base?: string, lastCommits?: number, since?: string, until?: string) =>
    tauriInvoke<TestReportBundle>('collect_test_report', { repoPath, base, lastCommits, since, until }),
  invokeCollectFolderReport: (
    folderName: string,
    repoPaths: string[],
    base?: string,
    lastCommits?: number,
    since?: string,
    until?: string,
  ) =>
    tauriInvoke<FolderReportBundle>('collect_folder_report', {
      folderName,
      repoPaths,
      base,
      lastCommits,
      since,
      until,
    }),
  invokeGenerateTestReportAi: (reportId: string, config: AIModelConfig, system?: string, requirement?: string) =>
    tauriInvoke<AiResult>('generate_test_report_ai', { reportId, config, system, requirement }),
  invokeRegenerateTestReportAiSection: (
    reportId: string,
    config: AIModelConfig,
    section: string,
    system?: string,
    requirement?: string,
  ) =>
    tauriInvoke<AiResult>('regenerate_test_report_ai_section', {
      reportId,
      config,
      section,
      system,
      requirement,
    }),
  invokeCancelTestReportAi: (reportId: string) =>
    tauriInvoke<void>('cancel_test_report_ai', { reportId }),
  invokeSaveReportAiHistory: (entry: ReportAiHistoryInput) =>
    tauriInvoke<number>('save_report_ai_history', { entry }),
  invokeListReportAiHistory: () => tauriInvoke<ReportAiHistoryEntry[]>('list_report_ai_history'),
  invokeDeleteReportAiHistory: (id: number) =>
    tauriInvoke<void>('delete_report_ai_history', { id }),

  // Hosts
  invokeReadSystemHosts: () => tauriInvoke<string>('read_system_hosts'),
  invokeIsAdmin: () => tauriInvoke<boolean>('is_admin'),
  invokeRelaunchAsAdmin: () => tauriInvoke<void>('relaunch_as_admin'),
  invokeWriteSystemHosts: (content: string) => tauriInvoke<string>('write_system_hosts', { content }),
  invokeListHostBackups: () => tauriInvoke<{ path: string; name: string }[]>('list_host_backups'),
  invokeRestoreHostBackup: (path: string) => tauriInvoke<string>('restore_host_backup', { path }),
  invokeDeleteHostBackup: (path: string) => tauriInvoke<void>('delete_host_backup', { path }),

  // System
  invokeSetAutoStart: (enabled: boolean) =>
    tauriInvoke<void>('set_auto_start', { enabled }),
  invokeGetAutoStart: () =>
    tauriInvoke<boolean>('get_auto_start'),
  invokeCopyToClipboard: (text: string) =>
    tauriInvoke<void>('copy_to_clipboard', { text }),
  invokeReadClipboard: () => tauriInvoke<string>('read_clipboard'),
  invokeExportData: () => tauriInvoke<string>('export_data'),
  invokeImportData: () => tauriInvoke<string>('import_data'),
  invokeSaveTextFile: (content: string, defaultName: string, title?: string) =>
    tauriInvoke<string>('save_text_file', { content, defaultName, title: title ?? null }),
  invokePickTextFile: (title?: string) =>
    tauriInvoke<string>('pick_text_file', { title: title ?? null }),
  invokePickSourceFile: (title?: string) =>
    tauriInvoke<string>('pick_source_file', { title: title ?? null }),
  invokeReadTextFile: (path: string) =>
    tauriInvoke<string>('read_text_file', { path }),

  // Runtime switch
  invokeListRuntimeVersions: (kind: RuntimeKind) =>
    tauriInvoke<RuntimeVersion[]>('list_runtime_versions', { kind }),
  invokeGetActiveRuntime: (kind: RuntimeKind) =>
    tauriInvoke<RuntimeVersion | null>('get_active_runtime', { kind }),
  invokeAddCustomRuntime: (kind: RuntimeKind, path: string) =>
    tauriInvoke<RuntimeVersion>('add_custom_runtime', { kind, path }),
  invokeRemoveCustomRuntime: (kind: RuntimeKind, path: string) =>
    tauriInvoke<void>('remove_custom_runtime', { kind, path }),
  invokeSwitchRuntime: (kind: RuntimeKind, path: string) =>
    tauriInvoke<RuntimeSwitchResult>('switch_runtime', { kind, path }),
  invokePlanRuntimeSwitch: (kind: RuntimeKind, path: string) =>
    tauriInvoke<RuntimeSwitchPlan>('plan_runtime_switch', { kind, path }),
  invokeOpenRuntimeFolder: (path: string) =>
    tauriInvoke<void>('open_runtime_folder', { path }),
  invokeOpenRuntimeTerminal: (kind: RuntimeKind, binPath?: string | null) =>
    tauriInvoke<void>('open_runtime_terminal', { kind, binPath: binPath ?? null }),
  invokeListInstallableRuntimes: (kind: RuntimeKind) =>
    tauriInvoke<InstallableVersion[]>('list_installable_runtimes', { kind }),
  invokeInstallRuntime: (kind: RuntimeKind, version: string) =>
    tauriInvoke<RuntimeVersion>('install_runtime', { kind, version }),
  invokeUninstallRuntime: (kind: RuntimeKind, path: string) =>
    tauriInvoke<void>('uninstall_runtime', { kind, path }),
  invokeLaunchApp: (path: string, args?: string) =>
    tauriInvoke<string>('launch_app', { path, args: args ?? null }),
  invokeKillApp: (processName: string) =>
    tauriInvoke<string>('kill_app', { processName }),
  invokeIsAppRunning: (processName: string) =>
    tauriInvoke<boolean>('is_app_running', { processName }),
  invokeScanInstalledApps: () =>
    tauriInvoke<InstalledAppInfo[]>('scan_installed_apps'),
  invokeScanDesktopApps: () =>
    tauriInvoke<InstalledAppInfo[]>('scan_desktop_apps'),
  invokeScanStorePackages: () =>
    tauriInvoke<string[]>('scan_store_packages'),
  invokeGetAppVersion: (path: string) =>
    tauriInvoke<string>('get_app_version', { path }),
  invokeFindPortListeners: (ports: number[]) =>
    tauriInvoke<PortListener[]>('find_port_listeners', { ports }),
  invokeKillPortListeners: (port: number, pids: number[]) =>
    tauriInvoke<PortKillResult>('kill_port_listeners', { port, pids }),
  invokeUpdateLatestTag: () => tauriInvoke<string>('update_latest_tag'),
  invokeUpdateReleaseInfo: (version: string) =>
    tauriInvoke<ReleaseInfo>('update_fetch_release_info', { version }),
  invokeUpdateDownload: (requestId: string, url: string) =>
    tauriInvoke<DownloadedInstaller>('update_download', { requestId, url }),
  invokeUpdateInstall: (path: string) => tauriInvoke<void>('update_install', { path }),
  invokeCancelRequest: (id: string) => tauriInvoke<void>('cancel_request', { id }),
  invokeFetchSiteIcon: (url: string) => tauriInvoke<string | null>('fetch_site_icon', { url }),
};

export default invocations;
