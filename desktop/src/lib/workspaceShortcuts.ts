/**
 * Global workspace shortcut resolution.
 *
 * Extracted from App's capture-phase key handler so the one property that
 * matters can be proved rather than eyeballed: **no workspace shortcut resolves
 * while a modal, consent or overlay authority boundary is open.**
 *
 * The handler previously gated on the modifier key alone, so ⌘1–⌘6, ⌘\, ⌘B,
 * ⌘⌥B, ⌘⇧L and ⌘⌥←/→ all stayed live underneath a permission prompt. An
 * operator could switch the active session, open a dock or collapse the panes
 * while judging a consent request that shows only a truncated session id —
 * exactly the condition under which the wrong approval gets granted.
 *
 * This module decides *which* shortcut a keystroke names. Whether the workspace
 * can currently service it (dock exists, split is possible) stays in App, where
 * that state lives.
 */

export type WorkspaceShortcut =
  | { kind: "toggle-sidebar" }
  | { kind: "toggle-rightbar" }
  | { kind: "toggle-live" }
  | { kind: "focus-dock"; index: number }
  | { kind: "cycle-dock"; delta: -1 | 1 }
  | { kind: "open-beside" };

export type ShortcutEvent = {
  key: string;
  code?: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
};

export type ShortcutContext = {
  /** True while any overlay authority boundary owns the keyboard. */
  chromeLocked: boolean;
};

export function resolveWorkspaceShortcut(
  event: ShortcutEvent,
  { chromeLocked }: ShortcutContext,
): WorkspaceShortcut | null {
  // The single gate that makes consent a real boundary. Everything below is
  // unreachable while an overlay is open.
  if (chromeLocked) return null;

  const meta = Boolean(event.metaKey || event.ctrlKey);
  if (!meta) return null;

  // ⌘B left chrome, ⌘⌥B right chrome, ⌘⇧L Live
  const isB = event.key === "b" || event.key === "B" || event.code === "KeyB";
  if (isB) {
    if (event.altKey) return { kind: "toggle-rightbar" };
    if (!event.shiftKey) return { kind: "toggle-sidebar" };
  }
  if (
    event.shiftKey &&
    (event.key === "l" || event.key === "L" || event.code === "KeyL")
  ) {
    return { kind: "toggle-live" };
  }

  // ⌘1–⌘6 focus dock by zone index
  if (
    event.key >= "1" &&
    event.key <= "6" &&
    event.key.length === 1 &&
    !event.altKey &&
    !event.shiftKey
  ) {
    return { kind: "focus-dock", index: Number(event.key) - 1 };
  }

  // ⌘⌥← / ⌘⌥→ cycle docks
  if (event.altKey && event.key === "ArrowLeft") {
    return { kind: "cycle-dock", delta: -1 };
  }
  if (event.altKey && event.key === "ArrowRight") {
    return { kind: "cycle-dock", delta: 1 };
  }

  // ⌘\ toggle multi-dock
  if (event.key === "\\") return { kind: "open-beside" };

  return null;
}
