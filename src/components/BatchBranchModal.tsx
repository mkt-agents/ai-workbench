import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, GitBranch, Info, Loader2, RefreshCw } from "lucide-react";
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

  const trimmed = name.trim();
  const nameError = validateBranchName(trimmed);
  const canSubmit = !nameError && !reading;
  /** Create mode: flag repos that already have the typed branch locally. */
  const typedMatch = trimmed ? branches?.find((b) => b.name === trimmed) : undefined;
  const existsLocally = mode === "create" && !!typedMatch && typedMatch.localCount > 0;
  const filtering = query.trim() !== "";
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

  return (
    <div className="modal-overlay">
      <div className="modal batch-branch-modal">
        <ModalTitleRow
          title={mode === "create" ? t("branchModal.createTitle") : t("branchModal.switchTitle")}
          onClose={onClose}
          badge={<span className="card-title-badge">{paths.length}</span>}
        />
        <div className="bb-body">
          <div className="input-group">
            <label className="input-label">{t("branchModal.nameLabel")}</label>
            <input
              className="input-field bb-name-input"
              value={name}
              autoFocus
              spellCheck={false}
              placeholder={t("branchModal.namePlaceholder")}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleSubmit();
              }}
            />
            {trimmed !== "" && nameError && (
              <div className="repos-modal-error">{t(`branchModal.invalid.${nameError}`)}</div>
            )}
            {existsLocally && !nameError && (
              <div className="bb-warn">
                <AlertTriangle size={12} />
                <span>{t("branchModal.alreadyHave", { count: typedMatch!.localCount })}</span>
              </div>
            )}
          </div>

          <div className="bb-callout">
            <Info size={13} />
            <span>
              {mode === "create" ? t("branchModal.createHint") : t("branchModal.switchHint")}
            </span>
          </div>

          {mode === "create" && (
            <label className="repos-apply-now bb-switch-existing">
              <input
                type="checkbox"
                checked={switchExisting}
                onChange={(e) => setSwitchExisting(e.target.checked)}
              />
              <span>{t("branchModal.switchExisting")}</span>
            </label>
          )}

          <div className="bb-section-head">
            <span className="input-label">{t("branchModal.listLabel")}</span>
            {filtering && (
              <span className="branch-pick-count">
                {t("branchModal.matchCount", {
                  count: visibleBranches.length,
                  total: branches?.length ?? 0,
                })}
              </span>
            )}
            <input
              className="bb-filter"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("branchModal.filterPlaceholder")}
              spellCheck={false}
              disabled={!branches || branches.length === 0}
            />
            <button
              type="button"
              className="bb-refresh"
              onClick={() => void readBranches()}
              disabled={reading || paths.length === 0}
            >
              {reading ? <Loader2 size={12} className="spin" /> : <RefreshCw size={12} />}
              <span>
                {reading
                  ? t("branchModal.reading")
                  : readOnce
                    ? t("branchModal.refresh")
                    : t("branchModal.readBranches")}
              </span>
            </button>
          </div>

          {readOnce && branches && branches.length === 0 && (
            <div className="bb-empty">{t("branchModal.noBranches")}</div>
          )}
          {branches && branches.length > 0 && visibleBranches.length === 0 && (
            <div className="bb-empty">{t("branchModal.noMatch")}</div>
          )}
          {visibleBranches.length > 0 && (
            <ul className="branch-pick-list">
              {visibleBranches.map((b) => (
                <li key={b.name}>
                  <button
                    type="button"
                    className={`branch-pick-item${b.name === trimmed ? " is-active" : ""}`}
                    onClick={() => setName(b.name)}
                  >
                    <GitBranch size={12} className="bb-item-icon" />
                    <span className="branch-pick-name">{b.name}</span>
                    <span className="bb-tag">
                      {b.localCount === 0
                        ? t("branchModal.remoteOnly")
                        : b.remoteCount === 0
                          ? t("branchModal.localOnly")
                          : t("branchModal.localAndRemote")}
                    </span>
                    <span
                      className="branch-pick-count"
                      title={t("branchModal.usageDetail", { local: b.localCount, remote: b.remoteCount })}
                    >
                      {t("branchModal.availability", { count: b.count, total: paths.length })}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          {!readOnce && (
            <div className="bb-empty">{t("branchModal.listHint")}</div>
          )}
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
            title={trimmed !== "" && nameError ? t(`branchModal.invalid.${nameError}`) : undefined}
          >
            {mode === "create" ? t("batch.createBranch") : t("batch.switchBranch")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default BatchBranchModal;
