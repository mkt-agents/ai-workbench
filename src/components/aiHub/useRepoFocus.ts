import { useEffect, useMemo, useRef, useState } from "react";
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
 * persisted picker-added folders / repos, picker groups, the agent's working
 * directory and the labels shown in the composer.
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
  /** A single repo added with「添加文件夹」(persisted as paths). */
  const [extraRepos, setExtraRepos] = useState<GitScannedRepo[]>([]);
  /** A folder added with「添加文件夹」: shown as a workspace (persisted as paths; services rescan on boot). */
  const [extraFolders, setExtraFolders] = useState<ExtraFolder[]>([]);
  const [scanning, setScanning] = useState(false);

  /** Persist the picker-added folders / repos (paths only; services rebuild on boot). */
  const persistExtra = (folders: ExtraFolder[], repos: GitScannedRepo[]) => {
    setSettings({
      aiHubExtraFolders: folders.map((f) => f.path),
      aiHubExtraRepos: repos.map((r) => r.path),
    });
  };

  // Restore picker-added folders / repos once on mount. Folder services rescan
  // (same depth as 添加文件夹); folders that became saved Git workspaces are skipped.
  const restoredRef = useRef(false);
  useEffect(() => {
    if (restoredRef.current) return;
    restoredRef.current = true;
    const folderPaths = settings.aiHubExtraFolders ?? [];
    const repoPaths = settings.aiHubExtraRepos ?? [];
    if (folderPaths.length === 0 && repoPaths.length === 0) return;
    void (async () => {
      // A persisted repo that became a saved workspace root is already shown
      // via its workspace group — skip it instead of duplicating.
      const restoredRepos: GitScannedRepo[] = repoPaths
        .filter((p) => !gitWorkspaces.some((w) => pathKey(w.path) === pathKey(p)))
        .map((p) => ({ path: p, name: projectNameFromPath(p) }));
      const restoredFolders: ExtraFolder[] = [];
      for (const dir of folderPaths) {
        if (gitWorkspaces.some((w) => pathKey(w.path) === pathKey(dir))) continue;
        try {
          const found = await invokeGitScanRepos(dir, 2);
          restoredFolders.push({ path: dir, name: projectNameFromPath(dir), repos: found });
        } catch {
          restoredFolders.push({ path: dir, name: projectNameFromPath(dir), repos: [] });
        }
      }
      setExtraRepos((prev) => {
        const seen = new Set(prev.map((r) => pathKey(r.path)));
        return [...prev, ...restoredRepos.filter((r) => !seen.has(pathKey(r.path)))];
      });
      setExtraFolders((prev) => {
        const seen = new Set(prev.map((f) => pathKey(f.path)));
        return [...prev, ...restoredFolders.filter((f) => !seen.has(pathKey(f.path)))];
      });
    })();
    // One-shot restore; later changes go through scanFolder / removeExtra.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
    // Empty workspaces have neither token nor services — fall back to the folder
    // path so single-place mode keeps the just-picked workspace.
    commitFocus(
      limit(toggleGroup(focusedRepos, group), group.token ?? group.repos[0]?.path ?? group.path ?? ""),
    );
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

  /** Remove a picker-added workspace from the list (files on disk are untouched). */
  const removeExtra = (group: PickerGroup) => {
    const k = group.path ? pathKey(group.path) : null;
    if (!k) return;
    const nextFolders = extraFolders.filter((f) => pathKey(f.path) !== k);
    const nextRepos = extraRepos.filter((r) => pathKey(r.path) !== k);
    setExtraFolders(nextFolders);
    setExtraRepos(nextRepos);
    persistExtra(nextFolders, nextRepos);
    // Drop the selection entries that pointed at the removed folder / its services.
    const gone = new Set([k, ...group.repos.map((r) => pathKey(r.path))]);
    commitFocus(focusedRepos.filter((s) => !gone.has(pathKey(s))));
  };

  /**
   * Pick a folder and select it: a repo is added as a standalone service, a
   * folder of repos becomes a workspace (a saved one is reused). Both persist
   * across restarts.
   */
  const scanFolder = async () => {
    if (scanning) return;
    setScanning(true);
    try {
      const dir = await invokePickDirectory();
      if (!dir) return;
      const k = pathKey(dir);
      // The folder is already a saved Git workspace: just select it, never
      // duplicate it as a picker-added entry.
      const savedWs = gitWorkspaces.find((w) => pathKey(w.path) === k);
      if (savedWs) {
        commitFocus(opts.single ? [`ws:${savedWs.id}`] : Array.from(new Set([...focusedRepos, `ws:${savedWs.id}`])));
        return;
      }
      if (await invokeGitIsRepo(dir)) {
        if (!extraRepos.some((r) => pathKey(r.path) === k)) {
          const next = [...extraRepos, { path: dir, name: projectNameFromPath(dir) }];
          setExtraRepos(next);
          persistExtra(extraFolders, next);
        }
        commitFocus(opts.single ? [dir] : Array.from(new Set([...focusedRepos, dir])));
        return;
      }
      const found = await invokeGitScanRepos(dir, 2);
      if (found.length === 0) {
        // 任意文件夹都是一个工作区（可以是空的）：非 git 目录一样可以做 agent 根，
        // 只是 git 类上下文（提交 / 改动）为空。
        const next = [
          ...extraFolders.filter((f) => pathKey(f.path) !== k),
          { path: dir, name: projectNameFromPath(dir), repos: [] },
        ];
        setExtraFolders(next);
        persistExtra(next, extraRepos);
        commitFocus(opts.single ? [dir] : Array.from(new Set([...focusedRepos, dir])));
        return;
      }
      const saved = gitWorkspaces.find((w) => pathKey(w.path) === k);
      if (saved) {
        const seen = new Set(extraRepos.map((r) => pathKey(r.path)));
        const next = [...extraRepos, ...found.filter((r) => !seen.has(pathKey(r.path)))];
        setExtraRepos(next);
        persistExtra(extraFolders, next);
      } else {
        const next = [
          ...extraFolders.filter((f) => pathKey(f.path) !== k),
          { path: dir, name: projectNameFromPath(dir), repos: found },
        ];
        setExtraFolders(next);
        persistExtra(next, extraRepos);
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
  /** Group keys the user can remove: folders / repos added via the picker. */
  const removableKeys = useMemo(() => {
    const paths = new Set([
      ...extraFolders.map((f) => pathKey(f.path)),
      ...extraRepos.map((r) => pathKey(r.path)),
    ]);
    return repoGroups.filter((g) => g.path && paths.has(pathKey(g.path))).map((g) => g.key);
  }, [repoGroups, extraFolders, extraRepos]);

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
    removeExtra,
    removableKeys,
    projectLabel,
    pickerLabel,
  };
}
