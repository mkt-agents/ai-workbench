import { useState } from "react";
import { useTranslation } from "react-i18next";
import GitReposPage from "./GitReposPage";
import GitCommitPanel from "./GitCommitPanel";

type GitSubTab = "repos" | "commit";

const SUB_TABS: { id: GitSubTab; labelKey: string }[] = [
  { id: "repos", labelKey: "tabs.repos" },
  { id: "commit", labelKey: "tabs.commit" },
];

type Props = {
  /** Sidebar Git tab is visible */
  active?: boolean;
};

function GitManager({ active = true }: Props) {
  const { t } = useTranslation("git");
  const [subTab, setSubTab] = useState<GitSubTab>("repos");

  return (
    <div className="git-manager">
      <div className="git-manager-column">
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
            />
          </div>
          <div className={subTab === "commit" ? "git-tab-panel is-active" : "git-tab-panel"}>
            <GitCommitPanel
              active={active && subTab === "commit"}
              onOpenRepos={() => setSubTab("repos")}
            />
          </div>
        </div>
      </div>
    </div>
  );
}

export default GitManager;
