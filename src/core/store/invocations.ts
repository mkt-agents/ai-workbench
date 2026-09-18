/**
 * Pure pass-through Tauri invoke wrappers.
 *
 * These functions do no business logic — they forward arguments to the
 * corresponding Rust `#[tauri::command]` and return the result. Keeping them
 * separate from the store's business logic makes both easier to read.
 */
import { tauriInvoke } from './helpers';
import type { AIModelConfig, DshInstance, DevtoolsHttpRequest, DevtoolsHttpResponse, DevtoolsPortEntry, DevtoolsProcessInfo, GitRepoSummary, GitScannedRepo, GitStatusEntry, InstallableVersion, RuntimeKind, RuntimeSwitchPlan, RuntimeSwitchResult, RuntimeVersion } from '../types';

export interface Invocations {
  invokeTestModelConnection: (config: AIModelConfig) => Promise<{ success: boolean; message: string }>;
  invokeGenerateText: (params: { config: AIModelConfig; system: string; user: string }) => Promise<string>;
  invokeListProviderModels: (params: { provider: string; apiKey: string; baseUrl: string }) => Promise<{ success: boolean; models: { id: string; label?: string }[]; message?: string }>;
  invokeStartDsh: (port?: number) => Promise<DshInstance>;
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
  invokeRestoreDshAuth: () => Promise<string>;
  invokeSyncModelToDsh: (params: { name: string; provider: string; apiKey: string; baseUrl: string; model: string; maxTokens: number }) => Promise<string>;
  invokeCloudflaredStatus: () => Promise<{ installed: boolean; version: string; path: string; message: string; customPath: boolean }>;
  invokeCloudflaredInstall: () => Promise<string>;
  invokeCloudflaredOpenDownload: () => Promise<void>;
  invokeCloudflaredPickBinary: () => Promise<string | null>;
  invokeCloudflaredSetBinaryPath: (path: string) => Promise<string>;
  invokeCloudflaredClearBinaryPath: () => Promise<string>;
  invokeCloudflaredPickConfig: () => Promise<string | null>;
  invokeCloudflaredStartQuickTunnel: (localUrl: string) => Promise<{ id: string; running: boolean; pid?: number | null; localUrl?: string | null; publicUrl?: string | null; mode?: string | null; profileId?: string | null }>;
  invokeCloudflaredStartNamedTunnel: (params: { profileId: string; hostname: string; token?: string; configPath?: string; localUrl?: string }) => Promise<{ id: string; running: boolean; pid?: number | null; localUrl?: string | null; publicUrl?: string | null; mode?: string | null; profileId?: string | null }>;
  invokeCloudflaredStopTunnel: (id: string) => Promise<string>;
  invokeCloudflaredStopAllTunnels: () => Promise<string>;
  invokeCloudflaredTunnelStatus: () => Promise<Array<{ id: string; running: boolean; pid?: number | null; localUrl?: string | null; publicUrl?: string | null; mode?: string | null; profileId?: string | null }>>;
  invokeCloudflaredSetupNewDomain: (params: { configPath: string; hostname: string; localUrl: string }) => Promise<string>;
  invokeSetGitConfig: (scope: string, name: string, email: string) => Promise<string>;
  invokeSetRepoGitConfig: (repoPath: string, name: string, email: string) => Promise<string>;
  invokeGetGitConfig: (scope?: string) => Promise<[string, string]>;
  invokeGetRepoGitConfig: (repoPath: string) => Promise<[string, string]>;
  invokePickDirectory: () => Promise<string | null>;
  invokeGitRepoSummary: (path: string) => Promise<GitRepoSummary>;
  invokeGitStatus: (path: string) => Promise<GitStatusEntry[]>;
  invokeGitStage: (path: string, files: string[]) => Promise<string>;
  invokeGitUnstage: (path: string, files: string[]) => Promise<string>;
  invokeGitCommit: (path: string, message: string) => Promise<string>;
  invokeGitPush: (path: string) => Promise<string>;
  invokeGitPull: (path: string) => Promise<string>;
  invokeGitDiff: (path: string, filePath: string, staged: boolean) => Promise<string>;
  invokeGitLog: (path: string, count: number) => Promise<string[][]>;
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
  invokeReadSystemHosts: () => Promise<string>;
  invokeIsAdmin: () => Promise<boolean>;
  invokeWriteSystemHosts: (content: string) => Promise<string>;
  invokeListHostBackups: () => Promise<Array<{ path: string; name: string }>>;
  invokeRestoreHostBackup: (path: string) => Promise<string>;
  invokeSetAutoStart: (enabled: boolean) => Promise<void>;
  invokeGetAutoStart: () => Promise<boolean>;
  invokeCopyToClipboard: (text: string) => Promise<void>;
  invokeReadClipboard: () => Promise<string>;
  invokeExportData: () => Promise<string>;
  invokeImportData: () => Promise<string>;
  invokeSaveTextFile: (content: string, defaultName: string, title?: string) => Promise<string>;
  invokePickTextFile: (title?: string) => Promise<string>;
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
  invokeListPorts: () => Promise<DevtoolsPortEntry[]>;
  invokeResolveProcesses: (pids: number[]) => Promise<DevtoolsProcessInfo[]>;
  invokeKillProcess: (pid: number) => Promise<void>;
  invokeHttpRequest: (req: DevtoolsHttpRequest) => Promise<DevtoolsHttpResponse>;
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

  // DeepSeek Harness
  invokeStartDsh: (port?: number) =>
    tauriInvoke<DshInstance>('start_dsh', { port }),
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
  invokeRestoreDshAuth: () =>
    tauriInvoke<string>('restore_dsh_auth'),
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
  invokeCloudflaredStartQuickTunnel: (localUrl: string) =>
    tauriInvoke<{
      id: string;
      running: boolean;
      pid?: number | null;
      localUrl?: string | null;
      publicUrl?: string | null;
      mode?: string | null;
      profileId?: string | null;
    }>('cloudflared_start_quick_tunnel', { localUrl }),
  invokeCloudflaredStartNamedTunnel: (params: {
    profileId: string;
    hostname: string;
    token?: string;
    configPath?: string;
    localUrl?: string;
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
  invokeGitRepoSummary: (path: string) =>
    tauriInvoke<GitRepoSummary>('git_repo_summary', { path }),
  invokeGitStatus: (path: string) =>
    tauriInvoke<GitStatusEntry[]>('git_status', { path }),
  invokeGitStage: (path: string, files: string[]) =>
    tauriInvoke<string>('git_stage', { path, files }),
  invokeGitUnstage: (path: string, files: string[]) =>
    tauriInvoke<string>('git_unstage', { path, files }),
  invokeGitCommit: (path: string, message: string) =>
    tauriInvoke<string>('git_commit', { path, message }),
  invokeGitPush: (path: string) =>
    tauriInvoke<string>('git_push', { path }),
  invokeGitPull: (path: string) =>
    tauriInvoke<string>('git_pull', { path }),
  invokeGitDiff: (path: string, filePath: string, staged: boolean) =>
    tauriInvoke<string>('git_diff', { path, filePath, staged }),
  invokeGitLog: (path: string, count: number) =>
    tauriInvoke<string[][]>('git_log', { path, count }),
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

  // Hosts
  invokeReadSystemHosts: () => tauriInvoke<string>('read_system_hosts'),
  invokeIsAdmin: () => tauriInvoke<boolean>('is_admin'),
  invokeWriteSystemHosts: (content: string) => tauriInvoke<string>('write_system_hosts', { content }),
  invokeListHostBackups: () => tauriInvoke<{ path: string; name: string }[]>('list_host_backups'),
  invokeRestoreHostBackup: (path: string) => tauriInvoke<string>('restore_host_backup', { path }),

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
  invokeListPorts: () => tauriInvoke<DevtoolsPortEntry[]>('devtools_list_ports'),
  invokeResolveProcesses: (pids: number[]) => tauriInvoke<DevtoolsProcessInfo[]>('devtools_resolve_processes', { pids }),
  invokeKillProcess: (pid: number) => tauriInvoke<void>('devtools_kill_process', { pid }),
  invokeHttpRequest: (req: DevtoolsHttpRequest) => tauriInvoke<DevtoolsHttpResponse>('devtools_http_request', { req }),
};

export default invocations;
