import type { PermissionRequest } from "./protocol";

/**
 * Pure helpers for the permission-modal queue (#141).
 *
 * Concurrent permission_required events must not clobber each other, and
 * answering a modal must target `request.session_id` — not whichever session
 * happens to be focused.
 */

/** Append a request if its id is new (idempotent). */
export function enqueuePermission(
  queue: PermissionRequest[],
  req: PermissionRequest,
): PermissionRequest[] {
  if (queue.some((p) => p.id === req.id)) return queue;
  return [...queue, req];
}

/** Remove a request by id after the user answers (or it is cancelled). */
export function dequeuePermission(
  queue: PermissionRequest[],
  id: string,
): PermissionRequest[] {
  return queue.filter((p) => p.id !== id);
}

/** Head of the queue — the modal currently shown. */
export function headPermission(
  queue: PermissionRequest[],
): PermissionRequest | null {
  return queue[0] ?? null;
}

/**
 * Session that owns the permission answer.
 * Prefer the request's session_id over the focused tab.
 */
export function sessionIdForPermission(
  req: PermissionRequest,
  fallbackSessionId?: string | null,
): string {
  if (req.session_id) return req.session_id;
  return fallbackSessionId ?? "";
}

export type PermissionDecision = "allow" | "deny" | "always_allow";
