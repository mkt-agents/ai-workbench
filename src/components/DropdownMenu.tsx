/**
 * Minimal overflow menu, shared across pages.
 *
 * A trigger button (the `children`) toggles a floating action list. The menu
 * closes on any outside click or on Escape. Used to keep crowded rows (the
 * project cards' eight action buttons) to a few primary actions.
 */
import { useEffect, useRef, useState, type ReactNode } from "react";

export type DropdownItem = {
  label: string;
  icon?: ReactNode;
  danger?: boolean;
  disabled?: boolean;
  onClick: () => void;
} | { divider: true };

export default function DropdownMenu({
  label,
  items,
  children,
  triggerClassName = "btn btn-secondary btn-icon",
  onOpenChange,
}: {
  /** aria-label for the trigger button. */
  label: string;
  items: DropdownItem[];
  /** The trigger button's inner content (icon). */
  children: ReactNode;
  triggerClassName?: string;
  /**
   * Called synchronously on every real open/close transition (including the
   * item-click close, before its action runs). Pages whose content is covered
   * by a native window use this to hide it while the menu is up.
   */
  onOpenChange?: (open: boolean) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  const changeOpen = (next: boolean) => {
    setOpen(next);
    onOpenChange?.(next);
  };

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) changeOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") changeOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, onOpenChange]);

  return (
    <div className="dd" ref={ref}>
      <button
        type="button"
        className={triggerClassName}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        title={label}
        onClick={() => changeOpen(!open)}
      >
        {children}
      </button>
      {open && (
        <div className="dd-menu" role="menu">
          {items.map((item, i) =>
            "divider" in item ? (
              <div key={i} className="dd-divider" role="separator" />
            ) : (
              <button
                key={i}
                type="button"
                role="menuitem"
                className={`dd-item${item.danger ? " is-danger" : ""}`}
                disabled={item.disabled}
                onClick={() => {
                  changeOpen(false);
                  item.onClick();
                }}
              >
                {item.icon}
                {item.label}
              </button>
            )
          )}
        </div>
      )}
    </div>
  );
}
