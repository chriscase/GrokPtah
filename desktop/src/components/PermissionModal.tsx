import { useCallback, useEffect, useRef } from "react";
import type { PermissionRequest } from "../lib/protocol";
import {
  sessionIdForPermission,
  type PermissionDecision,
} from "../lib/permissionQueue";
import type { DenyHistoryEntry } from "../lib/denyHistory";
import { trapTabKey, useDialogFocus } from "../lib/overlayA11y";

export type PermissionModalProps = {
  request: PermissionRequest;
  /** How many more requests wait behind this one (concurrent queue). */
  queuedBehind?: number;
  /**
   * Called with the request id, decision, and the **owning** session id
   * (request.session_id) — never invent the focused tab here (#141).
   */
  onRespond: (
    requestId: string,
    decision: PermissionDecision,
    sessionId: string,
  ) => void | Promise<void>;
  /** Optional fallback only if request.session_id is empty. */
  fallbackSessionId?: string | null;
  /** Recent denials for this project/session (#175). */
  denyHistory?: DenyHistoryEntry[];
};

/** Public accessibility text never carries a full privileged id. */
function shortSession(sessionId: string): string {
  return sessionId.slice(0, 8);
}

/**
 * Safety-boundary modal for tool permission prompts.
 * Extracted from App so session targeting and concurrent queue can be tested.
 *
 * This dialog is raised **asynchronously**, mid-turn, while the operator is
 * usually typing in the composer, and it blocks execution until answered. It
 * therefore has to behave like a real safety boundary rather than merely
 * declare `role="dialog"`:
 *
 *  - initial focus lands on **Deny**, the fail-closed answer — never on Allow;
 *  - Tab is trapped, so no keypress reaches the application behind it;
 *  - the background is inert and aria-hidden;
 *  - a `role="alert"` line announces that execution is now blocked;
 *  - focus returns to the opener on every terminal path.
 *
 * Escape answers **deny**. It is deliberately not a dismissal: a consent gate
 * must never be closable in a way that leaves the request unanswered, and it
 * must never resolve to anything but the safe answer.
 */
export function PermissionModal({
  request,
  queuedBehind = 0,
  onRespond,
  fallbackSessionId = null,
  denyHistory = [],
}: PermissionModalProps) {
  const sessionId = sessionIdForPermission(request, fallbackSessionId);
  const detail =
    typeof request.detail === "object" && request.detail !== null
      ? (request.detail as Record<string, unknown>)
      : {};
  const risk =
    typeof detail.risk === "string" ? detail.risk : undefined;
  const riskTier =
    typeof detail.risk_tier === "string" ? detail.risk_tier : undefined;
  const tier = riskTier ?? "ask";
  /** Deny-tier requests are fail-closed: a standing grant is never offered. */
  const offerAlwaysAllow = riskTier !== "deny";

  const backdropRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const denyRef = useRef<HTMLButtonElement>(null);
  const answeredRef = useRef(false);

  const respond = useCallback(
    async (decision: PermissionDecision) => {
      // One answer per request. Escape plus a queued click must not double-send.
      if (answeredRef.current) return;
      answeredRef.current = true;
      try {
        await onRespond(request.id, decision, sessionId);
      } catch (reason) {
        answeredRef.current = false;
        throw reason;
      }
    },
    [onRespond, request.id, sessionId],
  );

  useEffect(() => {
    answeredRef.current = false;
  }, [request.id]);

  useDialogFocus({
    layerRef: backdropRef,
    initialFocusRef: denyRef,
    focusKey: request.id,
  });

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      // Capture phase + stopPropagation so Escape cannot also close Settings,
      // the session browser or any overlay layered underneath this prompt.
      event.preventDefault();
      event.stopPropagation();
      void respond("deny");
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [respond]);

  const announcement = [
    `Permission needed before execution continues: ${request.tool_name}.`,
    `Risk tier ${tier}.`,
    sessionId ? `Session ${shortSession(sessionId)}.` : "",
    queuedBehind > 0 ? `${queuedBehind} more waiting.` : "",
    "Deny is focused.",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      ref={backdropRef}
      className="modal-backdrop"
      data-modal-layer="consent"
      data-testid="permission-modal-backdrop"
    >
      <div
        ref={dialogRef}
        className="modal permission-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="permission-modal-title"
        aria-describedby="permission-modal-description"
        data-testid="permission-modal"
        data-session-id={sessionId}
        data-request-id={request.id}
        tabIndex={-1}
        onKeyDown={(event) => trapTabKey(event, dialogRef.current)}
      >
        {/*
          Assertive because the prompt arrives unannounced while the operator is
          doing something else, and execution is already blocked on it. Carries
          the tier and a truncated session only — never a full id or arguments.
        */}
        <p
          className="sr-only"
          role="alert"
          data-testid="permission-announcement"
        >
          {announcement}
        </p>
        <h3 id="permission-modal-title">Needs your response</h3>
        {queuedBehind > 0 && (
          <p
            className="permission-queue-hint"
            data-testid="permission-queue-hint"
          >
            +{queuedBehind} more waiting
          </p>
        )}
        <p id="permission-modal-description" data-testid="permission-summary">
          {request.summary}
        </p>
        {(risk || riskTier) && (
          <p
            className="permission-risk"
            data-tier={tier}
            data-testid="permission-risk"
          >
            <strong>Exec-risk</strong> ({tier})
            {risk ? `: ${risk}` : ""}
            <span className="permission-risk-note">
              {" "}
              — tool safety gate, not an OS sandbox
            </span>
          </p>
        )}
        <p className="permission-meta">
          Tool: <code data-testid="permission-tool">{request.tool_name}</code>
          {sessionId ? (
            <>
              {" · "}
              Session:{" "}
              <code data-testid="permission-session">
                {shortSession(sessionId)}
              </code>
            </>
          ) : null}
        </p>
        {denyHistory.length > 0 && (
          <div
            className="permission-deny-history"
            data-testid="permission-deny-history"
          >
            <strong>Recent denials</strong>
            <ul>
              {denyHistory.slice(0, 8).map((e, i) => (
                <li
                  key={`${e.at}-${i}`}
                  data-testid="permission-deny-history-item"
                >
                  <code>{e.tool_name}</code>
                  {e.risk_tier ? ` [${e.risk_tier}]` : ""}:{" "}
                  {e.summary.slice(0, 80)}
                  {e.summary.length > 80 ? "…" : ""}
                </li>
              ))}
            </ul>
          </div>
        )}
        <details className="permission-details">
          {/* Explicit tabIndex so the trap boundary is identical in every engine. */}
          <summary tabIndex={0}>Technical details</summary>
          <pre>{JSON.stringify(request.detail, null, 2)}</pre>
        </details>
        <div className="modal-actions">
          <button
            ref={denyRef}
            type="button"
            className="danger"
            data-testid="permission-deny"
            onClick={() => void respond("deny")}
          >
            Deny
          </button>
          {offerAlwaysAllow && (
            <button
              type="button"
              data-testid="permission-always"
              onClick={() => void respond("always_allow")}
            >
              Always allow {request.tool_name}
            </button>
          )}
          <button
            type="button"
            className="primary"
            data-testid="permission-allow"
            onClick={() => void respond("allow")}
          >
            Allow
          </button>
        </div>
      </div>
    </div>
  );
}
