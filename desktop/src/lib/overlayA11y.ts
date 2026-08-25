/**
 * Shared overlay accessibility primitives for operator chrome.
 *
 * The Help Center already implements a correct focus trap, background inert
 * treatment and layered Escape handling. Consent — the surface that gates every
 * tool execution — had none of it. Rather than grow a second implementation,
 * the mechanism lives here so every authority boundary uses the same one.
 *
 * This module is desktop-internal: it is deliberately **not** re-exported from
 * `uiCore`/`public`, so no browser consumer can mistake DOM focus mechanics for
 * part of the transport-neutral contract surface.
 */
import { useEffect, useRef } from "react";

export const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "summary",
  '[tabindex]:not([tabindex="-1"])',
].join(", ");

/** Overlays that take keyboard authority away from the workspace shell. */
export type ChromeLockFlags = {
  settingsOpen?: boolean;
  sessionBrowserOpen?: boolean;
  permissionOpen?: boolean;
  searchOpen?: boolean;
  aboutOpen?: boolean;
  mcpTrustOpen?: boolean;
  helpOpen?: boolean;
  debugOpen?: boolean;
};

/**
 * True while any modal, consent or overlay authority boundary is open.
 *
 * Global workspace shortcuts must be suppressed whenever this is true: an
 * operator judging a consent prompt must not be able to switch session, cycle
 * docks or collapse chrome underneath the decision they are about to make.
 */
export function isChromeLocked(flags: ChromeLockFlags): boolean {
  return Boolean(
    flags.settingsOpen ||
      flags.sessionBrowserOpen ||
      flags.permissionOpen ||
      flags.searchOpen ||
      flags.aboutOpen ||
      flags.mcpTrustOpen ||
      flags.helpOpen ||
      flags.debugOpen,
  );
}

/**
 * Overlays that inert their own background from inside the component, via
 * `inertSiblings`. The consent prompt and the Help Center both do.
 *
 * `inert` must have exactly one owner per element. Two owners — a React prop on
 * the landmark and a save/restore helper — read each other's writes: the helper
 * records `inert: true` (written by React), React later removes the attribute
 * when the overlay closes, and the helper's cleanup then puts it back with
 * nothing left to take it off. The shell stays permanently non-interactive.
 *
 * So the shell only inerts the overlays that do not inert themselves.
 */
const SELF_INERTING_FLAGS = ["permissionOpen", "helpOpen"] as const;

/**
 * True while the shell must inert its own landmarks — that is, while an overlay
 * is open that does *not* manage the background itself.
 *
 * Callers pass the same flag object they pass to {@link isChromeLocked}, so the
 * two can never drift apart.
 */
export function isShellInert(flags: ChromeLockFlags): boolean {
  const owned: ChromeLockFlags = { ...flags };
  for (const flag of SELF_INERTING_FLAGS) owned[flag] = false;
  return isChromeLocked(owned);
}

/** React 18's DOM types omit `inert`; spreading a string attribute still works. */
export function inertProps(locked: boolean): Record<string, string> {
  return locked ? { inert: "" } : {};
}

export function focusableIn(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(
    root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
  ).filter(
    (element) =>
      !element.closest("[inert]") &&
      element.getAttribute("aria-hidden") !== "true" &&
      !element.hasAttribute("disabled") &&
      element.tabIndex !== -1,
  );
}

type TrapEvent = {
  key: string;
  shiftKey: boolean;
  preventDefault: () => void;
};

/**
 * Wrap Tab / Shift-Tab inside `root`.
 *
 * Also pulls focus back when it has escaped the root entirely — without that,
 * a dialog opened while focus sat in the composer would leak the very first
 * Tab into the application behind it.
 */
export function trapTabKey(event: TrapEvent, root: HTMLElement | null): void {
  if (event.key !== "Tab" || !root) return;
  const nodes = focusableIn(root);
  if (nodes.length === 0) return;
  const first = nodes[0];
  const last = nodes[nodes.length - 1];
  const active = document.activeElement;
  const outside = !(active instanceof Node) || !root.contains(active);
  if (event.shiftKey) {
    if (outside || active === first) {
      event.preventDefault();
      last.focus();
    }
    return;
  }
  if (outside || active === last) {
    event.preventDefault();
    first.focus();
  }
}

/**
 * Mark every sibling of `layer` inert and aria-hidden, returning an exact
 * restore function.
 *
 * `exemptModalLayers` keeps a higher-authority sibling reachable — the Help
 * Center uses it so a consent prompt can still be raised above it.
 */
export function inertSiblings(
  layer: HTMLElement | null,
  exemptModalLayers: readonly string[] = [],
): () => void {
  const shell = layer?.parentElement;
  if (!layer || !shell) return () => {};
  const siblings = Array.from(shell.children).filter(
    (child): child is HTMLElement =>
      child !== layer &&
      child instanceof HTMLElement &&
      !exemptModalLayers.includes(child.dataset.modalLayer ?? ""),
  );
  const previous = siblings.map((element) => ({
    element,
    ariaHidden: element.getAttribute("aria-hidden"),
    inert: element.hasAttribute("inert"),
  }));
  siblings.forEach((element) => {
    element.setAttribute("inert", "");
    element.setAttribute("aria-hidden", "true");
  });
  return () => {
    previous.forEach(({ element, ariaHidden, inert }) => {
      if (inert) element.setAttribute("inert", "");
      else element.removeAttribute("inert");
      if (ariaHidden === null) element.removeAttribute("aria-hidden");
      else element.setAttribute("aria-hidden", ariaHidden);
    });
  };
}

export type DialogFocusOptions = {
  /** The layer whose siblings become inert — usually the backdrop. */
  layerRef: { current: HTMLElement | null };
  /** Where focus lands on open. For a consent gate this must be the safe answer. */
  initialFocusRef: { current: HTMLElement | null };
  /** Re-runs initial focus when this changes (e.g. the next queued request). */
  focusKey?: string;
  /** Sibling `data-modal-layer` values that stay reachable. */
  exemptModalLayers?: readonly string[];
};

/**
 * Deterministic dialog focus lifecycle: capture the opener, place initial
 * focus, make the background inert, and restore the opener on every terminal
 * path — including the ones a caller forgets, because unmount is the only
 * place restoration is guaranteed to run.
 */
export function useDialogFocus({
  layerRef,
  initialFocusRef,
  focusKey,
  exemptModalLayers = [],
}: DialogFocusOptions): void {
  const openerRef = useRef<HTMLElement | null>(null);
  // Read through refs so the mount-only effect never captures a stale layer.
  const layer = layerRef;
  const exempt = useRef(exemptModalLayers);
  exempt.current = exemptModalLayers;

  useEffect(() => {
    openerRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    const restoreInert = inertSiblings(layer.current, exempt.current);
    return () => {
      restoreInert();
      const opener = openerRef.current;
      openerRef.current = null;
      if (opener && opener.isConnected) opener.focus();
    };
    // Mount/unmount only: the opener must be the element focused before the
    // dialog appeared, not whatever the next queued request re-rendered over.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    initialFocusRef.current?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusKey]);
}
