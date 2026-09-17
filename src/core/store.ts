import { create } from 'zustand';
import { persist, createJSONStorage } from 'zustand/middleware';
import type {
  GlobalState, AppSettings, GitAccount, GitRepoConfig,
  RecentProject, WebPlugin, HostProfile, CursorAccount,
  AIModelConfig, CloudflaredNamedProfile, GitWorkspace,
  Snippet, QuickAskSession,
} from './types';
import { storage } from './storage';
import { matchCursorAccount } from './cursorMatch';
import { normalizePath, pathKey, projectNameFromPath } from './pathUtils';
import { compareVersions } from '../lib/version';
import { buildSeedSnippets, optimisticUpdate, tauriInvoke, withTable } from './store/helpers';
import { invocations, type Invocations } from './store/invocations';

/** True when `v1` is older than `v2`; prerelease-aware (see `compareVersions`). */
const isOlderVersion = (v1: string, v2: string): boolean =>
  compareVersions(v1, v2) < 0;

const DEFAULT_SETTINGS: AppSettings = {
  theme: 'glass',
  language: 'zh-CN',
  sidebarCollapsed: false,
  autoStart: false,
  currentGitRepo: undefined,
  quickAskShortcut: 'Ctrl+Alt+K',
  quickAskPasteClipboard: true,
  quickAskChips: {
    workspace: true,
    git: true,
    dirty: true,
    clipboard: false,
  },
  quickAskBubbleEnabled: true,
  quickAskBubblePos: undefined,
  trayHintShown: false,
};

/** Preset web tools sites — shown as suggestions when user has no plugins yet. */
export const PRESET_WEB_PLUGINS: Omit<WebPlugin, 'id' | 'addedAt' | 'lastOpenedAt' | 'openCount'>[] = [
  { name: 'GitHub', url: 'https://github.com', group: '开发', tags: 'git,code', order: 0, hotkey: '', isPreset: true },
  { name: 'GitLab', url: 'https://gitlab.com', group: '开发', tags: 'git,code', order: 1, hotkey: '', isPreset: true },
  { name: 'Stack Overflow', url: 'https://stackoverflow.com', group: '开发', tags: 'qa,community', order: 2, hotkey: '', isPreset: true },
  { name: 'MDN Web Docs', url: 'https://developer.mozilla.org', group: '文档', tags: 'docs,reference', order: 3, hotkey: '', isPreset: true },
  { name: 'npm', url: 'https://www.npmjs.com', group: '开发', tags: 'packages,node', order: 4, hotkey: '', isPreset: true },
  { name: 'Vite', url: 'https://vitejs.dev', group: '文档', tags: 'docs,bundler', order: 5, hotkey: '', isPreset: true },
  { name: 'React', url: 'https://react.dev', group: '文档', tags: 'docs,framework', order: 6, hotkey: '', isPreset: true },
  { name: 'Tauri', url: 'https://tauri.app', group: '文档', tags: 'docs,desktop', order: 7, hotkey: '', isPreset: true },
  { name: 'ChatGPT', url: 'https://chat.openai.com', group: 'AI', tags: 'ai,chat', order: 8, hotkey: '', isPreset: true },
  { name: 'DeepSeek', url: 'https://chat.deepseek.com', group: 'AI', tags: 'ai,chat', order: 9, hotkey: '', isPreset: true },
  { name: 'Claude', url: 'https://claude.ai', group: 'AI', tags: 'ai,chat', order: 10, hotkey: '', isPreset: true },
  { name: 'Google', url: 'https://www.google.com', group: '工具', tags: 'search', order: 11, hotkey: '', isPreset: true },
  { name: 'YouTube', url: 'https://www.youtube.com', group: '工具', tags: 'video', order: 12, hotkey: '', isPreset: true },
];

/** One-shot: fold legacy logoVariant into theme, then drop the field. */
function migrateSettings(
  raw: Partial<AppSettings> & { logoVariant?: 'ice' | 'silver' }
): AppSettings {
  const { logoVariant, ...rest } = raw;
  let theme = rest.theme ?? DEFAULT_SETTINGS.theme;
  if (
    logoVariant === 'silver' &&
    theme !== 'ice' &&
    theme !== 'silver'
  ) {
    theme = 'silver';
  }
  return {
    ...DEFAULT_SETTINGS,
    ...rest,
    theme,
    quickAskChips: {
      ...DEFAULT_SETTINGS.quickAskChips!,
      ...(rest.quickAskChips ?? {}),
    },
  };
}

interface StoreState extends GlobalState, Invocations {
  // Settings
  setSettings: (settings: Partial<AppSettings>) => void;

  // Git accounts
  loadAccounts: () => Promise<void>;
  addAccount: (account: Omit<GitAccount, 'createdAt' | 'updatedAt'>) => Promise<void>;
  updateAccount: (id: string, updates: Partial<GitAccount>) => Promise<void>;
  deleteAccount: (id: string) => Promise<void>;

  // Repo configs
  loadRepoConfigs: () => Promise<void>;
  addRepoConfig: (config: Omit<GitRepoConfig, 'createdAt' | 'updatedAt'>) => Promise<void>;
  upsertRepoConfigs: (configs: Omit<GitRepoConfig, 'createdAt' | 'updatedAt'>[]) => Promise<void>;
  updateRepoConfig: (path: string, updates: Partial<GitRepoConfig>) => Promise<void>;
  deleteRepoConfig: (path: string) => Promise<void>;

  // Recent projects
  loadRecentProjects: () => Promise<void>;
  addRecentProject: (project: Omit<RecentProject, 'id' | 'lastOpenedAt'>) => Promise<void>;
  addRecentProjects: (projects: Omit<RecentProject, 'id' | 'lastOpenedAt'>[]) => Promise<number>;
  removeRecentProject: (id: number) => Promise<void>;

  // Git workspaces (parent folders grouping multiple repos)
  loadWorkspaces: () => Promise<void>;
  addWorkspace: (path: string, name?: string) => Promise<void>;
  removeWorkspace: (id: number) => Promise<void>;

  // Web plugins
  loadWebPlugins: () => Promise<void>;
  addWebPlugin: (plugin: Omit<WebPlugin, 'addedAt'>) => Promise<void>;
  updateWebPlugin: (id: string, updates: Partial<WebPlugin>) => Promise<void>;
  deleteWebPlugin: (id: string) => Promise<void>;
  reorderWebPlugins: (orderedIds: string[]) => Promise<void>;
  recordPluginOpen: (id: string) => Promise<void>;
  addPresetPlugins: () => Promise<void>;

  // Host profiles
  loadHostProfiles: () => Promise<void>;
  addHostProfile: (profile: Omit<HostProfile, 'createdAt' | 'updatedAt'>) => Promise<void>;
  updateHostProfile: (id: string, updates: Partial<HostProfile>) => Promise<void>;
  deleteHostProfile: (id: string) => Promise<void>;

  // Cursor accounts
  loadCursorAccounts: () => Promise<void>;
  addCursorAccount: (account: Omit<CursorAccount, 'id' | 'createdAt' | 'updatedAt' | 'backupPath' | 'isLoggedIn' | 'profileDir' | 'profileInitialized'>) => Promise<string>;
  finishAccountProfile: (id: string) => Promise<string>;
  /** Re-open the independent Cursor profile for an account stuck in "pending init" (e.g. user closed Cursor before logging in). */
  reopenCursorForInit: (id: string) => Promise<string>;
  updateCursorAccount: (id: string, updates: Partial<CursorAccount>) => Promise<void>;
  deleteCursorAccount: (id: string) => Promise<void>;
  switchCursorAccount: (id: string, currentAccountId?: string | null) => Promise<string>;

  // Cursor invoke wrappers (delegated to store/invocations)
  invokeInspectCursorBackup: (accountId: string) => Promise<{
    accountId: string;
    complete: boolean;
    authEmail: string;
    hasAuthJson: boolean;
    hasCookies: boolean;
    reason: string;
    /** Non-blocking advisory, e.g. token expiring soon. */
    warning?: string;
  }>;
  invokeGetCursorLoginStatus: () => Promise<{ email: string; name: string; isLoggedIn: boolean }>;
  syncCursorLoggedInFlags: (liveEmail: string, liveName?: string) => Promise<void>;
  invokeIsCursorRunning: () => Promise<boolean>;
  invokeInitAccountProfile: (accountId: string) => Promise<string>;
  /** Launch Cursor (with this account's isolated profile when an id is given). */
  invokeLaunchCursor: (accountId?: string | null) => Promise<string>;
  invokeQuitCursor: () => Promise<string>;
  invokeListCursorBackups: () => Promise<
    Array<{ accountId: string; path: string; sizeBytes: number }>
  >;
  invokeGetCursorOrphanProfiles: () => Promise<{
    count: number;
    bytes: number;
    ids: string[];
  }>;
  invokeCleanupCursorOrphanProfiles: () => Promise<{
    removedFiles: number;
    freedBytes: number;
    message: string;
  }>;
  invokeFinishAccountProfile: (accountId: string, relaunch?: boolean) => Promise<string>;
  invokeGetCursorProfileDir: (accountId: string) => Promise<string>;
  invokeSwitchCursorAccount: (targetAccountId: string, currentAccountId?: string | null, relaunch?: boolean) => Promise<string>;
  invokeDeleteCursorBackup: (accountId: string) => Promise<void>;
  invokeGetCursorDiskUsage: () => Promise<{
    backupsBytes: number;
    backupsFullDbBytes: number;
    staleDbCount: number;
    sharedBytes: number;
    liveDbBytes: number;
    backupsPath: string;
    sharedPath: string;
    liveDbPath: string;
  }>;
  invokeCleanupCursorFullBackups: () => Promise<{
    removedFiles: number;
    freedBytes: number;
    message: string;
  }>;
  invokeReadCursorDiagnostics: (accountId?: string | null) => Promise<string>;

  // AI models
  loadAIModels: () => Promise<void>;
  addAIModel: (config: Omit<AIModelConfig, 'id' | 'createdAt' | 'updatedAt'>) => Promise<void>;
  updateAIModel: (id: string, updates: Partial<AIModelConfig>) => Promise<void>;
  deleteAIModel: (id: string) => Promise<void>;
  setDefaultAIModel: (id: string) => Promise<void>;

  // Cloudflared named profiles
  loadCloudflaredProfiles: () => Promise<void>;
  addCloudflaredProfile: (
    profile: Omit<CloudflaredNamedProfile, 'id' | 'createdAt' | 'updatedAt'>
  ) => Promise<void>;
  updateCloudflaredProfile: (
    id: string,
    updates: Partial<CloudflaredNamedProfile>
  ) => Promise<void>;
  deleteCloudflaredProfile: (id: string) => Promise<void>;

  // Snippets
  loadSnippets: () => Promise<void>;
  addSnippet: (snippet: Omit<Snippet, 'id' | 'createdAt' | 'updatedAt' | 'useCount'>) => Promise<void>;
  updateSnippet: (id: string, updates: Partial<Snippet>) => Promise<void>;
  deleteSnippet: (id: string) => Promise<void>;
  /** Re-insert a full snippet (e.g. undo delete) keeping its id. */
  restoreSnippet: (snippet: Snippet) => Promise<void>;
  bumpSnippetUse: (id: string) => Promise<void>;

  // Quick-ask sessions
  loadQuickAskSessions: () => Promise<void>;
  /** Insert or update a session (by id); caps the stored list at 30 by updatedAt. */
  upsertQuickAskSession: (session: QuickAskSession) => Promise<void>;
  deleteQuickAskSession: (id: string) => Promise<void>;

  // DeepSeek Harness cached state
  dshNodejsInstalled: boolean | null;
  dshVersion: string | null;
  dshLatestVersion: string | null;
  dshStatusChecked: boolean;
  dshHasUpdate: boolean;
  _dshStatusPromise: Promise<void | null> | null;
  loadDshStatus: () => Promise<void>;
  refreshDshStatus: () => Promise<void>;

  setCurrentGitRepo: (path: string | undefined) => void;

  // Initialize
  initialize: () => Promise<void>;
}

export const useGlobalStore = create<StoreState>()(
  persist(
    (set, get) => ({
      // Initial state
      settings: DEFAULT_SETTINGS,
      git: {
        accounts: [],
        repoConfigs: [],
      },
      recentProjects: [],
      gitWorkspaces: [],
      webPlugins: [],
      hostProfiles: [],
      cursorAccounts: [],
      aiModels: [],
      cloudflaredProfiles: [],
      snippets: [],
      quickAskSessions: [],

      // Settings
      setSettings: (newSettings) => set((state) => ({
        settings: { ...state.settings, ...newSettings },
      })),

      // Git Accounts
      loadAccounts: async () => withTable("git_accounts", async () => {
        const accounts = await storage.accounts.load();
        set((state) => ({ git: { ...state.git, accounts } }));
      }),

      addAccount: async (account) => withTable("git_accounts", async () => {
        const now = new Date().toISOString();
        const accounts = get().git.accounts;
        const newAccount = { ...account, id: crypto.randomUUID(), createdAt: now, updatedAt: now };
        const next = [newAccount, ...accounts];
        await optimisticUpdate(
          accounts,
          next,
          (v) => set((s) => ({ git: { ...s.git, accounts: v } })),
          (v) => storage.accounts.save(v),
        );
      }),

      updateAccount: async (id, updates) => withTable("git_accounts", async () => {
        const accounts = get().git.accounts;
        const next = accounts.map(a => {
          if (a.id !== id) return a;
          const updated = { ...a, ...updates, updatedAt: new Date().toISOString() };
          if ("note" in updates) {
            const n = updates.note?.trim();
            updated.note = n || undefined;
          }
          return updated;
        });
        await optimisticUpdate(
          accounts,
          next,
          (v) => set((s) => ({ git: { ...s.git, accounts: v } })),
          (v) => storage.accounts.save(v),
        );
      }),

      deleteAccount: async (id) => withTable("git_accounts", async () => {
        const accounts = get().git.accounts;
        const next = accounts.filter(a => a.id !== id);
        await optimisticUpdate(
          accounts,
          next,
          (v) => set((s) => ({ git: { ...s.git, accounts: v } })),
          (v) => storage.accounts.save(v),
        );
      }),

      // Repo Configs
      loadRepoConfigs: async () => withTable("git_repo_configs", async () => {
        const configs = await storage.repoConfigs.load();
        set((state) => ({ git: { ...state.git, repoConfigs: configs } }));
      }),

      addRepoConfig: async (config) => {
        await withTable("git_repo_configs", async () => {
          const now = new Date().toISOString();
          const configs = get().git.repoConfigs;
          const existingIdx = configs.findIndex((c) => pathKey(c.path) === pathKey(config.path));
          const next = existingIdx >= 0
            ? configs.map((c, i) =>
                i === existingIdx
                  ? { ...c, ...config, path: c.path, updatedAt: now }
                  : c
              )
            : [{ ...config, createdAt: now, updatedAt: now }, ...configs];
          await storage.repoConfigs.save(next);
          set((state) => ({ git: { ...state.git, repoConfigs: next } }));
        });
      },

      upsertRepoConfigs: async (incoming) => {
        if (incoming.length === 0) return;
        await withTable("git_repo_configs", async () => {
          const now = new Date().toISOString();
          const next = [...get().git.repoConfigs];
          for (const config of incoming) {
            const key = pathKey(config.path);
            const existingIdx = next.findIndex((c) => pathKey(c.path) === key);
            if (existingIdx >= 0) {
              const prev = next[existingIdx];
              next[existingIdx] = {
                ...prev,
                ...config,
                path: prev.path,
                createdAt: prev.createdAt,
                updatedAt: now,
              };
            } else {
              next.unshift({ ...config, createdAt: now, updatedAt: now });
            }
          }
          await storage.repoConfigs.save(next);
          set((state) => ({ git: { ...state.git, repoConfigs: next } }));
        });
      },

      updateRepoConfig: async (path, updates) => {
        await withTable("git_repo_configs", async () => {
          const configs = get().git.repoConfigs.map(c =>
            pathKey(c.path) === pathKey(path) ? { ...c, ...updates, updatedAt: new Date().toISOString() } : c
          );
          await storage.repoConfigs.save(configs);
          set((state) => ({ git: { ...state.git, repoConfigs: configs } }));
        });
      },

      deleteRepoConfig: async (path) => {
        await withTable("git_repo_configs", async () => {
          const configs = get().git.repoConfigs.filter(c => pathKey(c.path) !== pathKey(path));
          await storage.repoConfigs.save(configs);
          set((state) => ({ git: { ...state.git, repoConfigs: configs } }));
        });
      },

      // Recent Projects
      loadRecentProjects: async () => withTable("recent_projects", async () => {
        const projects = await storage.recentProjects.load();
        set({ recentProjects: projects });
      }),

      addRecentProject: async (project) => withTable("recent_projects", async () => {
        const now = new Date().toISOString();
        const projects = get().recentProjects;
        const existing = projects.findIndex(p => p.path === project.path);
        let next: RecentProject[];
        if (existing >= 0) {
          next = projects.map((p, i) =>
            i === existing ? { ...p, lastOpenedAt: now } : p
          );
        } else {
          // 递增 id 防碰撞
          const maxId = projects.reduce((m, p) => Math.max(m, p.id || 0), 0);
          next = [{ ...project, lastOpenedAt: now, id: maxId + 1 }, ...projects].slice(0, 200);
        }
        await storage.recentProjects.save(next);
        set({ recentProjects: next });
      }),

      addRecentProjects: async (projects) => withTable("recent_projects", async () => {
        const now = new Date().toISOString();
        const current = get().recentProjects;
        const existingPaths = new Set(current.map(p => p.path));
        let maxId = current.reduce((m, p) => Math.max(m, p.id || 0), 0);
        const additions: RecentProject[] = projects
          .filter(p => !existingPaths.has(p.path))
          .map(p => ({ ...p, lastOpenedAt: now, id: ++maxId }));
        if (additions.length === 0) return 0;
        const next = [...additions, ...current].slice(0, 200);
        await storage.recentProjects.save(next);
        set({ recentProjects: next });
        return additions.length;
      }),

      removeRecentProject: async (id) => withTable("recent_projects", async () => {
        const projects = get().recentProjects.filter(p => p.id !== id);
        await storage.recentProjects.save(projects);
        set({ recentProjects: projects });
      }),

      loadWorkspaces: async () => withTable("git_workspaces", async () => {
        const gitWorkspaces = await storage.gitWorkspaces.load();
        set({ gitWorkspaces });
      }),

      addWorkspace: async (path, name) => withTable("git_workspaces", async () => {
        const normalized = normalizePath(path);
        const current = get().gitWorkspaces;
        if (current.some((w) => pathKey(w.path) === pathKey(normalized))) return;
        const maxId = current.reduce((m, w) => Math.max(m, w.id || 0), 0);
        const ws: GitWorkspace = {
          id: maxId + 1,
          path: normalized,
          name: (name && name.trim()) || projectNameFromPath(normalized),
          createdAt: new Date().toISOString(),
        };
        const next = [ws, ...current];
        await storage.gitWorkspaces.save(next);
        set({ gitWorkspaces: next });
      }),

      removeWorkspace: async (id) => withTable("git_workspaces", async () => {
        const next = get().gitWorkspaces.filter((w) => w.id !== id);
        await storage.gitWorkspaces.save(next);
        set({ gitWorkspaces: next });
      }),

      // Web Plugins
      loadWebPlugins: async () => withTable("web_plugins", async () => {
        const plugins = await storage.webPlugins.load();
        set(() => ({ webPlugins: plugins }));
      }),

      addWebPlugin: async (plugin) => withTable("web_plugins", async () => {
        const now = new Date().toISOString();
        const plugins = get().webPlugins;
        const maxOrder = plugins.reduce((max, p) => Math.max(max, p.order), -1);
        const newPlugin: WebPlugin = {
          ...plugin,
          group: plugin.group || '',
          tags: plugin.tags || '',
          order: plugin.order ?? maxOrder + 1,
          lastOpenedAt: plugin.lastOpenedAt || '',
          openCount: plugin.openCount ?? 0,
          hotkey: plugin.hotkey || '',
          isPreset: plugin.isPreset ?? false,
          addedAt: now,
        };
        const next = [...plugins, newPlugin];
        await storage.webPlugins.save(next);
        set(() => ({ webPlugins: next }));
      }),

      updateWebPlugin: async (id, updates) => withTable("web_plugins", async () => {
        const plugins = get().webPlugins.map(p =>
          p.id === id ? { ...p, ...updates } : p
        );
        await storage.webPlugins.save(plugins);
        set(() => ({ webPlugins: plugins }));
      }),

      deleteWebPlugin: async (id) => withTable("web_plugins", async () => {
        const plugins = get().webPlugins.filter(p => p.id !== id);
        await storage.webPlugins.save(plugins);
        set(() => ({ webPlugins: plugins }));
      }),

      reorderWebPlugins: async (orderedIds: string[]) => withTable("web_plugins", async () => {
        const plugins = get().webPlugins;
        const orderMap = new Map(orderedIds.map((id, i) => [id, i]));
        const reordered = plugins
          .map(p => ({ ...p, order: orderMap.get(p.id) ?? p.order }))
          .sort((a, b) => a.order - b.order);
        await storage.webPlugins.save(reordered);
        set(() => ({ webPlugins: reordered }));
      }),

      recordPluginOpen: async (id: string) => withTable("web_plugins", async () => {
        const now = new Date().toISOString();
        const plugins = get().webPlugins.map(p =>
          p.id === id
            ? { ...p, lastOpenedAt: now, openCount: p.openCount + 1 }
            : p
        );
        await storage.webPlugins.save(plugins);
        set(() => ({ webPlugins: plugins }));
      }),

      addPresetPlugins: async () => withTable("web_plugins", async () => {
        const existing = get().webPlugins;
        const existingUrls = new Set(existing.map(p => p.url));
        const toAdd = PRESET_WEB_PLUGINS.filter(p => !existingUrls.has(p.url));
        if (toAdd.length === 0) return;
        const now = new Date().toISOString();
        const newPlugins: WebPlugin[] = toAdd.map((p) => ({
          ...p,
          id: `preset-${crypto.randomUUID()}`,
          addedAt: now,
          lastOpenedAt: '',
          openCount: 0,
        }));
        const next = [...existing, ...newPlugins];
        await storage.webPlugins.save(next);
        set(() => ({ webPlugins: next }));
      }),

      // Host Profiles
      loadHostProfiles: async () => withTable("host_profiles", async () => {
        const profiles = await storage.hostProfiles.load();
        set({ hostProfiles: profiles });
      }),

      addHostProfile: async (profile) => withTable("host_profiles", async () => {
        const now = new Date().toISOString();
        const profiles = get().hostProfiles;
        const newProfile = { ...profile, createdAt: now, updatedAt: now };
        const next = [...profiles, newProfile];
        await storage.hostProfiles.save(next);
        set({ hostProfiles: next });
      }),

      updateHostProfile: async (id, updates) => withTable("host_profiles", async () => {
        const profiles = get().hostProfiles.map(p =>
          p.id === id ? { ...p, ...updates, updatedAt: new Date().toISOString() } : p
        );
        await storage.hostProfiles.save(profiles);
        set({ hostProfiles: profiles });
      }),

      deleteHostProfile: async (id) => withTable("host_profiles", async () => {
        const profiles = get().hostProfiles.filter(p => p.id !== id);
        await storage.hostProfiles.save(profiles);
        set({ hostProfiles: profiles });
      }),

      // Cursor Accounts
      loadCursorAccounts: async () => withTable("cursor_accounts", async () => {
        const accounts = await storage.cursorAccounts.load();
        set({ cursorAccounts: accounts });
      }),

      addCursorAccount: async (account) => withTable("cursor_accounts", async () => {
        const now = new Date().toISOString();
        const id = crypto.randomUUID();
        const profileDir = await get().invokeGetCursorProfileDir(id);
        const initMsg = await get().invokeInitAccountProfile(id);
        const newAccount: CursorAccount = {
          ...account,
          id,
          profileDir,
          profileInitialized: false,
          backupPath: "",
          isLoggedIn: false,
          createdAt: now,
          updatedAt: now,
        };
        const next = [newAccount, ...get().cursorAccounts];
        await storage.cursorAccounts.save(next);
        set({ cursorAccounts: next });
        return initMsg;
      }),

      finishAccountProfile: async (id) => {
        const backupPath = await get().invokeFinishAccountProfile(id, true);
        const cleanPath = backupPath.trim().split(/[\r\n]/)[0].trim();
        await get().updateCursorAccount(id, {
          profileInitialized: true,
          backupPath: cleanPath,
        });
        const live = await get().invokeGetCursorLoginStatus();
        await get().syncCursorLoggedInFlags(live.email, live.name);
        return backupPath;
      },

      reopenCursorForInit: async (id) => {
        return await get().invokeInitAccountProfile(id);
      },

      updateCursorAccount: async (id, updates) => withTable("cursor_accounts", async () => {
        const accounts = get().cursorAccounts;
        const next = accounts.map(a =>
          a.id === id ? { ...a, ...updates, updatedAt: new Date().toISOString() } : a
        );
        await optimisticUpdate(
          accounts,
          next,
          (v) => set({ cursorAccounts: v }),
          (v) => storage.cursorAccounts.save(v),
        );
      }),

      deleteCursorAccount: async (id) => withTable("cursor_accounts", async () => {
        await get().invokeDeleteCursorBackup(id);
        const accounts = get().cursorAccounts;
        const next = accounts.filter(a => a.id !== id);
        await optimisticUpdate(
          accounts,
          next,
          (v) => set({ cursorAccounts: v }),
          (v) => storage.cursorAccounts.save(v),
        );
      }),

      switchCursorAccount: async (id, currentAccountId) => {
        const account = get().cursorAccounts.find(a => a.id === id);
        if (!account) throw new Error('Account not found');

        const status = await get().invokeInspectCursorBackup(id);
        if (!status.complete) {
          throw new Error(status.reason || 'snapshot incomplete');
        }

        const live = await get().invokeGetCursorLoginStatus();
        const liveMatched = get().cursorAccounts.find((a) =>
          matchCursorAccount(a, live)
        );
        const resolvedCurrent = currentAccountId !== undefined
          ? currentAccountId
          : (liveMatched && liveMatched.id !== id ? liveMatched.id : null);

        const result = await get().invokeSwitchCursorAccount(id, resolvedCurrent, true);

        if (!account.profileInitialized) {
          const profileDir = account.profileDir || (await get().invokeGetCursorProfileDir(id));
          await get().updateCursorAccount(id, { profileInitialized: true, profileDir });
        }

        const after = await get().invokeGetCursorLoginStatus();
        await get().syncCursorLoggedInFlags(after.email, after.name);

        if (account.gitUserName && account.gitEmail) {
          try {
            await get().invokeSetGitConfig('global', account.gitUserName, account.gitEmail);
          } catch (gitErr) {
            return `${result} | Git sync failed: ${gitErr}`;
          }
        }
        return result;
      },

      syncCursorLoggedInFlags: async (liveEmail, liveName) => withTable("cursor_accounts", async () => {
        const live = {
          email: liveEmail || '',
          name: liveName || '',
          isLoggedIn: Boolean(liveEmail || liveName),
        };
        const accounts = get().cursorAccounts.map((a) => ({
          ...a,
          isLoggedIn: matchCursorAccount(a, live),
          updatedAt: new Date().toISOString(),
        }));
        await storage.cursorAccounts.save(accounts);
        set({ cursorAccounts: accounts });
      }),

      // Cursor invoke wrappers
      invokeInspectCursorBackup: async (accountId) =>
        tauriInvoke<{
          accountId: string;
          complete: boolean;
          authEmail: string;
          hasAuthJson: boolean;
          hasCookies: boolean;
          reason: string;
          warning?: string;
        }>('inspect_cursor_backup', { accountId }),
      invokeGetCursorLoginStatus: async () =>
        tauriInvoke<{ email: string; name: string; isLoggedIn: boolean }>('get_cursor_login_status'),
      invokeIsCursorRunning: async () =>
        tauriInvoke<boolean>('is_cursor_running'),
      invokeInitAccountProfile: async (accountId) =>
        tauriInvoke<string>("init_account_profile", { accountId }),
      invokeLaunchCursor: async (accountId) =>
        tauriInvoke<string>("launch_cursor", { accountId: accountId ?? null }),
      invokeQuitCursor: async () => tauriInvoke<string>("quit_cursor"),
      invokeListCursorBackups: async () =>
        tauriInvoke<Array<{ accountId: string; path: string; sizeBytes: number }>>(
          "list_cursor_backups"
        ),
      invokeGetCursorOrphanProfiles: async () =>
        tauriInvoke<{ count: number; bytes: number; ids: string[] }>(
          "get_cursor_orphan_profiles"
        ),
      invokeCleanupCursorOrphanProfiles: async () =>
        tauriInvoke<{ removedFiles: number; freedBytes: number; message: string }>(
          "cleanup_cursor_orphan_profiles"
        ),
      invokeFinishAccountProfile: async (accountId, relaunch = true) =>
        tauriInvoke<string>("finish_account_profile", { accountId, relaunch }),
      invokeGetCursorProfileDir: async (accountId) =>
        tauriInvoke<string>("get_cursor_profile_dir", { accountId }),
      invokeSwitchCursorAccount: async (targetAccountId, currentAccountId, relaunch = true) =>
        tauriInvoke<string>('switch_cursor_account', {
          targetAccountId,
          currentAccountId: currentAccountId ?? null,
          relaunch,
        }),
      invokeDeleteCursorBackup: async (accountId) =>
        tauriInvoke<void>('delete_cursor_backup', { accountId }),
      invokeGetCursorDiskUsage: async () =>
        tauriInvoke<{
          backupsBytes: number;
          backupsFullDbBytes: number;
          staleDbCount: number;
          sharedBytes: number;
          liveDbBytes: number;
          backupsPath: string;
          sharedPath: string;
          liveDbPath: string;
        }>('get_cursor_disk_usage'),
      invokeCleanupCursorFullBackups: async () =>
        tauriInvoke<{
          removedFiles: number;
          freedBytes: number;
          message: string;
        }>('cleanup_cursor_full_backups'),

      invokeReadCursorDiagnostics: async (accountId) =>
        tauriInvoke<string>('read_cursor_diagnostics', { accountId: accountId ?? null }),

      // AI Models
      loadAIModels: async () => withTable("ai_models", async () => {
        const models = await storage.aiModels.load();
        set({ aiModels: models });
      }),

      addAIModel: async (config) => withTable("ai_models", async () => {
        const now = new Date().toISOString();
        const id = crypto.randomUUID();
        const isFirst = get().aiModels.length === 0;
        const newConfig: AIModelConfig = {
          ...config,
          id,
          isDefault: isFirst ? true : config.isDefault,
          createdAt: now,
          updatedAt: now,
        };
        const current = get().aiModels;
        let models = current;
        if (newConfig.isDefault) {
          models = models.map(m => ({ ...m, isDefault: false }));
        }
        const next = [newConfig, ...models];
        await optimisticUpdate(
          current,
          next,
          (v) => set({ aiModels: v }),
          (v) => storage.aiModels.save(v),
        );
      }),

      updateAIModel: async (id, updates) => withTable("ai_models", async () => {
        const current = get().aiModels;
        let next = current.map(m =>
          m.id === id ? { ...m, ...updates, updatedAt: new Date().toISOString() } : m
        );
        if (updates.isDefault) {
          next = next.map(m => m.id === id ? m : { ...m, isDefault: false });
        }
        await optimisticUpdate(
          current,
          next,
          (v) => set({ aiModels: v }),
          (v) => storage.aiModels.save(v),
        );
      }),

      deleteAIModel: async (id) => withTable("ai_models", async () => {
        const current = get().aiModels;
        const deleted = current.find((m) => m.id === id);
        let next = current.filter((m) => m.id !== id);
        if (deleted?.isDefault && next.length > 0 && !next.some((m) => m.isDefault)) {
          const now = new Date().toISOString();
          next = next.map((m, i) =>
            i === 0 ? { ...m, isDefault: true, updatedAt: now } : m
          );
        }
        await optimisticUpdate(
          current,
          next,
          (v) => set({ aiModels: v }),
          (v) => storage.aiModels.save(v),
        );
      }),

      setDefaultAIModel: async (id) => withTable("ai_models", async () => {
        const current = get().aiModels;
        const next = current.map(m => ({
          ...m,
          isDefault: m.id === id,
          updatedAt: new Date().toISOString(),
        }));
        await optimisticUpdate(
          current,
          next,
          (v) => set({ aiModels: v }),
          (v) => storage.aiModels.save(v),
        );
      }),

      loadCloudflaredProfiles: async () => withTable("cloudflared_profiles", async () => {
        const profiles = await storage.cloudflaredProfiles.load();
        set({ cloudflaredProfiles: profiles });
      }),

      addCloudflaredProfile: async (profile) => withTable("cloudflared_profiles", async () => {
        const now = new Date().toISOString();
        const next: CloudflaredNamedProfile = {
          ...profile,
          id: crypto.randomUUID(),
          createdAt: now,
          updatedAt: now,
        };
        const profiles = [next, ...get().cloudflaredProfiles];
        await storage.cloudflaredProfiles.save(profiles);
        set({ cloudflaredProfiles: profiles });
      }),

      updateCloudflaredProfile: async (id, updates) => withTable("cloudflared_profiles", async () => {
        const profiles = get().cloudflaredProfiles.map((p) =>
          p.id === id ? { ...p, ...updates, updatedAt: new Date().toISOString() } : p
        );
        await storage.cloudflaredProfiles.save(profiles);
        set({ cloudflaredProfiles: profiles });
      }),

      deleteCloudflaredProfile: async (id) => withTable("cloudflared_profiles", async () => {
        const profiles = get().cloudflaredProfiles.filter((p) => p.id !== id);
        await storage.cloudflaredProfiles.save(profiles);
        set({ cloudflaredProfiles: profiles });
      }),

      loadSnippets: async () => withTable("snippets", async () => {
        let snippets = await storage.snippets.load();
        const now = new Date().toISOString();
        const seeds = buildSeedSnippets(now);
        if (snippets.length === 0) {
          snippets = seeds;
          await storage.snippets.save(snippets);
        } else {
          const existing = new Set(snippets.map((s) => s.id));
          const missing = seeds.filter((s) => !existing.has(s.id));
          if (missing.length > 0) {
            snippets = [...missing, ...snippets];
            await storage.snippets.save(snippets);
          }
        }
        set({ snippets });
      }),

      addSnippet: async (snippet) => withTable("snippets", async () => {
        const now = new Date().toISOString();
        const next: Snippet = {
          ...snippet,
          id: `snip-${crypto.randomUUID()}`,
          useCount: 0,
          createdAt: now,
          updatedAt: now,
        };
        const snippets = [next, ...get().snippets];
        await storage.snippets.save(snippets);
        set({ snippets });
      }),

      updateSnippet: async (id, updates) => withTable("snippets", async () => {
        const snippets = get().snippets.map((s) =>
          s.id === id ? { ...s, ...updates, updatedAt: new Date().toISOString() } : s
        );
        await storage.snippets.save(snippets);
        set({ snippets });
      }),

      deleteSnippet: async (id) => withTable("snippets", async () => {
        const snippets = get().snippets.filter((s) => s.id !== id);
        await storage.snippets.save(snippets);
        set({ snippets });
      }),

      restoreSnippet: async (snippet) => withTable("snippets", async () => {
        const without = get().snippets.filter((s) => s.id !== snippet.id);
        const snippets = [snippet, ...without];
        await storage.snippets.save(snippets);
        set({ snippets });
      }),

      bumpSnippetUse: async (id) => withTable("snippets", async () => {
        const snippets = get().snippets.map((s) =>
          s.id === id
            ? { ...s, useCount: (s.useCount || 0) + 1, updatedAt: new Date().toISOString() }
            : s
        );
        await storage.snippets.save(snippets);
        set({ snippets });
      }),

      loadQuickAskSessions: async () => withTable("quick_ask_sessions", async () => {
        const sessions = await storage.quickAskSessions.load();
        set({ quickAskSessions: sessions });
      }),

      upsertQuickAskSession: async (session) => withTable("quick_ask_sessions", async () => {
        const MAX_SESSIONS = 30;
        const without = get().quickAskSessions.filter((s) => s.id !== session.id);
        let sessions = [session, ...without];
        if (sessions.length > MAX_SESSIONS) {
          // Drop the stalest beyond the cap (list is newest-first by design).
          sessions = [...sessions]
            .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
            .slice(0, MAX_SESSIONS);
        }
        await storage.quickAskSessions.save(sessions);
        set({ quickAskSessions: sessions });
      }),

      deleteQuickAskSession: async (id) => withTable("quick_ask_sessions", async () => {
        const sessions = get().quickAskSessions.filter((s) => s.id !== id);
        await storage.quickAskSessions.save(sessions);
        set({ quickAskSessions: sessions });
      }),

      // Spread all pure pass-through Tauri invoke wrappers
      ...invocations,

      // DeepSeek Harness cached state
      dshNodejsInstalled: null,
      dshVersion: null,
      dshLatestVersion: null,
      dshStatusChecked: false,
      dshHasUpdate: false,
      _dshStatusPromise: null as Promise<void | null> | null,

      loadDshStatus: async () => {
        if (get().dshStatusChecked) return;
        if (get()._dshStatusPromise) return;

        const promise = (async () => {
          // Fast check — resolves immediately, update UI without waiting
          get()
            .invokeCheckNodejs()
            .then((installed) => set({ dshNodejsInstalled: installed }))
            .catch(() => set({ dshNodejsInstalled: false }));

          // Slow checks run in parallel (npm list + npm view)
          const [versionRes, latestRes] = await Promise.allSettled([
            get().invokeGetDshVersion(),
            get().invokeGetDshLatestVersion(),
          ]);

          const version =
            versionRes.status === "fulfilled" ? versionRes.value || "" : "";
          const latestVersion =
            latestRes.status === "fulfilled" ? latestRes.value || "" : "";
          const hasUpdate =
            version && latestVersion ? isOlderVersion(version, latestVersion) : false;

          set({
            dshVersion: version,
            dshLatestVersion: latestVersion,
            dshHasUpdate: hasUpdate,
            dshStatusChecked: true,
          });
        })();

        set((state) => ({ ...state, _dshStatusPromise: promise }));
        try {
          await promise;
        } finally {
          set((state) => ({ ...state, _dshStatusPromise: null }));
        }
      },

      refreshDshStatus: async () => {
        set((state) => ({ ...state, dshStatusChecked: false }));
        await get().loadDshStatus();
      },

      setCurrentGitRepo: (path) =>
        get().setSettings({ currentGitRepo: path || undefined }),

      // Initialize
      initialize: async () => {
        await Promise.all([
          get().loadAccounts(),
          get().loadRepoConfigs(),
          get().loadRecentProjects(),
          get().loadWorkspaces(),
          get().loadWebPlugins(),
          get().loadHostProfiles(),
          get().loadCursorAccounts(),
          get().loadAIModels(),
          get().loadCloudflaredProfiles(),
          get().loadSnippets(),
          get().loadQuickAskSessions(),
        ]);
      },
    }),
    {
      name: 'workbench-settings',
      storage: createJSONStorage(() => localStorage),
      partialize: (state) => ({ settings: state.settings }),
      merge: (persisted, current) => {
        const p = persisted as {
          settings?: Partial<AppSettings> & { logoVariant?: 'ice' | 'silver' };
        } | undefined;
        return {
          ...current,
          ...p,
          settings: migrateSettings({
            ...DEFAULT_SETTINGS,
            ...current.settings,
            ...p?.settings,
          }),
        };
      },
    }
  )
);
