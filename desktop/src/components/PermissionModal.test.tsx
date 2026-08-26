import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { PermissionModal } from "./PermissionModal";
import {
  dequeuePermission,
  enqueuePermission,
  headPermission,
} from "../lib/permissionQueue";
import type { PermissionRequest } from "../lib/protocol";
import type { DenyHistoryEntry } from "../lib/denyHistory";
import {
  CONSENT_COPY,
  permissionQueueAfterAcknowledgement,
  type ConsentAcknowledgement,
} from "../lib/operatorConsentPresentation";

afterEach(cleanup);

function makeReq(
  id: string,
  session_id: string,
  tool = "run_terminal_cmd",
  detail: Record<string, unknown> = {},
): PermissionRequest {
  return {
    id,
    session_id,
    tool_name: tool,
    summary: `Allow ${tool} on ${session_id}?`,
    detail: { session_id, ...detail },
  };
}

function PermissionHarness({
  initial,
  focusedSessionId,
  onAnswer,
  denyHistory = [],
  respondImpl,
}: {
  initial: PermissionRequest[];
  focusedSessionId: string;
  onAnswer: (requestId: string, decision: string, sessionId: string) => void;
  denyHistory?: DenyHistoryEntry[];
  respondImpl?: () => Promise<void | ConsentAcknowledgement>;
}) {
  const [queue, setQueue] = useState(initial);
  const head = headPermission(queue);
  if (!head) return <div data-testid="no-permission">none</div>;
  return (
    <div data-testid="operator-shell">
      <button type="button" data-testid="shell-opener">
        composer
      </button>
      <textarea data-testid="shell-composer" defaultValue="draft" />
      <PermissionModal
        request={head}
        queuedBehind={Math.max(0, queue.length - 1)}
        fallbackSessionId={focusedSessionId}
        denyHistory={denyHistory}
        acknowledgementTimeoutMs={40}
        onRespond={async (requestId, decision, sessionId) => {
          onAnswer(requestId, decision, sessionId);
          if (respondImpl) {
            const result = await respondImpl();
            if (result === undefined) {
              setQueue((q) =>
                permissionQueueAfterAcknowledgement(q, requestId, "acknowledged"),
              );
            }
            return result;
          }
          setQueue((q) =>
            permissionQueueAfterAcknowledgement(q, requestId, "acknowledged"),
          );
        }}
      />
    </div>
  );
}

function rawNeedles(request: PermissionRequest): string[] {
  return [
    request.id,
    request.session_id,
    request.tool_name,
    "Authorization: Bearer",
    "/Users/secret",
    "rm -rf",
    "{",
    "}",
  ];
}

describe("PermissionModal (#141 + operator consent)", () => {
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

    const modal = screen.getByTestId("permission-modal");
    expect(modal).not.toHaveAttribute("data-session-id");
    expect(modal).not.toHaveAttribute("data-request-id");
    expect(screen.getByTestId("permission-modal-backdrop")).toHaveAttribute(
      "data-modal-layer",
      "consent",
    );
    expect(screen.getByTestId("permission-session").textContent).toBe(
      CONSENT_COPY.sessionKnown,
    );
    expect(screen.getByTestId("permission-session").textContent).not.toContain("session-");

    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() => expect(answers).toHaveLength(1));
    expect(answers[0]).toEqual({
      requestId: "req-bg",
      decision: "allow",
      sessionId: "session-background-aaaa",
    });
    expect(answers[0].sessionId).not.toBe("session-focused-bbbb");
  });

  it("surfaces two concurrent permission requests in order after acknowledgement only", async () => {
    const answers: Array<{ requestId: string; sessionId: string }> = [];
    let q = enqueuePermission([], makeReq("r1", "sess-1", "write_file"));
    q = enqueuePermission(q, makeReq("r2", "sess-2", "run_terminal_cmd"));

    render(
      <PermissionHarness
        initial={q}
        focusedSessionId="sess-focused"
        onAnswer={(requestId, _d, sessionId) => answers.push({ requestId, sessionId })}
      />,
    );

    expect(screen.getByTestId("permission-queue-hint").textContent).toBe(
      CONSENT_COPY.queued,
    );
    expect(screen.getByTestId("permission-tool").textContent).toBe("Write a file");

    fireEvent.click(screen.getByTestId("permission-deny"));
    await waitFor(() => {
      expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
    });
    expect(answers).toEqual([{ requestId: "r1", sessionId: "sess-1" }]);
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

  it("shows closed risk and deny-history labels without raw summaries (#175)", () => {
    const history: DenyHistoryEntry[] = [
      {
        at: Date.now(),
        tool_name: "run_terminal_cmd",
        summary: "Allow shell: rm -rf /Users/secret",
        session_id: "sess-1",
        risk: "high-risk shell pattern",
        risk_tier: "deny",
      },
    ];
    render(
      <PermissionHarness
        initial={[
          makeReq("r-risk", "sess-1", "run_terminal_cmd", {
            risk: "nested shell invocation",
            risk_tier: "ask",
          }),
        ]}
        focusedSessionId="sess-1"
        onAnswer={() => {}}
        denyHistory={history}
      />,
    );
    expect(screen.getByTestId("permission-risk").textContent).toMatch(/Ask first/);
    expect(screen.getByTestId("permission-risk").textContent).toMatch(
      /nested shell invocation/,
    );
    expect(screen.getByTestId("permission-deny-history")).toBeTruthy();
    const item = screen.getByTestId("permission-deny-history-item").textContent ?? "";
    expect(item).toMatch(/Terminal command/);
    expect(item).not.toMatch(/run_terminal_cmd|rm -rf|\/Users\/secret/);
  });

  it("focuses Deny first, traps Tab, restores the opener, and inerts non-consent siblings", async () => {
    const opener = document.createElement("button");
    opener.type = "button";
    opener.textContent = "Open consent";
    document.body.append(opener);
    opener.focus();

    const { unmount } = render(
      <PermissionHarness
        initial={[makeReq("focus-1", "sess-1", "write_file")]}
        focusedSessionId="sess-1"
        onAnswer={() => {}}
      />,
    );

    await waitFor(() => expect(document.activeElement).toBe(screen.getByTestId("permission-deny")));
    expect(screen.getByTestId("shell-opener")).toHaveAttribute("inert");
    expect(screen.getByTestId("shell-opener")).toHaveAttribute("aria-hidden", "true");
    expect(screen.getByTestId("shell-composer").closest("[inert]")).toBeTruthy();
    expect(screen.getByTestId("permission-modal-backdrop")).not.toHaveAttribute("inert");

    const deny = screen.getByTestId("permission-deny");
    const dialog = screen.getByTestId("permission-modal");
    const focusable = Array.from(
      dialog.querySelectorAll<HTMLElement>(
        'button:not([disabled]), summary, [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((element) => element.tabIndex !== -1);
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    last.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(first);
    first.focus();
    fireEvent.keyDown(window, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
    screen.getByTestId("shell-opener").focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(first);
    expect(deny).toBeInTheDocument();

    unmount();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it("sends Deny on Escape only before submission", async () => {
    const answers: string[] = [];
    render(
      <PermissionHarness
        initial={[makeReq("esc-1", "sess-1")]}
        focusedSessionId="sess-1"
        onAnswer={(_id, decision) => answers.push(decision)}
      />,
    );
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(answers).toEqual(["deny"]));
  });

  it("submits at most once and suppresses Escape while pending or unconfirmed", async () => {
    let release: (() => void) | undefined;
    const pending = new Promise<void>((resolve) => {
      release = resolve;
    });
    const answers: string[] = [];
    render(
      <PermissionHarness
        initial={[makeReq("once-1", "sess-1")]}
        focusedSessionId="sess-1"
        onAnswer={(_id, decision) => answers.push(decision)}
        respondImpl={() => pending}
      />,
    );
    fireEvent.click(screen.getByTestId("permission-allow"));
    fireEvent.click(screen.getByTestId("permission-allow"));
    fireEvent.click(screen.getByTestId("permission-deny"));
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.getByTestId("permission-modal")).toHaveAttribute(
      "data-consent-phase",
      "pending",
    ));
    expect(answers).toEqual(["allow"]);
    expect(screen.getByTestId("permission-recovery").textContent).toBe(CONSENT_COPY.pending);
    release?.();
    await waitFor(() => expect(screen.queryByTestId("permission-modal")).toBeNull());
  });

  it("locks response unconfirmed on lost acknowledgement without retry or queue advance", async () => {
    const answers: string[] = [];
    const second = makeReq("lost-2", "sess-2", "write_file");
    function LostHarness() {
      const [queue, setQueue] = useState([makeReq("lost-1", "sess-1"), second]);
      const head = headPermission(queue);
      if (!head) return <div data-testid="no-permission">none</div>;
      return (
        <PermissionModal
          request={head}
          queuedBehind={Math.max(0, queue.length - 1)}
          acknowledgementTimeoutMs={20}
          onRespond={async (requestId, decision) => {
            answers.push(`${requestId}:${decision}`);
            setQueue((q) => permissionQueueAfterAcknowledgement(q, requestId, "lost"));
            return new Promise(() => {});
          }}
        />
      );
    }
    render(<LostHarness />);
    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute(
        "data-consent-phase",
        "unconfirmed",
      ),
    );
    fireEvent.click(screen.getByTestId("permission-allow"));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(answers).toEqual(["lost-1:allow"]);
    expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
    expect(screen.getByTestId("permission-recovery").textContent).toMatch(
      /Response unconfirmed/,
    );
    expect(screen.getByTestId("permission-recovery").textContent).not.toMatch(
      /succeeded|safe to retry/i,
    );
  });

  it("does not auto-retry or advance after a rejected acknowledgement", async () => {
    const send = vi.fn().mockRejectedValue(new Error("transport reset"));
    const queue = [makeReq("rej-1", "sess-1"), makeReq("rej-2", "sess-2", "write_file")];
    render(
      <PermissionModal
        request={queue[0]}
        queuedBehind={1}
        acknowledgementTimeoutMs={30}
        onRespond={async (requestId, decision) => {
          expect(permissionQueueAfterAcknowledgement(queue, requestId, "rejected")).toEqual(
            queue,
          );
          expect(dequeuePermission).not.toBeUndefined();
          send(requestId, decision);
          throw new Error("transport reset");
        }}
      />,
    );
    fireEvent.click(screen.getByTestId("permission-deny"));
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute(
        "data-consent-phase",
        "unconfirmed",
      ),
    );
    fireEvent.click(screen.getByTestId("permission-deny"));
    expect(send).toHaveBeenCalledOnce();
    expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
  });

  it("keeps an unconfirmed lock across stale field updates for the same request", async () => {
    const first = makeReq("stale-1", "sess-1", "write_file", {
      risk: "first note",
    });
    function StaleHarness() {
      const [request, setRequest] = useState(first);
      return (
        <div>
          <button
            type="button"
            data-testid="stale-update"
            onClick={() =>
              setRequest({
                ...request,
                summary: "Allow write_file on sess-1 with token Authorization: Bearer leaked",
                detail: { risk: "updated note", path: "/Users/secret" },
              })
            }
          >
            update
          </button>
          <PermissionModal
            request={request}
            acknowledgementTimeoutMs={20}
            onRespond={async () => {
              throw new Error("lost host");
            }}
          />
        </div>
      );
    }
    render(<StaleHarness />);
    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute(
        "data-consent-phase",
        "unconfirmed",
      ),
    );
    fireEvent.click(screen.getByTestId("stale-update"));
    expect(screen.getByTestId("permission-modal")).toHaveAttribute(
      "data-consent-phase",
      "unconfirmed",
    );
    expect(screen.getByTestId("permission-allow")).toBeDisabled();
    expect(screen.getByTestId("permission-summary").textContent).not.toMatch(
      /Authorization: Bearer|\/Users\/secret/,
    );
  });

  it("redacts secrets, paths, commands, and identifiers from every visible and live surface", () => {
    const request = makeReq(
      "req-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
      "session-background-aaaa",
      "run_terminal_cmd",
      {
        risk: "Authorization: Bearer sk-test-not-a-real-key in /Users/secret",
        risk_tier: "deny",
        command: "rm -rf /Users/secret",
      },
    );
    render(
      <PermissionModal request={request} queuedBehind={1} onRespond={async () => {}} />,
    );
    const root = screen.getByTestId("permission-modal");
    const text = `${root.textContent ?? ""}${root.outerHTML}`;
    for (const needle of rawNeedles(request)) {
      if (needle === "{" || needle === "}") continue;
      expect(text).not.toContain(needle);
    }
    expect(root.querySelector("pre")).toBeNull();
    expect(screen.queryByTestId("permission-always")).toBeNull();
    expect(screen.getByTestId("permission-announcement").textContent).not.toMatch(
      /session-background|req-aaaaaaaa|Bearer|\/Users/,
    );
    expect(screen.getByTestId("permission-standing-grant").textContent).toMatch(
      /Always Allow is unavailable/,
    );
    expect(screen.getByTestId("permission-known-facts").textContent).toMatch(
      /Scope: unavailable/,
    );
  });
});
