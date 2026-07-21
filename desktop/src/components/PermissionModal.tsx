import type { PermissionRequest } from "../lib/protocol";
import {
  sessionIdForPermission,
  type PermissionDecision,
} from "../lib/permissionQueue";
import type { DenyHistoryEntry } from "../lib/denyHistory";

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

/**
 * Safety-boundary modal for tool permission prompts.
 * Extracted from App so session targeting and concurrent queue can be tested.
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
        {risk && (
          <p
            data-testid="permission-risk"
            style={{
              margin: "0 0 0.75rem",
              padding: "0.5rem 0.65rem",
              borderRadius: 6,
              background:
                riskTier === "deny"
                  ? "rgba(220, 50, 50, 0.15)"
                  : "rgba(220, 160, 40, 0.12)",
              border: "1px solid var(--border, #333)",
              fontSize: "0.85rem",
            }}
          >
            <strong>Exec-risk</strong> ({riskTier ?? "ask"}): {risk}
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
        {denyHistory.length > 0 && (
          <div
            data-testid="permission-deny-history"
            style={{
              marginBottom: "0.75rem",
              padding: "0.5rem 0.65rem",
              borderRadius: 6,
              border: "1px solid var(--border, #333)",
              fontSize: "0.8rem",
              maxHeight: 120,
              overflow: "auto",
            }}
          >
            <strong>Recent denials</strong>
            <ul style={{ margin: "0.35rem 0 0", paddingLeft: "1.1rem" }}>
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
