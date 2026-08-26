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
  consentPhaseForRequest,
  CONSENT_COPY,
  observeNonConsentInert,
  owningSessionId,
  permissionQueueAfterAcknowledgement,
  phaseAfterAcknowledgement,
  presentDeniedPermissionRecord,
  presentOperatorConsent,
  presentationContainsForbiddenRaw,
  readConsentAcknowledgement,
  reduceConsentLock,
  standingGrantFactsAtThisHead,
  trapConsentTabKey,
} from "./operatorConsentPresentation";

const css = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "..", "styles", "app.css"),
  "utf8",
);

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

function req(overrides: Partial<PermissionRequest> = {}): PermissionRequest {
  return {
    id: "req-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
    session_id: "session-background-aaaa",
    tool_name: "run_terminal_cmd",
    summary: `Allow run_terminal_cmd on /etc/passwd via git status tok_opaque_9f3a \u202e`,
    detail: {
      risk: `nested shell /opt/x src/main.rs \u2066\u200b`,
      risk_tier: "ask",
      command: "git status",
      path: "/etc/passwd",
      token: "tok_opaque_9f3a",
    },
    ...overrides,
  };
}

function haystackOf(presented: ReturnType<typeof presentOperatorConsent>): string {
  return [
    presented.title,
    presented.toolLabel,
    presented.riskLabel,
    presented.riskNote,
    presented.summary,
    presented.sessionFact,
    presented.queueCopy,
    presented.liveAlert,
    presented.liveStatus,
    presented.recovery,
    presented.nextAction,
    presented.details.join(" "),
    presented.denyHistory.map((item) => `${item.toolLabel} ${item.summary}`).join(" "),
    JSON.stringify(presented),
  ].join("\n");
}

describe("operatorConsentPresentation", () => {
  it("maps only own-key tool and risk classes and never inherited keys", () => {
    expect(closedToolLabel("run_terminal_cmd")).toBe("Terminal command");
    expect(closedToolLabel("write_file")).toBe("Write a file");
    expect(closedToolLabel("/Users/secret/tool")).toBe(CONSENT_COPY.toolUnknown);
    expect(closedToolLabel({ raw: true })).toBe(CONSENT_COPY.toolUnknown);
    expect(closedToolLabel("__proto__")).toBe(CONSENT_COPY.toolUnknown);
    expect(closedToolLabel("constructor")).toBe(CONSENT_COPY.toolUnknown);
    expect(closedToolLabel("toString")).toBe(CONSENT_COPY.toolUnknown);
    expect(closedToolLabel("hasOwnProperty")).toBe(CONSENT_COPY.toolUnknown);
    expect(closedToolLabel("valueOf")).toBe(CONSENT_COPY.toolUnknown);
    expect(typeof closedToolLabel("__proto__")).toBe("string");
    expect(closedRiskLabel("deny")).toBe(CONSENT_COPY.riskDeny);
    expect(closedRiskLabel("ask")).toBe(CONSENT_COPY.riskAsk);
    expect(closedRiskLabel("constructor")).toBe(CONSENT_COPY.riskUnknown);
    expect(closedRiskLabel("toString")).toBe(CONSENT_COPY.riskUnknown);
  });

  it("projects only fixed copy plus mapped classes and never untrusted payload text", () => {
    const presented = presentOperatorConsent({
      request: req({
        tool_name: "__proto__",
        detail: {
          risk_tier: "constructor",
          risk: "git status /opt/x",
          path: "/etc/passwd",
        },
      }),
      queuedBehind: 2,
      phase: "idle",
      denyHistory: [
        {
          at: 1,
          tool_name: "toString",
          summary: "Allow shell: /etc/passwd git status",
          session_id: "session-background-aaaa",
          risk: "src/main.rs",
          risk_tier: "deny",
        },
      ],
    });
    expect(presented.toolLabel).toBe(CONSENT_COPY.toolUnknown);
    expect(presented.riskLabel).toBe(CONSENT_COPY.riskUnknown);
    expect(presented.summary).toBe(CONSENT_COPY.waiting);
    expect(presented.riskNote).toBe(CONSENT_COPY.riskNote);
    expect(presented.denyHistory[0]?.summary).toBe(CONSENT_COPY.priorDenial);
    const haystack = haystackOf(presented);
    expect(
      presentationContainsForbiddenRaw(haystack, [
        ...ADVERSARIAL,
        "req-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        "session-background-aaaa",
        "run_terminal_cmd",
        "nested shell",
      ]),
    ).toEqual([]);
  });

  it("never offers Always Allow and reports host facts unavailable at this head", () => {
    const facts = standingGrantFactsAtThisHead();
    expect(facts.offered).toBe(false);
    const presented = presentOperatorConsent({
      request: req({
        detail: { scope: "workspace", lifetime: "forever", revision: 99 },
      }),
      phase: "idle",
    });
    expect(presented.offerStandingGrant).toBe(false);
    expect(presented.details.join("\n")).toMatch(/Scope: unavailable/);
    expect(presented.details.join("\n")).not.toMatch(/forever|workspace|99/);
  });

  it("keeps distinct owning session identities in deny records and never renders them", () => {
    const alpha = presentDeniedPermissionRecord(
      req({ session_id: "session-alpha-1111" }),
      "session-alpha-1111",
    );
    const beta = presentDeniedPermissionRecord(
      req({ session_id: "session-beta-2222", tool_name: "write_file" }),
      "session-beta-2222",
    );
    expect(alpha?.session_id).toBe("session-alpha-1111");
    expect(beta?.session_id).toBe("session-beta-2222");
    expect(alpha?.session_id).not.toBe(beta?.session_id);
    expect(alpha?.session_id).not.toBe("owning-session");
    const presented = presentOperatorConsent({
      request: req(),
      phase: "idle",
      denyHistory: [
        { at: 1, ...alpha! },
        { at: 2, ...beta! },
      ],
    });
    const haystack = haystackOf(presented);
    expect(haystack).not.toContain("session-alpha-1111");
    expect(haystack).not.toContain("session-beta-2222");
    expect(haystack).not.toContain("owning-session");
  });

  it("never persists a focused fallback or claimed mismatch when the host owner is missing", () => {
    const focused = "session-focused-bbbb";
    expect(owningSessionId(req({ session_id: "" }))).toBeNull();
    expect(owningSessionId(req({ session_id: "   " }))).toBeNull();
    expect(owningSessionId({ session_id: undefined as unknown as string })).toBeNull();
    expect(presentDeniedPermissionRecord(req({ session_id: "" }), focused)).toBeNull();
    expect(presentDeniedPermissionRecord(req({ session_id: "" }), "")).toBeNull();
    expect(
      presentDeniedPermissionRecord(req({ session_id: "session-alpha-1111" }), focused),
    ).toBeNull();
    const presented = presentOperatorConsent({
      request: req({ session_id: "" }),
      phase: "idle",
      fallbackSessionId: focused,
    });
    expect(presented.sessionFact).toBe(CONSENT_COPY.sessionMissing);
    expect(haystackOf(presented)).not.toContain(focused);
    expect(haystackOf(presented)).not.toContain("owning-session");
  });

  it("binds request identity synchronously and ignores stale acknowledgement", () => {
    const idle = { requestId: "a1", phase: "idle" as const };
    const bound = reduceConsentLock(idle, { type: "bind", requestId: "b2" });
    expect(bound).toEqual({ requestId: "b2", phase: "idle" });
    expect(reduceConsentLock(idle, { type: "bind", requestId: "a1" })).toBe(idle);
    const pending = reduceConsentLock(bound, { type: "submit", requestId: "b2" });
    expect(pending).toEqual({ requestId: "b2", phase: "pending" });
    expect(reduceConsentLock(pending, { type: "submit", requestId: "b2" })).toBe(pending);
    expect(
      reduceConsentLock(pending, { type: "acknowledge", requestId: "a1", ack: "acknowledged" }),
    ).toBe(pending);
    expect(
      reduceConsentLock(pending, { type: "acknowledge", requestId: "b2", ack: "lost" }),
    ).toEqual({ requestId: "b2", phase: "unconfirmed" });
    expect(consentPhaseForRequest({ requestId: "a1", phase: "unconfirmed" }, "b2", "b2")).toBe(
      "pending",
    );
    expect(consentPhaseForRequest({ requestId: "a1", phase: "idle" }, "b2", null)).toBe("idle");
  });

  it("explains pending and unconfirmed recovery without implying success or safe retry", () => {
    const pending = presentOperatorConsent({ request: req(), phase: "pending" });
    const lost = presentOperatorConsent({ request: req(), phase: "unconfirmed" });
    expect(pending.recovery).toMatch(/Do not assume success/);
    expect(lost.recovery).toMatch(/Response unconfirmed/);
    expect(lost.nextAction).not.toMatch(/try again|safe retry|succeeded/i);
  });

  it("treats only explicit closed acknowledgements as acknowledgements", () => {
    expect(readConsentAcknowledgement("acknowledged")).toBe("acknowledged");
    expect(readConsentAcknowledgement("rejected")).toBe("rejected");
    expect(readConsentAcknowledgement("lost")).toBe("lost");
    expect(readConsentAcknowledgement(undefined)).toBeNull();
    expect(readConsentAcknowledgement(null)).toBeNull();
    expect(readConsentAcknowledgement("ok")).toBeNull();
    expect(readConsentAcknowledgement(42)).toBeNull();
    expect(readConsentAcknowledgement({ ok: true })).toBeNull();
    const queue = [req({ id: "r1" }), req({ id: "r2", tool_name: "write_file" })];
    expect(permissionQueueAfterAcknowledgement(queue, "r1", "rejected")).toEqual(queue);
    expect(permissionQueueAfterAcknowledgement(queue, "r1", "lost")).toEqual(queue);
    expect(permissionQueueAfterAcknowledgement(queue, "r1", "acknowledged")[0]?.id).toBe("r2");
  });

  it("allows Escape Deny only while idle and blocks workspace shortcuts while consent exists", () => {
    expect(applyConsentEscape("idle")).toBe("deny");
    expect(applyConsentEscape("pending")).toBe("suppress");
    expect(applyConsentEscape("unconfirmed")).toBe("suppress");
    expect(canSubmitConsent("idle")).toBe(true);
    expect(phaseAfterAcknowledgement("rejected")).toBe("unconfirmed");
    expect(consentBlocksWorkspaceShortcuts(true)).toBe(true);
  });

  it("inerts ancestor siblings, late overlays, and body portals, then restores prior state", async () => {
    const prior = document.createElement("aside");
    prior.setAttribute("inert", "");
    prior.setAttribute("aria-hidden", "false");
    prior.dataset.testid = "prior-overlay";
    const shell = document.createElement("div");
    const main = document.createElement("main");
    main.dataset.testid = "main";
    const layer = document.createElement("div");
    layer.dataset.modalLayer = "consent";
    shell.append(main, layer);
    document.body.append(prior, shell);

    const restore = observeNonConsentInert(layer);
    expect(main.hasAttribute("inert")).toBe(true);
    expect(prior.getAttribute("aria-hidden")).toBe("true");
    expect(layer.hasAttribute("inert")).toBe(false);

    const portal = document.createElement("div");
    portal.dataset.testid = "body-portal";
    document.body.append(portal);
    await vi.waitFor(() => expect(portal.hasAttribute("inert")).toBe(true));

    const late = document.createElement("div");
    late.dataset.testid = "late-sibling";
    shell.append(late);
    await vi.waitFor(() => expect(late.hasAttribute("inert")).toBe(true));

    const fake = document.createElement("div");
    fake.dataset.modalLayer = "consent";
    fake.dataset.testid = "mislabeled-consent";
    document.body.append(fake);

    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("data-testid", "svg-sibling");
    const foreign = document.createElementNS("http://www.w3.org/2000/svg", "foreignObject");
    const foreignButton = document.createElement("button");
    foreignButton.type = "button";
    foreignButton.textContent = "svg foreign";
    foreign.append(foreignButton);
    svg.append(foreign);
    document.body.append(svg);

    await vi.waitFor(() => {
      expect(fake.hasAttribute("inert")).toBe(true);
      expect(svg.hasAttribute("inert")).toBe(true);
    });
    expect(layer.hasAttribute("inert")).toBe(false);

    restore();
    expect(main.hasAttribute("inert")).toBe(false);
    expect(prior.hasAttribute("inert")).toBe(true);
    expect(prior.getAttribute("aria-hidden")).toBe("false");
    expect(portal.hasAttribute("inert")).toBe(false);
    expect(late.hasAttribute("inert")).toBe(false);
    expect(fake.hasAttribute("inert")).toBe(false);
    expect(svg.hasAttribute("inert")).toBe(false);

    prior.remove();
    shell.remove();
    portal.remove();
    fake.remove();
    svg.remove();
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
