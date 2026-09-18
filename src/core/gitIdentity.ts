import type { GitAccount, GitHostConfig, GitRepoConfig } from './types';

/**
 * Extract the hostname from a git remote URL.
 * Mirrors the Rust `extract_host` in git_commands.rs.
 * Handles HTTPS, SSH (`git@host:path`), and `ssh://` forms.
 */
export function extractHost(url: string): string | null {
  const trimmed = url.trim();
  if (!trimmed) return null;

  // URL with scheme (https://, ssh://, http://)
  if (trimmed.includes('://')) {
    try {
      const parsed = new URL(trimmed);
      const host = parsed.hostname; // URL API strips brackets and port
      return host ? host.toLowerCase() : null;
    } catch {
      return null;
    }
  }

  // SCP-like SSH: [user@]host:path
  const atIdx = trimmed.indexOf('@');
  const after = atIdx >= 0 ? trimmed.slice(atIdx + 1) : trimmed;
  const colonIdx = after.indexOf(':');
  const slashIdx = after.indexOf('/');
  let hostEnd: number;
  if (colonIdx >= 0 && slashIdx >= 0) {
    hostEnd = Math.min(colonIdx, slashIdx);
  } else if (colonIdx >= 0) {
    hostEnd = colonIdx;
  } else if (slashIdx >= 0) {
    hostEnd = slashIdx;
  } else {
    return null;
  }

  const host = after.slice(0, hostEnd);
  if (!host) return null;
  return host.toLowerCase();
}

/**
 * Resolve the account to use for a repo based on:
 * 1. Path-level binding (highest priority)
 * 2. Host/domain-level binding
 * 3. null (caller falls back to git config)
 */
export function resolveRepoAccount(params: {
  repoPath: string;
  remoteUrl: string | null;
  repoConfigs: GitRepoConfig[];
  hostConfigs: GitHostConfig[];
  accounts: GitAccount[];
}): GitAccount | null {
  const { repoPath, remoteUrl, repoConfigs, hostConfigs, accounts } = params;

  // 1. Exact path match
  const repo = repoConfigs.find((c) => c.path === repoPath);
  if (repo?.accountId) {
    const found = accounts.find((a) => a.id === repo.accountId);
    if (found) return found;
  }

  // 2. Host match
  if (remoteUrl) {
    const host = extractHost(remoteUrl);
    if (host) {
      const hc = hostConfigs.find((h) => h.host.toLowerCase() === host);
      if (hc) {
        const found = accounts.find((a) => a.id === hc.accountId);
        if (found) return found;
      }
    }
  }

  // 3. Fallback
  return null;
}
