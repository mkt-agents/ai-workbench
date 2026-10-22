import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import { ChevronsDown, ChevronsUp, ChevronDown, ChevronRight, GitBranch, Undo2 } from "lucide-react";
import type { GitStatusEntry } from "../../core/types";
import { looksLikeTempFile } from "../../lib/tempFiles";
import {
  buildChangeTree,
  collectDirPaths,
  collectLeafKeys,
  countLeaves,
  fileKey,
  filterTree,
  nodeSelectionState,
  selectionKey,
  statusCode,
  statusClass,
  type ChangeDirNode,
  type ChangeGroupId,
  type ChangeTreeNode,
} from "../../lib/changeTree";

export type ChangeViewMode = "tree" | "flat";

/** One repo's contribution to a workspace-level change group. */
export type ChangeFileRepo = {
  repoPath: string;
  repoLabel: string;
  branch?: string;
  entries: GitStatusEntry[];
};

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
  /** Tree depth: fixed checkbox column + per-level name indent (IDEA style). */
  depth?: number;
  /** Keyboard-focus highlight (↑↓ navigation). */
  focused?: boolean;
  rowRef?: (el: HTMLLIElement | null) => void;
  onFocusRow?: () => void;
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
  depth,
  focused,
  rowRef,
  onFocusRow,
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
      ref={rowRef}
      className={`cl-file ${active ? "is-selected" : ""}${focused ? " is-focused" : ""}${checked ? " is-checked" : ""}${conflicted ? " is-conflict" : ""}${depth !== undefined ? " cl-tree-row" : ""} cl-st-${statusClass(code).replace("git-status-", "")}`}
      style={
        depth !== undefined
          ? ({ paddingLeft: 30 + depth * 16 } as CSSProperties)
          : undefined
      }
      title={conflicted ? t("commit.conflictFileTip") : undefined}
      onClick={() => {
        onFocusRow?.();
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
      <span
        className="cl-file-path"
        title={`${repoPath.replace(/[\\/]+$/, "")}\\${entry.path.replace(/\//g, "\\")}`}
      >
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

type PerRepo = ChangeFileRepo & {
  selectable: GitStatusEntry[];
  hits: number;
  roots: ChangeTreeNode[];
  visibleEntries: GitStatusEntry[];
};

/** One visible row of the tree, in keyboard-navigation order. */
type RowDesc =
  | { kind: "file"; repoPath: string; entry: GitStatusEntry; depth: number }
  | { kind: "dir"; repoPath: string; node: ChangeDirNode; depth: number; open: boolean }
  | { kind: "repo"; repo: PerRepo; open: boolean };

type Props = {
  group: ChangeGroupId;
  /** All dirty repos of the current workspace scope, already split into this group. */
  repos: ChangeFileRepo[];
  viewMode: ChangeViewMode;
  /** Whole selection map keyed by repoPath (selectionKey entries per repo). */
  selected: Map<string, Set<string>>;
  busy: boolean;
  search: string;
  /** Row currently shown in the diff pane (highlight). */
  activeDiff: { repoPath: string; filePath: string } | null;
  /** Collapsed repo nodes (multi-repo mode). */
  collapsedRepos: Set<string>;
  /** Collapsed dir keys, `${repoPath}\0${dir}`. */
  collapsedDirs: Set<string>;
  headerCollapsed: boolean;
  onToggleHeader: () => void;
  onToggleRepoCollapsed: (repoPath: string) => void;
  onRepoClick: (repoPath: string) => void;
  onToggleDir: (repoPath: string, dir: string) => void;
  onExpandAll: () => void;
  onCollapseAll: () => void;
  onToggleFile: (repoPath: string, entry: GitStatusEntry) => void;
  onSetKeysChecked: (repoPath: string, keys: string[], checked: boolean) => void;
  onToggleGroup: () => void;
  /** Tri-state tick of one repo's whole group (repo node checkbox). */
  onToggleRepo: (repoPath: string, entries: GitStatusEntry[]) => void;
  onFileActivate: (repoPath: string, entry: GitStatusEntry) => void;
  onFileOpen: (repoPath: string, entry: GitStatusEntry) => void;
  onDiscard: (repoPath: string, entry: GitStatusEntry) => void;
  onResolveConflict: (repoPath: string, filePath: string) => void;
  statusLabel: (code: string) => string;
  /** Speed search: a printable key typed on the list lands in the search box. */
  onSpeedSearch?: (ch: string) => void;
  onClearSearch?: () => void;
};

/**
 * One workspace-level IDEA-style changelist group ("更改" / "未进行版本管理的
 * 文件"). With several dirty repos the group nests collapsible repo nodes
 * (tri-state checkbox + branch badge); a single repo renders its tree directly.
 * Pure presentation: selection math lives in lib/changeTree.
 */
export default function ChangeFileTree({
  group,
  repos,
  viewMode,
  selected,
  busy,
  search,
  activeDiff,
  collapsedRepos,
  collapsedDirs,
  headerCollapsed,
  onToggleHeader,
  onToggleRepoCollapsed,
  onRepoClick,
  onToggleDir,
  onExpandAll,
  onCollapseAll,
  onToggleFile,
  onSetKeysChecked,
  onToggleGroup,
  onToggleRepo,
  onFileActivate,
  onFileOpen,
  onDiscard,
  onResolveConflict,
  statusLabel,
  onSpeedSearch,
  onClearSearch,
}: Props) {
  const { t } = useTranslation("git");
  const searching = search.trim().length > 0;
  const q = search.trim().toLowerCase();

  const perRepo = useMemo<PerRepo[]>(
    () =>
      repos.map((r) => {
        const selectable = r.entries.filter((e) => e.group !== "conflicted");
        const sel = selected.get(r.repoPath) || new Set<string>();
        const hits = selectable.reduce((n, e) => n + (sel.has(selectionKey(e)) ? 1 : 0), 0);
        const roots = buildChangeTree(r.entries);
        const visibleEntries = searching
          ? r.entries.filter((e) => e.path.toLowerCase().includes(q))
          : r.entries;
        return { ...r, selectable, hits, roots, visibleEntries };
      }),
    [repos, selected, searching, q]
  );

  const multiRepo = repos.length > 1;
  const totalEntries = perRepo.reduce((n, r) => n + r.entries.length, 0);
  const visibleTotal = perRepo.reduce((n, r) => n + r.visibleEntries.length, 0);
  const selectableTotal = perRepo.reduce((n, r) => n + r.selectable.length, 0);
  const selectedTotal = perRepo.reduce((n, r) => n + r.hits, 0);
  const allSelected = selectableTotal > 0 && selectedTotal === selectableTotal;
  const someSelected = selectedTotal > 0 && !allSelected;

  const dirOpen = (repoPath: string, dir: string) =>
    searching || !collapsedDirs.has(`${repoPath}\0${dir}`);

  const toggleDirSelection = (repoPath: string, node: ChangeTreeNode) => {
    const sel = selected.get(repoPath) || new Set<string>();
    onSetKeysChecked(repoPath, collectLeafKeys(node), nodeSelectionState(node, sel) !== "all");
  };

  const rowProps = (repoPath: string, entry: GitStatusEntry, depth?: number) => ({
    entry,
    repoPath,
    checked:
      entry.group !== "conflicted" && (selected.get(repoPath)?.has(selectionKey(entry)) ?? false),
    busy,
    active: !!activeDiff && activeDiff.repoPath === repoPath && activeDiff.filePath === entry.path,
    showDir: viewMode === "flat",
    depth,
    onActivate: (e: GitStatusEntry) => onFileActivate(repoPath, e),
    onOpen: (e: GitStatusEntry) => onFileOpen(repoPath, e),
    onToggle: (e: GitStatusEntry) => onToggleFile(repoPath, e),
    onDiscard: (e: GitStatusEntry) => onDiscard(repoPath, e),
    onResolveConflict: (filePath: string) => onResolveConflict(repoPath, filePath),
    statusLabel,
  });

  const visibleRoots = (r: PerRepo) => (searching ? filterTree(r.roots, search) : r.roots);

  // Flat descriptor list of the visible tree rows — render order equals the
  // ↑↓ keyboard-navigation order.
  const rows = useMemo<RowDesc[]>(() => {
    if (viewMode !== "tree") return [];
    const out: RowDesc[] = [];
    const pushTree = (r: PerRepo, nodes: ChangeTreeNode[], depth: number) => {
      for (const node of nodes) {
        if (node.kind === "file") {
          out.push({ kind: "file", repoPath: r.repoPath, entry: node.entry, depth });
          continue;
        }
        const open = dirOpen(r.repoPath, node.path);
        out.push({ kind: "dir", repoPath: r.repoPath, node, depth, open });
        if (open) pushTree(r, node.children, depth + 1);
      }
    };
    if (multiRepo) {
      for (const r of perRepo) {
        const open = searching || !collapsedRepos.has(r.repoPath);
        out.push({ kind: "repo", repo: r, open });
        if (open) pushTree(r, visibleRoots(r), 1);
      }
    } else if (perRepo.length === 1) {
      pushTree(perRepo[0], visibleRoots(perRepo[0]), 0);
    }
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [perRepo, viewMode, multiRepo, search, collapsedRepos, collapsedDirs]);

  // ↑↓ moves focus, Space toggles the row's checkbox, Enter loads the diff.
  const [focusIdx, setFocusIdx] = useState<number | null>(null);
  const rowEls = useRef(new Map<number, HTMLElement>());
  const listRef = useRef<HTMLUListElement>(null);

  useEffect(() => {
    if (focusIdx === null) return;
    rowEls.current.get(focusIdx)?.scrollIntoView({ block: "nearest" });
  }, [focusIdx]);

  const focusList = () => listRef.current?.focus({ preventScroll: true });

  const onListKeyDown = (e: KeyboardEvent<HTMLUListElement>) => {
    // Speed search (IDEA style): type on the list to filter without touching the
    // box. Space is excluded — it toggles the focused row's checkbox.
    if (e.key.length === 1 && e.key !== " " && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      onSpeedSearch?.(e.key);
      return;
    }
    if (e.key === "Escape" && searching) {
      e.preventDefault();
      onClearSearch?.();
      return;
    }
    const n = rows.length;
    if (n === 0) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Home" || e.key === "End") {
      e.preventDefault();
      setFocusIdx((cur) => {
        if (e.key === "Home") return 0;
        if (e.key === "End") return n - 1;
        const base = cur === null ? (e.key === "ArrowDown" ? -1 : n) : cur;
        return Math.max(0, Math.min(n - 1, base + (e.key === "ArrowDown" ? 1 : -1)));
      });
      return;
    }
    if (focusIdx === null || focusIdx >= n) return;
    const row = rows[focusIdx];
    if (e.key === " ") {
      e.preventDefault();
      if (row.kind === "file") onToggleFile(row.repoPath, row.entry);
      else if (row.kind === "dir") toggleDirSelection(row.repoPath, row.node);
      else onToggleRepo(row.repo.repoPath, row.repo.entries);
    } else if (e.key === "Enter" && row.kind === "file" && row.entry.group !== "conflicted") {
      e.preventDefault();
      onFileActivate(row.repoPath, row.entry);
    }
  };

  const renderRow = (row: RowDesc, idx: number): ReactNode => {
    const focused = focusIdx === idx;
    const registerRow = (el: HTMLElement | null) => {
      if (el) rowEls.current.set(idx, el);
      else rowEls.current.delete(idx);
    };
    const focusOnClick = () => {
      setFocusIdx(idx);
      focusList();
    };
    if (row.kind === "file") {
      return (
        <FileRow
          key={fileKey(row.repoPath, row.entry.path, row.entry.group)}
          {...rowProps(row.repoPath, row.entry, row.depth)}
          focused={focused}
          rowRef={registerRow}
          onFocusRow={focusOnClick}
        />
      );
    }
    if (row.kind === "dir") {
      const { node } = row;
      const Chevron = row.open ? ChevronDown : ChevronRight;
      const state = nodeSelectionState(node, selected.get(row.repoPath) || new Set<string>());
      return (
        <li
          key={`dir\0${row.repoPath}\0${node.path}`}
          ref={registerRow}
          className={`cl-dir-row cl-tree-row${focused ? " is-focused" : ""}`}
          style={{ paddingLeft: 30 + row.depth * 16 } as CSSProperties}
          onClick={() => {
            focusOnClick();
            if (!searching) onToggleDir(row.repoPath, node.path);
          }}
        >
          <button
            type="button"
            className="cl-dir-toggle"
            aria-expanded={row.open}
            title={row.open ? t("commit.collapse") : t("commit.expand")}
            onClick={(e) => {
              e.stopPropagation();
              if (!searching) onToggleDir(row.repoPath, node.path);
            }}
          >
            <Chevron size={13} />
          </button>
          <IndeterminateCheckbox
            checked={state === "all"}
            indeterminate={state === "some"}
            disabled={busy}
            onChange={() => toggleDirSelection(row.repoPath, node)}
          />
          <span className="cl-dir-name" title={node.path}>
            {node.name}
          </span>
          <span className="cl-dir-count">{countLeaves(node)}</span>
        </li>
      );
    }
    const r = row.repo;
    const Chevron = row.open ? ChevronDown : ChevronRight;
    const state =
      r.selectable.length === 0
        ? "none"
        : r.hits === 0
          ? "none"
          : r.hits === r.selectable.length
            ? "all"
            : "some";
    return (
      <li
        key={`repo\0${r.repoPath}`}
        ref={registerRow}
        className={`cl-dir-row cl-repo-row cl-tree-row${focused ? " is-focused" : ""}`}
        style={{ paddingLeft: 30 } as CSSProperties}
        onClick={() => onToggleRepoCollapsed(r.repoPath)}
      >
        <button
          type="button"
          className="cl-dir-toggle"
          aria-expanded={row.open}
          onClick={(e) => {
            e.stopPropagation();
            onToggleRepoCollapsed(r.repoPath);
          }}
        >
          <Chevron size={14} />
        </button>
        <IndeterminateCheckbox
          checked={state === "all"}
          indeterminate={state === "some"}
          disabled={busy}
          onChange={() => onToggleRepo(r.repoPath, r.entries)}
        />
        <button
          type="button"
          className="cl-repo-name"
          title={r.repoPath}
          onClick={(e) => {
            e.stopPropagation();
            onRepoClick(r.repoPath);
          }}
        >
          {r.repoLabel}
        </button>
        {r.branch ? (
          <span className="commit-branch-badge" title={r.branch}>
            <GitBranch size={12} />
            <span className="commit-branch-text">{r.branch}</span>
          </span>
        ) : null}
        <span className="cl-dir-count">{r.visibleEntries.length}</span>
      </li>
    );
  };

  const dirCount = useMemo(() => {
    if (viewMode !== "tree") return 0;
    return perRepo.reduce((n, r) => n + collectDirPaths(visibleRoots(r)).length, 0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [perRepo, viewMode, search]);

  const title = group === "changes" ? t("commit.groupChanges") : t("commit.groupUntracked");

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
        <span
          className="cl-group-count"
          title={
            !searching && selectedTotal > 0 && !allSelected
              ? t("commit.groupSelectedCount", { selected: selectedTotal, total: totalEntries })
              : undefined
          }
        >
          {searching
            ? `${visibleTotal}/${totalEntries}`
            : !allSelected && selectedTotal > 0
              ? `${selectedTotal}/${totalEntries}`
              : totalEntries}
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
      {!headerCollapsed &&
        (visibleTotal === 0 ? (
          <div className="cl-search-empty runtime-muted">{t("commit.searchNoMatch")}</div>
        ) : viewMode === "tree" ? (
          <ul
            className="cl-tree-list"
            tabIndex={0}
            ref={listRef}
            onKeyDown={onListKeyDown}
          >
            {rows.map((row, idx) => renderRow(row, idx))}
          </ul>
        ) : (
          <div className="cl-flat-lists">
            {perRepo.map((r) => (
              <div key={r.repoPath} className="cl-flat-repo">
                {multiRepo && (
                  <div className="cl-dir-row cl-repo-row" onClick={() => onRepoClick(r.repoPath)}>
                    <button
                      type="button"
                      className="cl-repo-name"
                      title={r.repoPath}
                      onClick={(e) => {
                        e.stopPropagation();
                        onRepoClick(r.repoPath);
                      }}
                    >
                      {r.repoLabel}
                    </button>
                    {r.branch ? (
                      <span className="commit-branch-badge" title={r.branch}>
                        <GitBranch size={12} />
                        <span className="commit-branch-text">{r.branch}</span>
                      </span>
                    ) : null}
                    <span className="cl-dir-count">{r.visibleEntries.length}</span>
                  </div>
                )}
                <ul className="cl-file-list">
                  {r.visibleEntries.map((e) => (
                    <FileRow
                      key={fileKey(r.repoPath, e.path, e.group)}
                      {...rowProps(r.repoPath, e)}
                    />
                  ))}
                </ul>
              </div>
            ))}
          </div>
        ))}
    </div>
  );
}
