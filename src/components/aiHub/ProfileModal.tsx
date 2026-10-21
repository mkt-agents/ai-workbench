import { useTranslation } from "react-i18next";
import { Plus, Trash2 } from "lucide-react";
import type { AiHubProfile } from "../../core/types";
import { BUILTIN_PROFILES, newCustomProfile } from "../../lib/aiHubProfiles";
import ModalTitleRow from "../ModalTitleRow";

type Props = {
  /** Custom profiles only; built-ins are shown read-only from the module list. */
  custom: AiHubProfile[];
  onChange: (next: AiHubProfile[]) => void;
  onClose: () => void;
};

/**
 * Editor for 智枢 profiles: an extra system role plus an optional temperature
 * override. Custom ones live in settings (localStorage), not SQLite.
 */
export default function ProfileModal({ custom, onChange, onClose }: Props) {
  const { t } = useTranslation("aihub");
  const patch = (id: string, next: Partial<AiHubProfile>) =>
    onChange(custom.map((p) => (p.id === id ? { ...p, ...next } : p)));
  const remove = (id: string) => onChange(custom.filter((p) => p.id !== id));

  const rows = [...BUILTIN_PROFILES, ...custom];
  return (
    <div className="modal-overlay" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal modal-md" role="dialog" aria-modal="true" aria-label={t("profile.manage")}>
        <ModalTitleRow title={t("profile.manage")} onClose={onClose} />
        <p className="qa-profile-note">{t("profile.tip")}</p>
        <p className="qa-profile-note">{t("profile.tempHint")}</p>
        <div className="qa-profile-list">
          {rows.map((p) => {
            const isBuiltin = p.builtin === true;
            return (
              <div key={p.id} className={`qa-profile-card${isBuiltin ? " is-builtin" : ""}`}>
                <input
                  className="input-field qa-profile-name"
                  value={p.name}
                  readOnly={isBuiltin}
                  onChange={(e) => patch(p.id, { name: e.target.value })}
                  placeholder={t("profile.name")}
                  aria-label={t("profile.name")}
                />
                <textarea
                  className="input-field qa-profile-role"
                  value={p.systemRole || ""}
                  readOnly={isBuiltin}
                  rows={2}
                  onChange={(e) => patch(p.id, { systemRole: e.target.value })}
                  placeholder={t("profile.role")}
                  aria-label={t("profile.role")}
                />
                <div className="qa-profile-foot">
                  <div className={`qa-profile-temp-row${p.temperature == null ? " is-off" : ""}`}>
                    <span>{t("profile.temp")}</span>
                    <input
                      type="range"
                      min={0}
                      max={1}
                      step={0.1}
                      value={p.temperature ?? 0}
                      disabled={isBuiltin}
                      aria-label={t("profile.temp")}
                      onChange={(e) => patch(p.id, { temperature: Number(e.target.value) })}
                    />
                    <span className="qa-profile-temp-value">
                      {p.temperature == null ? t("profile.tempUnset") : p.temperature.toFixed(1)}
                    </span>
                    {!isBuiltin && p.temperature != null && (
                      <button
                        type="button"
                        className="qa-mini-btn"
                        onClick={() => patch(p.id, { temperature: undefined })}
                      >
                        {t("profile.tempAuto")}
                      </button>
                    )}
                  </div>
                  {!isBuiltin && (
                    <button type="button" className="btn-icon" onClick={() => remove(p.id)} title={t("profile.remove")}>
                      <Trash2 size={13} />
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </div>
        <div className="modal-actions modal-actions--split">
          <div className="modal-actions-secondary">
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => onChange([...custom, newCustomProfile({ name: t("profile.newName") })])}
            >
              <Plus size={13} /> {t("profile.add")}
            </button>
          </div>
          <div className="modal-actions-primary">
            <button type="button" className="btn btn-primary btn-small" onClick={onClose}>
              {t("done")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
