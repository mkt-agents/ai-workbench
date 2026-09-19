import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Check, Loader2, User, XCircle } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { pathKey, projectNameFromPath } from "../core/pathUtils";
import { readStoredString, writeStoredString } from "../core/localState";
import { getRepoSnapshot, invalidateRepos, refreshRepos, subscribeRepos } from "../core/gitCache";
import { resolveRepoAccount } from "../core/gitIdentity";
import { useConfirm } from "./ConfirmModal";
import AccountManagerModal from "./AccountManagerModal";
import CommitChangelist from "./CommitChangelist";

type Props = {
  active?: boolean;
  onOpenRepos?: () => void;
};

const CL_MODEL_KEY = "workbench-commit-model";

type AccountLike = {
  name?: string;
  userName?: string;
  email: string;
  id?: string;
};

function identityMatches(
  actual: { name: string; email: string },
  preset: { userName: string; email: string }
): boolean {
  return (
    actual.name.trim().toLowerCase() === preset.userName.trim().toLowerCase() &&
    actual.email.trim().toLowerCase() === preset.email.trim().toLowerCase()
  );
}

function resolveAccountName(account: AccountLike): string {
  return (account.name ?? account.userName ?? "").trim();
}

function formatInvokeError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}

function GitCommitPanel({ active = true, onOpenRepos }: Props) {
  const { t } = useTranslation("git");
  const confirm = useConfirm();
  const repoPath = useGlobalStore((s) => s.settings.currentGitRepo);
  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const accounts = useGlobalStore((s) => s.git.accounts);
  const repoConfigs = useGlobalStore((s) => s.git.repoConfigs);
  const loadRecentProjects = useGlobalStore((s) => s.loadRecentProjects);
  const loadAccounts = useGlobalStore((s) => s.loadAccounts);
  const loadRepoConfigs = useGlobalStore((s) => s.loadRepoConfigs);
  const loadHostConfigs = useGlobalStore((s) => s.loadHostConfigs);
  const addAccount = useGlobalStore((s) => s.addAccount);
  const updateRepoConfig = useGlobalStore((s) => s.updateRepoConfig);
  const invokeSetRepoGitConfig = useGlobalStore((s) => s.invokeSetRepoGitConfig);
  const invokeGitUndoLastCommit = useGlobalStore((s) => s.invokeGitUndoLastCommit);
  const aiModels = useGlobalStore((s) => s.aiModels);
  const loadAIModels = useGlobalStore((s) => s.loadAIModels);

  const repoItems = useSyncExternalStore(subscribeRepos, getRepoSnapshot);
  const attemptedHostApply = useRef<Set<string>>(new Set());
  const [switching, setSwitching] = useState(false);
  const [undoing, setUndoing] = useState(false);
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [showAccountsModal, setShowAccountsModal] = useState(false);
  const [toast, setToast] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [selectedModelId, setSelectedModelId] = useState<string>(() =>
    readStoredString(CL_MODEL_KEY)
  );

  const defaultModel = useMemo(
    () => aiModels.find((m) => m.isDefault) || aiModels[0] || null,
    [aiModels]
  );

  const selectedModelProp = useMemo(() => {
    if (selectedModelId) {
      const found = aiModels.find((m) => m.id === selectedModelId);
      if (found) return found;
    }
    return defaultModel;
  }, [selectedModelId, aiModels, defaultModel]);

  useEffect(() => {
    writeStoredString(CL_MODEL_KEY, selectedModelId);
  }, [selectedModelId]);

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setToast({ type, text });
    setTimeout(() => setToast(null), type === "error" ? 8000 : 3000);
  }, []);

  useEffect(() => {
    if (!active) return;
    const state = useGlobalStore.getState();
    if (state.recentProjects.length === 0) loadRecentProjects().catch(() => {});
    if (state.git.accounts.length === 0) loadAccounts().catch(() => {});
    if (state.git.repoConfigs.length === 0) loadRepoConfigs().catch(() => {});
    if (state.git.hostConfigs.length === 0) loadHostConfigs().catch(() => {});
    if (state.aiModels.length === 0) loadAIModels().catch(() => {});
  }, [active, loadAccounts, loadRecentProjects, loadRepoConfigs, loadHostConfigs, loadAIModels]);

  /**
   * Header state (branch, ahead/behind, effective identity) comes from the
   * shared repo cache the changelist already fills — this page used to spawn
   * nine git processes of its own on every mount.
   */
  const summary = useMemo(() => {
    if (!repoPath) return null;
    const direct = repoItems[repoPath];
    if (direct) return direct;
    const key = pathKey(repoPath);
    return Object.values(repoItems).find((item) => pathKey(item.path) === key) ?? null;
  }, [repoItems, repoPath]);

  const author = summary
    ? { name: summary.userName || "", email: summary.userEmail || "" }
    : { name: "", email: "" };

  // Host-level binding still applies here for repos visited only from this page.
  useEffect(() => {
    if (!active || !repoPath || !summary?.originUrl) return;
    const state = useGlobalStore.getState();
    if (state.git.hostConfigs.length === 0) return;
    if (state.git.repoConfigs.some((c) => c.path === repoPath)) return;
    const account = resolveRepoAccount({
      repoPath,
      remoteUrl: summary.originUrl,
      repoConfigs: state.git.repoConfigs,
      hostConfigs: state.git.hostConfigs,
      accounts: state.git.accounts,
    });
    if (!account) return;
    // Remember the attempt so a failed write is not retried on every render.
    const key = `${repoPath}|${account.id}`;
    if (attemptedHostApply.current.has(key)) return;
    attemptedHostApply.current.add(key);
    void invokeSetRepoGitConfig(repoPath, account.name, account.email)
      .then(() => {
        invalidateRepos([repoPath]);
        return refreshRepos([repoPath], { withStatus: true });
      })
      .then(() => {
        showMsg("success", t("repos.autoApplied", { count: 1 }));
      })
      .catch(() => {
        attemptedHostApply.current.delete(key);
      });
  }, [active, repoPath, summary, invokeSetRepoGitConfig, showMsg, t]);

  const currentRepoName = useMemo(() => {
    if (!repoPath) return null;
    const p = recentProjects.find((r) => r.path === repoPath);
    return p?.name || projectNameFromPath(repoPath);
  }, [recentProjects, repoPath]);

  const currentPreset = useMemo(
    () => (repoPath ? repoConfigs.find((c) => c.path === repoPath) : undefined),
    [repoConfigs, repoPath]
  );

  // The changelist already knows the identity mismatch is worth flagging before
  // a commit happens here — no longer gated on the repo being dirty, so a wrong
  // identity is visible the moment the repo opens (same rule as the repo cards).
  const identityMismatch =
    !!currentPreset && !!author.name && !identityMatches(author, currentPreset);

  /**
   * Tip already on the remote: undo becomes an IntelliJ-style revert (a new inverse
   * commit) instead of resetting history, so the button stays usable after a push.
   */
  const undoIsRevert = Boolean(summary?.hasUpstream && summary.ahead === 0);
  const undoBlockedReason = !repoPath
    ? t("commit.changelistPickRepo")
    : summary && !summary.hasCommits
      ? t("commit.undoLastBlockedEmpty")
      : summary && undoIsRevert && summary.dirtyCount > 0
        ? t("commit.undoLastBlockedDirty")
        : null;

  const handleSwitchAccount = async (account: AccountLike) => {
    if (!repoPath || switching) return;
    const name = resolveAccountName(account);
    const email = (account.email || "").trim();
    if (!name || !email) return;

    setSwitching(true);
    try {
      await invokeSetRepoGitConfig(repoPath, name, email);
      // Show what git now reports rather than what we asked for; the cache read
      // is the only proof the write landed.
      invalidateRepos([repoPath]);
      await refreshRepos([repoPath], { withStatus: true });
      if (!accounts.some((a) => a.email.trim().toLowerCase() === email.toLowerCase())) {
        const colors = ["#5f8f72", "#6a9a88", "#7a9068", "#4d7a6c", "#8a9e7a", "#5a8578"];
        await addAccount({
          id: Date.now().toString(),
          name,
          email,
          color: colors[accounts.length % colors.length],
        });
      }
      const existing = useGlobalStore.getState().git.repoConfigs.find((c) => c.path === repoPath);
      if (existing) {
        const matched = useGlobalStore
          .getState()
          .git.accounts.find((a) => a.email.trim().toLowerCase() === email.toLowerCase());
        await updateRepoConfig(repoPath, {
          userName: name,
          email,
          accountId: matched?.id ?? account.id,
        });
      }
      showMsg("success", t("commit.identityApplied"));
    } catch (e) {
      showMsg("error", formatInvokeError(e));
    } finally {
      setSwitching(false);
    }
  };

  const handleUndoLastCommit = async () => {
    if (!repoPath || undoing) return;
    if (undoBlockedReason) {
      showMsg("error", undoBlockedReason);
      return;
    }
    const ok = await confirm({
      title: undoIsRevert ? t("commit.undoLastRevertTitle") : t("commit.undoLastTitle"),
      message: undoIsRevert ? t("commit.undoLastRevertConfirm") : t("commit.undoLastConfirm"),
      warning: undoIsRevert ? t("commit.undoLastRevertWarn") : t("commit.undoLastWarn"),
      confirmText: t("commit.undoLast"),
      icon: "warning",
    });
    if (!ok) return;

    setUndoing(true);
    try {
      await invokeGitUndoLastCommit(repoPath);
      invalidateRepos([repoPath]);
      showMsg("success", t("commit.undoLastOk"));
      setRefreshNonce((n) => n + 1);
    } catch (e) {
      showMsg("error", formatInvokeError(e));
    } finally {
      setUndoing(false);
    }
  };

  if (recentProjects.length === 0) {
    return (
      <div className="page-scrollable runtime-empty">
        <div>{t("commit.noRepo")}</div>
        <div className="runtime-muted">{t("commit.noRepoHint")}</div>
        {onOpenRepos && (
          <button type="button" className="btn btn-primary" style={{ marginTop: 12 }} onClick={onOpenRepos}>
            {t("commit.goRepos")}
          </button>
        )}
      </div>
    );
  }

  return (
    <div className="page-scrollable git-commit-page">
      <div className="commit-toolbar commit-toolbar-compact">
        <div className="commit-meta-row">
          <div className="commit-current-repo" title={repoPath || undefined}>
            {currentRepoName ? (
              <>
                <span className="runtime-badge active">{t("workbench.current")}</span>
                <span className="commit-current-repo-name">{currentRepoName}</span>
              </>
            ) : (
              <span className="runtime-muted">{t("commit.changelistPickRepo")}</span>
            )}
          </div>
          <div className="commit-meta-identity">
            <User size={14} />
            <span title={`${author.name} <${author.email}>`}>
              {t("commit.currentIdentity")}{" "}
              {author.name || "—"}
              {author.email ? ` <${author.email}>` : ""}
            </span>
          </div>
          {accounts.length > 0 && (
            <div className="commit-identity-quick">
              {accounts.map((a) => {
                const isActive =
                  author.name.trim().toLowerCase() === a.name.trim().toLowerCase() &&
                  author.email.trim().toLowerCase() === a.email.trim().toLowerCase();
                return (
                  <button
                    key={a.id}
                    type="button"
                    className={`commit-identity-chip ${isActive ? "is-active" : ""}`}
                    disabled={switching || !repoPath}
                    onClick={() => handleSwitchAccount(a)}
                    title={
                      !repoPath
                        ? t("commit.changelistPickRepo")
                        : isActive
                          ? t("commit.identityInUse")
                          : `${a.name} <${a.email}>`
                    }
                  >
                    <span className="commit-identity-avatar" style={{ background: a.color }}>
                      {a.name.charAt(0).toUpperCase()}
                    </span>
                    <span>{a.name}</span>
                    {isActive && <Check size={11} />}
                  </button>
                );
              })}
            </div>
          )}
          <button
            type="button"
            className="btn commit-icon-btn"
            onClick={() => setShowAccountsModal(true)}
            title={t("commit.manageAccounts")}
          >
            <User size={14} />
            <span>{t("commit.manageAccounts")}</span>
          </button>
        </div>
        {identityMismatch && currentPreset && (
          <div className="repos-mismatch">
            <AlertTriangle size={13} />
            <span className="repos-mismatch-text">
              {t("commit.identityMismatch", {
                preset: `${currentPreset.userName} <${currentPreset.email}>`,
              })}
            </span>
            <button
              type="button"
              className="btn btn-primary btn-small"
              disabled={switching}
              onClick={() => handleSwitchAccount(currentPreset)}
            >
              {switching ? <Loader2 size={12} className="spin" /> : t("repos.applyPreset")}
            </button>
          </div>
        )}
      </div>

      {toast && (
        <div className={`toast toast-${toast.type}`} role="status">
          <span className="toast-icon">
            {toast.type === "success" ? <Check size={14} /> : <XCircle size={14} />}
          </span>
          <span className="toast-text">{toast.text}</span>
        </div>
      )}

      <CommitChangelist
        active={active}
        onToast={showMsg}
        refreshNonce={refreshNonce}
        onUndoLastCommit={repoPath ? () => void handleUndoLastCommit() : undefined}
        undoDisabled={!!undoBlockedReason}
        undoTitle={undoBlockedReason ?? (undoIsRevert ? t("commit.undoLastRevertHint") : t("commit.undoLast"))}
        undoing={undoing}
        selectedModel={selectedModelProp}
        selectedModelId={selectedModelId}
        onModelChange={setSelectedModelId}
      />

      {showAccountsModal && (
        <AccountManagerModal
          onClose={() => {
            setShowAccountsModal(false);
            void loadAccounts().catch(() => {});
          }}
        />
      )}
    </div>
  );
}

export default GitCommitPanel;
