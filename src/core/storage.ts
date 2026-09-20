/**
 * SQLite storage adapter — routes through typed Tauri db_load / db_save commands.
 */
import type { GitAccount, GitRepoConfig, GitHostConfig, HostProfile, WebPlugin, UserScript, RecentProject, CursorAccount, AIModelConfig, CloudflaredNamedProfile, GitWorkspace, Snippet, QuickAskSession, QuickAskTurn, JsonToolHistoryItem } from './types';

function finiteOr(value: unknown, fallback: number): number {
  if (value === null || value === undefined || value === "") return fallback;
  const n = typeof value === "number" ? value : Number(value);
  return Number.isFinite(n) ? n : fallback;
}

/** Parse the JSON-encoded turns column; corrupt rows degrade to empty turns. */
function parseTurns(raw: unknown): QuickAskTurn[] {
  if (typeof raw !== 'string' || !raw.trim()) return [];
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (t): t is QuickAskTurn =>
        !!t && typeof t === 'object' &&
        (t.role === 'user' || t.role === 'assistant') &&
        typeof t.content === 'string'
    );
  } catch {
    return [];
  }
}

type DbTable =
  | 'git_accounts'
  | 'git_repo_configs'
  | 'git_host_configs'
  | 'host_profiles'
  | 'web_plugins'
  | 'user_scripts'
  | 'plugin_states'
  | 'recent_projects'
  | 'git_workspaces'
  | 'cursor_accounts'
  | 'ai_models'
  | 'cloudflared_profiles'
  | 'snippets'
  | 'quick_ask_sessions'
  | 'json_tool_history';

async function loadRows(table: DbTable): Promise<Record<string, unknown>[]> {
  const { invoke } = await import('@tauri-apps/api/core');
  const rows = await invoke<{ columns: Record<string, unknown> }[]>('db_load', { table });
  return rows.map((r) => r.columns ?? (r as unknown as Record<string, unknown>));
}

async function saveRows(table: DbTable, rows: Record<string, unknown>[]): Promise<void> {
  const { invoke } = await import('@tauri-apps/api/core');
  await invoke<void>('db_save', { table, rows });
}

export const storage = {
  accounts: {
    load: async (): Promise<GitAccount[]> => {
      const rows = await loadRows('git_accounts');
      return rows.map(r => ({
        id: r['id'] as string,
        name: r['name'] as string,
        email: r['email'] as string,
        color: r['color'] as string,
        note: (r['note'] as string | undefined) || undefined,
        createdAt: r['created_at'] as string,
        updatedAt: r['updated_at'] as string,
      }));
    },
    save: async (items: GitAccount[]): Promise<void> => {
      await saveRows('git_accounts', items.map(item => ({
        id: item.id,
        name: item.name,
        email: item.email,
        color: item.color,
        note: item.note?.trim() ? item.note.trim() : null,
        created_at: item.createdAt,
        updated_at: item.updatedAt,
      })));
    },
  },
  repoConfigs: {
    load: async (): Promise<GitRepoConfig[]> => {
      const rows = await loadRows('git_repo_configs');
      return rows.map(r => ({
        path: r['path'] as string,
        name: r['name'] as string,
        userName: r['user_name'] as string,
        email: r['email'] as string,
        accountId: r['account_id'] as string | undefined,
        createdAt: r['created_at'] as string,
        updatedAt: r['updated_at'] as string,
      }));
    },
    save: async (items: GitRepoConfig[]): Promise<void> => {
      await saveRows('git_repo_configs', items.map(item => ({
        path: item.path,
        name: item.name,
        user_name: item.userName,
        email: item.email,
        account_id: item.accountId ?? null,
        created_at: item.createdAt,
        updated_at: item.updatedAt,
      })));
    },
  },
  hostConfigs: {
    load: async (): Promise<GitHostConfig[]> => {
      const rows = await loadRows('git_host_configs');
      return rows.map(r => ({
        id: r['id'] as string,
        host: r['host'] as string,
        accountId: r['account_id'] as string,
        note: (r['note'] as string | undefined) || undefined,
        createdAt: r['created_at'] as string,
        updatedAt: r['updated_at'] as string,
      }));
    },
    save: async (items: GitHostConfig[]): Promise<void> => {
      await saveRows('git_host_configs', items.map(item => ({
        id: item.id,
        host: item.host,
        account_id: item.accountId,
        note: item.note?.trim() ? item.note.trim() : null,
        created_at: item.createdAt,
        updated_at: item.updatedAt,
      })));
    },
  },
  hostProfiles: {
    load: async (): Promise<HostProfile[]> => {
      const rows = await loadRows('host_profiles');
      return rows.map(r => ({
        id: r['id'] as string,
        name: r['name'] as string,
        content: r['content'] as string,
        createdAt: r['created_at'] as string,
        updatedAt: r['updated_at'] as string,
      }));
    },
    save: async (items: HostProfile[]): Promise<void> => {
      await saveRows('host_profiles', items.map(item => ({
        id: item.id,
        name: item.name,
        content: item.content,
        created_at: item.createdAt,
        updated_at: item.updatedAt,
      })));
    },
  },
  userScripts: {
    load: async (): Promise<UserScript[]> => {
      const rows = await loadRows('user_scripts');
      return rows.map(r => {
        let patterns: string[] = [];
        try {
          const raw = r['match_patterns'];
          if (raw) {
            const parsed = JSON.parse(raw as string);
            if (Array.isArray(parsed)) patterns = parsed.filter(Boolean);
          }
        } catch {
          // 兼容旧数据：尝试从 match_pattern 读取
          const old = (r['match_pattern'] as string) || '';
          if (old) patterns = [old];
        }
        if (patterns.length === 0) patterns = ['<all_urls>'];
        return {
          id: r['id'] as string,
          name: r['name'] as string,
          description: (r['description'] as string) || '',
          matchPatterns: patterns,
          code: (r['code'] as string) || '',
          enabled: Boolean(r['enabled']),
          createdAt: (r['created_at'] as string) || '',
          updatedAt: (r['updated_at'] as string) || '',
        };
      });
    },
    save: async (items: UserScript[]): Promise<void> => {
      await saveRows('user_scripts', items.map(item => ({
        id: item.id,
        name: item.name,
        description: item.description || '',
        match_patterns: JSON.stringify(item.matchPatterns.length > 0 ? item.matchPatterns : ['<all_urls>']),
        match_pattern: item.matchPatterns[0] || '<all_urls>', // 兼容旧版读取
        code: item.code,
        enabled: item.enabled ? 1 : 0,
        created_at: item.createdAt,
        updated_at: item.updatedAt,
      })));
    },
  },
  webPlugins: {
    load: async (): Promise<WebPlugin[]> => {
      const rows = await loadRows('web_plugins');
      return rows.map(r => ({
        id: r['id'] as string,
        name: r['name'] as string,
        url: r['url'] as string,
        addedAt: r['added_at'] as string,
        group: (r['group'] as string) || '',
        tags: (r['tags'] as string) || '',
        order: (r['order'] as number) || 0,
        lastOpenedAt: (r['last_opened_at'] as string) || '',
        openCount: (r['open_count'] as number) || 0,
        hotkey: (r['hotkey'] as string) || '',
        isPreset: Boolean(r['is_preset']),
      }));
    },
    save: async (items: WebPlugin[]): Promise<void> => {
      await saveRows('web_plugins', items.map(item => ({
        id: item.id,
        name: item.name,
        url: item.url,
        local_path: null,
        downloaded_at: null,
        added_at: item.addedAt,
        group: item.group || '',
        tags: item.tags || '',
        order: item.order,
        last_opened_at: item.lastOpenedAt || null,
        open_count: item.openCount,
        hotkey: item.hotkey || '',
        is_preset: item.isPreset ? 1 : 0,
      })));
    },
  },
  recentProjects: {
    load: async (): Promise<RecentProject[]> => {
      const rows = await loadRows('recent_projects');
      return rows.map(r => ({
        id: r['id'] as number,
        path: r['path'] as string,
        name: r['name'] as string | undefined,
        lastOpenedAt: r['last_opened_at'] as string,
      }));
    },
    save: async (items: RecentProject[]): Promise<void> => {
      await saveRows('recent_projects', items.map(item => ({
        id: item.id,
        path: item.path,
        name: item.name ?? null,
        last_opened_at: item.lastOpenedAt,
      })));
    },
  },
  gitWorkspaces: {
    load: async (): Promise<GitWorkspace[]> => {
      const rows = await loadRows('git_workspaces');
      return rows.map(r => ({
        id: r['id'] as number,
        path: r['path'] as string,
        name: r['name'] as string,
        createdAt: r['created_at'] as string,
      }));
    },
    save: async (items: GitWorkspace[]): Promise<void> => {
      await saveRows('git_workspaces', items.map(item => ({
        id: item.id,
        path: item.path,
        name: item.name,
        created_at: item.createdAt,
      })));
    },
  },
  cursorAccounts: {
    load: async (): Promise<CursorAccount[]> => {
      const rows = await loadRows('cursor_accounts');
      return rows.map(r => ({
        id: r['id'] as string,
        name: r['name'] as string,
        email: r['email'] as string,
        color: r['color'] as string,
        backupPath: r['backup_path'] as string,
        profileDir: (r['profile_dir'] as string | undefined) || undefined,
        profileInitialized: ((r['profile_initialized'] as number | undefined) ?? 0) === 1,
        gitUserName: r['git_user_name'] as string | undefined,
        gitEmail: r['git_email'] as string | undefined,
        password: (r['password'] as string | undefined) || undefined,
        notes: r['notes'] as string | undefined,
        isLoggedIn: (r['is_logged_in'] as number) === 1,
        createdAt: r['created_at'] as string,
        updatedAt: r['updated_at'] as string,
      }));
    },
    save: async (items: CursorAccount[]): Promise<void> => {
      await saveRows('cursor_accounts', items.map(item => ({
        id: item.id,
        name: item.name,
        email: item.email,
        color: item.color,
        backup_path: item.backupPath,
        profile_dir: item.profileDir ?? null,
        profile_initialized: item.profileInitialized ? 1 : 0,
        git_user_name: item.gitUserName ?? null,
        git_email: item.gitEmail ?? null,
        password: item.password ?? null,
        notes: item.notes ?? null,
        is_logged_in: item.isLoggedIn ? 1 : 0,
        created_at: item.createdAt,
        updated_at: item.updatedAt,
      })));
    },
  },
  aiModels: {
    load: async (): Promise<AIModelConfig[]> => {
      const rows = await loadRows('ai_models');
      return rows.map(r => ({
        id: r['id'] as string,
        name: r['name'] as string,
        provider: (r['provider'] as AIModelConfig['provider']) || "openai",
        apiKey: (r['api_key'] as string) || "",
        baseUrl: (r['base_url'] as string) || "",
        model: (r['model'] as string) || "",
        authType: (r['auth_type'] as AIModelConfig['authType']) || "api",
        temperature: finiteOr(r['temperature'], 0.7),
        maxTokens: finiteOr(r['max_tokens'], 4096),
        isDefault: ((r['is_default'] as number) || 0) === 1,
        createdAt: (r['created_at'] as string) || "",
        updatedAt: (r['updated_at'] as string) || "",
        lastTest:
          r['last_test_ok'] === null || r['last_test_ok'] === undefined
            ? null
            : {
                ok: (r['last_test_ok'] as number) === 1,
                at: (r['last_test_at'] as string) || "",
                message: (r['last_test_msg'] as string) || "",
              },
      }));
    },
    save: async (items: AIModelConfig[]): Promise<void> => {
      await saveRows('ai_models', items.map(item => ({
        id: item.id,
        name: item.name,
        provider: item.provider,
        api_key: item.apiKey,
        auth_type: item.authType || "api",
        base_url: item.baseUrl,
        model: item.model,
        temperature: item.temperature,
        max_tokens: item.maxTokens,
        is_default: item.isDefault ? 1 : 0,
        last_test_ok: item.lastTest ? (item.lastTest.ok ? 1 : 0) : null,
        last_test_at: item.lastTest?.at ?? null,
        last_test_msg: item.lastTest?.message ?? null,
        created_at: item.createdAt,
        updated_at: item.updatedAt,
      })));
    },
  },
  cloudflaredProfiles: {
    load: async (): Promise<CloudflaredNamedProfile[]> => {
      const rows = await loadRows('cloudflared_profiles');
      return rows.map((r) => {
        const authMode =
          (r['auth_mode'] as string) === 'config' ? 'config' : 'token';
        const token = ((r['token'] as string) || '').trim();
        const configPath = ((r['config_path'] as string) || '').trim();
        return {
          id: r['id'] as string,
          name: r['name'] as string,
          hostname: r['hostname'] as string,
          localUrl: (r['local_url'] as string) || '',
          authMode,
          token: token || undefined,
          configPath: configPath || undefined,
          createdAt: r['created_at'] as string,
          updatedAt: r['updated_at'] as string,
        };
      });
    },
    save: async (items: CloudflaredNamedProfile[]): Promise<void> => {
      await saveRows(
        'cloudflared_profiles',
        items.map((item) => ({
          id: item.id,
          name: item.name,
          hostname: item.hostname,
          local_url: item.localUrl,
          token: item.token?.trim() ? item.token.trim() : "",
          auth_mode: item.authMode || 'token',
          config_path: item.configPath?.trim() ? item.configPath.trim() : null,
          created_at: item.createdAt,
          updated_at: item.updatedAt,
        }))
      );
    },
  },
  snippets: {
    load: async (): Promise<Snippet[]> => {
      const rows = await loadRows('snippets');
      return rows.map((r) => ({
        id: r['id'] as string,
        name: r['name'] as string,
        content: r['content'] as string,
        tags: (r['tags'] as string) || '',
        params: (r['params'] as string) || '',
        useCount: Number(r['use_count'] ?? 0),
        createdAt: r['created_at'] as string,
        updatedAt: r['updated_at'] as string,
      }));
    },
    save: async (items: Snippet[]): Promise<void> => {
      await saveRows(
        'snippets',
        items.map((item) => ({
          id: item.id,
          name: item.name,
          content: item.content,
          tags: item.tags || '',
          params: item.params || '',
          use_count: item.useCount ?? 0,
          created_at: item.createdAt,
          updated_at: item.updatedAt,
        }))
      );
    },
  },
  quickAskSessions: {
    load: async (): Promise<QuickAskSession[]> => {
      const rows = await loadRows('quick_ask_sessions');
      return rows.map((r) => ({
        id: r['id'] as string,
        title: (r['title'] as string) || '',
        task: (r['task'] as string) || 'none',
        turns: parseTurns(r['turns']),
        createdAt: r['created_at'] as string,
        updatedAt: r['updated_at'] as string,
      }));
    },
    save: async (items: QuickAskSession[]): Promise<void> => {
      await saveRows(
        'quick_ask_sessions',
        items.map((item) => ({
          id: item.id,
          title: item.title,
          task: item.task || 'none',
          turns: JSON.stringify(item.turns ?? []),
          created_at: item.createdAt,
          updated_at: item.updatedAt,
        }))
      );
    },
  },
  jsonToolHistory: {
    load: async (): Promise<JsonToolHistoryItem[]> => {
      const rows = await loadRows('json_tool_history');
      return rows.map((r) => ({
        id: Number(r['id'] ?? 0),
        timestamp: Number(r['created_at'] ?? 0),
        input: (r['input'] as string) || '',
        output: (r['output'] as string) || '',
        path: (r['path'] as string) || '',
        ok: ((r['ok'] as number) || 0) === 1,
        nodes: Number(r['nodes'] ?? 0),
        chars: Number(r['chars'] ?? 0),
      }));
    },
    save: async (items: JsonToolHistoryItem[]): Promise<void> => {
      await saveRows(
        'json_tool_history',
        items.map((item) => ({
          input: item.input,
          output: item.output,
          path: item.path || '',
          ok: item.ok ? 1 : 0,
          nodes: item.nodes ?? 0,
          chars: item.chars ?? 0,
          created_at: new Date(item.timestamp).toISOString(),
        }))
      );
    },
  },
};
