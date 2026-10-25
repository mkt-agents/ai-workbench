import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, ChevronDown, ChevronRight, Eye, FolderGit2, FolderPlus, Minus, Search, SlidersHorizontal, X } from "lucide-react";
import type { AiHubChips } from "../../core/types";
import { readStoredString, writeStoredString } from "../../core/localState";
import { pathKey } from "../../core/pathUtils";
import { groupState, isServiceSelected } from "./repoSelection";
import type { PickerGroup } from "./repoSelection";

const PART_KEYS: (keyof AiHubChips)[] = ["dirty", "git", "workspace"];
const CTX_PARTS_KEY = "qa-ctx-parts-open";
const CTX_HEIGHT_KEY = "qa-ctx-pop-height";
const CTX_HEIGHT_MIN = 300;
const CTX_HEIGHT_DEFAULT = 460;
/** Air kept between the panel and the window edge above it. */
const CTX_TOP_GAP = 12;

/** Ceiling imposed by the window itself (the composer lives below the panel). */
function windowCap(): number {
  return Math.max(CTX_HEIGHT_MIN, window.innerHeight - 80);
}

/**
 * Tallest the panel may get. It opens upwards from the composer, so the space
 * between the trigger row and the top edge is the real limit — a stored height
 * from a bigger window must not push the header off-screen.
 */
function ctxCap(root: HTMLElement | null): number {
  const top = root?.getBoundingClientRect().top;
  if (top == null) return windowCap();
  return Math.max(CTX_HEIGHT_MIN, Math.min(windowCap(), Math.round(top - CTX_TOP_GAP)));
}

function readCtxHeight(): number {
  // `readStoredString` answers "" for a missing key — `Number("")` is 0, which
  // used to pin the panel to the 300px floor instead of the 460px default.
  const raw = readStoredString(CTX_HEIGHT_KEY).trim();
  const n = raw ? Number(raw) : Number.NaN;
  if (!Number.isFinite(n)) return Math.min(CTX_HEIGHT_DEFAULT, windowCap());
  return Math.min(windowCap(), Math.max(CTX_HEIGHT_MIN, n));
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
  /** Picker-added workspaces the user can remove from the list. */
  removableKeys: string[];
  onRemoveGroup: (group: PickerGroup) => void;
  /** Edit levels: one workspace (or standalone repo) at a time. */
  single: boolean;
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
 * Where the agent works: a workspace (folder of service repos), some of its
 * services, or a standalone repo (empty = current repo). Below it, the summaries
 * attached up front (changes / git identity / repos and branches).
 */
export default function ContextPicker(props: Props) {
  const { t } = useTranslation("aihub");
  const {
    open, onToggleOpen, groups, selection, label, currentRepo, onToggleGroup, onToggleService,
    onClear, onScanFolder, scanning, removableKeys, onRemoveGroup, single, chips, setChip,
    onPreview, previewLoading, included, pinned, locked, includeTip, onToggleInclude,
  } = props;
  const [searchOpen, setSearchOpen] = useState(false);
  const [partsOpen, setPartsOpen] = useState(() => readStoredString(CTX_PARTS_KEY) !== "0");
  const togglePartsOpen = () => {
    setPartsOpen((prev) => {
      writeStoredString(CTX_PARTS_KEY, prev ? "0" : "1");
      return !prev;
    });
  };
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
  const rootRef = useRef<HTMLDivElement | null>(null);
  // The panel shrinks to its content until the user grabs the handle; from that
  // moment (this open only) it keeps the dragged height even if content is short,
  // otherwise pulling up would visibly do nothing.
  const [fixedHeight, setFixedHeight] = useState(false);
  const fixedHeightRef = useRef(false);

  // The panel is capped, not fixed: short lists stay compact and the cap keeps
  // following the space above the trigger row when the window shrinks.
  useEffect(() => {
    if (!open) return;
    const clamp = () => {
      const next = Math.min(popHeightRef.current, ctxCap(rootRef.current));
      if (next === popHeightRef.current) return;
      popHeightRef.current = next;
      setPopHeight(next);
    };
    clamp();
    window.addEventListener("resize", clamp);
    return () => window.removeEventListener("resize", clamp);
  }, [open]);

  // Reopen compact: the dragged size stays stored as the cap, the mode does not.
  useEffect(() => {
    if (open) return;
    fixedHeightRef.current = false;
    setFixedHeight(false);
  }, [open]);

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
    setSearchOpen(false);
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
    if (!fixedHeightRef.current) {
      fixedHeightRef.current = true;
      setFixedHeight(true);
    }
    const next = Math.min(ctxCap(rootRef.current), Math.max(CTX_HEIGHT_MIN, drag.h + (drag.y - e.clientY)));
    popHeightRef.current = next;
    setPopHeight(next);
  };
  const onResizeUp = () => {
    if (!resizeDrag.current) return;
    resizeDrag.current = null;
    writeStoredString(CTX_HEIGHT_KEY, String(Math.round(popHeightRef.current)));
  };

  return (
    <div ref={rootRef} className="qa-focus qa-ctx-picker">
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
          <ChevronDown size={13} className={`qa-ctx-caret-icon${open ? " is-open" : ""}`} />
        </button>
      </div>

      {open && (
        <div
          className="qa-focus-pop qa-ctx-pop is-left is-up"
          style={fixedHeight ? { height: popHeight } : { maxHeight: popHeight }}
        >
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
          <div className="qa-focus-pop-head qa-ctx-head">
            <span className="qa-ctx-head-title">{t("ctx.reposHead")}</span>
            <div className="qa-ctx-head-actions">
              {selection.length > 0 && (
                <button type="button" className="qa-mini-btn" onClick={onClear}>
                  <X size={12} /> {t("focusClear")}
                </button>
              )}
              <button
                type="button"
                className={`qa-ctx-head-btn${searchOpen ? " on" : ""}`}
                onClick={() => {
                  if (searchOpen) setQuery("");
                  setSearchOpen(!searchOpen);
                }}
                title={t("ctx.filter")}
                aria-label={t("ctx.filter")}
                aria-pressed={searchOpen}
              >
                <Search size={14} />
              </button>
              <button
                type="button"
                className={`qa-ctx-head-btn${partsOpen ? " on" : ""}`}
                onClick={togglePartsOpen}
                title={t("ctx.partsToggle")}
                aria-label={t("ctx.partsToggle")}
                aria-pressed={partsOpen}
              >
                <SlidersHorizontal size={14} />
              </button>
              <button
                type="button"
                className="qa-ctx-head-btn"
                onClick={onScanFolder}
                disabled={scanning}
                title={t("focusScanFolder")}
                aria-label={t("focusScanFolder")}
              >
                {scanning ? <span className="qa-spinner" /> : <FolderPlus size={14} />}
              </button>
            </div>
          </div>
          {searchOpen && (
            <label className="qa-ctx-filter">
              <Search size={12} />
              <input
                className="input-field"
                value={query}
                autoFocus
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key !== "Escape") return;
                  e.stopPropagation();
                  setQuery("");
                  setSearchOpen(false);
                }}
                placeholder={t("ctx.filter")}
              />
            </label>
          )}
          {single && <p className="qa-ctx-note">{t("ctx.singleNote")}</p>}
          <div className="qa-focus-list qa-ctx-tree">
            {visible.length === 0 && <span className="ai-hub-hint">{t("focusEmpty")}</span>}
            {visible.map((g) => {
              const full = groups.find((x) => x.key === g.key) || g;
              const state = groupState(full, selection);
              const isOpen = Boolean(q) || expanded.has(g.key);
              const groupName = g.path ? g.name : t("ctx.otherGroup");
              // A one-service workspace whose service IS the workspace folder
              // itself (an added repo / current repo): no child row — the group
              // row already is the service.
              const selfService =
                g.path != null &&
                full.repos.length === 1 &&
                pathKey(full.repos[0].path) === pathKey(g.path);
              return (
                <div key={g.key} className="qa-ctx-group-wrap">
                  <div className={`qa-ctx-group${state !== "none" ? " on" : ""}`}>
                    {selfService ? (
                      <span className="qa-ctx-group-caret is-leaf" aria-hidden />
                    ) : (
                      <button
                        type="button"
                        className={`qa-ctx-group-caret${isOpen ? " is-open" : ""}`}
                        onClick={() => toggleExpanded(g.key)}
                        aria-expanded={isOpen}
                        aria-label={groupName}
                      >
                        <ChevronRight size={12} />
                      </button>
                    )}
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
                    {removableKeys.includes(g.key) && (
                      <button
                        type="button"
                        className="qa-ctx-group-x"
                        onClick={(e) => {
                          e.stopPropagation();
                          onRemoveGroup(full);
                        }}
                        title={t("ctx.removeWs")}
                        aria-label={t("ctx.removeWs")}
                      >
                        <X size={11} />
                      </button>
                    )}
                  </div>
                  {isOpen && !selfService && (
                    <div className="qa-ctx-children">
                      {g.repos.length === 0 && <span className="ai-hub-hint">{t("ctx.emptyWs")}</span>}
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
          {partsOpen && (
            <>
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
            </>
          )}
        </div>
      )}
    </div>
  );
}
