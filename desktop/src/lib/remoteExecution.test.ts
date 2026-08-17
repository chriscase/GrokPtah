import { describe, expect, it } from "vitest";
import type { RemoteSessionTarget, RemoteTaskSubmission } from "./protocol";
import {
  executionTargetValue,
  parseExecutionTargetValue,
  remoteSubmissionMessage,
} from "./remoteExecution";

const session: RemoteSessionTarget = {
  sessionId: "remote-session-1",
  title: "VM Build",
  workspace: "/srv/grokptah",
  updatedAt: "2026-08-17T12:00:00Z",
  busy: false,
};

const submission: RemoteTaskSubmission = {
  runId: "remote-run-1",
  sessionId: session.sessionId,
  state: "queued",
  requestId: "request-1",
  executionMode: "shared",
  queuedPosition: 1,
};

describe("remote execution target mapping", () => {
  it("round-trips local and remote picker values", () => {
    expect(parseExecutionTargetValue(executionTargetValue({ kind: "local" }))).toEqual({
      kind: "local",
    });
    expect(
      parseExecutionTargetValue(
        executionTargetValue({ kind: "remote", sessionId: session.sessionId }),
      ),
    ).toEqual({ kind: "remote", sessionId: session.sessionId });
  });

  it("rejects malformed remote picker values", () => {
    expect(parseExecutionTargetValue("remote:")).toBeNull();
    expect(parseExecutionTargetValue("service:remote-session-1")).toBeNull();
  });

  it("describes the durable submission state for the transcript", () => {
    expect(remoteSubmissionMessage(session, submission)).toBe(
      "Submitted to VM Build. Run remote-run-1 is queued.",
    );
  });

  it("pins the typed remote submission contract used by the desktop client", () => {
    const required: Array<keyof RemoteTaskSubmission> = [
      "runId",
      "sessionId",
      "state",
      "requestId",
      "executionMode",
    ];
    for (const key of required) {
      expect(submission[key]).toBeTruthy();
    }
    expect(["queued", "running", "completed", "cancelled"]).toContain(submission.state);
    expect(submission.queuedPosition).toBe(1);
    expect(
      remoteSubmissionMessage(session, { ...submission, state: "completed", queuedPosition: null }),
    ).toBe("Submitted to VM Build. Run remote-run-1 is completed.");
  });
});
