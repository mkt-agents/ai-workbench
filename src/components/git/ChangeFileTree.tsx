import { useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { ChevronsDown, ChevronsUp, ChevronDown, ChevronRight, Undo2 } from "lucide-react";
import type { GitStatusEntry } from "../../core/types";
import { looksLikeTempFile } from "../../lib/tempFiles";
import {
  buildChangeTree,
  collectLeafKeys,
  countLeaves,
  fileKey,
  filterTree,
  nodeSelectionState,
  selectionKey,
  statusCode,
  statusClass,
  type ChangeGroupId,
  type ChangeTreeNode,
} from "../../lib/changeTree";

export type ChangeViewMode = "tree" | "flat";

function IndeterminateCheckbox({
  checked,
  indeterminate,
  disabled,
  onChange,
}: {
  checked: boolean;
  indeterminate: boolean;
  disabled?: boolean;
  onChange: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = indeterminate && !checked;
  }, [indeterminate, checked]);
  return (
    <input
      ref={ref}
      type="checkbox"
      checked={checked}
      disabled={disabled}
      onChange={onChange}
      onClick={(e) => e.stopPropagation()}
    />
  );
}

type FileRowProps = {
  entry: GitStatusEntry;
  repoPath: string;
  checked: boolean;
  busy: boolean;
  active: boolean;
  /** Flat view shows the directory suffix inline; the tree already nests it. */
  showDir: boolean;
  onActivate: (entry: GitStatusEntry) => void;
  onOpen: (entry: GitStatusEntry) => void;
  onToggle: (entry: GitStatusEntry) => void;
  onDiscard: (entry: GitStatusEntry) => void;
  onResolveConflict: (filePath: string) => void;
  statusLabel: (code: string) => string;
};

function FileRow({
  entry,
  repoPath,
  checked,
  busy,
  active,
  showDir,
  onActivate,
  onOpen,
  onToggle,
  onDiscard,
  onResolveConflict,
  statusLabel,
}: FileRowProps) {
  const { t } = useTranslation("git");
  const code = statusCode(entry);
  const conflicted = entry.group === "conflicted";
  return (
    <li
      className={`cl-file ${active ? "is-selected" : ""}${conflicted ? " is-conflict" : ""}`}
      title={conflicted ? t("commit.conflictFileTip") : undefined}
      onClick={() => {
        if (conflicted) {
          onResolveConflict(entry.path);
          return;
        }
        onActivate(entry);
      }}
      onDoubleClick={() => {
        if (conflicted) return;
        onOpen(entry);
      }}
    >
      <IndeterminateCheckbox
        checked={checked}
        indeterminate={false}
        disabled={busy || conflicted}
        onChange={() => onToggle(entry)}
      />
      <span className={`git-file-badge ${statusClass(code)}`}>{statusLabel(code)}</span>
      {looksLikeTempFile(entry.path) && (
        <span className="cl-file-temp" title={t("commit.tempBadgeTip")}>
          {t("commit.tempBadge")}
        </span>
      )}
      {entry.group === "staged" && <span className="cl-file-staged">{t("commit.statusStaged")}</span>}
      <span className="cl-file-path" title={`${repoPath.replace(/[\\/]+$/, "")}\\${entry.path.replace(/\//g, "\\")}`}>
        <span className="cl-file-name">{entry.path.slice(entry.path.lastIndexOf("/") + 1)}</span>
        {showDir && entry.path.includes("/") && (
          <span className="cl-file-dir">{entry.path.slice(0, entry.path.lastIndexOf("/"))}</span>
        )}
      </span>
      <button
        type="button"
        className="btn commit-icon-btn cl-file-discard"
        hidden={conflicted}
        disabled={busy}
        title={t("commit.discard")}
        onClick={(ev) => {
          ev.stopPropagation();
          onDiscard(entry);
        }}
      >
        <Undo2 size={12} />
      </button>
    </li>
  );
}

type Props = {
  repoPath: string;
  group: ChangeGroupId;
  entries: GitStatusEntry[];
  viewMode: ChangeViewMode;
  /** Repo-level selected-key set (`selectionKey` entries). */
  selected: Set<string>;
  busy: boolean;
  search: string;
  /** Path of the row currently shown in the diff pane (highlight). */
  activePath: string | null;
  headerCollapsed: boolean;
  onToggleHeader: () => void;
  /** Collapsed dir keys, `${repoPath}\0${dir}`. */
  collapsedDirs: Set<string>;
  onToggleDir: (dir: string) => void;
  onExpandAll: () => void;
  onCollapseAll: () => void;
  onToggleFile: (entry: GitStatusEntry) => void;
  onSetKeysChecked: (keys: string[], checked: boolean) => void;
  onToggleGroup: () => void;
  /** Single click: tick + load diff (caller decides; conflicts never reach it). */
  onFileActivate: (entry: GitStatusEntry) => void;
  onFileOpen: (entry: GitStatusEntry) => void;
  onDiscard: (entry: GitStatusEntry) => void;
  onResolveConflict: (filePath: string) => void;
  statusLabel: (code: string) => string;
};

/**
 * One IDEA-style changelist group ("更改" / "未进行版本管理的文件") with a
 * tri-state header, optional multi-level directory tree and a flat fallback.
 * Pure presentation: selection math lives in lib/changeTree.
 */
export default function ChangeFileTree({
  repoPath,
  group,
  entries,
  viewMode,
  selected,
  busy,
  search,
  activePath,
  headerCollapsed,
  onToggleHeader,
  collapsedDirs,
  onToggleDir,
  onExpandAll,
  onCollapseAll,
  onToggleFile,
  onSetKeysChecked,
  onToggleGroup,
  onFileActivate,
  onFileOpen,
  onDiscard,
  onResolveConflict,
  statusLabel,
}: Props) {
  const { t } = useTranslation("git");
  const searching = search.trim().length > 0;
  const visibleEntries = useMemo(() => {
    if (!searching) return entries;
    const q = search.trim().toLowerCase();
    return entries.filter((e) => e.path.toLowerCase().includes(q));
  }, [entries, search, searching]);

  const treeRoots = useMemo(() => buildChangeTree(entries), [entries]);
  const visibleRoots = useMemo(
    () => (searching ? filterTree(treeRoots, search) : treeRoots),
    [treeRoots, search, searching]
  );

  const selectable = useMemo(
    () => entries.filter((e) => e.group !== "conflicted"),
    [entries]
  );
  const selectedHits = useMemo(
    () => selectable.reduce((n, e) => n + (selected.has(selectionKey(e)) ? 1 : 0), 0),
    [selectable, selected]
  );
  const allSelected = selectable.length > 0 && selectedHits === selectable.length;
  const someSelected = selectedHits > 0 && !allSelected;

  const dirKey = (dir: string) => `${repoPath}\0${dir}`;
  const dirOpen = (dir: string) => searching || !collapsedDirs.has(dirKey(dir));

  const toggleDirSelection = (node: ChangeTreeNode) => {
    const keys = collectLeafKeys(node);
    onSetKeysChecked(keys, nodeSelectionState(node, selected) !== "all");
  };

  const rowProps = (entry: GitStatusEntry) => ({
    entry,
    repoPath,
    checked: entry.group !== "conflicted" && selected.has(selectionKey(entry)),
    busy,
    active: activePath === entry.path,
    showDir: viewMode === "flat",
    onActivate: onFileActivate,
    onOpen: onFileOpen,
    onToggle: onToggleFile,
    onDiscard,
    onResolveConflict,
    statusLabel,
  });

  const renderNodes = (nodes: ChangeTreeNode[], depth: number, isRoot = false) => (
    <ul className={`cl-tree-children${isRoot ? " cl-tree-root" : ""}`}>
      {nodes.map((node) => {
        if (node.kind === "file") {
          return (
            <FileRow
              key={fileKey(repoPath, node.entry.path, node.entry.group)}
              {...rowProps(node.entry)}
            />
          );
        }
        const open = dirOpen(node.path);
        const Chevron = open ? ChevronDown : ChevronRight;
        const state = nodeSelectionState(node, selected);
        return (
          <li key={`dir\0${node.path}`} className="cl-dir-item">
            <div
              className="cl-dir-row"
              onClick={() => !searching && onToggleDir(node.path)}
            >
              <button
                type="button"
                className="cl-dir-toggle"
                aria-expanded={open}
                title={open ? t("commit.collapse") : t("commit.expand")}
                onClick={(e) => {
                  e.stopPropagation();
                  if (!searching) onToggleDir(node.path);
                }}
              >
                <Chevron size={13} />
              </button>
              <IndeterminateCheckbox
                checked={state === "all"}
                indeterminate={state === "some"}
                disabled={busy}
                onChange={() => toggleDirSelection(node)}
              />
              <span className="cl-dir-name" title={node.path}>
                {node.name}
              </span>
              <span className="cl-dir-count">{countLeaves(node)}</span>
            </div>
            {open && renderNodes(node.children, depth + 1)}
          </li>
        );
      })}
    </ul>
  );

  const title =
    group === "changes" ? t("commit.groupChanges") : t("commit.groupUntracked");
  const visibleCount = searching ? visibleEntries.length : entries.length;
  const dirCount = useMemo(() => {
    if (viewMode !== "tree") return 0;
    const walk = (nodes: ChangeTreeNode[]): number =>
      nodes.reduce((n, x) => n + (x.kind === "dir" ? 1 + walk(x.children) : 0), 0);
    return walk(visibleRoots);
  }, [viewMode, visibleRoots]);

  return (
    <div className={`cl-group cl-group-${group}`}>
      <div className="cl-group-head">
        <button
          type="button"
          className="cl-dir-toggle"
          aria-expanded={!headerCollapsed}
          onClick={onToggleHeader}
        >
          {headerCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
        </button>
        <IndeterminateCheckbox
          checked={allSelected}
          indeterminate={someSelected}
          disabled={busy}
          onChange={onToggleGroup}
        />
        <button type="button" className="cl-group-title" onClick={onToggleHeader}>
          {title}
        </button>
        <span className="cl-group-count">
          {searching ? `${visibleCount}/${entries.length}` : entries.length}
        </span>
        {viewMode === "tree" && dirCount > 0 && !headerCollapsed && (
          <span className="cl-group-actions">
            <button
              type="button"
              className="btn commit-icon-btn cl-group-action"
              disabled={busy}
              title={t("commit.expandAll")}
              onClick={onExpandAll}
            >
              <ChevronsDown size={13} />
            </button>
            <button
              type="button"
              className="btn commit-icon-btn cl-group-action"
              disabled={busy}
              title={t("commit.collapseAll")}
              onClick={onCollapseAll}
            >
              <ChevronsUp size={13} />
            </button>
          </span>
        )}
      </div>
      {!headerCollapsed && (
        visibleEntries.length === 0 ? (
          <div className="cl-search-empty runtime-muted">{t("commit.searchNoMatch")}</div>
        ) : viewMode === "tree" ? (
          renderNodes(visibleRoots, 0, true)
        ) : (
          <ul className="cl-file-list">
            {visibleEntries.map((e) => (
              <FileRow key={fileKey(repoPath, e.path, e.group)} {...rowProps(e)} />
            ))}
          </ul>
        )
      )}
    </div>
  );
}
