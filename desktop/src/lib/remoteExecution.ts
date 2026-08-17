import type { RemoteSessionTarget, RemoteTaskSubmission } from "./protocol";

export type ExecutionTargetChoice =
  | { kind: "local" }
  | { kind: "remote"; sessionId: string };

/** Stable value used by the composer target picker. */
export function executionTargetValue(choice: ExecutionTargetChoice): string {
  return choice.kind === "local" ? "local" : `remote:${choice.sessionId}`;
}

/** Parse picker values without allowing malformed remote ids into a request. */
export function parseExecutionTargetValue(value: string): ExecutionTargetChoice | null {
  if (value === "local") return { kind: "local" };
  if (!value.startsWith("remote:")) return null;
  const sessionId = value.slice("remote:".length).trim();
  return sessionId ? { kind: "remote", sessionId } : null;
}

/** User-facing acknowledgement for a remote submission. */
export function remoteSubmissionMessage(
  session: RemoteSessionTarget,
  submission: RemoteTaskSubmission,
): string {
  const label = session.title || session.workspace;
  return `Submitted to ${label}. Run ${submission.runId} is ${submission.state}.`;
}
