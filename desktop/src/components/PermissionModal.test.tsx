import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { useState } from "react";
import { PermissionModal } from "./PermissionModal";
import {
  dequeuePermission,
  enqueuePermission,
  headPermission,
} from "../lib/permissionQueue";
import type { PermissionRequest } from "../lib/protocol";

function makeReq(
  id: string,
  session_id: string,
  tool = "run_terminal_cmd",
): PermissionRequest {
  return {
    id,
    session_id,
    tool_name: tool,
    summary: `Allow ${tool} on ${session_id}?`,
    detail: { session_id },
  };
}

/**
 * Stand-in for App's queue + modal: shows head, answers target request.session_id.
 */
function PermissionHarness({
  initial,
  focusedSessionId,
  onAnswer,
}: {
  initial: PermissionRequest[];
  focusedSessionId: string;
  onAnswer: (requestId: string, decision: string, sessionId: string) => void;
}) {
  const [queue, setQueue] = useState(initial);
  const head = headPermission(queue);
  if (!head) return <div data-testid="no-permission">none</div>;
  return (
    <PermissionModal
      request={head}
      queuedBehind={Math.max(0, queue.length - 1)}
      fallbackSessionId={focusedSessionId}
      onRespond={async (requestId, decision, sessionId) => {
        onAnswer(requestId, decision, sessionId);
        setQueue((q) => dequeuePermission(q, requestId));
      }}
    />
  );
}

describe("PermissionModal (#141)", () => {
  it("answers a permission for a non-focused session (not the focused tab)", async () => {
    const answers: Array<{ requestId: string; decision: string; sessionId: string }> =
      [];
    const background = makeReq("req-bg", "session-background-aaaa");
    render(
      <PermissionHarness
        initial={[background]}
        focusedSessionId="session-focused-bbbb"
        onAnswer={(requestId, decision, sessionId) =>
          answers.push({ requestId, decision, sessionId })
        }
      />,
    );

    expect(screen.getByTestId("permission-modal")).toHaveAttribute(
      "data-session-id",
      "session-background-aaaa",
    );
    expect(screen.getByTestId("permission-session").textContent).toContain(
      "session-",
    );

    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() => expect(answers).toHaveLength(1));
    expect(answers[0]).toEqual({
      requestId: "req-bg",
      decision: "allow",
      sessionId: "session-background-aaaa",
    });
    // Must NOT use the focused session.
    expect(answers[0].sessionId).not.toBe("session-focused-bbbb");
  });

  it("surfaces two concurrent permission requests in order", async () => {
    const answers: Array<{ requestId: string; sessionId: string }> = [];
    let q = enqueuePermission([], makeReq("r1", "sess-1", "write_file"));
    q = enqueuePermission(q, makeReq("r2", "sess-2", "run_terminal_cmd"));

    render(
      <PermissionHarness
        initial={q}
        focusedSessionId="sess-focused"
        onAnswer={(requestId, _d, sessionId) =>
          answers.push({ requestId, sessionId })
        }
      />,
    );

    expect(screen.getByTestId("permission-queue-hint").textContent).toMatch(
      /\+1 more waiting/,
    );
    expect(screen.getByTestId("permission-tool").textContent).toBe("write_file");
    expect(screen.getByTestId("permission-modal")).toHaveAttribute(
      "data-request-id",
      "r1",
    );

    fireEvent.click(screen.getByTestId("permission-deny"));
    await waitFor(() => {
      const modal = screen.getByTestId("permission-modal");
      expect(modal).toHaveAttribute("data-request-id", "r2");
    });
    expect(answers).toEqual([{ requestId: "r1", sessionId: "sess-1" }]);
    expect(screen.getByTestId("permission-tool").textContent).toBe(
      "run_terminal_cmd",
    );
    // Second modal should not show queue hint when only one left.
    expect(screen.queryByTestId("permission-queue-hint")).toBeNull();

    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() => {
      expect(answers).toHaveLength(2);
      expect(screen.queryByTestId("permission-modal")).toBeNull();
    });
    expect(answers).toEqual([
      { requestId: "r1", sessionId: "sess-1" },
      { requestId: "r2", sessionId: "sess-2" },
    ]);
  });
});
