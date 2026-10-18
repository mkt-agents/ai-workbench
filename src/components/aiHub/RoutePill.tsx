import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, FolderGit2, MessageCircle } from "lucide-react";
import type { RouteOverride, RouteResult } from "./route";

const ROUTE_OPTIONS: RouteOverride[] = ["auto", "chat", "project"];

type Props = {
  /** Live routing decision for the text currently in the composer. */
  route: RouteResult;
  routeOverride: RouteOverride;
  onRouteOverride: (v: RouteOverride) => void;
  open: boolean;
  onToggle: () => void;
  /** Repo label used in project mode; null = no repo available at all. */
  projectLabel: string | null;
};

/** Composer pill showing chat / project routing, with a one-shot override menu. */
export default function RoutePill({ route, routeOverride, onRouteOverride, open, onToggle, projectLabel }: Props) {
  const { t } = useTranslation("aihub");
  // Flash the pill when the live decision flips between chat and project.
  const [flashKey, setFlashKey] = useState(0);
  const prevModeRef = useRef(route.mode);
  useEffect(() => {
    if (prevModeRef.current !== route.mode) setFlashKey((k) => k + 1);
    prevModeRef.current = route.mode;
  }, [route.mode]);

  const isProject = route.mode === "project";
  const label = isProject
    ? projectLabel
      ? t("route.pillProject", { label: projectLabel })
      : t("route.pillProjectNoRepo")
    : t("route.pillChat");
  const tip = `${t(`route.reason.${route.reason}`)}${routeOverride === "auto" ? "" : ` · ${t("route.overrideOnce")}`}`;

  return (
    <div className="qa-focus qa-route">
      <button
        type="button"
        className={`qa-route-pill is-${route.mode}${routeOverride !== "auto" ? " is-pinned" : ""}${
          isProject && !projectLabel ? " is-warn" : ""
        }`}
        onClick={onToggle}
        title={tip}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        {flashKey > 0 && <span key={flashKey} className="qa-route-flash" aria-hidden />}
        {isProject ? <FolderGit2 size={12} /> : <MessageCircle size={12} />}
        <span className="qa-route-label">{label}</span>
        {routeOverride === "auto" && <span className="qa-route-auto">{t("route.autoTag")}</span>}
        <span className="qa-focus-caret">▾</span>
      </button>
      {open && (
        <div className="qa-focus-pop is-left is-up qa-route-pop" role="listbox">
          <div className="qa-focus-pop-head">
            <span>{t("route.menuTitle")}</span>
          </div>
          <div className="qa-focus-list">
            {ROUTE_OPTIONS.map((opt) => {
              const on = opt === routeOverride;
              return (
                <button
                  key={opt}
                  type="button"
                  role="option"
                  aria-selected={on}
                  className={`qa-focus-item${on ? " on" : ""}`}
                  onClick={() => {
                    onRouteOverride(opt);
                    onToggle();
                  }}
                >
                  <span className={`qa-focus-check${on ? " on" : ""}`}>{on && <Check size={11} />}</span>
                  <span className="qa-focus-name">{t(`route.${opt}`)}</span>
                  <span className="qa-route-desc">{t(`route.${opt}Desc`)}</span>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
