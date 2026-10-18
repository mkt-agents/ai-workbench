import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ChevronRight, Eye, FolderGit2, FolderSearch, Minus, Search, X } from "lucide-react";
import type { AiHubChips } from "../../core/types";
import { readStoredString, writeStoredString } from "../../core/localState";
import { pathKey } from "../../core/pathUtils";
import { groupState, isServiceSelected } from "./repoSelection";
import type { PickerGroup } from "./repoSelection";

const PART_KEYS: (keyof AiHubChips)[] = ["dirty", "git", "workspace"];
const FILTER_MIN_REPOS = 8;
const CTX_HEIGHT_KEY = "qa-ctx-pop-height";
const CTX_HEIGHT_MIN = 300;
const CTX_HEIGHT_DEFAULT = 460;

function ctxMaxHeight(): number {
  return Math.max(CTX_HEIGHT_MIN, window.innerHeight - 80);
}

function readCtxHeight(): number {
  const n = Number(readStoredString(CTX_HEIGHT_KEY));
  if (!Number.isFinite(n)) return CTX_HEIGHT_DEFAULT;
  return Math.min(ctxMaxHeight(), Math.max(CTX_HEIGHT_MIN, n));
}

type Props = {
  open: boolean;
  onToggleOpen: () => void;
  groups: PickerGroup[];
  selection: string[];
  /** Trigger label (already formatted by describeSelection). */
  label: string;
  currentRepo: string | null;
  onToggleGroup: (group: PickerGroup) => void;
  onToggleService: (group: PickerGroup, path: string) => void;
  onClear: () => void;
  onScanFolder: () => void;
  scanning: boolean;
  chips: AiHubChips;
  setChip: (key: keyof AiHubChips, value: boolean) => void;
  onPreview: () => void;
  previewLoading: boolean;
  /** The next message carries project context (lit) or not (dimmed). */
  included: boolean;
  /** Include state was set by hand for the next message (else auto-routed). */
  pinned: boolean;
  /** Write-capable permission: context is always included, the toggle is off. */
  locked: boolean;
  includeTip: string;
  onToggleInclude: () => void;
};

/**
 * One place for "what project context a project-mode question carries":
 * which workspaces / services (empty = current repo) and which parts (changes /
 * commits / workspace). Plain-chat questions ignore this picker.
 */
export default function ContextPicker(props: Props) {
  const { t } = useTranslation("aihub");
  const {
    open, onToggleOpen, groups, selection, label, currentRepo, onToggleGroup, onToggleService,
    onClear, onScanFolder, scanning, chips, setChip, onPreview, previewLoading,
    included, pinned, locked, includeTip, onToggleInclude,
  } = props;
  // Flash the repo name when the auto decision flips between include / leave out.
  const [flashKey, setFlashKey] = useState(0);
  const prevIncludedRef = useRef(included);
  useEffect(() => {
    if (prevIncludedRef.current !== included) setFlashKey((k) => k + 1);
    prevIncludedRef.current = included;
  }, [included]);
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [popHeight, setPopHeight] = useState(readCtxHeight);
  const popHeightRef = useRef(popHeight);
  const resizeDrag = useRef<{ y: number; h: number } | null>(null);

  const totalRepos = groups.reduce((n, g) => n + g.repos.length, 0);

  // Each time the popover opens, expand groups that matter right now.
  useEffect(() => {
    if (!open) return;
    const cur = currentRepo ? pathKey(currentRepo) : "";
    setExpanded(
      new Set(
        groups
          .filter(
            (g) =>
              groups.length === 1 ||
              groupState(g, selection) !== "none" ||
              g.repos.some((r) => pathKey(r.path) === cur),
          )
          .map((g) => g.key),
      ),
    );
    setQuery("");
    // Only on open: later selection changes must not collapse what the user expanded.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const q = query.trim().toLowerCase();
  const visible = useMemo(() => {
    if (!q) return groups;
    return groups
      .map((g) => ({
        ...g,
        repos: g.repos.filter((r) => `${r.name} ${r.path}`.toLowerCase().includes(q)),
      }))
      .filter((g) => g.repos.length > 0 || g.name.toLowerCase().includes(q));
  }, [groups, q]);

  const toggleExpanded = (key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const onResizeDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    resizeDrag.current = { y: e.clientY, h: popHeightRef.current };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onResizeMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const drag = resizeDrag.current;
    if (!drag) return;
    const next = Math.min(ctxMaxHeight(), Math.max(CTX_HEIGHT_MIN, drag.h + (drag.y - e.clientY)));
    popHeightRef.current = next;
    setPopHeight(next);
  };
  const onResizeUp = () => {
    if (!resizeDrag.current) return;
    resizeDrag.current = null;
    writeStoredString(CTX_HEIGHT_KEY, String(Math.round(popHeightRef.current)));
  };

  return (
    <div className="qa-focus qa-ctx-picker">
      <div className={`qa-ctx-trigger-row${open ? " is-open" : ""}`}>
        <button
          type="button"
          className={`qa-ctx-include${included ? "" : " is-excluded"}${pinned ? " is-pinned" : ""}${
            locked ? " is-locked" : ""
          }`}
          onClick={onToggleInclude}
          disabled={locked}
          title={includeTip}
          aria-pressed={included}
        >
          {flashKey > 0 && <span key={flashKey} className="qa-route-flash" aria-hidden />}
          <FolderGit2 size={12} className="qa-ctx-trigger-icon" />
          <span className="qa-focus-btn-label">{label}</span>
        </button>
        <button
          type="button"
          className="qa-ctx-caret"
          onClick={onToggleOpen}
          title={t("ctx.title")}
          aria-expanded={open}
          aria-label={t("ctx.title")}
        >
          <span className="qa-focus-caret">▾</span>
        </button>
      </div>

      {open && (
        <div className="qa-focus-pop qa-ctx-pop is-left is-up" style={{ height: popHeight }}>
          <div
            className="qa-ctx-resize"
            role="separator"
            aria-orientation="horizontal"
            aria-label={t("ctx.resize")}
            onPointerDown={onResizeDown}
            onPointerMove={onResizeMove}
            onPointerUp={onResizeUp}
            onPointerCancel={onResizeUp}
          />
          <div className="qa-focus-pop-head">
            <span>{t("ctx.reposHead")}</span>
            {selection.length > 0 && (
              <button type="button" className="qa-mini-btn" onClick={onClear}>
                <X size={12} /> {t("focusClear")}
              </button>
            )}
          </div>
          {totalRepos > FILTER_MIN_REPOS && (
            <label className="qa-ctx-filter">
              <Search size={12} />
              <input
                className="input-field"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("ctx.filter")}
              />
            </label>
          )}
          <div className="qa-focus-list qa-ctx-tree">
            {visible.length === 0 && <span className="ai-hub-hint">{t("focusEmpty")}</span>}
            {visible.map((g) => {
              const full = groups.find((x) => x.key === g.key) || g;
              const state = groupState(full, selection);
              const isOpen = Boolean(q) || expanded.has(g.key);
              const groupName = g.token ? g.name : t("ctx.otherGroup");
              return (
                <div key={g.key} className="qa-ctx-group-wrap">
                  <div className={`qa-ctx-group${state !== "none" ? " on" : ""}`}>
                    <button
                      type="button"
                      className={`qa-ctx-group-caret${isOpen ? " is-open" : ""}`}
                      onClick={() => toggleExpanded(g.key)}
                      aria-expanded={isOpen}
                      aria-label={groupName}
                    >
                      <ChevronRight size={12} />
                    </button>
                    <button
                      type="button"
                      className="qa-ctx-group-main"
                      onClick={() => onToggleGroup(full)}
                      title={g.path ? `${t("ctx.selectAll")} · ${g.path}` : t("ctx.selectAll")}
                      aria-pressed={state === "all"}
                    >
                      <span
                        className={`qa-focus-check${state === "all" ? " on" : ""}${state === "some" ? " is-partial" : ""}`}
                      >
                        {state === "all" && <Check size={11} />}
                        {state === "some" && <Minus size={11} />}
                      </span>
                      <span className="qa-focus-name">{groupName}</span>
                      <span className="qa-ctx-group-count">{full.repos.length}</span>
                    </button>
                  </div>
                  {isOpen && (
                    <div className="qa-ctx-children">
                      {g.repos.map((r) => {
                        const on = isServiceSelected(selection, full, r.path);
                        return (
                          <button
                            key={r.path}
                            type="button"
                            className={`qa-focus-item is-row${on ? " on" : ""}`}
                            onClick={() => onToggleService(full, r.path)}
                            title={r.path}
                            aria-pressed={on}
                          >
                            <span className={`qa-focus-check${on ? " on" : ""}`}>{on && <Check size={11} />}</span>
                            <span className="qa-focus-name">{r.name}</span>
                          </button>
                        );
                      })}
                    </div>
                  )}
                </div>
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

          <div className="qa-ctx-divider" />

          <div className="qa-focus-pop-head">
            <span>{t("ctx.partsHead")}</span>
          </div>
          <div className="qa-ctx-parts">
            {PART_KEYS.map((key) => (
              <button
                key={key}
                type="button"
                className={`qa-chip${chips[key] ? " on" : ""}`}
                aria-pressed={chips[key]}
                onClick={() => setChip(key, !chips[key])}
                title={t(`chipTip.${key}`)}
              >
                {t(`chip.${key}`)}
              </button>
            ))}
          </div>
          <p className="qa-ctx-note">{t("ctx.note")}</p>
          <button
            type="button"
            className="btn btn-secondary btn-small qa-focus-scan"
            onClick={onPreview}
            disabled={previewLoading}
          >
            {previewLoading ? <span className="qa-spinner" /> : <Eye size={13} />}
            {t("showContext")}
          </button>
        </div>
      )}
    </div>
  );
}
