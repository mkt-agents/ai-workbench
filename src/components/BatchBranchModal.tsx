import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Cloud, GitBranch, GitCommitHorizontal, Loader2, RefreshCw, Search } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { GIT_CONCURRENCY, mapPool } from "../core/asyncPool";
import { pathKey } from "../core/pathUtils";
import ModalTitleRow from "./ModalTitleRow";

export type BranchModalMode = "create" | "switch";
/** Where the new branch lands: only on origin, or checked out locally as well. */
export type BranchLanding = "remote" | "local";

export type BranchOptions = {
  switchExisting: boolean;
  /** Base branch to fork from; `null` means each repo's own current branch. */
  base: string | null;
  /** Fetch `origin/<base>` first so the base really is the newest tip. */
  fetchBase: boolean;
  landing: BranchLanding;
};

type BranchUsage = {
  name: string;
  /** Repos holding the branch locally or as a remote-tracking ref (a repo with both counts once). */
  count: number;
  localCount: number;
  remoteCount: number;
};

type Props = {
  mode: BranchModalMode;
  /** Selected repo paths the batch will act on. */
  paths: string[];
  onClose: () => void;
  /** Validated here; the parent owns the confirm dialog and the batch run. */
  onConfirm: (name: string, options: BranchOptions) => void;
};

type NameError =
  | "empty"
  | "leadingDash"
  | "slashEdge"
  | "lockSuffix"
  | "doubleDot"
  | "atBrace"
  | "loneAt"
  | "badChar"
  | "emptyPart"
  | "badPart";

/** Mirror of the Rust `validate_branch_name` (git check-ref-format rules). */
function validateBranchName(raw: string): NameError | null {
  const name = raw.trim();
  if (!name) return "empty";
  if (name.startsWith("-")) return "leadingDash";
  if (name.startsWith("/") || name.endsWith("/")) return "slashEdge";
  if (name.endsWith(".lock")) return "lockSuffix";
  if (name.includes("..")) return "doubleDot";
  if (name.includes("@{")) return "atBrace";
  if (name === "@") return "loneAt";
  if (/[ ~^:?*[\\]/.test(name)) return "badChar";
  for (const part of name.split("/")) {
    if (!part) return "emptyPart";
    if (part.startsWith(".") || part.endsWith(".")) return "badPart";
  }
  return null;
}

function BatchBranchModal({ mode, paths, onClose, onConfirm }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");
  const invokeGitListBranches = useGlobalStore((s) => s.invokeGitListBranches);

  const [name, setName] = useState("");
  const [query, setQuery] = useState("");
  const [switchExisting, setSwitchExisting] = useState(false);
  const [base, setBase] = useState<string | null>(null);
  const [fetchBase, setFetchBase] = useState(true);
  const [landing, setLanding] = useState<BranchLanding>("remote");
  const [branches, setBranches] = useState<BranchUsage[] | null>(null);
  const [reading, setReading] = useState(false);
  /** The list area has three looks: placeholder (never read), empty result, list. */
  const [readOnce, setReadOnce] = useState(false);
  const readSeq = useRef(0);
  const listRef = useRef<HTMLUListElement>(null);

  const trimmed = name.trim();
  const nameError = validateBranchName(trimmed);
  const canSubmit = !nameError && !reading;
  /** Create mode: flag repos that already have the typed branch locally. */
  const typedMatch = trimmed ? branches?.find((b) => b.name === trimmed) : undefined;
  // A local collision only matters when we are about to touch the worktree; a
  // remote one only matters when we are about to write on origin.
  const existsLocally =
    mode === "create" && landing === "local" && !!typedMatch && typedMatch.localCount > 0;
  const existsOnRemote =
    mode === "create" && landing === "remote" && !!typedMatch && typedMatch.remoteCount > 0;
  const baseUsage = base ? branches?.find((b) => b.name === base) : undefined;
  const usageDetail = (b: BranchUsage) =>
    t("branchModal.usageDetail", { total: paths.length, local: b.localCount, remote: b.remoteCount });
  const visibleBranches = useMemo(() => {
    if (!branches) return [];
    const q = query.trim().toLowerCase();
    return q ? branches.filter((b) => b.name.toLowerCase().includes(q)) : branches;
  }, [branches, query]);
  /** 分组口径：只要有任一仓库本地存在就归「本地已有」，一个分支名只出现一次。 */
  const [localGroup, remoteOnlyGroup] = useMemo(
    () => [
      visibleBranches.filter((b) => b.localCount > 0),
      visibleBranches.filter((b) => b.localCount === 0 && b.remoteCount > 0),
    ],
    [visibleBranches]
  );

  /** Union of local + remote-tracking refs across the selection — reads local
   *  refs only, no network. Unreadable repos just contribute nothing. */
  const readBranches = async () => {
    if (reading || paths.length === 0) return;
    setReading(true);
    const seq = ++readSeq.current;
    try {
      const repos = new Map<string, { count: Set<string>; local: Set<string>; remote: Set<string> }>();
      await mapPool(paths, GIT_CONCURRENCY, async (path) => {
        try {
          const list = await invokeGitListBranches(path);
          const key = pathKey(path);
          const localSet = new Set(list.local);
          const remoteSet = new Set(list.remote);
          // Both sets may hold the same branch name (local + remote-tracking).
          for (const b of new Set([...list.local, ...list.remote])) {
            const u = repos.get(b) ?? { count: new Set<string>(), local: new Set<string>(), remote: new Set<string>() };
            u.count.add(key);
            if (localSet.has(b)) u.local.add(key);
            if (remoteSet.has(b)) u.remote.add(key);
            repos.set(b, u);
          }
        } catch {
          /* unreadable repo: skip it in the union */
        }
      });
      if (seq !== readSeq.current) return;
      const rows: BranchUsage[] = [...repos.entries()]
        .map(([bname, u]) => ({
          name: bname,
          count: u.count.size,
          localCount: u.local.size,
          remoteCount: u.remote.size,
        }))
        .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
      setBranches(rows);
      setReadOnce(true);
    } finally {
      if (seq === readSeq.current) setReading(false);
    }
  };

  // Refs are local and cheap — populate the list right away so the picker and
  // the "already exists" hint work without an extra click.
  useEffect(() => {
    void readBranches();
  }, []);

  const handleSubmit = () => {
    if (!canSubmit) return;
    onConfirm(trimmed, { switchExisting, base, fetchBase, landing });
  };

  /** 新建形态：列表只用来挑基点，再点已选中的一行退回「不指定」。 */
  const pickBase = (branch: BranchUsage) => {
    setBase(base === branch.name ? null : branch.name);
  };

  /** 切换形态：点一行就是要切过去的分支名。 */
  const pickSwitchTarget = (branch: BranchUsage) => {
    setName(branch.name);
  };

  const moveRowFocus = (from: HTMLButtonElement, delta: number) => {
    const buttons = [
      ...(listRef.current?.querySelectorAll<HTMLButtonElement>("button.branch-pick-item") ?? []),
    ];
    const next = buttons[buttons.indexOf(from) + delta];
    next?.focus();
  };

  /**
   * 一行的读法：图标说「本地还是只在远端」（不再拿引用类型为难人），右侧说「几个仓库有它」，
   * 缺的仓库数以 chip 形式落在名字后面——它才是批量场景里真正要关心的东西。
   */
  const renderRow = (b: BranchUsage) => {
    const missing = paths.length - b.count;
    const selected = mode === "create" ? base === b.name : trimmed === b.name;
    return (
      <li key={b.name}>
        <button
          type="button"
          className={`branch-pick-item${selected ? " is-active" : ""}${missing > 0 ? " is-partial" : ""}`}
          aria-pressed={selected}
          title={usageDetail(b)}
          onClick={() => (mode === "create" ? pickBase(b) : pickSwitchTarget(b))}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              moveRowFocus(e.currentTarget, e.key === "ArrowDown" ? 1 : -1);
            }
            if (e.key === "Enter" && mode === "switch" && trimmed === b.name) {
              e.preventDefault();
              handleSubmit();
            }
          }}
        >
          {b.localCount > 0 ? (
            <GitBranch size={13} className="bb-item-icon" />
          ) : (
            <Cloud size={13} className="bb-item-icon is-remote" />
          )}
          <span className="branch-pick-name">{b.name}</span>
          {missing > 0 && (
            <span className="bb-missing">{t("branchModal.missingN", { count: missing })}</span>
          )}
          <span className="branch-pick-count">
            {t("branchModal.countOf", { have: b.count, total: paths.length })}
          </span>
        </button>
      </li>
    );
  };

  const nameField = (
    <div className="input-group bb-name">
      <label className="input-label">
        {mode === "create" ? t("branchModal.nameLabel") : t("branchModal.typeLabel")}
      </label>
      <input
        className="input-field bb-name-input"
        value={name}
        autoFocus={mode === "create"}
        spellCheck={false}
        placeholder={
          mode === "create" ? t("branchModal.namePlaceholder") : t("branchModal.namePlaceholderSwitch")
        }
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") handleSubmit();
          if (e.key === "ArrowDown") {
            e.preventDefault();
            listRef.current?.querySelector<HTMLButtonElement>("button.branch-pick-item")?.focus();
          }
        }}
      />
      {trimmed !== "" && nameError && (
        <div className="repos-modal-error">{t(`branchModal.invalid.${nameError}`)}</div>
      )}
      {mode === "create" && !nameError && (
        <p className="input-hint">
          {landing === "remote" ? t("branchModal.createHintRemote") : t("branchModal.createHintLocal")}
        </p>
      )}
      {mode === "switch" && trimmed !== "" && !nameError && !typedMatch && (
        <p className="input-hint">{t("branchModal.switchHint")}</p>
      )}
      {existsOnRemote && !nameError && (
        <div className="bb-collide">
          <div className="bb-warn">
            <AlertTriangle size={12} />
            <span>{t("branchModal.alreadyOnRemote", { count: typedMatch!.remoteCount })}</span>
          </div>
        </div>
      )}
      {existsLocally && !nameError && (
        <div className="bb-collide">
          <div className="bb-warn">
            <AlertTriangle size={12} />
            <span>{t("branchModal.alreadyHave", { count: typedMatch!.localCount })}</span>
          </div>
          <label className="repos-apply-now bb-switch-existing">
            <input
              type="checkbox"
              checked={switchExisting}
              onChange={(e) => setSwitchExisting(e.target.checked)}
            />
            <span>{t("branchModal.switchExisting")}</span>
          </label>
        </div>
      )}
    </div>
  );

  const planRows = (
    <div className="bb-plan">
      <div className="bb-row">
        <span className="bb-row-label">{t("branchModal.landingLabel")}</span>
        <div className="bb-radios">
          <label className="bb-radio">
            <input
              type="radio"
              name="bb-landing"
              checked={landing === "remote"}
              onChange={() => setLanding("remote")}
            />
            <span>{t("branchModal.landingRemote")}</span>
          </label>
          <label className="bb-radio">
            <input
              type="radio"
              name="bb-landing"
              checked={landing === "local"}
              onChange={() => setLanding("local")}
            />
            <span>{t("branchModal.landingLocal")}</span>
          </label>
        </div>
      </div>
      {landing === "remote" && !base && (
        <p className="input-hint">{t("branchModal.baseCurrentWarn")}</p>
      )}
    </div>
  );

  /** 基点摘要 + 取基点勾选放在不滚动的面板头里：选完基点后这两行不能跟着列表滚走。 */
  const baseLine = (
    <div className="bb-base-line">
      <span className="bb-base-text" title={baseUsage ? usageDetail(baseUsage) : undefined}>
        {base && baseUsage
          ? t(
              baseUsage.count < paths.length
                ? "branchModal.baseSummaryPartial"
                : "branchModal.baseSummary",
              {
                name: base,
                have: baseUsage.count,
                total: paths.length,
                missing: paths.length - baseUsage.count,
              }
            )
          : base
            ? t("branchModal.baseSummaryMissing", { name: base })
            : t("branchModal.baseSummaryNone")}
      </span>
      {base && (
        <label className="repos-apply-now bb-fetch-base" title={t("branchModal.fetchBaseTip")}>
          <input
            type="checkbox"
            checked={fetchBase}
            onChange={(e) => setFetchBase(e.target.checked)}
          />
          <span>{t("branchModal.fetchBase", { name: base })}</span>
        </label>
      )}
    </div>
  );

  return (
    <div className="modal-overlay">
      <div className="modal batch-branch-modal">
        <ModalTitleRow
          title={mode === "create" ? t("branchModal.createTitle") : t("branchModal.switchTitle")}
          onClose={onClose}
          badge={
            <span className="card-title-badge">
              {t("branchModal.selectedRepos", { count: paths.length })}
            </span>
          }
        />
        <div className="bb-body">
          {mode === "create" && (
            <>
              {nameField}
              {planRows}
            </>
          )}

          <div className="bb-panel">
            <div className="bb-panel-bar">
              <label className="bb-filter-wrap">
                <Search size={13} />
                <input
                  className="bb-filter"
                  value={query}
                  autoFocus={mode === "switch"}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder={t("branchModal.filterPlaceholder")}
                  spellCheck={false}
                  disabled={!branches || branches.length === 0}
                  onKeyDown={(e) => {
                    if (e.key === "ArrowDown") {
                      e.preventDefault();
                      listRef.current?.querySelector<HTMLButtonElement>("button.branch-pick-item")?.focus();
                    }
                    if (e.key === "Enter" && mode === "switch" && visibleBranches.length === 1) {
                      e.preventDefault();
                      pickSwitchTarget(visibleBranches[0]);
                    }
                  }}
                />
              </label>
              <button
                type="button"
                className="bb-refresh"
                onClick={() => void readBranches()}
                disabled={reading || paths.length === 0}
                title={reading ? t("branchModal.reading") : t("branchModal.refresh")}
                aria-label={reading ? t("branchModal.reading") : t("branchModal.refresh")}
              >
                {reading ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
              </button>
            </div>

            {mode === "create" ? (
              baseLine
            ) : (
              <div className="bb-colhead">
                <span>{t("branchModal.listLabelSwitch")}</span>
                <span title={t("branchModal.colCountTitle", { total: paths.length })}>
                  {t("branchModal.colCount")}
                </span>
              </div>
            )}

            {reading && !readOnce && (
              <div className="bb-empty">
                <Loader2 size={14} className="spin" />
                <span>{t("branchModal.reading")}</span>
              </div>
            )}
            {readOnce && branches && branches.length === 0 && (
              <div className="bb-empty">{t("branchModal.noBranches")}</div>
            )}
            {branches && branches.length > 0 && visibleBranches.length === 0 && (
              <div className="bb-empty">{t("branchModal.noMatch")}</div>
            )}
            {visibleBranches.length > 0 && (
              <ul className="branch-pick-list" ref={listRef}>
                {mode === "create" && (
                  <li>
                    <button
                      type="button"
                      className={`branch-pick-item is-base-none${base ? "" : " is-active"}`}
                      aria-pressed={!base}
                      title={t("branchModal.baseNoneTip")}
                      onClick={() => setBase(null)}
                      onKeyDown={(e) => {
                        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                          e.preventDefault();
                          moveRowFocus(e.currentTarget, e.key === "ArrowDown" ? 1 : -1);
                        }
                      }}
                    >
                      <GitCommitHorizontal size={13} className="bb-item-icon" />
                      <span className="branch-pick-name">{t("branchModal.baseNoneRow")}</span>
                    </button>
                  </li>
                )}
                {localGroup.length > 0 && (
                  <li className="bb-group-head" aria-hidden>
                    {t("branchModal.groupLocal")}
                  </li>
                )}
                {localGroup.map(renderRow)}
                {remoteOnlyGroup.length > 0 && (
                  <li className="bb-group-head" aria-hidden>
                    {mode === "create"
                      ? t("branchModal.groupRemoteBase")
                      : t("branchModal.groupRemoteSwitch")}
                  </li>
                )}
                {remoteOnlyGroup.map(renderRow)}
              </ul>
            )}

            {mode === "switch" && nameField}
          </div>
        </div>

        <div className="modal-actions bb-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            {tc("actions.cancel")}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            onClick={handleSubmit}
            disabled={!canSubmit}
            title={
              trimmed !== "" && nameError
                ? t(`branchModal.invalid.${nameError}`)
                : canSubmit
                  ? trimmed
                  : undefined
            }
          >
            {mode === "create" ? t("batch.createBranch") : t("batch.switchBranch")}
            {canSubmit ? ` · ${trimmed}` : ""}
          </button>
        </div>
      </div>
    </div>
  );
}

export default BatchBranchModal;
