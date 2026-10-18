import { useTranslation } from "react-i18next";
import { Check, Eye, FilePen, ShieldAlert, ShieldCheck, type LucideIcon } from "lucide-react";
import { AGENT_PERMISSIONS, type AgentPermission } from "./useAskStream";

const ICONS: Record<AgentPermission, LucideIcon> = {
  read: Eye,
  workspace: FilePen,
  review: ShieldCheck,
  full: ShieldAlert,
};

type Props = {
  permission: AgentPermission;
  onPermission: (next: AgentPermission) => void;
  open: boolean;
  onToggle: () => void;
};

/**
 * Composer permission level (mirrors DeepSeek Harness): what the AI may do in the repo.
 * Switchable any time, even mid-answer — it applies to the next message.
 */
export default function PermissionPicker({ permission, onPermission, open, onToggle }: Props) {
  const { t } = useTranslation("aihub");
  const Icon = ICONS[permission];
  return (
    <div className="qa-focus qa-perm">
      <button
        type="button"
        className={`qa-perm-pill is-${permission}`}
        onClick={onToggle}
        title={`${t(`permission.${permission}Desc`)} · ${t("permission.shortcut")}`}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <Icon size={12} />
        <span className="qa-perm-label">{t(`permission.${permission}`)}</span>
        {permission === "review" && <span className="qa-perm-exp">{t("permission.exp")}</span>}
        <span className="qa-focus-caret">▾</span>
      </button>
      {open && (
        <div className="qa-focus-pop is-left is-up qa-perm-pop" role="listbox">
          {AGENT_PERMISSIONS.map((p) => {
            const ItemIcon = ICONS[p];
            const on = p === permission;
            return (
              <div key={p}>
                {/* 完全权限单独隔开，减少误点 */}
                {p === "full" && <div className="qa-perm-sep" role="separator" />}
                <button
                  type="button"
                  role="option"
                  aria-selected={on}
                  className={`qa-focus-item qa-perm-item is-${p}${on ? " on" : ""}`}
                  onClick={() => {
                    onPermission(p);
                    onToggle();
                  }}
                >
                  <ItemIcon size={13} className="qa-perm-item-icon" />
                  <span className="qa-perm-item-text">
                    <span className="qa-perm-item-name">
                      {t(`permission.${p}`)}
                      {p === "review" && <span className="qa-perm-exp">{t("permission.exp")}</span>}
                    </span>
                    <span className="qa-perm-item-desc">{t(`permission.${p}Desc`)}</span>
                  </span>
                  {on && <Check size={12} className="qa-perm-item-check" />}
                </button>
              </div>
            );
          })}
          <div className="qa-perm-foot">{t("permission.shortcutHint")}</div>
        </div>
      )}
    </div>
  );
}
