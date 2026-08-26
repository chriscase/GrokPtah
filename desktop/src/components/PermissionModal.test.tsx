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
  presentDeniedPermissionRecord,
  type ConsentAcknowledgement,
} from "../lib/operatorConsentPresentation";

afterEach(cleanup);

const ADVERSARIAL = [
  "/etc/passwd",
  "/opt/x",
  "src/main.rs",
  "git status",
  "tok_opaque_9f3a",
  "\u202e",
  "\u2066",
  "\u200b",
  "__proto__",
  "constructor",
  "toString",
];

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
  respondImpl?: () => Promise<unknown>;
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
        onRespond={async (requestId, decision, sessionId) => {
          onAnswer(requestId, decision, sessionId);
          const result = respondImpl ? await respondImpl() : "acknowledged";
          if (result === "acknowledged") {
            setQueue((q) =>
              permissionQueueAfterAcknowledgement(q, requestId, "acknowledged"),
            );
          }
          return result;
        }}
      />
    </div>
  );
}

function surfaceHaystack(root: HTMLElement): string {
  const liveAlert = screen.queryByTestId("permission-announcement")?.textContent ?? "";
  const liveStatus = screen.queryByTestId("permission-live-status")?.textContent ?? "";
  return `${root.textContent ?? ""}\n${liveAlert}\n${liveStatus}\n${root.outerHTML}`;
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

  it("shows only closed risk and deny-history labels without raw summaries", () => {
    const history: DenyHistoryEntry[] = [
      {
        at: Date.now(),
        tool_name: "run_terminal_cmd",
        summary: "Allow shell: rm -rf /Users/secret nested shell invocation",
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
    const risk = screen.getByTestId("permission-risk").textContent ?? "";
    expect(risk).toMatch(/Ask first/);
    expect(risk).toMatch(/Untrusted risk prose is hidden/);
    expect(risk).not.toMatch(/nested shell invocation/);
    expect(screen.getByTestId("permission-summary").textContent).toBe(CONSENT_COPY.waiting);
    expect(screen.getByTestId("permission-deny-history")).toBeTruthy();
    const item = screen.getByTestId("permission-deny-history-item").textContent ?? "";
    expect(item).toMatch(/Terminal command/);
    expect(item).toMatch(CONSENT_COPY.priorDenial);
    expect(item).not.toMatch(/run_terminal_cmd|rm -rf|\/Users\/secret|nested shell/);
  });

  it("keeps distinct persisted session identities and never renders them", () => {
    const alpha = presentDeniedPermissionRecord(
      makeReq("d-alpha", "session-alpha-1111"),
      "session-alpha-1111",
    );
    const beta = presentDeniedPermissionRecord(
      makeReq("d-beta", "session-beta-2222", "write_file"),
      "session-beta-2222",
    );
    expect(alpha.session_id).toBe("session-alpha-1111");
    expect(beta.session_id).toBe("session-beta-2222");
    expect(alpha.session_id).not.toBe(beta.session_id);
    expect(alpha.session_id).not.toBe("owning-session");
    expect(beta.session_id).not.toBe("owning-session");

    render(
      <PermissionHarness
        initial={[makeReq("d-now", "session-now-3333")]}
        focusedSessionId="session-focused-bbbb"
        onAnswer={() => {}}
        denyHistory={[
          { at: 1, ...alpha },
          { at: 2, ...beta },
        ]}
      />,
    );
    const hay = surfaceHaystack(screen.getByTestId("permission-modal"));
    expect(hay).not.toContain("session-alpha-1111");
    expect(hay).not.toContain("session-beta-2222");
    expect(hay).not.toContain("session-now-3333");
    expect(hay).not.toContain("owning-session");
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

  it("inerts late overlays and body portals, restores prior state, and traps escaped focus", async () => {
    const prior = document.createElement("aside");
    prior.setAttribute("inert", "");
    prior.setAttribute("aria-hidden", "false");
    prior.dataset.testid = "prior-overlay";
    document.body.append(prior);

    const { unmount } = render(
      <PermissionHarness
        initial={[makeReq("inert-late", "sess-1")]}
        focusedSessionId="sess-1"
        onAnswer={() => {}}
      />,
    );

    const shell = screen.getByTestId("operator-shell");
    const late = document.createElement("div");
    late.dataset.testid = "late-overlay";
    const lateButton = document.createElement("button");
    lateButton.type = "button";
    lateButton.textContent = "late overlay";
    late.append(lateButton);
    shell.append(late);

    const portal = document.createElement("div");
    portal.dataset.testid = "body-portal";
    const portalButton = document.createElement("button");
    portalButton.type = "button";
    portalButton.textContent = "body portal";
    portal.append(portalButton);
    document.body.append(portal);

    await waitFor(() => {
      expect(late).toHaveAttribute("inert");
      expect(portal).toHaveAttribute("inert");
      expect(prior).toHaveAttribute("inert");
      expect(prior).toHaveAttribute("aria-hidden", "true");
    });
    expect(screen.getByTestId("permission-modal-backdrop")).not.toHaveAttribute("inert");

    const first = screen.getByTestId("permission-deny");
    portalButton.focus();
    fireEvent.keyDown(window, { key: "Tab" });
    expect(document.activeElement).toBe(first);

    unmount();
    expect(prior.hasAttribute("inert")).toBe(true);
    expect(prior.getAttribute("aria-hidden")).toBe("false");
    expect(late.hasAttribute("inert")).toBe(false);
    expect(portal.hasAttribute("inert")).toBe(false);
    prior.remove();
    late.remove();
    portal.remove();
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
    let release: ((value: ConsentAcknowledgement) => void) | undefined;
    const pending = new Promise<ConsentAcknowledgement>((resolve) => {
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
    release?.("acknowledged");
    await waitFor(() => expect(screen.queryByTestId("permission-modal")).toBeNull());
  });

  it("stays pending until onRespond returns and never invents a timeout acknowledgement", async () => {
    let finish: ((value: unknown) => void) | undefined;
    const hung = new Promise<unknown>((resolve) => {
      finish = resolve;
    });
    render(
      <PermissionHarness
        initial={[makeReq("hang-1", "sess-1"), makeReq("hang-2", "sess-2", "write_file")]}
        focusedSessionId="sess-1"
        onAnswer={() => {}}
        respondImpl={() => hung}
      />,
    );
    fireEvent.click(screen.getByTestId("permission-allow"));
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute(
        "data-consent-phase",
        "pending",
      ),
    );
    await new Promise((resolve) => setTimeout(resolve, 40));
    expect(screen.getByTestId("permission-modal")).toHaveAttribute(
      "data-consent-phase",
      "pending",
    );
    expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
    finish?.("lost");
    await waitFor(() =>
      expect(screen.getByTestId("permission-modal")).toHaveAttribute(
        "data-consent-phase",
        "unconfirmed",
      ),
    );
    expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
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
          onRespond={async (requestId, decision) => {
            answers.push(`${requestId}:${decision}`);
            setQueue((q) => permissionQueueAfterAcknowledgement(q, requestId, "lost"));
            return "lost";
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
        onRespond={async (requestId, decision) => {
          expect(permissionQueueAfterAcknowledgement(queue, requestId, "rejected")).toEqual(
            queue,
          );
          expect(dequeuePermission).not.toBeUndefined();
          send(requestId, decision);
          return "rejected";
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

  it("treats void and arbitrary onRespond resolution as unconfirmed and keeps the queue head", async () => {
    const cases: unknown[] = [undefined, "ok", { ok: true }, 42];
    for (const raw of cases) {
      cleanup();
      function ArbitraryHarness() {
        const [queue] = useState([
          makeReq("arb-1", "sess-1"),
          makeReq("arb-2", "sess-2", "write_file"),
        ]);
        const head = headPermission(queue);
        if (!head) return <div data-testid="no-permission">none</div>;
        return (
          <PermissionModal
            request={head}
            queuedBehind={queue.length - 1}
            onRespond={async () => raw}
          />
        );
      }
      render(<ArbitraryHarness />);
      fireEvent.click(screen.getByTestId("permission-allow"));
      await waitFor(() =>
        expect(screen.getByTestId("permission-modal")).toHaveAttribute(
          "data-consent-phase",
          "unconfirmed",
        ),
      );
      fireEvent.click(screen.getByTestId("permission-allow"));
      fireEvent.keyDown(window, { key: "Escape" });
      expect(screen.getByTestId("permission-tool").textContent).toBe("Terminal command");
      expect(screen.queryByTestId("no-permission")).toBeNull();
    }
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

  it("never renders secrets, paths, commands, ids, tokens, or formatting controls on any surface", () => {
    const request = makeReq(
      "req-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
      "session-background-aaaa",
      "__proto__",
      {
        risk: "Authorization: Bearer tok_opaque_9f3a nested shell /etc/passwd /opt/x src/main.rs git status \u202e\u2066\u200b",
        risk_tier: "constructor",
        command: "git status",
        path: "/etc/passwd",
        url: "https://example.invalid/opt/x",
        token: "tok_opaque_9f3a",
      },
    );
    request.summary =
      "Allow toString on /etc/passwd via git status tok_opaque_9f3a \u202e";
    const history: DenyHistoryEntry[] = [
      {
        at: 1,
        tool_name: "toString",
        summary: "src/main.rs git status /opt/x",
        session_id: "session-background-aaaa",
        risk: "/etc/passwd",
        risk_tier: "deny",
      },
    ];
    render(
      <PermissionModal
        request={request}
        queuedBehind={1}
        denyHistory={history}
        onRespond={async () => "acknowledged"}
      />,
    );
    const root = screen.getByTestId("permission-modal");
    const text = surfaceHaystack(root);
    const needles = [
      ...ADVERSARIAL,
      request.id,
      request.session_id,
      "Authorization: Bearer",
      "nested shell",
      "https://example.invalid",
      "run_terminal_cmd",
    ];
    for (const needle of needles) {
      expect(text).not.toContain(needle);
    }
    expect(root.querySelector("pre")).toBeNull();
    expect(screen.queryByTestId("permission-always")).toBeNull();
    expect(screen.getByTestId("permission-summary").textContent).toBe(CONSENT_COPY.waiting);
    expect(screen.getByTestId("permission-tool").textContent).toBe(CONSENT_COPY.toolUnknown);
    expect(screen.getByTestId("permission-risk").textContent).toMatch(CONSENT_COPY.riskUnknown);
    expect(screen.getByTestId("permission-announcement").textContent).not.toMatch(
      /session-background|req-aaaaaaaa|Bearer|\/etc\/passwd|git status/,
    );
    expect(screen.getByTestId("permission-live-status").textContent).not.toMatch(
      /\/opt\/x|src\/main\.rs|tok_opaque/,
    );
    expect(screen.getByTestId("permission-standing-grant").textContent).toMatch(
      /Always Allow is unavailable/,
    );
    expect(screen.getByTestId("permission-known-facts").textContent).toMatch(
      /Scope: unavailable/,
    );
  });
});
