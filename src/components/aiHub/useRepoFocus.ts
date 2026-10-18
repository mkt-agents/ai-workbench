import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalStore } from "../../core/store";
import type { GitScannedRepo } from "../../core/types";
import { projectNameFromPath } from "../../core/pathUtils";
import {
  buildPickerGroups,
  describeSelection,
  resolveFocus,
  toggleGroup,
  toggleService,
} from "./repoSelection";
import type { PickerGroup } from "./repoSelection";

/**
 * Which repos / services the AI hub analyzes: persisted selection, folder-scan
 * candidates, picker groups and the labels shown in the composer.
 */
export function useRepoFocus(opts: { onError: (msg: string) => void }) {
  const { t } = useTranslation("aihub");
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);
  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const gitWorkspaces = useGlobalStore((s) => s.gitWorkspaces);
  const invokeGitScanRepos = useGlobalStore((s) => s.invokeGitScanRepos);
  const invokeGitIsRepo = useGlobalStore((s) => s.invokeGitIsRepo);
  const invokePickDirectory = useGlobalStore((s) => s.invokePickDirectory);

  /** Workspace tokens (`ws:<id>`) and/or service paths; empty = auto (current repo). */
  const [focusedRepos, setFocusedRepos] = useState<string[]>(settings.aiHubFocusRepos ?? []);
  /** Extra candidates pulled in by a folder scan (session-only). */
  const [extraRepos, setExtraRepos] = useState<GitScannedRepo[]>([]);
  const [scanning, setScanning] = useState(false);

  const repoGroups = useMemo(
    () =>
      buildPickerGroups(gitWorkspaces, recentProjects, extraRepos, settings.currentGitRepo, focusedRepos),
    [gitWorkspaces, recentProjects, extraRepos, settings.currentGitRepo, focusedRepos],
  );
  const resolvedFocus = useMemo(() => resolveFocus(focusedRepos, repoGroups), [focusedRepos, repoGroups]);

  const commitFocus = (next: string[]) => {
    setFocusedRepos(next);
    setSettings({ aiHubFocusRepos: next });
  };
  const onToggleGroup = (group: PickerGroup) => commitFocus(toggleGroup(focusedRepos, group));
  const onToggleService = (group: PickerGroup, path: string) =>
    commitFocus(toggleService(focusedRepos, group, path));
  const clearFocus = () => commitFocus([]);

  /** Pick a folder, scan it for git repos, add them as candidates and select all. */
  const scanFolder = async () => {
    if (scanning) return;
    setScanning(true);
    try {
      const dir = await invokePickDirectory();
      if (!dir) return;
      let found: GitScannedRepo[];
      if (await invokeGitIsRepo(dir)) {
        found = [{ path: dir, name: projectNameFromPath(dir) }];
      } else {
        found = await invokeGitScanRepos(dir, 2);
      }
      if (found.length === 0) {
        opts.onError(t("focusScanEmpty"));
        return;
      }
      setExtraRepos((prev) => {
        const seen = new Set(prev.map((r) => r.path));
        return [...prev, ...found.filter((r) => !seen.has(r.path))];
      });
      // Select every repo just discovered (union with existing selection).
      commitFocus(Array.from(new Set([...focusedRepos, ...found.map((r) => r.path)])));
    } catch (e) {
      opts.onError(String(e));
    } finally {
      setScanning(false);
    }
  };

  const autoRepoName = settings.currentGitRepo ? projectNameFromPath(settings.currentGitRepo) : null;
  /** Selection label for the composer pill in project mode; null = nothing to analyze. */
  const projectLabel = useMemo(
    () => describeSelection(focusedRepos, repoGroups, autoRepoName, (k, p) => t(k, p)),
    [focusedRepos, repoGroups, autoRepoName, t],
  );
  const pickerLabel =
    focusedRepos.length === 0 || !projectLabel
      ? autoRepoName
        ? t("ctx.autoRepo", { name: autoRepoName })
        : t("ctx.noRepo")
      : projectLabel;

  return {
    focusedRepos,
    repoGroups,
    resolvedFocus,
    scanning,
    onToggleGroup,
    onToggleService,
    clearFocus,
    scanFolder,
    projectLabel,
    pickerLabel,
  };
}
