import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useGlobalStore } from "../../core/store";
import type { GitScannedRepo } from "../../core/types";
import { pathKey, projectNameFromPath } from "../../core/pathUtils";
import {
  agentTarget,
  buildPickerGroups,
  describeSelection,
  keepOnePlace,
  resolveFocus,
  spansPlaces,
  toggleGroup,
  toggleService,
} from "./repoSelection";
import type { ExtraFolder, PickerGroup } from "./repoSelection";

/**
 * Which workspaces / services the AI hub works on: persisted selection,
 * folders added this session, picker groups, the agent's working directory and
 * the labels shown in the composer.
 *
 * `single`: edit levels work in one workspace (or one standalone repo) at a time.
 */
export function useRepoFocus(opts: { single: boolean; onError: (msg: string) => void; onNotice: (msg: string) => void }) {
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
  /** A single repo added with「添加文件夹」(session-only). */
  const [extraRepos, setExtraRepos] = useState<GitScannedRepo[]>([]);
  /** A folder of repos added with「添加文件夹」: shown as a workspace (session-only). */
  const [extraFolders, setExtraFolders] = useState<ExtraFolder[]>([]);
  const [scanning, setScanning] = useState(false);

  const repoGroups = useMemo(
    () =>
      buildPickerGroups(
        gitWorkspaces,
        recentProjects,
        extraRepos,
        extraFolders,
        settings.currentGitRepo,
        focusedRepos,
      ),
    [gitWorkspaces, recentProjects, extraRepos, extraFolders, settings.currentGitRepo, focusedRepos],
  );
  const resolvedFocus = useMemo(() => resolveFocus(focusedRepos, repoGroups), [focusedRepos, repoGroups]);
  const target = useMemo(
    () => agentTarget(focusedRepos, repoGroups, settings.currentGitRepo),
    [focusedRepos, repoGroups, settings.currentGitRepo],
  );

  const commitFocus = (next: string[]) => {
    setFocusedRepos(next);
    setSettings({ aiHubFocusRepos: next });
  };
  const limit = (next: string[], touched: string) =>
    opts.single ? keepOnePlace(next, repoGroups, touched) : next;
  const onToggleGroup = (group: PickerGroup) =>
    commitFocus(limit(toggleGroup(focusedRepos, group), group.token ?? group.repos[0]?.path ?? ""));
  const onToggleService = (group: PickerGroup, path: string) =>
    commitFocus(limit(toggleService(focusedRepos, group, path), path));
  const clearFocus = () => commitFocus([]);

  // Switching to an edit level with several workspaces picked: keep the first one.
  useEffect(() => {
    if (!opts.single || !spansPlaces(focusedRepos, repoGroups)) return;
    const next = keepOnePlace(focusedRepos, repoGroups);
    commitFocus(next);
    const label = describeSelection(next, repoGroups, null, (k, p) => t(k, p));
    if (label) opts.onNotice(t("ctx.keptOne", { name: label }));
    // Only when the level flips; toggles already go through `limit`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opts.single]);

  /**
   * Pick a folder and select it: a repo is added as a standalone service, a
   * folder of repos becomes a workspace (a saved one is reused).
   */
  const scanFolder = async () => {
    if (scanning) return;
    setScanning(true);
    try {
      const dir = await invokePickDirectory();
      if (!dir) return;
      if (await invokeGitIsRepo(dir)) {
        setExtraRepos((prev) =>
          prev.some((r) => pathKey(r.path) === pathKey(dir))
            ? prev
            : [...prev, { path: dir, name: projectNameFromPath(dir) }],
        );
        commitFocus(opts.single ? [dir] : Array.from(new Set([...focusedRepos, dir])));
        return;
      }
      const found = await invokeGitScanRepos(dir, 2);
      if (found.length === 0) {
        opts.onError(t("focusScanEmpty"));
        return;
      }
      const saved = gitWorkspaces.find((w) => pathKey(w.path) === pathKey(dir));
      if (saved) {
        setExtraRepos((prev) => {
          const seen = new Set(prev.map((r) => pathKey(r.path)));
          return [...prev, ...found.filter((r) => !seen.has(pathKey(r.path)))];
        });
      } else {
        setExtraFolders((prev) => [
          ...prev.filter((f) => pathKey(f.path) !== pathKey(dir)),
          { path: dir, name: projectNameFromPath(dir), repos: found },
        ]);
      }
      const picked = saved ? [`ws:${saved.id}`] : found.map((r) => r.path);
      commitFocus(opts.single ? picked : Array.from(new Set([...focusedRepos, ...picked])));
    } catch (e) {
      opts.onError(String(e));
    } finally {
      setScanning(false);
    }
  };

  const autoRepoName = settings.currentGitRepo ? projectNameFromPath(settings.currentGitRepo) : null;
  /** Selection label for the composer pill; null = nothing to work on. */
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
    agentRoot: target.root,
    agentServices: target.services,
    scanning,
    onToggleGroup,
    onToggleService,
    clearFocus,
    scanFolder,
    projectLabel,
    pickerLabel,
  };
}
