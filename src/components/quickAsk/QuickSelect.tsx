import { useEffect, useRef } from "react";
import { Check } from "lucide-react";

export type QuickOption = { value: string; label: string; short?: string };

type Props = {
  value: string;
  options: QuickOption[];
  onChange: (v: string) => void;
  title?: string;
  placeholder?: string;
  /** Extra class on the wrapper (e.g. sizing the trigger for another page). */
  className?: string;
  /** Anchor the popover to the left edge of the trigger (default: right). */
  alignLeft?: boolean;
  /** Controlled by the shell so opening one popover closes the others. */
  open: boolean;
  onToggle: () => void;
};

/**
 * Single-select popover styled like FocusPicker's frosted panel — native
 * <select> popups are OS-drawn and can't match the window's glass surface.
 */
export default function QuickSelect({
  value,
  options,
  onChange,
  title,
  placeholder,
  className,
  alignLeft,
  open,
  onToggle,
}: Props) {
  const current = options.find((o) => o.value === value);
  const triggerLabel = (current?.short ?? current?.label) || placeholder || "";
  const selectedRef = useRef<HTMLButtonElement | null>(null);
  // Long model lists scroll the current pick into view when the popover opens.
  useEffect(() => {
    if (open) selectedRef.current?.scrollIntoView({ block: "nearest" });
  }, [open]);
  return (
    <div className={className ? `qa-focus ${className}` : "qa-focus"}>
      <button
        type="button"
        className={`input-field qa-focus-btn${open ? " is-open" : ""}`}
        onClick={onToggle}
        title={title || current?.label || placeholder}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <span className="qa-focus-btn-label">{triggerLabel}</span>
        <span className="qa-focus-caret">▾</span>
      </button>
      {open && (
        <div className={`qa-focus-pop${alignLeft ? " is-left" : ""}`} role="listbox">
          <div className="qa-focus-list">
            {options.map((o) => {
              const on = o.value === value;
              return (
                <button
                  key={o.value}
                  type="button"
                  role="option"
                  aria-selected={on}
                  ref={on ? selectedRef : undefined}
                  className={`qa-focus-item is-row${on ? " on" : ""}`}
                  onClick={() => {
                    onChange(o.value);
                    onToggle();
                  }}
                >
                  <span className={`qa-focus-check${on ? " on" : ""}`}>{on && <Check size={11} />}</span>
                  <span className="qa-focus-name" title={o.label}>
                    {o.label}
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
