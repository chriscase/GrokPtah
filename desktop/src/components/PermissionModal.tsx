import type { PermissionRequest } from "../lib/protocol";
import {
  sessionIdForPermission,
  type PermissionDecision,
} from "../lib/permissionQueue";

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
};

/**
 * Safety-boundary modal for tool permission prompts.
 * Extracted from App so session targeting and concurrent queue can be tested.
 */
export function PermissionModal({
  request,
  queuedBehind = 0,
  onRespond,
  fallbackSessionId = null,
}: PermissionModalProps) {
  const sessionId = sessionIdForPermission(request, fallbackSessionId);

  async function respond(decision: PermissionDecision) {
    await onRespond(request.id, decision, sessionId);
  }

  return (
    <div className="modal-backdrop" data-testid="permission-modal-backdrop">
      <div
        className="modal permission-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="permission-modal-title"
        data-testid="permission-modal"
        data-session-id={sessionId}
        data-request-id={request.id}
      >
        <h3 id="permission-modal-title">Needs your response</h3>
        {queuedBehind > 0 && (
          <p
            className="permission-queue-hint"
            data-testid="permission-queue-hint"
            style={{ fontSize: 12, color: "var(--muted)", marginTop: 0 }}
          >
            +{queuedBehind} more waiting
          </p>
        )}
        <p data-testid="permission-summary">{request.summary}</p>
        {typeof request.detail === "object" &&
          request.detail !== null &&
          "risk" in (request.detail as Record<string, unknown>) &&
          (request.detail as { risk?: string; risk_tier?: string }).risk && (
            <p
              data-testid="permission-risk"
              style={{
                margin: "0 0 0.75rem",
                padding: "0.5rem 0.65rem",
                borderRadius: 6,
                background:
                  (request.detail as { risk_tier?: string }).risk_tier === "deny"
                    ? "rgba(220, 50, 50, 0.15)"
                    : "rgba(220, 160, 40, 0.12)",
                border: "1px solid var(--border, #333)",
                fontSize: "0.85rem",
              }}
            >
              <strong>Exec-risk</strong> (
              {(request.detail as { risk_tier?: string }).risk_tier ?? "ask"}):{" "}
              {String((request.detail as { risk?: string }).risk)}
              <span style={{ opacity: 0.75 }}>
                {" "}
                — tool safety gate, not an OS sandbox
              </span>
            </p>
          )}
        <p style={{ fontSize: 12, color: "var(--muted)", marginTop: 0 }}>
          Tool: <code data-testid="permission-tool">{request.tool_name}</code>
          {sessionId ? (
            <>
              {" · "}
              Session:{" "}
              <code data-testid="permission-session">
                {sessionId.slice(0, 8)}
              </code>
            </>
          ) : null}
        </p>
        <details style={{ marginBottom: "0.75rem" }}>
          <summary
            style={{ cursor: "pointer", color: "var(--muted)", fontSize: 12 }}
          >
            Technical details
          </summary>
          <pre
            style={{
              fontSize: 11,
              color: "var(--muted)",
              maxHeight: 160,
              overflow: "auto",
            }}
          >
            {JSON.stringify(request.detail, null, 2)}
          </pre>
        </details>
        <div className="modal-actions">
          <button
            type="button"
            className="danger"
            data-testid="permission-deny"
            onClick={() => void respond("deny")}
          >
            Deny
          </button>
          <button
            type="button"
            data-testid="permission-always"
            onClick={() => void respond("always_allow")}
          >
            Always allow {request.tool_name}
          </button>
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
