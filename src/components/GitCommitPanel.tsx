import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Check, Loader2, User, XCircle } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { projectNameFromPath } from "../core/pathUtils";
import { useConfirm } from "./ConfirmModal";
import AccountManagerModal from "./AccountManagerModal";
import CommitChangelist from "./CommitChangelist";
import type { GitRepoSummary } from "../core/types";

type Props = {
  active?: boolean;
  onOpenRepos?: () => void;
};

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
  const addAccount = useGlobalStore((s) => s.addAccount);
  const updateRepoConfig = useGlobalStore((s) => s.updateRepoConfig);
  const invokeGetRepoGitConfig = useGlobalStore((s) => s.invokeGetRepoGitConfig);
  const invokeSetRepoGitConfig = useGlobalStore((s) => s.invokeSetRepoGitConfig);
  const invokeGitRepoSummary = useGlobalStore((s) => s.invokeGitRepoSummary);
  const invokeGitUndoLastCommit = useGlobalStore((s) => s.invokeGitUndoLastCommit);

  const [author, setAuthor] = useState({ name: "", email: "" });
  const [switching, setSwitching] = useState(false);
  const [undoing, setUndoing] = useState(false);
  const [summary, setSummary] = useState<GitRepoSummary | null>(null);
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [headerNonce, setHeaderNonce] = useState(0);
  const [showAccountsModal, setShowAccountsModal] = useState(false);
  const [dirtyScopePaths, setDirtyScopePaths] = useState<string[]>([]);
  const [toast, setToast] = useState<{ type: "success" | "error"; text: string } | null>(null);

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
  }, [active, loadAccounts, loadRecentProjects, loadRepoConfigs]);

  useEffect(() => {
    if (!active || !repoPath) {
      setAuthor({ name: "", email: "" });
      setSummary(null);
      return;
    }
    let cancelled = false;
    invokeGetRepoGitConfig(repoPath)
      .then(([name, email]) => {
        if (!cancelled) setAuthor({ name: name || "", email: email || "" });
      })
      .catch(() => {
        if (!cancelled) setAuthor({ name: "", email: "" });
      });
    invokeGitRepoSummary(repoPath)
      .then((s) => {
        if (!cancelled) setSummary(s);
      })
      .catch(() => {
        if (!cancelled) setSummary(null);
      });
    return () => {
      cancelled = true;
    };
  }, [active, invokeGetRepoGitConfig, invokeGitRepoSummary, repoPath, refreshNonce, headerNonce]);

  const currentRepoName = useMemo(() => {
    if (!repoPath) return null;
    const p = recentProjects.find((r) => r.path === repoPath);
    return p?.name || projectNameFromPath(repoPath);
  }, [recentProjects, repoPath]);

  const currentPreset = useMemo(
    () => (repoPath ? repoConfigs.find((c) => c.path === repoPath) : undefined),
    [repoConfigs, repoPath]
  );

  const identityMismatch =
    !!currentPreset &&
    !!author.name &&
    !!repoPath &&
    dirtyScopePaths.includes(repoPath) &&
    !identityMatches(author, currentPreset);

  /**
   * Tip already on the remote: undo becomes an IntelliJ-style revert (a new inverse
   * commit) instead of resetting history, so the button stays usable after a push.
   */
  const undoIsRevert = Boolean(summary?.hasUpstream && summary.ahead === 0);

  const onDirtyScopeChange = useCallback((paths: string[]) => {
    setDirtyScopePaths(paths);
  }, []);

  const onChangelistRefreshed = useCallback(() => {
    setHeaderNonce((n) => n + 1);
  }, []);

  const handleSwitchAccount = async (account: AccountLike) => {
    if (!repoPath || switching) return;
    const name = resolveAccountName(account);
    const email = (account.email || "").trim();
    if (!name || !email) return;

    setSwitching(true);
    try {
      await invokeSetRepoGitConfig(repoPath, name, email);
      if (!accounts.some((a) => a.email.trim().toLowerCase() === email.toLowerCase())) {
        const colors = ["#5f8f72", "#6a9a88", "#7a9068", "#4d7a6c", "#8a9e7a", "#5a8578"];
        await addAccount({
          id: Date.now().toString(),
          name,
          email,
          color: colors[accounts.length % colors.length],
        }).catch(() => {});
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
        }).catch(() => {});
      }
      setAuthor({ name, email });
      showMsg("success", t("commit.identityApplied"));
    } catch (e) {
      showMsg("error", String(e));
    } finally {
      setSwitching(false);
    }
  };

  const handleUndoLastCommit = async () => {
    if (!repoPath || undoing) return;
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
          {accounts.length > 0 ? (
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
          ) : (
            <div className="commit-meta-identity">
              <User size={14} />
              <span title={`${author.name} <${author.email}>`}>
                {author.name || "—"}
                {author.email ? ` <${author.email}>` : ""}
              </span>
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
        onDirtyScopeChange={onDirtyScopeChange}
        refreshNonce={refreshNonce}
        onRefreshed={onChangelistRefreshed}
        onUndoLastCommit={repoPath ? () => void handleUndoLastCommit() : undefined}
        undoDisabled={!repoPath}
        undoTitle={undoIsRevert ? t("commit.undoLastRevertHint") : t("commit.undoLast")}
        undoing={undoing}
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
