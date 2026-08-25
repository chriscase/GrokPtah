import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, fireEvent, waitFor } from "@testing-library/react";
import { useState } from "react";
import { PermissionModal } from "./PermissionModal";
import {
  dequeuePermission,
  enqueuePermission,
  headPermission,
} from "../lib/permissionQueue";
import type { PermissionRequest } from "../lib/protocol";
import type { DenyHistoryEntry } from "../lib/denyHistory";

// Vitest runs without `globals`, so RTL's auto-cleanup is off: unmount between
// tests or a later render finds two consent dialogs.
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

/**
 * Stand-in for App's queue + modal: shows head, answers target request.session_id.
 */
function PermissionHarness({
  initial,
  focusedSessionId,
  onAnswer,
  denyHistory = [],
}: {
  initial: PermissionRequest[];
  focusedSessionId: string;
  onAnswer: (requestId: string, decision: string, sessionId: string) => void;
  denyHistory?: DenyHistoryEntry[];
}) {
  const [queue, setQueue] = useState(initial);
  const head = headPermission(queue);
  if (!head) return <div data-testid="no-permission">none</div>;
  return (
    <PermissionModal
      request={head}
      queuedBehind={Math.max(0, queue.length - 1)}
      fallbackSessionId={focusedSessionId}
      denyHistory={denyHistory}
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
    expect(screen.getByTestId("permission-modal-backdrop")).toHaveAttribute(
      "data-modal-layer",
      "consent",
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

  it("shows exec-risk reason and deny history (#175)", () => {
    const history: DenyHistoryEntry[] = [
      {
        at: Date.now(),
        tool_name: "run_terminal_cmd",
        summary: "Allow shell: rm -rf /",
        session_id: "sess-1",
        risk: "high-risk shell pattern",
        risk_tier: "deny",
      },
    ];
    render(
      <PermissionHarness
        initial={[
          makeReq("r-risk", "sess-1", "run_terminal_cmd", {
            risk: "nested shell -c (opaque script)",
            risk_tier: "ask",
          }),
        ]}
        focusedSessionId="sess-1"
        onAnswer={() => {}}
        denyHistory={history}
      />,
    );
    expect(screen.getByTestId("permission-risk").textContent).toMatch(
      /nested shell/,
    );
    expect(screen.getByTestId("permission-deny-history")).toBeTruthy();
    expect(
      screen.getByTestId("permission-deny-history-item").textContent,
    ).toMatch(/run_terminal_cmd/);
  });
});

/** Shell with a background landmark, so inert treatment has something to act on. */
function ConsentShell({
  request,
  onRespond,
  queuedBehind = 0,
  denyHistory = [],
}: {
  request: PermissionRequest;
  onRespond: (
    requestId: string,
    decision: string,
    sessionId: string,
  ) => void | Promise<void>;
  queuedBehind?: number;
  denyHistory?: DenyHistoryEntry[];
}) {
  return (
    <div className="app-shell">
      <main data-testid="background">
        <button type="button">background control</button>
      </main>
      <PermissionModal
        request={request}
        queuedBehind={queuedBehind}
        denyHistory={denyHistory}
        onRespond={onRespond}
      />
    </div>
  );
}

function withOpener(): HTMLButtonElement {
  const opener = document.createElement("button");
  opener.textContent = "composer";
  document.body.append(opener);
  opener.focus();
  return opener;
}

describe("PermissionModal is a safety boundary", () => {
  afterEach(() => {
    document.querySelectorAll("body > button").forEach((node) => node.remove());
  });

  it("takes focus on Deny — the fail-closed answer — not on Allow", () => {
    const opener = withOpener();
    expect(document.activeElement).toBe(opener);

    render(
      <ConsentShell request={makeReq("req-focus", "session-focus-aaaa")} onRespond={vi.fn()} />,
    );

    expect(document.activeElement).toBe(screen.getByTestId("permission-deny"));
    expect(document.activeElement).not.toBe(screen.getByTestId("permission-allow"));
  });

  it("traps Tab and Shift-Tab inside the dialog", () => {
    render(
      <ConsentShell request={makeReq("req-trap", "session-trap-aaaa")} onRespond={vi.fn()} />,
    );
    const dialog = screen.getByTestId("permission-modal");
    const details = dialog.querySelector("summary") as HTMLElement;
    const deny = screen.getByTestId("permission-deny");
    const allow = screen.getByTestId("permission-allow");

    // Forward off the last stop wraps to the first.
    allow.focus();
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(document.activeElement).toBe(details);

    // Backward off the first stop wraps to the last.
    details.focus();
    fireEvent.keyDown(dialog, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(allow);

    // Focus that has leaked out is pulled back in.
    (document.querySelector("[data-testid='background'] button") as HTMLElement).focus();
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(document.activeElement).toBe(details);
    expect(deny).toBeTruthy();
  });

  it("answers deny on Escape, exactly once, and never dismisses unanswered", async () => {
    const answers: string[] = [];
    render(
      <ConsentShell
        request={makeReq("req-escape", "session-escape-aaaa")}
        onRespond={async (_id, decision) => {
          answers.push(decision);
        }}
      />,
    );

    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    await waitFor(() => expect(answers).toEqual(["deny"]));

    // A second Escape (or a queued click) must not double-answer.
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() => expect(answers).toEqual(["deny"]));
    // The prompt is still mounted: Escape answered it, it did not dismiss it.
    expect(screen.getByTestId("permission-modal")).toBeTruthy();
  });

  it("announces assertively that execution is blocked, with tier and queue depth", () => {
    render(
      <ConsentShell
        request={makeReq("req-announce", "session-announce-aaaa", "run_terminal_cmd", {
          risk: "nested shell -c",
          risk_tier: "ask",
        })}
        queuedBehind={2}
        onRespond={vi.fn()}
      />,
    );
    const alert = screen.getByTestId("permission-announcement");
    expect(alert).toHaveAttribute("role", "alert");
    expect(alert.className).toContain("sr-only");
    expect(alert.textContent).toMatch(/execution continues/i);
    expect(alert.textContent).toContain("run_terminal_cmd");
    expect(alert.textContent).toMatch(/Risk tier ask/);
    expect(alert.textContent).toMatch(/2 more waiting/);
    expect(alert.textContent).toMatch(/Deny is focused/);
  });

  it("keeps full privileged ids and arguments out of public accessibility text", () => {
    const sessionId = "session-privileged-0123456789abcdef";
    render(
      <ConsentShell
        request={{
          id: "req-privileged",
          session_id: sessionId,
          tool_name: "run_terminal_cmd",
          summary: "Allow run_terminal_cmd?",
          detail: {
            session_id: sessionId,
            args: ["--token", "sk-privileged-argument-value"],
          },
        }}
        onRespond={vi.fn()}
      />,
    );
    const dialog = screen.getByTestId("permission-modal");
    const accessibleText = [
      screen.getByTestId("permission-announcement").textContent,
      document.getElementById(dialog.getAttribute("aria-labelledby") ?? "")?.textContent,
      document.getElementById(dialog.getAttribute("aria-describedby") ?? "")?.textContent,
      screen.getByTestId("permission-session").textContent,
    ].join(" ");

    expect(accessibleText).not.toContain(sessionId);
    expect(accessibleText).not.toContain("sk-privileged-argument-value");
    expect(accessibleText).toContain("session-");
    // Routing still resolves to the owning session, in full, off the a11y path.
    expect(dialog).toHaveAttribute("data-session-id", sessionId);
  });

  it("wires an accessible description to the request summary", () => {
    render(
      <ConsentShell request={makeReq("req-desc", "session-desc-aaaa")} onRespond={vi.fn()} />,
    );
    const dialog = screen.getByTestId("permission-modal");
    const describedBy = dialog.getAttribute("aria-describedby");
    expect(describedBy).toBe("permission-modal-description");
    expect(document.getElementById(describedBy!)).toBe(
      screen.getByTestId("permission-summary"),
    );
    expect(dialog).toHaveAttribute("aria-modal", "true");
  });

  it("makes the background inert and aria-hidden, then restores it", () => {
    const { unmount } = render(
      <ConsentShell request={makeReq("req-inert", "session-inert-aaaa")} onRespond={vi.fn()} />,
    );
    const background = screen.getByTestId("background");
    expect(background.hasAttribute("inert")).toBe(true);
    expect(background).toHaveAttribute("aria-hidden", "true");

    unmount();
    expect(background.hasAttribute("inert")).toBe(false);
    expect(background.hasAttribute("aria-hidden")).toBe(false);
  });

  it.each(["allow", "deny", "always_allow"] as const)(
    "restores focus to the opener after %s",
    async (decision) => {
      const opener = withOpener();
      const answers: string[] = [];
      const { unmount } = render(
        <ConsentShell
          request={makeReq(`req-${decision}`, "session-restore-aaaa")}
          onRespond={async (_id, made) => {
            answers.push(made);
          }}
        />,
      );
      expect(document.activeElement).not.toBe(opener);

      fireEvent.click(screen.getByTestId(`permission-${decision === "always_allow" ? "always" : decision}`));
      await waitFor(() => expect(answers).toEqual([decision]));

      unmount();
      expect(document.activeElement).toBe(opener);
    },
  );

  it("restores focus to the opener after Escape denies", async () => {
    const opener = withOpener();
    const answers: string[] = [];
    const { unmount } = render(
      <ConsentShell
        request={makeReq("req-escape-restore", "session-restore-bbbb")}
        onRespond={async (_id, decision) => {
          answers.push(decision);
        }}
      />,
    );
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    await waitFor(() => expect(answers).toEqual(["deny"]));
    unmount();
    expect(document.activeElement).toBe(opener);
  });

  it("fails closed for a deny-tier request: no standing grant, tier always stated", () => {
    render(
      <ConsentShell
        request={makeReq("req-deny-tier", "session-deny-aaaa", "run_terminal_cmd", {
          risk_tier: "deny",
        })}
        onRespond={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("permission-always")).toBeNull();
    expect(screen.getByTestId("permission-deny")).toBeTruthy();
    expect(screen.getByTestId("permission-allow")).toBeTruthy();
    // A deny tier with no accompanying prose still renders the tier.
    const riskBlock = screen.getByTestId("permission-risk");
    expect(riskBlock.textContent).toMatch(/\(deny\)/);
    expect(riskBlock).toHaveAttribute("data-tier", "deny");
    // Styled by class, so the forced-colors rules can reach it.
    expect(riskBlock.getAttribute("style")).toBeNull();
  });

  it("still offers a standing grant for an ask-tier request", () => {
    render(
      <ConsentShell
        request={makeReq("req-ask-tier", "session-ask-aaaa", "write_file", {
          risk_tier: "ask",
        })}
        onRespond={vi.fn()}
      />,
    );
    expect(screen.getByTestId("permission-always").textContent).toContain("write_file");
  });

  it("re-arms deterministic Deny focus when the queue advances", async () => {
    const answers: string[] = [];
    let q = enqueuePermission([], makeReq("q1", "sess-queue-1", "write_file"));
    q = enqueuePermission(q, makeReq("q2", "sess-queue-2", "run_terminal_cmd"));
    render(
      <PermissionHarness
        initial={q}
        focusedSessionId="sess-focused"
        onAnswer={(_id, decision) => answers.push(decision)}
      />,
    );
    expect(document.activeElement).toBe(screen.getByTestId("permission-deny"));

    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute("data-request-id", "q2"),
    );
    expect(document.activeElement).toBe(screen.getByTestId("permission-deny"));
    expect(answers).toEqual(["allow"]);
  });
});
