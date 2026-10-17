import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import GitReposPage from "./GitReposPage";
import GitCommitPanel from "./GitCommitPanel";
import GitReportsPage from "./GitReportsPage";
import type { ReportTarget } from "./testReportTypes";

type GitSubTab = "repos" | "commit" | "report";

/** Quick Ask writes this, then opens the Git tab. Consumed once. */
export const GIT_SUBTAB_KEY = "workbench-git-subtab";

function readGitSubtab(value: string | null): GitSubTab | null {
  if (value === "repos" || value === "commit" || value === "report") return value;
  return null;
}

const SUB_TABS: { id: GitSubTab; labelKey: string }[] = [
  { id: "repos", labelKey: "tabs.repos" },
  { id: "commit", labelKey: "tabs.commit" },
  { id: "report", labelKey: "tabs.report" },
];

type Props = {
  /** Sidebar Git tab is visible */
  active?: boolean;
};

function GitManager({ active = true }: Props) {
  const { t } = useTranslation("git");
  const [subTab, setSubTab] = useState<GitSubTab>("repos");

  useEffect(() => {
    const apply = () => {
      const pending = readGitSubtab(localStorage.getItem(GIT_SUBTAB_KEY));
      if (!pending) return;
      setSubTab(pending);
      localStorage.removeItem(GIT_SUBTAB_KEY);
    };
    const onStorage = (event: StorageEvent) => {
      if (event.key !== GIT_SUBTAB_KEY) return;
      apply();
    };
    apply();
    window.addEventListener("storage", onStorage);
    window.addEventListener("workbench-git-subtab", apply);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("workbench-git-subtab", apply);
    };
  }, []);
  // Which repo/folder the report panel is pointed at. Lives here (not in the
  // repos page) so a card/group-header jump and a tree click drive one source.
  const [reportTarget, setReportTarget] = useState<ReportTarget | null>(null);

  return (
    <div className="git-manager">
      <div className="git-manager-column is-wide">
        <div className="git-subnav" role="tablist" aria-label={t("tabs.repos")}>
          {SUB_TABS.map((tab) => (
            <button
              key={tab.id}
              type="button"
              role="tab"
              aria-selected={subTab === tab.id}
              className={`git-subnav-item ${subTab === tab.id ? "active" : ""}`}
              onClick={() => setSubTab(tab.id)}
            >
              {t(tab.labelKey)}
            </button>
          ))}
        </div>
        <div className="git-manager-body" role="tabpanel">
          <div className={subTab === "repos" ? "git-tab-panel is-active" : "git-tab-panel"}>
            <GitReposPage
              active={active && subTab === "repos"}
              onOpenCommit={() => setSubTab("commit")}
              onOpenReport={(target) => {
                setReportTarget(target);
                setSubTab("report");
              }}
            />
          </div>
          <div className={subTab === "commit" ? "git-tab-panel is-active" : "git-tab-panel"}>
            <GitCommitPanel
              active={active && subTab === "commit"}
              onOpenRepos={() => setSubTab("repos")}
            />
          </div>
          <div className={subTab === "report" ? "git-tab-panel is-active" : "git-tab-panel"}>
            <GitReportsPage
              active={active && subTab === "report"}
              target={reportTarget}
              onTargetChange={setReportTarget}
            />
          </div>
        </div>
      </div>
    </div>
  );
}

export default GitManager;
