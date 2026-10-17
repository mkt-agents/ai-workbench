import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, GitBranch, Loader2, RefreshCw, Search } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { GIT_CONCURRENCY, mapPool } from "../core/asyncPool";
import { pathKey } from "../core/pathUtils";
import ModalTitleRow from "./ModalTitleRow";

export type BranchModalMode = "create" | "switch";

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
  onConfirm: (name: string, options: { switchExisting: boolean }) => void;
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
  const existsLocally = mode === "create" && !!typedMatch && typedMatch.localCount > 0;
  const visibleBranches = useMemo(() => {
    if (!branches) return [];
    const q = query.trim().toLowerCase();
    return q ? branches.filter((b) => b.name.toLowerCase().includes(q)) : branches;
  }, [branches, query]);

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
    onConfirm(trimmed, { switchExisting });
  };

  const selectBranch = (branch: BranchUsage) => {
    setName(branch.name);
    if (mode === "create" && branch.localCount > 0) setSwitchExisting(true);
  };

  const moveRowFocus = (from: HTMLButtonElement, delta: number) => {
    const buttons = [
      ...(listRef.current?.querySelectorAll<HTMLButtonElement>("button.branch-pick-item") ?? []),
    ];
    const next = buttons[buttons.indexOf(from) + delta];
    next?.focus();
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
      {mode === "create" && !nameError && !existsLocally && (
        <p className="input-hint">{t("branchModal.createHint")}</p>
      )}
      {mode === "switch" && trimmed !== "" && !nameError && !typedMatch && (
        <p className="input-hint">{t("branchModal.switchHint")}</p>
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
          {mode === "create" && nameField}

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
                      selectBranch(visibleBranches[0]);
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

            <div className="bb-colhead">
              <span>{mode === "create" ? t("branchModal.listLabelCreate") : t("branchModal.listLabelSwitch")}</span>
              <span
                title={
                  mode === "create"
                    ? t("branchModal.colHaveTitle", { total: paths.length })
                    : t("branchModal.colSwitchTitle", { total: paths.length })
                }
              >
                {mode === "create" ? t("branchModal.colHave") : t("branchModal.colSwitch")}
              </span>
            </div>

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
                {visibleBranches.map((b) => {
                  const partial = b.count < paths.length;
                  const where =
                    b.localCount === 0
                      ? t("branchModal.remoteOnly")
                      : b.remoteCount === 0
                        ? t("branchModal.localOnly")
                        : t("branchModal.localAndRemote");
                  const missing =
                    mode === "switch" && partial
                      ? t("branchModal.rowMissing", { count: paths.length - b.count })
                      : "";
                  return (
                    <li key={b.name}>
                      <button
                        type="button"
                        className={`branch-pick-item${b.name === trimmed ? " is-active" : ""}${
                          mode === "switch" && partial ? " is-partial" : ""
                        }`}
                        aria-pressed={b.name === trimmed}
                        onClick={() => selectBranch(b)}
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
                        <GitBranch size={13} className="bb-item-icon" />
                        <span className="branch-pick-name">{b.name}</span>
                        <span className="bb-where">{where}</span>
                        <span
                          className="branch-pick-count"
                          title={
                            missing ||
                            t("branchModal.usageDetail", {
                              total: paths.length,
                              local: b.localCount,
                              remote: b.remoteCount,
                            })
                          }
                        >
                          {t("branchModal.repoCount", { count: b.count })}
                        </span>
                      </button>
                    </li>
                  );
                })}
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
