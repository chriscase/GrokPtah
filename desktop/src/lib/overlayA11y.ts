/**
 * Shared overlay accessibility helpers for operator chrome.
 *
 * Keep this module free of wave-component files so it can ship as an isolated
 * certification slice while the bounded #367 repair stays on its 14-file list.
 */

export const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(", ");

export type ChromeLockFlags = {
  settingsOpen?: boolean;
  sessionBrowserOpen?: boolean;
  permissionOpen?: boolean;
  searchOpen?: boolean;
  aboutOpen?: boolean;
  mcpTrustOpen?: boolean;
};

export function isChromeLocked(flags: ChromeLockFlags): boolean {
  return Boolean(
    flags.settingsOpen ||
      flags.sessionBrowserOpen ||
      flags.permissionOpen ||
      flags.searchOpen ||
      flags.aboutOpen ||
      flags.mcpTrustOpen,
  );
}

/** React 18's DOM types omit `inert`; spreading a string attribute still works. */
export function inertProps(locked: boolean): Record<string, string> {
  return locked ? { inert: "" } : {};
}

export function focusableIn(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (el) =>
      el.getAttribute("aria-hidden") !== "true" &&
      !el.hasAttribute("disabled") &&
      el.tabIndex !== -1,
  );
}

export function trapTabKey(
  event: { key: string; shiftKey: boolean; preventDefault: () => void },
  root: HTMLElement | null,
): void {
  if (event.key !== "Tab" || !root) return;
  const nodes = focusableIn(root);
  if (nodes.length === 0) return;
  const first = nodes[0];
  const last = nodes[nodes.length - 1];
  const active = document.activeElement;
  if (event.shiftKey) {
    if (active === first || !root.contains(active)) {
      event.preventDefault();
      last.focus();
    }
  } else if (active === last || !root.contains(active)) {
    event.preventDefault();
    first.focus();
  }
}
