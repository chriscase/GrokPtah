/** Sequential tab stops for Help and consent dialog traps. */
export const MODAL_FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "a[href]",
  "summary",
  '[tabindex]:not([tabindex="-1"])',
].join(", ");

/**
 * Tab-cycle members for a modal trap.
 *
 * Native `<summary>` is included even without an explicit tabindex.
 * `tabindex="-1"` nodes (including unselected roving options) are omitted
 * so they cannot re-enter the trap; use Arrow keys / programmatic focus.
 */
export function focusableIn(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(MODAL_FOCUSABLE_SELECTOR)).filter(
    (element) =>
      !element.closest("[inert]") && element.getAttribute("tabindex") !== "-1",
  );
}
