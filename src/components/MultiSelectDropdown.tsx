import { useEffect, useRef, useState } from "react"
import { ChevronDown } from "lucide-react"

export interface MultiSelectOption {
  value: string
  label: string
}

interface Props {
  label: string
  options: MultiSelectOption[]
  selected: string[]
  onChange: (values: string[]) => void
}

export default function MultiSelectDropdown({ label, options, selected, onChange }: Props) {
  const [open, setOpen] = useState(false)
  const rootRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    if (!open) return
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false)
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false)
    }
    document.addEventListener("pointerdown", onPointerDown)
    document.addEventListener("keydown", onKeyDown)
    return () => {
      document.removeEventListener("pointerdown", onPointerDown)
      document.removeEventListener("keydown", onKeyDown)
    }
  }, [open])

  const toggleValue = (value: string) =>
    onChange(selected.includes(value) ? selected.filter((item) => item !== value) : [...selected, value])

  return (
    <div ref={rootRef} className={"tr-multiselect" + (open ? " is-open" : "")}>
      <button
        type="button"
        className="tr-multiselect-trigger"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        aria-haspopup="listbox"
      >
        <span>
          {label}
          {selected.length > 0 ? " · " + selected.length : ""}
        </span>
        <ChevronDown size={12} className="tr-multiselect-chevron" aria-hidden />
      </button>
      {open && (
        <div className="tr-multiselect-menu" role="listbox" aria-label={label} aria-multiselectable="true">
          {options.length === 0 ? (
            <span className="tr-multiselect-empty">—</span>
          ) : (
            options.map((option) => (
              <label
                key={option.value}
                className={"tr-multiselect-option" + (selected.includes(option.value) ? " is-selected" : "")}
                role="option"
                aria-selected={selected.includes(option.value)}
              >
                <input
                  type="checkbox"
                  checked={selected.includes(option.value)}
                  onChange={() => toggleValue(option.value)}
                />
                <span>{option.label}</span>
              </label>
            ))
          )}
        </div>
      )}
    </div>
  )
}
