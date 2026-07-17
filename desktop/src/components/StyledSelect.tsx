import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";

export type StyledSelectOption = {
  value: string;
  label: string;
  disabled?: boolean;
};

export type StyledSelectProps = {
  value: string;
  options: StyledSelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  /** Accessible name when no visible label is associated. */
  "aria-label"?: string;
  className?: string;
  id?: string;
};

/**
 * Custom dropdown matching GrokPtah amber chrome — replaces native &lt;select&gt;
 * so Settings/composer never show OS chevrons (#126).
 */
export function StyledSelect({
  value,
  options,
  onChange,
  disabled = false,
  className = "",
  id,
  "aria-label": ariaLabel,
}: StyledSelectProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const listId = useId();
  const selected = options.find((o) => o.value === value) ?? options[0];

  const close = useCallback(() => setOpen(false), []);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) close();
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, close]);

  function onTriggerKey(e: KeyboardEvent) {
    if (disabled) return;
    if (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      setOpen(true);
    }
  }

  return (
    <div
      className={`styled-select ${open ? "is-open" : ""} ${disabled ? "is-disabled" : ""} ${className}`}
      ref={rootRef}
      data-testid="styled-select"
    >
      <button
        type="button"
        id={id}
        className="styled-select-trigger"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        aria-label={ariaLabel}
        onClick={() => !disabled && setOpen((v) => !v)}
        onKeyDown={onTriggerKey}
      >
        <span className="styled-select-value">
          {selected?.label ?? value}
        </span>
        <span className="styled-select-chevron" aria-hidden>
          ▾
        </span>
      </button>
      {open && (
        <ul
          id={listId}
          className="styled-select-menu"
          role="listbox"
          aria-activedescendant={
            selected ? `${listId}-${selected.value}` : undefined
          }
        >
          {options.map((o) => {
            const isSel = o.value === value;
            return (
              <li key={o.value} role="presentation">
                <button
                  type="button"
                  role="option"
                  id={`${listId}-${o.value}`}
                  aria-selected={isSel}
                  disabled={o.disabled}
                  className={`styled-select-option ${isSel ? "is-selected" : ""}`}
                  onClick={() => {
                    if (o.disabled) return;
                    onChange(o.value);
                    close();
                  }}
                >
                  {o.label}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
