import { useEffect, useRef } from "react";
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
  /** 编辑档的写入也先弹 diff 确认。 */
  diffConfirm: boolean;
  onDiffConfirm: (on: boolean) => void;
  open: boolean;
  onToggle: () => void;
};

/**
 * Composer permission level (mirrors DeepSeek Harness): what the AI may do in the repo.
 * Switchable any time, even mid-answer — it applies to the next message.
 */
export default function PermissionPicker({
  permission, onPermission, diffConfirm, onDiffConfirm, open, onToggle,
}: Props) {
  const { t } = useTranslation("aihub");
  const Icon = ICONS[permission];
  const pillRef = useRef<HTMLButtonElement>(null);
  const wasOpen = useRef(false);
  /**
   * A menu hands focus back to its trigger. Without this, cancelling the 完全权限
   * second confirmation leaves focus on <body> — the popover is already gone.
   */
  useEffect(() => {
    if (wasOpen.current && !open) pillRef.current?.focus();
    wasOpen.current = open;
  }, [open]);
  return (
    <div className="qa-focus qa-perm">
      <button
        type="button"
        ref={pillRef}
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
          {/* 编辑档才可能有「直接落地」的写入，所以这一项只在编辑/审查档有意义 */}
          <label className="qa-perm-diff" title={t("permission.diffConfirmHint")}>
            <input
              type="checkbox"
              checked={diffConfirm}
              onChange={(e) => onDiffConfirm(e.target.checked)}
            />
            <span>{t("permission.diffConfirm")}</span>
          </label>
          <div className="qa-perm-foot">{t("permission.shortcutHint")}</div>
        </div>
      )}
    </div>
  );
}
