import { useState } from "react";
import { useTranslation } from "react-i18next";
import GitReposPage from "./GitReposPage";
import GitCommitPanel from "./GitCommitPanel";
import GitReportsPage from "./GitReportsPage";
import type { ReportTarget } from "./testReportTypes";

type GitSubTab = "repos" | "commit" | "report";

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
