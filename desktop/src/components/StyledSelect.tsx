import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
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

type MenuPosition = {
  placement: "above" | "below";
  top: number;
  bottom: number;
  left: number;
  width: number;
  maxHeight: number;
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
  const [menuPosition, setMenuPosition] = useState<MenuPosition | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const optionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const listId = useId();
  const selected = options.find((o) => o.value === value) ?? options[0];
  const selectedIndex = Math.max(
    0,
    options.findIndex((o) => o.value === value),
  );

  function enabledIndex(start: number, direction: 1 | -1 = 1) {
    if (options.length === 0) return 0;
    let index = Math.max(0, Math.min(start, options.length - 1));
    for (let count = 0; count < options.length; count += 1) {
      if (!options[index]?.disabled) return index;
      index = (index + direction + options.length) % options.length;
    }
    return 0;
  }

  const close = useCallback((restoreFocus = false) => {
    setOpen(false);
    setMenuPosition(null);
    if (restoreFocus) {
      triggerRef.current?.focus();
    }
  }, []);

  const openMenu = useCallback(() => {
    if (disabled || options.length === 0) return;
    setActiveIndex(enabledIndex(selectedIndex));
    setOpen(true);
  }, [disabled, options.length, selectedIndex]);

  const positionMenu = useCallback(() => {
    const trigger = triggerRef.current;
    if (!trigger || typeof window === "undefined") return;

    const rect = trigger.getBoundingClientRect();
    const edge = 8;
    const gap = 4;
    const desiredHeight = Math.min(240, Math.max(64, options.length * 34 + 8));
    const spaceBelow = window.innerHeight - rect.bottom - edge;
    const spaceAbove = rect.top - edge;
    const opensAbove =
      spaceBelow < desiredHeight && spaceAbove > spaceBelow;
    const available = Math.max(
      32,
      Math.min(desiredHeight, (opensAbove ? spaceAbove : spaceBelow) - gap),
    );
    const width = Math.min(
      Math.max(rect.width, 160),
      Math.max(1, window.innerWidth - edge * 2),
    );
    const left = Math.min(
      Math.max(edge, rect.left),
      Math.max(edge, window.innerWidth - width - edge),
    );

    setMenuPosition({
      placement: opensAbove ? "above" : "below",
      top: rect.bottom + gap,
      bottom: window.innerHeight - rect.top + gap,
      left,
      width,
      maxHeight: available,
    });
  }, [options.length]);

  useLayoutEffect(() => {
    if (!open) return;
    positionMenu();
    const onViewportChange = () => positionMenu();
    window.addEventListener("resize", onViewportChange);
    window.addEventListener("scroll", onViewportChange, true);
    return () => {
      window.removeEventListener("resize", onViewportChange);
      window.removeEventListener("scroll", onViewportChange, true);
    };
  }, [open, positionMenu]);

  useLayoutEffect(() => {
    if (!open) return;
    optionRefs.current[activeIndex]?.focus();
  }, [activeIndex, open]);

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
    if (
      e.key === "ArrowDown" ||
      e.key === "ArrowUp" ||
      e.key === "Enter" ||
      e.key === " "
    ) {
      e.preventDefault();
      openMenu();
    }
  }

  function focusOption(index: number) {
    const direction = index < activeIndex ? -1 : 1;
    const next = enabledIndex(index, direction);
    setActiveIndex(next);
    optionRefs.current[next]?.focus();
  }

  function selectOption(index: number) {
    const option = options[index];
    if (!option || option.disabled) return;
    onChange(option.value);
    close(true);
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
        ref={triggerRef}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        aria-label={ariaLabel}
        onClick={() => (open ? close() : openMenu())}
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
          data-placement={menuPosition?.placement}
          style={
            menuPosition
              ? {
                  position: "fixed",
                  left: menuPosition.left,
                  top:
                    menuPosition.placement === "below"
                      ? menuPosition.top
                      : "auto",
                  bottom:
                    menuPosition.placement === "above"
                      ? menuPosition.bottom
                      : "auto",
                  width: menuPosition.width,
                  minWidth: menuPosition.width,
                  maxWidth: menuPosition.width,
                  maxHeight: menuPosition.maxHeight,
                }
              : { visibility: "hidden" }
          }
          role="listbox"
          aria-activedescendant={
            options[activeIndex]
              ? `${listId}-${options[activeIndex].value}`
              : undefined
          }
        >
          {options.map((o, index) => {
            const isSel = o.value === value;
            return (
              <li key={o.value} role="presentation">
                <button
                  type="button"
                  role="option"
                  id={`${listId}-${o.value}`}
                  aria-selected={isSel}
                  disabled={o.disabled}
                  ref={(node) => {
                    optionRefs.current[index] = node;
                  }}
                  className={`styled-select-option ${isSel ? "is-selected" : ""}`}
                  onFocus={() => setActiveIndex(index)}
                  onMouseEnter={() => setActiveIndex(index)}
                  onKeyDown={(e) => {
                    if (e.key === "ArrowDown") {
                      e.preventDefault();
                      focusOption(index + 1);
                    } else if (e.key === "ArrowUp") {
                      e.preventDefault();
                      focusOption(index - 1);
                    } else if (e.key === "Home") {
                      e.preventDefault();
                      focusOption(0);
                    } else if (e.key === "End") {
                      e.preventDefault();
                      focusOption(options.length - 1);
                    } else if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      selectOption(index);
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      close(true);
                    } else if (e.key === "Tab") {
                      close();
                    }
                  }}
                  onClick={() => selectOption(index)}
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
