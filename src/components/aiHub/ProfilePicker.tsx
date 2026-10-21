import { useTranslation } from "react-i18next";
import { Check, SlidersHorizontal, UserRound } from "lucide-react";
import type { AiHubProfile } from "../../core/types";

type Props = {
  profiles: AiHubProfile[];
  /** Active profile id; "" = no profile (plain prompts only). */
  value: string;
  onChange: (id: string) => void;
  onManage: () => void;
  open: boolean;
  onToggle: () => void;
};

/**
 * Persona / temperature profile for the next round. Mirrors the permission pill:
 * switching mid-answer is fine — the running round keeps the one it started with.
 */
export default function ProfilePicker({ profiles, value, onChange, onManage, open, onToggle }: Props) {
  const { t } = useTranslation("aihub");
  const current = profiles.find((p) => p.id === value) || null;
  const label = current ? t(current.nameKey || "", { defaultValue: current.name }) : t("profile.none");
  return (
    <div className="qa-focus qa-profile">
      <button
        type="button"
        className={`qa-perm-pill is-profile${current ? " on" : ""}`}
        onClick={onToggle}
        title={`${t("profile.title")} · ${label}`}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <UserRound size={12} />
        <span className="qa-perm-label">{label}</span>
        <span className="qa-focus-caret">▾</span>
      </button>
      {open && (
        <div className="qa-focus-pop is-left is-up qa-profile-pop" role="listbox">
          <div className="qa-focus-pop-head">
            <span>{t("profile.title")}</span>
            <button type="button" className="qa-mini-btn" onClick={onManage}>
              <SlidersHorizontal size={11} /> {t("profile.manage")}
            </button>
          </div>
          <div className="qa-focus-list">
            <button
              type="button"
              role="option"
              aria-selected={!current}
              className={`qa-focus-item is-row${!current ? " on" : ""}`}
              onClick={() => {
                onChange("");
                onToggle();
              }}
            >
              <span className={`qa-focus-check${!current ? " on" : ""}`}>{!current && <Check size={11} />}</span>
              <span className="qa-focus-name">{t("profile.none")}</span>
            </button>
            {profiles.map((p) => {
              const on = p.id === value;
              return (
                <button
                  key={p.id}
                  type="button"
                  role="option"
                  aria-selected={on}
                  className={`qa-focus-item is-row qa-profile-item${on ? " on" : ""}`}
                  onClick={() => {
                    onChange(p.id);
                    onToggle();
                  }}
                  title={p.systemRole || undefined}
                >
                  <span className={`qa-focus-check${on ? " on" : ""}`}>{on && <Check size={11} />}</span>
                  <span className="qa-focus-name">{t(p.nameKey || "", { defaultValue: p.name })}</span>
                  {p.temperature != null && (
                    <span className="qa-profile-temp" title={t("profile.tempHint")}>
                      T {p.temperature.toFixed(1)}
                    </span>
                  )}
                </button>
              );
            })}
            {profiles.length === 0 && (
              <span className="ai-hub-hint">{t("profile.empty")}</span>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
