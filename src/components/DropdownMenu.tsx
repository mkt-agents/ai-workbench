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
}: {
  /** aria-label for the trigger button. */
  label: string;
  items: DropdownItem[];
  /** The trigger button's inner content (icon). */
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="dd" ref={ref}>
      <button
        type="button"
        className="btn btn-secondary btn-icon"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        onClick={() => setOpen((v) => !v)}
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
                  setOpen(false);
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
