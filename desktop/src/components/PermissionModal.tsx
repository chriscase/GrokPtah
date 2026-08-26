import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { PermissionRequest } from "../lib/protocol";
import {
  sessionIdForPermission,
  type PermissionDecision,
} from "../lib/permissionQueue";
import type { DenyHistoryEntry } from "../lib/denyHistory";
import {
  applyConsentEscape,
  canSubmitConsent,
  inertNonConsentSiblings,
  presentOperatorConsent,
  settleOperatorConsentAcknowledgement,
  trapConsentTabKey,
  type ConsentAcknowledgement,
  type ConsentDecision,
  type ConsentPhase,
} from "../lib/operatorConsentPresentation";

export type PermissionModalProps = {
  request: PermissionRequest;
  /** How many more requests wait behind this one (concurrent queue). */
  queuedBehind?: number;
  /**
   * Called with the request id, decision, and the **owning** session id
   * (request.session_id) — never invent the focused tab here (#141).
   * Advance the host queue only when the returned acknowledgement is resolved.
   */
  onRespond: (
    requestId: string,
    decision: PermissionDecision,
    sessionId: string,
  ) => void | Promise<void | ConsentAcknowledgement>;
  /** Optional fallback only if request.session_id is empty. */
  fallbackSessionId?: string | null;
  /** Recent denials for this project/session (#175). */
  denyHistory?: DenyHistoryEntry[];
  /** Test override for lost-acknowledgement timing. */
  acknowledgementTimeoutMs?: number;
};

/**
 * Safety-boundary modal for tool permission prompts.
 *
 * Presentation-only: focus, trapping, redaction, and acknowledgement lockout
 * do not change host authority. Always Allow is not offered at this head.
 */
export function PermissionModal({
  request,
  queuedBehind = 0,
  onRespond,
  fallbackSessionId = null,
  denyHistory = [],
  acknowledgementTimeoutMs,
}: PermissionModalProps) {
  const sessionId = sessionIdForPermission(request, fallbackSessionId);
  const [lock, setLock] = useState<{ id: string; phase: ConsentPhase }>({
    id: request.id,
    phase: "idle",
  });
  const phase: ConsentPhase = lock.id === request.id ? lock.phase : "idle";
  const presented = presentOperatorConsent({
    request,
    queuedBehind,
    denyHistory,
    phase,
    fallbackSessionId,
  });
  const backdropRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const denyRef = useRef<HTMLButtonElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const submitGate = useRef<string | null>(null);

  useEffect(() => {
    if (lock.id !== request.id) {
      setLock({ id: request.id, phase: "idle" });
      submitGate.current = null;
    }
  }, [lock.id, request.id]);

  useLayoutEffect(() => {
    openerRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const restore = inertNonConsentSiblings(backdropRef.current);
    return () => {
      restore();
      const opener = openerRef.current;
      openerRef.current = null;
      if (opener && opener.isConnected) opener.focus();
    };
  }, []);

  useLayoutEffect(() => {
    if (phase === "idle") denyRef.current?.focus();
  }, [phase, request.id]);

  const respond = useCallback(
    async (decision: ConsentDecision) => {
      if (!canSubmitConsent(phase) || submitGate.current === request.id) return;
      submitGate.current = request.id;
      setLock({ id: request.id, phase: "pending" });
      const ack = await settleOperatorConsentAcknowledgement(
        () => Promise.resolve(onRespond(request.id, decision, sessionId)),
        acknowledgementTimeoutMs,
      );
      if (ack === "acknowledged") {
        setLock({ id: request.id, phase: "idle" });
        return;
      }
      setLock({ id: request.id, phase: "unconfirmed" });
    },
    [acknowledgementTimeoutMs, onRespond, phase, request.id, sessionId],
  );

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Tab") {
        trapConsentTabKey(event, dialogRef.current);
        return;
      }
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      if (applyConsentEscape(phase) === "deny") {
        void respond("deny");
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [phase, respond]);

  const actionsLocked = !canSubmitConsent(phase);

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
        aria-describedby="permission-modal-description permission-recovery"
        data-testid="permission-modal"
        data-consent-phase={phase}
        tabIndex={-1}
      >
        <p className="sr-only" role="alert" data-testid="permission-announcement">
          {presented.liveAlert}
        </p>
        <p className="sr-only" role="status" data-testid="permission-live-status">
          {presented.liveStatus}
        </p>
        <h3 id="permission-modal-title">{presented.title}</h3>
        {presented.queueCopy ? (
          <p className="permission-queue-hint" data-testid="permission-queue-hint">
            {presented.queueCopy}
          </p>
        ) : null}
        <p id="permission-modal-description" data-testid="permission-summary">
          {presented.summary}
        </p>
        <p className="permission-risk" data-testid="permission-risk">
          <strong>Risk class</strong>: {presented.riskLabel}
          <span className="permission-risk-note"> — {presented.riskNote}</span>
        </p>
        <p className="permission-meta">
          <span>
            Tool class:{" "}
            <span data-testid="permission-tool">{presented.toolLabel}</span>
          </span>
          <span data-testid="permission-session">{presented.sessionFact}</span>
        </p>
        {presented.denyHistory.length > 0 && (
          <div className="permission-deny-history" data-testid="permission-deny-history">
            <strong>Recent denials</strong>
            <ul>
              {presented.denyHistory.map((entry, index) => (
                <li key={`deny-history-${index}`} data-testid="permission-deny-history-item">
                  {entry.toolLabel} [{entry.riskLabel}]: {entry.summary}
                </li>
              ))}
            </ul>
          </div>
        )}
        <details className="permission-details">
          <summary tabIndex={0}>Known facts</summary>
          <ul data-testid="permission-known-facts">
            {presented.details.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
        </details>
        <p
          id="permission-recovery"
          className="permission-recovery"
          data-testid="permission-recovery"
        >
          {presented.recovery}
        </p>
        <p className="permission-next-action" data-testid="permission-next-action">
          {presented.nextAction}
        </p>
        <p className="permission-standing-grant" data-testid="permission-standing-grant">
          {presented.standingGrant.explanation}
        </p>
        <div className="modal-actions">
          <button
            ref={denyRef}
            type="button"
            className="danger"
            data-testid="permission-deny"
            disabled={actionsLocked}
            onClick={() => void respond("deny")}
          >
            Deny
          </button>
          <button
            type="button"
            className="primary"
            data-testid="permission-allow"
            disabled={actionsLocked}
            onClick={() => void respond("allow")}
          >
            Allow once
          </button>
        </div>
      </div>
    </div>
  );
}
