import { useTranslation } from "react-i18next";
import { Check, FolderSearch, X } from "lucide-react";

export type RepoOption = { path: string; name: string };

type Props = {
  open: boolean;
  onToggleOpen: () => void;
  candidates: RepoOption[];
  selected: string[];
  onToggle: (path: string) => void;
  onClear: () => void;
  onScanFolder: () => void;
  scanning: boolean;
};

/**
 * Multi-select for the repos that get FULL diff context. Empty selection means
 * "auto" (current repo). Also exposes "扫描文件夹" to pull every git repo under
 * a chosen directory into the candidate list (and select them).
 */
export default function FocusPicker(props: Props) {
  const { t } = useTranslation("quickask");
  const { open, onToggleOpen, candidates, selected, onToggle, onClear, onScanFolder, scanning } = props;
  const selectedSet = new Set(selected);
  const label =
    selected.length === 0
      ? t("focusRepoAuto")
      : selected.length === 1
        ? candidates.find((c) => c.path === selected[0])?.name || selected[0].split(/[\\/]/).pop()
        : t("focusNSelected", { n: selected.length });

  return (
    <div className="qa-focus">
      <button
        type="button"
        className={`input-field qa-focus-btn${open ? " is-open" : ""}`}
        onClick={onToggleOpen}
        title={t("focusRepo")}
      >
        <span className="qa-focus-btn-label">{label}</span>
        <span className="qa-focus-caret">▾</span>
      </button>

      {open && (
        <div className="qa-focus-pop">
          <div className="qa-focus-pop-head">
            <span>{t("focusRepo")}</span>
            {selected.length > 0 && (
              <button type="button" className="qa-mini-btn" onClick={onClear}>
                <X size={12} /> {t("focusClear")}
              </button>
            )}
          </div>
          <div className="qa-focus-list">
            {candidates.length === 0 && <span className="quick-ask-hint">{t("focusEmpty")}</span>}
            {candidates.map((c) => {
              const on = selectedSet.has(c.path);
              return (
                <button
                  key={c.path}
                  type="button"
                  className={`qa-focus-item${on ? " on" : ""}`}
                  onClick={() => onToggle(c.path)}
                  title={c.path}
                >
                  <span className={`qa-focus-check${on ? " on" : ""}`}>{on && <Check size={11} />}</span>
                  <span className="qa-focus-name">{c.name}</span>
                  <span className="qa-focus-path">{c.path}</span>
                </button>
              );
            })}
          </div>
          <button
            type="button"
            className="btn btn-secondary btn-small qa-focus-scan"
            onClick={onScanFolder}
            disabled={scanning}
          >
            {scanning ? <span className="qa-spinner" /> : <FolderSearch size={13} />}
            {t("focusScanFolder")}
          </button>
        </div>
      )}
    </div>
  );
}
